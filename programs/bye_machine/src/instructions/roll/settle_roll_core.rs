//! One roll of a resolved batch, settled against an MPL Core card.
//!
//! **Same, mirrored.** Every guard, every arithmetic step, the fee split, the solvency check, the
//! ledger write and the events are `settle_roll`'s, called from here rather than copied: a second
//! derivation of what a settlement pays is not a style question, and a suite can pass while
//! getting it wrong. What differs is the transfer leg — MPL Core `TransferV1`, with the pNFT
//! account set absent — and one branch that has no counterpart in the Token Metadata family.
//!
//! **A blank drawn on this variant is handled by `settle_roll`, not here.** Blanks move no NFT,
//! so the twin exists only for the real-card leg. The operator predicts the drawn position to
//! build the account list at all, and a mis-prediction across the two variants fails on the
//! standard assert and costs one retry.
//!
//! **The branch with no Token Metadata counterpart: a card a third party has put beyond reach.**
//! MPL Core lets an authority other than this program burn an escrowed asset, transfer it out
//! under a permanent delegate, or freeze it in place. Token Metadata has no plugin system and no
//! such authority, which is why `settle_roll` runs no collateral test at all. Here the asset is
//! read before the transfer and before any payout, and a card that cannot move refunds the roll
//! and force-closes the phantom position in the same transaction, paying nobody. Without that
//! branch a resolved batch stalls on a roll no retry can clear, and every withdrawal, activation
//! and sweep in the pool stays frozen behind it.
//!
//! **The asset is an unchecked account, and that is load-bearing.** A burned asset's account is
//! closed; a typed `Account<AssetV1>` would fail deserialization *before* the handler runs,
//! turning a recoverable roll into a transaction that can never succeed — the exact stall the
//! branch exists to prevent, arriving one layer earlier.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};
use mpl_core::ID as MPL_CORE_ID;

use crate::common::core_asset::{
    classify_for_settlement, position_vault_address, release_core_from_vault, SettlementCollateral,
};
use crate::common::errors::ByeMachineError;
use crate::common::events::{
    BatchCompleted, PositionSeized, RebalanceEvaluated, RollRefunded, RollSettled, SeizureKind,
    TierChanged,
};
use crate::common::expansion::DrawTarget;
use crate::common::math::weight_of;
use crate::common::seeds::{
    BATCH_SEED, POOL_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED,
};
use crate::common::settlement_math::{split_protocol_share, FeeSplit};
use crate::common::standards::STANDARD_CORE;
use crate::common::tier;
use crate::instructions::roll::settle_roll::{
    apply_completion, apply_rebalance, apply_refund, apply_settle_ledger, assert_drawn_position,
    assert_expectation, assert_guards, assert_settlement_destination, assert_wallet_stats_binding,
    depart_drawn_position, draw_and_record, real_card_terminal, refund_code, roll_mut,
    seed_expectation, RollTerminal,
};
use crate::state::{
    Pool, Position, PositionState, ProtocolConfig, RollBatch, TopTier, WalletStats, WeightIndex,
};

/// The seizure this roll observed, carried from the read that authorised it to the writes and the
/// event that record it — so the cause published is the one that was actually read, not a second
/// reading taken later.
struct Seizure {
    kind: SeizureKind,
    owner_observed: Pubkey,
}

