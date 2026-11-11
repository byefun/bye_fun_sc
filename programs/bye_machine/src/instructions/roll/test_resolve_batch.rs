//! The resolution a validator suite cannot otherwise reach.
//!
//! `vrf_callback`'s only signer is the provider's scoped identity PDA, and no party to this
//! system can produce it. That is the property which makes the callback safe, and it is also
//! what leaves every roll instruction with no reachable happy path against a validator: no
//! batch reaches `Resolved`, so `settle_roll` cannot be driven at all. This instruction stands
//! in for the oracle's delivery.
//!
//! **It exists only in a build carrying the `testing` feature.** The devnet and mainnet
//! artifacts are built without it, where it has no discriminator, no IDL entry and no dispatch
//! arm — the feature gate is honoured by the framework on all three.
//!
//! **It reuses the callback's transition rather than restating it.** `apply_resolve` is the one
//! writer of the resolved shape: the randomness and the state flip. A second implementation here
//! would let the suite observe a batch shape a real delivery never produces, which would quietly
//! invalidate every measurement taken downstream of it. What this instruction bypasses is the
//! signature, and nothing else — the state machine still refuses a batch that is not
//! `Committed`, so a second resolution and a resolution after recovery are refused here exactly
//! as they are on the real path.
//!
//! It emits `BatchResolved` with the same fields the callback does, so a batch resolved here is
//! indistinguishable from a delivered one in its events as well as its state. Anything the suite
//! observes downstream is therefore observing the shape production produces.

use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::BatchResolved;
use crate::common::seeds::{BATCH_SEED, POOL_SEED, PROTOCOL_CONFIG_SEED};
use crate::instructions::roll::vrf_callback::apply_resolve;
use crate::state::{Pool, ProtocolConfig, RollBatch};

#[derive(Accounts)]
pub struct TestResolveBatch<'info> {
    /// The Operator, not an arbitrary signer: a test-only instruction that was permissionless
    /// in its own build would be the one place the suite could not tell an authority failure
    /// from a missing authority check.
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

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

pub fn handler(ctx: Context<TestResolveBatch>, randomness: [u8; 32]) -> Result<()> {
    apply_resolve(&mut ctx.accounts.batch, randomness)?;

    emit!(BatchResolved {
        pool: ctx.accounts.pool.key(),
        slot: Clock::get()?.slot,
        batch: ctx.accounts.batch.key(),
    });

    Ok(())
}
