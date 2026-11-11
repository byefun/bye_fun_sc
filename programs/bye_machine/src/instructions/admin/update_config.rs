use anchor_lang::prelude::*;

use crate::common::constants::{BLANK_SLOTS, MAX_BATCH_CAPACITY};
use crate::common::errors::ByeMachineError;
use crate::common::events::{ConfigChanged, ConfigValue, RebalanceEvaluated, TierChanged};
use crate::common::guards::{assert_weight_open, validate_blank_faces};
use crate::common::math::position_fee_payout;
use crate::common::rebalance;
use crate::common::seeds::{POOL_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED};
use crate::common::tier;
use crate::state::{Pool, Position, ProtocolConfig, TopTier};

const MAX_TIER_SIZE: u8 = 20;
const MAX_BPS: u16 = 10_000;

/// One configuration move. Independent parameters are scalar variants; the three coupled
/// surfaces are composites validated as a unit, since `fee == price - ticket_target`, the
/// 10,000 allocation sum and the face array cannot be satisfied one parameter at a time.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub enum ConfigUpdate {
    AdmissionFloor(u64),
    AdmissionCeiling(u64),
    WalletValueCap(u64),
    MaxN(u8),
    BuybackRateBps(u16),
    /// Refused: Treasury 04's ceiling is protocol-wide, set by `set_t04_ceiling`. The variant
    /// stays so every later variant keeps its wire tag.
    T04Ceiling(u64),
    SweepCadenceHours(u16),
    TierSize(u8),
    Pricing {
        price: u64,
        ticket_target: u64,
        fee: u64,
    },
    Allocation {
        equal_bps: u16,
        tier_bps: u16,
        protocol_bps: u16,
    },
    BlankFaces([u64; BLANK_SLOTS]),
    EscrowBidBps(u16),
}

pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, UpdateConfig<'info>>,
    update: ConfigUpdate,
) -> Result<()> {
    validate(&effective_config(&ctx.accounts.pool, &update))?;
    validate_offer_and_bid(&ctx.accounts.pool, &update)?;

    if requires_weight_freeze(&ctx.accounts.pool, &update) {
        assert_weight_open(&ctx.accounts.pool)?;
    }

    let changes = planned_changes(&ctx.accounts.pool, &update);
    let rebalances = retargets_blanks(&ctx.accounts.pool, &update);
    let pool_key = ctx.accounts.pool.key();
    let slot = Clock::get()?.slot;
    let authority = ctx.accounts.administrator.key();
    let pool = &mut ctx.accounts.pool;

    match update {
        ConfigUpdate::AdmissionFloor(value) => pool.admission_floor = value,
        ConfigUpdate::AdmissionCeiling(value) => pool.admission_ceiling = value,
        ConfigUpdate::WalletValueCap(value) => pool.wallet_value_cap = value,
        ConfigUpdate::MaxN(value) => pool.max_n = value,
        ConfigUpdate::BuybackRateBps(value) => pool.buyback_rate_bps = value,
        ConfigUpdate::T04Ceiling(_) => return err!(ByeMachineError::T04CeilingIsProtocolWide),
        ConfigUpdate::SweepCadenceHours(value) => pool.sweep_cadence_hours = value,
        ConfigUpdate::BlankFaces(faces) => pool.blank_faces = faces,
        ConfigUpdate::EscrowBidBps(value) => pool.escrow_bid_bps = value,
        ConfigUpdate::Pricing {
            price,
            ticket_target,
            fee,
        } => {
            pool.price = price;
            pool.ticket_target = ticket_target;
            pool.fee = fee;
        }
        ConfigUpdate::Allocation {
            equal_bps,
            tier_bps,
            protocol_bps,
        } => {
            pool.alloc_equal_bps = equal_bps;
            pool.alloc_tier_bps = tier_bps;
            pool.alloc_protocol_bps = protocol_bps;
        }
        ConfigUpdate::TierSize(value) => {
            pool.tier_size = value;
            demote_shrink_excess(
                &mut ctx.accounts.top_tier,
                ctx.remaining_accounts,
                value,
                pool_key,
                slot,
            )?;
        }
    }

    for (param, old, new) in changes {
        emit!(ConfigChanged {
            pool: pool_key,
            slot,
            authority,
            param: param.to_string(),
            old,
            new,
        });
    }

    if rebalances {
        let pool = &mut ctx.accounts.pool;
        let blanks_before = reevaluate_blanks(pool)?;
        emit!(RebalanceEvaluated {
            pool: pool_key,
            slot,
            n_real: pool.n_real,
            w_real: pool.w_real,
            blanks_before,
            blanks_after: pool.blanks,
        });
    }

    Ok(())
}

/// The weight-freeze call sites in this instruction. `BlankFaces` and a `Pricing` move of
/// `ticket_target` re-target the controller and so move `W`; a `Pricing` move of `price`/`fee`
/// alone is not gated, because a committed batch snapshots both.
///
/// `TierSize` is gated for liveness: `settle_roll` requires `top_tier.len == min(pool.tier_size,
/// pool.n_real)` and `demote_shrink_excess` only shrinks, so raising it mid-batch makes every
/// remaining roll revert `TierNotFull` and locks the pool behind `RollBatchInFlight`.
fn requires_weight_freeze(pool: &Pool, update: &ConfigUpdate) -> bool {
    retargets_blanks(pool, update)
        || matches!(*update, ConfigUpdate::TierSize(value) if value != pool.tier_size)
}

/// The updates that change a controller input — the faces, or the ticket — so the blank set is
/// re-evaluated in the same instruction rather than left sized for config that no longer holds:
/// a face zeroed under a live count would otherwise keep that count until the next rebalance.
/// Both are weight-frozen, so no open batch sees the new counts.
fn retargets_blanks(pool: &Pool, update: &ConfigUpdate) -> bool {
    match *update {
        ConfigUpdate::BlankFaces(_) => true,
        ConfigUpdate::Pricing { ticket_target, .. } => ticket_target != pool.ticket_target,
        _ => false,
    }
}

/// Re-sizes the blank set against the pool's current faces and ticket, returning the counts it
/// replaced.
fn reevaluate_blanks(pool: &mut Pool) -> Result<[u32; BLANK_SLOTS]> {
    let blanks_before = pool.blanks;
    pool.blanks = rebalance::evaluate(
        pool.n_real,
        pool.w_real,
        pool.blank_faces,
        pool.ticket_target,
    )?;
    Ok(blanks_before)
}

/// The guarded parameter surface, merged from the stored `Pool` and the submitted update, so
/// every cross-parameter guard rejects on whichever side of it changed.
struct EffectiveConfig {
    price: u64,
    ticket_target: u64,
    fee: u64,
    alloc_equal_bps: u16,
    alloc_tier_bps: u16,
    alloc_protocol_bps: u16,
    admission_floor: u64,
    blank_faces: [u64; BLANK_SLOTS],
    max_n: u8,
    tier_size: u8,
}

