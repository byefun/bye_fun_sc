use anchor_lang::prelude::*;

use crate::common::constants::MAX_BATCH_CAPACITY;
use crate::common::errors::ByeMachineError;
use crate::common::events::{ConfigChanged, ConfigValue, TierChanged};
use crate::common::guards::assert_weight_open;
use crate::common::math::position_fee_payout;
use crate::common::seeds::{POOL_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED};
use crate::common::tier;
use crate::state::{Pool, Position, ProtocolConfig, TopTier};

const MAX_TIER_SIZE: u8 = 20;

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
    BlankFaces([u64; 3]),
}

pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, UpdateConfig<'info>>,
    update: ConfigUpdate,
) -> Result<()> {
    validate(&effective_config(&ctx.accounts.pool, &update))?;

    if requires_weight_freeze(&ctx.accounts.pool, &update) {
        assert_weight_open(&ctx.accounts.pool)?;
    }

    let changes = planned_changes(&ctx.accounts.pool, &update);
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
        ConfigUpdate::T04Ceiling(value) => pool.t04_ceiling = value,
        ConfigUpdate::SweepCadenceHours(value) => pool.sweep_cadence_hours = value,
        ConfigUpdate::BlankFaces(faces) => pool.blank_faces = faces,
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

    Ok(())
}

/// The two weight-freeze call sites in this instruction: both re-target the controller and so move `W`.
/// A `Pricing` move of `price`/`fee` alone is not gated — a committed batch snapshots both.
fn requires_weight_freeze(pool: &Pool, update: &ConfigUpdate) -> bool {
    match *update {
        ConfigUpdate::BlankFaces(_) => true,
        ConfigUpdate::Pricing { ticket_target, .. } => ticket_target != pool.ticket_target,
        _ => false,
    }
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
    blank_faces: [u64; 3],
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
    require_gt!(
        config.admission_floor,
        config.ticket_target,
        ByeMachineError::AdmissionFloorNotAboveTicket
    );

    for face in config.blank_faces {
        require!(face > 0, ByeMachineError::BlankFaceZero);
        require_gt!(
            config.ticket_target,
            face,
            ByeMachineError::BlankFaceNotBelowTicket
        );
    }

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
        ConfigUpdate::T04Ceiling(value) => {
            push_amount(&mut changes, "t04_ceiling", pool.t04_ceiling, value)
        }
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

fn push_faces(changes: &mut Vec<FieldChange>, param: &'static str, old: [u64; 3], new: [u64; 3]) {
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
            blanks: [0, 0, 0],
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
        assert!(check(ConfigUpdate::T04Ceiling(3_000_000_000)).is_ok());
        assert!(check(ConfigUpdate::SweepCadenceHours(12)).is_ok());
        assert!(check(ConfigUpdate::AdmissionFloor(20_000_000)).is_ok());
        assert!(check(ConfigUpdate::BlankFaces([1_000_000, 3_000_000, 8_999_999])).is_ok());
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

    // --- floor above ticket: a floor lowering and a ticket raise ------------------------------

    #[test]
    fn rejects_a_floor_lowered_to_or_below_the_stored_ticket() {
        rejects(ConfigUpdate::AdmissionFloor(DEFAULT_TICKET_TARGET), 6500);
        rejects(ConfigUpdate::AdmissionFloor(8_000_000), 6500);
    }

    #[test]
    fn rejects_a_ticket_raised_to_the_stored_floor() {
        rejects(
            ConfigUpdate::Pricing {
                price: 15_000_000,
                ticket_target: DEFAULT_ADMISSION_FLOOR,
                fee: 3_000_000,
            },
            6500,
        );
    }

    // --- faces below ticket: a face raise and a ticket lowering -------------------------------

    #[test]
    fn rejects_a_face_raised_to_the_stored_ticket() {
        rejects(
            ConfigUpdate::BlankFaces([1_000_000, 2_000_000, DEFAULT_TICKET_TARGET]),
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
        rejects(ConfigUpdate::BlankFaces([1_000_000, 2_000_000, 0]), 6511);
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

    fn faces_of(value: &ConfigValue) -> [u64; 3] {
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
        let changes = plan(ConfigUpdate::BlankFaces([1_000_000, 2_000_000, 6_000_000]));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, "blank_faces");
        assert_eq!(faces_of(&changes[0].1), DEFAULT_BLANK_FACES);
        assert_eq!(faces_of(&changes[0].2), [1_000_000, 2_000_000, 6_000_000]);
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
            ConfigUpdate::T04Ceiling(3_000_000_000),
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
            ConfigUpdate::BlankFaces([1_000_000, 2_000_000, 6_000_000]),
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
                "t04_ceiling",
                "sweep_cadence_hours",
                "tier_size",
                "price",
                "ticket_target",
                "fee",
                "alloc_equal_bps",
                "alloc_tier_bps",
                "alloc_protocol_bps",
                "blank_faces",
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
    }

    // --- exactly two weight-gated moves --------------------------------------------------------

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
            ConfigUpdate::TierSize(5),
            ConfigUpdate::Allocation {
                equal_bps: 6_000,
                tier_bps: 1_000,
                protocol_bps: 3_000,
            },
        ];
        for update in ungated {
            assert!(!requires_weight_freeze(&pool, &update));
        }
    }

    // --- `ConfigUpdate`'s variant order is the wire discriminant -----------------------------

    /// Hand-written, independent of declaration order: a 12th `ConfigUpdate` variant fails this
    /// match to compile (E0004) rather than silently dropping out of the sweep below. Every test
    /// above this one is symbolic — it constructs a `ConfigUpdate` by name and never inspects
    /// its serialized bytes — so none of them can see a declaration-order swap (e.g.
    /// `AdmissionFloor` ⇄ `AdmissionCeiling`), which compiles clean and relocates which field an
    /// off-chain decoder reading `ConfigChanged`'s leading byte believes moved.
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
            ConfigUpdate::BlankFaces([0, 0, 0]),
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

    // The pin above enumerates the current variants by hand; a variant added without a matching
    // array entry compiles clean and stays invisible to it. Asserting that the first byte past
    // the current range still fails to deserialize closes that gap: a 12th variant makes the
    // byte valid, which reds this test until the iteration array above is updated.
    #[test]
    fn config_update_rejects_out_of_range_discriminant() {
        // See `common/events.rs`'s `config_value_rejects_out_of_range_discriminant`: a padded
        // buffer can't tell "tag out of range" apart from "payload truncated" — a length-prefixed
        // variant reads its length out of the padding itself and fails on that nonsense length no
        // matter how wide the padding is. Assert on borsh's own distinction instead: an unknown
        // enum tag fails with "Unexpected variant index", a truncated payload fails some other
        // way. A single tag byte is enough, since an invalid tag is rejected before any payload is
        // read.
        let mut buf: &[u8] = &[11u8];
        let err = match ConfigUpdate::deserialize(&mut buf) {
            Ok(_) => panic!(
                "byte 11 deserialized into a ConfigUpdate — a 12th variant was added; add it to \
                 config_update_variant_order_is_the_wire_discriminant's iteration array"
            ),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("Unexpected variant index"),
            "byte 11 failed to deserialize for a reason other than an out-of-range tag — a 12th \
             variant may have been added with a payload that also fails on truncation, masking \
             this probe; add the variant to \
             config_update_variant_order_is_the_wire_discriminant's iteration array \
             (borsh said: {err})"
        );
    }
}