/// The phantom position's departure: everything `depart_drawn_position` does to the pool's
/// counters, and nothing it does to the position's claim.
///
/// The two differ in one place: a settled position is paid, so both accumulator legs are
/// discharged and `accrued` is cleared; a seized one is paid nothing, so the equal leg settles
/// onto `accrued` and stays owed.
///
/// The account and its vault survive: on a freeze the card is still escrowed and the depositor's
/// claim stays callable the moment the issuer thaws it, and deallocating here would strand a
/// thawed card in a PDA nothing can sign for.
fn seize_phantom_position(
    pool: &mut Pool,
    position: &mut Position,
    wallet_stats: &mut WalletStats,
) -> Result<(u32, u64, u64, bool)> {
    require!(
        position.state == PositionState::Active,
        ByeMachineError::InvalidPositionState
    );

    let was_member = position.in_tier;
    let recorded_value = position.recorded_value;
    let slot_index = position.slot_index;

    position.accrued = crate::common::math::position_fee_payout(
        pool.acc_equal,
        position.equal_checkpoint,
        position.accrued,
    )?;
    position.equal_checkpoint = pool.acc_equal;

    pool.w_real = pool
        .w_real
        .checked_sub(u128::from(weight_of(recorded_value)?))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    pool.n_real = pool
        .n_real
        .checked_sub(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    wallet_stats.active_positions = wallet_stats
        .active_positions
        .checked_sub(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    wallet_stats.active_value = wallet_stats
        .active_value
        .checked_sub(recorded_value)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    position.state = PositionState::Seized;

    let expected_total = WeightIndex::narrow_expected_total(pool.w_real)?;
    Ok((slot_index, recorded_value, expected_total, was_member))
}

/// Operator-signed, on exactly its principal's terms.
///
/// **No `seeds` on any position-bound slot, and every one of them optional**, for the two reasons
/// `settle_roll` carries: a roll over an emptied pool names no position, and Anchor resolves a
/// seed expression against the declared field — on an `Option` that field is the `Option`. The
/// handler derives and pins each of them.
#[derive(Accounts)]
pub struct SettleRollCore<'info> {
    /// `mut` because the Operator is this transaction's payer. The Core release allocates
    /// nothing, but `TransferV1` takes a payer and the account must be able to fund it.
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

    #[account(
        mut,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(
        mut,
        seeds = [BATCH_SEED, batch.pool.as_ref(), batch.batch_id.to_le_bytes().as_ref()],
        bump = batch.bump,
        constraint = batch.pool == pool.key() @ ByeMachineError::BatchPoolMismatch
    )]
    pub batch: Box<Account<'info, RollBatch>>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = roller_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = roller_usdc.owner == batch.roller @ ByeMachineError::Unauthorized
    )]
    pub roller_usdc: Box<Account<'info, TokenAccount>>,

    #[account(mut, address = pool.treasury_04)]
    pub treasury_04: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        address = protocol_config.protocol_revenue @ ByeMachineError::Unauthorized,
        constraint = protocol_revenue.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted
    )]
    pub protocol_revenue: Box<Account<'info, TokenAccount>>,

    /// The shared Treasury 03 — see `settle_roll`'s field of the same name; the twin
    /// carries the protocol split identically, because the split is fee arithmetic and knows
    /// nothing about which standard the drawn card is.
    #[account(mut, address = pool.treasury_03)]
    pub treasury_03: Box<Account<'info, TokenAccount>>,

    /// The protocol-wide Treasury 05, derived as the USDC ATA of
    /// `protocol_config.t05_authority` — see `settle_roll`'s field of the same name.
    #[account(
        mut,
        associated_token::mint = principal_vault.mint,
        associated_token::authority = protocol_config.t05_authority,
    )]
    pub treasury_05: Box<Account<'info, TokenAccount>>,

    #[account(mut, address = pool.weight_index)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    /// The drawn position. This twin serves the MPL Core family only, and the standard is
    /// asserted here rather than in the handler so it runs before any CPI.
    #[account(
        mut,
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction
    )]
    pub position: Option<Box<Account<'info, Position>>>,

    #[account(mut)]
    pub wallet_stats: Option<Box<Account<'info, WalletStats>>>,

    /// CHECK: pinned to `position.depositor` by the handler. Receives the position's rent on a
    /// settlement, and nothing at all on a seizure — a seized position keeps its account.
    #[account(mut)]
    pub depositor: Option<UncheckedAccount<'info>>,

    #[account(
        mut,
        constraint = settlement_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted
    )]
    pub settlement_usdc: Option<Box<Account<'info, TokenAccount>>>,

    /// CHECK: the escrow identity, and **not an account** — its only on-chain expression is
    /// `AssetV1.owner`. Pinned in the handler against the address derived from this position's
    /// own `vault_bump`: the classification reads whatever vault it is handed, so a decoy would
    /// read as a transfer-out and force-close a position whose card never moved.
    pub position_vault: Option<UncheckedAccount<'info>>,

    /// CHECK: **unchecked on purpose, and pinned in the handler to `position.nft_mint`.** A
    /// burned asset's account is closed and a typed account would fail deserialization before the
    /// handler runs, turning a recoverable roll into one that can never succeed. Writable because
    /// `TransferV1` rewrites `AssetV1.owner`.
    #[account(mut)]
    pub asset: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned by the classification to the collection the asset itself declares. Supplied
    /// because MPL Core needs it to run the collection's own plugins over the transfer, and
    /// because this pool's live freeze delegate sits in the collection's registry rather than the
    /// asset's.
    ///
    /// **Optional in the asset's sense as well as the roll's.** A roll over an emptied pool names
    /// no position and supplies none of these slots; and an asset declaring no collection has no
    /// registry to walk and cannot be transferred *with* a collection account, so the slot is
    /// omitted for it too. The classification decides which case it is and hands the release the
    /// account it resolved — this slot is never read past that point.
    #[account(mut)]
    pub collection: Option<UncheckedAccount<'info>>,

    /// CHECK: the card's destination owner, pinned to the batch's own roller.
    #[account(address = batch.roller @ ByeMachineError::Unauthorized)]
    pub roller: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, SettleRollCore<'info>>,
    roll_index: u8,
) -> Result<()> {
    let clock = Clock::get()?;
    let pool_key = ctx.accounts.pool.key();
    let batch_key = ctx.accounts.batch.key();
    let roller = ctx.accounts.batch.roller;
    let tier_len = ctx.accounts.top_tier.len;
    let tier_size = ctx.accounts.pool.tier_size;

    assert_guards(
        &ctx.accounts.batch,
        &ctx.accounts.pool,
        tier_len,
        roll_index,
    )?;

    let mut drawn_split: Option<FeeSplit> = None;
    let mut seizure: Option<Seizure> = None;
    // The release takes the account the classification resolved, not the slot: `None` on an asset
    // that declares no collection, which MPL Core refuses to transfer with one.
    let mut drawn_collection: Option<&AccountInfo<'info>> = None;
    let terminal = if ctx.accounts.pool.n_real == 0 {
        RollTerminal::Refunded {
            reason: refund_code(ByeMachineError::RealPoolEmpty)?,
        }
    } else {
        if roll_index == 0 {
            seed_expectation(&mut ctx.accounts.batch, &ctx.accounts.pool)?;
        } else {
            assert_expectation(&ctx.accounts.batch, &ctx.accounts.pool)?;
        }
        let target = draw_and_record(&mut ctx.accounts.batch, &ctx.accounts.pool, roll_index)?;
        let record = *roll_mut(&mut ctx.accounts.batch, roll_index)?;
        let split = crate::common::settlement_math::split_fee(
            ctx.accounts.batch.s_fee,
            ctx.accounts.batch.s_ticket,
            ctx.accounts.batch.s_alloc_bps,
            record.n_real_at_draw,
            record.blanks_at_draw,
            ctx.accounts.batch.s_blank_faces,
            tier_len,
        )?;

        match target {
            // Blanks move no NFT, so they settle through `settle_roll` on either variant.
            // Refusing here rather than duplicating that leg keeps one home for the swap, and
            // the caller knows the outcome before choosing an instruction.
            DrawTarget::Blank { .. } => return Err(ByeMachineError::BlankSettlesElsewhere.into()),
            DrawTarget::Real { key } => {
                let position = ctx
                    .accounts
                    .position
                    .as_deref()
                    .ok_or(ErrorCode::AccountNotEnoughKeys)?;
                assert_drawn_position(&ctx.accounts.weight_index, position, pool_key, key)?;

                let asset = ctx
                    .accounts
                    .asset
                    .as_ref()
                    .ok_or(ErrorCode::AccountNotEnoughKeys)?;
                require_keys_eq!(
                    asset.key(),
                    position.nft_mint,
                    ByeMachineError::StandardNotAdmitted
                );
                let vault = position_vault_address(&position.key(), position.vault_bump)?;
                require_keys_eq!(
                    ctx.accounts
                        .position_vault
                        .as_ref()
                        .ok_or(ErrorCode::AccountNotEnoughKeys)?
                        .key(),
                    vault,
                    ByeMachineError::NotPositionOwner
                );

                // Before the transfer and before any payout: a roll that pays the depositor and
                // only then discovers the card cannot move has already spent the vault's money on
                // a settlement it cannot complete.
                let classified =
                    classify_for_settlement(asset, ctx.accounts.collection.as_deref(), &vault)?;

                match classified {
                    SettlementCollateral::Settleable(collection) => {
                        drawn_collection = collection;
                        let terminal = real_card_terminal(
                            &ctx.accounts.pool,
                            &ctx.accounts.top_tier,
                            position,
                            ctx.accounts.principal_vault.amount,
                            &split,
                        )?;
                        drawn_split = Some(split);
                        terminal
                    }
                    SettlementCollateral::Seize {
                        kind,
                        owner_observed,
                        cause,
                    } => {
                        seizure = Some(Seizure {
                            kind,
                            owner_observed,
                        });
                        RollTerminal::Refunded {
                            reason: refund_code(cause)?,
                        }
                    }
                }
            }
        }
    };

    let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();
    let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];
    let pool_signer: &[&[&[u8]]] = &[pool_seeds];

    match terminal {
        RollTerminal::Settled { outcome, delta } => {
            let split = drawn_split.ok_or(ByeMachineError::ArithmeticFailure)?;
            // Each destination takes exactly its own share: the equal share credits Treasury
            // 04 in full however full it already is, and the protocol share is never inflated
            // by a redirected remainder. This is what keeps the published protocol take an
            // exact figure rather than a floor.
            let to_t04 = split.treasury_04;
            let to_protocol = split.protocol;

            apply_settle_ledger(
                &mut ctx.accounts.pool,
                &mut ctx.accounts.top_tier,
                &mut ctx.accounts.batch,
                roll_index,
                &split,
                outcome,
                delta,
            )?;

            let acc_tier = ctx.accounts.top_tier.acc_tier;
            let position_key = ctx.accounts.position.as_ref().unwrap().key();
            let nft_mint = ctx.accounts.position.as_ref().unwrap().nft_mint;
            let depositor = ctx.accounts.position.as_ref().unwrap().depositor;
            let vault_bump = ctx.accounts.position.as_ref().unwrap().vault_bump;

            assert_core_satellites(ctx.accounts, pool_key, depositor)?;
            roll_mut(&mut ctx.accounts.batch, roll_index)?.selected_mint = nft_mint;

            let departure = depart_drawn_position(
                &mut ctx.accounts.pool,
                ctx.accounts.position.as_mut().unwrap(),
                ctx.accounts
                    .wallet_stats
                    .as_mut()
                    .ok_or(ErrorCode::AccountNotEnoughKeys)?,
                acc_tier,
            )?;

            ctx.accounts.weight_index.load_mut()?.remove(
                departure.slot_index,
                departure.recorded_value,
                departure.expected_total,
            )?;

            let mut tier_change: Option<(Option<Pubkey>, Option<Pubkey>)> = None;
            if departure.was_member {
                tier::vacate(
                    &mut ctx.accounts.top_tier,
                    position_key,
                    ctx.accounts.position.as_mut().unwrap(),
                    acc_tier,
                )?;
                let entered = tier::fill_vacancy_from_candidate(
                    &mut ctx.accounts.top_tier,
                    tier_size,
                    ctx.remaining_accounts,
                    pool_key,
                    acc_tier,
                )?;
                tier_change = Some((Some(position_key), entered));
            }

            pay_protocol_share(ctx.accounts, to_protocol, pool_signer)?;
            if to_t04 > 0 {
                pay_from_vault(ctx.accounts, &ctx.accounts.treasury_04, to_t04, pool_signer)?;
            }

            // The depositor's side of the settlement: the balance this position accrued, ticket
            // and fee shares alike — not the card's recorded value, which the vault never posted
            // to this position as a lump sum.
            let payout = delta;
            let settlement_usdc = ctx
                .accounts
                .settlement_usdc
                .as_deref()
                .ok_or(ErrorCode::AccountNotEnoughKeys)?;
            assert_settlement_destination(
                settlement_usdc.key(),
                settlement_usdc.owner,
                depositor,
                pool_key,
                ctx.accounts.pool.treasury_03,
            )?;
            pay_from_vault(ctx.accounts, settlement_usdc, payout, pool_signer)?;

            release_drawn_asset(ctx.accounts, drawn_collection, position_key, vault_bump)?;

            // Last, and after the release that signs under this position's vault seeds. There is
            // no vault token account to close: a Core position's escrow is the PDA itself.
            let rent_to = ctx
                .accounts
                .depositor
                .as_ref()
                .ok_or(ErrorCode::AccountNotEnoughKeys)?
                .to_account_info();
            ctx.accounts.position.as_ref().unwrap().close(rent_to)?;

            let record = *roll_mut(&mut ctx.accounts.batch, roll_index)?;
            emit!(RollSettled {
                pool: pool_key,
                slot: clock.slot,
                batch: batch_key,
                roll_index,
                outcome,
                selected_mint: record.selected_mint,
                blank_face_idx: record.blank_face_idx,
                draw_value: record.draw_value,
                attempts: record.attempts,
                w_at_draw: record.w_at_draw,
                n_real_at_draw: record.n_real_at_draw,
                blanks_at_draw: record.blanks_at_draw,
                fee_equal: split.real_equal,
                fee_tier: split.tier,
                fee_protocol: to_protocol,
                fee_t04: to_t04,
            });

            if let Some((left, entered)) = tier_change {
                emit!(TierChanged {
                    pool: pool_key,
                    slot: clock.slot,
                    left,
                    entered,
                });
            }
        }
        // Unreachable: the draw arm above refuses a blank before any terminal is built, because
        // blanks move no NFT and settle through `settle_roll` on either variant. Stated as an
        // arm rather than a wildcard so a future terminal added to the enum reds here.
        RollTerminal::SettledBlank { .. } => {
            return Err(ByeMachineError::WrongStandardForInstruction.into())
        }
        RollTerminal::Refunded { reason } => {
            let refund = apply_refund(
                &mut ctx.accounts.pool,
                &mut ctx.accounts.batch,
                roll_index,
                reason,
            )?;
            pay_from_vault(ctx.accounts, &ctx.accounts.roller_usdc, refund, pool_signer)?;

            emit!(RollRefunded {
                pool: pool_key,
                slot: clock.slot,
                batch: batch_key,
                roll_index,
                reason,
                wallet: roller,
            });

            // The force-close, in the same transaction as the refund it accompanies. A refund
            // that left the phantom in place would leave its weight live and its leaf drawable,
            // and the next roll would select a card nobody can move all over again.
            if let Some(Seizure {
                kind,
                owner_observed,
            }) = seizure
            {
                let position_key = ctx.accounts.position.as_ref().unwrap().key();
                let depositor = ctx.accounts.position.as_ref().unwrap().depositor;
                assert_core_satellites(ctx.accounts, pool_key, depositor)?;

                let source_state = ctx.accounts.position.as_ref().unwrap().state;
                let (slot_index, recorded_value, expected_total, was_member) =
                    seize_phantom_position(
                        &mut ctx.accounts.pool,
                        ctx.accounts.position.as_mut().unwrap(),
                        ctx.accounts
                            .wallet_stats
                            .as_mut()
                            .ok_or(ErrorCode::AccountNotEnoughKeys)?,
                    )?;

                ctx.accounts.weight_index.load_mut()?.remove(
                    slot_index,
                    recorded_value,
                    expected_total,
                )?;

                if was_member {
                    let acc_tier = ctx.accounts.top_tier.acc_tier;
                    tier::vacate(
                        &mut ctx.accounts.top_tier,
                        position_key,
                        ctx.accounts.position.as_mut().unwrap(),
                        acc_tier,
                    )?;
                    // **The departure only — this does not fill the vacancy it opens.** The seat
                    // being handed out takes a share of every ticket, and choosing who gets it
                    // during someone else's forced closure is a choice made on nobody's behalf.
                    // `update_tier` exists for that promotion.
                    emit!(TierChanged {
                        pool: pool_key,
                        slot: clock.slot,
                        left: Some(position_key),
                        entered: None,
                    });
                }

                emit!(PositionSeized {
                    pool: pool_key,
                    slot: clock.slot,
                    position: position_key,
                    kind,
                    owner_observed,
                    source_state,
                    recorded_value,
                });
            }
        }
    }

    let blanks_before = apply_rebalance(&mut ctx.accounts.pool, &mut ctx.accounts.batch)?;
    emit!(RebalanceEvaluated {
        pool: pool_key,
        slot: clock.slot,
        n_real: ctx.accounts.pool.n_real,
        w_real: ctx.accounts.pool.w_real,
        blanks_before,
        blanks_after: ctx.accounts.pool.blanks,
    });

    if let Some(open_batches_remaining) =
        apply_completion(&mut ctx.accounts.pool, &mut ctx.accounts.batch)?
    {
        emit!(BatchCompleted {
            pool: pool_key,
            slot: clock.slot,
            batch: batch_key,
            open_batches_remaining,
        });
    }

    Ok(())
}

