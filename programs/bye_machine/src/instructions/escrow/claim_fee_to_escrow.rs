//! `claim_fee`, paid into the depositor's escrow instead of their wallet.
//!
//! The settlement is `claim_fee`'s own `apply_claim_fee`, called unchanged: the same two legs, the
//! same single fold of `accrued`, the same `owed_fees` decrement, the same states paid (`Active`,
//! and a closed position's stored `accrued`). Only the destination differs — the vault of the escrow seeded on the signing depositor,
//! so the fee cannot land in anyone else's escrow. A Treasury 02 position never reaches this: its
//! depositor is a PDA with no key to sign.
//!
//! A fee claimed into the escrow has no minimum. A zero claim moves nothing and records no escrow
//! deposit, exactly as `claim_fee` skips its transfer at zero.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};

use crate::common::errors::ByeMachineError;
use crate::common::events::{EscrowDeposited, EscrowFundingSource, FeeClaimed};
use crate::common::seeds::{
    ESCROW_SEED, ESCROW_VAULT_SEED, POOL_SEED, POSITION_SEED, PRINCIPAL_VAULT_SEED, TOP_TIER_SEED,
};
use crate::instructions::exit::claim_fee::apply_claim_fee;
use crate::state::{Escrow, Pool, Position, TopTier};

pub fn handler(ctx: Context<ClaimFeeToEscrow>) -> Result<()> {
    let clock = Clock::get()?;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let depositor = ctx.accounts.depositor.key();
    let acc_tier = ctx.accounts.top_tier.acc_tier;

    let fees_paid = apply_claim_fee(&mut ctx.accounts.pool, &mut ctx.accounts.position, acc_tier)?;

    if fees_paid > 0 {
        let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();
        let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];
        let pool_signer: &[&[&[u8]]] = &[pool_seeds];

        transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.principal_vault.to_account_info(),
                    to: ctx.accounts.escrow_vault.to_account_info(),
                    authority: ctx.accounts.pool.to_account_info(),
                },
                pool_signer,
            ),
            fees_paid,
        )?;
        ctx.accounts.escrow_vault.reload()?;
    }

    emit!(FeeClaimed {
        pool: pool_key,
        slot: clock.slot,
        position: position_key,
        depositor,
        fees_paid,
    });

    if fees_paid > 0 {
        emit!(EscrowDeposited {
            escrow: ctx.accounts.escrow.key(),
            slot: clock.slot,
            owner: depositor,
            amount: fees_paid,
            source: EscrowFundingSource::Fees,
            position: position_key,
            position_pool: pool_key,
            balance_after: ctx.accounts.escrow_vault.amount,
        });
    }

    Ok(())
}

/// `ClaimFee`'s accounts, in its order, with `depositor_usdc` replaced by the depositor's escrow
/// and its vault.
#[derive(Accounts)]
pub struct ClaimFeeToEscrow<'info> {
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        seeds = [ESCROW_SEED, depositor.key().as_ref()],
        bump = escrow.bump
    )]
    pub escrow: Box<Account<'info, Escrow>>,

    #[account(
        mut,
        seeds = [ESCROW_VAULT_SEED, escrow.key().as_ref()],
        bump = escrow.vault_bump
    )]
    pub escrow_vault: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, field_writes, production, writes};

    const SRC: &str = include_str!("claim_fee_to_escrow.rs");
    const CLAIM_FEE_SRC: &str = include_str!("../exit/claim_fee.rs");

    #[test]
    fn the_depositor_is_the_only_signer_bound_to_the_positions_own_depositor() {
        let prod = production(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(prod.contains(
            "constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
    }

    #[test]
    fn settles_through_the_shared_claim_fee_accounting_and_writes_nothing_itself() {
        assert_eq!(calls(SRC, "apply_claim_fee"), 1);
        assert!(field_writes(SRC, "pool").is_empty());
        assert!(field_writes(SRC, "position").is_empty());
        assert!(field_writes(SRC, "escrow").is_empty());
        assert_eq!(writes(SRC, "position.state"), 0);
    }

    /// The fee can land only in the vault of the escrow seeded on the signer. A caller-chosen
    /// token account, or another wallet's escrow, fails the seeds.
    #[test]
    fn the_fee_goes_only_to_the_signers_own_escrow_vault() {
        let prod = production(SRC);
        let body = accounts_body(SRC);
        assert_eq!(calls(SRC, "transfer"), 1);
        assert!(prod.contains("to: ctx.accounts.escrow_vault.to_account_info(),"));
        assert!(body.contains("seeds = [ESCROW_SEED, depositor.key().as_ref()]"));
        assert!(body.contains("seeds = [ESCROW_VAULT_SEED, escrow.key().as_ref()]"));
        assert!(body.contains("bump = escrow.vault_bump"));
        assert!(!body.contains("depositor_usdc"));
    }

    #[test]
    fn a_zero_claim_moves_nothing_and_records_no_escrow_deposit() {
        let prod = production(SRC);
        assert_eq!(prod.matches("if fees_paid > 0 {").count(), 2);
        let second_gate = prod.rfind("if fees_paid > 0 {").unwrap();
        let deposited = prod.find("emit!(EscrowDeposited").unwrap();
        assert!(second_gate < deposited);
        assert!(prod.contains("source: EscrowFundingSource::Fees,"));
        assert!(prod.contains("amount: fees_paid,"));
        assert!(prod.contains("balance_after: ctx.accounts.escrow_vault.amount,"));
    }

    #[test]
    fn fee_claimed_is_emitted_before_escrow_deposited_and_unconditionally() {
        let prod = production(SRC);
        let claimed = prod.find("emit!(FeeClaimed").unwrap();
        let deposited = prod.find("emit!(EscrowDeposited").unwrap();
        assert!(claimed < deposited);
    }

    /// Every account `claim_fee` declares but its payout account appears here in the same
    /// order, so the two cannot drift into different settlement surfaces.
    #[test]
    fn mirrors_claim_fees_accounts_in_order_but_for_the_destination() {
        fn names(src: &str) -> Vec<String> {
            accounts_body(src)
                .lines()
                .filter_map(|l| l.trim().strip_prefix("pub "))
                .filter_map(|r| r.split_once(':'))
                .map(|(n, _)| n.trim().to_string())
                .collect()
        }
        let mut expected = names(CLAIM_FEE_SRC);
        let at = expected
            .iter()
            .position(|n| n == "depositor_usdc")
            .expect("claim_fee lost depositor_usdc");
        expected.splice(at..=at, ["escrow".to_string(), "escrow_vault".to_string()]);
        assert_eq!(names(SRC), expected);
    }

    #[test]
    fn reads_no_pause_flag_and_no_weight_guard() {
        let prod = production(SRC);
        assert_eq!(prod.matches("deposits_paused").count(), 0);
        assert_eq!(prod.matches("rolls_paused").count(), 0);
        assert_eq!(calls(SRC, "assert_weight_open"), 0);
    }
}
