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
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PROTOCOL_CONFIG_SEED, TREASURY_02_SEED,
    WALLET_STATS_SEED,
};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::state::{Pool, PoolCollection, Position, PositionState, ProtocolConfig, WalletStats};

/// The T02 authority's deposit: `deposit` for a card in the authority's own wallet, with the
/// Treasury 02 identity as the position's depositor. Nothing is paid and nothing about the intake
/// is skipped — the same pause flag, admission record, standard derivation and master-edition
/// guard. The position is `Pending` with no value on it, which is what tells it apart from one
/// `instant_sell` created; the Operator attests and activates it with `approve_deposit`, or
/// rejects it, and `return_rejected` sends a rejected card to the T02 authority.
pub fn handler(ctx: Context<T02Deposit>) -> Result<()> {
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
            let authority_token_record = ctx
                .accounts
                .authority_token_record
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
                &ctx.accounts.authority_token.to_account_info(),
                &ctx.accounts.t02_authority.to_account_info(),
                &ctx.accounts.position_vault.to_account_info(),
                &ctx.accounts.position.to_account_info(),
                &ctx.accounts.nft_mint.to_account_info(),
                &ctx.accounts.metadata,
                &ctx.accounts.master_edition,
                authority_token_record,
                position_token_record,
                &ctx.accounts.t02_authority.to_account_info(),
                &ctx.accounts.t02_authority.to_account_info(),
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
                ctx.accounts.authority_token_record.is_none(),
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
                &ctx.accounts.authority_token.to_account_info(),
                &ctx.accounts.position_vault.to_account_info(),
                &ctx.accounts.t02_authority.to_account_info(),
                &ctx.accounts.token_program.to_account_info(),
                &[],
            )?;
        }
        // The arm the `u8` match requires. The closed set lives in `resolve_standard`, which
        // refuses a value outside {0, 1}; this arm is unreachable and rejects rather than
        // falling through, since a position opened over an escrow that never happened is worse.
        // `standard == 2` reaches `t02_deposit_core`, never this instruction.
        _ => return Err(ByeMachineError::WrongStandardForInstruction.into()),
    }

    let position_id = ctx.accounts.pool.position_counter;
    ctx.accounts.pool.position_counter = position_id
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    let pool_key = ctx.accounts.pool.key();
    let t02_identity = ctx.accounts.t02_identity.key();
    let position_key = ctx.accounts.position.key();
    let bump = ctx.bumps.position;
    let vault_bump = ctx.bumps.position_vault;

    let position = &mut ctx.accounts.position;
    position.pool = pool_key;
    position.depositor = t02_identity;
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
    wallet_stats.owner = t02_identity;
    wallet_stats.bump = ctx.bumps.wallet_stats;

    // `standard` is read back off the position rather than from the local, so the event reports
    // the discriminant the account stores, which is what every downstream custody path reads.
    emit!(DepositPending {
        pool: pool_key,
        slot: Clock::get()?.slot,
        position: position_key,
        depositor: t02_identity,
        nft_mint,
        standard: ctx.accounts.position.standard,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct T02Deposit<'info> {
    #[account(mut)]
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

    /// CHECK: the Treasury 02 identity PDA — an address, not an account. It becomes
    /// `Position.depositor` and the owner of the wallet rollup.
    #[account(seeds = [TREASURY_02_SEED, pool.key().as_ref()], bump)]
    pub t02_identity: UncheckedAccount<'info>,

    /// The admission record for the collection the presented asset declares. Its address is
    /// re-derived in the handler from this pool's key and the collection the *asset's* metadata
    /// names, so neither another pool's record for this collection nor this pool's record for
    /// another collection is accepted.
    pub pool_collection: Box<Account<'info, PoolCollection>>,

    #[account(
        init,
        payer = t02_authority,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, pool.key().as_ref(), nft_mint.key().as_ref()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        init,
        payer = t02_authority,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump,
        token::mint = nft_mint,
        token::authority = position
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = t02_authority,
        space = 8 + WalletStats::INIT_SPACE,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), t02_identity.key().as_ref()],
        bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    pub nft_mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        constraint = authority_token.mint == nft_mint.key() @ ByeMachineError::StandardNotAdmitted,
        constraint = authority_token.owner == t02_authority.key() @ ByeMachineError::NotPositionOwner
    )]
    pub authority_token: Box<Account<'info, TokenAccount>>,

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
    pub authority_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `authority_token_record`.
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

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, production};

    const SRC: &str = include_str!("t02_deposit.rs");

    #[test]
    fn the_t02_authority_is_the_only_signer_and_is_checked_against_config() {
        let prod = production(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(prod.contains(
            "constraint = t02_authority.key() == protocol_config.t02_authority @ ByeMachineError::Unauthorized"
        ));
        assert_eq!(prod.matches("payer = t02_authority").count(), 3);
    }

    #[test]
    fn the_position_and_rollup_belong_to_the_treasury_identity_and_carry_no_value() {
        let prod = production(SRC);
        assert!(prod.contains("position.depositor = t02_identity;"));
        assert!(prod.contains("wallet_stats.owner = t02_identity;"));
        assert!(prod.contains("position.recorded_value = 0;"));
        assert!(prod.contains("position.state = PositionState::Pending;"));
    }

    #[test]
    fn nothing_is_skipped_and_nothing_is_paid() {
        let prod = production(SRC);
        assert!(prod.contains("!ctx.accounts.pool.deposits_paused"));
        assert_eq!(prod.matches("emit!(DepositPending").count(), 1);
        assert_eq!(calls(SRC, "validate_admission_limits"), 0);
        for absent in ["treasury_03", "usdc", "operator"] {
            assert!(
                !accounts_body(SRC).contains(absent),
                "unexpectedly names {absent}"
            );
        }
    }

    #[test]
    fn runs_the_depositors_admission_gate_once() {
        assert_eq!(calls(SRC, "validate_admission"), 1);
    }
}
