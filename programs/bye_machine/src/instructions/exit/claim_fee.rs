//! Pays out a position's accrued fee share. An `Active` position stays `Active`, in custody, and —
//! if it was one — a tier member. A closed position (`ClosedBelowFloor` or `Seized`) is paid the
//! `accrued` its closure stored and keeps its state, vault and card claim.
//!
//! **Not `apply_withdraw` with the counters and custody deleted, for the same reason
//! `close_seized`'s `Active` row is not: same fee-settle machinery, different preconditions and
//! a different disposition.** `apply_withdraw` requires the lock to have expired and the roll
//! gate to be clear, because it is about to remove a Fenwick leaf and hand back the card; this
//! instruction changes neither `w_real`, `n_real`, `pool.blanks` nor any `WalletStats` counter,
//! so it needs neither guard — a depositor may claim mid-lock and mid-batch precisely because
//! nothing here touches the domain either one protects. The two legs still fold `accrued` in
//! exactly once, and the tier leg still restamps `tier_checkpoint`, but membership survives: a
//! member keeps `in_tier` and its `TopTier` seat, which is the entire point — the alternative is
//! `withdraw`, and this instruction exists so a depositor does not have to call it just to be
//! paid what they have already earned.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};

use crate::common::errors::ByeMachineError;
use crate::common::events::FeeClaimed;
use crate::common::math::position_fee_payout;
use crate::common::seeds::{POOL_SEED, POSITION_SEED, PRINCIPAL_VAULT_SEED, TOP_TIER_SEED};
use crate::state::{Pool, Position, PositionState, TopTier};

/// `pool.owed_fees` is decremented by exactly what this returns. Zero is a legitimate result, not
/// a precondition failure: a position with nothing owed may still call this, exactly as a
/// zero-fee `withdraw` may. The match is exhaustive so that a new state must be ruled payable or
/// not.
pub(crate) fn apply_claim_fee(
    pool: &mut Pool,
    position: &mut Position,
    acc_tier: u128,
) -> Result<u64> {
    match position.state {
        PositionState::Active => settle_active_fee(pool, position, acc_tier),
        PositionState::ClosedBelowFloor | PositionState::Seized => {
            pay_closed_accrued(pool, position)
        }
        PositionState::Pending | PositionState::Rejected => {
            err!(ByeMachineError::InvalidPositionState)
        }
    }
}

/// Pays a closed position the `accrued` its closure stored, and nothing else. The closure already
/// settled both legs into `accrued` and left the weight, so accumulator growth since then is not
/// this position's: neither checkpoint is read or stamped. Shared by the fee claims and by the
/// card claims, which pay it before closing the account.
pub(crate) fn pay_closed_accrued(pool: &mut Pool, position: &mut Position) -> Result<u64> {
    require!(
        matches!(
            position.state,
            PositionState::ClosedBelowFloor | PositionState::Seized
        ),
        ByeMachineError::InvalidPositionState
    );

    let fees_paid = position.accrued;
    position.accrued = 0;
    pool.owed_fees = pool
        .owed_fees
        .checked_sub(fees_paid)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    Ok(fees_paid)
}

/// The equal leg, settled always, and the tier leg, settled only for a member — `accrued`
/// discharged in full.
fn settle_active_fee(pool: &mut Pool, position: &mut Position, acc_tier: u128) -> Result<u64> {
    let carried = if position.in_tier {
        let settled = position_fee_payout(acc_tier, position.tier_checkpoint, position.accrued)?;
        position.tier_checkpoint = acc_tier;
        settled
    } else {
        position.accrued
    };
    let fees_paid = position_fee_payout(pool.acc_equal, position.equal_checkpoint, carried)?;
    position.equal_checkpoint = pool.acc_equal;
    position.accrued = 0;
    pool.owed_fees = pool
        .owed_fees
        .checked_sub(fees_paid)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    Ok(fees_paid)
}

