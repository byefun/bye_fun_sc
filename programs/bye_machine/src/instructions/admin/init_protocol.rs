use anchor_lang::prelude::*;

use crate::common::damm_v2::assert_immutable_identity;
use crate::common::errors::ByeMachineError;
use crate::common::events::ProtocolInitialized;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::instructions::admin::update_vrf_config::assert_slippage_bound;
use crate::program_id::INITIAL_ADMINISTRATOR;
use crate::state::ProtocolConfig;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ProtocolConfigArgs {
    pub operator: Pubkey,
    pub t02_authority: Pubkey,
    /// Treasury 05's delegated $BYE-purchase key. Required and rejected at the default: Treasury
    /// 05 is this key's USDC associated token account, and an unset key would pin settlement
    /// credits to an address nobody controls.
    pub t05_authority: Pubkey,
    pub protocol_revenue: Pubkey,
    pub usdc_mint: Pubkey,
    pub bye_mint: Option<Pubkey>,
    pub vrf_program: Pubkey,
    pub oracle_queue: Pubkey,
    pub swap_pool: Option<Pubkey>,
    pub max_swap_slippage_bps: u16,
}

pub fn handler(ctx: Context<InitProtocol>, args: ProtocolConfigArgs) -> Result<()> {
    let bye_mint = args.bye_mint.unwrap_or_default();
    let swap_pool = args.swap_pool.unwrap_or_default();

    // A pool is meaningful only alongside its $BYE mint. Both may be omitted before the $BYE
    // mint exists; an omitted value is represented in state by Pubkey::default().
    require!(
        args.swap_pool.is_none() || args.bye_mint.is_some(),
        ByeMachineError::SwapPoolInvalid
    );

    // Validated here, where the venue is chosen: a pool for another pair, or a non-pool account
    // the venue owns, would pin cleanly and leave blank settlement unable to pay. The account
    // supplied must be the address being written.
    match (args.swap_pool, ctx.accounts.swap_pool.as_ref()) {
        (Some(expected), Some(account)) => {
            require_keys_eq!(account.key(), expected, ByeMachineError::SwapPoolInvalid);
            assert_immutable_identity(
                &account.key(),
                account.owner,
                &account.try_borrow_data()?,
                &bye_mint,
                &args.usdc_mint,
            )?;
        }
        (None, None) => {}
        _ => return Err(ByeMachineError::SwapPoolInvalid.into()),
    }
    assert_slippage_bound(args.max_swap_slippage_bps)?;

    // Treasury 05 is the ATA of this key and no later instruction supplies it, so a default
    // must be refused here rather than surfacing as a failing settlement.
    require_keys_neq!(
        args.t05_authority,
        Pubkey::default(),
        ByeMachineError::T05AuthorityUnset
    );

    let config = &mut ctx.accounts.protocol_config;
    config.administrator = ctx.accounts.payer.key();
    config.operator = args.operator;
    config.t05_authority = args.t05_authority;
    config.t02_authority = args.t02_authority;
    config.protocol_revenue = args.protocol_revenue;
    config.usdc_mint = args.usdc_mint;
    config.bye_mint = bye_mint;
    config.vrf_program = args.vrf_program;
    config.oracle_queue = args.oracle_queue;
    config.swap_pool = swap_pool;
    config.max_swap_slippage_bps = args.max_swap_slippage_bps;
    config.pool_counter = 0;
    config.bump = ctx.bumps.protocol_config;

    emit!(ProtocolInitialized {
        slot: Clock::get()?.slot,
        administrator: config.administrator,
        operator: args.operator,
        t02_authority: args.t02_authority,
        t05_authority: args.t05_authority,
        protocol_revenue: args.protocol_revenue,
        usdc_mint: args.usdc_mint,
        bye_mint,
        vrf_program: args.vrf_program,
        oracle_queue: args.oracle_queue,
        swap_pool,
        max_swap_slippage_bps: args.max_swap_slippage_bps,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct InitProtocol<'info> {
    // Pinned to `INITIAL_ADMINISTRATOR`, and becomes `protocol_config.administrator`. `init`
    // makes this once-only but not *who* — deploy and init are separate transactions, and an
    // unpinned `payer` hands the privilege root to whoever signs first. A `constraint` rather
    // than `address =`, and `//` rather than `///`, because `INITIAL_ADMINISTRATOR` varies per
    // build feature and either would make the committed IDL environment-specific.
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

    /// CHECK: the pinned swap venue, validated by the adapter — owner, pool discriminator, the
    /// mint pair as a set, and both of its own token vaults. Not an `Account` because it belongs
    /// to another program, and no `address =` constraint because the value it is pinned to is an
    /// instruction argument rather than a constant; the handler binds the two.
    pub swap_pool: Option<UncheckedAccount<'info>>,

    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::production;

    const SRC: &str = include_str!("init_protocol.rs");

    /// `t02_authority`, `t05_authority` and `protocol_revenue` are adjacent same-width `Pubkey`
    /// arguments; a transposed assignment would misconfigure Treasury 05 silently, so each field
    /// is written from its own argument exactly once.
    #[test]
    fn each_config_field_is_written_from_its_own_argument() {
        let prod = production(SRC);
        for (field, arg) in [
            ("operator", "args.operator"),
            ("t05_authority", "args.t05_authority"),
            ("t02_authority", "args.t02_authority"),
            ("protocol_revenue", "args.protocol_revenue"),
            ("usdc_mint", "args.usdc_mint"),
            ("oracle_queue", "args.oracle_queue"),
            ("vrf_program", "args.vrf_program"),
        ] {
            let assignment = format!("config.{field} = {arg};");
            assert_eq!(
                prod.matches(&assignment).count(),
                1,
                "`{assignment}` is not the single writer of config.{field} — a transposed \
                 assignment between two same-width fields is invisible to every other instrument"
            );
        }
    }

    /// Each of the two treasury authority slots is written exactly once.
    #[test]
    fn the_two_treasury_authority_slots_are_each_written_exactly_once() {
        let prod = production(SRC);
        for field in ["t05_authority", "t02_authority"] {
            assert_eq!(
                prod.matches(&format!("config.{field} =")).count(),
                1,
                "config.{field} is written more or less than once"
            );
        }
    }

    /// Treasury 05 is derived from this key and nothing else validates it, so the default is
    /// refused at init.
    #[test]
    fn the_treasury_05_authority_is_refused_at_the_default() {
        let prod = production(SRC);
        assert!(
            prod.contains("args.t05_authority,")
                && prod.contains("ByeMachineError::T05AuthorityUnset"),
            "the default-key guard on t05_authority is gone"
        );
        assert_eq!(prod.matches("require_keys_neq!").count(), 1);
    }
}
