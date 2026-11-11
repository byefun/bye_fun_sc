use anchor_lang::prelude::*;

use crate::common::constants::{
    ABOVE_TICKET_SLOT, BELOW_TICKET_SLOTS, BLANK_SLOTS, MIN_ABOVE_TICKET_GAP,
};
use crate::common::errors::ByeMachineError;
use crate::common::seeds::TREASURY_02_SEED;
use crate::state::Pool;

/// The blank-face slot rules and the admission-floor guard, checked on a pool's effective config —
/// the stored values merged with the update — so a change on either side of a rule is refused.
///
/// - Slots 0–2 each hold a face above 0 and below the ticket.
/// - Slot 3 holds 0 or a face at least `MIN_ABOVE_TICKET_GAP` above the ticket.
/// - Slots 4–7 hold 0.
/// - A floor at or below the ticket needs slot 3 set: only the above-ticket blank keeps a roll's
///   expected payout at the ticket once the cards can average below it.
pub fn validate_blank_faces(
    ticket_target: u64,
    admission_floor: u64,
    blank_faces: &[u64; BLANK_SLOTS],
) -> Result<()> {
    for face in &blank_faces[..BELOW_TICKET_SLOTS] {
        require!(*face > 0, ByeMachineError::BlankFaceZero);
        require!(
            *face < ticket_target,
            ByeMachineError::BlankFaceNotBelowTicket
        );
    }

    let above_face = blank_faces[ABOVE_TICKET_SLOT];
    if above_face != 0 {
        let least = ticket_target
            .checked_add(MIN_ABOVE_TICKET_GAP)
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        require!(
            above_face >= least,
            ByeMachineError::AboveTicketBlankTooClose
        );
    }

    for face in &blank_faces[ABOVE_TICKET_SLOT + 1..BLANK_SLOTS] {
        require!(*face == 0, ByeMachineError::ReservedBlankSlotSet);
    }

    require!(
        admission_floor > ticket_target || above_face != 0,
        ByeMachineError::FloorNeedsAboveTicketBlank
    );
    Ok(())
}

/// The weight-freeze guard: rejects while `pool.open_batches > 0`, since an open batch's
/// draw domain must stay fixed until it resolves.
///
/// Call sites: `approve_deposit`, `activate_acquired`, `record_value`, `withdraw`, and
/// `update_config` — one site, behind `requires_weight_freeze`, covering both its `BlankFaces`
/// and `Pricing.ticket_target` branches — plus `close_seized`'s `Active` row, the one row of that
/// instruction that departs weight.
///
/// **`commit_rolls` deliberately does not call it.** A commit queues behind whatever batch is
/// already open; ordering across the queue is enforced by `Pool::head_batch_id()`, which
/// `settle_roll`, `settle_roll_core`, and `recover_batch` all check first.
///
/// **`instant_sell` and its Core twin deliberately do not carry it.** Paying the seller and
/// moving the card change no position's weight, so the sell itself is never refused by an open
/// batch; the position it creates takes the rule instead, going `Pending` and reaching the draw
/// domain later through `activate_acquired`, which is where the guard sits. Putting it on the
/// sell would refuse a purchase the pool can safely make.
///
/// Never called by `deposit`, `reject_deposit`, `return_rejected`, `claim_nft`, or `update_tier`
/// — none of them moves `W`.
pub fn assert_weight_open(pool: &Pool) -> Result<()> {
    require_eq!(pool.open_batches, 0, ByeMachineError::RollBatchInFlight);
    Ok(())
}

/// Whether a position is one a sale created: it carries the accepted value `instant_sell` stores
/// on it, whoever its depositor — the Treasury 02 identity or the loader whose escrow paid.
///
/// A depositor's own `Pending` position carries zero there, as `deposit` and `t02_deposit` write
/// it, and `record_value` writes only an `Active` position. A sale accepts only a value at or
/// above the admission floor, so a position it created never holds zero.
fn is_acquired_position(recorded_value: u64) -> bool {
    recorded_value != 0
}

