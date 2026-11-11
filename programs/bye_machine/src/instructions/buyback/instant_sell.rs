use anchor_lang::error::ErrorCode;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::ID as SYSVAR_INSTRUCTIONS_ID;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token, TokenAccount};
use mpl_token_metadata::ID as TOKEN_METADATA_ID;

use crate::common::buyback::{
    pay_for_sale, record_acquisition, AcquisitionAccounts, AcquisitionTerms, LoaderAccounts,
    SaleAccounts,
};
use crate::common::errors::ByeMachineError;
use crate::common::escrow::{transfer_pnft, transfer_spl, validate_admission};
use crate::common::events::{InstantSold, RebalanceEvaluated, TierChanged};
use crate::common::guards::validate_admission_value;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED,
    TREASURY_02_SEED, WALLET_STATS_SEED,
};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::state::{
    Pool, PoolCollection, Position, ProtocolConfig, TopTier, WalletStats, WeightIndex,
};

/// The instant sell — one transaction, two signers.
///
/// The seller releases the card and the Operator attests its value. The sale then chooses who
/// pays, at execution, from the accounts' state at that moment: the loader's escrow when the
/// Operator names one that qualifies, otherwise Treasury 03. The seller receives `rate × V` either
/// way. On an escrow sale the vault pays `bid × V`, the spread goes to Treasury 03, and the card
/// becomes the loader's position; on a Treasury 03 sale the card becomes a position owned by the
/// T02 identity. Either way acceptance is a deposit as much as it is a purchase — which is why
/// `!deposits_paused` gates it: a pool that refuses depositors' cards must not acquire the same
/// cards through a sale instead.
///
/// **No `open_batches` guard.** Paying the seller and moving the card change no position's
/// weight; only entering the draw domain does. The sell therefore executes whenever its own
/// conditions hold, and the position it creates carries the admission rule instead —
/// `common/buyback.rs` owns that branch, and the payer selection.
///
/// **`standard ∈ {0, 1}` only.** `standard == 2` reaches `instant_sell_core`, whose sole
/// difference is the custody leg below; every other step is shared in `common/buyback.rs`.
pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, InstantSell<'info>>,
    value: u64,
    observed_at: i64,
) -> Result<()> {
    require!(
        !ctx.accounts.pool.deposits_paused,
        ByeMachineError::DepositsPaused
    );

    let nft_mint = ctx.accounts.nft_mint.key();

    // Open to any eligible-card holder, on exactly `deposit`'s gate: the standard is
    // resolved from the asset itself rather than taken from the caller.
    let standard = validate_admission(
        &ctx.accounts.nft_mint.to_account_info(),
        &ctx.accounts.metadata,
        &ctx.accounts.master_edition,
        &ctx.accounts.pool.key(),
        &ctx.accounts.pool_collection.key(),
        &ctx.accounts.pool_collection,
    )?;

    // The value limits, asserted here and not only at `approve_deposit`: checking only the floor
    // would let an acquisition admit a card an identical depositor submission is rejected for once
    // a ceiling is configured. The wallet cap is read inside `pay_for_sale`, against whichever
    // owner the position will belong to.
    validate_admission_value(&ctx.accounts.pool, value)?;

    let mut sale = pay_for_sale(
        SaleAccounts {
            pool: &ctx.accounts.pool,
            protocol_config: &ctx.accounts.protocol_config,
            treasury_03: &ctx.accounts.treasury_03,
            treasury_02_stats: &ctx.accounts.wallet_stats,
            seller_usdc: ctx.accounts.seller_usdc.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
            loader: LoaderAccounts {
                escrow: ctx.accounts.loader_escrow.as_deref(),
                escrow_vault: ctx.accounts.loader_escrow_vault.as_deref(),
                wallet_stats: ctx.accounts.loader_wallet_stats.as_deref(),
            },
        },
        value,
    )?;

    // Custody leg, the one half `instant_sell_core` replaces. The five pNFT accounts are
    // required-present on standard 0 and required-absent on standard 1, on `deposit`'s
    // reasoning: a legacy mint sold with a token record supplied would otherwise carry the
    // account unread. Anchor's own codes are reused: `AccountNotEnoughKeys` (3005) for an
    // omission, `ConstraintRaw` (2003) for a supplied account that must be absent.
    match standard {
        STANDARD_PNFT => {
            let seller_token_record = ctx
                .accounts
                .seller_token_record
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
                &ctx.accounts.seller_token.to_account_info(),
                &ctx.accounts.seller.to_account_info(),
                &ctx.accounts.position_vault.to_account_info(),
                &ctx.accounts.position.to_account_info(),
                &ctx.accounts.nft_mint.to_account_info(),
                &ctx.accounts.metadata,
                &ctx.accounts.master_edition,
                seller_token_record,
                position_token_record,
                &ctx.accounts.seller.to_account_info(),
                // The Operator pays, not the seller: this instruction's rent-bearing accounts are
                // the treasury's, and the seller is being paid rather than charged.
                &ctx.accounts.operator.to_account_info(),
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
                ctx.accounts.seller_token_record.is_none(),
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
                &ctx.accounts.seller_token.to_account_info(),
                &ctx.accounts.position_vault.to_account_info(),
                &ctx.accounts.seller.to_account_info(),
                &ctx.accounts.token_program.to_account_info(),
                &[],
            )?;
        }
        // Required by the match; the closed set lives in `resolve_standard`. It rejects rather
        // than falling through, since a position opened and paid for over no escrow is worse.
        _ => return Err(ByeMachineError::WrongStandardForInstruction.into()),
    }

    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let t02_identity = ctx.accounts.t02_identity.key();
    let seller = ctx.accounts.seller.key();

    let mut weight_index = ctx.accounts.weight_index.load_mut()?;
    let (depositor, rollup) = sale.acquirer(pool_key, t02_identity, &mut ctx.accounts.wallet_stats);
    let acquired = record_acquisition(
        AcquisitionAccounts {
            pool: &mut ctx.accounts.pool,
            weight_index: &mut weight_index,
            top_tier: &mut ctx.accounts.top_tier,
            position: &mut ctx.accounts.position,
            position_key,
            wallet_stats: rollup,
            remaining_accounts: ctx.remaining_accounts,
        },
        AcquisitionTerms {
            pool_key,
            depositor,
            nft_mint,
            standard,
            value,
            observed_at,
            now,
        },
    )?;
    drop(weight_index);
    sale.store_loader_rollup()?;

    ctx.accounts.wallet_stats.bump = ctx.bumps.wallet_stats;
    ctx.accounts.position.bump = ctx.bumps.position;
    ctx.accounts.position.vault_bump = ctx.bumps.position_vault;

    // `standard` is read back off the position rather than from the local, so the event reports
    // the discriminant the account stores.
    emit!(InstantSold {
        pool: pool_key,
        slot: clock.slot,
        position: position_key,
        seller,
        value,
        observed_at,
        observed_age_seconds: now
            .checked_sub(observed_at)
            .ok_or(ByeMachineError::ArithmeticFailure)?,
        rate_bps: sale.rate_bps,
        paid: sale.paid,
        standard: ctx.accounts.position.standard,
        payer: sale.payer,
        escrow_owner: sale.escrow_owner(),
        escrow_paid: sale.escrow_paid,
        bid_bps: sale.bid_bps,
    });

    // Both conditional on the immediate-activation path. When a batch is open the sell
    // creates a `Pending` position and emits neither — an unconditional `RebalanceEvaluated` here
    // would announce a recomputation that did not happen.
    if let Some(activation) = acquired.activation {
        emit!(RebalanceEvaluated {
            pool: pool_key,
            slot: clock.slot,
            n_real: ctx.accounts.pool.n_real,
            w_real: ctx.accounts.pool.w_real,
            blanks_before: activation.blanks_before,
            blanks_after: ctx.accounts.pool.blanks,
        });
        if activation.entered_tier {
            emit!(TierChanged {
                pool: pool_key,
                slot: clock.slot,
                left: activation.displaced,
                entered: Some(position_key),
            });
        }
    }

    Ok(())
}

