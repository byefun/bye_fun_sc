use anchor_lang::prelude::*;
use ephemeral_vrf_sdk::anchor::vrf_callback;

use crate::common::errors::ByeMachineError;
use crate::common::events::BatchResolved;
use crate::common::math::aggregate_weight;
use crate::common::seeds::{BATCH_SEED, POOL_SEED};
use crate::state::{BatchState, Pool, RollBatch};

/// The one instruction no party to this system can invoke.
///
/// The sole signer is the VRF program's scoped identity, `PDA([b"identity", bye_machine], vrf)`
/// — which is not the identity that signs the request. Substituting the request-side one, the
/// provider's legacy global identity, or any bye.fun authority fails the address constraint.
/// Dropping that constraint is the provider's documented top integration footgun: it leaves the
/// callback invocable by anyone.
///
/// Three accounts, not two: `pool` is here because the batch's running weight expectation is
/// seeded from live pool state at this point. The account list is fixed into the request's
/// stored metas by `commit_rolls`, so a batch committed with the pool absent could never resolve.
#[vrf_callback]
#[derive(Accounts)]
pub struct VrfCallback<'info> {
    /// Declared rather than left to `#[vrf_callback]` to inject, so the one signer is visible
    /// in this file and to the crate-wide source pins — an injected field is invisible to them,
    /// and a pinned body that omitted the signer would understate the very account surface the
    /// pin exists to fix. The constraint is the SDK's own derivation, unchanged.
    ///
    /// The macro is kept even though this declaration leaves it nothing to inject: it is the
    /// fail-safe. Delete this field and the macro puts the signer back; delete the macro too
    /// and the callback becomes invocable by anyone, silently.
    #[account(address = ephemeral_vrf_sdk::consts::scoped_vrf_identity(&crate::ID))]
    pub vrf_program_identity: Signer<'info>,

    #[account(
        mut,
        seeds = [BATCH_SEED, batch.pool.as_ref(), batch.batch_id.to_le_bytes().as_ref()],
        bump = batch.bump
    )]
    pub batch: Box<Account<'info, RollBatch>>,

    #[account(
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump,
        constraint = pool.key() == batch.pool @ ByeMachineError::BatchPoolMismatch
    )]
    pub pool: Box<Account<'info, Pool>>,
}

/// The state transition and both writes, over plain state.
///
/// Write-once is enforced by the state machine rather than by inspecting `randomness` for a
/// zero value: a second delivery finds the batch `Resolved` and is refused here, and a delivery
/// arriving after the recovery window finds it `Recovered` and is refused on the same line.
/// Zero is a legitimate 32-byte draw, so it cannot be the sentinel.
fn apply_resolve(batch: &mut RollBatch, pool: &Pool, randomness: [u8; 32]) -> Result<()> {
    require!(
        batch.state == BatchState::Committed,
        ByeMachineError::BatchNotResolved
    );

    batch.randomness = randomness;
    batch.state = BatchState::Resolved;
    // The running weight expectation each roll settlement checks against, seeded here and
    // rewritten by each settlement from its own draw and rebalance. Not a commitment snapshot:
    // the pipeline legitimately moves `W` between rolls, so equality with the value at
    // commitment would fail the batch at roll 1.
    batch.expected_w = aggregate_weight(pool.w_real, pool.blanks, pool.blank_faces)?;
    batch.expected_blanks = pool.blanks;

    Ok(())
}

