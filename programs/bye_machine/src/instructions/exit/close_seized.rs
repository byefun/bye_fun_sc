//! The cleanup instruction for a position whose escrowed Core card a third party has taken,
//! burned or frozen in place.
//!
//! **It lives in `exit/` and it is not an exit.** It moves no custody,
//! makes no CPI, closes no account and pays nobody; its `Pending` row acts on a position that
//! never activated. What it shares with this domain is the terminal it writes and the message
//! that sends a depositor here: `claim_nft_core` and `withdraw_core` reject with
//! `CollateralAbsent` (6307) or `CollateralFrozen` (6309) *naming this instruction*, and its
//! `Seized` terminal is the state `claim_nft_core` then serves. The account list is
//! `record_value`'s close path rather than a claim's.
//!
//! **Permissionless, and the asset account is the authority.** Nothing a caller signs
//! could establish that the collateral is beyond reach, and the program reads the asset itself
//! rather than trusting an assertion — so the gate is the authorisation, and it runs first.
//! A caller naming a card the census finds clear gets `CollateralPresent` (6308) and changes
//! nothing.
//!
//! **The fourth cause is `Undecidable`, and it is why the two gates finally cover everything.**
//! MPL Core can refuse a `Transfer` through mechanisms this program cannot enumerate exhaustively
//! — a collection-level freeze, a detached collection, `Royalties { rule_set: ProgramDenyList }`
//! and the external `Oracle` / `LifecycleHook` adapters, which are not even in the registry
//! `fetch_plugin` walks. Enumerating readable mechanisms one at a time always leaves a position
//! holding live `w_real` and a drawable Fenwick leaf behind a card nobody can move, which is a
//! pool halt. `core_asset.rs` instead censuses the registries of the asset **and** its collection
//! against the plugins that provably cannot gate a transfer, and answers `Undecidable` for
//! everything else — including a `plugin_type` byte a later `mpl-core` ships. That classification
//! is admitted by **both** gates: the exits proceed on it and MPL Core decides, and this
//! instruction accepts it as a cause. The set of mechanisms is still open; the complement no
//! longer depends on knowing it.
//!
//! **The overlap has a cost, and it is not hidden.** On `Undecidable` a stranger can close a
//! position whose card MPL Core would in fact still move — weight out, state `Seized`, the card
//! recoverable through `claim_nft_core` at the price of the accrual this instruction settles and
//! never pays. `core_asset.rs`'s gate states the three things that bound it; the fourth is that
//! the alternative is a pool halt.
//!
//! **The lock gate, on top of the weight freeze.** No pause flag reaches this instruction, but
//! the `Active` row **does** read `lock_until` — on every cause a depositor could arrange while
//! still ending up with the card. "The precondition is that their card is
//! gone" does not hold: this call is permissionless, so the depositor may make it themselves,
//! and on three of the four causes the precondition is something they can procure. A depositor
//! whose collection authority will act on request seizes their own position, takes the weight
//! out of the pool months before `season.tge_at + 14 days`, and still has the card. So the
//! `Active` row refuses with `PositionLocked` (6300) while the lock stands.
//!
//! **`Transferred` is on the gated side and it is the shortest of the three routes, which is
//! what the round that gated only the freezes missed.** A `PermanentTransferDelegate` — read
//! `Clear` by the census, correctly, since it grants transfers and never denies them — puts the
//! card straight into the depositor's hands, so there is no `claim_nft_core` leg to run
//! afterwards at all. Mainnet's `CCryptUfe…9CRac` carries that delegate at the collection level,
//! held by its update authority, so this is the production cohort's shape rather than a
//! hypothetical one.
//!
//! **Only `Burned` stays ungated, and it is not an exemption.** There the card is destroyed, so
//! there is nothing to collude for: a depositor who burns their own collateral to leave early
//! has paid more than the lock was worth, while the pool's weight is backed by nothing and every
//! extra block it holds pays them draw odds and fee accrual on a card that no longer exists.
//! Delaying *that* seizure would reward the depositor, not restrain them.
//!
//! **What the `Transferred` row costs, stated rather than buried:** a depositor whose card is
//! genuinely stolen keeps weight in the pool until their lock expires, and the pool pays draw
//! odds and accrual on a card it does not hold for that long. It is accepted because the
//! alternative is a lock any depositor with a cooperative collection authority can step out of,
//! which is not a lock. Narrowing the row to "the new owner is the depositor" was considered and
//! rejected — a colluding depositor names any other wallet they control, so it would buy the
//! appearance of a gate and not a gate. The split lives on
//! `SeizureKind::depositor_may_keep_the_card`, beside the enum, because it is a property of the
//! cause; a fifth cause has to choose a side there rather than inherit one.
//!
//! The three rows that hold no weight are ungated on every cause — there is no departure to
//! withhold — and two of them cannot be locked anyway: `deposit`/`deposit_core` write
//! `lock_until = 0` and only `approve_deposit` sets it, and `record_value` closes a position
//! below the floor only once the lock has expired (it *retains* a locked one).
//!
//! **The `Active` row carries the weight-freeze guard, the same as every other instruction that
//! moves `w_real`/`n_real`/`pool.blanks`.** A committed roll batch resolves its VRF draw against
//! a fixed weight domain; a seizure landing mid-batch would move that domain out from under it.
//! So `assert_weight_open` runs before `depart_weight`, exactly as it does in `approve_deposit`,
//! `record_value` and `withdraw`/`withdraw_core`. A card that is genuinely unreachable stays
//! unreachable until the batch closes — nothing here shortens the lock or the escrow, it only
//! decides which instruction is allowed to clear it and when.
//!
//! **The four source states carry two distinct effect sets, and the branch is load-bearing.**
//! Only `Active` holds weight; `Pending`, `Rejected` and `ClosedBelowFloor` hold none, for three
//! different reasons that all end in the same effect. Applying `Active`'s effect set to a
//! `ClosedBelowFloor` position would decrement `n_real` and `WalletStats` a second time and push
//! an already-freed Fenwick slot onto the free stack again — handing one leaf to two future
//! positions, a `67xx` weight-index corruption *created by the cleanup instruction*. That is why
//! the weight half is an `Option<WeightDeparture>` rather than an `if`: on the three rows that
//! hold no weight there is no slot index in the effect and no aggregate to remove it against, so
//! the removal is unreachable **through the effect** and the branch cannot be flattened by
//! accident.
//!
//! It is not unreachable absolutely, and the overclaim is worth deleting rather than softening:
//! `position.slot_index` is still readable from the handler, so a developer who "simplified the
//! branch" can write an unconditional removal that compiles. What stops *that* is the pin below
//! on where the removal sits — measured, not assumed, by an injection that wrote exactly it.

use anchor_lang::prelude::*;

use crate::common::core_asset::require_collateral_unreachable;
use crate::common::errors::ByeMachineError;
use crate::common::events::{PositionSeized, RebalanceEvaluated, SeizureKind, TierChanged};
use crate::common::guards::assert_weight_open;
use crate::common::math::{position_fee_payout, weight_of};
use crate::common::rebalance;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, TOP_TIER_SEED, WALLET_STATS_SEED,
};
use crate::common::standards::STANDARD_CORE;
use crate::common::tier;
use crate::state::{Pool, Position, PositionState, TopTier, WalletStats, WeightIndex};

/// The `Active` row's weight departure — the half that exists **only** on the one source state
/// that holds a Fenwick leaf. Carries what the `WeightIndex` removal must be driven with, the
/// blank counts from before the rebalance, and whether the departing position was a tier member.
///
/// A `None` here is not "nothing to report": it is the structural statement that this source
/// state never had a leaf, so no removal can be driven from what this function hands back.
#[cfg_attr(test, derive(Debug))]
struct WeightDeparture {
    slot_index: u32,
    recorded_value: u64,
    expected_total: u64,
    blanks_before: [u32; 3],
    was_member: bool,
}

/// What the handler needs back: the source state the event publishes (the only way an indexer
/// can tell which effect set ran), the value it publishes alongside, and the weight
/// departure on the `Active` row alone.
///
/// No test-only `Debug`, unlike [`WeightDeparture`]: it carries a `PositionState`, and that wire
/// enum derives none — its variant order is a serialization contract, so its derive list is left
/// exactly as `state/position.rs` pins it. The state comparisons below are `assert!` on `==`
/// rather than `assert_eq!` for the same reason.
struct SeizeEffect {
    source_state: PositionState,
    recorded_value: u64,
    departure: Option<WeightDeparture>,
}