/// **Two signers — the only instruction in the program with more than one.** Each authorises a
/// different leg and neither can stand for the other: the seller releases the card and the
/// Operator attests the value that decides admission and payout. The USDC needs no third
/// signature — the program signs Treasury 03's leg with the pool's seeds and the escrow's legs
/// with the `Escrow` PDA's. The crate-wide signer census in `instructions/mod.rs` carries the
/// expected count per instruction for exactly this reason.
#[derive(Accounts)]
pub struct InstantSell<'info> {
    /// Releases the card and receives the USDC. Not a payer — `operator` carries every rent-bearing
    /// account here, so the seller signs without funding anything.
    pub seller: Signer<'info>,

    #[account(mut)]
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

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

    /// CHECK: the T02 identity PDA — an address, not an account. It never holds a balance; its
    /// whole role is to be written into `Position.depositor` on a Treasury 03 sale. Declared here
    /// so Anchor derives it rather than the handler trusting a caller-supplied key.
    #[account(seeds = [TREASURY_02_SEED, pool.key().as_ref()], bump)]
    pub t02_identity: UncheckedAccount<'info>,

    #[account(
        init,
        payer = operator,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, pool.key().as_ref(), nft_mint.key().as_ref()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        init,
        payer = operator,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump,
        token::mint = nft_mint,
        token::authority = position
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    /// The T02 rollup. **Created here, rent paid by the Operator**, because the treasury PDA never
    /// calls `deposit` and so no other instruction creates it — and it is the account a Treasury
    /// 03 sale's wallet-cap check and every later Treasury 03 acquisition read. An escrow sale
    /// creates it when absent and leaves its counters alone.
    #[account(
        init_if_needed,
        payer = operator,
        space = 8 + WalletStats::INIT_SPACE,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), t02_identity.key().as_ref()],
        bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    #[account(mut, address = pool.weight_index)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    #[account(
        mut,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    /// The shared float, pinned to the address `Pool` records — `PDA(["t03"])`, which the
    /// `ProtocolConfig` PDA signs for.
    #[account(mut, address = pool.treasury_03)]
    pub treasury_03: Box<Account<'info, TokenAccount>>,

    /// Where the payout lands. Pinned to the seller and to Treasury 03's own mint, so neither a
    /// third party's account nor a same-owner account of some other mint can be substituted.
    #[account(
        mut,
        constraint = seller_usdc.mint == treasury_03.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = seller_usdc.owner == seller.key() @ ByeMachineError::NotPositionOwner
    )]
    pub seller_usdc: Box<Account<'info, TokenAccount>>,

    pub nft_mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        constraint = seller_token.mint == nft_mint.key() @ ByeMachineError::StandardNotAdmitted,
        constraint = seller_token.owner == seller.key() @ ByeMachineError::NotPositionOwner
    )]
    pub seller_token: Box<Account<'info, TokenAccount>>,

    /// CHECK: owner, PDA derivation and contents are asserted by `validate_admission`.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: owner, PDA derivation and contents are asserted by `validate_admission`.
    pub master_edition: UncheckedAccount<'info>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1, which is a condition no account constraint
    /// can express — the standard is not resolved until the handler reads the metadata.
    #[account(mut)]
    pub seller_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `seller_token_record`.
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

    /// CHECK: the loader's escrow, named by the Operator from the off-chain record of the card's
    /// latest draw. `bind_loader` asserts the owner program, the `Escrow` layout and the address
    /// `[ESCROW_SEED, escrow.owner]`; an account with no data means the loader never opened one.
    pub loader_escrow: Option<UncheckedAccount<'info>>,

    /// CHECK: bound in `bind_loader` to `[ESCROW_VAULT_SEED, loader_escrow]`. Debited only when
    /// the escrow pays, signed by the `Escrow` PDA.
    #[account(mut)]
    pub loader_escrow_vault: Option<UncheckedAccount<'info>>,

    /// CHECK: bound in `bind_loader` to `[WALLET_STATS_SEED, pool, escrow.owner]`. The rollup an
    /// escrow sale credits, written back by `store_loader_rollup`.
    #[account(mut)]
    pub loader_wallet_stats: Option<UncheckedAccount<'info>>,
}
