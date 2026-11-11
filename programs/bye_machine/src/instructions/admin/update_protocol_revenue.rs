use anchor_lang::prelude::*;
use anchor_spl::token::TokenAccount;

use crate::common::errors::ByeMachineError;
use crate::common::events::ProtocolRevenueChanged;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::state::ProtocolConfig;

/// Repoints `ProtocolConfig.protocol_revenue`. Every pool's `principal_vault` is minted from
/// `usdc_mint` (see `init_pool`), and settlement enforces `protocol_revenue.mint ==
/// principal_vault.mint`, so the new account is checked against `usdc_mint` directly rather than
/// against any one pool's vault.
pub fn handler(ctx: Context<UpdateProtocolRevenue>) -> Result<()> {
    let config = &mut ctx.accounts.protocol_config;
    let old_protocol_revenue = config.protocol_revenue;
    let protocol_revenue = ctx.accounts.protocol_revenue.key();
    config.protocol_revenue = protocol_revenue;

    emit!(ProtocolRevenueChanged {
        slot: Clock::get()?.slot,
        authority: ctx.accounts.administrator.key(),
        old_protocol_revenue,
        protocol_revenue,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateProtocolRevenue<'info> {
    pub administrator: Signer<'info>,
    #[account(mut, seeds = [PROTOCOL_CONFIG_SEED], bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized)]
    pub protocol_config: Account<'info, ProtocolConfig>,
    #[account(constraint = protocol_revenue.mint == protocol_config.usdc_mint @ ByeMachineError::StandardNotAdmitted)]
    pub protocol_revenue: Box<Account<'info, TokenAccount>>,
}
