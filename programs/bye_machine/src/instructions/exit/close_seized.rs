//! The cleanup instruction for a position whose escrowed Core card a third party has taken,
//! burned or frozen in place.
//!
//! It lives in `exit/` but is not an exit: it moves no custody, makes no CPI, closes no account
//! and pays nobody. What it shares with this domain is the terminal it writes: `claim_nft_core`
//! and `withdraw_core` reject with `CollateralAbsent` (6307) or `CollateralFrozen` (6309) naming
//! this instruction, and `Seized` is the state `claim_nft_core` then serves.
//!
//! **Operator-signed, and the asset account is still the authority.** The program reads the asset
//! itself, so the collateral gate is the authorisation and runs first: a card the census finds
//! clear gets `CollateralPresent` (6308) whoever signs. The signature only bounds who may act on
//! that reading; a depositor stranded by a sibling's `63xx` waits for the custody monitor.
//!
//! **`Undecidable` is the fourth cause.** MPL Core can refuse a `Transfer` through mechanisms this
//! program cannot enumerate (a collection-level freeze, a detached collection, a `Royalties`
//! `ProgramDenyList`, external `Oracle` / `LifecycleHook` adapters). Enumerating them would leave
//! a position with live weight behind a card nobody can move, which is a pool halt, so
//! `core_asset.rs` censuses the plugins that provably cannot gate a transfer and answers
//! `Undecidable` for the rest. Both gates admit it: the exits proceed and MPL Core decides, and
//! this instruction accepts it as a cause. The cost is that a position whose card MPL Core would
//! still move can be closed (weight out, state `Seized`, card recoverable via `claim_nft_core`);
//! the depositor loses the position and its future earnings, not the balance already settled onto
//! `accrued`.
//!
//! **Lock gate on the `Active` row.** The Operator gate does not stop a depositor arranging the
//! seizure: the custody monitor closes a procured seizure as a matter of course. A depositor
//! whose collection authority will act on request could freeze, seize their own position, thaw
//! and claim, taking the weight out long before `season.tge_at + 14 days`. So the `Active` row
//! refuses with `PositionLocked` (6300) while the lock stands, on every cause where the depositor
//! can end up with the card (`SeizureKind::depositor_may_keep_the_card`, beside the enum; a new
//! cause must choose a side there). `Transferred` is gated too: a collection-level
//! `PermanentTransferDelegate` (mainnet's `CCryptUfe…9CRac` carries one) puts the card straight
//! into the depositor's hands, and narrowing the row to "new owner is the depositor" would not
//! help since a colluding depositor names any other wallet. The accepted cost is that a genuinely
//! stolen card keeps its weight in the pool until the lock expires.
//!
//! `Burned` stays ungated: nothing is left to collude for, and the weight is backed by nothing,
//! so delaying the seizure would reward the depositor. The three rows that hold no weight are
//! ungated on every cause, since there is no departure to withhold.
//!
//! **Weight-freeze guard.** The `Active` row runs `assert_weight_open` before `depart_weight`,
//! like every instruction that moves `w_real`/`n_real`/`pool.blanks`: a committed roll batch
//! resolves its VRF draw against a fixed weight domain.
//!
//! **Two effect sets.** Only `Active` holds weight; `Pending`, `Rejected` and `ClosedBelowFloor`
//! hold none. Applying `Active`'s effects to `ClosedBelowFloor` would decrement `n_real` and
//! `WalletStats` twice and push an already-freed Fenwick slot onto the free stack again, handing
//! one leaf to two future positions. The weight half is therefore an `Option<WeightDeparture>`:
//! the non-weight rows carry no slot index to remove.

use anchor_lang::prelude::*;

use crate::common::constants::BLANK_SLOTS;
use crate::common::core_asset::require_collateral_unreachable;
use crate::common::errors::ByeMachineError;
use crate::common::events::{PositionSeized, RebalanceEvaluated, SeizureKind, TierChanged};
use crate::common::guards::assert_weight_open;
use crate::common::math::{position_fee_payout, weight_of};
use crate::common::rebalance;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED,
    WALLET_STATS_SEED,
};
use crate::common::standards::STANDARD_CORE;
use crate::common::tier;
use crate::state::{
    Pool, Position, PositionState, ProtocolConfig, TopTier, WalletStats, WeightIndex,
};

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
    blanks_before: [u32; BLANK_SLOTS],
    was_member: bool,
}