fn effective_config(pool: &Pool, update: &ConfigUpdate) -> EffectiveConfig {
    let mut config = EffectiveConfig {
        price: pool.price,
        ticket_target: pool.ticket_target,
        fee: pool.fee,
        alloc_equal_bps: pool.alloc_equal_bps,
        alloc_tier_bps: pool.alloc_tier_bps,
        alloc_protocol_bps: pool.alloc_protocol_bps,
        admission_floor: pool.admission_floor,
        blank_faces: pool.blank_faces,
        max_n: pool.max_n,
        tier_size: pool.tier_size,
    };

    match *update {
        ConfigUpdate::AdmissionFloor(value) => config.admission_floor = value,
        ConfigUpdate::MaxN(value) => config.max_n = value,
        ConfigUpdate::TierSize(value) => config.tier_size = value,
        ConfigUpdate::BlankFaces(faces) => config.blank_faces = faces,
        ConfigUpdate::Pricing {
            price,
            ticket_target,
            fee,
        } => {
            config.price = price;
            config.ticket_target = ticket_target;
            config.fee = fee;
        }
        ConfigUpdate::Allocation {
            equal_bps,
            tier_bps,
            protocol_bps,
        } => {
            config.alloc_equal_bps = equal_bps;
            config.alloc_tier_bps = tier_bps;
            config.alloc_protocol_bps = protocol_bps;
        }
        _ => {}
    }

    config
}

fn validate(config: &EffectiveConfig) -> Result<()> {
    validate_blank_faces(
        config.ticket_target,
        config.admission_floor,
        &config.blank_faces,
    )?;

    let alloc_sum = u32::from(config.alloc_equal_bps)
        + u32::from(config.alloc_tier_bps)
        + u32::from(config.alloc_protocol_bps);
    require_eq!(
        alloc_sum,
        10_000,
        ByeMachineError::AllocationBpsNotFullyAllocated
    );

    let expected_fee = config
        .price
        .checked_sub(config.ticket_target)
        .ok_or(ByeMachineError::FeeNotPriceMinusTicket)?;
    require_eq!(
        config.fee,
        expected_fee,
        ByeMachineError::FeeNotPriceMinusTicket
    );

    require!(
        (1..=MAX_BATCH_CAPACITY).contains(&usize::from(config.max_n)),
        ByeMachineError::MaxNOutOfBounds
    );
    require!(
        (1..=MAX_TIER_SIZE).contains(&config.tier_size),
        ByeMachineError::TierSizeOutOfBounds
    );

    Ok(())
}

/// The winner offer and the escrow bid bound each other: once a pool has a bid,
/// `buyback_rate_bps <= escrow_bid_bps <= 10_000`, so an escrow never pays less for a card than
/// the seller is offered. A bid of 0 is no escrow path and leaves the rate bounded by 10,000 alone.
/// Each variant is refused on its own error, whichever side of the bound it would break.
fn validate_offer_and_bid(pool: &Pool, update: &ConfigUpdate) -> Result<()> {
    match *update {
        ConfigUpdate::BuybackRateBps(rate) => require!(
            rate <= MAX_BPS && (pool.escrow_bid_bps == 0 || rate <= pool.escrow_bid_bps),
            ByeMachineError::BuybackRateOutOfRange
        ),
        ConfigUpdate::EscrowBidBps(bid) => require!(
            (pool.buyback_rate_bps..=MAX_BPS).contains(&bid),
            ByeMachineError::EscrowBidOutOfRange
        ),
        _ => {}
    }
    Ok(())
}

/// The shrink: a `tier_size` lowering demotes the excess lowest-ranked members here,
/// since `update_tier` only promotes and an over-full tier fails `settle_roll`'s tier-size gate
/// forever. Each demoted member's `Position` travels in `remaining_accounts`, matched by key.
fn demote_shrink_excess<'info>(
    top_tier: &mut Account<'info, TopTier>,
    remaining_accounts: &'info [AccountInfo<'info>],
    new_tier_size: u8,
    pool: Pubkey,
    slot: u64,
) -> Result<()> {
    let acc_tier = top_tier.acc_tier;

    for entry in tier::shrink(top_tier, new_tier_size) {
        let info = remaining_accounts
            .iter()
            .find(|account| account.key() == entry.position)
            .ok_or(ErrorCode::AccountNotEnoughKeys)?;
        require!(info.is_writable, ErrorCode::AccountNotMutable);

        let mut position = Account::<Position>::try_from(info)?;
        position.accrued =
            position_fee_payout(acc_tier, position.tier_checkpoint, position.accrued)?;
        position.tier_checkpoint = acc_tier;
        position.in_tier = false;
        position.exit(&crate::ID)?;

        emit!(TierChanged {
            pool,
            slot,
            left: Some(entry.position),
            entered: None
        });
    }

    Ok(())
}