/// `WalletStats` and the depositor, bound to the drawn position. There is no vault token account
/// on this family — a Core position's escrow is a PDA with no allocation — so the vault half of
/// the principal's check is the address pin the handler already made before reading the asset.
fn assert_core_satellites(
    accounts: &SettleRollCore,
    pool_key: Pubkey,
    depositor: Pubkey,
) -> Result<()> {
    assert_wallet_stats_binding(
        accounts
            .wallet_stats
            .as_deref()
            .ok_or(ErrorCode::AccountNotEnoughKeys)?,
        accounts.wallet_stats.as_ref().unwrap().key(),
        pool_key,
        depositor,
    )?;
    require_keys_eq!(
        accounts
            .depositor
            .as_ref()
            .ok_or(ErrorCode::AccountNotEnoughKeys)?
            .key(),
        depositor,
        ByeMachineError::NotPositionOwner
    );
    Ok(())
}

/// The card, out of the vault PDA and into the roller's hands. No pNFT account set, no token
/// account, no associated-token creation: a Core asset's owner is a field on the asset itself.
///
/// **`collection` is the classification's answer, passed in rather than read off `accounts`** —
/// the account on an asset that still declares a collection and `None` on one that declares
/// none, which MPL Core refuses to transfer with a collection account. Taking it as a parameter
/// is what makes "the account the CPI uses is the account the gate checked" true by
/// construction: a handler that ignored the gate's answer would have to say so.
fn release_drawn_asset<'info>(
    accounts: &SettleRollCore<'info>,
    collection: Option<&AccountInfo<'info>>,
    position_key: Pubkey,
    vault_bump: u8,
) -> Result<()> {
    let missing = || error!(ErrorCode::AccountNotEnoughKeys);

    release_core_from_vault(
        accounts.asset.as_ref().ok_or_else(missing)?,
        collection,
        &accounts.roller.to_account_info(),
        &accounts
            .position_vault
            .as_ref()
            .ok_or_else(missing)?
            .to_account_info(),
        &accounts.operator.to_account_info(),
        &accounts.system_program.to_account_info(),
        &accounts.mpl_core_program,
        &position_key,
        vault_bump,
    )
}