pub fn handler(ctx: Context<ClaimFee>) -> Result<()> {
    let clock = Clock::get()?;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let depositor = ctx.accounts.depositor.key();
    let acc_tier = ctx.accounts.top_tier.acc_tier;

    let fees_paid = apply_claim_fee(&mut ctx.accounts.pool, &mut ctx.accounts.position, acc_tier)?;

    if fees_paid > 0 {
        let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();
        let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];
        let pool_signer: &[&[&[u8]]] = &[pool_seeds];

        transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.principal_vault.to_account_info(),
                    to: ctx.accounts.depositor_usdc.to_account_info(),
                    authority: ctx.accounts.pool.to_account_info(),
                },
                pool_signer,
            ),
            fees_paid,
        )?;
    }

    emit!(FeeClaimed {
        pool: pool_key,
        slot: clock.slot,
        position: position_key,
        depositor,
        fees_paid,
    });

    Ok(())
}

/// Depositor-signed, and carries no NFT-custody account at all: no fee-claim path in this
/// program moves a card or reads a token standard, so one instruction serves all three rather
/// than splitting into twins the way every custody-moving exit does.
#[derive(Accounts)]
pub struct ClaimFee<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

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
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub depositor_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::ACC_PRECISION;
    use crate::common::test_support::{calls, field_writes, production, writes};
    use crate::state::position::ALL_POSITION_STATES;

    const CLAIM_FEE_SRC: &str = include_str!("claim_fee.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn blank_position() -> Position {
        Position {
            pool: Pubkey::default(),
            depositor: Pubkey::default(),
            nft_mint: Pubkey::default(),
            position_id: 0,
            state: PositionState::Active,
            recorded_value: 0,
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
            standard: 0,
            bump: 0,
            vault_bump: 0,
        }
    }

    #[test]
    fn folds_accrued_in_exactly_once_across_both_legs_for_a_tier_member() {
        let mut pool = Pool::blank();
        pool.acc_equal = 5 * ACC_PRECISION;
        pool.owed_fees = 100;
        let mut position = blank_position();
        position.in_tier = true;
        position.equal_checkpoint = 3 * ACC_PRECISION;
        position.tier_checkpoint = 4 * ACC_PRECISION;
        position.accrued = 7;

        let fees_paid = apply_claim_fee(&mut pool, &mut position, 9 * ACC_PRECISION).unwrap();

        assert_eq!(
            fees_paid, 14,
            "equal delta 2 + tier delta 5 + accrued 7, counted once"
        );
        assert_ne!(fees_paid, 21, "accrued double-counted across legs");
        assert_eq!(pool.owed_fees, 86);
        assert_eq!(position.accrued, 0);
        assert_eq!(position.equal_checkpoint, 5 * ACC_PRECISION);
        assert_eq!(position.tier_checkpoint, 9 * ACC_PRECISION);
    }

    #[test]
    fn a_non_members_payout_never_draws_against_a_stale_tier_checkpoint() {
        let mut pool = Pool::blank();
        pool.acc_equal = 5 * ACC_PRECISION;
        pool.owed_fees = 100;
        let mut position = blank_position();
        position.in_tier = false;
        position.equal_checkpoint = 3 * ACC_PRECISION;
        position.tier_checkpoint = 4 * ACC_PRECISION;
        position.accrued = 7;

        let fees_paid = apply_claim_fee(&mut pool, &mut position, 9 * ACC_PRECISION).unwrap();

        assert_eq!(fees_paid, 9, "equal delta 2 + accrued 7, no tier leg");
        assert_eq!(
            position.tier_checkpoint,
            4 * ACC_PRECISION,
            "a non-member's tier checkpoint is not restamped"
        );
    }

    #[test]
    fn membership_and_state_survive_the_claim() {
        let mut pool = Pool::blank();
        pool.owed_fees = 0;
        let mut position = blank_position();
        position.in_tier = true;
        position.tier_checkpoint = 0;
        position.recorded_value = 12_000_000;
        position.slot_index = 7;

        apply_claim_fee(&mut pool, &mut position, 0).unwrap();

        assert!(position.in_tier, "a tier member stays a tier member");
        assert!(position.state == PositionState::Active);
        assert_eq!(position.recorded_value, 12_000_000, "unmoved");
        assert_eq!(position.slot_index, 7, "unmoved");
    }

    #[test]
    fn a_zero_accrued_claim_pays_exactly_zero_and_stamps_the_checkpoint() {
        let mut pool = Pool::blank();
        pool.acc_equal = 3 * ACC_PRECISION;
        let mut position = blank_position();
        position.equal_checkpoint = 3 * ACC_PRECISION;

        let fees_paid = apply_claim_fee(&mut pool, &mut position, 0).unwrap();

        assert_eq!(fees_paid, 0);
        assert_eq!(pool.owed_fees, 0);
    }

    #[test]
    fn a_repeated_claim_with_no_new_accrual_pays_zero_the_second_time() {
        let mut pool = Pool::blank();
        pool.acc_equal = 5 * ACC_PRECISION;
        pool.owed_fees = 100;
        let mut position = blank_position();
        position.equal_checkpoint = 3 * ACC_PRECISION;
        position.accrued = 7;

        let first = apply_claim_fee(&mut pool, &mut position, 0).unwrap();
        assert_eq!(first, 9);

        let second = apply_claim_fee(&mut pool, &mut position, 0).unwrap();
        assert_eq!(second, 0, "nothing new accrued since the first claim");
        assert_eq!(pool.owed_fees, 91);
    }

    #[test]
    fn an_owed_fees_under_run_fails_closed_at_6703() {
        let mut pool = Pool::blank();
        pool.acc_equal = 5 * ACC_PRECISION;
        pool.owed_fees = 1;
        let mut position = blank_position();
        position.accrued = 7;

        let err = apply_claim_fee(&mut pool, &mut position, 0).unwrap_err();
        assert_eq!(error_code(err), 6703);
    }

    #[test]
    fn pending_and_rejected_reject_at_6301_and_every_other_state_is_payable() {
        for state in ALL_POSITION_STATES {
            let mut pool = Pool::blank();
            let mut position = blank_position();
            position.state = state;
            let result = apply_claim_fee(&mut pool, &mut position, 0);
            match state {
                PositionState::Pending | PositionState::Rejected => {
                    assert_eq!(
                        error_code(result.unwrap_err()),
                        6301,
                        "state {}",
                        state as u8
                    )
                }
                PositionState::Active | PositionState::ClosedBelowFloor | PositionState::Seized => {
                    assert!(result.is_ok(), "state {}", state as u8)
                }
            }
        }
    }

    const CLOSED_STATES: [PositionState; 2] =
        [PositionState::ClosedBelowFloor, PositionState::Seized];

    /// The closure settled both legs into `accrued`; growth on either accumulator since then
    /// belongs to the positions still carrying weight.
    #[test]
    fn a_closed_position_is_paid_its_stored_accrued_alone_and_no_growth_after_closure() {
        for state in CLOSED_STATES {
            let mut pool = Pool::blank();
            pool.acc_equal = 9 * ACC_PRECISION;
            pool.owed_fees = 100;
            let mut position = blank_position();
            position.state = state;
            position.in_tier = true;
            position.equal_checkpoint = 3 * ACC_PRECISION;
            position.tier_checkpoint = 4 * ACC_PRECISION;
            position.accrued = 7;

            let fees_paid = apply_claim_fee(&mut pool, &mut position, 20 * ACC_PRECISION).unwrap();

            assert_eq!(
                fees_paid, 7,
                "state {}: the stored accrued, no leg settled",
                state as u8
            );
            assert_eq!(pool.owed_fees, 93);
            assert_eq!(position.accrued, 0);
            assert_eq!(position.equal_checkpoint, 3 * ACC_PRECISION, "untouched");
            assert_eq!(position.tier_checkpoint, 4 * ACC_PRECISION, "untouched");
            assert!(position.state == state, "the position stays closed");
        }
    }

    #[test]
    fn a_closed_position_owing_nothing_pays_zero_and_a_repeat_claim_pays_zero() {
        for state in CLOSED_STATES {
            let mut pool = Pool::blank();
            pool.owed_fees = 10;
            let mut position = blank_position();
            position.state = state;
            position.accrued = 4;

            assert_eq!(apply_claim_fee(&mut pool, &mut position, 0).unwrap(), 4);
            assert_eq!(apply_claim_fee(&mut pool, &mut position, 0).unwrap(), 0);
            assert_eq!(pool.owed_fees, 6);
        }
    }

    #[test]
    fn a_closed_positions_owed_fees_under_run_fails_closed_at_6703() {
        for state in CLOSED_STATES {
            let mut pool = Pool::blank();
            pool.owed_fees = 1;
            let mut position = blank_position();
            position.state = state;
            position.accrued = 7;
            let err = apply_claim_fee(&mut pool, &mut position, 0).unwrap_err();
            assert_eq!(error_code(err), 6703);
        }
    }

    /// The card claims call this directly, with no `apply_claim_fee` match in front of it.
    #[test]
    fn pay_closed_accrued_refuses_every_open_state_at_6301() {
        for state in ALL_POSITION_STATES {
            let mut pool = Pool::blank();
            let mut position = blank_position();
            position.state = state;
            let result = pay_closed_accrued(&mut pool, &mut position);
            if CLOSED_STATES.contains(&state) {
                assert!(result.is_ok(), "state {}", state as u8);
            } else {
                assert_eq!(
                    error_code(result.unwrap_err()),
                    6301,
                    "state {}",
                    state as u8
                );
            }
        }
    }

    #[test]
    fn a_locked_position_may_still_claim() {
        let mut pool = Pool::blank();
        let mut position = blank_position();
        position.lock_until = i64::MAX;
        assert!(apply_claim_fee(&mut pool, &mut position, 0).is_ok());
    }

    #[test]
    fn an_open_batch_does_not_block_a_claim() {
        let mut pool = Pool::blank();
        pool.open_batches = 3;
        let mut position = blank_position();
        assert!(apply_claim_fee(&mut pool, &mut position, 0).is_ok());
    }

    #[test]
    fn the_transfer_is_skipped_at_zero_and_wired_from_the_returned_payout() {
        let prod = production(CLAIM_FEE_SRC);
        assert!(prod.contains("if fees_paid > 0 {"));
        assert_eq!(calls(CLAIM_FEE_SRC, "transfer"), 1);
        assert!(prod.contains("fees_paid,\n"));
    }

    #[test]
    fn the_file_writes_exactly_the_fields_the_settle_owns() {
        assert_eq!(field_writes(CLAIM_FEE_SRC, "pool"), ["owed_fees"]);
        assert_eq!(
            field_writes(CLAIM_FEE_SRC, "position"),
            ["accrued", "equal_checkpoint", "tier_checkpoint"]
        );
        assert_eq!(
            writes(CLAIM_FEE_SRC, "position.state"),
            0,
            "this instruction never transitions the position's state"
        );
        assert_eq!(
            writes(CLAIM_FEE_SRC, "position.in_tier"),
            0,
            "this instruction never changes tier membership"
        );
        assert_eq!(
            writes(CLAIM_FEE_SRC, "position.slot_index"),
            0,
            "this instruction never touches the Fenwick leaf"
        );
    }

    #[test]
    fn reads_no_pause_flag_and_no_weight_guard() {
        let prod = production(CLAIM_FEE_SRC);
        assert_eq!(prod.matches("deposits_paused").count(), 0);
        assert_eq!(prod.matches("rolls_paused").count(), 0);
        assert_eq!(calls(CLAIM_FEE_SRC, "assert_weight_open"), 0);
    }

    #[test]
    fn no_custody_or_weight_index_surface_is_declared() {
        let prod = production(CLAIM_FEE_SRC);
        for absent in [
            "weight_index",
            "wallet_stats",
            "position_vault",
            "release_from_vault",
            ".remove(",
        ] {
            assert!(
                !prod.contains(absent),
                "claim_fee unexpectedly references {absent}"
            );
        }
    }

    #[test]
    fn depositor_is_the_only_signer_bound_to_the_positions_own_depositor() {
        let prod = production(CLAIM_FEE_SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(prod.contains(
            "constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
    }
}
