//! One roll's ticket-and-fee split, the drawn position's entitlement, and the solvency test the
//! settlement is refused on.
//!
//! Pure over explicit inputs — nothing here reads an account or writes one. A settlement decides
//! what a depositor is paid and whether the vault can pay it, and a suite can pass while getting
//! either wrong, so both live in one place with the arithmetic exposed rather than inlined into
//! a handler alongside its transfers.
//!
//! **The equal share is divided across every position, virtual ones included, and it carries the
//! roll's whole ticket alongside the fee's own equal-fee allocation.** The ticket enters this
//! split undivided by any allocation — it is not fee revenue, so no part of it is ever routed to
//! protocol. The virtual positions' portion of the combined pot funds the treasury that settles
//! blanks; the real positions' portion stays in the vault as an owed balance, claimed through the
//! same accumulator a position's fee share is claimed through. A drawn position is paid
//! exactly that accrued balance — never a separate lump sum on top of it — so redistributing the
//! virtual portion to real positions instead would scale every depositor's accrual by the ratio of
//! real to total positions, and routing it to protocol would raise the protocol take above what is
//! published: the routing is what keeps the published exit percentage exact, not an accounting
//! convenience.

use anchor_lang::prelude::*;

use crate::common::constants::{ACC_PRECISION, BLANK_SLOTS};
use crate::common::errors::ByeMachineError;
use crate::common::math::{
    allocation, equal_share_unit, position_fee_payout, slot_is_empty, tier_share_unit,
};

/// Index into a pool's basis-point allocation array. The array is the on-chain field order, and
/// a settlement that reads the tier slice as the equal slice mis-splits every fee while still
/// summing to the whole, so the positions are named rather than written as literals.
pub const ALLOC_EQUAL: usize = 0;
pub const ALLOC_TIER: usize = 1;
pub const ALLOC_PROTOCOL: usize = 2;

/// How one roll's ticket and fee divide. The four destinations are disjoint and sum to the whole
/// ticket plus the whole fee exactly, at micro-USDC precision, for every input this accepts.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct FeeSplit {
    /// Real positions' share of the ticket plus the fee's equal allocation. Stays in the vault as
    /// an owed balance; claimed through the equal accumulator.
    pub real_equal: u64,
    /// Tier members' share of the fee's tier allocation. Stays in the vault; claimed through the
    /// tier accumulator. The ticket never reaches this pot — only the equal pot carries it.
    pub tier: u64,
    /// Virtual positions' share of the ticket plus the fee's equal allocation, transferred to the
    /// treasury that settles blanks. The caller applies that treasury's ceiling and books any
    /// excess as protocol revenue; this figure is the credit before the ceiling, because the
    /// ceiling depends on a balance no pure function can read.
    pub treasury_04: u64,
    /// Protocol revenue, out of the fee alone. Carries its own allocation **plus every integer
    /// remainder the fee's own bps split floors away**, which is what makes the fee's three bps
    /// allocations sum to the fee rather than to slightly less than it. The ticket enters no bps
    /// division, so it never contributes a remainder here.
    pub protocol: u64,
    /// Accumulator bump per real position, `ACC_PRECISION`-scaled. Carries the ticket share and
    /// the equal-fee share together; a position's payout does not separate the two.
    pub equal_unit: u128,
    /// Accumulator bump per tier member, `ACC_PRECISION`-scaled. Zero when the tier is empty.
    pub tier_unit: u128,
}

impl FeeSplit {
    /// What stays in the vault as an owed balance.
    pub fn fee_keep(&self) -> Result<u64> {
        self.real_equal
            .checked_add(self.tier)
            .ok_or_else(|| ByeMachineError::ArithmeticFailure.into())
    }

    /// What leaves the vault at settlement.
    pub fn fee_out(&self) -> Result<u64> {
        self.protocol
            .checked_add(self.treasury_04)
            .ok_or_else(|| ByeMachineError::ArithmeticFailure.into())
    }
}

/// Splits one roll's ticket and fee across the four destinations and derives the two accumulator
/// bumps.
///
/// `n_real` and `blanks` are the counts **before** this roll's draw removes anything: the
/// position a roll draws sits inside that roll's own equal-share denominator, because it was
/// active when the roll was priced and the disclosed odds derive from a pool that included it.
///
/// Each retained pot covers exactly as many claims as its denominator names, so `n_real` and
/// `tier_len` must be the true counts of positions accruing on each accumulator. A `tier_len`
/// below the number of positions carrying membership under-retains against the extra claims, and
/// nothing downstream of this function can detect it — the split still sums to the ticket plus
/// the fee.
///
/// `ticket` enters the equal pot whole, alongside the fee's own equal-bps allocation, and is
/// divided by no allocation of its own — a rounding remainder can only arise from a term that is
/// itself divided by a basis-point allocation, and the ticket is never that. Only the fee's own
/// three bps allocations can leave a remainder, and that remainder routes to protocol.
///
/// Every such remainder is routed to protocol rather than dropped, and the two retained pots are
/// taken as whole allocations rather than as the accumulator bumps multiplied back out. That
/// direction matters: the accumulator floors once per bump while a position floors once at the
/// end over many bumps, so a pot sized from the floored bumps can be smaller than the sum of the
/// claims it has to cover — two bumps worth 0.6 micro each retain nothing and owe one. Sizing the
/// pot from the allocation instead leaves the drift in the vault as dust, which is the only
/// direction that cannot become insolvency.
pub fn split_fee(
    fee: u64,
    ticket: u64,
    alloc_bps: [u16; 3],
    n_real: u32,
    blanks: [u32; BLANK_SLOTS],
    blank_faces: [u64; BLANK_SLOTS],
    tier_len: u8,
) -> Result<FeeSplit> {
    let bps_sum = u32::from(alloc_bps[ALLOC_EQUAL])
        .checked_add(u32::from(alloc_bps[ALLOC_TIER]))
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_add(u32::from(alloc_bps[ALLOC_PROTOCOL]))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    require_eq!(
        bps_sum,
        10_000,
        ByeMachineError::AllocationBpsNotFullyAllocated
    );

    // An empty slot has no count, as it has no weight and no draw; a zero face in slots 0–2 fails
    // with the `ZeroValue` the draw's `weight_of` raises for it.
    let mut n_blank = 0u64;
    for (slot, (count, face)) in blanks.into_iter().zip(blank_faces).enumerate() {
        if slot_is_empty(slot, face) {
            continue;
        }
        require!(face > 0, ByeMachineError::ZeroValue);
        n_blank = n_blank
            .checked_add(u64::from(count))
            .ok_or(ByeMachineError::ArithmeticFailure)?;
    }
    let n_total = n_blank
        .checked_add(u64::from(n_real))
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    let equal_total = allocation(fee, alloc_bps[ALLOC_EQUAL])?;
    let equal_pot = equal_total
        .checked_add(ticket)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let equal_unit = equal_share_unit(equal_pot, n_total)?;

    let treasury_04 = u128::from(n_blank)
        .checked_mul(equal_unit)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_div(ACC_PRECISION)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let treasury_04 = u64::try_from(treasury_04).map_err(|_| ByeMachineError::ArithmeticFailure)?;
    let real_equal = equal_pot
        .checked_sub(treasury_04)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    // An empty tier has no accumulator to bump, so its allocation falls through to protocol
    // with the remainders rather than being retained against nobody's claim.
    let (tier, tier_unit) = if tier_len > 0 {
        (
            allocation(fee, alloc_bps[ALLOC_TIER])?,
            tier_share_unit(fee, alloc_bps[ALLOC_TIER], tier_len)?,
        )
    } else {
        (0u64, 0u128)
    };

    let protocol = fee
        .checked_sub(equal_total)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_sub(tier)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    Ok(FeeSplit {
        real_equal,
        tier,
        treasury_04,
        protocol,
        equal_unit,
        tier_unit,
    })
}

