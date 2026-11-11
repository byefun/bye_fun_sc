use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};
use mpl_core::ID as MPL_CORE_ID;

use crate::common::core_asset::{release_core_from_vault, require_collateral_settleable};
use crate::common::errors::ByeMachineError;
use crate::common::events::NftClaimed;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED,
    TREASURY_02_SEED,
};
use crate::common::standards::STANDARD_CORE;
use crate::instructions::exit::claim_fee::pay_closed_accrued;
use crate::state::{Pool, Position, PositionState, ProtocolConfig};

/// `t02_claim_nft`'s MPL-Core twin.
///
/// **The one exit that must survive a seizure.** `ClosedBelowFloor` keeps its account and vault
/// until the claim, so a card seized after closure must still be claimable; `Seized` is
/// `close_seized`'s terminal and `close_seized` closes no account, so the claim is still owed
/// afterwards. Refusing either would refuse the call its sibling's `CollateralAbsent` message
/// sends the caller to make.
///
/// **Never gated, on any standard**: nothing may gate an exit here, and that is strongest where
/// the pool has already taken everything it will take from this position.
pub fn handler(ctx: Context<T02ClaimNftCore>) -> Result<()> {
    // Before the CPI, so a caller whose card is gone or frozen is told which, and to call
    // `close_seized`, instead of reading an MPL Core error number. The gate pins the collection
    // to the one the asset declares.
    let collection = require_collateral_settleable(
        &ctx.accounts.asset,
        &ctx.accounts.collection,
        &ctx.accounts.position_vault.key(),
    )?;

    let pool = ctx.accounts.position.pool;
    let nft_mint = ctx.accounts.position.nft_mint;
    let position_key = ctx.accounts.position.key();

    // Paid before the account closes over it: once `close = t02_authority` runs, nothing records
    // the balance. A `Seized` position is paid alike. A Treasury 02 position's settlements land in Treasury 03.
    let fees_paid = pay_closed_accrued(&mut ctx.accounts.pool, &mut ctx.accounts.position)?;
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

    release_core_from_vault(
        &ctx.accounts.asset,
        collection,
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.position_vault.to_account_info(),
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        &position_key,
        ctx.accounts.position.vault_bump,
    )?;

    emit!(NftClaimed {
        pool,
        slot: Clock::get()?.slot,
        position: position_key,
        depositor: ctx.accounts.t02_identity.key(),
        nft_mint,
        fees_paid,
    });

    Ok(())
}

