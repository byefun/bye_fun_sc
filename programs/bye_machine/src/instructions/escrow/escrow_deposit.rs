//! Moves USDC from the owner's own token account into their escrow's vault.
//!
//! The program places no floor on the amount; the smallest deposit the product offers is enforced
//! before signing, off chain. Arming is left exactly as it was, and no pause flag or roll batch is
//! read: putting one's own money into one's own escrow changes no pool.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};

use crate::common::events::{EscrowDeposited, EscrowFundingSource};
use crate::common::seeds::{ESCROW_SEED, ESCROW_VAULT_SEED};
use crate::state::Escrow;

pub fn handler(ctx: Context<EscrowDeposit>, amount: u64) -> Result<()> {
    let clock = Clock::get()?;

    transfer(
        CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.owner_usdc.to_account_info(),
                to: ctx.accounts.escrow_vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        amount,
    )?;
    ctx.accounts.escrow_vault.reload()?;

    emit!(EscrowDeposited {
        escrow: ctx.accounts.escrow.key(),
        slot: clock.slot,
        owner: ctx.accounts.owner.key(),
        amount,
        source: EscrowFundingSource::Wallet,
        position: Pubkey::default(),
        position_pool: Pubkey::default(),
        balance_after: ctx.accounts.escrow_vault.amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct EscrowDeposit<'info> {
    pub owner: Signer<'info>,

    #[account(
        seeds = [ESCROW_SEED, owner.key().as_ref()],
        bump = escrow.bump
    )]
    pub escrow: Box<Account<'info, Escrow>>,

    #[account(
        mut,
        seeds = [ESCROW_VAULT_SEED, escrow.key().as_ref()],
        bump = escrow.vault_bump
    )]
    pub escrow_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = escrow_vault.mint,
        token::authority = owner
    )]
    pub owner_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, field_writes, production};

    const SRC: &str = include_str!("escrow_deposit.rs");

    #[test]
    fn the_owner_signs_and_both_escrow_accounts_are_derived_from_them() {
        let prod = production(SRC);
        let body = accounts_body(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(body.contains("seeds = [ESCROW_SEED, owner.key().as_ref()]"));
        assert!(body.contains("bump = escrow.bump"));
        assert!(body.contains("seeds = [ESCROW_VAULT_SEED, escrow.key().as_ref()]"));
        assert!(body.contains("bump = escrow.vault_bump"));
    }

    #[test]
    fn the_source_is_the_owners_own_usdc_account() {
        let body = accounts_body(SRC);
        assert!(body.contains("token::mint = escrow_vault.mint"));
        assert!(body.contains("token::authority = owner"));
    }

    #[test]
    fn one_transfer_into_the_vault_and_the_event_reads_the_vault_back() {
        let prod = production(SRC);
        assert_eq!(calls(SRC, "transfer"), 1);
        assert!(prod.contains("to: ctx.accounts.escrow_vault.to_account_info(),"));
        assert!(prod.contains("ctx.accounts.escrow_vault.reload()?;"));
        assert!(prod.contains("balance_after: ctx.accounts.escrow_vault.amount,"));
        assert!(prod.contains("source: EscrowFundingSource::Wallet,"));
    }

    #[test]
    fn places_no_floor_on_the_amount_and_never_touches_arming() {
        let prod = production(SRC);
        assert!(!prod.contains("require!"));
        assert!(field_writes(SRC, "escrow").is_empty());
    }
}
