//! The accounting half of a withdrawal — one home, both `withdraw` twins.
//!
//! `withdraw` and `withdraw_core` differ in their custody leg and in nothing else: every twin
//! is *same, mirrored*, with the same state transition, weight effect, fee effect
//! and events. So the roll gate, the lock, the two-leg fee settle, the four checked counter
//! decrements and the rebalance evaluation live here rather than once per standard. A second copy
//! of this arithmetic is not a style question — it decides what a depositor is paid on the way
//! out, and a suite can pass while getting that wrong.
//!
//! **What does not belong here, so a later row does not fold it in by resemblance.**
//! `close_seized`'s `Active` row is *described* as withdraw's accounting half without the
//! transfer, and it is not this function: it reads no `open_batches` and no
//! `lock_until` — both deliberate, since there is no collateral left for a lock to hold — and its
//! fee delta settles into `Position.accrued` instead of being paid out. Same fields, different
//! preconditions and a different disposition, which is a different function.

use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::guards::assert_weight_open;
use crate::common::math::{position_fee_payout, weight_of};
use crate::common::rebalance;
use crate::state::{Pool, Position, PositionState, WalletStats, WeightIndex};

/// What the handler needs back once the guards, the fee settle and the counter deltas have run:
/// the payout `Withdrawn.fees_paid` publishes, the pre-close value and slot the `WeightIndex`
/// removal verifies against, the aggregate that removal must land on, the blank counts from
/// before the rebalance, and whether the closing position was a tier member.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct WithdrawEffect {
    pub(crate) fees_paid: u64,
    pub(crate) recorded_value: u64,
    pub(crate) slot_index: u32,
    pub(crate) expected_total: u64,
    pub(crate) blanks_before: [u32; 3],
    pub(crate) was_member: bool,
}

