use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::{
    BelowFloorClosed, BelowFloorRetained, RebalanceEvaluated, TierChanged, ValueRecorded,
};
use crate::common::guards::assert_weight_open;
use crate::common::math::{position_fee_payout, weight_of};
use crate::common::rebalance;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED, WALLET_STATS_SEED,
};
use crate::common::tier;
use crate::state::{
    Pool, Position, PositionState, ProtocolConfig, TierEntry, TopTier, WalletStats, WeightIndex,
};

/// The three dispositions a re-attested value can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordOutcome {
    /// `value >= admission_floor` — inclusive at the boundary.
    Refresh,
    /// Below floor, but `position.lock_until > now`: the value is still recorded, the position
    /// stays fully `Active`, and an extra event is emitted. Every state effect otherwise matches
    /// `Refresh`.
    Retained,
    /// Below floor and unlocked: the position deactivates in this instruction.
    Close,
}

fn classify(value: u64, admission_floor: u64, lock_until: i64, now: i64) -> RecordOutcome {
    if value >= admission_floor {
        RecordOutcome::Refresh
    } else if lock_until > now {
        RecordOutcome::Retained
    } else {
        RecordOutcome::Close
    }
}

/// Moves `w_real` from `old_value`'s leaf weight to `new_value`'s, or to zero when `new_value`
/// is `None` — the close path, where the leaf leaves the tree entirely. Checked in whichever
/// direction the weight moves; an under-run is `ArithmeticFailure`, never wrapped.
fn move_w_real(w_real: u128, old_value: u64, new_value: Option<u64>) -> Result<u128> {
    let old_weight = u128::from(weight_of(old_value)?);
    let new_weight = match new_value {
        Some(value) => u128::from(weight_of(value)?),
        None => 0,
    };
    let moved = match new_weight.checked_sub(old_weight) {
        Some(delta) => w_real.checked_add(delta),
        None => w_real.checked_sub(old_weight - new_weight),
    };
    Ok(moved.ok_or(ByeMachineError::ArithmeticFailure)?)
}

/// Moves `active_value` by the same value delta `old_value` → `new_value` describes, or to a
/// full removal of `old_value` when `new_value` is `None`. Checked in both directions.
fn move_active_value(active_value: u64, old_value: u64, new_value: Option<u64>) -> Result<u64> {
    let new_value = new_value.unwrap_or(0);
    let moved = match new_value.checked_sub(old_value) {
        Some(delta) => active_value.checked_add(delta),
        None => active_value.checked_sub(old_value - new_value),
    };
    Ok(moved.ok_or(ByeMachineError::ArithmeticFailure)?)
}

/// What the handler needs back to drive the `WeightIndex` call and the emits: the disposition,
/// the pre-write value (`ValueRecorded.old`, and the value `remove`/`update` verifies against),
/// the position's unmoved slot, the post-mutation aggregate `WeightIndex` must match, and the
/// blank counts from before the rebalance.
#[cfg_attr(test, derive(Debug))]
struct RecordEffect {
    outcome: RecordOutcome,
    old_value: u64,
    slot_index: u32,
    expected_total: u64,
    blanks_before: [u32; 3],
}