/// Treasury 03's and Treasury 05's basis-point slices of the protocol share. Operations' own
/// 5000 is deliberately **not** a constant here: it is never multiplied out, only derived by
/// subtracting the other two, which is what makes the three sum to the share exactly rather
/// than to within a rounding step of it. A constant would invite the multiplication back.
pub const PROTOCOL_SHARE_T03_BPS: u16 = 2_500;
pub const PROTOCOL_SHARE_T05_BPS: u16 = 2_500;

/// How one roll's protocol share divides across its three final destinations. Disjoint, and
/// summing to the share exactly at micro-USDC precision for every input.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct ProtocolShare {
    /// Operations — `protocol_config.protocol_revenue`. Carries its own 50% **plus the
    /// remainder** the two quarter-shares floor away.
    pub operations: u64,
    /// This pool's own Treasury 03, never a shared or cross-pool pot: per-pool attribution is
    /// what crediting at settlement rather than at a later sweep buys.
    pub treasury_03: u64,
    /// The protocol-wide Treasury 05, earmarked for on-market $BYE purchases.
    pub treasury_05: u64,
}

/// Splits the 20% protocol allocation 50/25/25 across Operations, this pool's Treasury 03, and
/// Treasury 05.
///
/// `protocol` is [`FeeSplit::protocol`] — the 20% allocation **plus** every remainder the other
/// three top-level destinations floored away, so this is a sub-split of an already-inclusive
/// figure rather than a second bite at the fee.
///
/// One level under the allocation rounding rule, in the same direction: the two quarter-shares
/// floor, and what they leave over lands in Operations. Deriving Operations by subtraction rather
/// than as its own `allocation(protocol, 5000)` is what makes the sum exact — the three are `p −
/// ⌊p/4⌋ − ⌊p/4⌋`, `⌊p/4⌋`, `⌊p/4⌋`, which add to `p` by construction and cannot be made not to by
/// a rounding change.
///
/// **The remainder's size is worth stating, because it is not symmetric across the three.** Each
/// quarter-share sits within 0.75 micro-USDC of an exact 25%, but Operations absorbs both
/// floors, so it can sit up to 1.5 micro-USDC above an exact 50% — at `p % 4 == 3`, the worst
/// case. That is the intended direction (the leftover is routed to Operations) and the reason
/// the bound is asserted as "Operations is never short" rather than as a symmetric tolerance.
pub fn split_protocol_share(protocol: u64) -> Result<ProtocolShare> {
    let treasury_03 = allocation(protocol, PROTOCOL_SHARE_T03_BPS)?;
    let treasury_05 = allocation(protocol, PROTOCOL_SHARE_T05_BPS)?;
    let operations = protocol
        .checked_sub(treasury_03)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_sub(treasury_05)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    Ok(ProtocolShare {
        operations,
        treasury_03,
        treasury_05,
    })
}

/// The drawn position's fee entitlement, read off both accumulators **after** this roll's own
/// accrual has been applied to them — the drawing roll counts as the position's last active
/// roll, which is what makes the published exit percentage exact rather than approximate.
///
/// `accrued` is folded in exactly once. The tier leg already adds it, so what the equal leg
/// carries forward is the tier leg's result, not `accrued` a second time; a non-member's tier
/// checkpoint is never a claim and is not read.
pub fn position_delta(
    acc_equal: u128,
    equal_checkpoint: u128,
    acc_tier: u128,
    tier_checkpoint: u128,
    in_tier: bool,
    accrued: u64,
) -> Result<u64> {
    let carried = if in_tier {
        position_fee_payout(acc_tier, tier_checkpoint, accrued)?
    } else {
        accrued
    };
    position_fee_payout(acc_equal, equal_checkpoint, carried)
}

