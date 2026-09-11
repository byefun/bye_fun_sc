use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::ID as SYSVAR_INSTRUCTIONS_ID;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{close_account, CloseAccount, Mint, Token, TokenAccount};
use mpl_token_metadata::ID as TOKEN_METADATA_ID;

use crate::common::errors::ByeMachineError;
use crate::common::escrow::{release_from_vault, PnftRelease};
use crate::common::events::NftReturned;
use crate::common::seeds::{POSITION_SEED, POSITION_VAULT_SEED};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::state::{Position, PositionState};

pub fn handler(ctx: Context<ReturnRejected>) -> Result<()> {
    let pool = ctx.accounts.position.pool;
    let nft_mint = ctx.accounts.position.nft_mint;
    let position_seeds: &[&[u8]] = &[
        POSITION_SEED,
        pool.as_ref(),
        nft_mint.as_ref(),
        &[ctx.accounts.position.bump],
    ];
    let signer_seeds: &[&[&[u8]]] = &[position_seeds];

    release_from_vault(
        ctx.accounts.position.standard,
        &ctx.accounts.position_vault.to_account_info(),
        &ctx.accounts.position.to_account_info(),
        &ctx.accounts.depositor_token,
        &ctx.accounts.depositor,
        &ctx.accounts.nft_mint.to_account_info(),
        &ctx.accounts.metadata,
        &ctx.accounts.master_edition,
        &ctx.accounts.payer.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.associated_token_program.to_account_info(),
        &ctx.accounts.token_metadata_program,
        PnftRelease {
            position_token_record: ctx.accounts.position_token_record.as_deref(),
            depositor_token_record: ctx.accounts.depositor_token_record.as_deref(),
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
            destination: ctx.accounts.depositor.to_account_info(),
            authority: ctx.accounts.position.to_account_info(),
        },
        signer_seeds,
    ))?;

    emit!(NftReturned {
        pool,
        slot: Clock::get()?.slot,
        position: ctx.accounts.position.key(),
        depositor: ctx.accounts.depositor.key(),
        nft_mint,
    });

    Ok(())
}

/// Permissionless: `payer` funds the transaction and any destination token account it creates,
/// but the NFT and both rent refunds go to the position's own recorded depositor.
#[derive(Accounts)]
pub struct ReturnRejected<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: pinned to the position's recorded depositor; receives the NFT and the rent.
    #[account(mut)]
    pub depositor: UncheckedAccount<'info>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // This instruction serves the Token Metadata family only. On the accounts struct rather
        // than in the handler so all three exits carry the guard in the same place and it runs
        // before any CPI — which is load-bearing in `withdraw`, where the fee payout moves USDC
        // before the card is touched, and uniform here so the three do not drift into two shapes.
        constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)
            @ ByeMachineError::WrongStandardForInstruction,
        constraint = position.state == PositionState::Rejected @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        mut,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub nft_mint: Box<Account<'info, Mint>>,

    /// CHECK: created as the depositor's associated token account by `TransferV1` when empty,
    /// and checked against `depositor` by that same processor when it already exists.
    #[account(mut)]
    pub depositor_token: UncheckedAccount<'info>,

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
    pub depositor_token_record: Option<UncheckedAccount<'info>>,

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