/// The guards, the every-path obligations, the close-path deactivation and the fee settle-up —
/// everything that touches `Pool`/`Position`/`WalletStats` directly. Tier membership and the
/// `WeightIndex` leaf move are the handler's, since neither takes a plain-struct argument here.
fn apply_record_value(
    pool: &mut Pool,
    position: &mut Position,
    wallet_stats: &mut WalletStats,
    value: u64,
    observed_at: i64,
    now: i64,
) -> Result<RecordEffect> {
    require!(pool.sweep_pending, ByeMachineError::SweepNotOpen);
    assert_weight_open(pool)?;
    require!(
        position.state == PositionState::Active,
        ByeMachineError::InvalidPositionState
    );
    require!(value > 0, ByeMachineError::ZeroValue);

    pool.sweep_updates = pool
        .sweep_updates
        .checked_add(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    let old_value = position.recorded_value;
    let slot_index = position.slot_index;
    let outcome = classify(value, pool.admission_floor, position.lock_until, now);
    let refreshed_to = if outcome == RecordOutcome::Close {
        None
    } else {
        Some(value)
    };

    pool.w_real = move_w_real(pool.w_real, old_value, refreshed_to)?;
    wallet_stats.active_value =
        move_active_value(wallet_stats.active_value, old_value, refreshed_to)?;

    if outcome == RecordOutcome::Close {
        pool.n_real = pool
            .n_real
            .checked_sub(1)
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        wallet_stats.active_positions = wallet_stats
            .active_positions
            .checked_sub(1)
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        position.state = PositionState::ClosedBelowFloor;
        position.accrued =
            position_fee_payout(pool.acc_equal, position.equal_checkpoint, position.accrued)?;
        position.equal_checkpoint = pool.acc_equal;
    }

    position.recorded_value = value;
    position.value_observed_at = observed_at;

    let blanks_before = pool.blanks;
    let targets = rebalance::evaluate(
        pool.n_real,
        pool.w_real,
        pool.blank_faces,
        pool.ticket_target,
    )?;
    pool.blanks = [targets.b1, targets.b2, targets.b5];

    let expected_total = WeightIndex::narrow_expected_total(pool.w_real)?;

    Ok(RecordEffect {
        outcome,
        old_value,
        slot_index,
        expected_total,
        blanks_before,
    })
}

pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, RecordValue<'info>>,
    value: u64,
    observed_at: i64,
) -> Result<()> {
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let authority = ctx.accounts.operator.key();
    let depositor = ctx.accounts.position.depositor;

    let effect = apply_record_value(
        &mut ctx.accounts.pool,
        &mut ctx.accounts.position,
        &mut ctx.accounts.wallet_stats,
        value,
        observed_at,
        now,
    )?;

    match effect.outcome {
        RecordOutcome::Close => {
            ctx.accounts.weight_index.load_mut()?.remove(
                effect.slot_index,
                effect.old_value,
                effect.expected_total,
            )?;
        }
        RecordOutcome::Refresh | RecordOutcome::Retained => {
            ctx.accounts.weight_index.load_mut()?.update(
                effect.slot_index,
                effect.old_value,
                value,
                effect.expected_total,
            )?;
        }
    }

    let acc_tier = ctx.accounts.top_tier.acc_tier;
    let tier_size = ctx.accounts.pool.tier_size;
    let mut tier_change: Option<(Option<Pubkey>, Option<Pubkey>)> = None;

    if effect.outcome == RecordOutcome::Close {
        if ctx.accounts.position.in_tier {
            tier::vacate(
                &mut ctx.accounts.top_tier,
                position_key,
                &mut ctx.accounts.position,
                acc_tier,
            )?;

            let entered = tier::fill_vacancy_from_candidate(
                &mut ctx.accounts.top_tier,
                tier_size,
                ctx.remaining_accounts,
                pool_key,
                acc_tier,
            )?;
            tier_change = Some((Some(position_key), entered));
        }
    } else if ctx.accounts.position.in_tier {
        let refreshed = TierEntry {
            position: position_key,
            value,
            activated_at: ctx.accounts.position.activated_at,
            position_id: ctx.accounts.position.position_id,
        };
        tier::refresh_member(
            &mut ctx.accounts.top_tier,
            tier_size,
            position_key,
            refreshed,
        )?;
    } else {
        let candidate = TierEntry {
            position: position_key,
            value,
            activated_at: ctx.accounts.position.activated_at,
            position_id: ctx.accounts.position.position_id,
        };
        match tier::cross_into_tier(
            &mut ctx.accounts.top_tier,
            tier_size,
            &mut ctx.accounts.position,
            candidate,
            acc_tier,
        ) {
            tier::CrossOutcome::Inserted => {
                tier_change = Some((None, Some(position_key)));
            }
            tier::CrossOutcome::Swapped(evicted) => {
                tier::settle_displaced_member(ctx.remaining_accounts, &evicted, acc_tier)?;
                tier_change = Some((Some(evicted.position), Some(position_key)));
            }
            tier::CrossOutcome::Rejected => {}
        }
    }

    emit!(ValueRecorded {
        pool: pool_key,
        slot: clock.slot,
        authority,
        position: position_key,
        old: effect.old_value,
        new: value,
        observed_at,
    });

    emit!(RebalanceEvaluated {
        pool: pool_key,
        slot: clock.slot,
        n_real: ctx.accounts.pool.n_real,
        w_real: ctx.accounts.pool.w_real,
        blanks_before: effect.blanks_before,
        blanks_after: ctx.accounts.pool.blanks,
    });

    match effect.outcome {
        RecordOutcome::Close => emit!(BelowFloorClosed {
            pool: pool_key,
            slot: clock.slot,
            authority,
            position: position_key,
            depositor,
        }),
        RecordOutcome::Retained => emit!(BelowFloorRetained {
            pool: pool_key,
            slot: clock.slot,
            authority,
            position: position_key,
            lock_until: ctx.accounts.position.lock_until,
        }),
        RecordOutcome::Refresh => {}
    }

    if let Some((left, entered)) = tier_change {
        emit!(TierChanged {
            pool: pool_key,
            slot: clock.slot,
            left,
            entered,
        });
    }

    Ok(())
}