/// Signed by the T02 authority alone: the custody release of a Treasury 02 position that is
/// already closed, paying first its stored `accrued` to Treasury 03. No weight, tier or counter
/// surface — every one of those moved when the position closed, whether `record_value` closed it
/// below the floor or `close_seized` seized it.
#[derive(Accounts)]
pub struct T02ClaimNftCore<'info> {
    #[account(mut)]
    pub t02_authority: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = t02_authority.key() == protocol_config.t02_authority @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    /// The position's pool: the Treasury 02 identity and the position are derived from it, so a
    /// position of another pool cannot pass as this pool's treasury position, and its `owed_fees`
    /// and principal vault are the ones the balance is discharged against and paid from.
    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// CHECK: the Treasury 02 identity PDA — an address, not an account.
    #[account(seeds = [TREASURY_02_SEED, pool.key().as_ref()], bump)]
    pub t02_identity: UncheckedAccount<'info>,

    #[account(
        mut,
        close = t02_authority,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == t02_identity.key() @ ByeMachineError::NotATreasuryPosition,
        // MPL Core only; on the accounts struct so it runs before any CPI.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction,
        // Both terminals are live: `Seized` is written only by `close_seized`, which requires
        // `standard == 2`, so this instruction alone serves every seized position.
        constraint = matches!(
            position.state,
            PositionState::ClosedBelowFloor | PositionState::Seized
        ) @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: the escrow identity, and **not an account** — its only on-chain expression is
    /// `AssetV1.owner`. Derived here so Anchor pins the address the collateral gate reads against
    /// and so the seeds the release signs under are this position's own. Never `init` and never
    /// `mut`: there is nothing to allocate and nothing to write.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    /// Where the balance lands, pinned to the single address `Pool` records for Treasury 03.
    #[account(
        mut,
        address = pool.treasury_03,
        constraint = treasury_03.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted
    )]
    pub treasury_03: Box<Account<'info, TokenAccount>>,

    /// CHECK: **the position's own card, and this pin is the one the collateral gate cannot
    /// make.** `read_collateral` reads whatever account it is handed, so without this address a
    /// caller could present a settleable asset of their own and claim against a position
    /// whose card is elsewhere. Writable because `TransferV1` rewrites `AssetV1.owner`.
    #[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: pinned in the handler to the collection the asset itself declares. Required because
    /// MPL Core needs the collection account present to run its plugins over the transfer.
    pub collection: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, field_writes, flat, production};

    const SRC: &str = include_str!("t02_claim_nft_core.rs");

    #[test]
    fn the_t02_authority_is_the_only_signer_and_is_checked_against_config() {
        let prod = production(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(prod.contains(
            "constraint = t02_authority.key() == protocol_config.t02_authority @ ByeMachineError::Unauthorized"
        ));
    }

    #[test]
    fn only_a_closed_treasury_position_of_this_pool_can_be_claimed_and_its_rent_goes_to_the_authority(
    ) {
        let prod = production(SRC);
        assert!(prod.contains(
            "constraint = position.depositor == t02_identity.key() @ ByeMachineError::NotATreasuryPosition"
        ));
        assert!(prod.contains("close = t02_authority"));
        assert!(prod
            .contains("seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()]"));
        assert!(prod.contains("PositionState::ClosedBelowFloor"));
    }

    #[test]
    fn reads_no_pause_flag_and_no_weight_guard() {
        let prod = production(SRC);
        assert_eq!(prod.matches("deposits_paused").count(), 0);
        assert_eq!(prod.matches("rolls_paused").count(), 0);
        assert_eq!(calls(SRC, "assert_weight_open"), 0);
    }

    #[test]
    fn both_closed_terminals_are_claimable_and_the_collateral_gate_precedes_the_release() {
        let prod = production(SRC);
        assert!(prod.contains("PositionState::ClosedBelowFloor | PositionState::Seized"));
        let gate = prod.find("require_collateral_settleable(").unwrap();
        let release = prod.find("release_core_from_vault(").unwrap();
        assert!(gate < release);
    }

    /// The stored `accrued` goes to the single Treasury 03 address `Pool` records, out of the
    /// pool's principal vault under the pool's signature, through the fee claims' shared function,
    /// before the card leaves. The pool is writable for `owed_fees` and seeds the vault, so the
    /// balance is discharged against and paid from the position's own pool.
    #[test]
    fn the_stored_accrual_is_paid_to_treasury_03_before_the_release() {
        let flat = flat(SRC);
        assert_eq!(calls(SRC, "pay_closed_accrued"), 1);
        assert!(flat.contains(
            "let fees_paid = pay_closed_accrued(&mut ctx.accounts.pool, &mut ctx.accounts.position)?;"
        ));
        assert!(flat.contains("if fees_paid > 0 {"));
        assert!(flat.contains(
            "Transfer { from: ctx.accounts.principal_vault.to_account_info(), \
             to: ctx.accounts.treasury_03.to_account_info(), \
             authority: ctx.accounts.pool.to_account_info(), }, pool_signer, ), fees_paid, )?;"
        ));
        assert_eq!(calls(SRC, "transfer"), 1);
        assert!(flat.contains("nft_mint, fees_paid, });"));
        let prod = production(SRC);
        assert!(
            prod.find("pay_closed_accrued(").unwrap()
                < prod.find("release_core_from_vault(").unwrap()
        );

        let accounts = accounts_body(SRC);
        assert!(accounts.contains(
            "mut,\n        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],\n        bump = pool.bump"
        ));
        assert!(accounts.contains("seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],"));
        assert!(accounts.contains(
            "mut,\n        address = pool.treasury_03,\n        constraint = treasury_03.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted"
        ));
        for receiver in [
            "pool",
            "position",
            "ctx.accounts.pool",
            "ctx.accounts.position",
        ] {
            assert!(
                field_writes(SRC, receiver).is_empty(),
                "{receiver} is written here rather than in pay_closed_accrued"
            );
        }
    }
}
