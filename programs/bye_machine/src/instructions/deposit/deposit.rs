use anchor_lang::error::ErrorCode;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_lang::solana_program::sysvar::instructions::ID as SYSVAR_INSTRUCTIONS_ID;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token, TokenAccount};
use mpl_token_metadata::ID as TOKEN_METADATA_ID;

use crate::common::errors::ByeMachineError;
use crate::common::escrow::{transfer_pnft, transfer_spl, validate_admission};
use crate::common::events::DepositPending;
use crate::common::seeds::{POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, WALLET_STATS_SEED};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::state::{Pool, PoolCollection, Position, PositionState, WalletStats};

pub fn handler(ctx: Context<Deposit>) -> Result<()> {
    require!(
        !ctx.accounts.pool.deposits_paused,
        ByeMachineError::DepositsPaused
    );

    let nft_mint = ctx.accounts.nft_mint.key();

    // The gate resolves the standard from the asset itself rather than assuming one.
    let standard = validate_admission(
        &ctx.accounts.nft_mint.to_account_info(),
        &ctx.accounts.metadata,
        &ctx.accounts.master_edition,
        &ctx.accounts.pool.key(),
        &ctx.accounts.pool_collection.key(),
        &ctx.accounts.pool_collection,
    )?;
    // The five pNFT accounts are **required-present** on standard 0 and **required-absent** on
    // standard 1 — the second half is the one a compile-time pin cannot see, and it is not
    // decorative: a legacy mint deposited with a token record supplied would otherwise leave the
    // account carried, unread, in a transaction whose shape says pNFT.
    //
    // Handler-side rather than `#[account(constraint = ..)]` because the condition is the standard
    // `validate_admission` has only just resolved, and account validation ran before it. Anchor's
    // own codes are reused: `AccountNotEnoughKeys` (3005) for an omission, `ConstraintRaw` (2003)
    // for a supplied account that must be absent.
    match standard {
        STANDARD_PNFT => {
            let depositor_token_record = ctx
                .accounts
                .depositor_token_record
                .as_ref()
                .ok_or(ErrorCode::AccountNotEnoughKeys)?;
            let position_token_record = ctx
                .accounts
                .position_token_record
                .as_ref()
                .ok_or(ErrorCode::AccountNotEnoughKeys)?;
            let sysvar_instructions = ctx
                .accounts
                .sysvar_instructions
                .as_ref()
                .ok_or(ErrorCode::AccountNotEnoughKeys)?;
            transfer_pnft(
                &ctx.accounts.depositor_token.to_account_info(),
                &ctx.accounts.depositor.to_account_info(),
                &ctx.accounts.position_vault.to_account_info(),
                &ctx.accounts.position.to_account_info(),
                &ctx.accounts.nft_mint.to_account_info(),
                &ctx.accounts.metadata,
                &ctx.accounts.master_edition,
                depositor_token_record,
                position_token_record,
                &ctx.accounts.depositor.to_account_info(),
                &ctx.accounts.depositor.to_account_info(),
                &ctx.accounts.system_program.to_account_info(),
                sysvar_instructions,
                &ctx.accounts.token_program.to_account_info(),
                &ctx.accounts.associated_token_program.to_account_info(),
                &ctx.accounts.token_metadata_program,
                ctx.accounts.authorization_rules_program.as_deref(),
                ctx.accounts.authorization_rules.as_deref(),
                &[],
            )?;
        }
        STANDARD_LEGACY => {
            require!(
                ctx.accounts.depositor_token_record.is_none(),
                ErrorCode::ConstraintRaw
            );
            require!(
                ctx.accounts.position_token_record.is_none(),
                ErrorCode::ConstraintRaw
            );
            require!(
                ctx.accounts.sysvar_instructions.is_none(),
                ErrorCode::ConstraintRaw
            );
            require!(
                ctx.accounts.authorization_rules_program.is_none(),
                ErrorCode::ConstraintRaw
            );
            require!(
                ctx.accounts.authorization_rules.is_none(),
                ErrorCode::ConstraintRaw
            );
            transfer_spl(
                &ctx.accounts.depositor_token.to_account_info(),
                &ctx.accounts.position_vault.to_account_info(),
                &ctx.accounts.depositor.to_account_info(),
                &ctx.accounts.token_program.to_account_info(),
                &[],
            )?;
        }
        // `standard` is a `u8`, so the arm is required by the match rather than chosen — the
        // closed set lives in `resolve_standard`, which is where a value outside {0, 1} is
        // actually refused. Unreachable today; it **rejects** rather than falling through,
        // because the alternative to a transfer here is a position opened over an escrow that
        // never happened. `standard == 2` reaches `deposit_core`, never this instruction.
        _ => return Err(ByeMachineError::WrongStandardForInstruction.into()),
    }

    let position_id = ctx.accounts.pool.position_counter;
    ctx.accounts.pool.position_counter = position_id
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    let pool_key = ctx.accounts.pool.key();
    let depositor = ctx.accounts.depositor.key();
    let position_key = ctx.accounts.position.key();
    let bump = ctx.bumps.position;
    let vault_bump = ctx.bumps.position_vault;

    let position = &mut ctx.accounts.position;
    position.pool = pool_key;
    position.depositor = depositor;
    position.nft_mint = nft_mint;
    position.position_id = position_id;
    position.state = PositionState::Pending;
    position.recorded_value = 0;
    position.value_observed_at = 0;
    position.deposit_value = 0;
    position.slot_index = 0;
    position.activated_at = 0;
    position.equal_checkpoint = 0;
    position.accrued = 0;
    position.in_tier = false;
    position.tier_checkpoint = 0;
    position.lock_until = 0;
    position.reject_reason = 0;
    position.standard = standard;
    position.bump = bump;
    position.vault_bump = vault_bump;

    let wallet_stats = &mut ctx.accounts.wallet_stats;
    wallet_stats.pool = pool_key;
    wallet_stats.owner = depositor;
    wallet_stats.bump = ctx.bumps.wallet_stats;

    emit!(DepositPending {
        pool: pool_key,
        slot: Clock::get()?.slot,
        position: position_key,
        depositor,
        nft_mint,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// The admission record for the collection the presented asset declares. Its address is
    /// re-derived in the handler from this pool's key and the collection the *asset's* metadata
    /// names, so neither another pool's record for this collection nor this pool's record for
    /// another collection is accepted.
    pub pool_collection: Box<Account<'info, PoolCollection>>,

    #[account(
        init,
        payer = depositor,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, pool.key().as_ref(), nft_mint.key().as_ref()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        init,
        payer = depositor,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump,
        token::mint = nft_mint,
        token::authority = position
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = depositor,
        space = 8 + WalletStats::INIT_SPACE,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), depositor.key().as_ref()],
        bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    pub nft_mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        constraint = depositor_token.mint == nft_mint.key() @ ByeMachineError::StandardNotAdmitted,
        constraint = depositor_token.owner == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub depositor_token: Box<Account<'info, TokenAccount>>,

    /// CHECK: owner, PDA derivation and contents are asserted by `validate_admission`.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: owner, PDA derivation and contents are asserted by `validate_admission`.
    pub master_edition: UncheckedAccount<'info>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1, which is a condition no account
    /// constraint can express — the standard is not resolved until the handler reads the
    /// metadata.
    #[account(mut)]
    pub depositor_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `depositor_token_record`.
    #[account(mut)]
    pub position_token_record: Option<UncheckedAccount<'info>>,

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
