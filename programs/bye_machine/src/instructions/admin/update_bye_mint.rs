use anchor_lang::prelude::*;
use anchor_spl::token::Mint;

use crate::common::errors::ByeMachineError;
use crate::common::events::ByeMintChanged;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::state::ProtocolConfig;

/// Sets or clears the protocol $BYE mint. A configured swap pool must be cleared first so the
/// protocol never records a pool whose pair no longer matches its mint.
pub fn handler(ctx: Context<UpdateByeMint>, bye_mint: Option<Pubkey>) -> Result<()> {
    require_keys_eq!(
        ctx.accounts.protocol_config.swap_pool,
        Pubkey::default(),
        ByeMachineError::SwapPoolInvalid
    );

    match (bye_mint, ctx.accounts.bye_mint.as_ref()) {
        (Some(expected), Some(mint)) => {
            require_keys_eq!(mint.key(), expected, ByeMachineError::SwapPoolInvalid)
        }
        (None, None) => {}
        _ => return Err(ByeMachineError::SwapPoolInvalid.into()),
    }

    let config = &mut ctx.accounts.protocol_config;
    let old_bye_mint = config.bye_mint;
    let bye_mint = bye_mint.unwrap_or_default();
    config.bye_mint = bye_mint;

    emit!(ByeMintChanged {
        slot: Clock::get()?.slot,
        authority: ctx.accounts.administrator.key(),
        old_bye_mint,
        bye_mint,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateByeMint<'info> {
    pub administrator: Signer<'info>,
    #[account(mut, seeds = [PROTOCOL_CONFIG_SEED], bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized)]
    pub protocol_config: Account<'info, ProtocolConfig>,
    pub bye_mint: Option<Account<'info, Mint>>,
}