/// What the handler needs back: the source state the event publishes (the only way an indexer
/// can tell which effect set ran), the value it publishes alongside, and the weight
/// departure on the `Active` row alone.
///
/// No test-only `Debug`: `PositionState`'s derive list is a serialization contract.
struct SeizeEffect {
    source_state: PositionState,
    recorded_value: u64,
    departure: Option<WeightDeparture>,
}

/// The `Active` row's accounting: the equal accumulator leg settled **onto** the position, the
/// four checked decrements, and the rebalance evaluation.
///
/// Unlike `apply_withdraw`, `pool.owed_fees` is not decremented because nothing is paid: the fee
/// delta settles into `Position.accrued`, a debt earned while active that neither the seizure nor
/// the closure reaches. Releasing it would turn a depositor's earned fees into protocol revenue.
/// No instruction pays it out on this path.
///
/// The tier leg is not settled here: `tier::vacate` folds it into `accrued` and re-stamps
/// `tier_checkpoint` itself, so settling it here too would pay it twice.
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
    pool.blanks = rebalance::evaluate(
        pool.n_real,
        pool.w_real,
        pool.blank_faces,
        pool.ticket_target,
    )?;

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
/// The match is wildcard-free so a new `PositionState` fails to compile here; the one
/// inadmissible state errors with `InvalidPositionState` (6301), as the accounts constraint does.
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
        PositionState::Active => {
            // Lock gate only on causes where the depositor can end up with the card; the module
            // doc explains why the Operator signature does not close that route.
            if kind.depositor_may_keep_the_card() {
                require!(now >= position.lock_until, ByeMachineError::PositionLocked);
            }
            // Only this row departs weight, so only it waits for a committed roll batch to close.
            assert_weight_open(pool)?;
            Some(depart_weight(pool, position, wallet_stats)?)
        }
        // No weight to depart: `Pending` was never activated (no counters or checkpoint to
        // settle), `Rejected` is a `Pending` the operator refused and is admitted because
        // `return_rejected_core` strands it here, and `ClosedBelowFloor` was already fully
        // decremented by `record_value`.
        PositionState::Pending | PositionState::Rejected | PositionState::ClosedBelowFloor => None,
        PositionState::Seized => return Err(ByeMachineError::InvalidPositionState.into()),
    };

    position.state = PositionState::Seized;

    Ok(SeizeEffect {
        source_state,
        recorded_value,
        departure,
    })
}