/// The guards, the two-leg fee settle, the counter deltas and the rebalance — everything that
/// touches `Pool`/`Position`/`WalletStats` directly. `acc_tier` arrives as a value because
/// `TopTier` takes no plain-struct argument here; the membership removal itself stays with the
/// calling handler, through `tier::vacate`.
///
/// Both accumulator legs settle here and both checkpoints are stamped, with `accrued` folded in
/// exactly **once**: `position_fee_payout` already adds `accrued` to its own delta, so the tier
/// leg's result is what the equal leg carries forward rather than `accrued` a second time. The
/// tier leg is drawn only for a member — a non-member's `tier_checkpoint` is never a claim.
/// `tier::vacate` re-runs the tier leg afterwards against the checkpoint this leaves at
/// `acc_tier`, which is a zero delta on a discharged `accrued` by construction.
pub(crate) fn apply_withdraw(
    pool: &mut Pool,
    position: &mut Position,
    wallet_stats: &mut WalletStats,
    acc_tier: u128,
    now: i64,
) -> Result<WithdrawEffect> {
    assert_weight_open(pool)?;
    require!(
        position.state == PositionState::Active,
        ByeMachineError::InvalidPositionState
    );
    require!(now >= position.lock_until, ByeMachineError::PositionLocked);

    let was_member = position.in_tier;
    let recorded_value = position.recorded_value;
    let slot_index = position.slot_index;

    let carried = if was_member {
        let settled = position_fee_payout(acc_tier, position.tier_checkpoint, position.accrued)?;
        position.tier_checkpoint = acc_tier;
        settled
    } else {
        position.accrued
    };
    let fees_paid = position_fee_payout(pool.acc_equal, position.equal_checkpoint, carried)?;
    position.equal_checkpoint = pool.acc_equal;
    position.accrued = 0;
    pool.owed_fees = pool
        .owed_fees
        .checked_sub(fees_paid)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    pool.w_real = pool
        .w_real
        .checked_sub(u128::from(weight_of(recorded_value)?))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    pool.n_real = pool
        .n_real
        .checked_sub(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    wallet_stats.active_positions = wallet_stats
        .active_positions
        .checked_sub(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    wallet_stats.active_value = wallet_stats
        .active_value
        .checked_sub(recorded_value)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    let blanks_before = pool.blanks;
    let targets = rebalance::evaluate(
        pool.n_real,
        pool.w_real,
        pool.blank_faces,
        pool.ticket_target,
    )?;
    pool.blanks = [targets.b1, targets.b2, targets.b5];

    let expected_total = WeightIndex::narrow_expected_total(pool.w_real)?;

    Ok(WithdrawEffect {
        fees_paid,
        recorded_value,
        slot_index,
        expected_total,
        blanks_before,
        was_member,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        ACC_PRECISION, DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };
    use crate::common::test_support::{calls, field_writes, production, writes};
    use crate::common::tier;
    use crate::state::position::ALL_POSITION_STATES;
    use crate::state::{TierEntry, TopTier};

    const EXIT_ACCOUNTING_SRC: &str = include_str!("exit_accounting.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn blank_position() -> Position {
        Position {
            pool: Pubkey::default(),
            depositor: Pubkey::default(),
            nft_mint: Pubkey::default(),
            position_id: 0,
            state: PositionState::Active,
            recorded_value: DEFAULT_ADMISSION_FLOOR,
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
            standard: 0,
            bump: 0,
            vault_bump: 0,
        }
    }

    fn blank_wallet_stats() -> WalletStats {
        WalletStats {
            pool: Pubkey::default(),
            owner: Pubkey::default(),
            active_positions: 1,
            active_value: DEFAULT_ADMISSION_FLOOR,
            bump: 0,
        }
    }

    /// One floor-valued real position, a solvable rebalance surface, and enough `owed_fees` to
    /// discharge the synthetic payouts below.
    fn withdrawable_pool() -> Pool {
        let mut pool = Pool::blank();
        pool.admission_floor = DEFAULT_ADMISSION_FLOOR;
        pool.blank_faces = DEFAULT_BLANK_FACES;
        pool.ticket_target = DEFAULT_TICKET_TARGET;
        pool.n_real = 1;
        pool.w_real = u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        pool.owed_fees = 100;
        pool
    }

    // --- the fee settle, over synthetic non-zero accumulators -----------------------------

    /// `position_fee_payout` folds `accrued` into its own result, so a settle that passes
    /// `position.accrued` to both the tier and the equal leg counts it twice. Every input is
    /// permanently `0` in slice 1, so this is the only instrument that can tell the two apart:
    /// the correct total is 14, the double-counting one 21.
    #[test]
    fn apply_withdraw_folds_accrued_in_exactly_once_across_both_legs() {
        let mut pool = withdrawable_pool();
        pool.acc_equal = 5 * ACC_PRECISION;
        let mut position = blank_position();
        position.in_tier = true;
        position.equal_checkpoint = 3 * ACC_PRECISION;
        position.tier_checkpoint = 4 * ACC_PRECISION;
        position.accrued = 7;
        let mut wallet_stats = blank_wallet_stats();

        let effect = apply_withdraw(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            9 * ACC_PRECISION,
            0,
        )
        .unwrap();

        assert_eq!(
            effect.fees_paid, 14,
            "equal delta 2 + tier delta 5 + accrued 7, counted once"
        );
        assert_ne!(effect.fees_paid, 21, "accrued double-counted across legs");
        assert_eq!(pool.owed_fees, 86);
    }

    #[test]
    fn a_non_members_payout_never_draws_against_a_stale_tier_checkpoint() {
        let mut pool = withdrawable_pool();
        pool.acc_equal = 5 * ACC_PRECISION;
        let mut position = blank_position();
        position.in_tier = false;
        position.equal_checkpoint = 3 * ACC_PRECISION;
        position.tier_checkpoint = 4 * ACC_PRECISION;
        position.accrued = 7;
        let mut wallet_stats = blank_wallet_stats();

        let effect = apply_withdraw(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            9 * ACC_PRECISION,
            0,
        )
        .unwrap();

        assert!(!effect.was_member);
        assert_eq!(
            effect.fees_paid, 9,
            "equal delta 2 + accrued 7, no tier leg"
        );
        assert_eq!(
            position.tier_checkpoint,
            4 * ACC_PRECISION,
            "a non-member's tier checkpoint is not restamped"
        );
    }

    #[test]
    fn the_settle_discharges_accrued_and_stamps_both_member_checkpoints() {
        let mut pool = withdrawable_pool();
        pool.acc_equal = 5 * ACC_PRECISION;
        let mut position = blank_position();
        position.in_tier = true;
        position.accrued = 7;
        let mut wallet_stats = blank_wallet_stats();

        apply_withdraw(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            9 * ACC_PRECISION,
            0,
        )
        .unwrap();

        assert_eq!(position.accrued, 0);
        assert_eq!(position.equal_checkpoint, 5 * ACC_PRECISION);
        assert_eq!(position.tier_checkpoint, 9 * ACC_PRECISION);
    }

    /// The tier leg lands in `accrued` for `record_value`'s close and in the payout here, so
    /// `tier::vacate` runs after the checkpoint is already stamped. Its own settle must
    /// therefore be a zero delta on a discharged `accrued`, or the member branch pays twice.
    #[test]
    fn vacate_after_the_settle_adds_nothing_further_to_accrued() {
        let mut pool = withdrawable_pool();
        pool.acc_equal = 5 * ACC_PRECISION;
        let acc_tier = 9 * ACC_PRECISION;
        let mut position = blank_position();
        position.in_tier = true;
        position.accrued = 7;
        let mut wallet_stats = blank_wallet_stats();

        apply_withdraw(&mut pool, &mut position, &mut wallet_stats, acc_tier, 0).unwrap();

        let position_key = Pubkey::new_unique();
        let mut top_tier = TopTier {
            pool: Pubkey::default(),
            len: 1,
            entries: [TierEntry {
                position: position_key,
                value: DEFAULT_ADMISSION_FLOOR,
                activated_at: 0,
                position_id: 0,
            }; 20],
            acc_tier,
            bump: 0,
        };
        tier::vacate(&mut top_tier, position_key, &mut position, acc_tier).unwrap();

        assert_eq!(position.accrued, 0);
        assert!(!position.in_tier);
        assert_eq!(top_tier.len, 0);
    }

    #[test]
    fn an_owed_fees_under_run_fails_closed_at_6703() {
        let mut pool = withdrawable_pool();
        pool.acc_equal = 5 * ACC_PRECISION;
        pool.owed_fees = 1;
        let mut position = blank_position();
        position.accrued = 7;
        let mut wallet_stats = blank_wallet_stats();

        let err = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap_err();
        assert_eq!(error_code(err), 6703);
    }

    /// The structural zero is asserted as a value, not as an absence — `fees_paid` is
    /// what `Withdrawn` publishes today.
    #[test]
    fn the_slice_one_structural_zero_pays_exactly_zero() {
        let mut pool = withdrawable_pool();
        pool.owed_fees = 0;
        let mut position = blank_position();
        let mut wallet_stats = blank_wallet_stats();

        let effect = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap();

        assert_eq!(effect.fees_paid, 0);
        assert_eq!(pool.owed_fees, 0);
    }

    // --- the lock boundary, both sides ---------------------------------------------------

    #[test]
    fn the_lock_boundary_passes_at_now_equal_to_lock_until() {
        let mut pool = withdrawable_pool();
        let mut position = blank_position();
        position.lock_until = 500;
        let mut wallet_stats = blank_wallet_stats();

        assert!(apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 500).is_ok());
    }

    #[test]
    fn one_second_before_the_lock_expires_rejects_at_6300() {
        let mut pool = withdrawable_pool();
        let mut position = blank_position();
        position.lock_until = 500;
        let mut wallet_stats = blank_wallet_stats();

        let err = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 499).unwrap_err();
        assert_eq!(error_code(err), 6300);
    }

    // --- the counter deltas ---------------------------------------------------------------

    #[test]
    fn w_real_moves_down_by_exactly_the_positions_leaf_weight() {
        let mut pool = withdrawable_pool();
        let value = DEFAULT_ADMISSION_FLOOR * 3;
        pool.n_real = 2;
        pool.w_real = u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap())
            + u128::from(weight_of(value).unwrap());
        let mut position = blank_position();
        position.recorded_value = value;
        let mut wallet_stats = blank_wallet_stats();
        wallet_stats.active_positions = 2;
        wallet_stats.active_value = DEFAULT_ADMISSION_FLOOR + value;

        let effect = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap();

        assert_eq!(
            pool.w_real,
            u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap())
        );
        assert_eq!(effect.recorded_value, value);
        assert_eq!(
            effect.expected_total,
            weight_of(DEFAULT_ADMISSION_FLOOR).unwrap(),
            "expected_total is derived from the post-decrement w_real"
        );
    }

    #[test]
    fn a_w_real_under_run_fails_closed_at_6703() {
        let mut pool = withdrawable_pool();
        pool.w_real = 1;
        let mut position = blank_position();
        let mut wallet_stats = blank_wallet_stats();

        let err = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap_err();
        assert_eq!(error_code(err), 6703);
    }

    #[test]
    fn withdrawing_the_last_real_position_drives_n_real_to_zero_and_blanks_to_zero() {
        let mut pool = withdrawable_pool();
        pool.blanks = [4, 5, 6];
        let mut position = blank_position();
        let mut wallet_stats = blank_wallet_stats();

        let effect = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap();

        assert_eq!(pool.n_real, 0);
        assert_eq!(pool.w_real, 0);
        assert_eq!(
            pool.blanks,
            [0, 0, 0],
            "the protective no-priced-roll state"
        );
        assert_eq!(effect.blanks_before, [4, 5, 6]);
        assert_eq!(effect.expected_total, 0);
    }

    #[test]
    fn an_n_real_under_run_fails_closed_at_6703() {
        let mut pool = withdrawable_pool();
        pool.n_real = 0;
        let mut position = blank_position();
        let mut wallet_stats = blank_wallet_stats();

        let err = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap_err();
        assert_eq!(error_code(err), 6703);
    }

    /// A missed decrement to `active_positions` has no other failure signature today —
    /// `wallet_value_cap` is enforced on value. The two fields move as two separately checked
    /// writes.
    #[test]
    fn wallet_stats_moves_the_count_and_the_value_as_two_checked_writes() {
        let mut pool = withdrawable_pool();
        pool.n_real = 3;
        pool.w_real = u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap()) * 3;
        let mut position = blank_position();
        let mut wallet_stats = blank_wallet_stats();
        wallet_stats.active_positions = 2;
        wallet_stats.active_value = DEFAULT_ADMISSION_FLOOR * 2;

        apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap();

        assert_eq!(wallet_stats.active_positions, 1);
        assert_eq!(wallet_stats.active_value, DEFAULT_ADMISSION_FLOOR);
        assert_eq!(
            writes(EXIT_ACCOUNTING_SRC, "wallet_stats.active_positions"),
            1
        );
        assert_eq!(writes(EXIT_ACCOUNTING_SRC, "wallet_stats.active_value"), 1);
    }

    #[test]
    fn an_active_positions_under_run_fails_closed_at_6703() {
        let mut pool = withdrawable_pool();
        let mut position = blank_position();
        let mut wallet_stats = blank_wallet_stats();
        wallet_stats.active_positions = 0;

        let err = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap_err();
        assert_eq!(error_code(err), 6703);
    }

    #[test]
    fn an_active_value_under_run_fails_closed_at_6703() {
        let mut pool = withdrawable_pool();
        let mut position = blank_position();
        let mut wallet_stats = blank_wallet_stats();
        wallet_stats.active_value = 0;

        let err = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap_err();
        assert_eq!(error_code(err), 6703);
    }

    // --- the guards, every state variant --------------------------------------------------

    /// Driven off `ALL_POSITION_STATES` rather than a list written here, because the list
    /// written here said three long after `Seized` made it four — the comment above claimed
    /// "every state variant" and the one state a Core position ends in was the one missing.
    #[test]
    fn every_position_state_but_active_rejects_at_6301() {
        for state in ALL_POSITION_STATES
            .into_iter()
            .filter(|state| *state != PositionState::Active)
        {
            let mut pool = withdrawable_pool();
            let mut position = blank_position();
            position.state = state;
            let mut wallet_stats = blank_wallet_stats();
            let err =
                apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap_err();
            assert_eq!(error_code(err), 6301);
        }

        let mut pool = withdrawable_pool();
        let mut position = blank_position();
        position.state = PositionState::Active;
        let mut wallet_stats = blank_wallet_stats();
        assert!(apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).is_ok());
    }

    /// **The guard order between the state check and the lock.** Every case above sets one
    /// cause: the state cases carry `lock_until
    /// == 0` and the lock cases are `Active`, so the two guards could be transposed with all 17
    /// still green. The position that discriminates is a real one — `record_value` closes an
    /// `Active` position below the floor and **leaves `lock_until` where it was**, so a Season-0
    /// deposit that closes below the floor is `ClosedBelowFloor` with a live lock. Withdrawing it
    /// must answer `InvalidPositionState`: the remedy is `claim_nft`, and `PositionLocked` sends
    /// that depositor away to wait for a date at which `withdraw` will still refuse them.
    #[test]
    fn a_closed_position_under_a_live_lock_reports_the_state_and_not_the_lock() {
        let mut pool = withdrawable_pool();
        let mut position = blank_position();
        position.state = PositionState::ClosedBelowFloor;
        position.lock_until = 500;
        let mut wallet_stats = blank_wallet_stats();

        let err = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 499).unwrap_err();
        assert_eq!(error_code(err), 6301);
    }

    #[test]
    fn a_batch_in_flight_rejects_at_6200_before_any_other_guard() {
        let mut pool = withdrawable_pool();
        pool.open_batches = 1;
        let mut position = blank_position();
        position.state = PositionState::Rejected;
        let mut wallet_stats = blank_wallet_stats();

        let err = apply_withdraw(&mut pool, &mut position, &mut wallet_stats, 0, 0).unwrap_err();
        assert_eq!(error_code(err), 6200);
        assert_eq!(calls(EXIT_ACCOUNTING_SRC, "assert_weight_open"), 1);
    }

    // --- the module's own surface, re-derived here because the writes it counts moved here ----
    //
    // Every figure below is aimed at this file rather than at `withdraw.rs`, where the writes
    // used to live: a write count asserted against the file the write left is a count of zero
    // that passes, which is the failure mode the whole set exists to prevent.

    /// `remove` verifies against the aggregate the caller hands it, so the aggregate must be
    /// read *after* `w_real` has been decremented — a pre-decrement total verifies the removal
    /// against a tree that still contains the leaf being removed.
    #[test]
    fn expected_total_is_derived_after_w_real_is_decremented() {
        let prod = production(EXIT_ACCOUNTING_SRC);
        let w_real_write = prod.find("pool.w_real = pool").unwrap();
        let narrow = prod
            .find("WeightIndex::narrow_expected_total(pool.w_real)")
            .unwrap();
        assert!(w_real_write < narrow);
        assert_eq!(
            calls(EXIT_ACCOUNTING_SRC, "WeightIndex::narrow_expected_total"),
            1
        );
    }

    #[test]
    fn every_path_obligation_appears_exactly_once() {
        assert_eq!(calls(EXIT_ACCOUNTING_SRC, "assert_weight_open"), 1);
        assert_eq!(calls(EXIT_ACCOUNTING_SRC, "rebalance::evaluate"), 1);
        assert_eq!(calls(EXIT_ACCOUNTING_SRC, "position_fee_payout"), 2);
        assert_eq!(writes(EXIT_ACCOUNTING_SRC, "pool.blanks"), 1);
    }

    #[test]
    fn the_lock_guard_uses_position_locked_and_not_the_deactivation_code() {
        let prod = production(EXIT_ACCOUNTING_SRC);
        assert_eq!(prod.matches("ByeMachineError::PositionLocked").count(), 1);
        assert_eq!(prod.matches("LockedDeactivationRejected").count(), 0);
    }

    /// `recorded_value` and `slot_index` are read here and written elsewhere — a write
    /// to either would silently re-home a live leaf — and `state` belongs to the caller's
    /// `close`, not to the accounting.
    #[test]
    fn the_accounting_writes_exactly_the_fields_it_owns_and_no_others() {
        assert_eq!(
            field_writes(EXIT_ACCOUNTING_SRC, "pool"),
            ["blanks", "n_real", "owed_fees", "w_real"]
        );
        assert_eq!(
            field_writes(EXIT_ACCOUNTING_SRC, "position"),
            ["accrued", "equal_checkpoint", "tier_checkpoint"]
        );
        assert_eq!(
            field_writes(EXIT_ACCOUNTING_SRC, "wallet_stats"),
            ["active_positions", "active_value"]
        );
    }

    /// The write counts the corrected `is_assignment` predicate is controlled against — the
    /// list `withdraw.rs` carried, following the writes to their new home. The predicate's own
    /// tokenisation cases stay in `withdraw.rs`, where they were written; this is the count
    /// half, and it is only a control while the counts are non-trivial.
    #[test]
    fn every_pinned_write_count_survives_the_move() {
        for (field, expected) in [
            ("pool.w_real", 1),
            ("pool.n_real", 1),
            ("pool.owed_fees", 1),
            ("pool.blanks", 1),
            ("wallet_stats.active_positions", 1),
            ("wallet_stats.active_value", 1),
            ("position.tier_checkpoint", 1),
            ("position.equal_checkpoint", 1),
            ("position.accrued", 1),
            ("position.state", 0),
            ("position.in_tier", 0),
            ("position.recorded_value", 0),
            ("position.slot_index", 0),
        ] {
            assert_eq!(writes(EXIT_ACCOUNTING_SRC, field), expected, "{field}");
        }
    }

    /// **`pub(crate)`, and the visibility is a measured 504 bytes rather than a preference.**
    /// Nothing outside this crate calls the accounting half — `lib.rs` exposes instructions, not
    /// this — and declaring it plain `pub` gives the symbol external linkage, so the deployment
    /// build carries a standalone copy of a function it also inlines: `cargo build-sbf
    /// --features dev --arch v3` went **768,184 → 768,688 B (+504)** under `pub` and **768,184 →
    /// 768,184 (0)** under `pub(crate)`.
    #[test]
    fn the_accounting_half_is_crate_internal() {
        let prod = production(EXIT_ACCOUNTING_SRC);
        assert!(prod.contains("pub(crate) fn apply_withdraw("));
        assert!(prod.contains("pub(crate) struct WithdrawEffect {"));
        assert_eq!(
            prod.matches("pub fn ").count(),
            0,
            "an externally visible fn here is a symbol the deployment build cannot discard"
        );
    }

    /// **The property that makes this module shareable at all: it moves no card.** Every one of
    /// the three custody legs is the caller's, which is why one accounting half can serve
    /// standards 0, 1 and 2. A release folded in here would be a custody path keyed on nothing,
    /// reached by both twins, with each twin's account set half-absent.
    #[test]
    fn no_custody_leg_reaches_the_accounting_half() {
        for callee in [
            "release_from_vault",
            "transfer_core",
            "transfer_pnft",
            "transfer_spl",
            "close_account",
            "invoke_signed",
        ] {
            assert_eq!(
                calls(EXIT_ACCOUNTING_SRC, callee),
                0,
                "{callee} reaches the accounting half"
            );
        }
        assert_eq!(
            production(EXIT_ACCOUNTING_SRC)
                .matches("CpiContext")
                .count(),
            0
        );
    }
}
