//! One roll of a resolved batch, driven to a terminal.
//!
//! Rolls settle strictly in index order, one transaction each, and the pool stays frozen for
//! every externally-triggered weight change until the last of them lands. That makes the
//! instruction's failure mode asymmetric: a roll that errors stays `Pending` and is retried, but
//! a roll that *cannot* succeed leaves the batch unfinishable and every gated action — every
//! withdrawal, activation and sweep in the pool — frozen behind it. So each guard here is stated
//! against a condition a retry can clear, and the completion write that reopens the pool lands in
//! the same transaction as the last roll rather than in a later operator step.
//!
//! Nothing observable happens before the final writes commit, so a retry after any error starts
//! from the same state the failed attempt did.
//!
//! **This module serves the Token Metadata custody family (`standard` 0 and 1).** A drawn MPL
//! Core card settles through `settle_roll_core`, which is this instruction with one transfer
//! leg swapped and a collateral gate ahead of it. What this module owns besides the Token
//! Metadata leg is everything both legs share: the guards, the draw and its persisted record,
//! the fee ledger, the blank payout, the refund leg, the rebalance, and batch completion.
//!
//! **Every position-specific account is optional, and that is what the protective refund needs.**
//! A roll over an emptied pool draws nothing, so there is no position for the caller to name; a
//! required position account would make the one terminal that exists for an empty pool
//! unreachable. The handler requires them where the draw does, and `AccountNotEnoughKeys` is what
//! a caller that predicted the wrong outcome gets.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::ID as SYSVAR_INSTRUCTIONS_ID;
use anchor_spl::associated_token::{
    create_idempotent, AssociatedToken, Create as AssociatedTokenCreate,
};
use anchor_spl::token::{
    close_account, transfer, CloseAccount, Mint, Token, TokenAccount, Transfer,
};
use mpl_token_metadata::ID as TOKEN_METADATA_ID;

use crate::common::constants::{BLANK_SLOTS, MAX_BATCH_CAPACITY};
use crate::common::damm_v2::{
    assert_immutable_identity, assert_swappable, sqrt_price, swap_exact_in, token_vaults, SwapCpi,
    CP_AMM_ID, EVENT_AUTHORITY_SEED, POOL_AUTHORITY,
};
use crate::common::errors::ByeMachineError;
use crate::common::escrow::{release_from_vault, PnftRelease};
use crate::common::events::{
    BatchCompleted, RebalanceEvaluated, RollRefunded, RollSettled, TierChanged,
};
use crate::common::expansion::{expand, map_draw, DrawTarget};
use crate::common::math::{aggregate_weight, weight_of};
use crate::common::rebalance;
use crate::common::seeds::{
    BATCH_SEED, POOL_SEED, POSITION_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED,
    TOP_TIER_SEED, TREASURY_02_SEED, WALLET_STATS_SEED,
};
use crate::common::settlement_math::{position_delta, split_fee, split_protocol_share, FeeSplit};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::common::swap_math::{blank_min_out, in_pair_order};
use crate::common::tier;
use crate::common::tier::required_tier_len;
use crate::state::{
    BatchState, Pool, Position, PositionState, ProtocolConfig, RollBatch, RollOutcome, RollStatus,
    TopTier, WalletStats, WeightIndex,
};

/// Where one roll ended. Every execution of this instruction reaches exactly one of these or
/// reverts — there is no third state a roll can be left in once the transaction commits, which is
/// what keeps a batch from stalling on a roll nobody can drive further.
/// No `Debug`, deriving only equality: it carries a wire enum, and those carry none by design.
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) enum RollTerminal {
    /// `delta` is what the settlement paid out of `owed_fees` to the drawn position; a blank pays
    /// no position, so it settles with `delta == 0`.
    ///
    /// `delta` is carried from before the accumulators are written: it is defined against an
    /// entitlement read *after* this roll's own accrual (the drawing roll counts as the
    /// position's last active roll), so `real_card_terminal` advances both accumulators as values
    /// and reads the entitlement off those. Reading the pool's un-bumped fields under-pays by one
    /// roll's share while the allocation equality still balances. The values cannot simply be
    /// written first, because the balance check that consumes `delta` gates every write.
    Settled { outcome: RollOutcome, delta: u64 },
    /// A drawn blank, and the face in micro-USDC it pays. Separate from `Settled` because the two
    /// legs share only the fee ledger: a blank moves no card, departs no position, touches
    /// neither the weight index nor the tier, and pays through a swap venue rather than out of
    /// the principal vault. Folding it into `Settled` would put every one of those steps behind
    /// a runtime test of the outcome it already carries.
    SettledBlank { face: u64 },
    /// `reason` is the error code of the condition that refused the settlement, so a reader maps
    /// a refunded roll back to its branch of the refund matrix without a second numbering.
    Refunded { reason: u16 },
}

/// An error code as the `u16` the roll record stores it in.
///
/// The record's refund reason and the error the caller saw are deliberately the same number: two
/// independent numberings for one condition is how a refund branch ends up recorded as a
/// different branch than the one that fired.
pub(crate) fn refund_code(err: ByeMachineError) -> Result<u16> {
    u16::try_from(u32::from(err)).map_err(|_| ByeMachineError::ArithmeticFailure.into())
}

/// Step 0 — this batch is the pool's settlement queue head, checked first of all: a batch sitting
/// behind the head is not merely "not resolved yet" or "out of turn", and letting either later
/// guard answer for this one would send an Operator hunting the wrong condition. Head-of-queue
/// gating keeps the Operator-signed `settle_roll` from settling two different batches out of
/// order, since `commit_rolls` admits a second commit while one is open.
///
/// Step 1 — the batch is resolved, this is the next roll in sequence, and the tier is full
/// against **live** config.
///
/// The tier is re-checked here rather than trusted from commitment because it is live state: the
/// gate `commit_rolls` passed can go stale before the batch finishes.
pub(crate) fn assert_guards(
    batch: &RollBatch,
    pool: &Pool,
    tier_len: u8,
    roll_index: u8,
) -> Result<()> {
    require_eq!(
        batch.batch_id,
        pool.head_batch_id()?,
        ByeMachineError::BatchNotHeadOfQueue
    );
    require!(
        batch.state == BatchState::Resolved,
        ByeMachineError::BatchNotResolved
    );
    require_eq!(roll_index, batch.next_roll, ByeMachineError::RollOutOfOrder);
    require!(roll_index < batch.n, ByeMachineError::RollOutOfOrder);
    require_eq!(
        tier_len,
        required_tier_len(pool.tier_size, pool.n_real),
        ByeMachineError::TierNotFull
    );
    Ok(())
}

/// Step 3a — the pool is where this batch's previous roll left it.
///
/// The expectation is a **running** value, not a commitment snapshot: this batch's own
/// `roll_index == 0` seeds it from live state (not by `vrf_callback`, which could
/// seed it while an earlier queued batch was still moving `W`) and each roll rewrites it from its
/// own rebalance, so the batch's own draws pass and a mutation from outside the batch fails.
/// Comparing against the value at commitment instead would fail every batch at its first roll.
pub(crate) fn assert_expectation(batch: &RollBatch, pool: &Pool) -> Result<()> {
    let live_w = aggregate_weight(pool.w_real, pool.blanks, pool.blank_faces)?;
    require_eq!(
        live_w,
        batch.expected_w,
        ByeMachineError::BatchExpectationMismatch
    );
    require!(
        pool.blanks == batch.expected_blanks,
        ByeMachineError::BatchExpectationMismatch
    );
    Ok(())
}

/// Step 3a (roll 0 only) — the expectation seeded from live state instead of asserted.
///
/// This batch has just reached the head of the queue: nothing outside it can move `W` again
/// until it reaches a terminal, so its first roll fixes the expectation here rather than
/// comparing against a value seeded earlier — possibly while a batch ahead of it in the queue
/// was still drawing. Rolls `1..n-1` keep asserting against the value the previous
/// roll's own rebalance wrote.
pub(crate) fn seed_expectation(batch: &mut RollBatch, pool: &Pool) -> Result<()> {
    batch.expected_w = aggregate_weight(pool.w_real, pool.blanks, pool.blank_faces)?;
    batch.expected_blanks = pool.blanks;
    Ok(())
}

/// Step 3b — the draw, and the pool snapshot it resolved against, persisted on the roll record.
///
/// The snapshot is written rather than recomputed at read time because it is not the snapshot at
/// commitment: the pipeline removes a position and rebalances between rolls, so each roll sees a
/// different pool and only the values stored here reconstruct its draw.
pub(crate) fn draw_and_record(
    batch: &mut RollBatch,
    pool: &Pool,
    roll_index: u8,
) -> Result<DrawTarget> {
    let w = aggregate_weight(pool.w_real, pool.blanks, pool.blank_faces)?;
    let draw = expand(&batch.randomness, roll_index, w)?;
    let target = map_draw(draw.r, pool.w_real, pool.blanks, pool.blank_faces)?;

    let record = roll_mut(batch, roll_index)?;
    record.draw_value = draw.r;
    record.attempts = draw.attempts;
    record.w_at_draw = w;
    record.n_real_at_draw = pool.n_real;
    record.blanks_at_draw = pool.blanks;

    Ok(target)
}

/// The roll record at `roll_index`, refused rather than indexed past the array's end. The guards
/// above already bound the index; this is what makes a corrupt `n` a returned error instead of a
/// panic inside the runtime.
pub(crate) fn roll_mut(
    batch: &mut RollBatch,
    roll_index: u8,
) -> Result<&mut crate::state::RollRecord> {
    let index = usize::from(roll_index);
    require!(
        index < MAX_BATCH_CAPACITY,
        ByeMachineError::RollCountOutOfBounds
    );
    batch
        .rolls
        .get_mut(index)
        .ok_or_else(|| ByeMachineError::RollCountOutOfBounds.into())
}

