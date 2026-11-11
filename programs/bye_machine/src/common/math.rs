use anchor_lang::prelude::*;

use crate::common::constants::{ACC_PRECISION, BELOW_TICKET_SLOTS, BLANK_SLOTS, WEIGHT_C};
use crate::common::errors::ByeMachineError;

/// A position's Fenwick weight for a given value, floor division. The one place this is
/// derived, so a leaf's insertion and its later removal can never compute a different number
/// for the same value.
pub fn weight_of(value: u64) -> Result<u64> {
    require!(value > 0, ByeMachineError::ZeroValue);
    let w = WEIGHT_C
        .checked_div(u128::from(value))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    require!(w > 0, ByeMachineError::ArithmeticFailure);
    let w = u64::try_from(w).map_err(|_| ByeMachineError::ArithmeticFailure)?;
    Ok(w)
}

/// Whether a blank slot is empty and has no count, weight or draw: slot 3 or a reserved slot with
/// a face of 0. Slots 0–2 always hold a face, so a 0 there is never empty — it reaches `weight_of`
/// and fails, as it did before slot 3 existed.
pub fn slot_is_empty(slot: usize, face: u64) -> bool {
    slot >= BELOW_TICKET_SLOTS && face == 0
}

/// The pool's total draw weight: real positions plus each blank slot's counter weight times its
/// count. Empty slots are skipped; a zero face in slots 0–2 fails.
pub fn aggregate_weight(
    w_real: u128,
    blanks: [u32; BLANK_SLOTS],
    blank_faces: [u64; BLANK_SLOTS],
) -> Result<u128> {
    let mut total = w_real;
    for d in 0..BLANK_SLOTS {
        if slot_is_empty(d, blank_faces[d]) {
            continue;
        }
        let face_weight = u128::from(weight_of(blank_faces[d])?);
        let contribution = face_weight
            .checked_mul(u128::from(blanks[d]))
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        total = total
            .checked_add(contribution)
            .ok_or(ByeMachineError::ArithmeticFailure)?;
    }
    Ok(total)
}

/// The published expected-value ticket quote: `n_total` slots priced against the pool's
/// aggregate weight, floor.
pub fn ticket_quote(n_total: u64, aggregate_weight: u128) -> Result<u64> {
    require!(aggregate_weight > 0, ByeMachineError::ArithmeticFailure);
    let numerator = WEIGHT_C
        .checked_mul(u128::from(n_total))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let quote = numerator
        .checked_div(aggregate_weight)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let quote = u64::try_from(quote).map_err(|_| ByeMachineError::ArithmeticFailure)?;
    Ok(quote)
}

/// A basis-point slice of `fee` in micro-USDC, floor. The one place an allocation is taken, so
/// the pot a settlement transfers out and the accumulator unit derived from the same pot can
/// never disagree about where the floor fell.
pub fn allocation(fee: u64, alloc_bps: u16) -> Result<u64> {
    let share = u128::from(fee)
        .checked_mul(u128::from(alloc_bps))
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_div(10_000)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let share = u64::try_from(share).map_err(|_| ByeMachineError::ArithmeticFailure)?;
    Ok(share)
}

/// `ACC_PRECISION`-scaled per-`denom` share of `amount`, floor; the remainder is the caller's to
/// route to protocol revenue at its own settlement site.
fn accumulator_unit(amount: u64, denom: u64) -> Result<u128> {
    require!(denom > 0, ByeMachineError::ZeroValue);
    let unit = u128::from(amount)
        .checked_mul(ACC_PRECISION)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_div(u128::from(denom))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    Ok(unit)
}

/// Per-real-position equal-share accumulator unit. `equal_pot` is the whole equal-share
/// destination for one roll — the roll's ticket, undivided by any allocation, plus the fee's own
/// bps-allocated equal share — divided evenly across every active position, real and virtual
/// alike.
pub fn equal_share_unit(equal_pot: u64, n_total: u64) -> Result<u128> {
    accumulator_unit(equal_pot, n_total)
}

