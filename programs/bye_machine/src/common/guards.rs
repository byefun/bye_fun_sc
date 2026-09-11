use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::seeds::TREASURY_02_SEED;
use crate::state::Pool;

/// The weight-freeze guard: rejects while `pool.open_batches > 0`, since an open batch's
/// draw domain must stay fixed until it resolves.
///
/// Slice-1 call sites (5): `approve_deposit`, `record_value`, `withdraw`, and `update_config` —
/// one site, behind `requires_weight_freeze`, covering both its `BlankFaces` and
/// `Pricing.ticket_target` branches — plus `close_seized`'s `Active` row, the one row of that
/// instruction that departs weight. `activate_acquired` and `instant_sell` are round 2 and are
/// not wired to this guard here.
///
/// Never called by `deposit`, `reject_deposit`, `return_rejected`, `claim_nft`, or `update_tier`
/// — none of them moves `W`.
pub fn assert_weight_open(pool: &Pool) -> Result<()> {
    require_eq!(pool.open_batches, 0, ByeMachineError::RollBatchInFlight);
    Ok(())
}

/// The treasury exclusion: a position whose depositor is the pool's Treasury 02 identity
/// PDA leaves `Pending` only through `activate_acquired`, on the value Treasury 03 already paid
/// against. `approve_deposit` would re-attest it and `reject_deposit` would route it out of the
/// pool through permissionless `return_rejected`, so both refuse it.
pub fn assert_not_treasury_position(pool: &Pubkey, depositor: &Pubkey) -> Result<()> {
    let (t02_identity, _) =
        Pubkey::find_program_address(&[TREASURY_02_SEED, pool.as_ref()], &crate::ID);
    require_keys_neq!(
        *depositor,
        t02_identity,
        ByeMachineError::TreasuryPositionNotEligible
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            blank_faces: [0, 0, 0],
            t04_ceiling: 0,
            sweep_cadence_hours: 0,
            tier_size: 0,
            deposits_paused: false,
            rolls_paused: false,
            pause_reason: 0,
            n_real: 0,
            blanks: [0, 0, 0],
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

    // --- treasury exclusion -----------------------------------------------------------------

    fn t02_identity(pool: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[TREASURY_02_SEED, pool.as_ref()], &crate::ID).0
    }

    #[test]
    fn rejects_a_position_whose_depositor_is_this_pools_treasury_identity() {
        let pool = Pubkey::new_unique();
        assert_eq!(
            error_code(assert_not_treasury_position(&pool, &t02_identity(&pool)).unwrap_err()),
            6303
        );
    }

    #[test]
    fn passes_for_an_ordinary_depositor() {
        let pool = Pubkey::new_unique();
        assert!(assert_not_treasury_position(&pool, &Pubkey::new_unique()).is_ok());
    }

    /// The identity is per-pool, so another pool's t02 PDA is an ordinary depositor here.
    #[test]
    fn passes_for_another_pools_treasury_identity() {
        let pool = Pubkey::new_unique();
        let other_pool = Pubkey::new_unique();
        assert!(assert_not_treasury_position(&pool, &t02_identity(&other_pool)).is_ok());
    }
}