/// The `Active` row's accounting: the equal accumulator leg settled **onto** the position, the
/// four checked decrements, and the rebalance evaluation.
///
/// **This is not `apply_withdraw` with the transfer deleted, and the two differences are the
/// reason it is a separate function.** `pool.owed_fees` is not decremented, because nothing is
/// paid — the fee delta settles into `Position.accrued` — and there is no roll gate, deliberate
/// above.
///
/// **What the depositor is finally owed is still open, and the accounting has a hole until it is
/// ruled.** `claim_nft_core` closes the position without paying `accrued`, so on a seized
/// position that credit is discarded while `pool.owed_fees` keeps carrying it — a liability with
/// no path that discharges it, permanently overstating what the pool owes. **It is exactly zero
/// today**, and that is measurable rather than hoped: nothing in this program increments
/// `owed_fees`, because no instruction accrues a fee yet (`grep owed_fees` over `src/` finds one
/// initialiser and one decrement). The moment fee accrual ships, either this row discharges the
/// seized position's share or the field has to track it separately, and that must be ruled
/// before that instruction lands.
/// Settling the leg here rather than dropping it is deliberate: `Position.accrued` is where the
/// claim would be read from if the decision goes that way, and a leg not settled is a number
/// nobody can reconstruct later.
///
/// **The tier leg is deliberately not settled here.** `tier::vacate` folds it into `accrued` and
/// re-stamps `tier_checkpoint` itself, exactly as it does on `record_value`'s below-floor close,
/// so settling it here as well would pay the tier delta into `accrued` twice. `accrued` is
/// therefore folded in exactly once per leg: the equal leg here, the tier leg there.
fn depart_weight(
    pool: &mut Pool,
    position: &mut Position,
    wallet_stats: &mut WalletStats,
) -> Result<WeightDeparture> {
    let recorded_value = position.recorded_value;
    let slot_index = position.slot_index;
    let was_member = position.in_tier;

    position.accrued =
        position_fee_payout(pool.acc_equal, position.equal_checkpoint, position.accrued)?;
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

    let blanks_before = pool.blanks;
    let targets = rebalance::evaluate(
        pool.n_real,
        pool.w_real,
        pool.blank_faces,
        pool.ticket_target,
    )?;
    pool.blanks = [targets.b1, targets.b2, targets.b5];

    let expected_total = WeightIndex::narrow_expected_total(pool.w_real)?;

    Ok(WeightDeparture {
        slot_index,
        recorded_value,
        expected_total,
        blanks_before,
        was_member,
    })
}

/// The branch, and the one write every source state shares.
///
/// The match is wildcard-free: a sixth `PositionState` fails to compile here rather than landing
/// on whichever arm is convenient, and the one inadmissible state errors rather than falling
/// through to "no weight moves". That restates the accounts-struct constraint with the same code
/// — `InvalidPositionState` (6301) — which is deliberate: this function is driven directly by
/// its own tests, and an exhaustive match has no third option that is not either a panic on
/// chain or a silent re-seizure of an already-`Seized` position.
fn apply_close_seized(
    pool: &mut Pool,
    position: &mut Position,
    wallet_stats: &mut WalletStats,
    kind: SeizureKind,
    now: i64,
) -> Result<SeizeEffect> {
    let source_state = position.state;
    let recorded_value = position.recorded_value;

    let departure = match source_state {
        // Holds weight, a Fenwick leaf, counters and possibly tier membership.
        PositionState::Active => {
            // The lock, on the one row that has weight to take out and on every cause a
            // depositor could arrange while still ending up with the card — the module doc
            // argues the split. This call is permissionless, so they can make it themselves,
            // and `claim_nft_core` is never lock-gated: an ungated departure on any such cause
            // is the Season-0 lock dissolved by a depositor and a cooperative
            // collection authority.
            if kind.depositor_may_keep_the_card() {
                require!(now >= position.lock_until, ByeMachineError::PositionLocked);
            }
            // This row alone departs weight, so it alone waits for the batch to close — a
            // seizure that moved `w_real`/`n_real`/`pool.blanks` mid-batch would change the
            // domain a committed VRF draw is resolved against.
            assert_weight_open(pool)?;
            Some(depart_weight(pool, position, wallet_stats)?)
        }
        // Three rows, one effect, three different reasons for it. `Pending` was never
        // activated, so `w_real`, `n_real` and both `WalletStats` counters were never
        // incremented for it and no accumulator checkpoint was ever stamped —
        // settling a fee leg here would credit it the pool's entire fee history. `Rejected` is
        // a `Pending` the operator refused, so nothing was ever incremented for it either; what
        // brings it here is that `return_rejected_core` refuses a burned or frozen card with
        // 6307/6309 **naming this instruction**, and a state its sibling can strand has to be a
        // state this instruction accepts. `ClosedBelowFloor` had all four decremented and its
        // leaf removed by `record_value` at closure; every decrement already ran in the
        // closing sweep.
        PositionState::Pending | PositionState::Rejected | PositionState::ClosedBelowFloor => None,
        PositionState::Seized => return Err(ByeMachineError::InvalidPositionState.into()),
    };

    // The state moves and the account stays, on every source state.
    position.state = PositionState::Seized;

    Ok(SeizeEffect {
        source_state,
        recorded_value,
        departure,
    })
}

/// **Plain `Context`, and the narrowing is the point.** The `<'_, '_, 'info, 'info>` form exists
/// so a handler can read `remaining_accounts`; this one no longer has any to read, and leaving
/// the wider shape behind would advertise a candidate slot that is not there.
pub fn handler(ctx: Context<CloseSeized>) -> Result<()> {
    // First, and before anything is written: this is the authorisation, not a validation. It
    // censuses two plugin registries — the card's, and the collection's, which is where this
    // pool's live freeze delegate actually sits — and answers `Undecidable` for anything it
    // cannot decide, which is the one classification the exits proceed on as well. So no
    // position is refused by both gates, and the complement holds without either side having to
    // know the list of mechanisms MPL Core might be holding a transfer up with.
    let (kind, owner_observed) = require_collateral_unreachable(
        &ctx.accounts.asset,
        &ctx.accounts.collection,
        &ctx.accounts.position_vault.key(),
    )?;

    let clock = Clock::get()?;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let acc_tier = ctx.accounts.top_tier.acc_tier;

    // `kind` and `now` reach the effect because the `Active` row's lock gate turns on both: the
    // lock is read only where the cause leaves the card in the vault.
    let effect = apply_close_seized(
        &mut ctx.accounts.pool,
        &mut ctx.accounts.position,
        &mut ctx.accounts.wallet_stats,
        kind,
        clock.unix_timestamp,
    )?;

    let mut tier_change: Option<(Option<Pubkey>, Option<Pubkey>)> = None;
    if let Some(departure) = &effect.departure {
        ctx.accounts.weight_index.load_mut()?.remove(
            departure.slot_index,
            departure.recorded_value,
            departure.expected_total,
        )?;

        if departure.was_member {
            tier::vacate(
                &mut ctx.accounts.top_tier,
                position_key,
                &mut ctx.accounts.position,
                acc_tier,
            )?;
            // **The departure only. This instruction does not fill the vacancy it opens, and
            // that is the one place its permissionless signer changes what it may do.**
            // `fill_vacancy_from_candidate` validates a candidate's pool and its eligibility —
            // `Active`, not already a member — and it cannot validate what promotion actually
            // requires, that the candidate is the *next-ranked* non-member: `TopTier` holds the
            // top `tier_size` entries and nothing about the positions outside it, so rank among
            // non-members is not knowable from any account this instruction is handed. On
            // `withdraw`, `withdraw_core` and `record_value` that unverifiable choice belongs to
            // the depositor or the Operator. Here it would belong to **anyone**, and the thing
            // being chosen is membership of the tier that takes 5% of every ticket — so a
            // stranger could wait for any seizure and hand a position of their own the departing
            // member's seat. The vacancy is left open for `update_tier`, which is
            // Operator-signed and exists for exactly this promotion; an under-filled tier is
            // already a reachable state on every close path, since a caller can simply supply no
            // candidate.
            tier_change = Some((Some(position_key), None));
        }
    }

    emit!(PositionSeized {
        pool: pool_key,
        slot: clock.slot,
        position: position_key,
        kind,
        owner_observed,
        source_state: effect.source_state,
        recorded_value: effect.recorded_value,
    });

    // Conditional on the same `Option` the removal is: an unconditional emit here would claim
    // a blank-set re-evaluation on a source state that moved no weight.
    if let Some(departure) = &effect.departure {
        emit!(RebalanceEvaluated {
            pool: pool_key,
            slot: clock.slot,
            n_real: ctx.accounts.pool.n_real,
            w_real: ctx.accounts.pool.w_real,
            blanks_before: departure.blanks_before,
            blanks_after: ctx.accounts.pool.blanks,
        });
    }

    if let Some((left, entered)) = tier_change {
        emit!(TierChanged {
            pool: pool_key,
            slot: clock.slot,
            left,
            entered,
        });
    }

    Ok(())
}

