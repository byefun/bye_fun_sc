use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::ProtocolInitialized;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::program_id::INITIAL_ADMINISTRATOR;
use crate::state::ProtocolConfig;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ProtocolConfigArgs {
    pub operator: Pubkey,
    pub t03_authority: Pubkey,
    pub t02_authority: Pubkey,
    pub protocol_revenue: Pubkey,
    pub usdc_mint: Pubkey,
    pub bye_mint: Pubkey,
    pub vrf_program: Pubkey,
    pub oracle_queue: Pubkey,
    pub swap_pool: Pubkey,
    pub max_swap_slippage_bps: u16,
}

pub fn handler(ctx: Context<InitProtocol>, args: ProtocolConfigArgs) -> Result<()> {
    let config = &mut ctx.accounts.protocol_config;
    config.administrator = ctx.accounts.payer.key();
    config.pending_administrator = Pubkey::default();
    config.pending_operator = Pubkey::default();
    config.pending_t03_authority = Pubkey::default();
    config.pending_t02_authority = Pubkey::default();
    config.operator = args.operator;
    config.t03_authority = args.t03_authority;
    config.t02_authority = args.t02_authority;
    config.protocol_revenue = args.protocol_revenue;
    config.usdc_mint = args.usdc_mint;
    config.bye_mint = args.bye_mint;
    config.vrf_program = args.vrf_program;
    config.oracle_queue = args.oracle_queue;
    config.swap_pool = args.swap_pool;
    config.max_swap_slippage_bps = args.max_swap_slippage_bps;
    config.pool_counter = 0;
    config.bump = ctx.bumps.protocol_config;

    emit!(ProtocolInitialized {
        slot: Clock::get()?.slot,
        administrator: config.administrator,
        operator: args.operator,
        t02_authority: args.t02_authority,
        t03_authority: args.t03_authority,
        protocol_revenue: args.protocol_revenue,
        usdc_mint: args.usdc_mint,
        bye_mint: args.bye_mint,
        vrf_program: args.vrf_program,
        oracle_queue: args.oracle_queue,
        swap_pool: args.swap_pool,
        max_swap_slippage_bps: args.max_swap_slippage_bps,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct InitProtocol<'info> {
    // Pinned to `INITIAL_ADMINISTRATOR`, and becomes `protocol_config.administrator` below.
    // The `init` on `protocol_config` already makes this instruction once-only; what it does NOT
    // do is say *who* — deploy and init are separate transactions, and an unpinned `payer` hands
    // the root of the privilege set to whoever signs first in that window.
    //
    // Two reasons this is a `//` comment and a `constraint`, not a `///` doc and an `address =`:
    // anchor emits BOTH doc comments and `address =` constants into the IDL, `target/idl/
    // bye_machine.json` is tracked, and `INITIAL_ADMINISTRATOR` varies per build feature — so
    // either spelling would make the committed IDL environment-specific and force a regeneration
    // commit on every build-flag change. See `program_id.rs`.
    #[account(
        mut,
        constraint = payer.key() == INITIAL_ADMINISTRATOR @ ByeMachineError::Unauthorized,
    )]
    pub payer: Signer<'info>,

    #[account(
        init,
        payer = payer,
        space = 8 + ProtocolConfig::INIT_SPACE,
        seeds = [PROTOCOL_CONFIG_SEED],
        bump
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    pub system_program: Program<'info, System>,
}
