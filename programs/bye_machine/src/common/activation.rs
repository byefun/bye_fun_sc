//! The activation half — every transition into `PositionState::Active` in one place.
//!
//! Four instructions reach `Active`: `approve_deposit` on an Operator's attestation,
//! `instant_sell` and `instant_sell_core` on the treasury's own acquisition when no batch is
//! open, and `activate_acquired` on the acquisition a batch deferred. The relationship between
//! them is identity rather than resemblance — the sell's `Active` path is weight, tier and
//! rebalance exactly as in approval, and the deferred activation is approval's activation half
//! in full — so it is written once here and called, not restated per handler where four copies
//! can drift apart one edit at a time.
//!
//! The activation takes `&mut` accounts rather than a `Context` so that it can be driven directly
//! against real state in unit tests.

use anchor_lang::prelude::*;

use crate::common::constants::BLANK_SLOTS;
use crate::common::errors::ByeMachineError;
use crate::common::math::weight_of;
use crate::common::rebalance;
use crate::common::tier::{self, PromoteOutcome};
use crate::state::{Pool, Position, PositionState, TierEntry, TopTier, WalletStats, WeightIndex};

/// The four scalars the activation writes. **Named fields rather than four positional
/// arguments, and that is load-bearing:** `observed_at`, `lock_until` and `now` are three
/// adjacent `i64`s, so any transposition of them compiles, encodes identically and fails
/// quietly — `lock_until` taking the attestation time is a position with no Season-0 lock at
/// all, and `value_observed_at` taking `now` is an observation that always reads fresh.
pub struct ActivationTerms {
    pub value: u64,
    pub observed_at: i64,
    /// Write-once. `approve_deposit` passes the Operator's attested Season-0 lock; both treasury
    /// paths pass `0`, because a treasury-acquired position earns no Season-0 score.
    pub lock_until: i64,
    pub now: i64,
}

/// What the caller still needs after the accounts are written — every field here feeds an event
/// and none of it is re-derivable from the post-state. `blanks_before` is gone the moment the
/// rebalance lands, and `displaced` names an account the caller never otherwise sees.
pub struct Activation {
    pub entered_tier: bool,
    pub displaced: Option<Pubkey>,
    pub blanks_before: [u32; BLANK_SLOTS],
}

/// The accounts `activate_position` writes. A struct rather than a long argument list so that
/// two same-shaped `&mut` accounts (`pool`, `top_tier`) cannot be transposed at a call site.
pub struct ActivationAccounts<'a, 'info> {
    pub pool: &'a mut Pool,
    /// Plain `&mut` rather than the handler's `AccountLoader`, so unit tests can drive it.
    pub weight_index: &'a mut WeightIndex,
    pub top_tier: &'a mut TopTier,
    pub position: &'a mut Position,
    /// Passed rather than read off `position`: the account data carries no address, and the tier
    /// candidate is ranked and later settled by this key.
    pub position_key: Pubkey,
    pub wallet_stats: &'a mut WalletStats,
    /// An evicted tier member's `Position` arrives here. `settle_displaced_member` binds the
    /// slice's lifetime to the infos inside it, which is why this is `'info` and not `'a`.
    pub remaining_accounts: &'info [AccountInfo<'info>],
}

