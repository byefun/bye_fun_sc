//! Creates a wallet's escrow and its empty USDC vault, disarmed. The owner signs and pays the rent
//! for both. `init` refuses a second call, so a wallet has exactly one escrow.
//!
//! Never gated on a pause flag or a roll batch: an escrow holds the owner's own funds and is
//! outside every pool.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::common::events::EscrowOpened;
use crate::common::seeds::{ESCROW_SEED, ESCROW_VAULT_SEED, PROTOCOL_CONFIG_SEED};
use crate::state::{Escrow, ProtocolConfig};

pub fn handler(ctx: Context<OpenEscrow>) -> Result<()> {
    let clock = Clock::get()?;
    let owner = ctx.accounts.owner.key();

    let escrow = &mut ctx.accounts.escrow;
    escrow.owner = owner;
    escrow.armed = false;
    escrow.bump = ctx.bumps.escrow;
    escrow.vault_bump = ctx.bumps.escrow_vault;
    escrow.reserved = [0; 64];

    emit!(EscrowOpened {
        escrow: escrow.key(),
        slot: clock.slot,
        owner,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct OpenEscrow<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        init,
        payer = owner,
        space = 8 + Escrow::INIT_SPACE,
        seeds = [ESCROW_SEED, owner.key().as_ref()],
        bump
    )]
    pub escrow: Box<Account<'info, Escrow>>,

    /// The escrow's balance. Its only authority is the escrow PDA, so the only debits are the
    /// ones this program signs with the escrow's own seeds.
    #[account(
        init,
        payer = owner,
        seeds = [ESCROW_VAULT_SEED, escrow.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = escrow
    )]
    pub escrow_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = protocol_config.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, field_writes, production};

    const SRC: &str = include_str!("open_escrow.rs");

    #[test]
    fn the_owner_is_the_only_signer_and_pays_for_both_accounts() {
        let prod = production(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert_eq!(prod.matches("payer = owner").count(), 2);
        assert_eq!(prod.matches("init,").count(), 2);
    }

    #[test]
    fn the_escrow_is_seeded_on_the_signer_and_the_vault_on_the_escrow() {
        let body = accounts_body(SRC);
        assert!(body.contains("seeds = [ESCROW_SEED, owner.key().as_ref()]"));
        assert!(body.contains("seeds = [ESCROW_VAULT_SEED, escrow.key().as_ref()]"));
        assert!(body.contains("token::authority = escrow"));
        assert!(body.contains("token::mint = usdc_mint"));
        assert!(body.contains("#[account(address = protocol_config.usdc_mint)]"));
    }

    #[test]
    fn a_new_escrow_starts_disarmed_with_both_bumps_recorded() {
        let prod = production(SRC);
        assert!(prod.contains("escrow.armed = false;"));
        assert!(prod.contains("escrow.bump = ctx.bumps.escrow;"));
        assert!(prod.contains("escrow.vault_bump = ctx.bumps.escrow_vault;"));
        assert_eq!(
            field_writes(SRC, "escrow"),
            ["armed", "bump", "owner", "reserved", "vault_bump"]
        );
    }
}
