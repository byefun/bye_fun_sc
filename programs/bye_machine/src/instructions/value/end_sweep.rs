use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::SweepEnded;
use crate::common::seeds::{POOL_SEED, PROTOCOL_CONFIG_SEED};
use crate::state::{Pool, ProtocolConfig};

/// Returns `(closing_epoch, positions_updated)` to publish. `closing_epoch` is captured before
/// the bump — the same value `SweepBegun` emitted — and `positions_updated`
/// is read from `sweep_updates`, which this function never resets; `begin_sweep` owns that.
fn apply_end_sweep(pool: &mut Pool, now: i64) -> Result<(u64, u32)> {
    require!(pool.sweep_pending, ByeMachineError::SweepNotOpen);

    let closing_epoch = pool.sweep_epoch;
    let positions_updated = pool.sweep_updates;

    pool.sweep_pending = false;
    pool.sweep_epoch = closing_epoch
        .checked_add(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    pool.last_sweep_at = now;

    Ok((closing_epoch, positions_updated))
}

pub fn handler(ctx: Context<EndSweep>) -> Result<()> {
    let clock = Clock::get()?;
    let (closing_epoch, positions_updated) =
        apply_end_sweep(&mut ctx.accounts.pool, clock.unix_timestamp)?;

    emit!(SweepEnded {
        pool: ctx.accounts.pool.key(),
        slot: clock.slot,
        authority: ctx.accounts.operator.key(),
        sweep_epoch: closing_epoch,
        positions_updated,
        last_sweep_at: clock.unix_timestamp,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct EndSweep<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = include_str!("end_sweep.rs");

    const OPERATOR_CONSTRAINT: &str =
        "constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized";

    /// Every `pool.<field> = ` assignment ahead of the account surface, one per statement
    /// regardless of how many lines it spans. A mechanical count, not a hand-picked list.
    use crate::common::test_support::{calls, is_assignment, production};

    fn pool_field_writes(source: &str) -> usize {
        let source = production(source);
        let end = source.find("#[derive(Accounts)]").unwrap();
        source[..end]
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                t.starts_with("pool.")
                    && match t.find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '.') {
                        Some(i) => is_assignment(&t[i..]),
                        None => false,
                    }
            })
            .count()
    }

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn base_pool() -> Pool {
        Pool::blank()
    }

    // --- Behavioural: the `Pool` mutation, over a plain struct -----------------------------

    /// The returned (and therefore emitted) epoch is
    /// the pre-call value, and `pool.sweep_epoch` afterwards is one higher — the pair a
    /// polarity reversal or a reordered capture would both get wrong.
    #[test]
    fn closes_the_window_and_bumps_the_epoch() {
        let mut pool = base_pool();
        pool.sweep_pending = true;
        pool.sweep_epoch = 7;
        pool.sweep_updates = 5;

        let (closing_epoch, positions_updated) = apply_end_sweep(&mut pool, 1_000).unwrap();

        assert_eq!(closing_epoch, 7, "must publish the pre-bump epoch");
        assert_eq!(positions_updated, 5);
        assert!(!pool.sweep_pending);
        assert_eq!(pool.sweep_epoch, 8, "must bump exactly once");
        assert_eq!(pool.last_sweep_at, 1_000);
        assert_eq!(
            pool.sweep_updates, 5,
            "end_sweep must not reset sweep_updates — begin_sweep owns that"
        );
    }

    #[test]
    fn rejects_when_not_open_at_6402() {
        let mut pool = base_pool();
        assert_eq!(error_code(apply_end_sweep(&mut pool, 0).unwrap_err()), 6402);
    }

    #[test]
    fn epoch_overflow_returns_arithmetic_failure_at_6703() {
        let mut pool = base_pool();
        pool.sweep_pending = true;
        pool.sweep_epoch = u64::MAX;
        assert_eq!(error_code(apply_end_sweep(&mut pool, 0).unwrap_err()), 6703);
    }

    // --- Structural: declaration facts a behavioural test can't reach without a validator ---

    #[test]
    fn operator_constraint_present_once() {
        assert_eq!(production(SRC).matches(OPERATOR_CONSTRAINT).count(), 1);
    }

    #[test]
    fn writes_exactly_the_three_close_out_fields() {
        assert_eq!(pool_field_writes(SRC), 3);
    }

    #[test]
    fn touches_no_weight_or_tier_surface() {
        for term in ["weight_index", "w_real", "top_tier", "assert_weight_open"] {
            assert_eq!(
                production(SRC).matches(term).count(),
                0,
                "{term} should not appear"
            );
        }
    }

    #[test]
    fn every_pda_declares_its_seeds() {
        assert_eq!(production(SRC).matches("seeds = [").count(), 2);
        assert!(production(SRC).contains("seeds = [PROTOCOL_CONFIG_SEED]"));
        assert!(
            production(SRC).contains("seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()]")
        );
    }

    /// The only wiring fact a `Pool`-only test can't reach: that the handler emits the pure
    /// function's returned locals, not a fresh (post-bump) read of `pool.sweep_epoch`.
    #[test]
    fn the_handler_emits_the_returned_locals_not_a_fresh_account_read() {
        assert_eq!(calls(SRC, "apply_end_sweep"), 1);
        assert!(production(SRC).contains("sweep_epoch: closing_epoch,"));
        assert_eq!(
            production(SRC)
                .matches("sweep_epoch: ctx.accounts.pool.sweep_epoch")
                .count(),
            0
        );
    }
}
