use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::PauseSet;
use crate::common::seeds::{POOL_SEED, PROTOCOL_CONFIG_SEED};
use crate::state::{Pool, ProtocolConfig};

pub fn handler(ctx: Context<SetPause>, deposits: bool, rolls: bool, reason: u16) -> Result<()> {
    let pool = &mut ctx.accounts.pool;
    pool.deposits_paused = deposits;
    pool.rolls_paused = rolls;
    pool.pause_reason = reason;

    emit!(PauseSet {
        pool: pool.key(),
        slot: Clock::get()?.slot,
        authority: ctx.accounts.administrator.key(),
        deposits_paused: deposits,
        rolls_paused: rolls,
        reason,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct SetPause<'info> {
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
}