/// The ledger a settlement's solvency is tested against, all of it **pre-state**: every field is
/// read before this roll's writes land.
#[cfg_attr(test, derive(Debug, Clone, Copy))]
pub struct SolvencyLedger {
    /// Principal vault balance.
    pub principal_balance: u64,
    /// Liability for every unsettled roll, **this one still included**.
    pub pending_roll_liability: u64,
    /// This roll's snapshotted price — the refund this roll would return.
    pub s_price: u64,
    /// Fees owed to positions before this roll's accrual.
    pub owed_fees: u64,
    /// What this roll's split retains in the vault.
    pub fee_keep: u64,
    /// What this roll's split moves out of the vault.
    pub fee_out: u64,
    /// The drawn position's entitlement, after this roll's accrual.
    pub delta: u64,
    /// The drawn position's recorded value — the principal half of its payout.
    pub recorded_value: u64,
}

/// Whether the vault covers this settlement with every other obligation still reserved.
///
/// ```text
/// principal − (liability − s_price) − (owed_fees + fee_keep − delta) − fee_out
///     ≥ recorded_value + delta
/// ```
///
/// Each obligation is counted once. `liability − s_price` reserves the *other* unsettled rolls'
/// refunds — this roll's price stays inside the balance because settling forfeits its refund.
/// `owed_fees + fee_keep − delta` reserves what is owed to *other* positions after this roll's
/// accrual; `delta` leaves the reserve exactly because the payout line pays it.
///
/// **Both inner subtractions are required to be non-negative rather than allowed to wrap.** A
/// negative reserve would *raise* the funds the test believes are available, so a liability that
/// somehow excluded this roll, or an entitlement larger than everything owed, would read as
/// solvent instead of as the corruption it is. The comparison itself is evaluated signed and in
/// the written form: a shortfall is an ordinary refund, not an arithmetic error.
pub fn is_solvent(ledger: &SolvencyLedger) -> Result<bool> {
    let other_refunds = i128::from(ledger.pending_roll_liability)
        .checked_sub(i128::from(ledger.s_price))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    require!(other_refunds >= 0, ByeMachineError::ArithmeticFailure);

    let other_fee_claims = i128::from(ledger.owed_fees)
        .checked_add(i128::from(ledger.fee_keep))
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_sub(i128::from(ledger.delta))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    require!(other_fee_claims >= 0, ByeMachineError::ArithmeticFailure);

    let available = i128::from(ledger.principal_balance)
        .checked_sub(other_refunds)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_sub(other_fee_claims)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_sub(i128::from(ledger.fee_out))
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    let payout = i128::from(ledger.recorded_value)
        .checked_add(i128::from(ledger.delta))
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    Ok(available >= payout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        DEFAULT_ALLOC_EQUAL_BPS, DEFAULT_ALLOC_PROTOCOL_BPS, DEFAULT_ALLOC_TIER_BPS,
        DEFAULT_BLANK_FACES, DEFAULT_FEE, DEFAULT_TICKET_TARGET,
    };

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    const LAUNCH_BPS: [u16; 3] = [
        DEFAULT_ALLOC_EQUAL_BPS,
        DEFAULT_ALLOC_TIER_BPS,
        DEFAULT_ALLOC_PROTOCOL_BPS,
    ];

    struct Xorshift64(u64);

    impl Xorshift64 {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }

        /// Three basis-point allocations summing to exactly 10,000.
        fn allocations(&mut self) -> [u16; 3] {
            let equal = self.below(10_001);
            let tier = self.below(10_001 - equal);
            [
                u16::try_from(equal).unwrap(),
                u16::try_from(tier).unwrap(),
                u16::try_from(10_000 - equal - tier).unwrap(),
            ]
        }
    }

    // --- the array positions are the pool's own field order --------------------------------

    #[test]
    fn the_allocation_indices_are_the_pool_field_order() {
        // Pairwise-different allocations: a swapped index still sums to the fee.
        let split = split_fee(
            1_000_000,
            0,
            [7_000, 2_000, 1_000],
            1,
            [0; 8],
            DEFAULT_BLANK_FACES,
            1,
        )
        .unwrap();
        assert_eq!(split.real_equal, 700_000, "the equal slice is not index 0");
        assert_eq!(split.tier, 200_000, "the tier slice is not index 1");
        assert_eq!(split.protocol, 100_000, "the protocol slice is not index 2");
        assert_eq!([ALLOC_EQUAL, ALLOC_TIER, ALLOC_PROTOCOL], [0, 1, 2]);
    }

    // --- the protocol share's own 50/25/25 sub-split --------------------------------

    /// Hand-computed cases over every `p % 4` residue, the only thing that decides how much the
    /// two floors leave over.
    #[test]
    fn the_sub_split_is_pinned_at_every_residue_of_four() {
        // (protocol, operations, t03, t05)
        for (p, ops, t03, t05) in [
            (0u64, 0u64, 0u64, 0u64),
            (100, 50, 25, 25),
            (101, 51, 25, 25),
            (102, 52, 25, 25),
            (103, 53, 25, 25),
            (4, 2, 1, 1),
            (7, 5, 1, 1),
        ] {
            let share = split_protocol_share(p).unwrap();
            assert_eq!(
                share,
                ProtocolShare {
                    operations: ops,
                    treasury_03: t03,
                    treasury_05: t05,
                },
                "protocol={p}"
            );
        }
    }

    /// The sum is exact; the two quarter-shares are within a micro-USDC of an exact 25%; and the
    /// leftover lands in Operations and only there.
    #[test]
    fn the_three_destinations_sum_exactly_and_the_remainder_is_always_operations() {
        let mut rng = Xorshift64(0x243f_6a88_85a3_08d3);
        for _ in 0..200_000 {
            let p = rng.below(50_000_000);
            let share = split_protocol_share(p).unwrap();

            assert_eq!(
                share.operations + share.treasury_03 + share.treasury_05,
                p,
                "the three destinations do not reconstitute the protocol share; protocol={p}"
            );

            // Each quarter-share is the floor of an exact 25%, so it is short by at most the
            // rounding step and never over.
            for (name, got) in [("t03", share.treasury_03), ("t05", share.treasury_05)] {
                let exact_times_four = u128::from(p);
                let got_times_four = u128::from(got) * 4;
                assert!(
                    got_times_four <= exact_times_four && exact_times_four - got_times_four < 4,
                    "{name} is not within one micro-USDC below an exact 25%; protocol={p} \
                     got={got}"
                );
            }

            // Operations absorbs both floors, so it is never short of an exact 50% and never
            // more than the two rounding steps above it.
            let ops_times_two = u128::from(share.operations) * 2;
            let exact_times_two = u128::from(p);
            assert!(
                ops_times_two >= exact_times_two && ops_times_two - exact_times_two <= 3,
                "operations did not absorb exactly the remainder; protocol={p} \
                 operations={}",
                share.operations
            );
        }
    }

    /// A remainder is reachable: three of every four inputs leave one for Operations.
    #[test]
    fn a_remainder_is_actually_reachable() {
        let with_remainder = (0u64..64)
            .filter(|p| {
                let s = split_protocol_share(*p).unwrap();
                s.operations * 2 > *p
            })
            .count();
        assert_eq!(
            with_remainder, 48,
            "three residues in four leave a remainder for Operations; if this moved, the \
             sweep above may be asserting over inputs that never round"
        );
    }

    /// Treasury 03 and Treasury 05 take equal shares, so this module cannot tell them apart;
    /// which account each transfer names is checked at the call site.
    #[test]
    fn the_two_quarter_shares_are_equal_and_therefore_not_distinguishable_here() {
        for p in [0u64, 1, 2, 3, 99, 1_000_000, u64::MAX] {
            let share = split_protocol_share(p).unwrap();
            assert_eq!(share.treasury_03, share.treasury_05, "protocol={p}");
        }
    }

    /// The share is a `u64` and the two quarter-shares are taken in `u128`, so the largest
    /// possible input must not overflow or saturate.
    #[test]
    fn the_maximum_share_splits_without_overflow() {
        let share = split_protocol_share(u64::MAX).unwrap();
        assert_eq!(
            share.operations as u128 + share.treasury_03 as u128 + share.treasury_05 as u128,
            u128::from(u64::MAX)
        );
    }

    // --- (c) the split sums to the ticket plus the fee exactly -------------------------------

    #[test]
    fn the_split_sums_to_the_ticket_plus_fee_over_randomized_allocations() {
        let mut rng = Xorshift64(0x9e37_79b9_7f4a_7c15);
        for _ in 0..50_000 {
            let fee = rng.below(50_000_000);
            let ticket = rng.below(50_000_000);
            let alloc_bps = rng.allocations();
            let n_real = u32::try_from(rng.below(2_000)).unwrap();
            let blanks = [
                u32::try_from(rng.below(200)).unwrap(),
                u32::try_from(rng.below(200)).unwrap(),
                u32::try_from(rng.below(200)).unwrap(),
                u32::try_from(rng.below(200)).unwrap(),
                0,
                0,
                0,
                0,
            ];
            let tier_len = u8::try_from(rng.below(21)).unwrap();
            if u64::from(n_real) + blanks.iter().map(|b| u64::from(*b)).sum::<u64>() == 0 {
                continue;
            }

            let split = split_fee(
                fee,
                ticket,
                alloc_bps,
                n_real,
                blanks,
                DEFAULT_BLANK_FACES,
                tier_len,
            )
            .unwrap();
            let total = split.real_equal + split.tier + split.treasury_04 + split.protocol;
            assert_eq!(
                total,
                fee + ticket,
                "fee={fee} ticket={ticket} bps={alloc_bps:?} n_real={n_real} blanks={blanks:?} \
                 tier_len={tier_len} split={split:?}"
            );
            assert_eq!(
                split.fee_keep().unwrap() + split.fee_out().unwrap(),
                fee + ticket,
                "the two halves the settlement moves do not reconstitute the ticket plus the fee"
            );
        }
    }

    #[test]
    fn the_launch_configuration_splits_as_published() {
        // 20 real positions, no blanks, a full tier: 75% equal, 5% tier, 20% protocol — and the
        // whole 9-USDC ticket rides the equal share alongside its 75%.
        let split = split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            LAUNCH_BPS,
            20,
            [0, 0, 0, 0, 0, 0, 0, 0],
            DEFAULT_BLANK_FACES,
            20,
        )
        .unwrap();
        assert_eq!(split.real_equal, 9_675_000);
        assert_eq!(split.tier, 45_000);
        assert_eq!(split.treasury_04, 0);
        assert_eq!(split.protocol, 180_000);
        assert_eq!(split.fee_keep().unwrap(), 9_720_000);
        assert_eq!(split.fee_out().unwrap(), 180_000);
    }

    #[test]
    fn the_virtual_positions_share_of_the_equal_pot_goes_to_the_blank_treasury() {
        // 5 real and 5 virtual: the equal pot (ticket plus the fee's own equal share) halves,
        // and the virtual half leaves the vault.
        let split = split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            LAUNCH_BPS,
            5,
            [3, 1, 1, 0, 0, 0, 0, 0],
            DEFAULT_BLANK_FACES,
            5,
        )
        .unwrap();
        assert_eq!(split.real_equal + split.treasury_04, 9_675_000);
        assert_eq!(split.treasury_04, 4_837_500);
        assert_eq!(split.real_equal, 4_837_500);
    }

    /// A Gold pool's faces: slot 3 holds the above-ticket face, slots 4–7 are 0.
    const GOLD_SPLIT_FACES: [u64; BLANK_SLOTS] =
        [5_000_000, 10_000_000, 25_000_000, 100_000_000, 0, 0, 0, 0];

    fn split_with(blanks: [u32; BLANK_SLOTS], faces: [u64; BLANK_SLOTS]) -> FeeSplit {
        split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            LAUNCH_BPS,
            5,
            blanks,
            faces,
            5,
        )
        .unwrap()
    }

    /// The above-ticket slot's blanks are virtual positions like any other: a Gold pool in its
    /// low-value state holds only slot-3 blanks, and Treasury 04, which pays them, takes their
    /// share of the equal pot.
    #[test]
    fn the_above_ticket_slot_counts_toward_treasury_04s_share() {
        let below = split_with([3, 1, 1, 0, 0, 0, 0, 0], GOLD_SPLIT_FACES);
        let above = split_with([0, 0, 0, 5, 0, 0, 0, 0], GOLD_SPLIT_FACES);
        assert_eq!(above.treasury_04, 4_837_500);
        assert_eq!(above, below);
    }

    /// A slot whose face is 0 has no count: a stale count left in it moves nothing in the split,
    /// in a reserved slot or in slot 3 of a pool with no above-ticket face.
    /// Slots 0–2 always hold a face: a 0 there fails the split as it fails the draw.
    #[test]
    fn a_zero_face_in_slots_0_to_2_fails_the_split() {
        let mut faces = DEFAULT_BLANK_FACES;
        faces[1] = 0;
        assert_eq!(
            split_fee(
                DEFAULT_FEE,
                DEFAULT_TICKET_TARGET,
                LAUNCH_BPS,
                5,
                [0; 8],
                faces,
                5
            )
            .map(|_| ())
            .unwrap_err(),
            ByeMachineError::ZeroValue.into()
        );
    }

    #[test]
    fn a_count_in_a_zero_face_slot_contributes_nothing() {
        let none = split_with([0; 8], GOLD_SPLIT_FACES);
        assert_eq!(none.treasury_04, 0);
        assert_eq!(split_with([0, 0, 0, 0, 7, 7, 7, 7], GOLD_SPLIT_FACES), none);
        assert_eq!(
            split_with([0, 0, 0, 9, 0, 0, 0, 0], DEFAULT_BLANK_FACES),
            split_with([0; 8], DEFAULT_BLANK_FACES)
        );
        assert_eq!(
            split_with([3, 1, 1, 9, 9, 9, 9, 9], DEFAULT_BLANK_FACES),
            split_with([3, 1, 1, 0, 0, 0, 0, 0], DEFAULT_BLANK_FACES)
        );
        assert_ne!(
            split_with([3, 1, 1, 0, 0, 0, 0, 0], DEFAULT_BLANK_FACES),
            none,
            "the comparison is vacuous unless counted slots move the split"
        );
    }

    #[test]
    fn a_pool_of_only_virtual_positions_retains_no_equal_share() {
        let split = split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            LAUNCH_BPS,
            0,
            [4, 4, 2, 0, 0, 0, 0, 0],
            DEFAULT_BLANK_FACES,
            0,
        )
        .unwrap();
        assert_eq!(split.real_equal, 0);
        assert_eq!(
            split.treasury_04, 9_675_000,
            "the ticket rides along with it"
        );
        assert_eq!(split.tier, 0, "an empty tier retains nothing");
        assert_eq!(split.tier_unit, 0);
        assert_eq!(
            split.protocol, 225_000,
            "the empty tier's allocation falls to protocol — the ticket never does"
        );
    }

    #[test]
    fn an_empty_tier_retains_nothing_and_bumps_nothing() {
        let with_tier = split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            LAUNCH_BPS,
            10,
            [0, 0, 0, 0, 0, 0, 0, 0],
            DEFAULT_BLANK_FACES,
            1,
        )
        .unwrap();
        let without = split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            LAUNCH_BPS,
            10,
            [0, 0, 0, 0, 0, 0, 0, 0],
            DEFAULT_BLANK_FACES,
            0,
        )
        .unwrap();
        assert!(with_tier.tier > 0 && with_tier.tier_unit > 0);
        assert_eq!(without.tier, 0);
        assert_eq!(without.tier_unit, 0);
        assert_eq!(
            without.protocol,
            with_tier.protocol + with_tier.tier,
            "the empty tier's allocation must reach protocol, not vanish"
        );
    }

    #[test]
    fn split_rejects_allocations_that_do_not_sum_to_ten_thousand() {
        for bps in [[7_500u16, 500, 1_999], [7_500, 500, 2_001], [0, 0, 0]] {
            assert_eq!(
                error_code(
                    split_fee(
                        DEFAULT_FEE,
                        DEFAULT_TICKET_TARGET,
                        bps,
                        10,
                        [0; 8],
                        DEFAULT_BLANK_FACES,
                        10
                    )
                    .unwrap_err()
                ),
                6501,
                "bps={bps:?}"
            );
        }
    }

    #[test]
    fn split_rejects_a_pool_with_no_positions_at_all() {
        assert_eq!(
            error_code(
                split_fee(
                    DEFAULT_FEE,
                    DEFAULT_TICKET_TARGET,
                    LAUNCH_BPS,
                    0,
                    [0, 0, 0, 0, 0, 0, 0, 0],
                    DEFAULT_BLANK_FACES,
                    0
                )
                .unwrap_err()
            ),
            6401
        );
    }

    // --- (c) owed_fees never falls below the claims it has to cover -------------------------

    /// Replays a pool of positions across randomized rolls: each roll splits a fee, bumps both
    /// accumulators, and adds only what the split retained to `owed_fees`. Positions join and
    /// leave the tier between rolls. At every intermediate state the retained total must cover
    /// the sum of what every position could withdraw right then.
    ///
    /// The claims are computed the way a withdrawal computes them — one floor at the end over
    /// the whole accumulated delta — which is precisely what a per-roll floor on the retained
    /// side fails to cover.
    #[test]
    fn owed_fees_covers_every_outstanding_claim_at_every_step() {
        let mut rng = Xorshift64(0xdead_beef_0bad_f00d);

        for trial in 0..200 {
            let alloc_bps = rng.allocations();
            let n_real = u32::try_from(1 + rng.below(24)).unwrap();
            let tier_size = u8::try_from(rng.below(u64::from(n_real).min(20) + 1)).unwrap();

            let mut acc_equal = 0u128;
            let mut acc_tier = 0u128;
            let mut owed_fees = 0u64;
            // (equal_checkpoint, tier_checkpoint, in_tier)
            let mut positions: Vec<(u128, u128, bool)> = (0..n_real)
                .map(|i| (0u128, 0u128, u32::from(tier_size) > i))
                .collect();

            for _ in 0..40 {
                let fee = 1 + rng.below(2_000_000);
                let ticket = rng.below(10_000_000);
                let blanks = [
                    u32::try_from(rng.below(30)).unwrap(),
                    u32::try_from(rng.below(30)).unwrap(),
                    u32::try_from(rng.below(30)).unwrap(),
                    u32::try_from(rng.below(30)).unwrap(),
                    0,
                    0,
                    0,
                    0,
                ];
                let split = split_fee(
                    fee,
                    ticket,
                    alloc_bps,
                    n_real,
                    blanks,
                    DEFAULT_BLANK_FACES,
                    tier_size,
                )
                .unwrap();

                acc_equal += split.equal_unit;
                acc_tier += split.tier_unit;
                owed_fees += split.fee_keep().unwrap();

                let claims: u64 = positions
                    .iter()
                    .map(|(eq_cp, tier_cp, in_tier)| {
                        position_delta(acc_equal, *eq_cp, acc_tier, *tier_cp, *in_tier, 0).unwrap()
                    })
                    .sum();

                assert!(
                    owed_fees >= claims,
                    "trial {trial}: owed_fees={owed_fees} < outstanding claims={claims} \
                     (bps={alloc_bps:?} n_real={n_real} tier_size={tier_size})"
                );

                // Churn tier membership so checkpoints are not all identical. Membership is
                // **swapped**, never toggled: the retained tier pot covers exactly `tier_size`
                // claims, so a simulation that let the member count drift above it would be
                // asserting against a denominator the split was never given.
                if tier_size > 0 {
                    let members: Vec<usize> = positions
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.2)
                        .map(|(i, _)| i)
                        .collect();
                    let outsiders: Vec<usize> = positions
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| !p.2)
                        .map(|(i, _)| i)
                        .collect();
                    if !members.is_empty() && !outsiders.is_empty() {
                        let demoted =
                            members[usize::try_from(rng.below(members.len() as u64)).unwrap()];
                        let promoted =
                            outsiders[usize::try_from(rng.below(outsiders.len() as u64)).unwrap()];
                        positions[demoted].2 = false;
                        positions[demoted].1 = acc_tier;
                        positions[promoted].2 = true;
                        positions[promoted].1 = acc_tier;
                    }
                    assert_eq!(
                        positions.iter().filter(|p| p.2).count(),
                        usize::from(tier_size),
                        "the simulation lost the tier-size invariant it asserts against"
                    );
                }
            }
        }
    }

    /// Sizing the pot from the floored bumps would fail: two bumps each worth a fraction of a
    /// micro-unit retain nothing per roll and owe a whole one.
    #[test]
    fn a_pot_sized_from_the_floored_accumulator_bump_would_under_retain() {
        let alloc_bps = [10_000u16, 0, 0];
        let n_real = 1u32;
        let mut acc_equal = 0u128;
        let mut retained_from_split = 0u64;
        let mut retained_from_bump = 0u64;

        for _ in 0..2 {
            // A fee whose whole equal pot is a single micro-unit, split across two positions:
            // each bump is half a micro-unit and floors to nothing on its own.
            let split = split_fee(
                1,
                0,
                alloc_bps,
                n_real,
                [1, 0, 0, 0, 0, 0, 0, 0],
                DEFAULT_BLANK_FACES,
                0,
            )
            .unwrap();
            acc_equal += split.equal_unit;
            retained_from_split += split.fee_keep().unwrap();
            retained_from_bump +=
                u64::try_from(u128::from(n_real) * split.equal_unit / ACC_PRECISION).unwrap();
        }

        let claim = position_delta(acc_equal, 0, 0, 0, false, 0).unwrap();
        assert_eq!(
            retained_from_bump, 0,
            "the floored bump retains nothing over these rolls"
        );
        assert_eq!(
            claim, 1,
            "the position's single floor at the end still owes a micro-unit"
        );
        assert!(
            retained_from_split >= claim,
            "the shipped sizing must cover the claim the floored-bump sizing misses"
        );
    }

    // --- (c) the drawn position's entitlement ------------------------------------------------

    #[test]
    fn position_delta_folds_accrued_in_exactly_once() {
        let acc_equal = 3 * ACC_PRECISION;
        let acc_tier = 2 * ACC_PRECISION;
        assert_eq!(
            position_delta(acc_equal, 0, acc_tier, 0, true, 7).unwrap(),
            3 + 2 + 7
        );
        assert_eq!(
            position_delta(acc_equal, 0, acc_tier, 0, false, 7).unwrap(),
            3 + 7,
            "a non-member drew on the tier accumulator"
        );
    }

    #[test]
    fn position_delta_reads_no_tier_claim_for_a_non_member() {
        // A stale tier checkpoint ahead of the accumulator would fail the tier leg if it were
        // read; a non-member's must not be read at all.
        assert_eq!(
            position_delta(ACC_PRECISION, 0, 0, 5 * ACC_PRECISION, false, 0).unwrap(),
            1
        );
        assert_eq!(
            error_code(
                position_delta(ACC_PRECISION, 0, 0, 5 * ACC_PRECISION, true, 0).unwrap_err()
            ),
            6703
        );
    }

    // --- (c) the solvency inequality ---------------------------------------------------------

    /// A ledger with room to spare in every direction, so a single-micro-unit perturbation of
    /// any one term cannot flip the verdict by accident.
    fn comfortable() -> SolvencyLedger {
        SolvencyLedger {
            principal_balance: 100_000_000,
            pending_roll_liability: 29_700_000,
            s_price: 9_900_000,
            owed_fees: 4_000_000,
            fee_keep: 720_000,
            fee_out: 180_000,
            delta: 500_000,
            recorded_value: 40_000_000,
        }
    }

    /// The same ledger balanced on the edge: solvent, and one micro-unit from not being.
    fn on_the_edge() -> SolvencyLedger {
        let mut ledger = comfortable();
        // available − payout, with delta cancelling out of both sides.
        let slack = i128::from(ledger.principal_balance)
            - (i128::from(ledger.pending_roll_liability) - i128::from(ledger.s_price))
            - (i128::from(ledger.owed_fees) + i128::from(ledger.fee_keep))
            - i128::from(ledger.fee_out)
            - i128::from(ledger.recorded_value);
        assert!(slack > 0, "fixture is not solvent to begin with");
        ledger.recorded_value += u64::try_from(slack).unwrap();
        ledger
    }

    #[test]
    fn the_edge_fixture_is_solvent_by_exactly_one_micro_unit_of_slack() {
        let ledger = on_the_edge();
        assert!(is_solvent(&ledger).unwrap());

        let mut short = ledger;
        short.recorded_value += 1;
        assert!(
            !is_solvent(&short).unwrap(),
            "one more micro-unit must not still be covered"
        );
    }

    /// Every scalar the inequality reads, perturbed one micro-unit in each direction from the
    /// edge, with the expected effect on solvency stated per term.
    #[test]
    fn every_term_of_the_inequality_is_load_bearing() {
        // (name, mutation, direction that must break solvency)
        type Mutation = fn(&mut SolvencyLedger, i64);
        let terms: [(&str, Mutation, bool); 8] = [
            (
                "principal_balance",
                |l, d| l.principal_balance = shift(l.principal_balance, d),
                true,
            ),
            (
                "pending_roll_liability",
                |l, d| l.pending_roll_liability = shift(l.pending_roll_liability, d),
                false,
            ),
            ("s_price", |l, d| l.s_price = shift(l.s_price, d), true),
            (
                "owed_fees",
                |l, d| l.owed_fees = shift(l.owed_fees, d),
                false,
            ),
            ("fee_keep", |l, d| l.fee_keep = shift(l.fee_keep, d), false),
            ("fee_out", |l, d| l.fee_out = shift(l.fee_out, d), false),
            (
                "recorded_value",
                |l, d| l.recorded_value = shift(l.recorded_value, d),
                false,
            ),
            ("delta", |l, d| l.delta = shift(l.delta, d), false),
        ];

        for (name, mutate, raising) in terms {
            let mut up = on_the_edge();
            mutate(&mut up, 1);
            let mut down = on_the_edge();
            mutate(&mut down, -1);

            if name == "delta" {
                // Delta sits on both sides of the inequality and cancels exactly. That is a
                // property of the equation, not a gap in the fixture: the vault's ability to
                // cover a settlement does not depend on how the payout divides between the
                // position's principal and its fee entitlement.
                assert!(is_solvent(&up).unwrap(), "delta raised the verdict");
                assert!(is_solvent(&down).unwrap(), "delta lowered the verdict");
                continue;
            }

            let (breaking, keeping) = if raising { (down, up) } else { (up, down) };
            assert!(
                !is_solvent(&breaking).unwrap(),
                "`{name}` moved one micro-unit against the vault and the verdict did not flip — \
                 the term is not load-bearing in this inequality"
            );
            assert!(
                is_solvent(&keeping).unwrap(),
                "`{name}` moved one micro-unit in the vault's favour and the verdict flipped"
            );
        }
    }

    fn shift(value: u64, delta: i64) -> u64 {
        if delta >= 0 {
            value + u64::try_from(delta).unwrap()
        } else {
            value - u64::try_from(-delta).unwrap()
        }
    }

    #[test]
    fn the_settling_rolls_own_price_stays_inside_the_balance() {
        // Reserving this roll's refund alongside the settlement would double-count it: settling
        // forfeits the refund. A ledger holding exactly one roll must not reserve anything.
        let ledger = SolvencyLedger {
            principal_balance: 9_900_000,
            pending_roll_liability: 9_900_000,
            s_price: 9_900_000,
            owed_fees: 0,
            fee_keep: 0,
            fee_out: 0,
            delta: 0,
            recorded_value: 9_900_000,
        };
        assert!(is_solvent(&ledger).unwrap());

        let mut two_rolls = ledger;
        two_rolls.pending_roll_liability = 19_800_000;
        assert!(
            !is_solvent(&two_rolls).unwrap(),
            "the other unsettled roll's refund must still be reserved"
        );
    }

    #[test]
    fn a_liability_that_excludes_this_roll_is_rejected_rather_than_read_as_headroom() {
        let mut ledger = comfortable();
        ledger.pending_roll_liability = ledger.s_price - 1;
        assert_eq!(error_code(is_solvent(&ledger).unwrap_err()), 6703);
    }

    #[test]
    fn an_entitlement_larger_than_everything_owed_is_rejected() {
        let mut ledger = comfortable();
        ledger.delta = ledger.owed_fees + ledger.fee_keep + 1;
        assert_eq!(error_code(is_solvent(&ledger).unwrap_err()), 6703);

        ledger.delta = ledger.owed_fees + ledger.fee_keep;
        assert!(
            is_solvent(&ledger).is_ok(),
            "an exactly-covered entitlement is not corruption"
        );
    }

    #[test]
    fn an_empty_vault_never_reads_as_solvent() {
        let mut ledger = comfortable();
        ledger.principal_balance = 0;
        assert!(!is_solvent(&ledger).unwrap());
    }

    #[test]
    fn the_verdict_is_independent_of_how_the_payout_divides() {
        // Moving a micro-unit between the principal half and the fee half of the same payout
        // leaves both the payout and the reserve unchanged, so the verdict must not move.
        let mut rng = Xorshift64(0x2468_ace0_1357_bdf9);
        for _ in 0..10_000 {
            let mut ledger = on_the_edge();
            let movable = ledger
                .recorded_value
                .min(ledger.owed_fees + ledger.fee_keep - ledger.delta);
            let step = rng.below(movable.min(100_000) + 1);
            ledger.recorded_value -= step;
            ledger.delta += step;
            assert!(
                is_solvent(&ledger).unwrap(),
                "moving {step} from principal to entitlement changed the verdict"
            );
        }
    }

    // --- the split feeds the solvency test ---------------------------------------------------

    #[test]
    fn a_settlement_priced_at_the_launch_configuration_is_solvent_against_its_own_ticket() {
        let split = split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            LAUNCH_BPS,
            20,
            [0, 0, 0, 0, 0, 0, 0, 0],
            DEFAULT_BLANK_FACES,
            20,
        )
        .unwrap();
        let delta = 486_000; // this roll's ticket + equal + tier share for one position of twenty
        let ledger = SolvencyLedger {
            principal_balance: 9_900_000 + 40_000_000,
            pending_roll_liability: 9_900_000,
            s_price: 9_900_000,
            owed_fees: 0,
            fee_keep: split.fee_keep().unwrap(),
            fee_out: split.fee_out().unwrap(),
            delta,
            recorded_value: 40_000_000,
        };
        assert!(is_solvent(&ledger).unwrap());
    }

    /// The published exit percentage is a **derivation** from the equal pot and the ticket
    /// target, never a stored constant — so it has to be checked as one. A position drawn at its
    /// expected time accrues `equal_pot × V / ticket_quote`, where the equal pot is the roll's
    /// whole ticket plus the fee's own equal-bps allocation; when the quote is the ticket target,
    /// that is exactly 107.5% of the position's value — the ticket returning the value itself by
    /// accrual, plus the familiar 7.5% the equal fee share adds on top.
    ///
    /// Moving the equal allocation republishes a different exit percentage even if the split
    /// still sums to the ticket plus the fee.
    #[test]
    fn a_position_drawn_at_its_expected_time_accrues_the_published_fraction_of_its_value() {
        use crate::common::constants::WEIGHT_C;
        use crate::common::math::{aggregate_weight, ticket_quote, weight_of};

        // A pool of identical real positions each worth the ticket target quotes at the ticket
        // target, so the general identity collapses to the published constant with no rounding
        // left to argue about.
        let value = DEFAULT_TICKET_TARGET;
        let n_real = 40u64;
        let leaf = u128::from(weight_of(value).unwrap());
        let w_total = leaf * u128::from(n_real);
        assert_eq!(
            ticket_quote(n_real, w_total).unwrap(),
            DEFAULT_TICKET_TARGET,
            "the fixture pool does not quote at the ticket target"
        );

        let split = split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            LAUNCH_BPS,
            u32::try_from(n_real).unwrap(),
            [0, 0, 0, 0, 0, 0, 0, 0],
            DEFAULT_BLANK_FACES,
            0,
        )
        .unwrap();

        // Expected rolls before this position is drawn is `w_total / leaf`; each roll credits
        // `equal_unit`. Both are kept scaled so the product is exact.
        let expected_accrual = split.equal_unit * w_total / leaf / ACC_PRECISION;
        assert_eq!(
            expected_accrual,
            u128::from(value) * 10_750 / 10_000,
            "a position drawn at its expected time no longer accrues 107.5% of its value"
        );

        // The same identity on a pool that is neither homogeneous nor blank-free: expected
        // accrual times the quote is the equal pot times the value, up to the floors.
        let faces = crate::common::constants::DEFAULT_BLANK_FACES;
        let blanks = [6u32, 4, 2, 0, 0, 0, 0, 0];
        let values = [12_000_000u64, 45_000_000, 300_000_000];
        let w_real: u128 = values
            .iter()
            .map(|v| u128::from(weight_of(*v).unwrap()))
            .sum();
        let w_all = aggregate_weight(w_real, blanks, faces).unwrap();
        let n_total = u64::try_from(values.len()).unwrap()
            + u64::from(blanks[0])
            + u64::from(blanks[1])
            + u64::from(blanks[2]);
        let quote = ticket_quote(n_total, w_all).unwrap();
        let split = split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            LAUNCH_BPS,
            u32::try_from(values.len()).unwrap(),
            blanks,
            DEFAULT_BLANK_FACES,
            0,
        )
        .unwrap();
        let equal_pot = u128::from(allocation(DEFAULT_FEE, LAUNCH_BPS[ALLOC_EQUAL]).unwrap())
            + u128::from(DEFAULT_TICKET_TARGET);

        for value in values {
            let leaf = u128::from(weight_of(value).unwrap());
            let accrual_scaled = split.equal_unit * w_all / leaf; // still ACC_PRECISION-scaled
            let left = accrual_scaled * u128::from(quote);
            let right = equal_pot * u128::from(value) * ACC_PRECISION;
            let (hi, lo) = if left > right {
                (left, right)
            } else {
                (right, left)
            };
            assert!(
                (hi - lo) * 1_000_000 < hi,
                "V={value}: expected accrual × quote diverged from the equal pot × V by more \
                 than one part in a million ({left} vs {right})"
            );
            // `WEIGHT_C` is the only place the leaf and the quote meet; a change to it must move
            // both sides together rather than one.
            assert_eq!(leaf, WEIGHT_C / u128::from(value));
        }
    }

    // --- grep-assertable — no unchecked arithmetic in this module's production code ---------

    #[test]
    fn no_unchecked_arithmetic_outside_test_module() {
        let source = include_str!("settlement_math.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for (line_no, line) in production.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("use ") {
                continue; // glob imports (`prelude::*`) are not arithmetic
            }
            let code = line.split("//").next().unwrap_or("").replace("->", "");
            for op in ['+', '-', '*', '/', '%'] {
                assert!(
                    !code.contains(op),
                    "raw `{op}` at settlement_math.rs:{}: {line}",
                    line_no + 1
                );
            }
        }
    }
}
