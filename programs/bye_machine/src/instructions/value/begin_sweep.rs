use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::SweepBegun;
use crate::common::seeds::{POOL_SEED, PROTOCOL_CONFIG_SEED};
use crate::state::{Pool, ProtocolConfig};

/// Returns the epoch to publish — the caller's current, unbumped `sweep_epoch`. `end_sweep`
/// owns the bump.
fn apply_begin_sweep(pool: &mut Pool) -> Result<u64> {
    require!(!pool.sweep_pending, ByeMachineError::SweepAlreadyOpen);
    pool.sweep_pending = true;
    pool.sweep_updates = 0;
    Ok(pool.sweep_epoch)
}

pub fn handler(ctx: Context<BeginSweep>) -> Result<()> {
    let sweep_epoch = apply_begin_sweep(&mut ctx.accounts.pool)?;

    emit!(SweepBegun {
        pool: ctx.accounts.pool.key(),
        slot: Clock::get()?.slot,
        authority: ctx.accounts.operator.key(),
        sweep_epoch,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct BeginSweep<'info> {
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

    const SRC: &str = include_str!("begin_sweep.rs");

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

    #[test]
    fn opens_the_window_and_resets_sweep_updates() {
        let mut pool = base_pool();
        pool.sweep_epoch = 7;
        pool.sweep_updates = 42;

        let emitted_epoch = apply_begin_sweep(&mut pool).unwrap();

        assert_eq!(emitted_epoch, 7, "must publish the pre-call epoch");
        assert!(pool.sweep_pending);
        assert_eq!(pool.sweep_updates, 0);
        assert_eq!(pool.sweep_epoch, 7, "begin_sweep must not bump the epoch");
    }

    #[test]
    fn rejects_a_second_open_at_6404() {
        let mut pool = base_pool();
        pool.sweep_pending = true;
        assert_eq!(error_code(apply_begin_sweep(&mut pool).unwrap_err()), 6404);
    }

    // --- Structural: declaration facts a behavioural test can't reach without a validator ---

    #[test]
    fn operator_constraint_present_once_and_no_other_signer_class() {
        assert_eq!(production(SRC).matches(OPERATOR_CONSTRAINT).count(), 1);
        assert_eq!(production(SRC).matches("administrator").count(), 0);
    }

    #[test]
    fn writes_exactly_the_two_sweep_open_fields() {
        assert_eq!(pool_field_writes(SRC), 2);
    }

    #[test]
    fn touches_no_weight_freeze_or_pause_surface() {
        assert_eq!(calls(SRC, "assert_weight_open"), 0);
        assert_eq!(production(SRC).matches("deposits_paused").count(), 0);
        assert_eq!(production(SRC).matches("rolls_paused").count(), 0);
    }

    #[test]
    fn every_pda_declares_its_seeds() {
        assert_eq!(production(SRC).matches("seeds = [").count(), 2);
        assert!(production(SRC).contains("seeds = [PROTOCOL_CONFIG_SEED]"));
        assert!(
            production(SRC).contains("seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()]")
        );
    }

    /// `apply_begin_sweep` returns the epoch, so polarity cannot be reordered away — the
    /// count pin only needs to confirm the emit itself was not dropped or duplicated.
    #[test]
    fn sweep_begun_is_emitted_exactly_once() {
        assert_eq!(production(SRC).matches("emit!(SweepBegun").count(), 1);
    }

    /// The only wiring fact a `Pool`-only test can't reach: that the handler emits the pure
    /// function's returned local, not a fresh read of `pool.sweep_epoch`.
    #[test]
    fn the_handler_emits_the_returned_local_not_a_fresh_account_read() {
        assert_eq!(calls(SRC, "apply_begin_sweep"), 1);
        assert!(production(SRC).contains("let sweep_epoch = apply_begin_sweep"));
        assert!(production(SRC).contains("sweep_epoch,\n"));
        assert_eq!(
            production(SRC)
                .matches("sweep_epoch: ctx.accounts.pool.sweep_epoch")
                .count(),
            0
        );
    }
}