#[derive(Accounts)]
pub struct RecordValue<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(mut, address = pool.weight_index)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    #[account(
        mut,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(
        mut,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()],
        bump = wallet_stats.bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        ACC_PRECISION, DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };

    const RECORD_SRC: &str = include_str!("record_value.rs");

    const OPERATOR_CONSTRAINT: &str =
        "constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized";

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    use crate::common::test_support::{calls, production, writes};

    fn blank_position() -> Position {
        Position {
            pool: Pubkey::default(),
            depositor: Pubkey::default(),
            nft_mint: Pubkey::default(),
            position_id: 0,
            state: PositionState::Active,
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
            standard: 0,
            bump: 0,
            vault_bump: 0,
        }
    }

    fn blank_wallet_stats() -> WalletStats {
        WalletStats {
            pool: Pubkey::default(),
            owner: Pubkey::default(),
            active_positions: 0,
            active_value: 0,
            bump: 0,
        }
    }

    /// A pool with a sweep open and a realistic rebalance surface, so `apply_record_value`'s
    /// unconditional `rebalance::evaluate` call has a config it can actually solve.
    fn sweep_open_pool() -> Pool {
        let mut pool = Pool::blank();
        pool.admission_floor = DEFAULT_ADMISSION_FLOOR;
        pool.blank_faces = DEFAULT_BLANK_FACES;
        pool.ticket_target = DEFAULT_TICKET_TARGET;
        pool.sweep_pending = true;
        pool
    }

    // --- the classifier, pure --------------------------------------------------------------

    #[test]
    fn classify_at_and_above_the_floor_is_refresh() {
        let floor = DEFAULT_ADMISSION_FLOOR;
        assert_eq!(classify(floor + 1, floor, 0, 0), RecordOutcome::Refresh);
        assert_eq!(
            classify(floor, floor, 0, 0),
            RecordOutcome::Refresh,
            "inclusive at the boundary"
        );
    }

    #[test]
    fn classify_below_floor_locked_is_retained_not_an_error() {
        let floor = DEFAULT_ADMISSION_FLOOR;
        assert_eq!(classify(floor - 1, floor, 100, 50), RecordOutcome::Retained);
    }

    #[test]
    fn classify_lock_boundary_closes_not_retains() {
        let floor = DEFAULT_ADMISSION_FLOOR;
        assert_eq!(classify(floor - 1, floor, 50, 50), RecordOutcome::Close);
    }

    #[test]
    fn classify_below_floor_unlocked_closes() {
        let floor = DEFAULT_ADMISSION_FLOOR;
        assert_eq!(classify(floor - 1, floor, 0, 0), RecordOutcome::Close);
    }

    // --- the delta arithmetic, pure --------------------------------------------------------

    #[test]
    fn move_active_value_moves_up_down_and_stays_put() {
        assert_eq!(move_active_value(100, 40, Some(60)).unwrap(), 120);
        assert_eq!(move_active_value(100, 60, Some(40)).unwrap(), 80);
        assert_eq!(move_active_value(100, 50, Some(50)).unwrap(), 100);
        assert_eq!(
            move_active_value(100, 50, None).unwrap(),
            50,
            "close removes old_value entirely"
        );
    }

    #[test]
    fn move_w_real_moves_up_down_and_stays_put() {
        let floor = DEFAULT_ADMISSION_FLOOR;
        let w_floor = u128::from(weight_of(floor).unwrap());
        let w_double = u128::from(weight_of(floor * 2).unwrap());
        assert!(w_double < w_floor, "a higher value must weigh less");

        assert_eq!(
            move_w_real(w_floor, floor, Some(floor * 2)).unwrap(),
            w_double
        );
        assert_eq!(
            move_w_real(w_double, floor * 2, Some(floor)).unwrap(),
            w_floor
        );
        assert_eq!(move_w_real(w_floor, floor, Some(floor)).unwrap(), w_floor);
        assert_eq!(
            move_w_real(w_floor, floor, None).unwrap(),
            0,
            "close removes the leaf entirely"
        );
    }

    #[test]
    fn move_active_value_under_run_fails_closed_at_6703() {
        let err = move_active_value(10, 50, Some(20)).unwrap_err();
        assert_eq!(error_code(err), 6703);
    }

    #[test]
    fn move_w_real_under_run_fails_closed_at_6703() {
        let err = move_w_real(0, DEFAULT_ADMISSION_FLOOR, None).unwrap_err();
        assert_eq!(error_code(err), 6703);
    }

    // --- the guards, in order ---------------------------------------------------------------

    #[test]
    fn apply_record_value_rejects_a_zero_value_at_6401() {
        // `recorded_value` is deliberately non-zero: a `blank_position()` default of 0 would let
        // `move_w_real`'s own `weight_of(old_value)` call reject with the same 6401 by
        // coincidence, on the close path a zero value takes, masking a deleted guard entirely.
        let mut pool = sweep_open_pool();
        let mut position = blank_position();
        position.recorded_value = DEFAULT_ADMISSION_FLOOR;
        let mut wallet_stats = blank_wallet_stats();
        let err =
            apply_record_value(&mut pool, &mut position, &mut wallet_stats, 0, 0, 0).unwrap_err();
        assert_eq!(error_code(err), 6401);
    }

    #[test]
    fn apply_record_value_rejects_when_sweep_is_not_open_at_6402() {
        let mut pool = sweep_open_pool();
        pool.sweep_pending = false;
        let mut position = blank_position();
        let mut wallet_stats = blank_wallet_stats();
        let err = apply_record_value(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            DEFAULT_ADMISSION_FLOOR,
            0,
            0,
        )
        .unwrap_err();
        assert_eq!(error_code(err), 6402);
    }

    #[test]
    fn apply_record_value_rejects_a_batch_in_flight_at_6200() {
        let mut pool = sweep_open_pool();
        pool.open_batches = 1;
        let mut position = blank_position();
        let mut wallet_stats = blank_wallet_stats();
        let err = apply_record_value(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            DEFAULT_ADMISSION_FLOOR,
            0,
            0,
        )
        .unwrap_err();
        assert_eq!(error_code(err), 6200);
    }

    #[test]
    fn apply_record_value_rejects_a_non_active_position_at_6301() {
        let mut pool = sweep_open_pool();
        let mut position = blank_position();
        position.state = PositionState::Pending;
        let mut wallet_stats = blank_wallet_stats();
        let err = apply_record_value(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            DEFAULT_ADMISSION_FLOOR,
            0,
            0,
        )
        .unwrap_err();
        assert_eq!(error_code(err), 6301);
    }

    // --- Behavioural: the three dispositions, over hand-built accounts ----------------------

    #[test]
    fn a_refresh_above_the_floor_bumps_sweep_updates_and_leaves_the_position_active() {
        let mut pool = sweep_open_pool();
        pool.n_real = 1;
        pool.w_real = u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        let mut position = blank_position();
        position.recorded_value = DEFAULT_ADMISSION_FLOOR;
        position.slot_index = 3;
        let mut wallet_stats = blank_wallet_stats();
        wallet_stats.active_value = DEFAULT_ADMISSION_FLOOR;

        let new_value = DEFAULT_ADMISSION_FLOOR * 2;
        let effect = apply_record_value(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            new_value,
            555,
            0,
        )
        .unwrap();

        assert_eq!(effect.outcome, RecordOutcome::Refresh);
        assert_eq!(effect.old_value, DEFAULT_ADMISSION_FLOOR);
        assert_eq!(effect.slot_index, 3, "slot_index is never reallocated");
        assert_eq!(pool.sweep_updates, 1);
        assert!(position.state == PositionState::Active);
        assert_eq!(position.recorded_value, new_value);
        assert_eq!(position.value_observed_at, 555);
        assert_eq!(wallet_stats.active_value, new_value);
        assert_eq!(pool.n_real, 1, "refresh never touches n_real");
        assert_eq!(pool.w_real, u128::from(weight_of(new_value).unwrap()));
    }

    #[test]
    fn a_below_floor_locked_refresh_is_retained_and_stays_fully_active() {
        let mut pool = sweep_open_pool();
        pool.n_real = 1;
        pool.w_real = u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        let mut position = blank_position();
        position.recorded_value = DEFAULT_ADMISSION_FLOOR;
        position.lock_until = 1_000;
        let mut wallet_stats = blank_wallet_stats();
        wallet_stats.active_value = DEFAULT_ADMISSION_FLOOR;

        let effect = apply_record_value(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            DEFAULT_ADMISSION_FLOOR - 1,
            0,
            0,
        )
        .unwrap();

        assert_eq!(effect.outcome, RecordOutcome::Retained);
        assert!(
            position.state == PositionState::Active,
            "retained stays fully active"
        );
        assert_eq!(position.recorded_value, DEFAULT_ADMISSION_FLOOR - 1);
        assert_eq!(pool.n_real, 1);
        assert_eq!(wallet_stats.active_value, DEFAULT_ADMISSION_FLOOR - 1);
    }

    #[test]
    fn an_unlocked_below_floor_refresh_closes_the_position() {
        let mut pool = sweep_open_pool();
        pool.n_real = 1;
        pool.w_real = u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        pool.acc_equal = 3 * ACC_PRECISION;
        let mut position = blank_position();
        position.recorded_value = DEFAULT_ADMISSION_FLOOR;
        position.equal_checkpoint = ACC_PRECISION;
        position.accrued = 7;
        let mut wallet_stats = blank_wallet_stats();
        wallet_stats.active_positions = 1;
        wallet_stats.active_value = DEFAULT_ADMISSION_FLOOR;

        let effect = apply_record_value(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            DEFAULT_ADMISSION_FLOOR - 1,
            0,
            0,
        )
        .unwrap();

        assert_eq!(effect.outcome, RecordOutcome::Close);
        assert!(position.state == PositionState::ClosedBelowFloor);
        assert_eq!(pool.n_real, 0);
        assert_eq!(pool.w_real, 0);
        assert_eq!(wallet_stats.active_positions, 0);
        assert_eq!(wallet_stats.active_value, 0);
        assert_eq!(
            position.accrued, 9,
            "(3-1) ACC_PRECISION / ACC_PRECISION + 7"
        );
        assert_eq!(position.equal_checkpoint, pool.acc_equal);
    }

    /// `old_value == value` is the common case in a real sweep — most cards'
    /// assessed value does not move — and an unguarded no-op path through the interesting-looking
    /// logic is invisible until something drives it. Nothing before this test drove
    /// `apply_record_value` with old == new.
    #[test]
    fn a_no_op_refresh_still_bumps_sweep_updates_and_runs_every_obligation() {
        let mut pool = sweep_open_pool();
        pool.n_real = 1;
        pool.w_real = u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        let mut position = blank_position();
        position.recorded_value = DEFAULT_ADMISSION_FLOOR;
        position.slot_index = 5;
        let mut wallet_stats = blank_wallet_stats();
        wallet_stats.active_value = DEFAULT_ADMISSION_FLOOR;

        let effect = apply_record_value(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            DEFAULT_ADMISSION_FLOOR,
            777,
            0,
        )
        .unwrap();

        assert_eq!(effect.outcome, RecordOutcome::Refresh);
        assert_eq!(effect.old_value, DEFAULT_ADMISSION_FLOOR);
        assert_eq!(effect.slot_index, 5);
        assert_eq!(
            effect.expected_total,
            u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap()) as u64
        );
        assert_eq!(
            pool.sweep_updates, 1,
            "the bump does not skip an unchanged value"
        );
        assert_eq!(
            pool.w_real,
            u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap())
        );
        assert_eq!(wallet_stats.active_value, DEFAULT_ADMISSION_FLOOR);
        assert_eq!(position.recorded_value, DEFAULT_ADMISSION_FLOOR);
        assert_eq!(position.value_observed_at, 777);
        assert_eq!(pool.n_real, 1);
    }

    #[test]
    fn close_path_decrements_n_real_only_w_real_is_written_before_the_close_branch() {
        assert_eq!(writes(RECORD_SRC, "pool.n_real"), 1);
        assert_eq!(writes(RECORD_SRC, "pool.w_real"), 1);
        let prod = production(RECORD_SRC);
        let w_real_write = prod.find("pool.w_real = move_w_real").unwrap();
        let n_real_write = prod.find("pool.n_real = pool").unwrap();
        assert!(
            w_real_write < n_real_write,
            "w_real must be written before the close-only branch, on every path"
        );
    }

    // --- single exit ------------------------------------------------------------------------

    fn handler_body(source: &str) -> &str {
        let prod = production(source);
        let start = prod.find("pub fn handler").unwrap();
        let rest = &prod[start..];
        let end = rest.find("\n#[derive(Accounts)]").unwrap();
        &rest[..end]
    }

    #[test]
    fn handler_has_exactly_one_ok_and_zero_return_statements() {
        let body = handler_body(RECORD_SRC);
        assert_eq!(body.matches("Ok(())").count(), 1);
        assert_eq!(body.matches("return ").count(), 0);
    }

    // --- every-path obligations, ordering --------------------------------------------------

    #[test]
    fn every_path_obligations_appear_exactly_once() {
        let prod = production(RECORD_SRC);
        assert_eq!(writes(RECORD_SRC, "pool.sweep_updates"), 1);
        assert_eq!(prod.matches("emit!(ValueRecorded").count(), 1);
        assert_eq!(prod.matches("rebalance::evaluate(").count(), 1);
        assert_eq!(writes(RECORD_SRC, "pool.blanks"), 1);
        assert_eq!(writes(RECORD_SRC, "position.recorded_value"), 1);
        assert_eq!(writes(RECORD_SRC, "position.value_observed_at"), 1);
    }

    #[test]
    fn sweep_updates_bump_precedes_the_outcome_classification() {
        let prod = production(RECORD_SRC);
        let bump_pos = prod.find("pool.sweep_updates = pool").unwrap();
        let classify_pos = prod.find("classify(value, pool.admission_floor").unwrap();
        assert!(
            bump_pos < classify_pos,
            "the bump must run before the branch decision, so Close and a no-op Refresh both count"
        );
    }

    #[test]
    fn old_value_is_captured_before_recorded_value_is_overwritten() {
        let prod = production(RECORD_SRC);
        let capture = prod
            .find("let old_value = position.recorded_value;")
            .unwrap();
        let write = prod.find("position.recorded_value = value;").unwrap();
        assert!(capture < write);
    }

    // --- the assignment sweep, mechanically ------------------------------------------------

    #[test]
    fn close_path_writes_the_literal_closed_below_floor_variant() {
        assert!(
            production(RECORD_SRC).contains("position.state = PositionState::ClosedBelowFloor;")
        );
        assert_eq!(writes(RECORD_SRC, "position.state"), 1);
    }

    #[test]
    fn apply_record_value_writes_every_field_the_design_requires() {
        for field in [
            "pool.sweep_updates",
            "pool.w_real",
            "wallet_stats.active_value",
            "position.recorded_value",
            "position.value_observed_at",
            "pool.blanks",
            "position.equal_checkpoint",
        ] {
            assert_eq!(writes(RECORD_SRC, field), 1, "{field} is not assigned once");
        }
        for field in ["pool.n_real", "wallet_stats.active_positions"] {
            assert_eq!(
                writes(RECORD_SRC, field),
                1,
                "{field} is not assigned once on the close path"
            );
        }
        assert_eq!(
            writes(RECORD_SRC, "position.slot_index"),
            0,
            "update is in-place"
        );
    }

    /// `vacate` and `cross_into_tier` own every tier-side `Position` write —
    /// the close-in_tier settle-up and the crossing's Inserted/Swapped `in_tier`/
    /// `tier_checkpoint` stamps all moved into `common/tier.rs`. This file's handler writes
    /// **zero** `Position` tier fields directly, which is a stronger backstop than an expected
    /// count that has to be updated every time the block's shape changes: any tier write that
    /// creeps back into the handler fails this test regardless of what it is or where it lands.
    #[test]
    fn the_handler_writes_no_position_tier_fields_directly() {
        assert_eq!(
            writes(RECORD_SRC, "position.tier_checkpoint"),
            0,
            "moved to common/tier.rs's vacate and cross_into_tier"
        );
        assert_eq!(
            writes(RECORD_SRC, "position.in_tier"),
            0,
            "moved to common/tier.rs's vacate and cross_into_tier"
        );
        assert_eq!(
            writes(RECORD_SRC, "candidate.in_tier"),
            0,
            "moved to common/tier.rs's fill_vacancy_from_candidate"
        );
        assert_eq!(
            writes(RECORD_SRC, "candidate.tier_checkpoint"),
            0,
            "moved to common/tier.rs's fill_vacancy_from_candidate"
        );
        assert_eq!(
            writes(RECORD_SRC, "position.accrued"),
            1,
            "apply_record_value's equal-share settle only — the tier settle-ups moved to \
             common/tier.rs's vacate and settle_displaced_member"
        );
        assert_eq!(
            calls(RECORD_SRC, "position_fee_payout"),
            1,
            "the equal-share settle only"
        );
    }

    // --- no pause flag ----------------------------------------------------------------------

    #[test]
    fn reads_no_pause_flag() {
        assert_eq!(production(RECORD_SRC).matches("deposits_paused").count(), 0);
        assert_eq!(production(RECORD_SRC).matches("rolls_paused").count(), 0);
    }

    // --- authority, guard call count, unreachable code -------------------------------------

    #[test]
    fn operator_constraint_present_once_and_no_other_signer_class() {
        assert_eq!(
            production(RECORD_SRC).matches(OPERATOR_CONSTRAINT).count(),
            1
        );
        assert_eq!(production(RECORD_SRC).matches("administrator").count(), 0);
    }

    #[test]
    fn sweep_and_weight_open_guards_present_with_the_right_codes() {
        assert!(production(RECORD_SRC)
            .contains("require!(pool.sweep_pending, ByeMachineError::SweepNotOpen);"));
        assert_eq!(calls(RECORD_SRC, "assert_weight_open"), 1);
    }

    #[test]
    fn state_active_guard_present_at_invalid_position_state() {
        let prod = production(RECORD_SRC);
        assert!(prod.contains("position.state == PositionState::Active,"));
        assert!(prod.contains("ByeMachineError::InvalidPositionState"));
    }

    /// Reusing `validate_admission_limits` would contradict the whole reason this
    /// handler has a below-floor branch. If this ever changes, `approve_deposit.rs`'s own pin on
    /// the crate-wide call count must be updated in the same commit — see its comment.
    #[test]
    fn validate_admission_limits_is_not_reused() {
        assert_eq!(calls(RECORD_SRC, "validate_admission_limits"), 0);
    }

    #[test]
    fn no_ceiling_or_wallet_cap_recheck_on_refresh() {
        let prod = production(RECORD_SRC);
        assert_eq!(prod.matches("admission_ceiling").count(), 0);
        assert_eq!(prod.matches("wallet_value_cap").count(), 0);
    }

    /// The locked branch succeeds and emits; it does not `Err`, which is why
    /// `LockedDeactivationRejected` has no emitter here or anywhere else in slice 1.
    #[test]
    fn locked_deactivation_rejected_is_never_used_here() {
        assert_eq!(
            production(RECORD_SRC)
                .matches("LockedDeactivationRejected")
                .count(),
            0
        );
    }

    // --- the accounts surface ---------------------------------------------------------------

    #[test]
    fn every_account_is_boxed_and_every_pda_declares_its_seeds() {
        let prod = production(RECORD_SRC);
        assert_eq!(prod.matches("Box<Account<").count(), 5);
        assert_eq!(prod.matches("seeds = [").count(), 5);
        assert!(prod.contains("#[account(mut, address = pool.weight_index)]"));
        assert!(prod.contains("pub weight_index: AccountLoader<'info, WeightIndex>"));
        assert!(prod.contains(
            "seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()]"
        ));
        assert!(prod.contains("seeds = [TOP_TIER_SEED, pool.key().as_ref()]"));
        assert!(prod
            .contains("seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()]"));
    }

    // --- tier membership. `settle_displaced_member` and the reorder / close-path pure logic
    // live in `common/tier.rs`'s `refresh_member`/`vacate`/`fill_vacancy`/
    // `fill_vacancy_from_candidate` — see their own behavioural tests there. What remains here
    // is wiring: that the handler calls the right function on the right branch. ---------------

    #[test]
    fn tier_changed_is_emitted_from_a_single_call_site_carrying_both_sides() {
        let prod = production(RECORD_SRC);
        assert_eq!(prod.matches("emit!(TierChanged").count(), 1);
        assert_eq!(
            prod.matches("tier_change = Some(").count(),
            3,
            "the crossing Inserted branch, the crossing Swapped branch, and the close settle"
        );
    }

    /// A wrong-pool candidate and an ineligible one are different failures with different
    /// operator remedies, so they are given separate codes rather than conflated under one — the
    /// same principle `SweepAlreadyOpen` and `WeightDesync` follow elsewhere. Behaviourally
    /// exercised over `fill_vacancy_from_candidate` in `common/tier.rs`; kept here as the
    /// numeric pin the client checks against.
    #[test]
    fn position_pool_mismatch_is_pinned_at_6304() {
        assert_eq!(u32::from(ByeMachineError::PositionPoolMismatch), 6304);
    }

    /// Every one of the four tier transitions delegates to `common/tier.rs` —
    /// the reorder branch to `tier::refresh_member`, the close branch to `tier::vacate` +
    /// `tier::fill_vacancy_from_candidate`, and the crossing branch to `tier::cross_into_tier`.
    /// This file no longer calls `tier::demote` or `tier::promote` directly at all.
    #[test]
    fn every_tier_transition_delegates_to_the_checked_tier_operations() {
        let prod = production(RECORD_SRC);
        assert!(prod.contains("} else if ctx.accounts.position.in_tier {"));
        assert_eq!(calls(RECORD_SRC, "tier::demote"), 0);
        assert_eq!(calls(RECORD_SRC, "tier::promote"), 0);
        assert_eq!(calls(RECORD_SRC, "tier::refresh_member"), 1);
        assert_eq!(calls(RECORD_SRC, "tier::vacate"), 1);
        assert_eq!(calls(RECORD_SRC, "tier::fill_vacancy_from_candidate"), 1);
        assert_eq!(calls(RECORD_SRC, "tier::cross_into_tier"), 1);
    }

    /// The `Swapped` outcome's only production caller of `settle_displaced_member` — without
    /// it, a displaced member's tier delta is never settled and it never leaves `in_tier`, so it
    /// silently becomes unpromotable forever, while the transaction itself still succeeds.
    #[test]
    fn the_swapped_outcome_settles_the_displaced_member() {
        assert_eq!(calls(RECORD_SRC, "settle_displaced_member"), 1);
        assert!(production(RECORD_SRC).contains(
            "tier::CrossOutcome::Swapped(evicted) => {\n                tier::settle_displaced_member(ctx.remaining_accounts, &evicted, acc_tier)?;"
        ));
    }

    /// The only wiring fact `apply_record_value`'s own tests can't reach: the handler must pass
    /// the pre-write value to `WeightIndex`, not the newly-attested one — `remove`/`update` both
    /// verify their `old_value`/`old_weight` argument against the leaf's actual stored weight,
    /// so passing `value` in its place either fails a desync it shouldn't, or (if the caller's
    /// old and new value happen to weigh the same) silently verifies against the wrong leaf.
    /// Neither is visible to a test that drives `apply_record_value` and `WeightIndex` separately.
    #[test]
    fn the_handler_passes_old_value_not_the_refreshed_value_to_weight_index_calls() {
        let prod = production(RECORD_SRC);
        assert!(prod.contains(
            "effect.slot_index,\n                effect.old_value,\n                effect.expected_total,"
        ));
        assert!(prod.contains(
            "effect.slot_index,\n                effect.old_value,\n                value,\n                effect.expected_total,"
        ));
    }

    // --- the WeightIndex leaf move, driven directly ----------------------------------------

    fn boxed_zeroed_weight_index() -> Box<WeightIndex> {
        unsafe {
            let layout = std::alloc::Layout::new::<WeightIndex>();
            let ptr = std::alloc::alloc_zeroed(layout).cast::<WeightIndex>();
            assert!(!ptr.is_null(), "allocation failed");
            Box::from_raw(ptr)
        }
    }

    /// Independent of `fenwick_prefix` on purpose, so a fault in the production reader cannot
    /// hide behind the oracle meant to catch it.
    fn sum_of_leaves(tree: &[u64]) -> u64 {
        fn prefix(tree: &[u64], mut i: usize) -> u64 {
            let mut sum = 0u64;
            while i > 0 {
                sum += tree[i - 1];
                i &= i - 1;
            }
            sum
        }
        (1..=tree.len())
            .map(|i| prefix(tree, i) - prefix(tree, i - 1))
            .sum()
    }

    #[test]
    fn a_refresh_moves_the_leaf_and_total_weight_together_against_sigma_leaves() {
        let mut index = boxed_zeroed_weight_index();
        let slot = index
            .insert(
                DEFAULT_ADMISSION_FLOOR,
                weight_of(DEFAULT_ADMISSION_FLOOR).unwrap(),
            )
            .unwrap();

        let new_value = DEFAULT_ADMISSION_FLOOR * 3;
        let new_weight = weight_of(new_value).unwrap();
        index
            .update(slot, DEFAULT_ADMISSION_FLOOR, new_value, new_weight)
            .unwrap();

        assert_eq!(index.total_weight, new_weight);
        assert_eq!(sum_of_leaves(&index.tree), new_weight);
    }

    #[test]
    fn insert_update_remove_insert_recycles_the_slot_with_oracle_agreement() {
        let mut index = boxed_zeroed_weight_index();
        let value_a = DEFAULT_ADMISSION_FLOOR;
        let weight_a = weight_of(value_a).unwrap();
        let slot = index.insert(value_a, weight_a).unwrap();

        let value_b = DEFAULT_ADMISSION_FLOOR * 2;
        let weight_b = weight_of(value_b).unwrap();
        index.update(slot, value_a, value_b, weight_b).unwrap();
        assert_eq!(index.total_weight, weight_b);
        assert_eq!(sum_of_leaves(&index.tree), weight_b);

        index.remove(slot, value_b, 0).unwrap();
        assert_eq!(index.total_weight, 0);
        assert_eq!(sum_of_leaves(&index.tree), 0);

        let value_c = DEFAULT_ADMISSION_FLOOR * 3;
        let weight_c = weight_of(value_c).unwrap();
        let recycled = index.insert(value_c, weight_c).unwrap();
        assert_eq!(recycled, slot, "the released slot is reused");
        assert_eq!(index.total_weight, weight_c);
        assert_eq!(sum_of_leaves(&index.tree), weight_c);
    }

    /// The exact prescribed order, distinct from the test above: the refresh sits *after* the
    /// recycled slot is re-populated, not before the remove, and there are three of them in a
    /// row rather than one. `total_weight == Σ leaves` is asserted after every single op.
    #[test]
    fn insert_remove_recycle_then_three_refreshes_agree_with_sigma_leaves() {
        let mut index = boxed_zeroed_weight_index();

        let value_a = DEFAULT_ADMISSION_FLOOR;
        let weight_a = weight_of(value_a).unwrap();
        let slot = index.insert(value_a, weight_a).unwrap();
        assert_eq!(index.total_weight, weight_a);
        assert_eq!(sum_of_leaves(&index.tree), weight_a);

        index.remove(slot, value_a, 0).unwrap();
        assert_eq!(index.total_weight, 0);
        assert_eq!(sum_of_leaves(&index.tree), 0);

        let value_b = DEFAULT_ADMISSION_FLOOR * 2;
        let weight_b = weight_of(value_b).unwrap();
        let recycled = index.insert(value_b, weight_b).unwrap();
        assert_eq!(recycled, slot, "the released slot is reused");
        assert_eq!(index.total_weight, weight_b);
        assert_eq!(sum_of_leaves(&index.tree), weight_b);

        let mut current_value = value_b;
        for next_value in [
            DEFAULT_ADMISSION_FLOOR * 3,
            DEFAULT_ADMISSION_FLOOR * 4,
            DEFAULT_ADMISSION_FLOOR * 5,
        ] {
            let next_weight = weight_of(next_value).unwrap();
            index
                .update(recycled, current_value, next_value, next_weight)
                .unwrap();
            assert_eq!(index.total_weight, next_weight);
            assert_eq!(sum_of_leaves(&index.tree), next_weight);
            current_value = next_value;
        }
    }

    #[test]
    fn slot_index_is_never_rewritten_by_apply_record_value() {
        assert_eq!(writes(RECORD_SRC, "position.slot_index"), 0);
    }
}