/// The acquisition exclusion: a `Pending` position a sale created leaves `Pending` only through
/// `activate_acquired`, on the value already paid against. `approve_deposit` would re-attest it
/// and `reject_deposit` would route it out of the pool, so both refuse it.
pub fn assert_not_acquired_position(recorded_value: u64) -> Result<()> {
    require!(
        !is_acquired_position(recorded_value),
        ByeMachineError::TreasuryPositionNotEligible
    );
    Ok(())
}

/// The acquisition inclusion — `assert_not_acquired_position`'s opposite polarity, and the guard
/// that makes `activate_acquired` serve positions a sale created *only*.
///
/// Both directions are needed and neither implies the other. Without the exclusion, an acquired
/// position leaves `Pending` through `approve_deposit`, which re-attests a card already paid for.
/// Without this one, a `Pending` position with no stored attestation activates through
/// `activate_acquired` on a value nobody attested.
pub fn assert_acquired_position(recorded_value: u64) -> Result<()> {
    require!(
        is_acquired_position(recorded_value),
        ByeMachineError::NotATreasuryPosition
    );
    Ok(())
}

/// Where a returned or released card goes: the position's own depositor, except for a Treasury 02
/// position, whose stored depositor is a program address with no key. Its card goes to the wallet
/// configured as the T02 authority at the time of the call.
pub fn card_recipient(pool: &Pubkey, depositor: &Pubkey, t02_authority: &Pubkey) -> Pubkey {
    let (t02_identity, _) =
        Pubkey::find_program_address(&[TREASURY_02_SEED, pool.as_ref()], &crate::ID);
    if *depositor == t02_identity {
        *t02_authority
    } else {
        *depositor
    }
}

/// The admission limits, checked against the attested value and the wallet's already-active
/// value. Every limit in force is a published `Pool` field, `0` encodes "not configured", and
/// each configured limit is inclusive at its boundary.
///
/// `record_value` deliberately does **not** call it: the helper opens on
/// `value >= admission_floor`, which is the exact condition that instruction's below-floor branch
/// exists to handle.
pub fn validate_admission_limits(pool: &Pool, value: u64, wallet_active_value: u64) -> Result<()> {
    validate_admission_value(pool, value)?;
    require!(
        fits_wallet_cap(pool, value, wallet_active_value)?,
        ByeMachineError::WalletValueCapExceeded
    );
    Ok(())
}

/// The limits on the value alone: not zero, at or above the floor, and at or below the ceiling
/// when one is configured. The sale checks these before it knows who pays, because they hold for
/// every payer.
pub fn validate_admission_value(pool: &Pool, value: u64) -> Result<()> {
    require!(value > 0, ByeMachineError::ZeroValue);
    require!(
        value >= pool.admission_floor,
        ByeMachineError::BelowAdmissionFloor
    );

    if pool.admission_ceiling > 0 {
        require!(
            value <= pool.admission_ceiling,
            ByeMachineError::AboveAdmissionCeiling
        );
    }

    Ok(())
}