/// Permissionless: `caller` signs for the transaction and for nothing else. It is not
/// named `payer` — the crate's other two permissionless handlers fund an account MPL Core
/// creates for them, and this instruction makes no CPI, allocates nothing and closes nothing, so
/// there is no payment for a name to describe.
///
/// **The account set is `record_value`'s close path, not `claim_nft`'s** — the two claims carry
/// no pool-level surface at all, because everything had already moved when the position closed,
/// whereas the `Active` row here is the instruction that moves it. Every one of these accounts
/// is bound through `pool`: the position's seeds take `pool.key()` rather than its own stored
/// `position.pool`, so a caller cannot pair one pool's position with another pool's counters and
/// decrement the wrong pool. Three of them are untouched on three of the four source states,
/// which is the cost of one instruction serving all four rather than four instructions.
///
/// **No `close =` on any account, on any source state**: the position and its vault
/// survive the seizure, because a seizure is reversible on three of its four causes and the
/// account is what the claim needs. A freeze, a transfer-out and a plugin this program cannot
/// decide are all undoable by the authority behind them — which is also why all three are
/// lock-gated on the `Active` row, since undoable by that authority means arrangeable with it —
/// and `claim_nft_core` succeeds the moment any of them is undone, but only if
/// `depositor` and `vault_bump` still exist to sign with, which is exactly what deallocating
/// here would take away, stranding a thawed card in a PDA nothing can sign for.
///
/// **On the burn cause the claim is owed to nobody and the rent stays locked, and that is an open
/// decision rather than this instruction's answer.** There is no card to return, so
/// `claim_nft_core` refuses with `CollateralAbsent` (6307) forever and the position's rent is
/// unreclaimable. Whether the depositor may voluntarily close and reclaim it is
/// unsettled, so no close path is written here — one would be deciding it. What this comment must
/// not do is claim the claim is still owed on all three causes, which is false on exactly this
/// one.
#[derive(Accounts)]
pub struct CloseSeized<'info> {
    pub caller: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        // The MPL Core family only. On the accounts struct, as every twin and
        // principal places it, so it runs before the collateral read.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction,
        // Every state whose card is still in the vault, which is every state but `Seized`
        // itself — escrow begins before activation, outlives closure, and outlives rejection.
        // The admitted set is derived from one rule rather than listed: a
        // sibling that refuses a burned or frozen card returns 6307/6309 **naming this
        // instruction**, so any state a sibling can strand has to be a state this instruction
        // accepts. `ClosedBelowFloor` is `claim_nft_core`'s, and `Rejected` is
        // `return_rejected_core`'s.
        constraint = matches!(
            position.state,
            PositionState::Pending
                | PositionState::Active
                | PositionState::ClosedBelowFloor
                | PositionState::Rejected
        ) @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(mut, address = pool.weight_index)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    #[account(
        mut,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(
        mut,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()],
        bump = wallet_stats.bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    /// CHECK: the escrow identity, and **not an account** — its only on-chain expression is
    /// `AssetV1.owner`, which is precisely what the collateral read compares against. Derived
    /// here so that comparison is against this position's own vault. Never `mut`: nothing signs
    /// under these seeds here, because nothing is transferred.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    /// CHECK: **the position's own card, and this pin is the one the collateral read cannot make
    /// for itself.** `read_collateral` classifies whatever account it is handed, so without this
    /// address a caller could present some unrelated burned asset and seize a position whose
    /// card is safely escrowed. Not `mut`: this instruction reads the asset and never moves it.
    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: the asset's collection, whose **own** plugin registry is half the freeze read —
    /// `permanent_freeze_delegate` sits on either an asset or a collection, and this pool's live
    /// one sits on the collection, held by a third party. Without this
    /// account the seizure this instruction exists for is undetectable: the card is present,
    /// vault-owned and unfrozen in its own registry, so the read would answer `Settleable` and
    /// refuse to close a position whose card MPL Core will not move.
    ///
    /// **Pinned in the classification rather than here, and it has to be.** `Position` stores no
    /// collection, so there is no stored address for `#[account(address = ..)]` to compare
    /// against; the only truth is the asset's own `update_authority`, which is not readable at
    /// account-validation time. `read_collateral` requires this account to be exactly that
    /// collection before it walks the registry, which is what stops a caller naming any frozen
    /// collection and seizing a healthy position with it. Never `mut`: read only, and no CPI.
    ///
    /// **The census walks this account's registry as well as the asset's**, so it is not only
    /// where a freeze can hide — it is where a rejecting `Royalties` rule set or an external
    /// adapter that declares a Transfer check can hide too, and either of those is what makes
    /// the classification `Undecidable` rather than `Settleable`. A `Royalties` record whose
    /// rule set is `None` hides nothing and is `Clear`: the live production collection carries
    /// one, and reading it by plugin type rather than by rule set is what once put the whole
    /// cohort on the undecidable arm.
    pub collection: UncheckedAccount<'info>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        ACC_PRECISION, DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };
    use crate::common::events::ALL_SEIZURE_KINDS;
    use crate::common::test_support::{
        accounts_body, calls, crate_sources, flat, production, without_doc_comments,
    };
    use crate::state::position::ALL_POSITION_STATES;
    use crate::state::{TierEntry, TopTier};
    use std::fs;

    const CLOSE_SEIZED_SRC: &str = include_str!("close_seized.rs");
    const CLAIM_NFT_CORE_SRC: &str = include_str!("claim_nft_core.rs");
    const WITHDRAW_SRC: &str = include_str!("withdraw.rs");
    const WITHDRAW_CORE_SRC: &str = include_str!("withdraw_core.rs");
    const RECORD_VALUE_SRC: &str = include_str!("../value/record_value.rs");
    const EXIT_ACCOUNTING_SRC: &str = include_str!("../../common/exit_accounting.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    /// The handler's own body — production source, doc comments stripped, bounded **above** by
    /// its signature and **below** by the accounts struct that follows it. Every earlier attempt
    /// at "the handler does not do X" here read to end-of-file and so answered the question with
    /// the `#[derive(Accounts)]` block, where the vault seed legitimately appears.
    fn handler_body() -> String {
        let code = without_doc_comments(production(CLOSE_SEIZED_SRC));
        let start = code.find("pub fn handler").expect("the handler");
        let end = code
            .find("#[derive(Accounts)]")
            .expect("the accounts struct");
        assert!(
            start < end,
            "the handler precedes the accounts struct in this file"
        );
        code[start..end].to_string()
    }

    /// A variant's own identifier, as the accounts struct spells it. Wildcard-free, so a sixth
    /// `PositionState` fails to compile here rather than being formatted into a name the source
    /// scan below would never find.
    fn state_name(state: PositionState) -> &'static str {
        match state {
            PositionState::Pending => "Pending",
            PositionState::Active => "Active",
            PositionState::ClosedBelowFloor => "ClosedBelowFloor",
            PositionState::Rejected => "Rejected",
            PositionState::Seized => "Seized",
        }
    }

    /// **The cause and the clock every case below runs under unless it says otherwise.**
    /// `Frozen` is deliberate rather than arbitrary: it is one of the two causes that leave the
    /// card in the vault, so it is the pair `(Active, escrowed cause)` the lock gate actually
    /// reads — a default of `Burned` would run every `Active` case down the ungated path and no
    /// case would notice the gate being deleted. `now` is well past the fixture's `lock_until`
    /// of 0, so the gate passes and the effect sets are what each case is measuring.
    fn seize(
        pool: &mut Pool,
        position: &mut Position,
        wallet_stats: &mut WalletStats,
    ) -> Result<SeizeEffect> {
        apply_close_seized(pool, position, wallet_stats, SeizureKind::Frozen, 1_000)
    }

    fn position_in(state: PositionState) -> Position {
        Position {
            pool: Pubkey::default(),
            depositor: Pubkey::default(),
            nft_mint: Pubkey::default(),
            position_id: 0,
            state,
            recorded_value: DEFAULT_ADMISSION_FLOOR,
            value_observed_at: 0,
            deposit_value: 0,
            slot_index: 7,
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

    /// The pool as it stands with **one** floor-valued `Active` real position in it — the state
    /// every `Active`-row case below seizes out of.
    fn pool_holding_one_active() -> Pool {
        let mut pool = Pool::blank();
        pool.admission_floor = DEFAULT_ADMISSION_FLOOR;
        pool.blank_faces = DEFAULT_BLANK_FACES;
        pool.ticket_target = DEFAULT_TICKET_TARGET;
        pool.n_real = 1;
        pool.w_real = u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap());
        pool.owed_fees = 100;
        pool
    }

    fn wallet_holding_one_active() -> WalletStats {
        WalletStats {
            pool: Pubkey::default(),
            owner: Pubkey::default(),
            active_positions: 1,
            active_value: DEFAULT_ADMISSION_FLOOR,
            bump: 0,
        }
    }

    /// The pool **after** `record_value` closed that position below the floor: the leaf gone, the
    /// aggregate at zero, both counters already decremented. This is the state the
    /// `ClosedBelowFloor` row is reached in, and the state the `Active` effect set corrupts.
    fn pool_after_the_closing_sweep() -> (Pool, WalletStats) {
        let mut pool = pool_holding_one_active();
        pool.n_real = 0;
        pool.w_real = 0;
        let wallet_stats = WalletStats {
            pool: Pubkey::default(),
            owner: Pubkey::default(),
            active_positions: 0,
            active_value: 0,
            bump: 0,
        };
        (pool, wallet_stats)
    }

    // --- one case per source state, each asserting the two effect sets that did NOT run

    /// The `Active` row: the full set. Every field the other two rows must leave alone is
    /// asserted here **as a movement**, so the tests below asserting the same fields as
    /// *unmoved* are a comparison rather than a restatement of the initial value.
    #[test]
    fn the_active_row_moves_the_weight_and_reports_the_leaf_to_remove() {
        let mut pool = pool_holding_one_active();
        let mut position = position_in(PositionState::Active);
        let mut wallet_stats = wallet_holding_one_active();

        let effect = seize(&mut pool, &mut position, &mut wallet_stats).unwrap();

        assert!(effect.source_state == PositionState::Active);
        assert!(position.state == PositionState::Seized);
        assert_eq!(pool.w_real, 0, "the departing leaf's weight left w_real");
        assert_eq!(pool.n_real, 0);
        assert_eq!(wallet_stats.active_positions, 0);
        assert_eq!(wallet_stats.active_value, 0);

        let departure = effect
            .departure
            .as_ref()
            .expect("the Active row is the one source state that carries a weight departure");
        assert_eq!(
            departure.slot_index, 7,
            "the removal is driven with the position's own slot, unmoved"
        );
        assert_eq!(
            departure.recorded_value, DEFAULT_ADMISSION_FLOOR,
            "the removal verifies against the value the leaf was inserted at"
        );
        assert_eq!(
            departure.expected_total, 0,
            "and against the aggregate this function has already written"
        );
        assert!(!departure.was_member);
    }

    /// The `Pending` row: the state and nothing else. Its counters are incremented at
    /// *activation*, not at `deposit`, so a position that never activated never
    /// contributed to them — and the `Active` effect set applied here would decrement four
    /// counters this position was never counted in.
    #[test]
    fn the_pending_row_moves_nothing_but_the_state() {
        let mut pool = pool_holding_one_active();
        let mut position = position_in(PositionState::Pending);
        let mut wallet_stats = wallet_holding_one_active();
        let before = (pool.w_real, pool.n_real, pool.blanks, pool.owed_fees);

        let effect = seize(&mut pool, &mut position, &mut wallet_stats).unwrap();

        assert!(effect.source_state == PositionState::Pending);
        assert!(position.state == PositionState::Seized);
        assert!(
            effect.departure.is_none(),
            "a Pending position has no Fenwick leaf, so there is nothing for the handler to \
             remove — and no slot index it could remove one with"
        );
        assert_eq!(
            (pool.w_real, pool.n_real, pool.blanks, pool.owed_fees),
            before,
            "no pool field moves on the Pending row"
        );
        assert_eq!(wallet_stats.active_positions, 1, "a wallet counter moved");
        assert_eq!(wallet_stats.active_value, DEFAULT_ADMISSION_FLOOR);
    }

    /// The `ClosedBelowFloor` row: record the cause and nothing else. Every decrement
    /// already ran in the closing sweep, and the account and vault were already being
    /// kept for `claim_nft_core`.
    #[test]
    fn the_closed_below_floor_row_moves_nothing_but_the_state() {
        let (mut pool, mut wallet_stats) = pool_after_the_closing_sweep();
        let mut position = position_in(PositionState::ClosedBelowFloor);
        let before = (pool.w_real, pool.n_real, pool.blanks, pool.owed_fees);

        let effect = seize(&mut pool, &mut position, &mut wallet_stats).unwrap();

        assert!(effect.source_state == PositionState::ClosedBelowFloor);
        assert!(position.state == PositionState::Seized);
        assert!(effect.departure.is_none());
        assert_eq!(
            (pool.w_real, pool.n_real, pool.blanks, pool.owed_fees),
            before
        );
        assert_eq!(wallet_stats.active_positions, 0);
        assert_eq!(wallet_stats.active_value, 0);
        assert_eq!(
            effect.recorded_value, DEFAULT_ADMISSION_FLOOR,
            "the last recorded value still reaches the event — the sweep removed the leaf, not \
             the record of what it was worth"
        );
    }

    /// **Why the branch is load-bearing, driven rather than argued.** The `Active` effect set,
    /// applied to a position the closing sweep already accounted for, fails closed on the very
    /// first decrement — and every one of the four would have been a second decrement. The
    /// handler never reaches it, because `apply_close_seized` hands back no departure to reach
    /// it with; this drives the private half directly to show what the branch is standing
    /// between.
    #[test]
    fn the_active_effect_set_applied_to_a_closed_position_fails_closed_at_6703() {
        let (mut pool, mut wallet_stats) = pool_after_the_closing_sweep();
        let mut position = position_in(PositionState::ClosedBelowFloor);

        let err = depart_weight(&mut pool, &mut position, &mut wallet_stats).unwrap_err();
        assert_eq!(
            error_code(err),
            6703,
            "n_real is already 0 — the second decrement under-runs"
        );
    }

    /// The other half of the same hazard, and the half no arithmetic catches: a
    /// `ClosedBelowFloor` position's `slot_index` still names the leaf `record_value` freed, so
    /// an `Active`-row removal here would push an already-free slot onto the free stack a second
    /// time and hand one leaf to two future positions. The type is what prevents it — there is
    /// no `WeightDeparture` on this row to carry the stale slot forward.
    #[test]
    fn a_closed_positions_stale_slot_index_never_reaches_a_removal() {
        let (mut pool, mut wallet_stats) = pool_after_the_closing_sweep();
        let mut position = position_in(PositionState::ClosedBelowFloor);
        assert_eq!(position.slot_index, 7, "the freed slot is still recorded");

        let effect = seize(&mut pool, &mut position, &mut wallet_stats).unwrap();

        assert!(effect.departure.is_none());
        assert_eq!(
            position.slot_index, 7,
            "and it is left recorded — this instruction does not rewrite history, it just \
             refuses to act on it"
        );
    }

    /// All four admitted source states reach the same terminal, and the one inadmissible state
    /// is refused with the code the accounts-struct constraint returns. The iteration is over
    /// `PositionState`'s own five variants with no wildcard, so a sixth variant owes this test a
    /// decision.
    ///
    /// **`Rejected` is admitted, and it is the fourth row rather than the third for one reason:**
    /// `return_rejected_core` refuses a burned or frozen card with `CollateralAbsent` (6307) or
    /// `CollateralFrozen` (6309), both of which name this instruction. A `close_seized` that
    /// refused `Rejected` would refuse the only call that message leaves a caller, and the
    /// rejected position — whose card is still in the vault, since rejection returns nothing by
    /// itself — would have no terminal at all. It holds no weight, so its effect set is
    /// `Pending`'s.
    #[test]
    fn every_admitted_source_state_terminates_at_seized_and_no_other_state_is_admitted() {
        let admitted = [
            PositionState::Pending,
            PositionState::Active,
            PositionState::ClosedBelowFloor,
            PositionState::Rejected,
        ];
        // `ALL_POSITION_STATES`, not a copy of it: three sweeps in this crate wrote their own
        // and three of them still read four after `Seized` arrived.
        for state in ALL_POSITION_STATES {
            let mut pool = pool_holding_one_active();
            let mut position = position_in(state);
            let mut wallet_stats = wallet_holding_one_active();
            let outcome = seize(&mut pool, &mut position, &mut wallet_stats);

            if admitted.contains(&state) {
                let effect = outcome.expect("an admitted source state must close");
                assert!(effect.source_state == state);
                assert!(position.state == PositionState::Seized);
                assert_eq!(
                    effect.departure.is_some(),
                    state == PositionState::Active,
                    "only the Active row moves weight"
                );
            } else {
                // Matched rather than `unwrap_err`, which would need `SeizeEffect: Debug` — and
                // it carries a `PositionState`, whose derive list is a wire contract.
                let err = match outcome {
                    Ok(_) => panic!("an inadmissible source state must be refused"),
                    Err(err) => err,
                };
                assert_eq!(
                    error_code(err),
                    6301,
                    "an inadmissible source state must fail with the same code the accounts \
                     constraint returns"
                );
                assert!(
                    position.state == state,
                    "and must leave the state it refused untouched"
                );
            }
        }
    }

    // --- the fee legs, settled onto the position and never paid --------------------------------

    /// **No cash moves here, and `owed_fees` is the field that proves it.** The equal
    /// leg is settled onto `Position.accrued` and `owed_fees` is left standing.
    /// `apply_withdraw` decrements `owed_fees` by what it pays out, which is the difference that
    /// makes this a separate function rather than the same one with the transfer deleted.
    ///
    /// **What is left standing is a liability nothing discharges, and calling it "stopped
    /// growing" was the error worth correcting.** `claim_nft_core` closes a seized position
    /// without paying `accrued`, so the credit settled here is discarded while the pool's own
    /// figure keeps carrying it — an overstatement with no path that clears it. The amount is
    /// **structurally zero today**, which the second assertion below measures
    /// rather than assumes: nothing in this crate increments `owed_fees`, so a settled leg can
    /// only ever be `0` and the liability can only ever be the initialiser's. This has to be
    /// ruled before the instruction that accrues a fee ships — either this row discharges the
    /// seized share or the field tracks it apart.
    #[test]
    fn the_equal_leg_settles_onto_the_position_and_pays_nothing_out() {
        let mut pool = pool_holding_one_active();
        pool.acc_equal = 5 * ACC_PRECISION;
        let mut position = position_in(PositionState::Active);
        position.equal_checkpoint = 3 * ACC_PRECISION;
        position.accrued = 7;
        let mut wallet_stats = wallet_holding_one_active();

        seize(&mut pool, &mut position, &mut wallet_stats).unwrap();

        assert_eq!(
            position.accrued, 9,
            "equal delta 2 + accrued 7, folded in once"
        );
        assert_eq!(position.equal_checkpoint, 5 * ACC_PRECISION);
        assert_eq!(
            pool.owed_fees, 100,
            "owed_fees is untouched — nothing was paid, so nothing discharged, and nothing ever \
             will be for this position: claim_nft_core closes it without reading `accrued`"
        );
        // The measurement the doc's "structurally zero" claim rests on, and it is about the
        // whole crate rather than this function: an incrementing site is what would turn the
        // undischarged liability from a `0` into a number, so the claim reds when one lands
        // rather than when someone rereads the comment. `= 0` initialisers are excluded by the
        // `+ ` and `checked_add` forms this looks for.
        //
        // **Walked rather than listed.** A hand-listed set of files would miss a new handler
        // accruing into `owed_fees` — and a new handler doing exactly that is what
        // `commit_rolls` brings. A tripwire whose reach is narrower than its claim is a tripwire
        // that reds late.
        for path in crate_sources() {
            let label = path.display().to_string();
            let source = fs::read_to_string(&path).unwrap();
            // Whitespace removed rather than collapsed, so the match cannot depend on how
            // rustfmt happened to break the chain: `pool.owed_fees.checked_add(..)` and the
            // same call wrapped over three lines are one string here. The list this replaced
            // carried a three-line literal with eight spaces of indentation baked in, which
            // would have missed the identical call one block deeper.
            let packed: String = without_doc_comments(production(&source))
                .split_whitespace()
                .collect();
            assert!(
                !packed.contains("owed_fees.checked_add") && !packed.contains("owed_fees+"),
                "{label} accrues into owed_fees — a seized \
                 position's undischarged share is no longer zero, and the fee-discharge \
                 decision above must be ruled"
            );
        }
        assert_eq!(
            calls(EXIT_ACCOUNTING_SRC, "checked_sub"),
            5,
            "apply_withdraw's five checked_sub calls include the owed_fees discharge this \
             function deliberately omits — if that count moves, re-read which one this claim \
             is contrasting against"
        );
    }

    /// **A `Pending` position never had a checkpoint stamped, so settling a fee leg on it would
    /// credit it the pool's entire fee history.** With `acc_equal` at 5 and
    /// `equal_checkpoint` at its as-deposited zero, the fabricated credit would be 5 rather than
    /// the 0 this position is owed. The `Active` effect set applied to a `Pending` row is
    /// usually described as a double-decrement hazard; this is the same branch stopping an
    /// invented credit, which no counter under-run would have caught.
    #[test]
    fn the_pending_row_settles_no_fee_leg_and_invents_no_accrual() {
        let mut pool = pool_holding_one_active();
        pool.acc_equal = 5 * ACC_PRECISION;
        let mut position = position_in(PositionState::Pending);
        let mut wallet_stats = wallet_holding_one_active();

        seize(&mut pool, &mut position, &mut wallet_stats).unwrap();

        assert_eq!(position.accrued, 0, "a pending position accrued nothing");
        assert_eq!(
            position.equal_checkpoint, 0,
            "and its checkpoint is not stamped — activation stamps it, and this position never \
             activated"
        );
    }

    /// **`accrued` is folded in exactly once per leg, and the tier leg is `vacate`'s.** This
    /// function settles the equal leg only; `tier::vacate` settles the tier leg and re-stamps
    /// `tier_checkpoint` itself, exactly as it does on `record_value`'s below-floor close.
    /// Settling it here as well would pay the tier delta into `accrued` twice — 2 + 5 + 7 = 14
    /// against the double-counted 19.
    #[test]
    fn the_tier_leg_is_vacates_and_lands_in_accrued_exactly_once() {
        let mut pool = pool_holding_one_active();
        pool.acc_equal = 5 * ACC_PRECISION;
        let acc_tier = 9 * ACC_PRECISION;
        let mut position = position_in(PositionState::Active);
        position.in_tier = true;
        position.equal_checkpoint = 3 * ACC_PRECISION;
        position.tier_checkpoint = 4 * ACC_PRECISION;
        position.accrued = 7;
        let mut wallet_stats = wallet_holding_one_active();

        let effect = seize(&mut pool, &mut position, &mut wallet_stats).unwrap();
        assert!(effect.departure.as_ref().unwrap().was_member);
        assert_eq!(
            position.accrued, 9,
            "the equal leg alone at this point — the tier delta has not been drawn yet"
        );

        let position_key = Pubkey::new_unique();
        let mut top_tier = TopTier {
            pool: Pubkey::default(),
            len: 1,
            entries: [TierEntry {
                position: position_key,
                value: DEFAULT_ADMISSION_FLOOR,
                activated_at: 0,
                position_id: 0,
            }; 20],
            acc_tier,
            bump: 0,
        };
        tier::vacate(&mut top_tier, position_key, &mut position, acc_tier).unwrap();

        assert_eq!(position.accrued, 14, "equal 2 + tier 5 + accrued 7");
        assert_ne!(
            position.accrued, 19,
            "accrued double-counted across the legs"
        );
        assert_eq!(position.tier_checkpoint, acc_tier);
        assert!(!position.in_tier);
        assert_eq!(top_tier.len, 0);
    }

    /// The rebalance runs on the `Active` row and its pre-mutation blank counts are what the
    /// event publishes. Driven at a `blank_faces`/`ticket_target` surface where the evaluation
    /// actually moves the blanks, so a `blanks_before` captured *after* the write would be
    /// visible.
    #[test]
    fn the_active_row_reports_the_blanks_from_before_its_own_rebalance() {
        let mut pool = pool_holding_one_active();
        pool.blanks = [1, 2, 3];
        let mut position = position_in(PositionState::Active);
        let mut wallet_stats = wallet_holding_one_active();

        let effect = seize(&mut pool, &mut position, &mut wallet_stats).unwrap();

        let departure = effect.departure.unwrap();
        assert_eq!(departure.blanks_before, [1, 2, 3]);
        assert_eq!(
            pool.blanks,
            [0, 0, 0],
            "n_real reached 0, so the evaluation zeroes the blank set — and the before/after \
             pair the event publishes are two different values"
        );
    }

    // --- the gate, first and inverse ------------------------------------------------------------

    /// **The gate is the authorisation, so its position in the handler is the claim.** It runs
    /// before `apply_close_seized`, and therefore before any state write, and it is the inverse
    /// of the one the three exits carry: `core_asset.rs` owns the classification and the codes,
    /// this file owns the reach and the ordering.
    #[test]
    fn the_inverse_gate_runs_first_against_this_positions_vault() {
        assert!(flat(CLOSE_SEIZED_SRC).contains(
            "let (kind, owner_observed) = require_collateral_unreachable( &ctx.accounts.asset, \
             &ctx.accounts.collection, &ctx.accounts.position_vault.key(), )?;"
        ));
        assert_eq!(calls(CLOSE_SEIZED_SRC, "require_collateral_unreachable"), 1);
        assert_eq!(
            calls(CLOSE_SEIZED_SRC, "require_collateral_settleable"),
            0,
            "the exits' gate would refuse exactly the positions this instruction exists to close"
        );

        // Sliced to the handler: over the whole file, `find` would land on
        // `fn apply_close_seized(`'s own declaration, which precedes the handler and would make
        // this assertion true for a reason that has nothing to do with the call order.
        let handler = handler_body();
        assert!(
            handler.find("require_collateral_unreachable(").unwrap()
                < handler.find("apply_close_seized(").unwrap(),
            "the gate must precede the state write, not follow it"
        );
        assert!(
            calls(CLAIM_NFT_CORE_SRC, "require_collateral_settleable") == 1
                && calls(CLAIM_NFT_CORE_SRC, "require_collateral_unreachable") == 0,
            "the two gates must still be on opposite sides of this pair, or the contrast above \
             measures nothing"
        );
    }

    /// **The cause and the owner reach the event from the read that authorised the call**, not
    /// from a second read — one classification, so the audit record cannot disagree with the
    /// gate that admitted it.
    #[test]
    fn the_event_payload_is_pinned_field_by_field_and_carries_the_gates_own_reading() {
        assert!(flat(CLOSE_SEIZED_SRC).contains(
            "emit!(PositionSeized { pool: pool_key, slot: clock.slot, position: position_key, \
             kind, owner_observed, source_state: effect.source_state, recorded_value: \
             effect.recorded_value, });"
        ));
        assert!(flat(CLOSE_SEIZED_SRC).contains("let (kind, owner_observed) ="));
        assert_eq!(calls(CLOSE_SEIZED_SRC, "read_collateral"), 0);
        assert_eq!(
            production(CLOSE_SEIZED_SRC).matches("emit!(").count(),
            3,
            "PositionSeized always, RebalanceEvaluated and TierChanged conditionally"
        );
    }

    /// Both conditional events sit inside the departure branch: an unconditional
    /// `RebalanceEvaluated` here would claim an effect that did not occur on
    /// three of the four source states.
    #[test]
    fn the_two_conditional_events_are_inside_the_departure_branch() {
        let code = without_doc_comments(production(CLOSE_SEIZED_SRC));
        let branch = code
            .find("if let Some(departure) = &effect.departure {")
            .expect("the removal branch");
        let second = code[branch + 1..]
            .find("if let Some(departure) = &effect.departure {")
            .expect("the emit branch");
        assert!(
            code.find("emit!(RebalanceEvaluated").unwrap() > branch + second,
            "RebalanceEvaluated must sit inside the second departure branch"
        );
        assert!(
            code.find("emit!(TierChanged").unwrap()
                > code
                    .find("if let Some((left, entered)) = tier_change {")
                    .unwrap(),
            "TierChanged must sit inside the tier_change branch"
        );
        assert!(
            code.find("emit!(PositionSeized").unwrap() < branch + second,
            "PositionSeized is unconditional and must precede both"
        );
    }

    /// **The removal sits in a window only the branch can enter, and that — not the `Option`
    /// alone — is what makes an unconditional removal unwritable.** The two ordering asserts
    /// below bound it above by the departure branch's opening and below by `tier::vacate`, and
    /// an unconditional removal lands outside that window in one direction or the other: before
    /// the branch, and the first assert fails; after the tier work, and the second does. An
    /// injection wrote the second form — reading `position.slot_index` directly, which
    /// compiles — and this is the pin that reddened on it. The `Option` stops the branch being
    /// flattened; this stops it being bypassed.
    #[test]
    fn the_leaf_removal_is_reached_only_through_the_departure_and_only_once() {
        let code = without_doc_comments(production(CLOSE_SEIZED_SRC));
        assert_eq!(code.matches(".remove(").count(), 1);
        assert_eq!(calls(CLOSE_SEIZED_SRC, "vacate"), 1);
        assert!(
            code.find("if let Some(departure) = &effect.departure {")
                .unwrap()
                < code.find(".remove(").unwrap()
        );
        assert!(code.find(".remove(").unwrap() < code.find("tier::vacate(").unwrap());
    }

    /// **This instruction vacates a tier seat and never fills one, and the absence is the whole
    /// of a permissionless caller's reach into the tier.** `fill_vacancy_from_candidate` checks
    /// a candidate's pool and its eligibility and *cannot* check the actual promotion rule — that
    /// the candidate is the next-ranked non-member — because rank among non-members is not derivable
    /// from `TopTier`, which holds only the members. On the three signed close paths that
    /// unverifiable choice belongs to the depositor or the Operator; here it would belong to
    /// anyone who noticed a seizure, and what they would be choosing is a seat in the tier that
    /// takes 5% of every ticket.
    ///
    /// Asserted as an absence with its own positive control — the three callers that do fill —
    /// because an absence test over a name that no longer exists anywhere passes forever.
    #[test]
    fn the_permissionless_cleanup_vacates_a_tier_seat_and_never_fills_one() {
        assert_eq!(
            calls(CLOSE_SEIZED_SRC, "fill_vacancy"),
            0,
            "neither `fill_vacancy` nor `fill_vacancy_from_candidate`: a caller who may be \
             anybody must not choose who takes the seat the seizure vacates"
        );
        assert_eq!(
            calls(CLOSE_SEIZED_SRC, "remaining_accounts"),
            0,
            "and with nothing to fill from, the candidate slot is gone too"
        );
        for (label, source) in [
            ("exit/withdraw.rs", WITHDRAW_SRC),
            ("exit/withdraw_core.rs", WITHDRAW_CORE_SRC),
            ("value/record_value.rs", RECORD_VALUE_SRC),
        ] {
            assert_eq!(
                calls(source, "fill_vacancy_from_candidate"),
                1,
                "{label} still fills its own vacancy — without that this absence proves only \
                 that the helper was deleted"
            );
        }
        assert!(
            production(CLOSE_SEIZED_SRC)
                .contains("tier_change = Some((Some(position_key), None));"),
            "the departure is still reported, with no entrant"
        );
    }

    // --- the lock, on one row and on two causes -------------------------------------------------

    /// **The `Active` row's lock gate, driven over every cause in both lock states — sixteen
    /// cells, and the split is the finding.** "It is not a lock bypass a depositor can drive,
    /// because the precondition is that their card is gone" is false on every cause the
    /// depositor can procure. This call is
    /// permissionless so they can make it themselves, and `claim_nft_core` is never lock-gated —
    /// so "have the card frozen, seize your own position, have it thawed, claim" takes the
    /// weight out months before `season.tge_at + 14 days` and hands the card back.
    ///
    /// **`Transferred` is a gated cell, and it is the shortest route rather than an absent
    /// one.** A collection-level
    /// `PermanentTransferDelegate` puts the card directly into the depositor's hands, so the
    /// claim leg disappears from the sequence entirely; mainnet's `CCryptUfe…9CRac` carries one.
    ///
    /// Only `Burned` stays open, and it is not an exemption: the card is destroyed, so there is
    /// nothing to collude for, while the weight is backed by nothing and every block it stays
    /// pays its depositor draw odds and accrual on a card that no longer exists. Delaying that
    /// seizure would reward the depositor rather than restrain them.
    ///
    /// Iterated over `ALL_SEIZURE_KINDS` rather than the causes that motivated the gate, so a
    /// fifth cause is a cell here as well as a decision on
    /// `SeizureKind::depositor_may_keep_the_card` — and the expectation is read from that
    /// predicate rather than restated, because a second copy of the split is a second place it
    /// can be wrong.
    #[test]
    fn the_lock_holds_the_active_row_on_the_causes_that_leave_the_card_in_the_vault() {
        for kind in ALL_SEIZURE_KINDS {
            for (label, now, refused) in [
                ("locked", 499, kind.depositor_may_keep_the_card()),
                ("the boundary", 500, false),
                ("expired", 501, false),
                // A position whose attestor never set a lock: `deposit`/`deposit_core` write
                // `lock_until = 0`, so this is the row every other case in this file runs at.
                ("never locked", 0, false),
            ] {
                let mut pool = pool_holding_one_active();
                let mut position = position_in(PositionState::Active);
                position.lock_until = if label == "never locked" { 0 } else { 500 };
                let mut wallet_stats = wallet_holding_one_active();

                let outcome =
                    apply_close_seized(&mut pool, &mut position, &mut wallet_stats, kind, now);

                if refused {
                    let err = match outcome {
                        Ok(_) => panic!(
                            "a {label} Active position must not be seized on a cause that \
                             leaves the card in the vault — that is the Season-0 lock dissolved \
                             by collusion"
                        ),
                        Err(err) => err,
                    };
                    assert_eq!(error_code(err), 6300, "{label}: PositionLocked");
                    assert!(
                        position.state == PositionState::Active,
                        "{label}: the refusal leaves the state it refused untouched"
                    );
                    assert_eq!(
                        pool.w_real,
                        u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap()),
                        "{label}: and takes no weight out"
                    );
                    assert_eq!(pool.n_real, 1);
                    assert_eq!(wallet_stats.active_positions, 1);
                    assert_eq!(position.accrued, 0, "{label}: and settles no fee leg");
                } else {
                    let effect = outcome.expect("an unlocked or gone-collateral row must close");
                    assert!(effect.departure.is_some());
                    assert!(position.state == PositionState::Seized);
                }
            }
        }
    }

    /// **The `Transferred` route, named end to end rather than left to the table above.**
    ///
    /// The sixteen-cell test reads its expectation *from*
    /// `SeizureKind::depositor_may_keep_the_card`, which is what keeps the two copies of the
    /// split from drifting — and it is exactly why it cannot catch the split being set back the
    /// way it was: flip the predicate and the table flips its expectation with it. This case
    /// states the outcome directly, so the handler's refusal is pinned by something no edit to
    /// the predicate can move.
    ///
    /// The sequence it stands for: a collection carrying a `PermanentTransferDelegate` moves the
    /// escrowed card to a wallet the depositor controls, the depositor calls this permissionless
    /// instruction on their own `Active` position, and the weight leaves months before
    /// `season.tge_at + 14 days`. There is no `claim_nft_core` leg — they already hold the card —
    /// which makes this the shortest of the three collusion routes, not the one that is absent.
    #[test]
    fn a_transferred_card_cannot_take_a_locked_position_out_of_the_pool() {
        let mut pool = pool_holding_one_active();
        let mut position = position_in(PositionState::Active);
        position.lock_until = 500;
        let mut wallet_stats = wallet_holding_one_active();

        let err = match apply_close_seized(
            &mut pool,
            &mut position,
            &mut wallet_stats,
            SeizureKind::Transferred,
            499,
        ) {
            Ok(_) => panic!(
                "a locked Active position was seized on a transferred card — that is the \
                 Season-0 lock dissolved by a collection authority and a depositor, one step \
                 shorter than the frozen route"
            ),
            Err(err) => err,
        };
        assert_eq!(error_code(err), 6300, "PositionLocked");
        assert!(position.state == PositionState::Active);
        assert_eq!(
            pool.w_real,
            u128::from(weight_of(DEFAULT_ADMISSION_FLOOR).unwrap()),
            "and the weight the route existed to extract stays in the pool"
        );

        // The other side of the same row: once the lock has expired the cause is served
        // normally, so this gate delays the cleanup rather than removing it.
        let mut position = position_in(PositionState::Active);
        position.lock_until = 500;
        let effect = apply_close_seized(
            &mut pool,
            &mut position,
            &mut wallet_holding_one_active(),
            SeizureKind::Transferred,
            500,
        )
        .expect("an expired lock must not hold the row");
        assert!(effect.departure.is_some());
        assert!(position.state == PositionState::Seized);
    }

    /// **And the lock reaches none of the three rows that hold no weight, on any cause.** There
    /// is no departure to withhold there, so a gate would only refuse a cleanup for a position
    /// that has nothing to bypass with. Two of them cannot be locked at all in practice —
    /// `deposit`/`deposit_core` write `lock_until = 0` and only `approve_deposit` sets it, and
    /// `record_value` *retains* a below-floor position whose lock still stands rather than
    /// closing it — so this drives a lock they could not reach, which is the stronger case.
    #[test]
    fn the_lock_never_reaches_a_row_that_holds_no_weight() {
        for state in [
            PositionState::Pending,
            PositionState::Rejected,
            PositionState::ClosedBelowFloor,
        ] {
            for kind in ALL_SEIZURE_KINDS {
                let (mut pool, mut wallet_stats) = pool_after_the_closing_sweep();
                let mut position = position_in(state);
                position.lock_until = i64::MAX;

                let effect =
                    apply_close_seized(&mut pool, &mut position, &mut wallet_stats, kind, 0)
                        .expect("a weightless row is closeable however long its lock has to run");
                assert!(effect.departure.is_none());
                assert!(position.state == PositionState::Seized);
            }
        }
    }

    /// The gate reads the *cause*, and it reads it from the classification rather than from an
    /// argument the handler chose — so the lock cannot be sidestepped by a handler that seizes
    /// under one cause and reports another. One `kind`, threaded from the gate into the effect
    /// and into the event.
    #[test]
    fn the_cause_the_lock_reads_is_the_one_the_event_publishes() {
        let handler = flat(&handler_body());
        // Three uses, each asserted as its own form rather than as a count: the handler carries a
        // line comment naming `kind`, so a count measures the prose as well as the code.
        for use_site in [
            "let (kind, owner_observed) = require_collateral_unreachable(",
            "&mut ctx.accounts.wallet_stats, kind, clock.unix_timestamp, )?;",
            "position: position_key, kind, owner_observed,",
        ] {
            assert!(
                handler.contains(use_site),
                "the cause must reach `{use_site}` — bound by the gate, passed to the row that \
                 gates on it, and published by the event, with no fourth reading of it"
            );
        }
        let handler = handler_body();
        assert!(
            handler.find("require_collateral_unreachable(").unwrap()
                < handler.find("apply_close_seized(").unwrap(),
            "the cause must be established before the row that gates on it"
        );
        let code = without_doc_comments(production(CLOSE_SEIZED_SRC));
        assert_eq!(
            code.matches("depositor_may_keep_the_card()").count(),
            1,
            "the split lives on SeizureKind, beside the enum — a second reading of it here is a \
             second place the two causes can be listed wrong"
        );
        assert!(
            code.contains("ByeMachineError::PositionLocked"),
            "and the refusal is the same code every other locked exit returns"
        );
    }

    // --- the absences, each one a decision -------------------------------------------------------

    /// **Ungated but for the lock and the weight freeze, and counted over the code alone** —
    /// this file's prose names every gate it does not carry, and a raw-word count reads that
    /// prose as the gate it denies. `record_value` carries the sweep gate and the weight guard,
    /// and `withdraw_core` reaches the weight guard through the shared accounting, so the
    /// absences here are a comparison rather than a spelling.
    ///
    /// `lock_until` and `assert_weight_open` are **not** on this list — both gate the `Active`
    /// row, the one row that has anything to hold back.
    #[test]
    fn no_pause_flag_and_no_sweep_gate_reaches_the_cleanup() {
        let code = without_doc_comments(production(CLOSE_SEIZED_SRC));
        for gate in [
            "deposits_paused",
            "rolls_paused",
            "claims_paused",
            "apply_withdraw",
            "sweep_pending",
        ] {
            assert_eq!(
                code.matches(gate).count(),
                0,
                "{gate} gates nothing here"
            );
        }
        assert_eq!(
            calls(CLOSE_SEIZED_SRC, "assert_weight_open"),
            1,
            "the Active row must call the weight-freeze guard exactly once"
        );
        let record_value = without_doc_comments(production(RECORD_VALUE_SRC));
        assert!(
            record_value.contains("assert_weight_open") && record_value.contains("sweep_pending"),
            "record_value must still carry both, or the absences above measure nothing"
        );
        assert_eq!(
            calls(WITHDRAW_CORE_SRC, "apply_withdraw"),
            1,
            "and withdraw_core must still reach the guard through the shared accounting"
        );
    }

    /// **No CPI at all, which is what makes "before the CPI" a question this file does not
    /// have.** No release, no transfer, no account close: the classification is a read of two
    /// accounts, and the position and its vault survive every source state so `claim_nft_core`
    /// can serve the three reversible causes.
    ///
    /// **Attempt-and-classify — drive the exit's own release and take MPL Core's refusal as the
    /// cause — is not implementable on chain.** The refusal is not recoverable —
    /// `core-transfer-refused.test.ts` records MPL Core answering
    /// `0x9` to a deny-listed release and the **outer** program failing with the same code, so a
    /// caller reading `release_core_from_vault(..).is_ok()` never reaches its own next line. The
    /// complement is closed by `core_asset.rs`'s plugin census and its `Undecidable` arm instead,
    /// which needs no CPI at all.
    #[test]
    fn no_custody_leg_and_no_account_close_reaches_the_cleanup() {
        for callee in [
            "release_core_from_vault",
            "release_from_vault",
            "transfer_core",
            "transfer_pnft",
            "transfer_spl",
            "transfer",
            "close_account",
            "invoke",
            "invoke_signed",
        ] {
            assert_eq!(
                calls(CLOSE_SEIZED_SRC, callee),
                0,
                "{callee} reaches close_seized — this instruction moves nothing, and a failed \
                 CPI could not be recovered from anyway"
            );
        }
        assert_eq!(
            calls(WITHDRAW_CORE_SRC, "release_core_from_vault"),
            1,
            "withdraw_core must still carry a release, or the absences above measure nothing"
        );
        let accounts = without_doc_comments(accounts_body(CLOSE_SEIZED_SRC));
        assert_eq!(
            accounts.matches("close = ").count(),
            0,
            "no account closes on any source state, so the claim stays owed"
        );
        assert!(
            without_doc_comments(accounts_body(CLAIM_NFT_CORE_SRC)).contains("close = depositor,"),
            "claim_nft_core must still close its position, or the absence above measures nothing"
        );
    }

    /// Nothing is allocated and no `mpl_core_program` or `system_program` slot appears: with no
    /// CPI there is no program to invoke and no rent to pay, and a pinned program account here
    /// would be a CPI somebody planned.
    #[test]
    fn nothing_is_allocated_and_no_program_account_is_carried() {
        let accounts = without_doc_comments(accounts_body(CLOSE_SEIZED_SRC));
        for form in ["init", "zero", "token::", "payer = "] {
            assert_eq!(
                accounts.matches(form).count(),
                0,
                "{form} allocates nothing here"
            );
        }
        for absent in ["mpl_core_program", "system_program", "token_program"] {
            assert_eq!(
                accounts.matches(absent).count(),
                0,
                "{absent} is not this handler's"
            );
        }
    }

    // --- the account bindings ---------------------------------------------------------------

    /// **The position is keyed on the *supplied* pool, and that is the binding that stops a
    /// cross-pool decrement.** `claim_nft_core` keys its position on `position.pool` because it
    /// carries no pool account to disagree with; this handler decrements `Pool` counters, so a
    /// self-anchored position seed would let a caller pair one pool's position with another
    /// pool's counters. Every other account is bound through the same `pool`.
    #[test]
    fn every_account_is_bound_through_the_supplied_pool() {
        let accounts = accounts_body(CLOSE_SEIZED_SRC);
        assert!(accounts
            .contains("seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],"));
        assert!(accounts.contains("seeds = [TOP_TIER_SEED, pool.key().as_ref()],"));
        assert!(accounts.contains(
            "seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()],"
        ));
        assert!(accounts.contains("#[account(mut, address = pool.weight_index)]"));
        assert!(
            accounts_body(CLAIM_NFT_CORE_SRC).contains(
                "seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],"
            ),
            "the claim must still be the self-anchored one, or the contrast above is stale"
        );
    }

    /// The standard and the four source states, on the accounts struct, and nothing else. The
    /// constraint count pins the absence of a third: this instruction is permissionless, so a
    /// signer constraint added here would be a gate it cannot have.
    ///
    /// **`Seized` is the one state on the far side of the guard**, and the split is not a
    /// preference: every other state has the card in the vault, and every sibling that refuses a
    /// burned or frozen card sends its caller here. `Seized` is this instruction's own terminal,
    /// so admitting it would let a second call re-emit `PositionSeized` for a position that has
    /// already left the pool.
    #[test]
    fn the_standard_and_the_four_source_states_are_asserted_and_nothing_else_is() {
        let accounts = accounts_body(CLOSE_SEIZED_SRC);
        assert!(accounts.contains(
            "constraint = position.standard == STANDARD_CORE\n            @ ByeMachineError::WrongStandardForInstruction,"
        ));
        assert!(accounts.contains(
            "constraint = matches!(\n            position.state,\n            PositionState::Pending\n                | PositionState::Active\n                | PositionState::ClosedBelowFloor\n                | PositionState::Rejected\n        ) @ ByeMachineError::InvalidPositionState"
        ));
        assert_eq!(accounts.matches("constraint = ").count(), 2);

        let admitted = [
            PositionState::Pending,
            PositionState::Active,
            PositionState::ClosedBelowFloor,
            PositionState::Rejected,
        ];
        for state in ALL_POSITION_STATES {
            let name = state_name(state);
            assert_eq!(
                accounts.contains(&format!("PositionState::{name}")),
                admitted.contains(&state),
                "PositionState::{name} is on the wrong side of this instruction's state guard"
            );
        }
    }

    /// The asset is pinned to the position's own card and is **read-only** — the pin the
    /// collateral read cannot make for itself, and the absence of `mut` is the claim that this
    /// instruction never moves what it reads.
    #[test]
    fn the_asset_is_pinned_to_the_positions_own_card_and_is_never_writable() {
        assert!(accounts_body(CLOSE_SEIZED_SRC).contains(
            "#[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]\n    pub asset: UncheckedAccount<'info>,"
        ));
        assert!(
            accounts_body(CLAIM_NFT_CORE_SRC).contains(
                "#[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]"
            ),
            "the claim must still declare its asset writable — it transfers, and this one does \
             not"
        );
    }

    /// The vault is an address the read compares against, signed by nobody: no seeds are ever
    /// signed under here, because there is no CPI to sign for.
    #[test]
    fn the_vault_is_an_address_to_compare_against_and_never_a_signer() {
        let accounts = accounts_body(CLOSE_SEIZED_SRC);
        assert!(accounts.contains(
            "#[account(\n        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],\n        bump = position.vault_bump\n    )]\n    pub position_vault: UncheckedAccount<'info>,"
        ));
        assert_eq!(
            without_doc_comments(accounts)
                .matches("POSITION_VAULT_SEED")
                .count(),
            1,
            "the vault seed appears once, in the derivation"
        );
        assert_eq!(
            handler_body().matches("POSITION_VAULT_SEED").count(),
            0,
            "and never in the handler — a use there would be seeds signed under, which is a CPI"
        );
    }

    /// Permissionless: one signer, checked against nothing, and named for what it does rather
    /// than for what the crate's other two permissionless handlers do. Those fund an MPL Core
    /// account creation; this one makes no CPI, so there is no payment for `payer` to describe.
    #[test]
    fn the_sole_signer_is_unchecked_and_is_not_a_payer() {
        let accounts = accounts_body(CLOSE_SEIZED_SRC);
        assert!(accounts.contains("pub caller: Signer<'info>,"));
        assert_eq!(accounts.matches(": Signer<'info>").count(), 1);
        assert_eq!(
            without_doc_comments(accounts).matches("caller").count(),
            1,
            "the signer is named once and constrained nowhere — a constraint on it would be an \
             authority this instruction cannot have"
        );
        assert_eq!(
            without_doc_comments(production(CLOSE_SEIZED_SRC))
                .matches("ctx.accounts.caller")
                .count(),
            0,
            "and the handler never reads it"
        );
    }
}
