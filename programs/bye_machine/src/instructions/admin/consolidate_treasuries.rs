//! Moves a pool created before Treasury 03 and Treasury 04 were shared onto the shared accounts.
//!
//! Such a pool holds its own Treasury 03 and Treasury 04 at `["t03", pool]` and `["t04", pool]`,
//! with the pool as their authority. This instruction moves both balances into the shared
//! `["t03"]` and `["t04"]` (creating either if absent), points the pool at them, and closes the
//! per-pool accounts, their rent going to the Administrator. Nothing else may then debit what the
//! per-pool accounts held, since they no longer exist.
//!
//! **Once per pool, by construction.** A second call finds the per-pool accounts closed and fails
//! deserializing them; a pool `init_pool` created with the shared accounts never had them.
//!
//! **Refused while the pool has a batch open.** That batch settles its blanks against the pool's
//! own Treasury 04 and `Pool.treasury_04`, and this empties the one and rewrites the other.

use anchor_lang::prelude::*;
use anchor_spl::token::{
    close_account, transfer, CloseAccount, Mint, Token, TokenAccount, Transfer,
};

use crate::common::errors::ByeMachineError;
use crate::common::events::TreasuriesConsolidated;
use crate::common::guards::assert_weight_open;
use crate::common::seeds::{POOL_SEED, PROTOCOL_CONFIG_SEED, TREASURY_03_SEED, TREASURY_04_SEED};
use crate::state::{Pool, ProtocolConfig};

pub fn handler(ctx: Context<ConsolidateTreasuries>) -> Result<()> {
    assert_weight_open(&ctx.accounts.pool)?;

    let amount_03 = ctx.accounts.legacy_treasury_03.amount;
    let amount_04 = ctx.accounts.legacy_treasury_04.amount;

    let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();
    let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];
    let pool_signer: &[&[&[u8]]] = &[pool_seeds];

    empty_and_close(
        ctx.accounts,
        &ctx.accounts.legacy_treasury_03,
        &ctx.accounts.treasury_03,
        amount_03,
        pool_signer,
    )?;
    empty_and_close(
        ctx.accounts,
        &ctx.accounts.legacy_treasury_04,
        &ctx.accounts.treasury_04,
        amount_04,
        pool_signer,
    )?;

    let old_treasury_03 = ctx.accounts.legacy_treasury_03.key();
    let old_treasury_04 = ctx.accounts.legacy_treasury_04.key();
    let treasury_03 = ctx.accounts.treasury_03.key();
    let treasury_04 = ctx.accounts.treasury_04.key();

    let pool = &mut ctx.accounts.pool;
    pool.treasury_03 = treasury_03;
    pool.treasury_04 = treasury_04;

    emit!(TreasuriesConsolidated {
        pool: pool.key(),
        slot: Clock::get()?.slot,
        authority: ctx.accounts.administrator.key(),
        old_treasury_03,
        old_treasury_04,
        treasury_03,
        treasury_04,
        amount_03,
        amount_04,
    });

    Ok(())
}

/// Moves `amount`, the per-pool account's whole balance, to the shared account, then closes the
/// per-pool account with its rent to the Administrator. Both legs are signed by the pool, the
/// per-pool account's authority. A zero balance skips the transfer and still closes.
fn empty_and_close<'info>(
    accounts: &ConsolidateTreasuries<'info>,
    legacy: &Account<'info, TokenAccount>,
    shared: &Account<'info, TokenAccount>,
    amount: u64,
    pool_signer: &[&[&[u8]]],
) -> Result<()> {
    if amount > 0 {
        transfer(
            CpiContext::new_with_signer(
                accounts.token_program.to_account_info(),
                Transfer {
                    from: legacy.to_account_info(),
                    to: shared.to_account_info(),
                    authority: accounts.pool.to_account_info(),
                },
                pool_signer,
            ),
            amount,
        )?;
    }
    close_account(CpiContext::new_with_signer(
        accounts.token_program.to_account_info(),
        CloseAccount {
            account: legacy.to_account_info(),
            destination: accounts.administrator.to_account_info(),
            authority: accounts.pool.to_account_info(),
        },
        pool_signer,
    ))
}

