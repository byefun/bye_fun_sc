use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::CollectionWithdrawn;
use crate::common::seeds::{COLLECTION_SEED, POOL_SEED, PROTOCOL_CONFIG_SEED};
use crate::state::{Pool, PoolCollection, ProtocolConfig};

/// Closes the admission record and does nothing else. It blocks new deposits and instant sells of
/// that collection and leaves every existing position active, accruing, selectable, and exiting on
/// its own rules — structurally, not by promise: a closed account cannot reach a `Position`, and
/// `Position.standard` was written once at intake and is never re-read from here.
///
/// Emptying the admitted set is permitted and not guarded. It blocks every new deposit pool-wide,
/// which is a power the Administrator already holds through the deposit pause, and a guard against
/// closing the last record would add a failure mode with no safety gained.
pub fn handler(ctx: Context<WithdrawCollection>, collection: Pubkey, reason: u16) -> Result<()> {
    let standards = ctx.accounts.pool_collection.standards;

    emit!(CollectionWithdrawn {
        pool: ctx.accounts.pool.key(),
        slot: Clock::get()?.slot,
        collection,
        standards,
        reason,
        authority: ctx.accounts.administrator.key(),
    });

    Ok(())
}

#[derive(Accounts)]
#[instruction(collection: Pubkey)]
pub struct WithdrawCollection<'info> {
    #[account(mut)]
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    #[account(
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,

    #[account(
        mut,
        close = administrator,
        seeds = [COLLECTION_SEED, pool.key().as_ref(), collection.as_ref()],
        bump = pool_collection.bump
    )]
    pub pool_collection: Account<'info, PoolCollection>,
}
