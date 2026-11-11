use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::ID as SYSVAR_INSTRUCTIONS_ID;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{
    close_account, transfer, CloseAccount, Mint, Token, TokenAccount, Transfer,
};
use mpl_token_metadata::ID as TOKEN_METADATA_ID;

use crate::common::errors::ByeMachineError;
use crate::common::escrow::{release_from_vault, PnftRelease};
use crate::common::events::NftClaimed;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED,
    TREASURY_02_SEED,
};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::instructions::exit::claim_fee::pay_closed_accrued;
use crate::state::{Pool, Position, PositionState, ProtocolConfig};

/// Releases the card of a Treasury 02 position closed below the floor, to the configured T02
/// authority, after paying the position's stored `accrued` to Treasury 03. `claim_nft`'s legs, called with the authority in place of the depositor: the
/// stored depositor is a program address with no key, so for this position it is the T02
/// authority's wallet that the custody invariant means by "its own depositor".
pub fn handler(ctx: Context<T02ClaimNft>) -> Result<()> {
    let pool = ctx.accounts.position.pool;
    let nft_mint = ctx.accounts.position.nft_mint;
    let position_seeds: &[&[u8]] = &[
        POSITION_SEED,
        pool.as_ref(),
        nft_mint.as_ref(),
        &[ctx.accounts.position.bump],
    ];
    let signer_seeds: &[&[&[u8]]] = &[position_seeds];

    // Paid before the account closes over it: once `close = t02_authority` runs, nothing records
    // the balance. A Treasury 02 position's settlements land in Treasury 03.
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

    release_from_vault(
        ctx.accounts.position.standard,
        &ctx.accounts.position_vault.to_account_info(),
        &ctx.accounts.position.to_account_info(),
        &ctx.accounts.authority_token,
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.nft_mint.to_account_info(),
        &ctx.accounts.metadata,
        &ctx.accounts.master_edition,
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.associated_token_program.to_account_info(),
        &ctx.accounts.token_metadata_program,
        PnftRelease {
            position_token_record: ctx.accounts.position_token_record.as_deref(),
            depositor_token_record: ctx.accounts.authority_token_record.as_deref(),
            sysvar_instructions: ctx.accounts.sysvar_instructions.as_deref(),
            authorization_rules_program: ctx.accounts.authorization_rules_program.as_deref(),
            authorization_rules: ctx.accounts.authorization_rules.as_deref(),
        },
        signer_seeds,
    )?;

    // The vault PDA is derived from the position, which the same seeds re-create on a
    // re-deposit — leaving an emptied token account behind would make that re-deposit
    // permanently unsatisfiable.
    close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        CloseAccount {
            account: ctx.accounts.position_vault.to_account_info(),
            destination: ctx.accounts.t02_authority.to_account_info(),
            authority: ctx.accounts.position.to_account_info(),
        },
        signer_seeds,
    ))?;

    emit!(NftClaimed {
        pool,
        slot: Clock::get()?.slot,
        position: ctx.accounts.position.key(),
        depositor: ctx.accounts.t02_identity.key(),
        nft_mint,
        fees_paid,
    });

    Ok(())
}

/// Signed by the T02 authority alone: the custody release of a Treasury 02 position
/// `record_value` already closed below the floor, paying first its stored `accrued`. No weight,
/// tier or counter surface — every one of those moved when the position was closed. Never pause-gated, and it never reaches the weight
/// freeze either: nothing here moves weight.
#[derive(Accounts)]
pub struct T02ClaimNft<'info> {
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
        // Token Metadata family only; on the accounts struct so it runs before any CPI, as in
        // the other exits.
        constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)
            @ ByeMachineError::WrongStandardForInstruction,
        // One terminal: `Seized` is written by `close_seized` alone, which requires
        // `standard == 2`, so a `Seized` arm here could never run.
        //
        // The Token Metadata family has no seizure terminal, which is a recorded gap: a Metaplex
        // rule-set revision that denies the exit (none of the live ones does) would strand a pNFT
        // with weight behind it, and nothing could clear it, since `close_seized` refuses the
        // standard and widening it needs a new release leg and account list. This needs a decision
        // before this family goes to mainnet.
        constraint = position.state == PositionState::ClosedBelowFloor
            @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        mut,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

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

    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub nft_mint: Box<Account<'info, Mint>>,

    /// CHECK: created as the T02 authority's associated token account by `TransferV1` when empty,
    /// and checked against the T02 authority by that same processor when it already exists.
    #[account(mut)]
    pub authority_token: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    pub master_edition: UncheckedAccount<'info>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1.
    #[account(mut)]
    pub position_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `position_token_record`.
    #[account(mut)]
    pub authority_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the Token Metadata program this pool's pNFTs are governed by.
    #[account(address = TOKEN_METADATA_ID)]
    pub token_metadata_program: UncheckedAccount<'info>,

    /// CHECK: optional, forwarded to `TransferV1`, which pins it to Token Auth Rules.
    pub authorization_rules_program: Option<UncheckedAccount<'info>>,

    /// CHECK: optional, forwarded to `TransferV1`, which asserts it matches the mint's rule set.
    pub authorization_rules: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the instructions sysvar `TransferV1` reads for its CPI guard. Optional on
    /// the same terms as the two token records — SPL `Transfer` has no CPI guard to read it.
    #[account(address = SYSVAR_INSTRUCTIONS_ID)]
    pub sysvar_instructions: Option<UncheckedAccount<'info>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, field_writes, flat, production};

    const SRC: &str = include_str!("t02_claim_nft.rs");

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
    fn the_card_goes_to_the_authoritys_token_account_through_the_standard_dispatch() {
        assert_eq!(calls(SRC, "release_from_vault"), 1);
        assert!(production(SRC).contains("&ctx.accounts.authority_token,"));
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
            prod.find("pay_closed_accrued(").unwrap() < prod.find("release_from_vault(").unwrap()
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
