use anchor_lang::prelude::*;

use crate::common::damm_v2::assert_immutable_identity;
use crate::common::errors::ByeMachineError;
use crate::common::events::SwapPoolChanged;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::state::ProtocolConfig;

/// Sets or clears the configured venue. A non-null pool is checked against the currently
/// configured $BYE mint and USDC mint on every update.
pub fn handler(ctx: Context<UpdateSwapPool>, swap_pool: Option<Pubkey>) -> Result<()> {
    match (swap_pool, ctx.accounts.swap_pool.as_ref()) {
        (Some(expected), Some(account)) => {
            require_keys_neq!(
                ctx.accounts.protocol_config.bye_mint,
                Pubkey::default(),
                ByeMachineError::SwapPoolInvalid
            );
            require_keys_eq!(account.key(), expected, ByeMachineError::SwapPoolInvalid);
            assert_immutable_identity(
                &account.key(),
                account.owner,
                &account.try_borrow_data()?,
                &ctx.accounts.protocol_config.bye_mint,
                &ctx.accounts.protocol_config.usdc_mint,
            )?;
        }
        (None, None) => {}
        _ => return Err(ByeMachineError::SwapPoolInvalid.into()),
    }

    let config = &mut ctx.accounts.protocol_config;
    let old_swap_pool = config.swap_pool;
    let swap_pool = swap_pool.unwrap_or_default();
    config.swap_pool = swap_pool;
    emit!(SwapPoolChanged {
        slot: Clock::get()?.slot,
        authority: ctx.accounts.administrator.key(),
        old_swap_pool,
        swap_pool
    });
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateSwapPool<'info> {
    pub administrator: Signer<'info>,
    #[account(mut, seeds = [PROTOCOL_CONFIG_SEED], bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized)]
    pub protocol_config: Account<'info, ProtocolConfig>,
    pub swap_pool: Option<UncheckedAccount<'info>>,
}