/// Step 6 — the ledger both outcome legs land on.
///
/// `fee_keep` stays in the vault against the accumulators this bumps; `fee_out` leaves it. `delta`
/// is the drawn position's entitlement, already paid by the caller's transfer, so it leaves the
/// owed-fee reserve on the same line that adds this roll's retention.
///
/// The four destinations are asserted to sum to the snapshotted ticket plus fee before anything
/// moves: a split that sums to less leaves the difference unattributed in the vault, and one that
/// sums to more spends money against claims that were never funded.
pub(crate) fn apply_settle_ledger(
    pool: &mut Pool,
    top_tier: &mut TopTier,
    batch: &mut RollBatch,
    roll_index: u8,
    split: &FeeSplit,
    outcome: RollOutcome,
    delta: u64,
) -> Result<()> {
    let allocated = split
        .real_equal
        .checked_add(split.tier)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_add(split.treasury_04)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_add(split.protocol)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let ticket_plus_fee = batch
        .s_ticket
        .checked_add(batch.s_fee)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    require_eq!(
        allocated,
        ticket_plus_fee,
        ByeMachineError::AllocationBpsNotFullyAllocated
    );

    pool.acc_equal = pool
        .acc_equal
        .checked_add(split.equal_unit)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    top_tier.acc_tier = top_tier
        .acc_tier
        .checked_add(split.tier_unit)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    // `owed_fees` tracks claims, not cash: this roll's retention creates claims and the drawn
    // position's payout discharges one, so both move here. Adding before subtracting is what
    // keeps the intermediate from underflowing when `delta` exceeds the prior reserve — the
    // drawing roll's own accrual is part of what funds it.
    pool.owed_fees = pool
        .owed_fees
        .checked_add(split.fee_keep()?)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_sub(delta)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    // The ticket stays in the principal vault — settling forfeits this roll's refund, so the
    // liability is released without a matching transfer out.
    pool.pending_roll_liability = pool
        .pending_roll_liability
        .checked_sub(batch.s_price)
        .ok_or(ByeMachineError::PrincipalShortfall)?;

    batch.settled = batch
        .settled
        .checked_add(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let record = roll_mut(batch, roll_index)?;
    record.status = RollStatus::Settled;
    record.outcome = outcome;
    record.refund_reason = 0;

    Ok(())
}

/// What the drawn position's departure leaves the handler to finish: the `WeightIndex` removal's
/// three verified arguments, and whether the position held a tier seat.
pub(crate) struct WeightDeparture {
    pub(crate) slot_index: u32,
    pub(crate) recorded_value: u64,
    pub(crate) expected_total: u64,
    pub(crate) was_member: bool,
}

/// The caller's prediction of which position the draw selected, checked against the tree that
/// made the selection.
///
/// The Fenwick tree stores weights and nothing else, so a drawn leaf names no account: the
/// operator has to predict the position to build the transaction at all, and this is what keeps
/// the prediction from being taken on trust. A live position owns its slot exclusively, so the
/// pool and the slot together identify exactly one.
pub(crate) fn assert_drawn_position(
    weight_index: &AccountLoader<WeightIndex>,
    position: &Position,
    pool_key: Pubkey,
    key: u64,
) -> Result<()> {
    // `key` is the draw value with the blank prefix already removed — the argument the range
    // search takes, never the raw draw value. The two are equal whenever the pool holds no
    // blanks, which is the state most fixtures start in, so passing the raw value instead is
    // wrong everywhere and fails nowhere obvious.
    let slot_index = weight_index
        .load()?
        .prefix_search(key)
        .ok_or(ByeMachineError::WeightDesync)?;

    require_keys_eq!(
        position.pool,
        pool_key,
        ByeMachineError::PositionPoolMismatch
    );
    require_eq!(
        position.slot_index,
        slot_index,
        ByeMachineError::DrawnPositionMismatch
    );
    Ok(())
}

/// The wallet whose counters a settlement decrements, bound to the drawn position's depositor.
///
/// **This is the binding an accounts-struct seed would normally carry.** Anchor resolves a seed
/// expression against the declared field, and on an `Option` that field is the `Option` — so
/// `position.depositor` does not compile there. Without this the caller chooses whose counters a
/// settlement takes from, and the stored bump makes the re-derivation a single hash.
///
/// Shared by both twins: the counters and their binding are identical across custody families, so
/// a second copy of this would be a second answer to whose stats a settlement moves.
pub(crate) fn assert_wallet_stats_binding(
    wallet_stats: &WalletStats,
    wallet_stats_key: Pubkey,
    pool_key: Pubkey,
    depositor: Pubkey,
) -> Result<()> {
    let derived = Pubkey::create_program_address(
        &[
            WALLET_STATS_SEED,
            pool_key.as_ref(),
            depositor.as_ref(),
            &[wallet_stats.bump],
        ],
        &crate::ID,
    )
    .map_err(|_| error!(ByeMachineError::NotPositionOwner))?;
    require_keys_eq!(wallet_stats_key, derived, ByeMachineError::NotPositionOwner);
    Ok(())
}

/// The payout a drawn blank owes, and whether Treasury 04 can cover it.
///
/// **The face is the batch's snapshot, and the assertion against live config is not dead code.**
/// Blank faces cannot change while the pool has an open batch, so the two agree by construction —
/// the payout reads the snapshot anyway, and this is what turns a future change to that freeze
/// into a refusal rather than into blanks paying a face nobody quoted.
///
/// **Treasury 04's balance is read including this roll's own credit.** The credit lands in the
/// same transaction and before the payout, so excluding it would refund settlements the treasury
/// can in fact cover — every one of them at the moment a pool's first blank is drawn, when the
/// standing balance is still zero.
pub(crate) fn blank_terminal(
    pool: &Pool,
    batch: &RollBatch,
    face_index: u8,
    t04_balance: u64,
    incoming_t04: u64,
) -> Result<RollTerminal> {
    let index = usize::from(face_index);
    let face = *batch
        .s_blank_faces
        .get(index)
        .ok_or(ByeMachineError::RollCountOutOfBounds)?;
    let live = *pool
        .blank_faces
        .get(index)
        .ok_or(ByeMachineError::RollCountOutOfBounds)?;
    require_eq!(face, live, ByeMachineError::BatchExpectationMismatch);
    require!(face > 0, ByeMachineError::BlankFaceZero);

    let available = t04_balance
        .checked_add(incoming_t04)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    if available < face {
        return Ok(RollTerminal::Refunded {
            reason: refund_code(ByeMachineError::T04Shortfall)?,
        });
    }

    Ok(RollTerminal::SettledBlank { face })
}

/// Step 5a — the drawn position's terminal, decided before a single write lands.
///
/// The payout is the position's own accrued balance, ticket and fee shares alike, every unit of
/// which the principal vault already retained against it, so the vault cannot be short of any
/// single position. The balance check is a defensive assertion on that accounting: it aborts the
/// settlement rather than refunding, since there is nothing to refund a card leg for.
///
/// Both accumulators are advanced as values here and the entitlement is read off those, not off
/// the pool's own fields (see `RollTerminal::Settled`).
pub(crate) fn real_card_terminal(
    pool: &Pool,
    top_tier: &TopTier,
    position: &Position,
    principal_balance: u64,
    split: &FeeSplit,
) -> Result<RollTerminal> {
    let acc_equal_after = pool
        .acc_equal
        .checked_add(split.equal_unit)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let acc_tier_after = top_tier
        .acc_tier
        .checked_add(split.tier_unit)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    let delta = position_delta(
        acc_equal_after,
        position.equal_checkpoint,
        acc_tier_after,
        position.tier_checkpoint,
        position.in_tier,
        position.accrued,
    )?;

    require!(
        principal_balance >= delta,
        ByeMachineError::PrincipalShortfall
    );

    Ok(RollTerminal::Settled {
        outcome: RollOutcome::RealCard,
        delta,
    })
}

/// Step 5b — the drawn position leaves the pool, its entitlement already funded.
///
/// Runs **after** `apply_settle_ledger`, so `pool.acc_equal` is the accumulator `Δ` was computed
/// against and stamping the checkpoint here discharges exactly what the caller transfers. Both
/// legs are discharged and `accrued` is cleared: this position is being paid, not seized.
///
/// `owed_fees` is deliberately untouched — the ledger already subtracted `Δ` from it on the same
/// line that added this roll's retention, and a second subtraction here would discharge one claim
/// twice and under-reserve every other position's.
///
/// No rebalance and no blank-set write: the handler evaluates once, outside every terminal, so
/// that a replay cannot tell a skipped evaluation from one that changed nothing.
pub(crate) fn depart_drawn_position(
    pool: &mut Pool,
    position: &mut Position,
    wallet_stats: &mut WalletStats,
    acc_tier: u128,
) -> Result<WeightDeparture> {
    // Active by construction — only active positions carry weight, and only weighted positions
    // are reachable through the Fenwick search. Asserted anyway so a later mechanism change
    // fails loudly instead of settling a position that never earned the right to be drawn.
    require!(
        position.state == PositionState::Active,
        ByeMachineError::InvalidPositionState
    );

    let was_member = position.in_tier;
    let recorded_value = position.recorded_value;
    let slot_index = position.slot_index;

    if was_member {
        position.tier_checkpoint = acc_tier;
    }
    position.equal_checkpoint = pool.acc_equal;
    position.accrued = 0;

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

    Ok(WeightDeparture {
        slot_index,
        recorded_value,
        expected_total: WeightIndex::narrow_expected_total(pool.w_real)?,
        was_member,
    })
}

/// Step 7 — the roll returned to its roller.
///
/// **No fee is split on a refunded roll**: the whole ticket price goes back, so neither
/// accumulator moves and `owed_fees` is untouched. A refund that split the fee first would keep
/// the protocol's share of a roll the roller never received.
pub(crate) fn apply_refund(
    pool: &mut Pool,
    batch: &mut RollBatch,
    roll_index: u8,
    reason: u16,
) -> Result<u64> {
    require!(reason != 0, ByeMachineError::ArithmeticFailure);

    let refund = batch.s_price;
    pool.pending_roll_liability = pool
        .pending_roll_liability
        .checked_sub(refund)
        .ok_or(ByeMachineError::PrincipalShortfall)?;

    batch.refunded = batch
        .refunded
        .checked_add(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let record = roll_mut(batch, roll_index)?;
    record.status = RollStatus::Refunded;
    record.outcome = RollOutcome::None;
    record.refund_reason = reason;

    Ok(refund)
}

/// Step 8 — the blank set re-evaluated, then the batch's expectation rewritten from it.
///
/// Runs on every terminal, refunds included, and the caller emits the evaluation even when the
/// set is unchanged: the evaluation is part of the batch's reproducible record, so an emission
/// conditional on a change would leave a gap a replay cannot distinguish from a skipped step.
///
/// The expectation rewrite is a **write of what this step just computed**, not a second
/// evaluation — recomputing it here is how the stored expectation and the live pool come to
/// disagree by construction.
pub(crate) fn apply_rebalance(
    pool: &mut Pool,
    batch: &mut RollBatch,
) -> Result<[u32; BLANK_SLOTS]> {
    let blanks_before = pool.blanks;

    pool.blanks = rebalance::evaluate(
        pool.n_real,
        pool.w_real,
        pool.blank_faces,
        pool.ticket_target,
    )?;

    batch.expected_w = aggregate_weight(pool.w_real, pool.blanks, pool.blank_faces)?;
    batch.expected_blanks = pool.blanks;

    Ok(blanks_before)
}

/// Step 9 — the roll counter advances, and the last roll completes the batch.
///
/// Returns the post-decrement `open_batches` when this roll completed the batch. The decrement is
/// what releases every action gated on an open batch, and it lands here rather than in a separate
/// operator instruction: splitting it out would put a second required step on the critical path
/// of a freeze that is already holding the whole pool.
pub(crate) fn apply_completion(pool: &mut Pool, batch: &mut RollBatch) -> Result<Option<u32>> {
    batch.next_roll = batch
        .next_roll
        .checked_add(1)
        .ok_or(ByeMachineError::RollCountOutOfBounds)?;

    if batch.next_roll < batch.n {
        return Ok(None);
    }

    batch.state = BatchState::Complete;
    pool.open_batches = pool
        .open_batches
        .checked_sub(1)
        .ok_or(ByeMachineError::RollBatchInFlight)?;

    Ok(Some(pool.open_batches))
}

/// Operator-signed. The instruction derives its own outcome and enforces its own ordering, so the
/// signature gates nothing the handler does not already check.
///
/// The roller's USDC account is the refund destination and is pinned to the batch's own roller: a
/// refund is owed to whoever paid for the batch, not to whoever submits the settlement.
#[derive(Accounts)]
pub struct SettleRoll<'info> {
    /// `mut` because the Operator is this transaction's payer: the roller's associated token
    /// account is created idempotently inside the settlement, and a missing ATA must not turn a
    /// settleable draw into a refund.
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

    /// The shared Treasury 04, pinned to the address `Pool` records. Its authority is the
    /// `ProtocolConfig` PDA, which signs the blank payout's swap.
    #[account(mut, address = pool.treasury_04)]
    pub treasury_04: Box<Account<'info, TokenAccount>>,

    /// The first consumer of `protocol_config.protocol_revenue`, which is stored as a bare
    /// `Pubkey` — so the mint is checked here rather than left to the transfer. Without it a
    /// revenue account configured on the wrong mint fails inside the CPI, which surfaces as a
    /// token-program error naming nothing about this program's configuration.
    #[account(
        mut,
        address = protocol_config.protocol_revenue @ ByeMachineError::Unauthorized,
        constraint = protocol_revenue.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted
    )]
    pub protocol_revenue: Box<Account<'info, TokenAccount>>,

    /// The shared Treasury 03. `address = pool.treasury_03` makes the destination a property of
    /// the pool being settled rather than an argument the Operator chooses.
    #[account(mut, address = pool.treasury_03)]
    pub treasury_03: Box<Account<'info, TokenAccount>>,

    /// The protocol-wide Treasury 05. It is the USDC associated token account of
    /// `protocol_config.t05_authority`, so the address is *derived* here rather than read from a
    /// stored field: owner and mint determine the one address, and there is no second config
    /// value that could disagree with it.
    ///
    /// **It must already exist.** Like `protocol_revenue`, this is a deployment-time account
    /// rather than one that arrives with a caller, so it is pinned and not created here — the
    /// idempotent creation this instruction does for a roller's $BYE account exists because a
    /// roller shows up unpredictably, which is not true of either fee destination. A missing
    /// Treasury 05 fails every settlement in every pool until someone creates the ATA; that is
    /// a condition a retry clears, and anyone may create it permissionlessly.
    #[account(
        mut,
        associated_token::mint = principal_vault.mint,
        associated_token::authority = protocol_config.t05_authority,
    )]
    pub treasury_05: Box<Account<'info, TokenAccount>>,

    #[account(mut, address = pool.weight_index)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    /// The drawn position. **No `seeds` here, and none on the five accounts bound to it below:**
    /// Anchor resolves a seed expression against the declared field, and on an `Option` that
    /// field is the `Option` itself, so `position.nft_mint` does not compile. The handler derives
    /// every one of them from the stored bumps instead, which is both the binding and the only
    /// place a reader has to look for it.
    ///
    /// This instruction serves the Token Metadata family only, and the standard is asserted here
    /// rather than in the handler so it runs **before any CPI** — a settlement moves USDC before
    /// it touches the card, so a handler-side check would come after money had already left.
    #[account(
        mut,
        constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)
            @ ByeMachineError::WrongStandardForInstruction
    )]
    pub position: Option<Box<Account<'info, Position>>>,

    #[account(mut)]
    pub wallet_stats: Option<Box<Account<'info, WalletStats>>>,

    /// CHECK: pinned to `position.depositor` by the handler. Receives the rent of the position
    /// and its vault, and never the settlement itself — a Treasury-02 position's proceeds route
    /// to Treasury 03 while its rent still returns to the account that paid it.
    #[account(mut)]
    pub depositor: Option<UncheckedAccount<'info>>,

    /// Where the drawn position's accrued balance lands: the depositor's own USDC account, or
    /// this pool's Treasury 03 float when the depositor is the Treasury-02 identity. The handler
    /// decides which, because the choice is a property of the position rather than of the caller.
    #[account(
        mut,
        constraint = settlement_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted
    )]
    pub settlement_usdc: Option<Box<Account<'info, TokenAccount>>>,

    #[account(mut)]
    pub position_vault: Option<Box<Account<'info, TokenAccount>>>,

    pub nft_mint: Option<Box<Account<'info, Mint>>>,

    /// CHECK: the card's destination owner, pinned to the batch's own roller — the card is owed
    /// to whoever paid for the batch, not to whoever submits the settlement.
    #[account(address = batch.roller @ ByeMachineError::Unauthorized)]
    pub roller: UncheckedAccount<'info>,

    /// CHECK: created as the roller's associated token account by `TransferV1` when empty, and
    /// checked against `roller` by that same processor when it already exists.
    #[account(mut)]
    pub roller_token: Option<UncheckedAccount<'info>>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    #[account(mut)]
    pub metadata: Option<UncheckedAccount<'info>>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    pub master_edition: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1.
    #[account(mut)]
    pub position_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `position_token_record`.
    #[account(mut)]
    pub roller_token_record: Option<UncheckedAccount<'info>>,

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

    /// CHECK: the pinned venue. Bound to the address `protocol_config` records, and its contents
    /// re-checked by the adapter on every settlement that reads it — the address cannot be
    /// substituted, but the account behind it can be rewritten by the venue at any time.
    #[account(mut, address = protocol_config.swap_pool @ ByeMachineError::SwapPoolInvalid)]
    pub swap_pool: Option<UncheckedAccount<'info>>,

    /// CHECK: the roller's $BYE associated token account, created idempotently by the handler so
    /// a missing account does not turn a settleable blank into a refund. The Operator pays its
    /// rent, which is why this instruction's signer is `mut`.
    #[account(mut)]
    pub roller_bye: Option<UncheckedAccount<'info>>,

    #[account(address = protocol_config.bye_mint @ ByeMachineError::SwapPoolInvalid)]
    pub bye_mint: Option<Box<Account<'info, Mint>>>,

    #[account(address = protocol_config.usdc_mint @ ByeMachineError::SwapPoolInvalid)]
    pub usdc_mint: Option<Box<Account<'info, Mint>>>,

    /// CHECK: pinned by the handler to the vault address the pool account itself records, which
    /// the adapter has already checked is that pool's own derived token vault.
    #[account(mut)]
    pub swap_token_a_vault: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned on the same terms as `swap_token_a_vault`.
    #[account(mut)]
    pub swap_token_b_vault: Option<UncheckedAccount<'info>>,

    /// CHECK: the venue's single global authority, the same address for every pool it hosts.
    #[account(address = POOL_AUTHORITY)]
    pub cp_amm_pool_authority: Option<UncheckedAccount<'info>>,

    /// CHECK: the venue's own event-authority PDA, derived and pinned by the handler.
    pub cp_amm_event_authority: Option<UncheckedAccount<'info>>,

    /// CHECK: the venue program. A compile-time constant rather than config — which pool is
    /// pinned is a deploy choice, which program owns it is not.
    #[account(address = CP_AMM_ID)]
    pub cp_amm_program: Option<UncheckedAccount<'info>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, SettleRoll<'info>>,
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

    // Step 2. A batch's own draws can take the pool's last real position, and the rolls after
    // that one have nothing priced to draw against. They are refunded rather than drawn, which is
    // why this sits ahead of the expectation check and the draw rather than inside them.
    let mut drawn_split: Option<FeeSplit> = None;
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
        let split = split_fee(
            ctx.accounts.batch.s_fee,
            ctx.accounts.batch.s_ticket,
            ctx.accounts.batch.s_alloc_bps,
            record.n_real_at_draw,
            record.blanks_at_draw,
            ctx.accounts.batch.s_blank_faces,
            tier_len,
        )?;

        match target {
            DrawTarget::Blank { face_index } => {
                // Treasury 04's cover is decided against the balance this roll's own credit
                // will leave it with, and that credit is the whole equal share. Nothing here
                // reads the treasury's ceiling: `sweep_t04_overflow` restores it out of band,
                // so a settlement can neither be re-priced nor redirected by a ceiling change.
                let terminal = blank_terminal(
                    &ctx.accounts.pool,
                    &ctx.accounts.batch,
                    face_index,
                    ctx.accounts.treasury_04.amount,
                    split.treasury_04,
                )?;
                roll_mut(&mut ctx.accounts.batch, roll_index)?.blank_face_idx = face_index;
                drawn_split = Some(split);
                terminal
            }
            DrawTarget::Real { key } => {
                let position = ctx
                    .accounts
                    .position
                    .as_deref()
                    .ok_or(ErrorCode::AccountNotEnoughKeys)?;
                assert_drawn_position(&ctx.accounts.weight_index, position, pool_key, key)?;

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
        }
    };

    let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();
    let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];
    let pool_signer: &[&[&[u8]]] = &[pool_seeds];

    match terminal {
        RollTerminal::Settled { outcome, delta } => {
            // Computed once, where the draw that priced it happened. Recomputing here from the
            // record would put two derivations of the same split in one transaction, and only
            // one of them is the one the settlement's own cover check passed against.
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

            // The accumulators have moved, so this is the value the drawn position's entitlement
            // was computed against and the one its tier checkpoint must be stamped at.
            let acc_tier = ctx.accounts.top_tier.acc_tier;
            let position_key = ctx
                .accounts
                .position
                .as_ref()
                .ok_or(ErrorCode::AccountNotEnoughKeys)?
                .key();
            let nft_mint = ctx.accounts.position.as_ref().unwrap().nft_mint;
            let depositor = ctx.accounts.position.as_ref().unwrap().depositor;
            let standard = ctx.accounts.position.as_ref().unwrap().standard;
            let position_bump = ctx.accounts.position.as_ref().unwrap().bump;

            assert_wallet_stats_binding(
                ctx.accounts
                    .wallet_stats
                    .as_deref()
                    .ok_or(ErrorCode::AccountNotEnoughKeys)?,
                ctx.accounts.wallet_stats.as_ref().unwrap().key(),
                pool_key,
                depositor,
            )?;
            // The escrow's authority is the position, so a substituted vault either fails here or
            // fails the transfer that reads the card out of it. Bound by ownership rather than by
            // re-derivation: the signing seeds for a position vault keep one home in this crate.
            require_keys_eq!(
                ctx.accounts
                    .position_vault
                    .as_deref()
                    .ok_or(ErrorCode::AccountNotEnoughKeys)?
                    .owner,
                position_key,
                ByeMachineError::NotPositionOwner
            );
            require_keys_eq!(
                ctx.accounts
                    .depositor
                    .as_ref()
                    .ok_or(ErrorCode::AccountNotEnoughKeys)?
                    .key(),
                depositor,
                ByeMachineError::NotPositionOwner
            );

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

            // The roller's side. Signed by the position, which is also the escrow's token owner
            // — the same authority `withdraw` releases under, and the reason the position account
            // may not be closed before this lands.
            let position_seeds: &[&[u8]] = &[
                POSITION_SEED,
                pool_key.as_ref(),
                nft_mint.as_ref(),
                &[position_bump],
            ];
            let position_signer: &[&[&[u8]]] = &[position_seeds];

            release_drawn_card(ctx.accounts, standard, position_signer)?;

            // The vault PDA is derived from the position, which the same seeds re-create on a
            // re-deposit — leaving an emptied token account behind would make that re-deposit
            // permanently unsatisfiable.
            close_account(CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                CloseAccount {
                    account: ctx
                        .accounts
                        .position_vault
                        .as_deref()
                        .ok_or(ErrorCode::AccountNotEnoughKeys)?
                        .to_account_info(),
                    destination: ctx
                        .accounts
                        .depositor
                        .as_ref()
                        .ok_or(ErrorCode::AccountNotEnoughKeys)?
                        .to_account_info(),
                    authority: ctx.accounts.position.as_ref().unwrap().to_account_info(),
                },
                position_signer,
            ))?;

            // Last, and after both CPIs that sign as this account: closing it zeroes the
            // discriminator, and a settlement that closed first would be signing as an account it
            // had already emptied.
            let rent_to = ctx.accounts.depositor.as_ref().unwrap().to_account_info();
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
        RollTerminal::SettledBlank { face } => {
            let split = drawn_split.ok_or(ByeMachineError::ArithmeticFailure)?;
            let to_t04 = split.treasury_04;
            let to_protocol = split.protocol;

            // The blank counter is deliberately not decremented: a drawn blank returns the pool
            // to the state it was in, so the virtual position stays and the next roll can draw
            // it again. Every other outcome removes what it drew.
            apply_settle_ledger(
                &mut ctx.accounts.pool,
                &mut ctx.accounts.top_tier,
                &mut ctx.accounts.batch,
                roll_index,
                &split,
                RollOutcome::Blank,
                0,
            )?;

            pay_protocol_share(ctx.accounts, to_protocol, pool_signer)?;
            if to_t04 > 0 {
                pay_from_vault(ctx.accounts, &ctx.accounts.treasury_04, to_t04, pool_signer)?;
            }

            // Last, and after the credit that may be what funds it: the treasury is paid in
            // before it is drawn from, so a pool settling its first blank is not short by
            // exactly this roll's own share.
            pay_blank_through_swap(ctx.accounts, face)?;

            let record = *roll_mut(&mut ctx.accounts.batch, roll_index)?;
            emit!(RollSettled {
                pool: pool_key,
                slot: clock.slot,
                batch: batch_key,
                roll_index,
                outcome: RollOutcome::Blank,
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

/// Where a settled position's accrued balance is owed. A Treasury-02 position settles on the
/// same rules as any other depositor; only the destination differs, because Treasury 02 holds
/// cards and never cash, so its proceeds route to this pool's Treasury 03 float. Keyed on the
/// position's depositor, never on anything the caller supplies.
pub(crate) fn assert_settlement_destination(
    settlement_usdc_key: Pubkey,
    settlement_usdc_owner: Pubkey,
    depositor: Pubkey,
    pool_key: Pubkey,
    treasury_03: Pubkey,
) -> Result<()> {
    let (t02_identity, _) =
        Pubkey::find_program_address(&[TREASURY_02_SEED, pool_key.as_ref()], &crate::ID);

    if depositor == t02_identity {
        require_keys_eq!(
            settlement_usdc_key,
            treasury_03,
            ByeMachineError::Unauthorized
        );
    } else {
        require_keys_eq!(
            settlement_usdc_owner,
            depositor,
            ByeMachineError::NotPositionOwner
        );
    }
    Ok(())
}

/// The drawn card, out of the position vault and into the roller's hands.
///
/// The five pNFT-only accounts are two-sided optional — required at standard 0 and required
/// absent at standard 1 — and `release_from_vault` is what enforces that, so the whole set is
/// forwarded rather than branched on here.
fn release_drawn_card<'info>(
    accounts: &SettleRoll<'info>,
    standard: u8,
    position_signer: &[&[&[u8]]],
) -> Result<()> {
    let missing = || error!(ErrorCode::AccountNotEnoughKeys);

    release_from_vault(
        standard,
        &accounts
            .position_vault
            .as_deref()
            .ok_or_else(missing)?
            .to_account_info(),
        &accounts
            .position
            .as_deref()
            .ok_or_else(missing)?
            .to_account_info(),
        accounts.roller_token.as_deref().ok_or_else(missing)?,
        &accounts.roller.to_account_info(),
        &accounts
            .nft_mint
            .as_deref()
            .ok_or_else(missing)?
            .to_account_info(),
        accounts.metadata.as_deref().ok_or_else(missing)?,
        accounts.master_edition.as_deref().ok_or_else(missing)?,
        &accounts.operator.to_account_info(),
        &accounts.system_program.to_account_info(),
        &accounts.token_program.to_account_info(),
        &accounts.associated_token_program.to_account_info(),
        &accounts.token_metadata_program,
        PnftRelease {
            position_token_record: accounts.position_token_record.as_deref(),
            depositor_token_record: accounts.roller_token_record.as_deref(),
            sysvar_instructions: accounts.sysvar_instructions.as_deref(),
            authorization_rules_program: accounts.authorization_rules_program.as_deref(),
            authorization_rules: accounts.authorization_rules.as_deref(),
        },
        position_signer,
    )
}

/// The blank payout: the drawn face, valued in $BYE at the venue's own price, swapped out of
/// Treasury 04 and delivered to the roller in this same transaction.
///
/// **The price is read here, in the instruction that swaps against it.** That excludes an
/// interleave between the read and the call; it does not exclude an attacker who moved the pool
/// beforehand, and the bound is what caps the damage from that.
///
/// **A swap that misses the bound aborts the transaction.** A failed inner instruction cannot be
/// caught, so this function has no short branch — the roll stays unsettled and the Operator
/// retries, which a later attempt can clear once the pool recovers.
fn pay_blank_through_swap<'info>(accounts: &SettleRoll<'info>, face: u64) -> Result<()> {
    let venue = accounts
        .swap_pool
        .as_ref()
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;
    let bye_mint = accounts
        .bye_mint
        .as_deref()
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;
    let usdc_mint = accounts
        .usdc_mint
        .as_deref()
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;
    let roller_bye = accounts
        .roller_bye
        .as_ref()
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;
    let vault_a = accounts
        .swap_token_a_vault
        .as_ref()
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;
    let vault_b = accounts
        .swap_token_b_vault
        .as_ref()
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;
    let pool_authority = accounts
        .cp_amm_pool_authority
        .as_ref()
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;
    let event_authority = accounts
        .cp_amm_event_authority
        .as_ref()
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;
    let cp_amm = accounts
        .cp_amm_program
        .as_ref()
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;

    let (ordering, min_out) = {
        // Scoped so the borrow is released before the venue mutates the account it reads from.
        let data = venue.try_borrow_data()?;
        let ordering = assert_immutable_identity(
            &venue.key(),
            venue.owner,
            &data,
            &bye_mint.key(),
            &usdc_mint.key(),
        )?;
        assert_swappable(&data, &Clock::get()?)?;

        let (recorded_a, recorded_b) = token_vaults(&data)?;
        require_keys_eq!(vault_a.key(), recorded_a, ByeMachineError::SwapPoolInvalid);
        require_keys_eq!(vault_b.key(), recorded_b, ByeMachineError::SwapPoolInvalid);

        (
            ordering,
            blank_min_out(
                face,
                sqrt_price(&data)?,
                ordering,
                accounts.protocol_config.max_swap_slippage_bps,
            )?,
        )
    };

    // The mint slots follow the pool's own ordering, not this protocol's naming: the vaults above
    // are pinned to the pool's recorded addresses, so a positional `bye`/`usdc` fill would pair
    // slot A's vault with the other side's mint on a USDC-first pool and the venue would reject.
    let (token_a_mint, token_b_mint) = in_pair_order(
        ordering,
        bye_mint.to_account_info(),
        usdc_mint.to_account_info(),
    );

    let (expected_event_authority, _) =
        Pubkey::find_program_address(&[EVENT_AUTHORITY_SEED], &CP_AMM_ID);
    require_keys_eq!(
        event_authority.key(),
        expected_event_authority,
        ByeMachineError::SwapPoolInvalid
    );

    // Idempotent: the roller may already hold a $BYE account, and a missing one must not turn a
    // settleable blank into a refund.
    create_idempotent(CpiContext::new(
        accounts.associated_token_program.to_account_info(),
        AssociatedTokenCreate {
            payer: accounts.operator.to_account_info(),
            associated_token: roller_bye.to_account_info(),
            authority: accounts.roller.to_account_info(),
            mint: bye_mint.to_account_info(),
            system_program: accounts.system_program.to_account_info(),
            token_program: accounts.token_program.to_account_info(),
        },
    ))?;

    // Treasury 04's authority is the `ProtocolConfig` PDA, so it is the swap's paying signer.
    let config_seeds: &[&[u8]] = &[PROTOCOL_CONFIG_SEED, &[accounts.protocol_config.bump]];
    let config_signer: &[&[&[u8]]] = &[config_seeds];

    swap_exact_in(
        &SwapCpi {
            pool_authority: pool_authority.to_account_info(),
            pool: venue.to_account_info(),
            input_token_account: accounts.treasury_04.to_account_info(),
            output_token_account: roller_bye.to_account_info(),
            token_a_vault: vault_a.to_account_info(),
            token_b_vault: vault_b.to_account_info(),
            token_a_mint,
            token_b_mint,
            payer: accounts.protocol_config.to_account_info(),
            token_a_program: accounts.token_program.to_account_info(),
            token_b_program: accounts.token_program.to_account_info(),
            event_authority: event_authority.to_account_info(),
            cp_amm_program: cp_amm.to_account_info(),
        },
        face,
        min_out,
        config_signer,
    )
}

/// The 20% protocol share, split 50/25/25 across Operations, the shared Treasury 03 and the
/// protocol-wide Treasury 05, and credited in this same instruction.
///
/// **Written once and used by both outcome arms**, because the two arms differ in everything
/// except this: a real-card settlement and a blank settlement compute the same `fee_protocol`
/// and owe it to the same three accounts, and a second copy of the routing is the shape that
/// lets one arm's destinations drift from the other's while both still sum correctly.
///
/// Each leg is skipped at zero rather than transferred — a zero-amount SPL transfer succeeds,
/// but it costs a CPI on an instruction whose budget is already the tightest in the program, and
/// a share of 0 to 3 micro-USDC makes two of the three zero.
fn pay_protocol_share<'info>(
    accounts: &SettleRoll<'info>,
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

/// Every USDC leg of this instruction leaves the principal vault under the pool's own authority.
/// Written once so the three destinations cannot drift apart on the authority or the signer
/// seeds, which is the difference between a transfer that fails and one that succeeds from the
/// wrong account.
fn pay_from_vault<'info>(
    accounts: &SettleRoll<'info>,
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
        ACC_PRECISION, DEFAULT_ADMISSION_FLOOR, DEFAULT_ALLOC_EQUAL_BPS,
        DEFAULT_ALLOC_PROTOCOL_BPS, DEFAULT_ALLOC_TIER_BPS, DEFAULT_BLANK_FACES, DEFAULT_FEE,
        DEFAULT_PRICE, DEFAULT_TICKET_TARGET,
    };
    use crate::common::math::weight_of;
    use crate::common::test_support::{accounts_body, calls, flat, production};
    use crate::state::RollRecord;

    const SRC: &str = include_str!("settle_roll.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    const UNDRAWN: RollRecord = RollRecord {
        status: RollStatus::Pending,
        outcome: RollOutcome::None,
        selected_mint: Pubkey::new_from_array([0u8; 32]),
        blank_face_idx: 0,
        draw_value: 0,
        attempts: 0,
        w_at_draw: 0,
        n_real_at_draw: 0,
        blanks_at_draw: [0; 8],
        refund_reason: 0,
    };

    /// A pool holding `n_real` floor-valued real cards and no blanks, so `W` is exactly the real
    /// weight and every test below can state the expectation in one term.
    fn pool_with(n_real: u32) -> Pool {
        let mut pool = Pool::blank();
        pool.price = DEFAULT_PRICE;
        pool.ticket_target = DEFAULT_TICKET_TARGET;
        pool.fee = DEFAULT_FEE;
        pool.alloc_equal_bps = DEFAULT_ALLOC_EQUAL_BPS;
        pool.alloc_tier_bps = DEFAULT_ALLOC_TIER_BPS;
        pool.alloc_protocol_bps = DEFAULT_ALLOC_PROTOCOL_BPS;
        pool.admission_floor = DEFAULT_ADMISSION_FLOOR;
        pool.blank_faces = DEFAULT_BLANK_FACES;
        pool.tier_size = 20;
        pool.max_n = 10;
        pool.n_real = n_real;
        pool.w_real = u128::from(n_real) * u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        pool.open_batches = 1;
        // `head_batch_id()` is `batch_counter - open_batches`, so this must track `open_batches`
        // for `resolved_batch`'s `batch_id: 0` to sit at the head — one committed batch, still
        // open, is exactly the queue depth `resolved_batch` assumes.
        pool.batch_counter = 1;
        pool
    }

    /// A batch mid-pipeline: resolved, `n` rolls, none settled, and its running expectation
    /// already agreeing with `pool` — the state every roll but the refusal cases starts from.
    /// `batch_id: 0` is the queue head against `pool_with`'s `batch_counter = 1, open_batches = 1`.
    fn resolved_batch(pool: &Pool, n: u8) -> RollBatch {
        RollBatch {
            pool: Pubkey::new_unique(),
            roller: Pubkey::new_unique(),
            batch_id: 0,
            state: BatchState::Resolved,
            n,
            next_roll: 0,
            settled: 0,
            refunded: 0,
            request_slot: 100,
            caller_seed: [7u8; 32],
            randomness: [42u8; 32],
            expected_w: aggregate_weight(pool.w_real, pool.blanks, pool.blank_faces).unwrap(),
            expected_blanks: pool.blanks,
            s_price: DEFAULT_PRICE,
            s_ticket: DEFAULT_TICKET_TARGET,
            s_fee: DEFAULT_FEE,
            s_alloc_bps: [
                DEFAULT_ALLOC_EQUAL_BPS,
                DEFAULT_ALLOC_TIER_BPS,
                DEFAULT_ALLOC_PROTOCOL_BPS,
            ],
            s_blank_faces: DEFAULT_BLANK_FACES,
            rolls: [UNDRAWN; MAX_BATCH_CAPACITY],
            bump: 255,
        }
    }

    fn tier() -> TopTier {
        TopTier {
            pool: Pubkey::default(),
            len: 0,
            entries: [crate::state::TierEntry {
                position: Pubkey::default(),
                value: 0,
                activated_at: 0,
                position_id: 0,
            }; 20],
            acc_tier: 0,
            bump: 255,
        }
    }

    // --- Step 1: the guards, from both directions ---------------------------------------------

    #[test]
    fn admits_the_next_roll_of_a_resolved_batch_with_a_full_tier() {
        let pool = pool_with(3);
        let batch = resolved_batch(&pool, 5);
        assert!(assert_guards(&batch, &pool, 3, 0).is_ok());
    }

    // --- Step 0: head-of-queue gating -------------------------------------------

    /// A batch queued behind the head is refused before every other guard — in particular ahead
    /// of `BatchNotResolved`, so a `Resolved` batch sitting second in the queue is never reported
    /// as unresolved.
    #[test]
    fn rejects_a_batch_that_is_not_yet_the_queue_head_at_6215() {
        let mut pool = pool_with(3);
        pool.open_batches = 2;
        pool.batch_counter = 2;
        let mut batch = resolved_batch(&pool, 5);
        batch.batch_id = 1; // committed second; batch 0 is still ahead of it

        assert_eq!(
            error_code(assert_guards(&batch, &pool, 3, 0).unwrap_err()),
            6215
        );
    }

    /// The gate does not misfire on the ordinary case: a single open batch at `batch_id == 0` is
    /// its own queue's head.
    #[test]
    fn admits_the_only_open_batch_as_the_queue_head() {
        let pool = pool_with(3);
        let batch = resolved_batch(&pool, 5);
        assert_eq!(batch.batch_id, pool.head_batch_id().unwrap());
        assert!(assert_guards(&batch, &pool, 3, 0).is_ok());
    }

    /// Once the head batch terminates, the next committed batch becomes head and may settle.
    #[test]
    fn admits_the_next_batch_once_the_former_head_has_terminated() {
        let mut pool = pool_with(3);
        pool.batch_counter = 2;
        pool.open_batches = 1; // batch 0 already terminated; batch 1 is now the head
        let mut batch = resolved_batch(&pool, 5);
        batch.batch_id = 1;

        assert!(assert_guards(&batch, &pool, 3, 0).is_ok());
    }

    /// The head-of-queue check is textually first in `assert_guards`, ahead of the `Resolved`
    /// check, so a second-in-queue batch is never reported as unresolved.
    #[test]
    fn the_head_of_queue_check_precedes_every_other_guard_in_source() {
        let prod = production(SRC);
        let body = &prod[prod
            .find("pub(crate) fn assert_guards")
            .expect("assert_guards exists")..];
        let head_check = body
            .find("BatchNotHeadOfQueue")
            .expect("assert_guards checks BatchNotHeadOfQueue");
        let resolved_check = body
            .find("BatchNotResolved")
            .expect("assert_guards checks BatchNotResolved");
        assert!(
            head_check < resolved_check,
            "the head-of-queue check must run before the resolved-state check"
        );
    }

    /// The sequence gate rejects in **both** directions. A gate that only refused the future
    /// would let a settled roll be replayed, which is the direction that pays twice.
    #[test]
    fn rejects_a_roll_that_is_not_the_next_one_in_either_direction_at_6205() {
        let pool = pool_with(3);
        let mut batch = resolved_batch(&pool, 5);
        batch.next_roll = 2;

        for index in [0u8, 1, 3, 4] {
            assert_eq!(
                error_code(assert_guards(&batch, &pool, 3, index).unwrap_err()),
                6205,
                "roll_index = {index} against next_roll = 2"
            );
        }
        assert!(assert_guards(&batch, &pool, 3, 2).is_ok());
    }

    /// `next_roll == n` is the batch's end. Reachable only on corrupt state, since completion
    /// moves the batch out of `Resolved` — refused rather than indexed past the array.
    #[test]
    fn rejects_a_roll_at_or_past_the_batchs_own_length_at_6205() {
        let pool = pool_with(3);
        let mut batch = resolved_batch(&pool, 3);
        batch.next_roll = 3;
        assert_eq!(
            error_code(assert_guards(&batch, &pool, 3, 3).unwrap_err()),
            6205
        );
    }

    #[test]
    fn rejects_every_batch_state_but_resolved_at_6206() {
        let pool = pool_with(3);
        for state in [
            BatchState::Committed,
            BatchState::Recovered,
            BatchState::Complete,
        ] {
            let mut batch = resolved_batch(&pool, 3);
            batch.state = state;
            assert_eq!(
                error_code(assert_guards(&batch, &pool, 3, 0).unwrap_err()),
                6206
            );
        }
    }

    /// The tier gate is an equality against **live** config, checked from both sides: a tier
    /// longer than the pool requires under-retains as surely as a short one over-retains, and
    /// both would make the fee split cover a different number of claims than it funds.
    #[test]
    fn rejects_a_tier_that_is_not_exactly_full_in_either_direction_at_6207() {
        let pool = pool_with(3);
        let batch = resolved_batch(&pool, 3);
        for tier_len in [0u8, 2, 4, 20] {
            assert_eq!(
                error_code(assert_guards(&batch, &pool, tier_len, 0).unwrap_err()),
                6207,
                "tier_len = {tier_len} against n_real = 3"
            );
        }
    }

    /// The gate reads live `tier_size`, not the terms the batch was priced on — `tier_size` is
    /// not snapshotted, so a shrink mid-batch moves what "full" means and the settlement must
    /// follow it rather than the value at commitment.
    #[test]
    fn the_tier_requirement_follows_live_config_rather_than_the_snapshot() {
        let mut pool = pool_with(30);
        let batch = resolved_batch(&pool, 3);
        assert!(assert_guards(&batch, &pool, 20, 0).is_ok());

        pool.tier_size = 5;
        assert!(assert_guards(&batch, &pool, 5, 0).is_ok());
        assert_eq!(
            error_code(assert_guards(&batch, &pool, 20, 0).unwrap_err()),
            6207
        );
    }

    // --- Step 3a: the running expectation ---------------------------------------------

    #[test]
    fn accepts_a_pool_that_still_matches_the_batchs_expectation() {
        let pool = pool_with(3);
        let batch = resolved_batch(&pool, 3);
        assert!(assert_expectation(&batch, &pool).is_ok());
    }

    /// A mutation from outside the batch fails; the batch's own rebalance, which rewrites the
    /// running expectation, passes.
    #[test]
    fn the_batchs_own_rebalance_passes_where_an_outside_mutation_fails() {
        let mut pool = pool_with(3);
        let mut batch = resolved_batch(&pool, 3);

        // The batch's own step 8, then the next roll's step 3 over the result.
        apply_rebalance(&mut pool, &mut batch).unwrap();
        assert!(
            assert_expectation(&batch, &pool).is_ok(),
            "a batch must pass its own rebalance"
        );

        // The same magnitude of change, arriving from anywhere else.
        pool.blanks[0] += 1;
        assert_eq!(
            error_code(assert_expectation(&batch, &pool).unwrap_err()),
            6212
        );
    }

    /// Weight and the blank set are checked separately: a mutation that moved `w_real` while
    /// leaving the blank counts alone would slip past a blanks-only comparison, and the draw
    /// reduces over total weight.
    #[test]
    fn a_weight_move_that_leaves_the_blank_counts_alone_still_fails_at_6212() {
        let mut pool = pool_with(3);
        let batch = resolved_batch(&pool, 3);
        pool.w_real += 1;
        assert_eq!(
            error_code(assert_expectation(&batch, &pool).unwrap_err()),
            6212
        );
        assert_eq!(pool.blanks, batch.expected_blanks, "only weight moved");
    }

    /// **Roll 0 seeds instead of asserting.** A batch reaching the head with a stale
    /// expectation — exactly the state a queue can put it in — must still settle its first roll:
    /// `seed_expectation` overwrites the mismatch with live state rather than failing on it.
    #[test]
    fn seed_expectation_overwrites_a_stale_value_from_live_pool_state() {
        let pool = pool_with(3);
        let mut batch = resolved_batch(&pool, 3);
        // A value from a different, earlier pool state — exactly what a queue can leave behind.
        batch.expected_w = 0;
        batch.expected_blanks = [9; 8];

        seed_expectation(&mut batch, &pool).unwrap();

        assert_eq!(
            batch.expected_w,
            aggregate_weight(pool.w_real, pool.blanks, pool.blank_faces).unwrap()
        );
        assert_eq!(batch.expected_blanks, pool.blanks);
        assert!(assert_expectation(&batch, &pool).is_ok());
    }

    // --- Step 3b: the draw and its record ------------------------------------------------------

    /// **Replay.** Every input the published expansion takes is on the record, so the
    /// draw re-derives from the batch account alone — no pool read at replay time.
    #[test]
    fn the_recorded_draw_re_derives_from_the_record_alone() {
        let pool = pool_with(4);
        let mut batch = resolved_batch(&pool, 3);

        draw_and_record(&mut batch, &pool, 0).unwrap();
        let record = batch.rolls[0];

        assert_eq!(record.w_at_draw, batch.expected_w);
        assert_eq!(record.n_real_at_draw, 4);
        assert_eq!(record.blanks_at_draw, pool.blanks);
        assert!(record.attempts >= 1);
        assert!(record.draw_value < record.w_at_draw);

        let replay = expand(&batch.randomness, 0, record.w_at_draw).unwrap();
        assert_eq!(replay.r, record.draw_value);
        assert_eq!(replay.attempts, record.attempts);
    }

    /// The roll index reaches the expansion: two rolls of one batch must not draw the same
    /// value from one 32-byte randomness.
    #[test]
    fn successive_rolls_of_one_batch_draw_independently() {
        let pool = pool_with(50);
        let mut batch = resolved_batch(&pool, 3);

        draw_and_record(&mut batch, &pool, 0).unwrap();
        draw_and_record(&mut batch, &pool, 1).unwrap();
        assert_ne!(batch.rolls[0].draw_value, batch.rolls[1].draw_value);
    }

    /// The snapshot is per-roll, not per-batch: the pipeline moves the pool between rolls, and
    /// a record carrying the batch's opening state would not reconstruct the later draws.
    #[test]
    fn each_rolls_record_carries_the_pool_as_that_roll_found_it() {
        let mut pool = pool_with(4);
        let mut batch = resolved_batch(&pool, 3);
        draw_and_record(&mut batch, &pool, 0).unwrap();

        pool.n_real = 3;
        pool.w_real = u128::from(3u32) * u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        apply_rebalance(&mut pool, &mut batch).unwrap();
        draw_and_record(&mut batch, &pool, 1).unwrap();

        assert_eq!(batch.rolls[0].n_real_at_draw, 4);
        assert_eq!(batch.rolls[1].n_real_at_draw, 3);
        assert_ne!(batch.rolls[0].w_at_draw, batch.rolls[1].w_at_draw);
    }

    // --- Step 6: the ledger --------------------------------------------------------------------

    fn split_for(pool: &Pool, tier_len: u8) -> FeeSplit {
        split_fee(
            DEFAULT_FEE,
            DEFAULT_TICKET_TARGET,
            [
                DEFAULT_ALLOC_EQUAL_BPS,
                DEFAULT_ALLOC_TIER_BPS,
                DEFAULT_ALLOC_PROTOCOL_BPS,
            ],
            pool.n_real,
            pool.blanks,
            pool.blank_faces,
            tier_len,
        )
        .unwrap()
    }

    #[test]
    fn a_settlement_retains_the_kept_fee_and_releases_the_ticket_liability() {
        let mut pool = pool_with(3);
        pool.pending_roll_liability = 3 * DEFAULT_PRICE;
        let mut top_tier = tier();
        top_tier.len = 3;
        let mut batch = resolved_batch(&pool, 3);
        let split = split_for(&pool, 3);

        apply_settle_ledger(
            &mut pool,
            &mut top_tier,
            &mut batch,
            0,
            &split,
            RollOutcome::Blank,
            0,
        )
        .unwrap();

        assert_eq!(pool.owed_fees, split.fee_keep().unwrap());
        assert_eq!(pool.pending_roll_liability, 2 * DEFAULT_PRICE);
        assert_eq!(pool.acc_equal, split.equal_unit);
        assert_eq!(top_tier.acc_tier, split.tier_unit);
        assert_eq!(batch.settled, 1);
        assert_eq!(batch.refunded, 0);
        assert!(batch.rolls[0].status == RollStatus::Settled);
        assert!(batch.rolls[0].outcome == RollOutcome::Blank);
        assert_eq!(batch.rolls[0].refund_reason, 0);
    }

    /// The drawn position's payout leaves the reserve on the same write that adds this roll's
    /// retention — the drawing roll counts as the position's last, so its own accrual is part
    /// of what funds `delta`.
    #[test]
    fn a_settled_real_card_discharges_its_payout_from_the_reserve() {
        let mut pool = pool_with(3);
        pool.pending_roll_liability = DEFAULT_PRICE;
        let mut top_tier = tier();
        top_tier.len = 3;
        let mut batch = resolved_batch(&pool, 1);
        let split = split_for(&pool, 3);
        let delta = 1_000u64;

        apply_settle_ledger(
            &mut pool,
            &mut top_tier,
            &mut batch,
            0,
            &split,
            RollOutcome::RealCard,
            delta,
        )
        .unwrap();

        assert_eq!(pool.owed_fees, split.fee_keep().unwrap() - delta);
    }

    /// A split that does not account for the whole snapshotted fee is refused before anything
    /// moves. Summing to less leaves the difference unattributed; summing to more spends
    /// against claims nothing funded.
    #[test]
    fn a_split_that_does_not_sum_to_the_snapshotted_fee_is_refused_at_6501() {
        for skew in [-1i64, 1] {
            let mut pool = pool_with(3);
            let mut top_tier = tier();
            top_tier.len = 3;
            let mut batch = resolved_batch(&pool, 1);
            let mut split = split_for(&pool, 3);
            split.protocol = (split.protocol as i64 + skew) as u64;

            assert_eq!(
                error_code(
                    apply_settle_ledger(
                        &mut pool,
                        &mut top_tier,
                        &mut batch,
                        0,
                        &split,
                        RollOutcome::Blank,
                        0,
                    )
                    .unwrap_err()
                ),
                6501,
                "skew = {skew}"
            );
        }
    }

    // --- Step 7: the refund leg ----------------------------------------------------------------

    /// **No fee is split on a refunded roll.** The whole ticket price returns, so neither
    /// accumulator moves and the reserve is untouched.
    #[test]
    fn a_refund_returns_the_whole_price_and_splits_no_fee() {
        let mut pool = pool_with(3);
        pool.pending_roll_liability = 2 * DEFAULT_PRICE;
        pool.owed_fees = 500;
        pool.acc_equal = 7;
        let mut batch = resolved_batch(&pool, 2);

        let refund = apply_refund(&mut pool, &mut batch, 0, 6204).unwrap();

        assert_eq!(refund, DEFAULT_PRICE, "the full 9.90 returns");
        assert_eq!(pool.pending_roll_liability, DEFAULT_PRICE);
        assert_eq!(pool.owed_fees, 500, "no fee was kept");
        assert_eq!(pool.acc_equal, 7, "no accumulator moved");
        assert_eq!(batch.refunded, 1);
        assert_eq!(batch.settled, 0);
        assert!(batch.rolls[0].status == RollStatus::Refunded);
        assert!(batch.rolls[0].outcome == RollOutcome::None);
        assert_eq!(batch.rolls[0].refund_reason, 6204);
    }

    /// Zero is the settled roll's reason, so it can never be a refund's. A refund recorded with
    /// reason `0` is indistinguishable from a settlement on the persisted record.
    #[test]
    fn a_refund_may_not_be_recorded_with_the_settled_rolls_reason() {
        let mut pool = pool_with(3);
        pool.pending_roll_liability = DEFAULT_PRICE;
        let mut batch = resolved_batch(&pool, 1);
        assert!(apply_refund(&mut pool, &mut batch, 0, 0).is_err());
    }

    /// The reason on the record is the error code the caller saw, derived rather than written as
    /// a second literal. `RealPoolEmpty` is the only condition this build refunds on.
    #[test]
    fn the_protective_refunds_reason_is_the_empty_pool_error_code() {
        assert_eq!(refund_code(ByeMachineError::RealPoolEmpty).unwrap(), 6204);
    }

    /// `delta` may not be computed before the accumulators move: `position_delta` reads the
    /// entitlement at the accumulator the settlement has just advanced. The split balances
    /// either way and the payout would be one roll's share short, so the ordering is asserted.
    #[test]
    fn position_delta_is_never_read_before_the_accumulators_advance() {
        let prod = production(SRC);
        // The landmark is looked up first, so a renamed or deleted bump reds here rather than
        // leaving the ordering check to pass by never running.
        let bump = prod
            .find("pool.acc_equal = pool")
            .expect("the equal accumulator's bump is this file's ordering landmark");
        let Some(read) = prod.find("position_delta(") else {
            return;
        };
        assert!(
            bump < read,
            "position_delta is read before pool.acc_equal advances — the drawn position is \
             entitled to the drawing roll's own share, so this under-pays it by exactly one \
             roll and the allocation equality still passes"
        );
    }

    /// The draw domain reads the pool's live config (`blank_faces`, `ticket_target`), not a batch
    /// snapshot. That is correct only because `update_config` refuses both moves while a batch
    /// is open, a dependency invisible from this file, so it is asserted across the seam.
    #[test]
    fn the_live_draw_domain_read_is_covered_by_update_configs_freeze_gate() {
        let update_config = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/instructions/admin/update_config.rs"),
        )
        .unwrap();
        let gate = production(&update_config);
        let body = gate
            .split("fn requires_weight_freeze")
            .nth(1)
            .expect("update_config no longer defines requires_weight_freeze");
        let body: String = body.split_whitespace().collect();

        assert!(
            body.contains("ConfigUpdate::BlankFaces(_)=>true"),
            "blank_faces is no longer frozen while a batch is open, and this file reads it live \
             — either restore the gate or settle from batch.s_blank_faces"
        );
        assert!(
            body.contains("ticket_target!=pool.ticket_target"),
            "ticket_target is no longer frozen while a batch is open, and this file reads it \
             live — either restore the gate or settle from batch.s_ticket"
        );
    }

    /// The card leg pays out of principal, so a balance check must stand in front of it. The
    /// blank leg pays out of Treasury 04 instead; its cover test is `blank_terminal`.
    #[test]
    fn the_card_leg_is_guarded_by_a_balance_check() {
        let prod = production(SRC);
        let body = prod
            .split("fn real_card_terminal(")
            .nth(1)
            .expect("real_card_terminal is no longer defined in this file");
        let delta_at = body
            .find("let delta = position_delta(")
            .expect("real_card_terminal no longer computes delta from position_delta");
        let guard_at = body.find("PrincipalShortfall").expect(
            "the card leg constructs a payout and no PrincipalShortfall check guards it — \
                 the vault could pay out more than it holds",
        );
        assert!(
            delta_at < guard_at,
            "the balance check must run after delta is computed, not before"
        );
    }

    // --- Step 8: the rebalance and the expectation rewrite -------------------------------------

    /// The rewrite is of what the rebalance just produced. A second evaluation here is how the
    /// stored expectation and the live pool come to disagree by construction.
    #[test]
    fn the_expectation_is_rewritten_from_the_post_rebalance_pool() {
        let mut pool = pool_with(3);
        let mut batch = resolved_batch(&pool, 3);
        batch.expected_w = 0;
        batch.expected_blanks = [99; 8];

        let blanks_before = apply_rebalance(&mut pool, &mut batch).unwrap();

        assert_eq!(blanks_before, [0; 8], "the pool opened with no blanks");
        assert_eq!(batch.expected_blanks, pool.blanks);
        assert_eq!(
            batch.expected_w,
            aggregate_weight(pool.w_real, pool.blanks, pool.blank_faces).unwrap()
        );
    }

    /// An unchanged blank set still rewrites and still reports — the evaluation is part of the
    /// batch's reproducible record, so a caller that emitted only on a change would leave a gap
    /// a replay cannot distinguish from a skipped step.
    #[test]
    fn a_rebalance_that_changes_nothing_still_runs_and_still_reports_its_before_state() {
        let mut pool = pool_with(3);
        let mut batch = resolved_batch(&pool, 3);
        apply_rebalance(&mut pool, &mut batch).unwrap();
        let settled_blanks = pool.blanks;

        let blanks_before = apply_rebalance(&mut pool, &mut batch).unwrap();

        assert_eq!(blanks_before, settled_blanks);
        assert_eq!(pool.blanks, settled_blanks, "nothing moved");
        assert_eq!(batch.expected_blanks, settled_blanks);
    }

    // --- Step 9: completion --------------------------------------------------------------------

    #[test]
    fn only_the_last_roll_completes_the_batch_and_reopens_the_pool() {
        let mut pool = pool_with(3);
        let mut batch = resolved_batch(&pool, 3);

        assert!(apply_completion(&mut pool, &mut batch).unwrap().is_none());
        assert_eq!(batch.next_roll, 1);
        assert_eq!(pool.open_batches, 1);
        assert!(batch.state == BatchState::Resolved);

        assert!(apply_completion(&mut pool, &mut batch).unwrap().is_none());
        assert_eq!(pool.open_batches, 1);

        assert_eq!(apply_completion(&mut pool, &mut batch).unwrap(), Some(0));
        assert_eq!(batch.next_roll, 3);
        assert!(batch.state == BatchState::Complete);
        assert_eq!(pool.open_batches, 0);
    }

    /// **A completion at non-zero `open_batches` releases nothing.** The event carries the
    /// post-decrement count so a watcher reads the reopening off the event rather than assuming
    /// one; a completion that left the count above zero has not reopened the pool.
    #[test]
    fn a_completion_that_leaves_the_count_above_zero_reports_it_and_releases_nothing() {
        let mut pool = pool_with(3);
        pool.open_batches = 2;
        let mut batch = resolved_batch(&pool, 1);

        let remaining = apply_completion(&mut pool, &mut batch).unwrap();

        assert_eq!(remaining, Some(1));
        assert_eq!(pool.open_batches, 1);
        assert!(
            crate::common::guards::assert_weight_open(&pool).is_err(),
            "the freeze must still hold at open_batches = 1"
        );
    }

    /// A batch of one is completed by its only roll — an off-by-one in the completion test
    /// would leave every single-roll batch open and the pool frozen behind it.
    #[test]
    fn a_single_roll_batch_completes_on_that_roll() {
        let mut pool = pool_with(3);
        let mut batch = resolved_batch(&pool, 1);
        assert_eq!(apply_completion(&mut pool, &mut batch).unwrap(), Some(0));
        assert!(batch.state == BatchState::Complete);
    }

    // --- The full pipeline, driven ------------------------------------------------------------

    /// **A complete n-roll drive, not a refusal matrix.** Every roll of a batch on an emptied
    /// pool takes the protective branch, and the batch still reaches its terminal: the pool
    /// reopens, the liability returns to where it started, and every roll holds a terminal
    /// status. A suite that only exercised the refusals would pass with a pipeline that can
    /// never finish, which is the failure this drive exists to exclude.
    #[test]
    fn a_batch_over_an_emptied_pool_refunds_every_roll_and_still_completes() {
        let mut pool = pool_with(0);
        pool.pending_roll_liability = 10 * DEFAULT_PRICE;
        let liability_before = pool.pending_roll_liability;
        let owed_before = pool.owed_fees;
        let mut batch = resolved_batch(&pool, 10);

        for roll_index in 0..10u8 {
            assert_guards(&batch, &pool, 0, roll_index).unwrap();
            assert_eq!(pool.n_real, 0, "the protective condition holds all the way");

            let reason = refund_code(ByeMachineError::RealPoolEmpty).unwrap();
            apply_refund(&mut pool, &mut batch, roll_index, reason).unwrap();
            apply_rebalance(&mut pool, &mut batch).unwrap();
            let completed = apply_completion(&mut pool, &mut batch).unwrap();

            assert_eq!(
                completed.is_some(),
                roll_index == 9,
                "only the last roll may complete the batch"
            );
        }

        assert!(batch.state == BatchState::Complete);
        assert_eq!(pool.open_batches, 0, "the pool reopened");
        assert_eq!(batch.refunded, 10);
        assert_eq!(batch.settled, 0);
        assert_eq!(
            pool.pending_roll_liability,
            liability_before - 10 * DEFAULT_PRICE
        );
        assert_eq!(pool.owed_fees, owed_before, "a refunded batch keeps no fee");
        for roll in batch.rolls.iter() {
            assert!(roll.status == RollStatus::Refunded);
            assert_eq!(roll.refund_reason, 6204);
        }
    }

    /// **The liability property, over batches of every admissible length and every mix of
    /// terminals.** `pending_roll_liability` returns to its pre-batch value once every roll has
    /// terminated, whichever terminal each took, and `owed_fees` never falls below what the
    /// settled rolls retained at any intermediate point.
    #[test]
    fn liability_returns_to_its_pre_batch_value_over_every_mix_of_terminals() {
        for n in 1..=10u8 {
            // A deterministic sweep of every settle/refund pattern for this length, so the
            // property is checked over mixed batches rather than over all-settled and
            // all-refunded ones only.
            for mask in 0u16..(1 << n) {
                let mut pool = pool_with(3);
                let mut top_tier = tier();
                top_tier.len = 3;
                let opening_liability = 5_000_000_000u64;
                pool.pending_roll_liability = opening_liability + u64::from(n) * DEFAULT_PRICE;
                pool.owed_fees = 0;
                let mut batch = resolved_batch(&pool, n);
                let split = split_for(&pool, 3);

                let mut retained = 0u64;
                for roll_index in 0..n {
                    if mask & (1 << roll_index) != 0 {
                        apply_settle_ledger(
                            &mut pool,
                            &mut top_tier,
                            &mut batch,
                            roll_index,
                            &split,
                            RollOutcome::Blank,
                            0,
                        )
                        .unwrap();
                        retained += split.fee_keep().unwrap();
                    } else {
                        apply_refund(&mut pool, &mut batch, roll_index, 6204).unwrap();
                    }
                    assert_eq!(
                        pool.owed_fees, retained,
                        "n = {n}, mask = {mask}: the reserve must cover exactly what the \
                         settled rolls retained at every intermediate state"
                    );
                    apply_completion(&mut pool, &mut batch).unwrap();
                }

                assert!(
                    batch.state == BatchState::Complete,
                    "n = {n}, mask = {mask}"
                );
                assert_eq!(
                    pool.pending_roll_liability, opening_liability,
                    "n = {n}, mask = {mask}: every roll released exactly its own ticket"
                );
                assert_eq!(
                    u32::from(batch.settled) + u32::from(batch.refunded),
                    u32::from(n),
                    "n = {n}, mask = {mask}: every roll reached a terminal"
                );
            }
        }
    }

    // --- Step 5: the real-card leg ------------------------------------------------------------

    /// One floor-valued `Active` position, drawable and unremarkable.
    fn drawn_position() -> Position {
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
            standard: STANDARD_PNFT,
            bump: 0,
            vault_bump: 0,
        }
    }

    fn drawn_wallet_stats() -> WalletStats {
        WalletStats {
            pool: Pubkey::default(),
            owner: Pubkey::default(),
            active_positions: 1,
            active_value: DEFAULT_ADMISSION_FLOOR,
            bump: 0,
        }
    }

    /// A vault holding far more than any payout below needs, so the balance check is never the
    /// thing under test unless a case makes it so.
    const AMPLE: u64 = 100_000_000;

    /// The entitlement is read off the accumulator this roll has already moved, since the drawing
    /// roll is the position's last active roll. The fixture makes the two orders differ by a
    /// non-zero amount: one real card takes the whole equal share, so reading one bump short
    /// pays exactly that much less.
    #[test]
    fn the_drawn_position_is_paid_its_own_rolls_share_and_not_the_previous_rolls() {
        let pool = pool_with(1);
        let top_tier = tier();
        let batch = resolved_batch(&pool, 1);
        let position = drawn_position();
        let split = split_fee(
            batch.s_fee,
            batch.s_ticket,
            batch.s_alloc_bps,
            1,
            [0; 8],
            DEFAULT_BLANK_FACES,
            0,
        )
        .unwrap();

        let RollTerminal::Settled { outcome, delta } =
            real_card_terminal(&pool, &top_tier, &position, AMPLE, &split).unwrap()
        else {
            panic!("an ample vault settles");
        };
        assert!(matches!(outcome, RollOutcome::RealCard));

        // What the same read returns against the un-bumped accumulators — the value a leg that
        // filled `delta` before the ledger would have used.
        let stale = position_delta(
            pool.acc_equal,
            position.equal_checkpoint,
            top_tier.acc_tier,
            position.tier_checkpoint,
            position.in_tier,
            position.accrued,
        )
        .unwrap();

        assert_eq!(stale, 0, "the stale read is the whole shortfall here");
        assert_eq!(
            delta, split.real_equal,
            "the sole real card takes the entire equal share of the roll that drew it"
        );
        assert!(
            delta - stale > 0,
            "a fixture where the two orders agree cannot pin an ordering"
        );
    }

    /// The tier leg is read at the bumped accumulator too, and `accrued` is folded in exactly
    /// once across both legs — the double-count is invisible in any sum identity, because the
    /// split still sums to the ticket plus the fee either way.
    #[test]
    fn a_tier_member_draws_both_legs_at_the_bumped_accumulators() {
        let pool = pool_with(1);
        let mut top_tier = tier();
        top_tier.len = 1;
        let batch = resolved_batch(&pool, 1);
        let mut position = drawn_position();
        position.in_tier = true;
        position.accrued = 7;
        let split = split_fee(
            batch.s_fee,
            batch.s_ticket,
            batch.s_alloc_bps,
            1,
            [0; 8],
            DEFAULT_BLANK_FACES,
            1,
        )
        .unwrap();

        let RollTerminal::Settled { delta, .. } =
            real_card_terminal(&pool, &top_tier, &position, AMPLE, &split).unwrap()
        else {
            panic!("an ample vault settles");
        };

        assert_eq!(
            delta,
            split.real_equal + split.tier + 7,
            "both legs at this roll's accumulators, and `accrued` once"
        );
    }

    /// A vault that cannot cover the payout aborts the settlement rather than refunding it: the
    /// payout is a balance the vault already holds against this position, so a shortfall is
    /// corrupted accounting, not a reachable outcome.
    #[test]
    fn a_vault_short_of_the_payout_is_refused_at_6600() {
        let pool = pool_with(1);
        let top_tier = tier();
        let batch = resolved_batch(&pool, 1);
        let position = drawn_position();
        let split = split_fee(
            batch.s_fee,
            batch.s_ticket,
            batch.s_alloc_bps,
            1,
            [0; 8],
            DEFAULT_BLANK_FACES,
            0,
        )
        .unwrap();

        let err = real_card_terminal(&pool, &top_tier, &position, 0, &split)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(
            error_code(err),
            6600,
            "an empty vault must refuse the card leg at PrincipalShortfall, not settle it"
        );
    }

    /// **The boundary, from both sides.** One micro-USDC decides it, and the two outcomes are
    /// reached by moving only the vault balance — a boundary tested from the short side alone
    /// passes for an implementation that refuses everything.
    #[test]
    fn the_balance_boundary_settles_at_exactly_enough_and_refuses_one_below() {
        let pool = pool_with(1);
        let top_tier = tier();
        let batch = resolved_batch(&pool, 1);
        let position = drawn_position();
        let split = split_fee(
            batch.s_fee,
            batch.s_ticket,
            batch.s_alloc_bps,
            1,
            [0; 8],
            DEFAULT_BLANK_FACES,
            0,
        )
        .unwrap();
        let exactly_enough = split.real_equal;

        let settled =
            real_card_terminal(&pool, &top_tier, &position, exactly_enough, &split).unwrap();
        assert!(
            matches!(settled, RollTerminal::Settled { .. }),
            "the vault covers it exactly, so the roll settles"
        );

        let short = real_card_terminal(&pool, &top_tier, &position, exactly_enough - 1, &split);
        assert_eq!(
            short.map(|_| ()).unwrap_err(),
            ByeMachineError::PrincipalShortfall.into(),
            "one micro-USDC below the same figure is refused"
        );
    }

    /// The departure takes the position's weight, its count and its wallet's counters, and
    /// discharges both accumulator legs rather than settling them into `accrued` — the position
    /// is being paid, not seized.
    #[test]
    fn the_departure_discharges_both_legs_and_takes_the_weight_out() {
        let mut pool = pool_with(2);
        pool.acc_equal = 5 * ACC_PRECISION;
        let mut position = drawn_position();
        position.in_tier = true;
        position.accrued = 11;
        let mut stats = drawn_wallet_stats();
        stats.active_positions = 2;
        stats.active_value = DEFAULT_ADMISSION_FLOOR * 2;

        let w_before = pool.w_real;
        let departure =
            depart_drawn_position(&mut pool, &mut position, &mut stats, 3 * ACC_PRECISION).unwrap();

        assert_eq!(departure.recorded_value, DEFAULT_ADMISSION_FLOOR);
        assert!(departure.was_member);
        assert_eq!(pool.n_real, 1);
        assert_eq!(
            pool.w_real,
            w_before - u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap())
        );
        assert_eq!(stats.active_positions, 1);
        assert_eq!(stats.active_value, DEFAULT_ADMISSION_FLOOR);

        assert_eq!(position.accrued, 0, "paid out, not carried forward");
        assert_eq!(position.equal_checkpoint, pool.acc_equal);
        assert_eq!(position.tier_checkpoint, 3 * ACC_PRECISION);
    }

    /// `owed_fees` is discharged once, on the ledger line, and the departure must not take it a
    /// second time — a second subtraction under-reserves every other position's claim while every
    /// allocation identity still balances.
    #[test]
    fn the_departure_leaves_owed_fees_to_the_ledger() {
        let mut pool = pool_with(2);
        pool.owed_fees = 900;
        let mut position = drawn_position();
        let mut stats = drawn_wallet_stats();
        stats.active_positions = 2;
        stats.active_value = DEFAULT_ADMISSION_FLOOR * 2;

        depart_drawn_position(&mut pool, &mut position, &mut stats, 0).unwrap();
        assert_eq!(pool.owed_fees, 900);
    }

    /// A non-member's `tier_checkpoint` is never a claim and is not re-stamped: leaving it alone
    /// is what keeps a later promotion reading its own stamp rather than one a settlement wrote.
    #[test]
    fn a_non_members_tier_checkpoint_is_left_alone() {
        let mut pool = pool_with(2);
        let mut position = drawn_position();
        position.tier_checkpoint = 123;
        let mut stats = drawn_wallet_stats();
        stats.active_positions = 2;
        stats.active_value = DEFAULT_ADMISSION_FLOOR * 2;

        depart_drawn_position(&mut pool, &mut position, &mut stats, 9 * ACC_PRECISION).unwrap();
        assert_eq!(position.tier_checkpoint, 123);
    }

    /// Every position state but `Active` is refused: only active positions carry the weight the
    /// search ranges over, so this turns a future mechanism change into a loud failure.
    #[test]
    fn the_departure_refuses_every_state_but_active_at_6301() {
        let all = crate::state::position::ALL_POSITION_STATES;
        assert_eq!(
            all.len(),
            5,
            "a sixth PositionState owes this settlement a decision"
        );

        let mut refused = 0;
        for (index, state) in all.into_iter().enumerate() {
            if matches!(state, PositionState::Active) {
                continue;
            }
            let mut pool = pool_with(2);
            let mut position = drawn_position();
            position.state = state;
            let mut stats = drawn_wallet_stats();

            let err = match depart_drawn_position(&mut pool, &mut position, &mut stats, 0) {
                Ok(_) => panic!("ALL_POSITION_STATES[{index}] settled and must not have"),
                Err(err) => err,
            };
            assert_eq!(error_code(err), 6301, "ALL_POSITION_STATES[{index}]");
            refused += 1;
        }
        assert_eq!(refused, 4, "four states refused, one admitted");
    }

    // --- Step 5: the settlement destination ---------------------------------------------------

    /// An ordinary depositor is paid into an account they own, and into no other.
    #[test]
    fn an_ordinary_depositors_settlement_must_land_in_their_own_account() {
        let pool_key = Pubkey::new_unique();
        let treasury_03 = Pubkey::new_unique();
        let depositor = Pubkey::new_unique();
        let destination = Pubkey::new_unique();

        assert!(assert_settlement_destination(
            destination,
            depositor,
            depositor,
            pool_key,
            treasury_03
        )
        .is_ok());

        let err = assert_settlement_destination(
            destination,
            Pubkey::new_unique(),
            depositor,
            pool_key,
            treasury_03,
        )
        .unwrap_err();
        assert_eq!(error_code(err), 6302);
    }

    /// Treasury 02 holds cards and never cash, so its proceeds route to Treasury 03. The redirect
    /// is keyed on the position's depositor and is the only difference from any other depositor;
    /// no owner check applies because Treasury 03 is an authority-held float this program does
    /// not derive.
    #[test]
    fn a_treasury_02_positions_settlement_routes_to_treasury_03() {
        let pool_key = Pubkey::new_unique();
        let treasury_03 = Pubkey::new_unique();
        let (t02_identity, _) =
            Pubkey::find_program_address(&[TREASURY_02_SEED, pool_key.as_ref()], &crate::ID);

        assert!(assert_settlement_destination(
            treasury_03,
            Pubkey::new_unique(),
            t02_identity,
            pool_key,
            treasury_03
        )
        .is_ok());

        // The depositor's own account is not a destination for this position, however it is
        // owned — routing it there is the leak this branch exists to prevent.
        let err = assert_settlement_destination(
            Pubkey::new_unique(),
            t02_identity,
            t02_identity,
            pool_key,
            treasury_03,
        )
        .unwrap_err();
        assert_eq!(error_code(err), 6503);
    }

    /// The redirect is scoped to this pool's own Treasury-02 identity. Another pool's identity is
    /// an ordinary depositor here, and paying it into this pool's Treasury 03 would move one
    /// pool's settlement into another's float.
    #[test]
    fn another_pools_treasury_02_identity_is_an_ordinary_depositor_here() {
        let pool_key = Pubkey::new_unique();
        let other_pool = Pubkey::new_unique();
        let treasury_03 = Pubkey::new_unique();
        let (other_t02, _) =
            Pubkey::find_program_address(&[TREASURY_02_SEED, other_pool.as_ref()], &crate::ID);

        let err = assert_settlement_destination(
            treasury_03,
            treasury_03,
            other_t02,
            pool_key,
            treasury_03,
        )
        .unwrap_err();
        assert_eq!(
            error_code(err),
            6302,
            "it is checked as a depositor, not redirected"
        );
    }

    // --- Structural: facts a state-level test cannot reach ------------------------------------

    /// The rebalance evaluation is unconditional. Inside a branch it would publish an evaluation
    /// on some terminals and not others, and a replay could not tell a skipped evaluation from
    /// one that changed nothing.
    #[test]
    fn the_rebalance_and_its_event_sit_outside_every_terminal_branch() {
        let prod = production(SRC);
        let handler = &prod[prod.find("pub fn handler").expect("handler is declared")..];

        assert_eq!(handler.matches("emit!(RebalanceEvaluated").count(), 1);
        assert_eq!(calls(handler, "apply_rebalance"), 1);

        let terminal_match = handler
            .find("match terminal {")
            .expect("the terminal match is where the two legs' ledgers sit");
        assert!(
            handler.find("apply_rebalance(").unwrap() > terminal_match,
            "the rebalance must follow the terminal branch, not sit inside one arm of it"
        );
        assert!(
            handler.find("emit!(RebalanceEvaluated").unwrap()
                > handler.find("apply_rebalance(").unwrap(),
            "the event reports the evaluation and so must follow it"
        );
    }

    /// Completion is the last thing the handler does, and it is in this handler rather than in
    /// an instruction of its own: the decrement is what reopens the pool, so a separate step
    /// would put a second required transaction on the critical path of a live freeze.
    #[test]
    fn completion_lands_in_this_handler_after_the_rebalance() {
        let prod = production(SRC);
        let handler = &prod[prod.find("pub fn handler").expect("handler is declared")..];

        assert_eq!(calls(handler, "apply_completion"), 1);
        assert_eq!(handler.matches("emit!(BatchCompleted").count(), 1);
        assert!(
            handler.find("apply_completion(").unwrap()
                > handler.find("emit!(RebalanceEvaluated").unwrap()
        );
    }

    /// Every roll terminates in exactly one of the two ledgers. A handler that could reach
    /// neither would leave the roll `Pending` with the transaction committed, which is the one
    /// state the batch cannot be driven out of.
    #[test]
    fn the_two_ledgers_are_reached_once_each_and_only_from_the_terminal_match() {
        let prod = production(SRC);
        let handler = &prod[prod.find("pub fn handler").expect("handler is declared")..];
        // Two settling arms, one ledger call each: the card leg and the blank leg reach the
        // same ledger by different routes, and a leg that skipped it would leave the roll
        // `Pending` with the transaction committed.
        assert_eq!(calls(handler, "apply_settle_ledger"), 2);
        assert_eq!(calls(handler, "apply_refund"), 1);
        assert_eq!(
            handler
                .matches("RollTerminal::SettledBlank { face } =>")
                .count(),
            1
        );
        // Arm patterns, not bare variant names: `Refunded` is also *constructed* in the
        // protective branch above the match, so a name count reads 2 there and 1 for `Settled`
        // and measures construction rather than the arm it is meant to pin.
        assert_eq!(
            handler
                .matches("RollTerminal::Settled { outcome, delta } =>")
                .count(),
            1
        );
        assert_eq!(
            handler
                .matches("RollTerminal::Refunded { reason } =>")
                .count(),
            1
        );
    }

    // --- the blank leg's cover test ------------------------------------------------------------

    /// A blank settles when Treasury 04 can cover the drawn face.
    #[test]
    fn a_blank_settles_at_the_snapshotted_face() {
        let pool = pool_with(3);
        let batch = resolved_batch(&pool, 1);
        for (index, face) in DEFAULT_BLANK_FACES.iter().copied().enumerate().take(3) {
            let terminal = blank_terminal(&pool, &batch, index as u8, face, 0).expect("covered");
            assert!(
                matches!(terminal, RollTerminal::SettledBlank { face: paid } if paid == face),
                "face index {index} must settle at its own face"
            );
        }
    }

    /// **The treasury's own credit for this roll counts toward the cover.** It lands in the same
    /// transaction and before the payout, so excluding it would refund every first blank a pool
    /// ever draws — the standing balance is zero until one settles.
    #[test]
    fn this_rolls_credit_counts_toward_the_cover() {
        let pool = pool_with(3);
        let batch = resolved_batch(&pool, 1);
        let face = DEFAULT_BLANK_FACES[0];

        assert!(
            matches!(
                blank_terminal(&pool, &batch, 0, 0, face).unwrap(),
                RollTerminal::SettledBlank { face: paid } if paid == face
            ),
            "a standing balance of zero plus a credit that covers the face must settle"
        );
        assert!(
            matches!(
                blank_terminal(&pool, &batch, 0, 0, face - 1).unwrap(),
                RollTerminal::Refunded { .. }
            ),
            "one unit short is still short"
        );
    }

    /// A treasury that cannot cover refunds, and the reason it records is the shortfall — not the
    /// principal-vault one the card leg uses, which would file a blank under the wrong branch.
    #[test]
    fn a_treasury_that_cannot_cover_refunds_at_the_t04_shortfall() {
        let pool = pool_with(3);
        let batch = resolved_batch(&pool, 1);
        let face = DEFAULT_BLANK_FACES[2];
        let shortfall = refund_code(ByeMachineError::T04Shortfall).unwrap();
        assert!(
            matches!(
                blank_terminal(&pool, &batch, 2, face - 1, 0).unwrap(),
                RollTerminal::Refunded { reason } if reason == shortfall
            ),
            "an uncovered face must refund, and record the treasury shortfall as the reason"
        );
        assert_ne!(
            shortfall,
            refund_code(ByeMachineError::PrincipalShortfall).unwrap(),
            "the two shortfalls must stay distinguishable in the record"
        );
    }

    /// The snapshot is what pays, and it is asserted against live config. The two agree by
    /// construction today because faces cannot move while a batch is open — this is what turns a
    /// future change to that freeze into a refusal instead of a payout at a face nobody quoted.
    #[test]
    fn a_snapshot_that_has_drifted_from_live_config_refuses() {
        let mut pool = pool_with(3);
        let batch = resolved_batch(&pool, 1);
        pool.blank_faces[1] = DEFAULT_BLANK_FACES[1] + 1;

        assert_eq!(
            blank_terminal(&pool, &batch, 1, u64::MAX, 0)
                .map(|_| ())
                .unwrap_err(),
            ByeMachineError::BatchExpectationMismatch.into()
        );
        assert!(
            blank_terminal(&pool, &batch, 0, u64::MAX, 0).is_ok(),
            "the other faces still agree and must still settle"
        );
    }

    /// The above-ticket slot is a blank like the others: it pays its own face from Treasury 04
    /// when covered and refunds at the shortfall when not.
    #[test]
    fn an_above_ticket_blank_settles_or_refunds_as_any_blank() {
        let mut pool = pool_with(3);
        pool.blank_faces[3] = 100_000_000;
        let mut batch = resolved_batch(&pool, 1);
        batch.s_blank_faces[3] = 100_000_000;
        assert!(matches!(
            blank_terminal(&pool, &batch, 3, 100_000_000, 0).unwrap(),
            RollTerminal::SettledBlank { face: 100_000_000 }
        ));
        let shortfall = refund_code(ByeMachineError::T04Shortfall).unwrap();
        assert!(matches!(
            blank_terminal(&pool, &batch, 3, 99_999_999, 0).unwrap(),
            RollTerminal::Refunded { reason } if reason == shortfall
        ));
    }

    /// A face index the array does not hold is a returned error, never an index past the end.
    #[test]
    fn a_face_index_out_of_range_is_refused() {
        let pool = pool_with(3);
        let batch = resolved_batch(&pool, 1);
        assert_eq!(
            blank_terminal(&pool, &batch, 8, u64::MAX, 0)
                .map(|_| ())
                .unwrap_err(),
            ByeMachineError::RollCountOutOfBounds.into()
        );
    }

    /// A zero face would settle by paying nothing, and the swap would ask the venue for nothing.
    #[test]
    fn a_zero_face_is_refused_rather_than_settled_for_nothing() {
        let mut pool = pool_with(3);
        let mut batch = resolved_batch(&pool, 1);
        pool.blank_faces[0] = 0;
        batch.s_blank_faces[0] = 0;
        assert_eq!(
            blank_terminal(&pool, &batch, 0, u64::MAX, 0)
                .map(|_| ())
                .unwrap_err(),
            ByeMachineError::BlankFaceZero.into()
        );
    }

    /// The CPI's mint slots are filled from the detected ordering, never positionally: a
    /// positional fill is correct only on pools whose token A is $BYE, and on the other layout
    /// the venue rejects the swap and the roll stays unsettled with the batch behind it.
    #[test]
    fn the_cpi_mint_slots_are_ordered_rather_than_named() {
        let prod = production(SRC);
        assert_eq!(
            calls(&prod, "in_pair_order"),
            1,
            "the mint slots must be filled from the pool's own ordering"
        );
        for positional in ["token_a_mint: bye_mint", "token_b_mint: usdc_mint"] {
            assert!(
                !flat(&prod).contains(positional),
                "{positional} names a slot by this protocol's convention rather than the pool's"
            );
        }
    }

    /// Neither draw arm may panic: a panicking seam aborts as `ProgramFailedToComplete`, which
    /// carries no error code. The arms stay separate patterns because they share almost nothing;
    /// a combined arm would take the card leg's path for a draw that has no card.
    #[test]
    fn neither_draw_arm_panics_and_the_two_stay_separate() {
        let prod = production(SRC);
        for seam in ["todo!(", "unimplemented!(", "panic!("] {
            assert_eq!(
                prod.matches(seam).count(),
                0,
                "{seam} in a handler every resolved batch reaches"
            );
        }
        assert!(prod.contains("DrawTarget::Blank { face_index } =>"));
        assert!(prod.contains("DrawTarget::Real { key } =>"));
        assert_eq!(
            prod.matches("ByeMachineError::BlankSettlesElsewhere")
                .count(),
            0,
            "this instruction is where a blank settles, so it never defers one elsewhere"
        );
    }

    /// The entitlement is read off the two advanced values by name, not off the pool's own
    /// fields (which would under-pay by one roll's share).
    #[test]
    fn the_entitlement_takes_the_advanced_accumulators_and_not_the_stored_ones() {
        let flat_src = flat(SRC);
        assert!(
            flat_src.contains(
                "position_delta( acc_equal_after, position.equal_checkpoint, acc_tier_after, \
                 position.tier_checkpoint, position.in_tier, position.accrued, )?;"
            ),
            "the entitlement's arguments moved — the first and third must be the advanced \
             values, not pool.acc_equal and top_tier.acc_tier"
        );
        assert!(
            flat_src.contains("checked_add(split.equal_unit)")
                && flat_src.contains("checked_add(split.tier_unit)"),
            "the advanced values must come from this roll's own units"
        );
        assert_eq!(
            calls(SRC, "position_delta"),
            1,
            "one entitlement read in production — a second is a second definition of what the \
             drawn position is owed"
        );
    }

    /// The standard gate sits on the accounts struct, so it runs before the handler and therefore
    /// before any CPI. A settlement moves USDC out of the vault before it touches the card, so a
    /// handler-side check would come after money had already left.
    #[test]
    fn the_standard_gate_runs_ahead_of_the_handler() {
        let accounts = accounts_body(SRC);
        assert!(accounts
            .contains("constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)"));
        assert_eq!(
            production(SRC).matches("STANDARD_CORE").count(),
            0,
            "this twin serves the Token Metadata family only"
        );
    }

    /// The card leaves through one release helper, and the position that signs for it is still
    /// allocated when it does. Closing the position first would be signing as an account whose
    /// discriminator had already been zeroed.
    #[test]
    fn the_position_is_closed_after_everything_that_signs_as_it() {
        let prod = production(SRC);
        assert_eq!(calls(&prod, "release_from_vault"), 1);
        assert_eq!(calls(&prod, "close_account"), 1);

        let close = prod
            .find(".close(rent_to)")
            .expect("the position is closed by value, not by a `close =` attribute");
        assert!(
            prod.find("release_drawn_card(").unwrap() < close,
            "the card must leave before the account that authorises the move is closed"
        );
        assert!(
            prod.find("close_account(").unwrap() < close,
            "the vault is closed under the position's authority, so it must close first"
        );
    }

    /// **`close =` cannot carry this closure, and the absence is the point.** Anchor's attribute
    /// closes unconditionally at the end of the instruction, and two of this handler's three
    /// terminals leave the position open — a refunded roll leaves it `Active` with its card
    /// unmoved. So the close is conditional, in the settled arm, by value.
    #[test]
    fn the_position_is_never_closed_by_attribute() {
        assert_eq!(
            accounts_body(SRC).matches("close =").count(),
            0,
            "an unconditional close would deallocate the position of a refunded roll"
        );
    }

    /// The instruction carries no weight-freeze guard. It runs only while a batch is open,
    /// which is the state the guard refuses — carrying it would make the pipeline refuse itself
    /// and leave the pool frozen behind a batch that can never finish.
    #[test]
    fn carries_no_weight_freeze_guard() {
        assert_eq!(calls(SRC, "assert_weight_open"), 0);
    }

    /// Every USDC leg leaves the principal vault through one helper under the pool's own
    /// authority: seven call sites, one `Transfer` construction. Four are direct (two Treasury 04
    /// credits, the depositor's settlement, the refund); three are the protocol split's legs
    /// inside `pay_protocol_share`. Both outcome arms must reach the split through that helper,
    /// or one could credit two of the three destinations, or Treasury 05 twice, while still
    /// summing to `fee_protocol`.
    #[test]
    fn every_usdc_leg_leaves_the_vault_through_one_authority() {
        let prod = production(SRC);
        assert_eq!(prod.matches("Transfer {").count(), 1);
        assert_eq!(prod.matches("from: accounts.principal_vault").count(), 1);
        assert_eq!(prod.matches("authority: accounts.pool").count(), 1);
        // The definition reads `fn pay_from_vault<'info>(`, so `calls` — which looks for
        // `name(` — counts the call sites and not the declaration.
        assert_eq!(calls(&prod, "pay_from_vault"), 7, "seven call sites");
        assert_eq!(
            calls(&prod, "pay_protocol_share"),
            2,
            "one per outcome arm — the card leg and the blank leg, each routing the protocol \
             share through the shared helper rather than inlining three transfers"
        );
    }

    /// The three protocol destinations are each named exactly once, inside the one helper that
    /// routes them. The two quarter-shares are equal, so wiring Treasury 05's amount to
    /// Treasury 03 changes no sum; only the account references distinguish it.
    #[test]
    fn the_protocol_split_credits_three_distinct_accounts() {
        let prod = production(SRC);
        for account in ["protocol_revenue", "treasury_03", "treasury_05"] {
            assert_eq!(
                prod.matches(&format!("&accounts.{account},")).count(),
                1,
                "{account} is not credited exactly once by the protocol split"
            );
        }
        assert!(prod.contains("share.operations,"));
        assert!(prod.contains("share.treasury_03,"));
        assert!(prod.contains("share.treasury_05,"));
    }

    /// The refund is owed to whoever paid for the batch, not to whoever submits the
    /// settlement — the two are different parties here, since this instruction is Operator-signed.
    #[test]
    fn the_refund_destination_is_pinned_to_the_batchs_own_roller() {
        assert!(production(SRC).contains("roller_usdc.owner == batch.roller"));
    }

    /// The accumulator bump is `ACC_PRECISION`-scaled, so the ledger must add the unit rather
    /// than a micro-USDC amount. A unit-confused bump is arithmetically silent.
    #[test]
    fn the_accumulators_take_the_scaled_units_and_not_the_pot() {
        let prod = production(SRC);
        assert!(prod.contains("checked_add(split.equal_unit)"));
        assert!(prod.contains("checked_add(split.tier_unit)"));
        assert_eq!(ACC_PRECISION, 1_000_000_000_000);
    }
}