/// Per-tier-member fee accumulator unit.
pub fn tier_share_unit(fee: u64, alloc_tier_bps: u16, tier_len: u8) -> Result<u128> {
    accumulator_unit(allocation(fee, alloc_tier_bps)?, u64::from(tier_len))
}

/// A position's unpaid fee-accumulator delta since its checkpoint, plus anything already
/// settled to `accrued`, floor.
pub fn position_fee_payout(acc_now: u128, checkpoint: u128, accrued: u64) -> Result<u64> {
    let delta = acc_now
        .checked_sub(checkpoint)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let scaled = delta
        .checked_div(ACC_PRECISION)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let scaled = u64::try_from(scaled).map_err(|_| ByeMachineError::ArithmeticFailure)?;
    let payout = scaled
        .checked_add(accrued)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    Ok(payout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, FENWICK_CAPACITY,
    };

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

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
    }

    // --- zero-value rejection ----------------------------------------------------------------

    #[test]
    fn weight_of_rejects_zero() {
        assert_eq!(error_code(weight_of(0).unwrap_err()), 6401);
    }

    #[test]
    fn accumulator_unit_rejects_zero_denominator() {
        assert_eq!(error_code(equal_share_unit(900_000, 0).unwrap_err()), 6401);
        assert_eq!(
            error_code(tier_share_unit(900_000, 500, 0).unwrap_err()),
            6401
        );
    }

    // --- round-trip + monotonicity over V ∈ [admission_floor, u64::MAX] --------------------

    /// The largest value that still floors to a nonzero weight (`WEIGHT_C` itself, weight 1).
    const MAX_NONZERO_WEIGHT_VALUE: u64 = WEIGHT_C as u64;

    #[test]
    fn weight_of_matches_raw_floor_division() {
        for v in [
            1u64,
            12_000_000,
            DEFAULT_ADMISSION_FLOOR,
            MAX_NONZERO_WEIGHT_VALUE,
        ] {
            assert_eq!(u128::from(weight_of(v).unwrap()), WEIGHT_C / u128::from(v));
        }
    }

    #[test]
    fn weight_of_round_trips_within_its_own_floor_bucket() {
        // w = floor(C/V); floor(C/w) must land back at or above V, never below it.
        for v in [
            DEFAULT_ADMISSION_FLOOR,
            DEFAULT_ADMISSION_FLOOR + 1,
            1_000_000_000,
            MAX_NONZERO_WEIGHT_VALUE,
        ] {
            let w = weight_of(v).unwrap();
            let recovered = WEIGHT_C / u128::from(w);
            assert!(
                recovered >= u128::from(v),
                "V={v} w={w} recovered={recovered}"
            );
        }
    }

    #[test]
    fn weight_of_is_monotonically_non_increasing_over_admission_floor_range() {
        let mut rng = Xorshift64(0x5eed_1234 | 1);
        let span = MAX_NONZERO_WEIGHT_VALUE - DEFAULT_ADMISSION_FLOOR - 2;
        for _ in 0..10_000 {
            let v1 = DEFAULT_ADMISSION_FLOOR + rng.below(span);
            let v2 = (v1 + 1 + rng.below(1_000_000)).max(v1 + 1);
            assert!(
                weight_of(v1).unwrap() >= weight_of(v2).unwrap(),
                "V1={v1} V2={v2}"
            );
        }
    }

    #[test]
    fn weight_of_fails_closed_rather_than_returning_a_degenerate_zero_weight() {
        // Above WEIGHT_C, floor(C/V) is 0 — a real position no draw could ever select.
        // Rejecting is the only invariant-preserving option; not the caller's problem to notice.
        for v in [
            MAX_NONZERO_WEIGHT_VALUE + 1,
            MAX_NONZERO_WEIGHT_VALUE + 1_000_000,
            u64::MAX,
        ] {
            assert_eq!(error_code(weight_of(v).unwrap_err()), 6703);
        }
    }

    // --- no overflow at 16,384 floor-value leaves ------------------------------------------

    #[test]
    fn aggregate_weight_handles_fenwick_capacity_of_floor_value_reals_no_overflow() {
        let w = u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        let w_real = w.checked_mul(FENWICK_CAPACITY as u128).unwrap();
        let agg = aggregate_weight(w_real, [0; 8], DEFAULT_BLANK_FACES).unwrap();
        assert_eq!(agg, w_real);

        let quote = ticket_quote(FENWICK_CAPACITY as u64, agg).unwrap();
        assert!(quote > 0);
    }

    #[test]
    fn aggregate_weight_adds_blank_counters() {
        let w1 = u128::from(weight_of(DEFAULT_BLANK_FACES[0]).unwrap());
        let w2 = u128::from(weight_of(DEFAULT_BLANK_FACES[1]).unwrap());
        let w5 = u128::from(weight_of(DEFAULT_BLANK_FACES[2]).unwrap());
        let agg = aggregate_weight(0, [3, 2, 1, 0, 0, 0, 0, 0], DEFAULT_BLANK_FACES).unwrap();
        assert_eq!(agg, w1 * 3 + w2 * 2 + w5);
    }

    #[test]
    fn aggregate_weight_adds_the_above_ticket_slot_and_skips_zero_faces() {
        let faces = [5_000_000, 10_000_000, 25_000_000, 100_000_000, 0, 0, 0, 0];
        let w100 = u128::from(weight_of(100_000_000).unwrap());
        let w25 = u128::from(weight_of(25_000_000).unwrap());
        assert_eq!(
            aggregate_weight(7, [0, 0, 1, 4, 0, 0, 0, 0], faces).unwrap(),
            7 + w25 + 4 * w100
        );
        // A count in an empty slot 3–7 carries no weight rather than failing on `weight_of`.
        assert_eq!(
            aggregate_weight(7, [0, 0, 0, 0, 9, 9, 9, 9], faces).unwrap(),
            7
        );
        let mut no_above = faces;
        no_above[3] = 0;
        assert_eq!(
            aggregate_weight(7, [0, 0, 0, 9, 0, 0, 0, 0], no_above).unwrap(),
            7
        );
    }

    #[test]
    fn aggregate_weight_fails_on_a_zero_face_in_slots_0_to_2() {
        for slot in 0..3 {
            let mut faces = DEFAULT_BLANK_FACES;
            faces[slot] = 0;
            assert!(
                aggregate_weight(7, [0; 8], faces).is_err(),
                "slot {slot} with no face must fail even at a count of 0"
            );
        }
        assert!(!slot_is_empty(1, 0));
        assert!(slot_is_empty(3, 0));
        assert!(!slot_is_empty(3, 100_000_000));
    }

    // --- fee accumulator units ---------------------------------------------------------------

    #[test]
    fn equal_share_and_tier_share_units_match_hand_computation() {
        let fee = 900_000u64;
        let equal_pot = fee * 7_500 / 10_000;
        let unit = equal_share_unit(equal_pot, 20).unwrap();
        assert_eq!(unit, u128::from(equal_pot) * ACC_PRECISION / 20);

        let tier_unit = tier_share_unit(fee, 500, 20).unwrap();
        assert_eq!(
            tier_unit,
            u128::from(fee) * 500 / 10_000 * ACC_PRECISION / 20
        );
    }

    #[test]
    fn position_fee_payout_adds_delta_to_accrued() {
        let checkpoint = 10 * ACC_PRECISION;
        let acc_now = 13 * ACC_PRECISION;
        assert_eq!(position_fee_payout(acc_now, checkpoint, 7).unwrap(), 10);
    }

    #[test]
    fn position_fee_payout_rejects_checkpoint_ahead_of_accumulator() {
        assert_eq!(error_code(position_fee_payout(5, 6, 0).unwrap_err()), 6703);
    }

    // --- grep-assertable — no unchecked arithmetic in this module's production code ---------

    #[test]
    fn no_unchecked_arithmetic_outside_test_module() {
        let source = include_str!("math.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for (line_no, line) in production.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("use ") {
                continue; // glob imports (`prelude::*`) are not arithmetic
            }
            let code = line.split("//").next().unwrap_or("").replace("->", "");
            for op in ['+', '-', '*', '/'] {
                assert!(
                    !code.contains(op),
                    "raw `{op}` at math.rs:{}: {line}",
                    line_no + 1
                );
            }
        }
    }
}
