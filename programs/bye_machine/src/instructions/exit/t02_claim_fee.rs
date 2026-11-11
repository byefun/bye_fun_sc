//! Pays a Treasury 02 position's accrued fee share to Treasury 03, signed by the configured T02
//! authority.
//!
//! The settlement is `claim_fee`'s own `apply_claim_fee`, called unchanged: the same two legs, the
//! same single fold of `accrued`, the same `owed_fees` decrement. Only the signer and the
//! destination differ. The position's depositor is the Treasury 02 identity PDA, which has no key
//! and so can never sign `claim_fee`; the fee is paid to Treasury 03 because that is where a
//! Treasury 02 position's settlements are routed, never to the authority's own wallet.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};

use crate::common::errors::ByeMachineError;
use crate::common::events::FeeClaimed;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED,
    TREASURY_02_SEED,
};
use crate::instructions::exit::claim_fee::apply_claim_fee;
use crate::state::{Pool, Position, ProtocolConfig, TopTier};

pub fn handler(ctx: Context<T02ClaimFee>) -> Result<()> {
    let clock = Clock::get()?;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let t02_identity = ctx.accounts.t02_identity.key();
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
                    to: ctx.accounts.treasury_03.to_account_info(),
                    authority: ctx.accounts.pool.to_account_info(),
                },
                pool_signer,
            ),
            fees_paid,
        )?;
    }

    emit!(FeeClaimed {
        pool: pool_key,
        slot: clock.slot,
        position: position_key,
        depositor: t02_identity,
        fees_paid,
    });

    Ok(())
}

/// Signed by the T02 authority alone, and carries no custody account: like `claim_fee`, it moves
/// no card, so one instruction serves all three token standards.
#[derive(Accounts)]
pub struct T02ClaimFee<'info> {
    pub t02_authority: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = t02_authority.key() == protocol_config.t02_authority @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// CHECK: the Treasury 02 identity PDA — an address, not an account. Declared so Anchor
    /// derives it rather than the handler trusting a caller-supplied key.
    #[account(seeds = [TREASURY_02_SEED, pool.key().as_ref()], bump)]
    pub t02_identity: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == t02_identity.key() @ ByeMachineError::NotATreasuryPosition
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

    /// Where the fee lands, pinned to the single address `Pool` records for Treasury 03.
    #[account(
        mut,
        address = pool.treasury_03,
        constraint = treasury_03.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted
    )]
    pub treasury_03: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, field_writes, production, writes};

    const SRC: &str = include_str!("t02_claim_fee.rs");

    #[test]
    fn the_t02_authority_is_the_only_signer_and_is_checked_against_config() {
        let prod = production(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(prod.contains(
            "constraint = t02_authority.key() == protocol_config.t02_authority @ ByeMachineError::Unauthorized"
        ));
    }

    #[test]
    fn the_position_must_be_owned_by_the_t02_identity() {
        assert!(production(SRC).contains(
            "constraint = position.depositor == t02_identity.key() @ ByeMachineError::NotATreasuryPosition"
        ));
    }

    #[test]
    fn the_fee_goes_to_treasury_03_and_never_to_a_caller_chosen_account() {
        let prod = production(SRC);
        assert_eq!(calls(SRC, "transfer"), 1);
        assert!(prod.contains("to: ctx.accounts.treasury_03.to_account_info(),"));
        assert!(prod.contains("address = pool.treasury_03"));
        assert!(prod.contains("if fees_paid > 0 {"));
        assert!(!accounts_body(SRC).contains("depositor_usdc"));
    }

    #[test]
    fn settles_through_the_shared_claim_fee_accounting_and_writes_nothing_itself() {
        assert_eq!(calls(SRC, "apply_claim_fee"), 1);
        assert!(field_writes(SRC, "pool").is_empty());
        assert!(field_writes(SRC, "position").is_empty());
        assert_eq!(writes(SRC, "position.state"), 0);
    }

    #[test]
    fn reads_no_pause_flag_and_no_weight_guard() {
        let prod = production(SRC);
        assert_eq!(prod.matches("deposits_paused").count(), 0);
        assert_eq!(prod.matches("rolls_paused").count(), 0);
        assert_eq!(calls(SRC, "assert_weight_open"), 0);
    }
}
