//! Moves USDC from an escrow's vault back to its owner, signed by the escrow PDA.
//!
//! **The owner can always withdraw their whole balance.** The owner's signature and the amount
//! are the only things checked: no authority co-signs, and no pause flag, roll batch, lock or
//! cooldown is read. Nothing is ever reserved across transactions — a sale that pays from an
//! escrow commits and settles in one instruction — so the entire vault balance is the owner's to
//! take. Arming is left exactly as it was, even when the vault reaches zero.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};

use crate::common::errors::ByeMachineError;
use crate::common::events::EscrowWithdrawn;
use crate::common::seeds::{ESCROW_SEED, ESCROW_VAULT_SEED};
use crate::state::Escrow;

/// The vault balance a withdrawal of `amount` leaves, or the refusal. A refused withdrawal moves
/// nothing, because the transfer is never attempted.
pub(crate) fn withdrawal_balance_after(amount: u64, balance: u64) -> Result<u64> {
    require!(amount > 0, ByeMachineError::EscrowAmountZero);
    balance
        .checked_sub(amount)
        .ok_or_else(|| error!(ByeMachineError::EscrowBalanceShort))
}

pub fn handler(ctx: Context<EscrowWithdraw>, amount: u64) -> Result<()> {
    let clock = Clock::get()?;
    withdrawal_balance_after(amount, ctx.accounts.escrow_vault.amount)?;

    let owner = ctx.accounts.owner.key();
    let escrow_seeds: &[&[u8]] = &[ESCROW_SEED, owner.as_ref(), &[ctx.accounts.escrow.bump]];
    let escrow_signer: &[&[&[u8]]] = &[escrow_seeds];

    transfer(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.escrow_vault.to_account_info(),
                to: ctx.accounts.owner_usdc.to_account_info(),
                authority: ctx.accounts.escrow.to_account_info(),
            },
            escrow_signer,
        ),
        amount,
    )?;
    ctx.accounts.escrow_vault.reload()?;

    emit!(EscrowWithdrawn {
        escrow: ctx.accounts.escrow.key(),
        slot: clock.slot,
        owner,
        amount,
        balance_after: ctx.accounts.escrow_vault.amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct EscrowWithdraw<'info> {
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
    use super::*;
    use crate::common::test_support::{accounts_body, calls, field_writes, production};

    const SRC: &str = include_str!("escrow_withdraw.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_amount_is_refused_at_6800_whatever_the_balance() {
        for balance in [0, 1, u64::MAX] {
            let err = withdrawal_balance_after(0, balance).unwrap_err();
            assert_eq!(error_code(err), 6800);
        }
    }

    #[test]
    fn one_micro_usdc_above_the_balance_is_refused_at_6801() {
        let err = withdrawal_balance_after(10_000_001, 10_000_000).unwrap_err();
        assert_eq!(error_code(err), 6801);
        let err = withdrawal_balance_after(1, 0).unwrap_err();
        assert_eq!(error_code(err), 6801);
    }

    #[test]
    fn the_whole_balance_is_always_withdrawable_and_leaves_zero() {
        for balance in [1, 10_000_000, u64::MAX] {
            assert_eq!(withdrawal_balance_after(balance, balance).unwrap(), 0);
        }
    }

    #[test]
    fn a_partial_withdrawal_leaves_exactly_the_remainder() {
        assert_eq!(
            withdrawal_balance_after(7_000_000, 13_000_000).unwrap(),
            6_000_000
        );
    }

    #[test]
    fn the_amount_is_checked_against_the_vault_before_the_transfer() {
        let prod = production(SRC);
        let check = prod
            .find("withdrawal_balance_after(amount, ctx.accounts.escrow_vault.amount)?;")
            .expect("the handler no longer checks the amount against the vault");
        let cpi = prod.find("transfer(").expect("the transfer is gone");
        assert!(check < cpi, "the amount check must precede the transfer");
        assert_eq!(calls(SRC, "transfer"), 1);
    }

    #[test]
    fn the_escrow_pda_signs_and_the_owner_usdc_is_the_only_destination() {
        let prod = production(SRC);
        let body = accounts_body(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(prod.contains("authority: ctx.accounts.escrow.to_account_info(),"));
        assert!(prod.contains("to: ctx.accounts.owner_usdc.to_account_info(),"));
        assert!(prod.contains("&[ESCROW_SEED, owner.as_ref(), &[ctx.accounts.escrow.bump]]"));
        assert!(body.contains("seeds = [ESCROW_SEED, owner.key().as_ref()]"));
        assert!(body.contains("seeds = [ESCROW_VAULT_SEED, escrow.key().as_ref()]"));
        assert!(body.contains("token::authority = owner"));
    }

    #[test]
    fn the_event_reads_the_vault_back_and_arming_is_untouched() {
        let prod = production(SRC);
        assert!(prod.contains("ctx.accounts.escrow_vault.reload()?;"));
        assert!(prod.contains("balance_after: ctx.accounts.escrow_vault.amount,"));
        assert!(field_writes(SRC, "escrow").is_empty());
    }
}
