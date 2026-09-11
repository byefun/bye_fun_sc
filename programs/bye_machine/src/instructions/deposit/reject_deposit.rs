use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::DepositRejected;
use crate::common::guards::assert_not_treasury_position;
use crate::common::seeds::{POSITION_SEED, PROTOCOL_CONFIG_SEED};
use crate::state::{Position, PositionState, ProtocolConfig};

pub fn handler(ctx: Context<RejectDeposit>, reason: u16) -> Result<()> {
    let authority = ctx.accounts.operator.key();

    let position = &mut ctx.accounts.position;
    require!(
        position.state == PositionState::Pending,
        ByeMachineError::PositionNotPending
    );
    assert_not_treasury_position(&position.pool, &position.depositor)?;
    position.state = PositionState::Rejected;
    position.reject_reason = reason;

    emit!(DepositRejected {
        pool: position.pool,
        slot: Clock::get()?.slot,
        authority,
        position: position.key(),
        reason,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct RejectDeposit<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump
    )]
    pub position: Box<Account<'info, Position>>,
}