/// Whether `value` fits under the pool's per-wallet cap on top of `wallet_active_value`. Always
/// true when no cap is configured. Returned rather than raised, because the sale reads it against
/// the loader to choose the payer, where failing it is not an error.
pub fn fits_wallet_cap(pool: &Pool, value: u64, wallet_active_value: u64) -> Result<bool> {
    if pool.wallet_value_cap == 0 {
        return Ok(true);
    }
    let wallet_total = wallet_active_value
        .checked_add(value)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    Ok(wallet_total <= pool.wallet_value_cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn pool_with_open_batches(open_batches: u32) -> Pool {
        Pool {
            pool_id: 0,
            weight_index: Pubkey::default(),
            treasury_03: Pubkey::default(),
            price: 0,
            ticket_target: 0,
            fee: 0,
            alloc_equal_bps: 0,
            alloc_tier_bps: 0,
            alloc_protocol_bps: 0,
            admission_floor: 0,
            admission_ceiling: 0,
            wallet_value_cap: 0,
            max_n: 0,
            buyback_rate_bps: 0,
            blank_faces: [0; 8],
            t04_ceiling: 0,
            sweep_cadence_hours: 0,
            tier_size: 0,
            deposits_paused: false,
            rolls_paused: false,
            pause_reason: 0,
            n_real: 0,
            blanks: [0; 8],
            w_real: 0,
            open_batches,
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

    // --- both polarities --------------------------------------------------------------------

    #[test]
    fn rejects_with_roll_batch_in_flight_when_a_batch_is_open() {
        let pool = pool_with_open_batches(1);
        assert_eq!(error_code(assert_weight_open(&pool).unwrap_err()), 6200);
    }

    #[test]
    fn passes_when_no_batch_is_open() {
        let pool = pool_with_open_batches(0);
        assert!(assert_weight_open(&pool).is_ok());
    }

    #[test]
    fn rejects_regardless_of_how_many_batches_are_open() {
        for open_batches in [2u32, 5, u32::MAX] {
            let pool = pool_with_open_batches(open_batches);
            assert_eq!(error_code(assert_weight_open(&pool).unwrap_err()), 6200);
        }
    }

    // --- acquisition exclusion and inclusion ----------------------------------------------------

    fn t02_identity(pool: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[TREASURY_02_SEED, pool.as_ref()], &crate::ID).0
    }

    #[test]
    fn rejects_an_acquired_position_whoever_its_depositor() {
        assert_eq!(
            error_code(assert_not_acquired_position(12_000_000).unwrap_err()),
            6303
        );
    }

    #[test]
    fn passes_a_position_which_carries_no_stored_value() {
        assert!(assert_not_acquired_position(0).is_ok());
    }

    #[test]
    fn the_inclusion_accepts_a_position_with_a_stored_value() {
        assert!(assert_acquired_position(12_000_000).is_ok());
    }

    #[test]
    fn the_inclusion_rejects_a_position_with_no_stored_value_with_its_own_code() {
        assert_eq!(error_code(assert_acquired_position(0).unwrap_err()), 6311);
    }

    /// The two guards are complements: a position passes exactly one of them.
    #[test]
    fn the_two_acquisition_guards_are_exact_complements() {
        for value in [0, 1, 12_000_000, u64::MAX] {
            assert_ne!(
                assert_not_acquired_position(value).is_ok(),
                assert_acquired_position(value).is_ok(),
                "the two guards agreed on {value}"
            );
        }
    }

    // --- card recipient ---------------------------------------------------------------------

    #[test]
    fn a_treasury_positions_card_goes_to_the_configured_t02_authority() {
        let pool = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        assert_eq!(
            card_recipient(&pool, &t02_identity(&pool), &authority),
            authority
        );
    }

    #[test]
    fn any_other_positions_card_goes_to_its_own_depositor() {
        let pool = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let depositor = Pubkey::new_unique();
        assert_eq!(card_recipient(&pool, &depositor, &authority), depositor);
        // Another pool's identity is an ordinary depositor here, so it is not redirected.
        let other = t02_identity(&Pubkey::new_unique());
        assert_eq!(card_recipient(&pool, &other, &authority), other);
    }

    // --- the admission limits ---------------------------------------------------

    fn pool_with_limits(admission_ceiling: u64, wallet_value_cap: u64) -> Pool {
        Pool {
            pool_id: 0,
            weight_index: Pubkey::default(),
            treasury_03: Pubkey::default(),
            price: 0,
            ticket_target: DEFAULT_TICKET_TARGET,
            fee: 0,
            alloc_equal_bps: 0,
            alloc_tier_bps: 0,
            alloc_protocol_bps: 0,
            admission_floor: DEFAULT_ADMISSION_FLOOR,
            admission_ceiling,
            wallet_value_cap,
            max_n: 0,
            buyback_rate_bps: 0,
            blank_faces: DEFAULT_BLANK_FACES,
            t04_ceiling: 0,
            sweep_cadence_hours: 0,
            tier_size: 0,
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

    // --- no position activates on a zero value ----------------------------------------------

    #[test]
    fn rejects_a_zero_value() {
        let pool = pool_with_limits(0, 0);
        assert_eq!(
            error_code(validate_admission_limits(&pool, 0, 0).unwrap_err()),
            6401
        );
    }

    #[test]
    fn rejects_a_zero_value_even_with_no_floor_configured() {
        let mut pool = pool_with_limits(0, 0);
        pool.admission_floor = 0;
        assert_eq!(
            error_code(validate_admission_limits(&pool, 0, 0).unwrap_err()),
            6401
        );
    }

    // --- each limit, inclusive at its boundary ----------------------------------------------

    #[test]
    fn accepts_a_value_at_exactly_the_admission_floor() {
        let pool = pool_with_limits(0, 0);
        assert!(validate_admission_limits(&pool, DEFAULT_ADMISSION_FLOOR, 0).is_ok());
    }

    #[test]
    fn rejects_a_value_one_below_the_admission_floor() {
        let pool = pool_with_limits(0, 0);
        assert_eq!(
            error_code(
                validate_admission_limits(&pool, DEFAULT_ADMISSION_FLOOR - 1, 0).unwrap_err()
            ),
            6104
        );
    }

    #[test]
    fn accepts_a_value_at_exactly_the_admission_ceiling() {
        let ceiling = DEFAULT_ADMISSION_FLOOR * 4;
        let pool = pool_with_limits(ceiling, 0);
        assert!(validate_admission_limits(&pool, ceiling, 0).is_ok());
    }

    #[test]
    fn rejects_a_value_one_above_the_admission_ceiling() {
        let ceiling = DEFAULT_ADMISSION_FLOOR * 4;
        let pool = pool_with_limits(ceiling, 0);
        assert_eq!(
            error_code(validate_admission_limits(&pool, ceiling + 1, 0).unwrap_err()),
            6105
        );
    }

    #[test]
    fn accepts_a_wallet_total_at_exactly_the_wallet_value_cap() {
        let cap = DEFAULT_ADMISSION_FLOOR * 4;
        let pool = pool_with_limits(0, cap);
        let already_active = cap - DEFAULT_ADMISSION_FLOOR;
        assert!(validate_admission_limits(&pool, DEFAULT_ADMISSION_FLOOR, already_active).is_ok());
    }

    #[test]
    fn rejects_a_wallet_total_one_above_the_wallet_value_cap() {
        let cap = DEFAULT_ADMISSION_FLOOR * 4;
        let pool = pool_with_limits(0, cap);
        let already_active = cap - DEFAULT_ADMISSION_FLOOR + 1;
        assert_eq!(
            error_code(
                validate_admission_limits(&pool, DEFAULT_ADMISSION_FLOOR, already_active)
                    .unwrap_err()
            ),
            6106
        );
    }

    #[test]
    fn rejects_a_wallet_total_that_would_overflow_rather_than_wrapping_under_the_cap() {
        let pool = pool_with_limits(0, u64::MAX);
        assert_eq!(
            error_code(validate_admission_limits(&pool, u64::MAX, u64::MAX).unwrap_err()),
            6703
        );
    }

    // --- Profile 1: an unset ceiling and cap are no limit, not a zero limit -----------------

    #[test]
    fn accepts_any_value_above_the_floor_when_no_ceiling_is_configured() {
        let pool = pool_with_limits(0, 0);
        assert!(validate_admission_limits(&pool, u64::MAX, 0).is_ok());
    }

    #[test]
    fn accepts_any_wallet_total_when_no_wallet_cap_is_configured() {
        let pool = pool_with_limits(0, 0);
        assert!(validate_admission_limits(&pool, DEFAULT_ADMISSION_FLOOR, u64::MAX).is_ok());
    }
}
