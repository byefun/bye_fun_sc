use anchor_lang::prelude::*;
use ephemeral_vrf_sdk::anchor::vrf_callback;

use crate::common::errors::ByeMachineError;
use crate::common::events::BatchResolved;
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
/// Three accounts, not two: `pool` is here for the cross-check constraint below, not to read
/// anything from it. The expectation pair is not seeded at resolution — it is
/// seeded by `settle_roll`/`settle_roll_core` at this batch's own `roll_index == 0`, which is
/// closer to head-of-queue activation and cannot observe a stale value from batches still ahead
/// of it. The account list is fixed into the request's stored metas by `commit_rolls`, so a batch
/// committed with the pool absent could never resolve.
#[vrf_callback]
#[derive(Accounts)]
pub struct VrfCallback<'info> {
    /// Declared rather than left to `#[vrf_callback]` to inject, so the one signer is visible in
    /// this file. The constraint is the SDK's own derivation, unchanged.
    ///
    /// The macro is kept as the fail-safe: delete this field and the macro puts the signer back;
    /// delete the macro too and the callback becomes invocable by anyone, silently.
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
pub(crate) fn apply_resolve(batch: &mut RollBatch, randomness: [u8; 32]) -> Result<()> {
    require!(
        batch.state == BatchState::Committed,
        ByeMachineError::BatchNotResolved
    );

    batch.randomness = randomness;
    batch.state = BatchState::Resolved;

    Ok(())
}

pub fn handler(ctx: Context<VrfCallback>, randomness: [u8; 32]) -> Result<()> {
    apply_resolve(&mut ctx.accounts.batch, randomness)?;

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
            blanks_at_draw: [0; 8],
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
            expected_blanks: [0; 8],
            s_price: 9_900_000,
            s_ticket: 9_000_000,
            s_fee: 900_000,
            s_alloc_bps: [7_500, 500, 2_000],
            s_blank_faces: [1_000_000, 2_000_000, 5_000_000, 0, 0, 0, 0, 0],
            rolls: [undrawn(); 10],
            bump: 255,
        }
    }

    // --- The success path ----------------------------------------------------------------

    #[test]
    fn writes_the_randomness_and_resolves_the_batch() {
        let mut batch = committed_batch();
        apply_resolve(&mut batch, [9u8; 32]).unwrap();

        assert_eq!(batch.randomness, [9u8; 32]);
        assert!(batch.state == BatchState::Resolved);
    }

    /// Zero is a legitimate draw, so the write-once guard cannot key on it — this pins that a
    /// zero delivery is accepted and still resolves, which is what forces the state machine to
    /// carry the property instead.
    #[test]
    fn accepts_an_all_zero_draw_as_a_real_value() {
        let mut batch = committed_batch();
        apply_resolve(&mut batch, [0u8; 32]).unwrap();
        assert!(batch.state == BatchState::Resolved);
    }

    // --- Write-once, behaviourally -------------------------------------------------------

    /// A second delivery reverts. Driven by calling twice rather than by reading the handler.
    #[test]
    fn a_second_delivery_reverts_at_6206() {
        let mut batch = committed_batch();
        apply_resolve(&mut batch, [1u8; 32]).unwrap();
        assert_eq!(
            error_code(apply_resolve(&mut batch, [2u8; 32]).unwrap_err()),
            6206
        );
    }

    /// And it leaves the first value in place — a revert that still overwrote would satisfy a
    /// test that only checked the error.
    #[test]
    fn a_refused_second_delivery_does_not_overwrite_the_first() {
        let mut batch = committed_batch();
        apply_resolve(&mut batch, [1u8; 32]).unwrap();

        assert!(apply_resolve(&mut batch, [2u8; 32]).is_err());

        assert_eq!(batch.randomness, [1u8; 32], "randomness was overwritten");
        assert!(batch.state == BatchState::Resolved);
    }

    /// A callback arriving after recovery fails on the same guard — the batch is `Recovered`,
    /// its rolls are refunded, and late randomness must not revive it.
    #[test]
    fn a_callback_after_recovery_is_refused_at_6206() {
        let mut batch = committed_batch();
        batch.state = BatchState::Recovered;
        assert_eq!(
            error_code(apply_resolve(&mut batch, [1u8; 32]).unwrap_err()),
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
                error_code(apply_resolve(&mut batch, [1u8; 32]).unwrap_err()),
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
        assert!(
            prod.contains("address = ephemeral_vrf_sdk::consts::scoped_vrf_identity(&crate::ID)")
        );
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
        assert!(production(SRC).contains(
            "constraint = pool.key() == batch.pool @ ByeMachineError::BatchPoolMismatch"
        ));
    }

    /// This handler writes randomness and the state — nothing else. It does not
    /// seed the expectation pair, and it never touches the pool, which it holds read-only.
    #[test]
    fn writes_exactly_two_batch_fields_and_no_pool_field() {
        let prod = production(SRC);
        for field in ["batch.randomness = ", "batch.state = "] {
            assert_eq!(prod.matches(field).count(), 1, "{field}");
        }
        assert_eq!(
            prod.matches("batch.expected_w").count(),
            0,
            "the expectation pair is seeded by settlement, not by the callback"
        );
        assert_eq!(prod.matches("batch.expected_blanks").count(), 0);
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
