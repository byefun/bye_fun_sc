use anchor_lang::prelude::*;

/// A wallet's USDC escrow, one per owner and shared by every pool. Seeds: `[ESCROW_SEED, owner]`.
///
/// **It stores no balance.** The balance is the token amount of the vault at
/// `[ESCROW_VAULT_SEED, escrow]`, whose only authority is this PDA, so there is no second number
/// to drift from the first. USDC sent straight to the vault by a plain SPL transfer is spendable
/// balance like any other.
///
/// Created by its owner, disarmed, and never closed; the vault is never closed either. Field
/// order is the byte layout and is append-only — `reserved` exists so a later field does not
/// force a reallocation of every escrow.
#[account]
#[derive(InitSpace)]
pub struct Escrow {
    pub owner: Pubkey,
    pub armed: bool,
    pub bump: u8,
    pub vault_bump: u8,
    pub reserved: [u8; 64],
}