pub fn handler(ctx: Context<VrfCallback>, randomness: [u8; 32]) -> Result<()> {
    apply_resolve(&mut ctx.accounts.batch, &ctx.accounts.pool, randomness)?;

    emit!(BatchResolved {
        pool: ctx.accounts.pool.key(),
        slot: Clock::get()?.slot,
        batch: ctx.accounts.batch.key(),
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::WEIGHT_C;
    use crate::common::test_support::production;

    const SRC: &str = include_str!("vrf_callback.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    /// A local copy rather than a reach into `commit_rolls` — this module's subject is the
    /// transition, and the roll array is inert to it.
    fn undrawn() -> crate::state::RollRecord {
        crate::state::RollRecord {
            status: crate::state::RollStatus::Pending,
            outcome: crate::state::RollOutcome::None,
            selected_mint: Pubkey::default(),
            blank_face_idx: 0,
            draw_value: 0,
            attempts: 0,
            w_at_draw: 0,
            n_real_at_draw: 0,
            blanks_at_draw: [0; 3],
            refund_reason: 0,
        }
    }

    fn committed_batch() -> RollBatch {
        RollBatch {
            pool: Pubkey::new_unique(),
            roller: Pubkey::new_unique(),
            batch_id: 0,
            state: BatchState::Committed,
            n: 3,
            next_roll: 0,
            settled: 0,
            refunded: 0,
            request_slot: 100,
            caller_seed: [7u8; 32],
            randomness: [0u8; 32],
            expected_w: 0,
            expected_blanks: [0; 3],
            s_price: 9_900_000,
            s_ticket: 9_000_000,
            s_fee: 900_000,
            s_alloc_bps: [7_500, 500, 2_000],
            s_blank_faces: [1_000_000, 2_000_000, 5_000_000],
            rolls: [undrawn(); 10],
            bump: 255,
        }
    }

    fn live_pool() -> Pool {
        let mut pool = Pool::blank();
        pool.blank_faces = [1_000_000, 2_000_000, 5_000_000];
        pool.blanks = [4, 2, 1];
        pool.w_real = 12_345;
        pool
    }

    // --- The success path ----------------------------------------------------------------

    #[test]
    fn writes_the_randomness_and_resolves_the_batch() {
        let mut batch = committed_batch();
        let pool = live_pool();
        apply_resolve(&mut batch, &pool, [9u8; 32]).unwrap();

        assert_eq!(batch.randomness, [9u8; 32]);
        assert!(batch.state == BatchState::Resolved);
    }

    /// Zero is a legitimate draw, so the write-once guard cannot key on it — this pins that a
    /// zero delivery is accepted and still resolves, which is what forces the state machine to
    /// carry the property instead.
    #[test]
    fn accepts_an_all_zero_draw_as_a_real_value() {
        let mut batch = committed_batch();
        apply_resolve(&mut batch, &live_pool(), [0u8; 32]).unwrap();
        assert!(batch.state == BatchState::Resolved);
    }

    /// Seeded from **live** pool state, not from anything the batch carries.
    #[test]
    fn seeds_the_expectation_from_live_pool_state() {
        let mut batch = committed_batch();
        let pool = live_pool();
        apply_resolve(&mut batch, &pool, [1u8; 32]).unwrap();

        let blank_weight: u128 = (4 * (WEIGHT_C / 1_000_000))
            + (2 * (WEIGHT_C / 2_000_000))
            + (WEIGHT_C / 5_000_000);
        assert_eq!(batch.expected_w, 12_345 + blank_weight);
        assert_eq!(batch.expected_blanks, [4, 2, 1]);
    }

    /// The expectation tracks the pool as it is at delivery, not as it was at commitment: two
    /// batches resolving against different pool states must seed different expectations.
    #[test]
    fn the_expectation_follows_the_pool_rather_than_the_commitment() {
        let mut first = committed_batch();
        let mut second = committed_batch();
        let mut pool = live_pool();

        apply_resolve(&mut first, &pool, [1u8; 32]).unwrap();
        pool.w_real += 500;
        pool.blanks = [5, 2, 1];
        apply_resolve(&mut second, &pool, [1u8; 32]).unwrap();

        assert_ne!(first.expected_w, second.expected_w);
        assert_eq!(first.expected_blanks, [4, 2, 1]);
        assert_eq!(second.expected_blanks, [5, 2, 1]);
    }

    // --- Write-once, behaviourally -------------------------------------------------------

    /// A second delivery reverts. Driven by calling twice rather than by reading the handler.
    #[test]
    fn a_second_delivery_reverts_at_6206() {
        let mut batch = committed_batch();
        apply_resolve(&mut batch, &live_pool(), [1u8; 32]).unwrap();
        assert_eq!(
            error_code(apply_resolve(&mut batch, &live_pool(), [2u8; 32]).unwrap_err()),
            6206
        );
    }

    /// And it leaves the first value in place — a revert that still overwrote would satisfy a
    /// test that only checked the error.
    #[test]
    fn a_refused_second_delivery_does_not_overwrite_the_first() {
        let mut batch = committed_batch();
        apply_resolve(&mut batch, &live_pool(), [1u8; 32]).unwrap();
        let mut other_pool = live_pool();
        other_pool.w_real = 999_999;

        assert!(apply_resolve(&mut batch, &other_pool, [2u8; 32]).is_err());

        assert_eq!(batch.randomness, [1u8; 32], "randomness was overwritten");
        assert_ne!(batch.expected_w, 999_999, "the expectation was re-seeded");
        assert!(batch.state == BatchState::Resolved);
    }

    /// A callback arriving after recovery fails on the same guard — the batch is `Recovered`,
    /// its rolls are refunded, and late randomness must not revive it.
    #[test]
    fn a_callback_after_recovery_is_refused_at_6206() {
        let mut batch = committed_batch();
        batch.state = BatchState::Recovered;
        assert_eq!(
            error_code(apply_resolve(&mut batch, &live_pool(), [1u8; 32]).unwrap_err()),
            6206
        );
        assert_eq!(batch.randomness, [0u8; 32]);
    }

    /// Every non-`Committed` state is refused, not just the two named above — the guard is an
    /// equality on `Committed`, so a fifth batch state added later is refused by construction.
    #[test]
    fn every_non_committed_state_is_refused() {
        for state in [
            BatchState::Resolved,
            BatchState::Recovered,
            BatchState::Complete,
        ] {
            let mut batch = committed_batch();
            batch.state = state;
            assert_eq!(
                error_code(apply_resolve(&mut batch, &live_pool(), [1u8; 32]).unwrap_err()),
                6206
            );
        }
    }

    // --- Structural ----------------------------------------------------------------------

    /// The signer is pinned to the VRF program's **scoped** identity. The request-side identity
    /// is a different PDA, and confusing the two is the provider's documented top footgun.
    #[test]
    fn the_signer_is_pinned_to_the_scoped_identity() {
        let prod = production(SRC);
        assert!(prod.contains("address = ephemeral_vrf_sdk::consts::scoped_vrf_identity(&crate::ID)"));
        assert_eq!(
            prod.matches("VRF_PROGRAM_IDENTITY").count(),
            0,
            "the provider's deprecated global identity must not appear"
        );
        assert_eq!(
            prod.matches("REQUEST_IDENTITY").count(),
            0,
            "the request-side identity is not what signs a callback"
        );
    }

    /// The batch is reached through its own PDA and the pool is cross-checked against it, so a
    /// self-consistent but wrong pair cannot resolve one batch against another pool's weights.
    #[test]
    fn the_pool_is_cross_checked_against_the_batch() {
        assert!(production(SRC)
            .contains("constraint = pool.key() == batch.pool @ ByeMachineError::BatchPoolMismatch"));
    }

    /// This handler writes randomness, the state, and the expectation pair — nothing else. In
    /// particular it does not touch the pool, which it holds read-only.
    #[test]
    fn writes_exactly_four_batch_fields_and_no_pool_field() {
        let prod = production(SRC);
        for field in [
            "batch.randomness = ",
            "batch.state = ",
            "batch.expected_w = ",
            "batch.expected_blanks = ",
        ] {
            assert_eq!(prod.matches(field).count(), 1, "{field}");
        }
        let pool_writes = prod
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                t.starts_with("pool.") && t.contains(" = ")
            })
            .count();
        assert_eq!(pool_writes, 0, "the callback must not write the pool");
    }
}