/// The 20% protocol share, split 50/25/25 across Operations, the shared Treasury 03 and the
/// protocol-wide Treasury 05 — its principal's `pay_protocol_share`, over this twin's
/// accounts. The arithmetic is shared through `split_protocol_share`, so only the three account
/// references are restated here; the amounts cannot diverge between the twins.
fn pay_protocol_share<'info>(
    accounts: &SettleRollCore<'info>,
    protocol: u64,
    pool_signer: &[&[&[u8]]],
) -> Result<()> {
    let share = split_protocol_share(protocol)?;

    if share.operations > 0 {
        pay_from_vault(
            accounts,
            &accounts.protocol_revenue,
            share.operations,
            pool_signer,
        )?;
    }
    if share.treasury_03 > 0 {
        pay_from_vault(
            accounts,
            &accounts.treasury_03,
            share.treasury_03,
            pool_signer,
        )?;
    }
    if share.treasury_05 > 0 {
        pay_from_vault(
            accounts,
            &accounts.treasury_05,
            share.treasury_05,
            pool_signer,
        )?;
    }

    Ok(())
}

/// Every USDC leg leaves the principal vault under the pool's own authority, written once so the
/// destinations cannot drift apart on the authority or the signer seeds.
fn pay_from_vault<'info>(
    accounts: &SettleRollCore<'info>,
    destination: &Account<'info, TokenAccount>,
    amount: u64,
    pool_signer: &[&[&[u8]]],
) -> Result<()> {
    transfer(
        CpiContext::new_with_signer(
            accounts.token_program.to_account_info(),
            Transfer {
                from: accounts.principal_vault.to_account_info(),
                to: destination.to_account_info(),
                authority: accounts.pool.to_account_info(),
            },
            pool_signer,
        ),
        amount,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        ACC_PRECISION, DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };
    use crate::common::test_support::{accounts_body, calls, field_writes, flat, production};

    const SRC: &str = include_str!("settle_roll_core.rs");
    const SETTLE_ROLL_SRC: &str = include_str!("settle_roll.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn seizable_pool() -> Pool {
        let mut pool = Pool::blank();
        pool.admission_floor = DEFAULT_ADMISSION_FLOOR;
        pool.blank_faces = DEFAULT_BLANK_FACES;
        pool.ticket_target = DEFAULT_TICKET_TARGET;
        pool.n_real = 2;
        pool.w_real = 2 * u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        pool.open_batches = 1;
        pool
    }

    fn phantom() -> Position {
        Position {
            pool: Pubkey::default(),
            depositor: Pubkey::new_unique(),
            nft_mint: Pubkey::new_unique(),
            position_id: 0,
            state: PositionState::Active,
            recorded_value: DEFAULT_ADMISSION_FLOOR,
            value_observed_at: 0,
            deposit_value: 0,
            slot_index: 0,
            activated_at: 0,
            equal_checkpoint: 0,
            accrued: 0,
            in_tier: false,
            tier_checkpoint: 0,
            lock_until: 0,
            reject_reason: 0,
            standard: STANDARD_CORE,
            bump: 0,
            vault_bump: 0,
        }
    }

    fn phantom_stats() -> WalletStats {
        WalletStats {
            pool: Pubkey::default(),
            owner: Pubkey::default(),
            active_positions: 2,
            active_value: DEFAULT_ADMISSION_FLOOR * 2,
            bump: 0,
        }
    }

    // --- the force-close ----------------------------------------------------------------------

    /// The phantom's weight, count and wallet counters all leave, and the state moves to `Seized`.
    #[test]
    fn the_force_close_takes_the_weight_out_and_marks_the_position_seized() {
        let mut pool = seizable_pool();
        let mut position = phantom();
        let mut stats = phantom_stats();
        let w_before = pool.w_real;

        let (slot_index, recorded_value, _, was_member) =
            seize_phantom_position(&mut pool, &mut position, &mut stats).unwrap();

        assert_eq!(slot_index, 0);
        assert_eq!(recorded_value, DEFAULT_ADMISSION_FLOOR);
        assert!(!was_member);
        assert_eq!(pool.n_real, 1);
        assert_eq!(
            pool.w_real,
            w_before - u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap())
        );
        assert_eq!(stats.active_positions, 1);
        assert_eq!(stats.active_value, DEFAULT_ADMISSION_FLOOR);
        assert!(matches!(position.state, PositionState::Seized));
    }

    /// The one place the two departures differ, and what "paying nobody" means: a settled
    /// position's balance is discharged and `accrued` goes to zero; a seized one is paid nothing,
    /// so the same balance settles onto `accrued` and stays owed.
    #[test]
    fn a_seized_position_keeps_its_earned_balance_where_a_settled_one_is_paid_it() {
        let earned = 4 * ACC_PRECISION;

        let mut seized_pool = seizable_pool();
        seized_pool.acc_equal = earned;
        let mut seized = phantom();
        let mut seized_stats = phantom_stats();
        seize_phantom_position(&mut seized_pool, &mut seized, &mut seized_stats).unwrap();

        let mut settled_pool = seizable_pool();
        settled_pool.acc_equal = earned;
        let mut settled = phantom();
        let mut settled_stats = phantom_stats();
        depart_drawn_position(&mut settled_pool, &mut settled, &mut settled_stats, 0).unwrap();

        assert_eq!(seized.accrued, 4, "the seized balance stays owed");
        assert_eq!(settled.accrued, 0, "the settled balance was paid out");
        assert_eq!(
            seized.equal_checkpoint, settled.equal_checkpoint,
            "both stamp the checkpoint — the difference is where the balance went, not whether \
             the delta was consumed"
        );
    }

    /// `owed_fees` is untouched by the force-close: nothing was paid, so nothing is discharged,
    /// and the balance that moved onto `accrued` is still owed out of the same reserve.
    #[test]
    fn the_force_close_discharges_nothing_from_the_reserve() {
        let mut pool = seizable_pool();
        pool.owed_fees = 500;
        pool.acc_equal = 9 * ACC_PRECISION;
        let mut position = phantom();
        let mut stats = phantom_stats();

        seize_phantom_position(&mut pool, &mut position, &mut stats).unwrap();
        assert_eq!(pool.owed_fees, 500);
    }

    /// Only an `Active` position can be seized here — the drawn position is `Active` by
    /// construction, and a second seizure of an already-`Seized` one would decrement every
    /// counter twice and push a freed Fenwick slot onto the stack again.
    #[test]
    fn the_force_close_refuses_every_state_but_active_at_6301() {
        let all = crate::state::position::ALL_POSITION_STATES;
        let mut refused = 0;
        for (index, state) in all.into_iter().enumerate() {
            if matches!(state, PositionState::Active) {
                continue;
            }
            let mut pool = seizable_pool();
            let mut position = phantom();
            position.state = state;
            let mut stats = phantom_stats();
            let err = match seize_phantom_position(&mut pool, &mut position, &mut stats) {
                Ok(_) => panic!("ALL_POSITION_STATES[{index}] was seized and must not have been"),
                Err(err) => err,
            };
            assert_eq!(error_code(err), 6301, "ALL_POSITION_STATES[{index}]");
            refused += 1;
        }
        assert_eq!(refused, 4);
    }

    // --- the twin relationship ----------------------------------------------------------------

    /// Every accounting receiver is written identically by both twins: the arithmetic that
    /// decides what a settlement pays has one home, so neither handler writes `pool` or
    /// `position` directly.
    #[test]
    fn the_twin_writes_no_accounting_field_its_principal_does_not() {
        for receiver in ["ctx.accounts.pool", "ctx.accounts.top_tier"] {
            assert_eq!(
                field_writes(SRC, receiver),
                field_writes(SETTLE_ROLL_SRC, receiver),
                "{receiver} is written differently by the two twins"
            );
        }
    }

    /// Both shared steps are reached exactly once, and neither is re-derived locally. A twin that
    /// recomputed the split or the entitlement would be a second definition of the payout.
    #[test]
    fn the_shared_arithmetic_is_called_and_never_re_derived() {
        for shared in [
            "apply_settle_ledger",
            "apply_refund",
            "apply_rebalance",
            "apply_completion",
            "real_card_terminal",
            "depart_drawn_position",
        ] {
            assert_eq!(
                calls(SRC, shared),
                1,
                "{shared} is not reached exactly once"
            );
        }
        assert_eq!(
            calls(SRC, "position_delta"),
            0,
            "the entitlement is the principal's to compute — a local read is a second definition"
        );
        assert_eq!(
            production(SRC).matches("PrincipalShortfall").count(),
            0,
            "the balance check travels with the entitlement it consumes"
        );
    }

    // --- the collateral gate ------------------------------------------------------------------

    /// The gate runs before the transfer **and before any payout**. A roll that paid the
    /// depositor and only then discovered the card cannot move has already spent the vault's
    /// money on a settlement it cannot finish.
    #[test]
    fn the_collateral_gate_precedes_every_cpi_in_this_handler() {
        let prod = production(SRC);
        assert_eq!(calls(SRC, "classify_for_settlement"), 1);
        let gate = prod.find("classify_for_settlement(").unwrap();

        for cpi in [
            "release_core_from_vault(",
            "pay_from_vault(",
            "transfer(",
            "invoke(",
        ] {
            if let Some(at) = prod.find(cpi) {
                assert!(gate < at, "the collateral gate must precede {cpi}");
            }
        }
    }

    /// The vault the classification reads against is derived, never the account the caller named.
    /// A decoy would read as a transfer-out and force-close a position whose card never moved.
    #[test]
    fn the_classification_reads_against_a_derived_vault() {
        let prod = production(SRC);
        assert_eq!(calls(SRC, "position_vault_address"), 1);
        assert!(
            prod.find("position_vault_address(").unwrap()
                < prod.find("classify_for_settlement(").unwrap(),
            "the vault must be derived before it is classified against"
        );
        assert!(
            flat(SRC).contains("classify_for_settlement(asset,"),
            "the classification takes the asset account the handler pinned, not another"
        );
    }

    /// The collection the release uses is the one the classification resolved, and the slot is
    /// handed over optionally: an asset declaring no collection must reach the gate with the slot
    /// absent, and the release must be given what the gate returned (`None` on such an asset,
    /// which MPL Core refuses to transfer with a collection account).
    #[test]
    fn the_release_takes_the_collection_the_classification_resolved() {
        let prod = production(SRC);
        assert_eq!(
            prod.matches("ctx.accounts.collection").count(),
            1,
            "the slot is named once, where it is handed to the gate; the release takes what the \
             gate returned"
        );
        assert!(
            flat(SRC).contains(
                "classify_for_settlement(asset, ctx.accounts.collection.as_deref(), &vault)?;"
            ),
            "the optional slot goes to the gate as an Option — requiring it here makes a \
             collectionless asset unsettleable before it is ever classified"
        );
        assert!(
            flat(SRC).contains(
                "SettlementCollateral::Settleable(collection) => { drawn_collection = collection;"
            ),
            "the settleable arm must keep the account the classification resolved"
        );
        assert!(flat(SRC).contains(
            "release_drawn_asset(ctx.accounts, drawn_collection, position_key, vault_bump)?;"
        ));
        assert!(
            flat(SRC).contains(
                "fn release_drawn_asset<'info>( accounts: &SettleRollCore<'info>, collection: Option<&AccountInfo<'info>>,"
            ),
            "the release takes the resolved account as a parameter rather than reading the slot"
        );
    }

    /// The asset is pinned to the drawn position's own card. Without it the operator could
    /// present a settleable asset of their own and settle a position whose card has been seized.
    #[test]
    fn the_asset_is_pinned_to_the_drawn_positions_card() {
        assert!(flat(SRC).contains("require_keys_eq!( asset.key(), position.nft_mint,"));
    }

    /// **The asset must not be a typed account.** A burned asset's account is closed, and Anchor
    /// would fail its deserialization before the handler runs — turning the one roll this branch
    /// exists to recover into a transaction that can never succeed.
    #[test]
    fn the_asset_slot_is_unchecked() {
        let accounts = accounts_body(SRC);
        assert!(accounts.contains("pub asset: Option<UncheckedAccount<'info>>"));
        // Over the declarations only: the doc comments on this struct name `AssetV1.owner` on
        // purpose, to say what the escrow's on-chain expression is, and a raw count reads those
        // sentences as the typed account they are explaining the absence of.
        assert_eq!(
            accounts.matches("AssetV1").count(),
            0,
            "a typed asset account fails before the handler can classify it"
        );
    }

    /// This twin serves the Core family only, asserted on the accounts struct so it runs ahead of
    /// every CPI.
    #[test]
    fn the_standard_gate_runs_ahead_of_the_handler() {
        let accounts = accounts_body(SRC);
        assert!(accounts.contains("constraint = position.standard == STANDARD_CORE"));
        for foreign in ["STANDARD_PNFT", "STANDARD_LEGACY"] {
            assert_eq!(production(SRC).matches(foreign).count(), 0);
        }
    }

    /// **The force-close vacates a tier seat and does not fill it.** The seat takes a share of
    /// every ticket, and choosing who gets it during someone else's forced closure is a choice
    /// made on nobody's behalf — `update_tier` exists for that promotion. The settled arm does
    /// fill it, because there the depositor is being paid out rather than closed on.
    #[test]
    fn only_the_settled_arm_fills_the_tier_vacancy_it_opens() {
        assert_eq!(calls(SRC, "tier::vacate"), 2, "both terminals vacate");
        assert_eq!(
            calls(SRC, "tier::fill_vacancy_from_candidate"),
            1,
            "and only the settlement fills"
        );
    }

    /// The seizure publishes the cause it read, and the refund records it too — one condition,
    /// two records, and the same number in both.
    #[test]
    fn the_seizure_is_published_and_recorded_from_one_reading() {
        let prod = production(SRC);
        assert_eq!(prod.matches("emit!(PositionSeized").count(), 1);
        assert_eq!(
            calls(SRC, "refund_code"),
            2,
            "the empty pool, and the seizure"
        );
        assert!(
            prod.find("refund_code(cause)").unwrap() < prod.find("emit!(PositionSeized").unwrap()
        );
    }

    /// The instruction carries no weight-freeze guard, for its principal's reason: it runs only
    /// while a batch is open, which is the state the guard refuses.
    #[test]
    fn carries_no_weight_freeze_guard() {
        assert_eq!(calls(SRC, "assert_weight_open"), 0);
    }

    /// A seized position keeps its account: the card may be thawed later, and `claim_nft_core`
    /// needs `depositor` and `vault_bump` to still exist to sign with. Only the settled arm
    /// closes, and it closes by value rather than by attribute.
    #[test]
    fn only_the_settled_arm_closes_the_position_account() {
        let prod = production(SRC);
        assert_eq!(accounts_body(SRC).matches("close =").count(), 0);
        assert_eq!(prod.matches(".close(rent_to)").count(), 1);
        assert!(
            prod.find(".close(rent_to)").unwrap() < prod.find("emit!(PositionSeized").unwrap(),
            "the close belongs to the settled arm, which precedes the refund arm in this match"
        );
    }
}
