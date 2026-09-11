use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::VrfConfigChanged;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::state::ProtocolConfig;

/// `vrf_program`, `oracle_queue` and `swap_pool` are write-once at `init_protocol` — nothing
/// else in the crate ever writes them, and `init_protocol` cannot run a second time against an
/// already-initialized `ProtocolConfig`. A value fixed at launch behind a provider that has not
/// been chosen yet, or a queue address a later migration retires, would otherwise have no way
/// back to a correct one for the life of the deployment.
pub fn handler(
    ctx: Context<UpdateVrfConfig>,
    vrf_program: Pubkey,
    oracle_queue: Pubkey,
    swap_pool: Pubkey,
) -> Result<()> {
    let config = &mut ctx.accounts.protocol_config;
    let old_vrf_program = config.vrf_program;
    let old_oracle_queue = config.oracle_queue;
    let old_swap_pool = config.swap_pool;

    config.vrf_program = vrf_program;
    config.oracle_queue = oracle_queue;
    config.swap_pool = swap_pool;

    emit!(VrfConfigChanged {
        slot: Clock::get()?.slot,
        authority: ctx.accounts.administrator.key(),
        old_vrf_program,
        old_oracle_queue,
        old_swap_pool,
        vrf_program,
        oracle_queue,
        swap_pool,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct UpdateVrfConfig<'info> {
    pub administrator: Signer<'info>,

    #[account(
        mut,
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,
}