/// Insert the position into the draw domain and settle everything that follows from it: weight,
/// the Fenwick leaf, the position's own activation fields, the wallet rollup, tier consideration
/// and the blank rebalance.
///
/// Emits nothing. Each caller's event set differs — `DepositApproved` against `InstantSold` — and
/// the two conditional events (`RebalanceEvaluated`, `TierChanged`) are the caller's to emit from
/// the returned `Activation`, so that a path which does *not* activate cannot emit them by
/// sharing this code.
pub fn activate_position(
    accounts: ActivationAccounts<'_, '_>,
    terms: ActivationTerms,
) -> Result<Activation> {
    let ActivationTerms {
        value,
        observed_at,
        lock_until,
        now,
    } = terms;
    let ActivationAccounts {
        pool,
        weight_index,
        top_tier,
        position,
        position_key,
        wallet_stats,
        remaining_accounts,
    } = accounts;

    let weight = weight_of(value)?;
    pool.w_real = pool
        .w_real
        .checked_add(u128::from(weight))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    pool.n_real = pool
        .n_real
        .checked_add(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let expected_total = WeightIndex::narrow_expected_total(pool.w_real)?;
    let slot_index = weight_index.insert(value, expected_total)?;

    let acc_equal = pool.acc_equal;
    let position_id = position.position_id;
    position.state = PositionState::Active;
    position.recorded_value = value;
    position.value_observed_at = observed_at;
    position.deposit_value = value;
    position.slot_index = slot_index;
    position.activated_at = now;
    position.equal_checkpoint = acc_equal;
    position.lock_until = lock_until;

    wallet_stats.active_positions = wallet_stats
        .active_positions
        .checked_add(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    wallet_stats.active_value = wallet_stats
        .active_value
        .checked_add(value)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    let tier_size = pool.tier_size;
    let acc_tier = top_tier.acc_tier;
    let candidate = TierEntry {
        position: position_key,
        value,
        activated_at: now,
        position_id,
    };
    let (entered_tier, displaced) = match tier::promote(top_tier, tier_size, candidate) {
        PromoteOutcome::Inserted => (true, None),
        PromoteOutcome::Swapped(evicted) => {
            tier::settle_displaced_member(remaining_accounts, &evicted, acc_tier)?;
            (true, Some(evicted.position))
        }
        PromoteOutcome::Rejected => (false, None),
    };
    if entered_tier {
        position.in_tier = true;
        position.tier_checkpoint = acc_tier;
    }

    let blanks_before = pool.blanks;
    pool.blanks = rebalance::evaluate(
        pool.n_real,
        pool.w_real,
        pool.blank_faces,
        pool.ticket_target,
    )?;

    Ok(Activation {
        entered_tier,
        displaced,
        blanks_before,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };

    /// The account is 196,672 B and overflows a test stack frame, so it is heap-allocated.
    fn boxed_zeroed_weight_index() -> Box<WeightIndex> {
        unsafe {
            let layout = std::alloc::Layout::new::<WeightIndex>();
            let ptr = std::alloc::alloc_zeroed(layout).cast::<WeightIndex>();
            assert!(!ptr.is_null(), "allocation failed");
            Box::from_raw(ptr)
        }
    }

    fn configured_pool() -> Pool {
        let mut pool = Pool::blank();
        pool.tier_size = 20;
        pool.blank_faces = DEFAULT_BLANK_FACES;
        pool.ticket_target = DEFAULT_TICKET_TARGET;
        pool
    }

    fn empty_top_tier() -> TopTier {
        TopTier {
            pool: Pubkey::default(),
            len: 0,
            entries: [TierEntry {
                position: Pubkey::default(),
                value: 0,
                activated_at: 0,
                position_id: 0,
            }; 20],
            acc_tier: 0,
            bump: 0,
        }
    }

    fn pending_position() -> Position {
        Position {
            pool: Pubkey::default(),
            depositor: Pubkey::default(),
            nft_mint: Pubkey::default(),
            position_id: 7,
            state: PositionState::Pending,
            recorded_value: 0,
            value_observed_at: 0,
            deposit_value: 0,
            slot_index: 0,
            activated_at: 0,
            equal_checkpoint: 0,
            accrued: 0,
            in_tier: false,
            tier_checkpoint: 0,
            lock_until: 0,
            reject_reason: 0,
            standard: 1,
            bump: 0,
            vault_bump: 0,
        }
    }

    fn empty_wallet_stats() -> WalletStats {
        WalletStats {
            pool: Pubkey::default(),
            owner: Pubkey::default(),
            active_positions: 0,
            active_value: 0,
            bump: 0,
        }
    }

    /// Drive one activation and return everything it touched.
    fn activate(
        pool: &mut Pool,
        position: &mut Position,
        wallet_stats: &mut WalletStats,
        terms: ActivationTerms,
    ) -> Result<Activation> {
        let mut weight_index = boxed_zeroed_weight_index();
        let mut top_tier = empty_top_tier();
        activate_position(
            ActivationAccounts {
                pool,
                weight_index: &mut weight_index,
                top_tier: &mut top_tier,
                position,
                position_key: Pubkey::new_unique(),
                wallet_stats,
                remaining_accounts: &[],
            },
            terms,
        )
    }

    fn terms(value: u64) -> ActivationTerms {
        // Distinct values so a transposed timestamp lands a recognisably wrong number.
        ActivationTerms {
            value,
            observed_at: 111,
            lock_until: 222,
            now: 333,
        }
    }

    /// Every activation field carries its intended value; a field left unwritten would leave the
    /// position in the tree, `w_real` and `wallet_stats` while its own record disagrees.
    #[test]
    fn activation_writes_every_field_the_position_needs() {
        let mut pool = configured_pool();
        let mut position = pending_position();
        let mut wallet_stats = empty_wallet_stats();
        let value = DEFAULT_ADMISSION_FLOOR * 3;
        pool.acc_equal = 9_999;

        let outcome = activate(&mut pool, &mut position, &mut wallet_stats, terms(value)).unwrap();

        assert!(
            position.state == PositionState::Active,
            "state did not reach Active"
        );
        assert_eq!(position.recorded_value, value, "recorded_value");
        assert_eq!(position.deposit_value, value, "deposit_value");
        assert_eq!(position.value_observed_at, 111, "value_observed_at");
        assert_eq!(position.lock_until, 222, "lock_until");
        assert_eq!(position.activated_at, 333, "activated_at");
        assert_eq!(position.equal_checkpoint, 9_999, "equal_checkpoint");
        assert_eq!(wallet_stats.active_positions, 1, "active_positions");
        assert_eq!(wallet_stats.active_value, value, "active_value");
        assert_eq!(pool.n_real, 1, "n_real");
        assert!(pool.w_real > 0, "w_real");
        assert!(outcome.entered_tier, "an empty tier admits the candidate");
        assert!(position.in_tier, "in_tier");
    }

    /// `observed_at`, `lock_until` and `now` each reach their own field, never a neighbour's.
    #[test]
    fn the_three_timestamps_land_uncrossed() {
        let mut pool = configured_pool();
        let mut position = pending_position();
        let mut wallet_stats = empty_wallet_stats();

        activate(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            terms(DEFAULT_ADMISSION_FLOOR),
        )
        .unwrap();

        assert_eq!(
            (
                position.value_observed_at,
                position.lock_until,
                position.activated_at
            ),
            (111, 222, 333),
            "one of observed_at / lock_until / now reached the wrong field"
        );
    }

    /// Fields the activation does not own (`accrued`, `standard`, ...) come out unchanged.
    #[test]
    fn activation_leaves_the_fields_it_does_not_own_alone() {
        let mut pool = configured_pool();
        let mut position = pending_position();
        position.accrued = 4_242;
        position.reject_reason = 5;
        let depositor = Pubkey::new_unique();
        position.depositor = depositor;
        let mut wallet_stats = empty_wallet_stats();

        activate(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            terms(DEFAULT_ADMISSION_FLOOR),
        )
        .unwrap();

        assert_eq!(position.accrued, 4_242, "accrued is not the activation's");
        assert_eq!(position.reject_reason, 5, "reject_reason is not touched");
        assert_eq!(position.depositor, depositor, "depositor is not rewritten");
        assert_eq!(position.standard, 1, "standard is written once, at intake");
    }
}