/// One `ConfigChanged` payload: the `Pool` field name in snake_case, and the field's value
/// before and after.
type FieldChange = (&'static str, ConfigValue, ConfigValue);

/// The fields this update actually moves — a composite variant yields up to three entries and a
/// submitted value equal to the stored one yields none, so `ConfigChanged` stays one per changed
/// field.
fn planned_changes(pool: &Pool, update: &ConfigUpdate) -> Vec<FieldChange> {
    let mut changes = Vec::new();

    match *update {
        ConfigUpdate::AdmissionFloor(value) => {
            push_amount(&mut changes, "admission_floor", pool.admission_floor, value)
        }
        ConfigUpdate::AdmissionCeiling(value) => push_amount(
            &mut changes,
            "admission_ceiling",
            pool.admission_ceiling,
            value,
        ),
        ConfigUpdate::WalletValueCap(value) => push_amount(
            &mut changes,
            "wallet_value_cap",
            pool.wallet_value_cap,
            value,
        ),
        ConfigUpdate::MaxN(value) => push_count(&mut changes, "max_n", pool.max_n, value),
        ConfigUpdate::BuybackRateBps(value) => push_bps(
            &mut changes,
            "buyback_rate_bps",
            pool.buyback_rate_bps,
            value,
        ),
        ConfigUpdate::T04Ceiling(_) => {}
        ConfigUpdate::SweepCadenceHours(value) => push_hours(
            &mut changes,
            "sweep_cadence_hours",
            pool.sweep_cadence_hours,
            value,
        ),
        ConfigUpdate::TierSize(value) => {
            push_count(&mut changes, "tier_size", pool.tier_size, value)
        }
        ConfigUpdate::Pricing {
            price,
            ticket_target,
            fee,
        } => {
            push_amount(&mut changes, "price", pool.price, price);
            push_amount(
                &mut changes,
                "ticket_target",
                pool.ticket_target,
                ticket_target,
            );
            push_amount(&mut changes, "fee", pool.fee, fee);
        }
        ConfigUpdate::Allocation {
            equal_bps,
            tier_bps,
            protocol_bps,
        } => {
            push_bps(
                &mut changes,
                "alloc_equal_bps",
                pool.alloc_equal_bps,
                equal_bps,
            );
            push_bps(
                &mut changes,
                "alloc_tier_bps",
                pool.alloc_tier_bps,
                tier_bps,
            );
            push_bps(
                &mut changes,
                "alloc_protocol_bps",
                pool.alloc_protocol_bps,
                protocol_bps,
            );
        }
        ConfigUpdate::BlankFaces(faces) => {
            push_faces(&mut changes, "blank_faces", pool.blank_faces, faces)
        }
        ConfigUpdate::EscrowBidBps(value) => {
            push_bps(&mut changes, "escrow_bid_bps", pool.escrow_bid_bps, value)
        }
    }

    changes
}

fn push_amount(changes: &mut Vec<FieldChange>, param: &'static str, old: u64, new: u64) {
    if old != new {
        changes.push((param, ConfigValue::Amount(old), ConfigValue::Amount(new)));
    }
}

fn push_bps(changes: &mut Vec<FieldChange>, param: &'static str, old: u16, new: u16) {
    if old != new {
        changes.push((param, ConfigValue::Bps(old), ConfigValue::Bps(new)));
    }
}

fn push_hours(changes: &mut Vec<FieldChange>, param: &'static str, old: u16, new: u16) {
    if old != new {
        changes.push((param, ConfigValue::Hours(old), ConfigValue::Hours(new)));
    }
}

fn push_count(changes: &mut Vec<FieldChange>, param: &'static str, old: u8, new: u8) {
    if old != new {
        changes.push((param, ConfigValue::Count(old), ConfigValue::Count(new)));
    }
}

fn push_faces(
    changes: &mut Vec<FieldChange>,
    param: &'static str,
    old: [u64; BLANK_SLOTS],
    new: [u64; BLANK_SLOTS],
) {
    if old != new {
        changes.push((param, ConfigValue::Faces(old), ConfigValue::Faces(new)));
    }
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        DEFAULT_ADMISSION_FLOOR, DEFAULT_ALLOC_EQUAL_BPS, DEFAULT_ALLOC_PROTOCOL_BPS,
        DEFAULT_ALLOC_TIER_BPS, DEFAULT_BLANK_FACES, DEFAULT_BUYBACK_RATE_BPS, DEFAULT_FEE,
        DEFAULT_PRICE, DEFAULT_SWEEP_CADENCE_HOURS, DEFAULT_T04_CEILING, DEFAULT_TICKET_TARGET,
        DEFAULT_TIER_SIZE,
    };

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn launch_pool() -> Pool {
        Pool {
            pool_id: 0,
            weight_index: Pubkey::default(),
            treasury_03: Pubkey::default(),
            price: DEFAULT_PRICE,
            ticket_target: DEFAULT_TICKET_TARGET,
            fee: DEFAULT_FEE,
            alloc_equal_bps: DEFAULT_ALLOC_EQUAL_BPS,
            alloc_tier_bps: DEFAULT_ALLOC_TIER_BPS,
            alloc_protocol_bps: DEFAULT_ALLOC_PROTOCOL_BPS,
            admission_floor: DEFAULT_ADMISSION_FLOOR,
            admission_ceiling: 0,
            wallet_value_cap: 0,
            max_n: 10,
            buyback_rate_bps: DEFAULT_BUYBACK_RATE_BPS,
            blank_faces: DEFAULT_BLANK_FACES,
            t04_ceiling: DEFAULT_T04_CEILING,
            sweep_cadence_hours: DEFAULT_SWEEP_CADENCE_HOURS,
            tier_size: DEFAULT_TIER_SIZE,
            deposits_paused: false,
            rolls_paused: false,
            pause_reason: 0,
            n_real: 0,
            blanks: [0; 8],
            w_real: 0,
            open_batches: 0,
            batch_counter: 0,
            position_counter: 0,
            pending_roll_liability: 0,
            owed_fees: 0,
            acc_equal: 0,
            sweep_pending: false,
            sweep_epoch: 0,
            last_sweep_at: 0,
            sweep_updates: 0,
            bump: 0,
            escrow_bid_bps: 0,
            treasury_04: Pubkey::default(),
            reserved: [0; 32],
        }
    }

    fn check(update: ConfigUpdate) -> Result<()> {
        validate(&effective_config(&launch_pool(), &update))
    }

    fn rejects(update: ConfigUpdate, expected_code: u32) {
        assert_eq!(error_code(check(update).unwrap_err()), expected_code);
    }

    // --- the stored config validates against itself ------------------------------------------

    #[test]
    fn accepts_updates_that_leave_every_guard_satisfied() {
        assert!(check(ConfigUpdate::AdmissionCeiling(50_000_000)).is_ok());
        assert!(check(ConfigUpdate::WalletValueCap(500_000_000)).is_ok());
        assert!(check(ConfigUpdate::BuybackRateBps(9_000)).is_ok());
        assert!(check(ConfigUpdate::SweepCadenceHours(12)).is_ok());
        assert!(check(ConfigUpdate::AdmissionFloor(20_000_000)).is_ok());
        assert!(check(ConfigUpdate::BlankFaces([
            1_000_000, 3_000_000, 8_999_999, 0, 0, 0, 0, 0
        ]))
        .is_ok());
        assert!(check(ConfigUpdate::Allocation {
            equal_bps: 6_000,
            tier_bps: 1_000,
            protocol_bps: 3_000
        })
        .is_ok());
        assert!(check(ConfigUpdate::Pricing {
            price: 11_000_000,
            ticket_target: 10_000_000,
            fee: 1_000_000
        })
        .is_ok());
    }

    // --- a floor at or below the ticket needs an above-ticket face ---------------------------

    #[test]
    fn rejects_a_floor_lowered_to_or_below_the_stored_ticket_with_no_above_ticket_face() {
        rejects(ConfigUpdate::AdmissionFloor(DEFAULT_TICKET_TARGET), 6527);
        rejects(ConfigUpdate::AdmissionFloor(8_000_000), 6527);
    }

    #[test]
    fn rejects_a_ticket_raised_to_the_stored_floor_with_no_above_ticket_face() {
        rejects(
            ConfigUpdate::Pricing {
                price: 15_000_000,
                ticket_target: DEFAULT_ADMISSION_FLOOR,
                fee: 3_000_000,
            },
            6527,
        );
    }

    const GOLD_FACES: [u64; BLANK_SLOTS] =
        [5_000_000, 10_000_000, 25_000_000, 100_000_000, 0, 0, 0, 0];

    fn gold_pool() -> Pool {
        let mut pool = launch_pool();
        pool.price = 49_500_000;
        pool.ticket_target = 45_000_000;
        pool.fee = 4_500_000;
        pool.admission_floor = 30_000_000;
        pool.admission_ceiling = 2_500_000_000;
        pool.blank_faces = GOLD_FACES;
        pool
    }

    fn check_gold(update: ConfigUpdate) -> Result<()> {
        validate(&effective_config(&gold_pool(), &update))
    }

    fn gold_rejects(update: ConfigUpdate, expected_code: u32) {
        assert_eq!(error_code(check_gold(update).unwrap_err()), expected_code);
    }

    #[test]
    fn gold_accepts_every_later_change_including_a_floor_at_its_ticket() {
        assert!(check_gold(ConfigUpdate::AdmissionCeiling(3_000_000_000)).is_ok());
        assert!(check_gold(ConfigUpdate::MaxN(5)).is_ok());
        assert!(check_gold(ConfigUpdate::AdmissionFloor(45_000_000)).is_ok());
        assert!(check_gold(ConfigUpdate::AdmissionFloor(29_000_000)).is_ok());
        assert!(check_gold(ConfigUpdate::BlankFaces(GOLD_FACES)).is_ok());
        assert!(check_gold(ConfigUpdate::Pricing {
            price: 55_000_000,
            ticket_target: 50_000_000,
            fee: 5_000_000,
        })
        .is_ok());
    }

    #[test]
    fn gold_refuses_its_above_ticket_face_set_to_zero() {
        let mut faces = GOLD_FACES;
        faces[3] = 0;
        gold_rejects(ConfigUpdate::BlankFaces(faces), 6527);
    }

    #[test]
    fn an_above_ticket_face_less_than_one_usdc_over_the_ticket_is_refused_set_directly() {
        for face in [45_999_999, 45_000_000, 30_000_000] {
            let mut faces = GOLD_FACES;
            faces[3] = face;
            gold_rejects(ConfigUpdate::BlankFaces(faces), 6528);
        }
        let mut faces = GOLD_FACES;
        faces[3] = 46_000_000;
        assert!(check_gold(ConfigUpdate::BlankFaces(faces)).is_ok());
    }

    #[test]
    fn an_above_ticket_face_less_than_one_usdc_over_the_ticket_is_refused_through_a_ticket_raise() {
        gold_rejects(
            ConfigUpdate::Pricing {
                price: 109_000_001,
                ticket_target: 99_000_001,
                fee: 10_000_000,
            },
            6528,
        );
        assert!(check_gold(ConfigUpdate::Pricing {
            price: 109_000_000,
            ticket_target: 99_000_000,
            fee: 10_000_000,
        })
        .is_ok());
    }

    #[test]
    fn a_face_in_any_reserved_slot_is_refused() {
        for slot in 4..8 {
            let mut faces = GOLD_FACES;
            faces[slot] = 1;
            gold_rejects(ConfigUpdate::BlankFaces(faces), 6529);
            let mut faces = DEFAULT_BLANK_FACES;
            faces[slot] = 200_000_000;
            rejects(ConfigUpdate::BlankFaces(faces), 6529);
        }
    }

    // --- faces below ticket: a face raise and a ticket lowering -------------------------------

    #[test]
    fn rejects_a_face_raised_to_the_stored_ticket() {
        rejects(
            ConfigUpdate::BlankFaces([1_000_000, 2_000_000, DEFAULT_TICKET_TARGET, 0, 0, 0, 0, 0]),
            6502,
        );
    }

    #[test]
    fn rejects_a_ticket_lowered_below_a_stored_face() {
        rejects(
            ConfigUpdate::Pricing {
                price: 1_400_000,
                ticket_target: 500_000,
                fee: 900_000,
            },
            6502,
        );
    }

    #[test]
    fn rejects_a_face_set_to_zero() {
        rejects(
            ConfigUpdate::BlankFaces([1_000_000, 2_000_000, 0, 0, 0, 0, 0, 0]),
            6511,
        );
    }

    // --- allocation sum: under and over 10,000 ------------------------------------------------

    #[test]
    fn rejects_an_allocation_that_does_not_sum_to_ten_thousand() {
        rejects(
            ConfigUpdate::Allocation {
                equal_bps: 7_500,
                tier_bps: 500,
                protocol_bps: 1_999,
            },
            6501,
        );
        rejects(
            ConfigUpdate::Allocation {
                equal_bps: 7_500,
                tier_bps: 500,
                protocol_bps: 2_001,
            },
            6501,
        );
    }

    #[test]
    fn allocation_sum_is_widened_before_comparison() {
        assert!(check(ConfigUpdate::Allocation {
            equal_bps: u16::MAX,
            tier_bps: u16::MAX,
            protocol_bps: u16::MAX
        })
        .is_err());
    }

    // --- fee == price - ticket_target: a price move and a ticket move -------------------------

    #[test]
    fn rejects_a_price_move_that_leaves_the_fee_stale() {
        rejects(
            ConfigUpdate::Pricing {
                price: 10_000_000,
                ticket_target: DEFAULT_TICKET_TARGET,
                fee: DEFAULT_FEE,
            },
            6505,
        );
    }

    #[test]
    fn rejects_a_ticket_move_that_leaves_the_fee_stale() {
        rejects(
            ConfigUpdate::Pricing {
                price: DEFAULT_PRICE,
                ticket_target: 8_000_000,
                fee: DEFAULT_FEE,
            },
            6505,
        );
    }

    #[test]
    fn rejects_a_price_below_the_ticket_target() {
        rejects(
            ConfigUpdate::Pricing {
                price: 8_000_000,
                ticket_target: 8_500_000,
                fee: 0,
            },
            6505,
        );
    }

    // --- max_n bounds ------------------------------------------------------------------------

    #[test]
    fn rejects_max_n_outside_one_through_the_batch_capacity() {
        rejects(ConfigUpdate::MaxN(0), 6506);
        rejects(ConfigUpdate::MaxN(11), 6506);
    }

    #[test]
    fn accepts_max_n_at_both_bounds() {
        assert!(check(ConfigUpdate::MaxN(1)).is_ok());
        assert!(check(ConfigUpdate::MaxN(MAX_BATCH_CAPACITY as u8)).is_ok());
    }

    // --- tier_size bounds --------------------------------------------------------------------

    #[test]
    fn rejects_tier_size_outside_one_through_twenty() {
        rejects(ConfigUpdate::TierSize(0), 6504);
        rejects(ConfigUpdate::TierSize(21), 6504);
    }

    #[test]
    fn accepts_tier_size_at_both_bounds() {
        assert!(check(ConfigUpdate::TierSize(1)).is_ok());
        assert!(check(ConfigUpdate::TierSize(MAX_TIER_SIZE)).is_ok());
    }

    // --- one ConfigChanged per changed field --------------------------------------------------

    // `ConfigValue` derives neither `PartialEq` nor `Debug` (a shared type in events.rs) — read
    // each variant back by matching instead.
    fn amount_of(value: &ConfigValue) -> u64 {
        match value {
            ConfigValue::Amount(inner) => *inner,
            _ => panic!("expected Amount"),
        }
    }

    fn bps_of(value: &ConfigValue) -> u16 {
        match value {
            ConfigValue::Bps(inner) => *inner,
            _ => panic!("expected Bps"),
        }
    }

    fn hours_of(value: &ConfigValue) -> u16 {
        match value {
            ConfigValue::Hours(inner) => *inner,
            _ => panic!("expected Hours"),
        }
    }

    fn count_of(value: &ConfigValue) -> u8 {
        match value {
            ConfigValue::Count(inner) => *inner,
            _ => panic!("expected Count"),
        }
    }

    fn faces_of(value: &ConfigValue) -> [u64; BLANK_SLOTS] {
        match value {
            ConfigValue::Faces(inner) => *inner,
            _ => panic!("expected Faces"),
        }
    }

    fn plan(update: ConfigUpdate) -> Vec<FieldChange> {
        planned_changes(&launch_pool(), &update)
    }

    fn params(update: ConfigUpdate) -> Vec<&'static str> {
        plan(update).iter().map(|(param, _, _)| *param).collect()
    }

    #[test]
    fn a_value_equal_to_the_stored_one_plans_no_change() {
        assert!(plan(ConfigUpdate::AdmissionFloor(DEFAULT_ADMISSION_FLOOR)).is_empty());
        assert!(plan(ConfigUpdate::TierSize(DEFAULT_TIER_SIZE)).is_empty());
        assert!(plan(ConfigUpdate::SweepCadenceHours(DEFAULT_SWEEP_CADENCE_HOURS)).is_empty());
        assert!(plan(ConfigUpdate::BlankFaces(DEFAULT_BLANK_FACES)).is_empty());
        assert!(plan(ConfigUpdate::Pricing {
            price: DEFAULT_PRICE,
            ticket_target: DEFAULT_TICKET_TARGET,
            fee: DEFAULT_FEE
        })
        .is_empty());
        assert!(plan(ConfigUpdate::Allocation {
            equal_bps: DEFAULT_ALLOC_EQUAL_BPS,
            tier_bps: DEFAULT_ALLOC_TIER_BPS,
            protocol_bps: DEFAULT_ALLOC_PROTOCOL_BPS
        })
        .is_empty());
    }

    #[test]
    fn a_composite_plans_only_the_fields_it_moves() {
        assert_eq!(
            params(ConfigUpdate::Pricing {
                price: 11_000_000,
                ticket_target: DEFAULT_TICKET_TARGET,
                fee: DEFAULT_FEE
            }),
            vec!["price"]
        );
        assert_eq!(
            params(ConfigUpdate::Pricing {
                price: 11_000_000,
                ticket_target: 10_000_000,
                fee: 1_000_000
            }),
            vec!["price", "ticket_target", "fee"]
        );
        assert_eq!(
            params(ConfigUpdate::Allocation {
                equal_bps: DEFAULT_ALLOC_EQUAL_BPS,
                tier_bps: 1_000,
                protocol_bps: 1_500
            }),
            vec!["alloc_tier_bps", "alloc_protocol_bps"]
        );
    }

    #[test]
    fn a_face_array_plans_exactly_one_change() {
        let new_faces = [1_000_000, 2_000_000, 6_000_000, 20_000_000, 0, 0, 0, 0];
        let changes = plan(ConfigUpdate::BlankFaces(new_faces));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, "blank_faces");
        assert_eq!(faces_of(&changes[0].1), DEFAULT_BLANK_FACES);
        assert_eq!(faces_of(&changes[0].2), new_faces);
    }

    /// The per-pool ceiling arm plans no change, and its write arm refuses with its own error
    /// before any field is written: the pool's `t04_ceiling` has no writer.
    #[test]
    fn the_per_pool_t04_ceiling_arm_is_refused() {
        for value in [0, 1, DEFAULT_T04_CEILING, u64::MAX] {
            assert!(planned_changes(&launch_pool(), &ConfigUpdate::T04Ceiling(value)).is_empty());
            assert!(validate(&effective_config(
                &launch_pool(),
                &ConfigUpdate::T04Ceiling(value)
            ))
            .is_ok());
        }
        let prod = crate::common::test_support::flat(include_str!("update_config.rs"));
        assert_eq!(
            prod.matches(
                "ConfigUpdate::T04Ceiling(_) => return err!(ByeMachineError::T04CeilingIsProtocolWide),"
            )
            .count(),
            1
        );
        assert_eq!(
            crate::common::test_support::writes(include_str!("update_config.rs"), "t04_ceiling"),
            0
        );
        assert_eq!(u32::from(ByeMachineError::T04CeilingIsProtocolWide), 6530);
    }

    #[test]
    fn every_pool_field_is_planned_under_its_own_name() {
        let mut planned = Vec::new();
        for update in [
            ConfigUpdate::AdmissionFloor(20_000_000),
            ConfigUpdate::AdmissionCeiling(50_000_000),
            ConfigUpdate::WalletValueCap(500_000_000),
            ConfigUpdate::MaxN(5),
            ConfigUpdate::BuybackRateBps(9_000),
            ConfigUpdate::SweepCadenceHours(12),
            ConfigUpdate::TierSize(5),
            ConfigUpdate::Pricing {
                price: 11_000_000,
                ticket_target: 10_000_000,
                fee: 1_000_000,
            },
            ConfigUpdate::Allocation {
                equal_bps: 6_000,
                tier_bps: 1_000,
                protocol_bps: 3_000,
            },
            ConfigUpdate::BlankFaces([1_000_000, 2_000_000, 6_000_000, 0, 0, 0, 0, 0]),
            ConfigUpdate::EscrowBidBps(9_250),
        ] {
            planned.extend(params(update));
        }

        assert_eq!(
            planned,
            vec![
                "admission_floor",
                "admission_ceiling",
                "wallet_value_cap",
                "max_n",
                "buyback_rate_bps",
                "sweep_cadence_hours",
                "tier_size",
                "price",
                "ticket_target",
                "fee",
                "alloc_equal_bps",
                "alloc_tier_bps",
                "alloc_protocol_bps",
                "blank_faces",
                "escrow_bid_bps",
            ]
        );
    }

    #[test]
    fn each_field_carries_its_stored_value_and_its_submitted_value() {
        let floor = plan(ConfigUpdate::AdmissionFloor(20_000_000));
        assert_eq!(amount_of(&floor[0].1), DEFAULT_ADMISSION_FLOOR);
        assert_eq!(amount_of(&floor[0].2), 20_000_000);

        let buyback = plan(ConfigUpdate::BuybackRateBps(9_000));
        assert_eq!(bps_of(&buyback[0].1), DEFAULT_BUYBACK_RATE_BPS);
        assert_eq!(bps_of(&buyback[0].2), 9_000);

        let cadence = plan(ConfigUpdate::SweepCadenceHours(12));
        assert_eq!(hours_of(&cadence[0].1), DEFAULT_SWEEP_CADENCE_HOURS);
        assert_eq!(hours_of(&cadence[0].2), 12);

        let tier_size = plan(ConfigUpdate::TierSize(5));
        assert_eq!(count_of(&tier_size[0].1), DEFAULT_TIER_SIZE);
        assert_eq!(count_of(&tier_size[0].2), 5);

        let bid = plan(ConfigUpdate::EscrowBidBps(9_250));
        assert_eq!(bps_of(&bid[0].1), 0);
        assert_eq!(bps_of(&bid[0].2), 9_250);
    }

    // --- the weight-gated moves ----------------------------------------------------------------

    #[test]
    fn blank_faces_and_a_ticket_retarget_are_weight_gated() {
        let pool = launch_pool();
        assert!(requires_weight_freeze(
            &pool,
            &ConfigUpdate::BlankFaces(DEFAULT_BLANK_FACES)
        ));
        assert!(requires_weight_freeze(
            &pool,
            &ConfigUpdate::Pricing {
                price: 11_000_000,
                ticket_target: 10_000_000,
                fee: 1_000_000
            }
        ));
    }

    #[test]
    fn a_pricing_move_of_price_and_fee_alone_is_not_weight_gated() {
        let pool = launch_pool();
        assert!(!requires_weight_freeze(
            &pool,
            &ConfigUpdate::Pricing {
                price: 10_000_000,
                ticket_target: DEFAULT_TICKET_TARGET,
                fee: 1_000_000
            }
        ));
    }

    /// A `tier_size` raise must not land mid-batch (`demote_shrink_excess` only shrinks, so
    /// `settle_roll`'s tier-length guard would fail every remaining roll). A shrink is gated on
    /// the same terms so the reasoning lives in one place.
    #[test]
    fn a_tier_size_move_is_weight_gated_in_both_directions_and_a_no_op_is_not() {
        let pool = launch_pool();
        assert!(requires_weight_freeze(
            &pool,
            &ConfigUpdate::TierSize(DEFAULT_TIER_SIZE + 5)
        ));
        assert!(requires_weight_freeze(
            &pool,
            &ConfigUpdate::TierSize(DEFAULT_TIER_SIZE - 1)
        ));
        assert!(
            !requires_weight_freeze(&pool, &ConfigUpdate::TierSize(DEFAULT_TIER_SIZE)),
            "a write that changes nothing moves no tier and wedges no batch"
        );
    }

    #[test]
    fn no_other_variant_is_weight_gated() {
        let pool = launch_pool();
        let ungated = [
            ConfigUpdate::AdmissionFloor(20_000_000),
            ConfigUpdate::AdmissionCeiling(50_000_000),
            ConfigUpdate::WalletValueCap(500_000_000),
            ConfigUpdate::MaxN(5),
            ConfigUpdate::BuybackRateBps(9_000),
            ConfigUpdate::T04Ceiling(3_000_000_000),
            ConfigUpdate::SweepCadenceHours(12),
            ConfigUpdate::Allocation {
                equal_bps: 6_000,
                tier_bps: 1_000,
                protocol_bps: 3_000,
            },
            ConfigUpdate::EscrowBidBps(9_250),
        ];
        for update in ungated {
            assert!(!requires_weight_freeze(&pool, &update));
        }
    }

    // --- the winner offer and the escrow bid bound each other ---------------------------------

    fn pool_with_rates(buyback_rate_bps: u16, escrow_bid_bps: u16) -> Pool {
        let mut pool = launch_pool();
        pool.buyback_rate_bps = buyback_rate_bps;
        pool.escrow_bid_bps = escrow_bid_bps;
        pool
    }

    fn rate_or_bid(rate: u16, bid: u16, update: ConfigUpdate) -> Result<()> {
        validate_offer_and_bid(&pool_with_rates(rate, bid), &update)
    }

    #[test]
    fn a_rate_above_ten_thousand_is_refused_with_or_without_a_bid() {
        for bid in [0, 9_250, MAX_BPS] {
            assert_eq!(
                error_code(
                    rate_or_bid(8_500, bid, ConfigUpdate::BuybackRateBps(10_001)).unwrap_err()
                ),
                6525,
                "bid {bid}"
            );
        }
        assert_eq!(
            error_code(rate_or_bid(8_500, 0, ConfigUpdate::BuybackRateBps(u16::MAX)).unwrap_err()),
            6525
        );
    }

    #[test]
    fn with_no_bid_any_rate_up_to_ten_thousand_is_accepted() {
        for rate in [0, 1, 8_500, 9_999, MAX_BPS] {
            assert!(rate_or_bid(8_500, 0, ConfigUpdate::BuybackRateBps(rate)).is_ok());
        }
    }

    #[test]
    fn with_a_bid_the_rate_may_rise_to_the_bid_and_no_further() {
        assert!(rate_or_bid(8_500, 9_250, ConfigUpdate::BuybackRateBps(9_250)).is_ok());
        assert!(rate_or_bid(8_500, 9_250, ConfigUpdate::BuybackRateBps(0)).is_ok());
        assert_eq!(
            error_code(rate_or_bid(8_500, 9_250, ConfigUpdate::BuybackRateBps(9_251)).unwrap_err()),
            6525
        );
    }

    #[test]
    fn a_bid_must_lie_between_the_rate_and_ten_thousand() {
        assert!(rate_or_bid(8_500, 0, ConfigUpdate::EscrowBidBps(8_500)).is_ok());
        assert!(rate_or_bid(8_500, 0, ConfigUpdate::EscrowBidBps(9_250)).is_ok());
        assert!(rate_or_bid(8_500, 9_250, ConfigUpdate::EscrowBidBps(MAX_BPS)).is_ok());
        for bid in [0, 8_499, 10_001, u16::MAX] {
            assert_eq!(
                error_code(rate_or_bid(8_500, 9_250, ConfigUpdate::EscrowBidBps(bid)).unwrap_err()),
                6526,
                "bid {bid}"
            );
        }
    }

    #[test]
    fn no_other_variant_is_bounded_by_the_offer_or_the_bid() {
        // A pool whose stored rate already breaks the bound does not block an unrelated move.
        let pool = pool_with_rates(10_001, 9_250);
        for update in [
            ConfigUpdate::AdmissionCeiling(50_000_000),
            ConfigUpdate::MaxN(5),
            ConfigUpdate::BlankFaces(DEFAULT_BLANK_FACES),
        ] {
            assert!(validate_offer_and_bid(&pool, &update).is_ok());
        }
    }

    /// No sequence of `EscrowBidBps` and `BuybackRateBps` moves leaves the rate above the bid while
    /// the bid is set, or either above 10,000. Each step applies only what the handler would
    /// accept; candidates mix uniform draws with the values either side of each bound, where an
    /// off-by-one would surface.
    #[test]
    fn no_sequence_of_rate_and_bid_moves_leaves_the_rate_above_a_set_bid() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        let mut accepted = [0usize; 2];
        let mut refused = [0usize; 2];
        for _ in 0..2_000 {
            let mut pool = pool_with_rates(DEFAULT_BUYBACK_RATE_BPS, 0);
            for _ in 0..40 {
                let (rate, bid) = (pool.buyback_rate_bps, pool.escrow_bid_bps);
                let edges = [
                    0,
                    1,
                    rate.saturating_sub(1),
                    rate,
                    rate.saturating_add(1),
                    bid.saturating_sub(1),
                    bid,
                    bid.saturating_add(1),
                    MAX_BPS - 1,
                    MAX_BPS,
                    MAX_BPS + 1,
                    u16::MAX,
                ];
                let r = next();
                let value = if r % 3 == 0 {
                    (next() % (u64::from(MAX_BPS) + 2)) as u16
                } else {
                    edges[(next() % edges.len() as u64) as usize]
                };
                let is_bid = r % 2 == 0;
                let update = if is_bid {
                    ConfigUpdate::EscrowBidBps(value)
                } else {
                    ConfigUpdate::BuybackRateBps(value)
                };

                if validate_offer_and_bid(&pool, &update).is_ok() {
                    accepted[usize::from(is_bid)] += 1;
                    match update {
                        ConfigUpdate::EscrowBidBps(v) => pool.escrow_bid_bps = v,
                        ConfigUpdate::BuybackRateBps(v) => pool.buyback_rate_bps = v,
                        _ => unreachable!(),
                    }
                } else {
                    refused[usize::from(is_bid)] += 1;
                }

                assert!(pool.buyback_rate_bps <= MAX_BPS);
                assert!(pool.escrow_bid_bps <= MAX_BPS);
                assert!(
                    pool.escrow_bid_bps == 0 || pool.buyback_rate_bps <= pool.escrow_bid_bps,
                    "rate {} above bid {}",
                    pool.buyback_rate_bps,
                    pool.escrow_bid_bps
                );
            }
        }

        // Both variants were exercised on both sides of the guard, so the run is not vacuous.
        for side in 0..2 {
            assert!(accepted[side] > 1_000, "{accepted:?}");
            assert!(refused[side] > 1_000, "{refused:?}");
        }
    }

    /// The handler runs the bound before it writes anything; the tests above call the bound
    /// directly and cannot see it dropped from the handler.
    #[test]
    fn the_handler_checks_the_offer_and_bid_before_writing_the_pool() {
        let prod = crate::common::test_support::code(include_str!("update_config.rs"));
        let handler = &prod
            [prod.find("pub fn handler").unwrap()..prod.find("fn requires_weight_freeze").unwrap()];
        assert_eq!(
            crate::common::test_support::calls(handler, "validate_offer_and_bid"),
            1
        );
        assert!(
            handler.find("validate_offer_and_bid(").unwrap()
                < handler.find("pool.escrow_bid_bps = value").unwrap()
        );
        assert!(
            handler.find("validate_offer_and_bid(").unwrap()
                < handler.find("pool.buyback_rate_bps = value").unwrap()
        );
    }

    /// `init_pool` writes the bid as 0 and `update_config` is the only other writer, so the
    /// bound above is the only way a bid is set. `DEFAULT_ESCROW_BID_BPS` is for deployment
    /// scripts: no production code applies it.
    #[test]
    fn the_escrow_bid_has_two_writers_and_its_default_is_never_applied() {
        use crate::common::test_support::{code, crate_sources, writes};

        let mut writers = Vec::new();
        let mut default_readers = Vec::new();
        for path in crate_sources() {
            let source = std::fs::read_to_string(&path).unwrap();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            if writes(&source, ".escrow_bid_bps") > 0 {
                writers.push(name.clone());
            }
            if code(&source).contains("DEFAULT_ESCROW_BID_BPS") {
                default_readers.push(name);
            }
        }
        writers.sort();
        assert_eq!(writers, vec!["init_pool.rs", "update_config.rs"]);
        assert_eq!(default_readers, vec!["constants.rs"]);
    }

    // --- the blank set follows a face or ticket change -----------------------------------------

    use crate::common::math::weight_of;

    /// Gold holding seven cards at 30 and three at 500 USDC: below its ticket, so only slot 3.
    fn gold_in_its_low_value_state() -> Pool {
        let mut pool = gold_pool();
        pool.n_real = 10;
        pool.w_real = 7 * u128::from(weight_of(30_000_000).unwrap())
            + 3 * u128::from(weight_of(500_000_000).unwrap());
        pool.blanks = [0, 0, 0, 1, 0, 0, 0, 0];
        pool
    }

    #[test]
    fn a_new_above_ticket_face_recomputes_gold_s_counts() {
        let mut pool = gold_in_its_low_value_state();
        assert_eq!(
            rebalance::evaluate(
                pool.n_real,
                pool.w_real,
                pool.blank_faces,
                pool.ticket_target
            )
            .unwrap(),
            pool.blanks,
            "the fixture's stored counts are the controller's own"
        );
        // At 50 USDC each blank lowers the bracket by C/10 against d ≈ 0.77·C, so the count
        // rises from 1 to 8. Hand-computed scores: 7 → 6.9999e16, 8 → 3.0000e16, 9 → 1.3000e17.
        let mut faces = GOLD_FACES;
        faces[3] = 50_000_000;
        let update = ConfigUpdate::BlankFaces(faces);
        assert!(retargets_blanks(&pool, &update));
        pool.blank_faces = faces;
        let before = reevaluate_blanks(&mut pool).unwrap();
        assert_eq!(before, [0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(pool.blanks, [0, 0, 0, 8, 0, 0, 0, 0]);
    }

    #[test]
    fn zeroing_slot_3_on_a_floor_above_ticket_pool_leaves_it_no_count() {
        // A pool whose floor sits above its ticket may drop its above-ticket face; a count the
        // controller set there under a raised ticket must not outlive the face.
        let mut pool = gold_in_its_low_value_state();
        pool.admission_floor = 50_000_000;
        let mut faces = GOLD_FACES;
        faces[3] = 0;
        let update = ConfigUpdate::BlankFaces(faces);
        assert!(validate(&effective_config(&pool, &update)).is_ok());
        assert!(retargets_blanks(&pool, &update));
        pool.blank_faces = faces;
        reevaluate_blanks(&mut pool).unwrap();
        assert_eq!(pool.blanks[3], 0);
        assert_eq!(
            pool.blanks, [0; 8],
            "cards below the ticket with no above-ticket face set nothing"
        );
    }

    #[test]
    fn only_a_face_or_ticket_change_re_evaluates() {
        let pool = launch_pool();
        assert!(retargets_blanks(
            &pool,
            &ConfigUpdate::BlankFaces(DEFAULT_BLANK_FACES)
        ));
        assert!(retargets_blanks(
            &pool,
            &ConfigUpdate::Pricing {
                price: 11_000_000,
                ticket_target: 10_000_000,
                fee: 1_000_000
            }
        ));
        assert!(!retargets_blanks(
            &pool,
            &ConfigUpdate::Pricing {
                price: 10_000_000,
                ticket_target: DEFAULT_TICKET_TARGET,
                fee: 1_000_000
            }
        ));
        for update in [
            ConfigUpdate::AdmissionFloor(20_000_000),
            ConfigUpdate::TierSize(5),
            ConfigUpdate::MaxN(5),
            ConfigUpdate::EscrowBidBps(9_250),
        ] {
            assert!(!retargets_blanks(&pool, &update));
        }
    }

    /// The handler re-evaluates after the config is written and announces it, behind the same
    /// predicate the tests above exercise.
    #[test]
    fn the_handler_re_evaluates_after_writing_and_emits_the_evaluation() {
        let prod = crate::common::test_support::code(include_str!("update_config.rs"));
        let handler = &prod
            [prod.find("pub fn handler").unwrap()..prod.find("fn requires_weight_freeze").unwrap()];
        let decided = handler
            .find("retargets_blanks(&ctx.accounts.pool, &update)")
            .unwrap();
        let written = handler
            .find("ConfigUpdate::BlankFaces(faces) => pool.blank_faces = faces")
            .unwrap();
        let reevaluated = handler.find("reevaluate_blanks(pool)").unwrap();
        let emitted = handler.find("emit!(RebalanceEvaluated").unwrap();
        assert!(decided < written && written < reevaluated && reevaluated < emitted);
        assert!(handler.contains("if rebalances {"));
    }

    // --- `ConfigUpdate`'s variant order is the wire discriminant -----------------------------

    /// Hand-written, independent of declaration order: a new `ConfigUpdate` variant fails this
    /// match to compile. Variant order is the wire discriminant an off-chain decoder reads from
    /// `ConfigChanged`, and the symbolic tests above cannot see a declaration-order swap.
    fn expected_config_update_byte(update: &ConfigUpdate) -> u8 {
        match update {
            ConfigUpdate::AdmissionFloor(_) => 0,
            ConfigUpdate::AdmissionCeiling(_) => 1,
            ConfigUpdate::WalletValueCap(_) => 2,
            ConfigUpdate::MaxN(_) => 3,
            ConfigUpdate::BuybackRateBps(_) => 4,
            ConfigUpdate::T04Ceiling(_) => 5,
            ConfigUpdate::SweepCadenceHours(_) => 6,
            ConfigUpdate::TierSize(_) => 7,
            ConfigUpdate::Pricing { .. } => 8,
            ConfigUpdate::Allocation { .. } => 9,
            ConfigUpdate::BlankFaces(_) => 10,
            ConfigUpdate::EscrowBidBps(_) => 11,
        }
    }

    #[test]
    fn config_update_variant_order_is_the_wire_discriminant() {
        for update in [
            ConfigUpdate::AdmissionFloor(0),
            ConfigUpdate::AdmissionCeiling(0),
            ConfigUpdate::WalletValueCap(0),
            ConfigUpdate::MaxN(0),
            ConfigUpdate::BuybackRateBps(0),
            ConfigUpdate::T04Ceiling(0),
            ConfigUpdate::SweepCadenceHours(0),
            ConfigUpdate::TierSize(0),
            ConfigUpdate::Pricing {
                price: 0,
                ticket_target: 0,
                fee: 0,
            },
            ConfigUpdate::Allocation {
                equal_bps: 0,
                tier_bps: 0,
                protocol_bps: 0,
            },
            ConfigUpdate::BlankFaces([0; 8]),
            ConfigUpdate::EscrowBidBps(0),
        ] {
            let expected = expected_config_update_byte(&update);
            let bytes = update.try_to_vec().unwrap();
            assert_eq!(
                bytes[0], expected,
                "ConfigUpdate's leading wire byte moved for the variant this pin expected at \
                 index {expected} — a declaration-order swap is invisible to every symbolic test \
                 above and relocates which field an off-chain decoder believes changed"
            );
        }
    }

    /// The blank-faces arm's whole body: tag 10, then eight little-endian `u64` faces, slot order.
    #[test]
    fn the_blank_faces_arm_carries_eight_faces_after_its_tag() {
        let faces = [1u64, 2, 3, 4, 5, 6, 7, u64::MAX];
        let mut expected = vec![10u8];
        for face in faces {
            expected.extend_from_slice(&face.to_le_bytes());
        }
        assert_eq!(expected.len(), 65);
        assert_eq!(
            ConfigUpdate::BlankFaces(faces).try_to_vec().unwrap(),
            expected
        );
    }

    // A variant added without a matching entry in the array above would stay invisible to it;
    // the first byte past the current range must still fail to deserialize.
    #[test]
    fn config_update_rejects_out_of_range_discriminant() {
        // A padded buffer cannot tell "tag out of range" from "payload truncated"; assert on
        // borsh's own "Unexpected variant index" instead. One tag byte suffices, since an invalid
        // tag is rejected before any payload is read.
        let mut buf: &[u8] = &[12u8];
        let err = match ConfigUpdate::deserialize(&mut buf) {
            Ok(_) => panic!(
                "byte 12 deserialized into a ConfigUpdate — a 13th variant was added; add it to \
                 config_update_variant_order_is_the_wire_discriminant's iteration array"
            ),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("Unexpected variant index"),
            "byte 12 failed to deserialize for a reason other than an out-of-range tag — a 13th \
             variant may have been added with a payload that also fails on truncation, masking \
             this probe; add the variant to \
             config_update_variant_order_is_the_wire_discriminant's iteration array \
             (borsh said: {err})"
        );
    }
}