/// Plain `Context`: there are no `remaining_accounts` to read, since no tier vacancy is filled.
pub fn handler(ctx: Context<CloseSeized>) -> Result<()> {
    // First, before anything is written: this is the authorisation. It censuses both plugin
    // registries (the card's and the collection's) and answers `Undecidable` for anything it
    // cannot decide, which the exits also admit.
    let (kind, owner_observed) = require_collateral_unreachable(
        &ctx.accounts.asset,
        &ctx.accounts.collection,
        &ctx.accounts.position_vault.key(),
    )?;

    let clock = Clock::get()?;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let acc_tier = ctx.accounts.top_tier.acc_tier;

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
            // This instruction vacates the seat and does not fill it: the next-ranked non-member
            // is not knowable from any account here, and choosing who joins the tier that takes
            // 5% of every ticket during someone else's forced closure is not its call.
            // `update_tier` fills it.
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

/// `operator` signs for the transaction and the authority check on `protocol_config` and nothing
/// else; it is not a `payer` because this instruction allocates and closes nothing.
///
/// The account set is `record_value`'s close path, not `claim_nft`'s. Every account is bound
/// through `pool` (the position's seeds take `pool.key()`, not `position.pool`), so a caller
/// cannot pair one pool's position with another pool's counters.
///
/// **No `close =` on any source state**: a seizure is reversible on three of its four causes
/// (freeze, transfer-out, undecidable plugin), and once the authority undoes it `claim_nft_core`
/// needs `depositor` and `vault_bump` to still exist to sign with.
///
/// On the burn cause the claim is owed to nobody: `claim_nft_core` refuses with
/// `CollateralAbsent` (6307) forever and the position's rent stays locked. No instruction closes
/// the position or reclaims that rent.
#[derive(Accounts)]
pub struct CloseSeized<'info> {
    pub operator: Signer<'info>,

    /// Operator-signed: a seizure is a cleanup of somebody else's act, and letting anyone record
    /// it would hand a stranger the timing of a position's closure. The signature bounds who may
    /// act on the asset's reading, not what the reading says: a healthy card is refused whoever
    /// asks.
    ///
    /// Cost: a depositor stranded at `6307`/`6309` by a sibling cannot run this themselves and
    /// waits on the Operator, and the phantom leaf persists for the monitor's polling interval
    /// plus its response time.
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
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        // MPL Core only; checked here so it runs before the collateral read.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction,
        // Every state whose card is still in the vault, i.e. all but `Seized`. A sibling that
        // refuses a burned or frozen card returns 6307/6309 naming this instruction, so any
        // state a sibling can strand must be admitted here; `ClosedBelowFloor` is
        // `claim_nft_core`'s and `Rejected` is `return_rejected_core`'s.
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
    /// **Pinned in the classification rather than here.** `Position` stores no collection, and
    /// the asset's own `update_authority` is not readable at account-validation time, so
    /// `read_collateral` requires this account to be exactly the asset's collection before
    /// walking the registry; that stops a caller naming any frozen collection to seize a healthy
    /// position. Never `mut`: read only, and no CPI.
    ///
    /// The census walks this registry as well as the asset's, so it is also where a rejecting
    /// `Royalties` rule set or an external adapter declaring a Transfer check can hide, which
    /// makes the classification `Undecidable`. A `Royalties` record whose rule set is `None`
    /// hides nothing and is `Clear`.
    pub collection: UncheckedAccount<'info>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        ACC_PRECISION, DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };
    use crate::common::events::ALL_SEIZURE_KINDS;
    use crate::common::test_support::{accounts_body, calls, crate_sources, flat, production};
    use crate::state::position::ALL_POSITION_STATES;
    use crate::state::{TierEntry, TopTier};

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

    /// The handler's own body: production source with doc comments stripped, bounded by its
    /// signature and the accounts struct that follows.
    fn handler_body() -> String {
        let code = production(CLOSE_SEIZED_SRC);
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

    /// Defaults to `Frozen` so `Active` cases run through the lock gate (a `Burned` default would
    /// skip it); `now` is past the fixture's `lock_until` of 0.
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

    /// The pool after `record_value` closed the position below the floor: leaf gone, aggregate
    /// zero, counters already decremented.
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

    /// The `Active` row: the full effect set, asserted as movements so the other rows' tests
    /// assert a comparison rather than an initial value.
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

    /// The `Pending` row moves the state only; its counters are incremented at activation, not
    /// at `deposit`.
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

    /// The `ClosedBelowFloor` row records the cause only; every decrement already ran in the
    /// closing sweep.
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

    /// The `Active` effect set applied to a position the closing sweep already accounted for
    /// fails closed on the first decrement (`n_real` under-runs).
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

    /// A closed position's `slot_index` still names the freed leaf; having no `WeightDeparture`
    /// on this row is what stops a second push onto the free stack.
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

    /// All four admitted source states reach `Seized`; the inadmissible one is refused with the
    /// accounts-struct code. Iterates every `PositionState` variant, so a new one needs a
    /// decision.
    ///
    /// `Rejected` is admitted because `return_rejected_core` refuses a burned or frozen card
    /// with 6307/6309 naming this instruction; refusing it here would leave it no terminal.
    #[test]
    fn every_admitted_source_state_terminates_at_seized_and_no_other_state_is_admitted() {
        let admitted = [
            PositionState::Pending,
            PositionState::Active,
            PositionState::ClosedBelowFloor,
            PositionState::Rejected,
        ];
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
                // `SeizeEffect` has no `Debug`, so `unwrap_err` is unavailable.
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

    /// Every statement in `source` that assigns to `owed_fees`, whitespace removed. Statement-
    /// scoped because the settlement's decrement is chained behind its accrual, so a bare
    /// `owed_fees.checked_sub` substring would miss it.
    fn owed_fees_assignments(source: &str) -> Vec<String> {
        let packed: String = production(source).split_whitespace().collect();
        packed
            .split("owed_fees=")
            .skip(1)
            .map(|rest| rest.split(';').next().unwrap_or("").to_string())
            .collect()
    }

    // --- the fee legs, settled onto the position and left owed ---------------------------------

    /// Both fee legs fold onto `Position.accrued` and `pool.owed_fees` is left alone: the balance
    /// is a debt earned while active, and the reserve may only shrink where the same instruction
    /// pays it out (as `apply_withdraw` does). Where it is finally paid on this path is an open
    /// product question.
    #[test]
    fn the_fee_legs_settle_onto_the_position_and_the_reserve_keeps_carrying_them() {
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
            "the reserve still owes what the position earned — the seizure took the card, not \
             the debt"
        );

        // Nothing in this file may shrink the reserve, on any source state.
        assert!(
            owed_fees_assignments(CLOSE_SEIZED_SRC).is_empty(),
            "close_seized writes owed_fees at all — an earned fee balance is owed regardless of \
             what closed the position, and nothing here pays it"
        );

        assert_eq!(
            calls(EXIT_ACCOUNTING_SRC, "checked_sub"),
            5,
            "apply_withdraw's five checked_sub calls include an owed_fees discharge this \
             function deliberately omits — there it is paired with a transfer to the depositor, \
             which is what makes the decrement legitimate"
        );
    }

    /// `owed_fees` may only shrink where the same instruction pays what it removes: a decrement
    /// with no matching transfer cancels an earned balance. An allowlist rather than a ban, so a
    /// new paying site is added deliberately with its payout named: `exit_accounting.rs`
    /// (`apply_withdraw`), `settle_roll.rs` (the drawn position's `delta`) and `claim_fee.rs`
    /// (`apply_claim_fee` and `pay_closed_accrued`, whose every caller transfers what it returns).
    #[test]
    fn every_owed_fees_decrement_in_the_crate_is_paired_with_a_payout() {
        const PAYING_SITES: [&str; 3] = ["exit_accounting.rs", "settle_roll.rs", "claim_fee.rs"];
        let mut found = 0usize;

        for path in crate_sources() {
            let label = path.display().to_string();
            let source = std::fs::read_to_string(&path).unwrap();
            let decrements = owed_fees_assignments(&source)
                .iter()
                .any(|stmt| stmt.contains("checked_sub") || stmt.contains('-'));
            if !decrements {
                continue;
            }
            found += 1;
            assert!(
                PAYING_SITES
                    .iter()
                    .any(|site| label.ends_with(&format!("/{site}"))),
                "{label} decrements owed_fees and is not a site that pays what it removes — name \
                 the transfer that discharges the claim, then add the file here. A decrement \
                 without a payout extinguishes a debt the depositor earned while active."
            );
        }

        // Positive control: an allowlist whose matcher stopped matching would pass every file.
        assert_eq!(
            found,
            PAYING_SITES.len(),
            "the decrement matcher found {found} sites, not the {} the allowlist names — either \
             a payout was removed, or the matcher stopped matching and this is now passing \
             everything",
            PAYING_SITES.len()
        );
    }

    /// A `Pending` position never had a checkpoint stamped, so settling a fee leg on it would
    /// credit it the pool's entire fee history (5 rather than 0 here).
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

    /// `accrued` is folded in exactly once per leg: this function settles the equal leg and
    /// `tier::vacate` the tier leg (2 + 5 + 7 = 14, not a double-counted 19).
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

    /// The `Active` row's event publishes the blank counts from before its own rebalance.
    #[test]
    fn the_active_row_reports_the_blanks_from_before_its_own_rebalance() {
        let mut pool = pool_holding_one_active();
        pool.blanks = [1, 2, 3, 0, 0, 0, 0, 0];
        let mut position = position_in(PositionState::Active);
        let mut wallet_stats = wallet_holding_one_active();

        let effect = seize(&mut pool, &mut position, &mut wallet_stats).unwrap();

        let departure = effect.departure.unwrap();
        assert_eq!(departure.blanks_before, [1, 2, 3, 0, 0, 0, 0, 0]);
        assert_eq!(
            pool.blanks, [0; 8],
            "n_real reached 0, so the evaluation zeroes the blank set — and the before/after \
             pair the event publishes are two different values"
        );
    }

    // --- the gate, first and inverse ------------------------------------------------------------

    /// The gate runs before `apply_close_seized`, hence before any state write, and is the
    /// inverse of the exits' gate; `core_asset.rs` owns the classification and the codes.
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

    /// The cause and owner reach the event from the read that authorised the call, not from a
    /// second read.
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
        let code = production(CLOSE_SEIZED_SRC);
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

    /// The removal must sit inside the departure branch and before `tier::vacate`; an
    /// unconditional removal (reading `position.slot_index` directly) lands outside that window
    /// and fails an ordering assert.
    #[test]
    fn the_leaf_removal_is_reached_only_through_the_departure_and_only_once() {
        let code = production(CLOSE_SEIZED_SRC);
        assert_eq!(code.matches(".remove(").count(), 1);
        assert_eq!(calls(CLOSE_SEIZED_SRC, "vacate"), 1);
        assert!(
            code.find("if let Some(departure) = &effect.departure {")
                .unwrap()
                < code.find(".remove(").unwrap()
        );
        assert!(code.find(".remove(").unwrap() < code.find("tier::vacate(").unwrap());
    }

    /// This instruction vacates a tier seat and never fills one: the next-ranked non-member is
    /// not derivable from `TopTier`, so a forced closure must not choose the entrant. Asserted as
    /// an absence with a positive control (the three callers that do fill).
    #[test]
    fn the_seizure_cleanup_vacates_a_tier_seat_and_never_fills_one() {
        assert_eq!(
            calls(CLOSE_SEIZED_SRC, "fill_vacancy"),
            0,
            "neither `fill_vacancy` nor `fill_vacancy_from_candidate`: a forced closure must \
             not choose who takes the seat it vacates"
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

    /// The `Active` row's lock gate, driven over every cause in both lock states. The refusal set
    /// is read from `SeizureKind::depositor_may_keep_the_card` rather than restated, and the loop
    /// covers `ALL_SEIZURE_KINDS` so a new cause becomes a cell here. Only `Burned` stays open:
    /// the card is destroyed, so there is nothing to collude for.
    #[test]
    fn the_lock_holds_the_active_row_on_the_causes_that_leave_the_card_in_the_vault() {
        for kind in ALL_SEIZURE_KINDS {
            for (label, now, refused) in [
                ("locked", 499, kind.depositor_may_keep_the_card()),
                ("the boundary", 500, false),
                ("expired", 501, false),
                // `deposit`/`deposit_core` write `lock_until = 0`.
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

    /// The `Transferred` route stated directly: the table test reads its expectation from the
    /// predicate, so it cannot catch the split being flipped; this case can.
    ///
    /// A collection `PermanentTransferDelegate` moves the escrowed card to a depositor-controlled
    /// wallet and the monitor submits this instruction; the weight must not leave before the
    /// lock expires.
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

    /// The lock never reaches the three rows that hold no weight, on any cause: there is no
    /// departure to withhold.
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

    /// The lock reads the cause from the gate's classification: one `kind`, threaded from the
    /// gate into the effect and the event.
    #[test]
    fn the_cause_the_lock_reads_is_the_one_the_event_publishes() {
        let handler = flat(&handler_body());
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
        let code = production(CLOSE_SEIZED_SRC);
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

    /// Ungated but for the lock and the weight freeze, counted over code only. `lock_until` and
    /// `assert_weight_open` gate the `Active` row, the one with something to hold back.
    #[test]
    fn no_pause_flag_and_no_sweep_gate_reaches_the_cleanup() {
        let code = production(CLOSE_SEIZED_SRC);
        for gate in [
            "deposits_paused",
            "rolls_paused",
            "claims_paused",
            "apply_withdraw",
            "sweep_pending",
        ] {
            assert_eq!(code.matches(gate).count(), 0, "{gate} gates nothing here");
        }
        assert_eq!(
            calls(CLOSE_SEIZED_SRC, "assert_weight_open"),
            1,
            "the Active row must call the weight-freeze guard exactly once"
        );
        let record_value = production(RECORD_VALUE_SRC);
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

    /// No CPI at all: no release, transfer or account close. The position and vault survive
    /// every source state so `claim_nft_core` can serve the reversible causes.
    ///
    /// Attempt-and-classify is not implementable on chain: MPL Core's refusal aborts the outer
    /// program with the same code, so `release_core_from_vault(..).is_ok()` never reaches the
    /// next line. The plugin census and `Undecidable` arm in `core_asset.rs` close the
    /// complement without a CPI.
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
        let accounts = accounts_body(CLOSE_SEIZED_SRC);
        assert_eq!(
            accounts.matches("close = ").count(),
            0,
            "no account closes on any source state, so the claim stays owed"
        );
        assert!(
            accounts_body(CLAIM_NFT_CORE_SRC).contains("close = depositor,"),
            "claim_nft_core must still close its position, or the absence above measures nothing"
        );
    }

    /// No allocation and no program account: with no CPI there is no program to invoke and no
    /// rent to pay.
    #[test]
    fn nothing_is_allocated_and_no_program_account_is_carried() {
        let accounts = accounts_body(CLOSE_SEIZED_SRC);
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

    /// The position is keyed on the supplied pool, which stops a cross-pool decrement. Every other
    /// account is bound through the same `pool`.
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
    }

    /// The standard and the four source states on `position`, plus the Operator check; the
    /// constraint count pins the absence of a fourth. `Seized` is excluded because admitting it
    /// would let a second call re-emit `PositionSeized`.
    #[test]
    fn the_standard_and_the_four_source_states_are_asserted_and_nothing_else_is() {
        let accounts = accounts_body(CLOSE_SEIZED_SRC);
        assert!(accounts.contains(
            "constraint = position.standard == STANDARD_CORE\n            @ ByeMachineError::WrongStandardForInstruction,"
        ));
        assert!(accounts.contains(
            "constraint = matches!(\n            position.state,\n            PositionState::Pending\n                | PositionState::Active\n                | PositionState::ClosedBelowFloor\n                | PositionState::Rejected\n        ) @ ByeMachineError::InvalidPositionState"
        ));
        assert_eq!(
            accounts.matches("constraint = ").count(),
            3,
            "these two on `position`, plus the Operator check on `protocol_config`"
        );

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

    /// The asset is pinned to the position's own card and is read-only.
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
            accounts.matches("POSITION_VAULT_SEED").count(),
            1,
            "the vault seed appears once, in the derivation"
        );
        assert_eq!(
            handler_body().matches("POSITION_VAULT_SEED").count(),
            0,
            "and never in the handler — a use there would be seeds signed under, which is a CPI"
        );
    }

    /// The sole signer is the Operator, a gate on who may act rather than on what is true: the
    /// asset account still authorises the close. It is not a payer, since nothing is allocated,
    /// closed or sent by CPI, so the signer is never `mut`.
    #[test]
    fn the_sole_signer_is_the_operator_and_is_not_a_payer() {
        let accounts = accounts_body(CLOSE_SEIZED_SRC);
        assert!(accounts.contains("pub operator: Signer<'info>,"));
        assert_eq!(accounts.matches(": Signer<'info>").count(), 1);
        assert!(
            accounts.contains(
                "constraint = operator.key() == protocol_config.operator \
                 @ ByeMachineError::Unauthorized"
            ),
            "the Operator check is on the accounts struct, so it runs before the collateral read"
        );
        assert!(
            !accounts.contains("mut,\n    pub operator")
                && !accounts.contains("#[account(mut)]\n    pub operator"),
            "the signer funds nothing and is never writable"
        );
        assert_eq!(
            production(CLOSE_SEIZED_SRC)
                .matches("ctx.accounts.operator")
                .count(),
            0,
            "and the handler never reads it — the constraint is the whole of its use"
        );
    }
}