#[derive(Accounts)]
pub struct ConsolidateTreasuries<'info> {
    /// Pays for the shared accounts when absent, and receives the per-pool accounts' rent.
    #[account(mut)]
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// The pool's own Treasury 03, which the pool must still name.
    #[account(
        mut,
        seeds = [TREASURY_03_SEED, pool.key().as_ref()],
        bump,
        address = pool.treasury_03 @ ByeMachineError::TreasuryNotPerPool
    )]
    pub legacy_treasury_03: Box<Account<'info, TokenAccount>>,

    /// The pool's own Treasury 04. `Pool` never recorded its address, so its seeds are the pin.
    #[account(
        mut,
        seeds = [TREASURY_04_SEED, pool.key().as_ref()],
        bump
    )]
    pub legacy_treasury_04: Box<Account<'info, TokenAccount>>,

    /// The shared Treasury 03, created here when no pool has created it yet — see `init_pool`.
    #[account(
        init_if_needed,
        payer = administrator,
        seeds = [TREASURY_03_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = protocol_config
    )]
    pub treasury_03: Box<Account<'info, TokenAccount>>,

    /// The shared Treasury 04, on Treasury 03's terms.
    #[account(
        init_if_needed,
        payer = administrator,
        seeds = [TREASURY_04_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = protocol_config
    )]
    pub treasury_04: Box<Account<'info, TokenAccount>>,

    #[account(address = protocol_config.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, code, field_writes, production};

    const SRC: &str = include_str!("consolidate_treasuries.rs");
    const INIT_POOL_SRC: &str = include_str!("init_pool.rs");

    #[test]
    fn the_administrator_is_the_only_signer_class() {
        let prod = production(SRC);
        assert_eq!(
            prod.matches(
                "constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized"
            )
            .count(),
            1
        );
        assert_eq!(prod.matches("operator").count(), 0);
        assert_eq!(accounts_body(SRC).matches("Signer<'info>").count(), 1);
    }

    /// The per-pool accounts are found by the pool's own seeds, and Treasury 03 must also be the
    /// one the pool names, so no other pool's or outside account can be emptied here.
    #[test]
    fn the_per_pool_accounts_are_the_pools_own() {
        let body = accounts_body(SRC);
        assert!(body.contains("seeds = [TREASURY_03_SEED, pool.key().as_ref()],"));
        assert!(body.contains("address = pool.treasury_03 @ ByeMachineError::TreasuryNotPerPool"));
        assert!(body.contains("seeds = [TREASURY_04_SEED, pool.key().as_ref()],"));
    }

    /// The shared accounts are declared exactly as `init_pool` declares them, so whichever
    /// instruction creates one, it is the same account with the same authority.
    #[test]
    fn the_shared_accounts_match_init_pools() {
        let init_pool = production(INIT_POOL_SRC);
        for seed in ["TREASURY_03_SEED", "TREASURY_04_SEED"] {
            let block = format!(
                "init_if_needed,\n        payer = administrator,\n        seeds = [{seed}],\n        \
                 bump,\n        token::mint = usdc_mint,\n        token::authority = protocol_config"
            );
            assert_eq!(production(SRC).matches(&block).count(), 1, "{seed} here");
            assert_eq!(init_pool.matches(&block).count(), 1, "{seed} in init_pool");
        }
    }

    /// The gate runs before any money moves.
    #[test]
    fn the_batch_gate_runs_before_any_transfer() {
        let prod = code(SRC);
        let handler = &prod[prod.find("pub fn handler(").unwrap()..];
        assert_eq!(calls(SRC, "assert_weight_open"), 1);
        assert!(
            handler.find("assert_weight_open(").unwrap()
                < handler.find("empty_and_close(").unwrap()
        );
    }

    /// Both per-pool accounts are emptied and closed, each signed by the pool and each closing to
    /// the Administrator. The transfer precedes the close, since a token account closes only empty.
    #[test]
    fn both_per_pool_accounts_are_emptied_then_closed_by_the_pool() {
        let prod = code(SRC);
        assert_eq!(calls(SRC, "empty_and_close"), 2);
        assert_eq!(calls(SRC, "transfer"), 1);
        assert_eq!(calls(SRC, "close_account"), 1);
        assert_eq!(
            prod.matches("authority: accounts.pool.to_account_info(),")
                .count(),
            2
        );
        assert!(prod.contains("destination: accounts.administrator.to_account_info(),"));
        assert!(prod.find("transfer(").unwrap() < prod.find("close_account(").unwrap());
    }

    /// The pool's two treasury fields are all it writes.
    #[test]
    fn writes_the_two_treasury_fields_and_nothing_else() {
        assert_eq!(
            field_writes(SRC, "pool"),
            vec!["treasury_03".to_string(), "treasury_04".to_string()]
        );
        assert_eq!(
            production(SRC)
                .matches("emit!(TreasuriesConsolidated")
                .count(),
            1
        );
    }
}
