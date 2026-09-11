use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::{DepositApproved, RebalanceEvaluated, TierChanged};
use crate::common::guards::{assert_not_treasury_position, assert_weight_open};
use crate::common::math::weight_of;
use crate::common::rebalance;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED, WALLET_STATS_SEED,
};
use crate::common::tier::{self, PromoteOutcome};
use crate::state::{
    Pool, Position, PositionState, ProtocolConfig, TierEntry, TopTier, WalletStats, WeightIndex,
};

/// The admission limits, checked against the attested value and the wallet's already-active
/// value. Every limit in force is a published `Pool` field, `0` encodes "not configured", and
/// each configured limit is inclusive at its boundary.
fn validate_admission_limits(pool: &Pool, value: u64, wallet_active_value: u64) -> Result<()> {
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

    if pool.wallet_value_cap > 0 {
        let wallet_total = wallet_active_value
            .checked_add(value)
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        require!(
            wallet_total <= pool.wallet_value_cap,
            ByeMachineError::WalletValueCapExceeded
        );
    }

    Ok(())
}

/// `lock_until` is the Season-0 lock, supplied by the attestor as `season.tge_at + 14 days` for
/// a Season-0 deposit and `0` otherwise. It is the only writer of this field: `withdraw` guards
/// on `now >= lock_until` and `record_value`'s below-floor branch retains a locked position
/// instead of closing it.
///
/// Taken as drawn and **not validated** — no bound, and no comparison against `observed_at` or
/// `now`. The Operator can therefore lock a position arbitrarily far out; that is not a new
/// authority, since the same signer already attests `value` and so decides admission, weight,
/// tier rank and payout on this very call, but nothing on chain bounds it.
///
/// `observed_at` and `lock_until` are adjacent `i64`s. Transposing them sets the lock to the
/// attestation timestamp — no lock at all — and is invisible to the IDL oracle, which compares an
/// encoder against an IDL generated from this same source. `approve_deposit_binds_its_two_i64_args_uncrossed`
/// is the pin; do not reorder these parameters or those two assignments.
pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, ApproveDeposit<'info>>,
    value: u64,
    observed_at: i64,
    lock_until: i64,
) -> Result<()> {
    require!(
        ctx.accounts.position.state == PositionState::Pending,
        ByeMachineError::PositionNotPending
    );
    assert_not_treasury_position(&ctx.accounts.pool.key(), &ctx.accounts.position.depositor)?;
    assert_weight_open(&ctx.accounts.pool)?;
    validate_admission_limits(
        &ctx.accounts.pool,
        value,
        ctx.accounts.wallet_stats.active_value,
    )?;

    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let authority = ctx.accounts.operator.key();

    let weight = weight_of(value)?;
    ctx.accounts.pool.w_real = ctx
        .accounts
        .pool
        .w_real
        .checked_add(u128::from(weight))
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    ctx.accounts.pool.n_real = ctx
        .accounts
        .pool
        .n_real
        .checked_add(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let expected_total = WeightIndex::narrow_expected_total(ctx.accounts.pool.w_real)?;
    let slot_index = ctx
        .accounts
        .weight_index
        .load_mut()?
        .insert(value, expected_total)?;

    let acc_equal = ctx.accounts.pool.acc_equal;
    let position = &mut ctx.accounts.position;
    let position_id = position.position_id;
    position.state = PositionState::Active;
    position.recorded_value = value;
    position.value_observed_at = observed_at;
    position.deposit_value = value;
    position.slot_index = slot_index;
    position.activated_at = now;
    position.equal_checkpoint = acc_equal;
    position.lock_until = lock_until;

    let wallet_stats = &mut ctx.accounts.wallet_stats;
    wallet_stats.active_positions = wallet_stats
        .active_positions
        .checked_add(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    wallet_stats.active_value = wallet_stats
        .active_value
        .checked_add(value)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    let tier_size = ctx.accounts.pool.tier_size;
    let acc_tier = ctx.accounts.top_tier.acc_tier;
    let candidate = TierEntry {
        position: position_key,
        value,
        activated_at: now,
        position_id,
    };
    let (entered_tier, displaced) =
        match tier::promote(&mut ctx.accounts.top_tier, tier_size, candidate) {
            PromoteOutcome::Inserted => (true, None),
            PromoteOutcome::Swapped(evicted) => {
                tier::settle_displaced_member(ctx.remaining_accounts, &evicted, acc_tier)?;
                (true, Some(evicted.position))
            }
            PromoteOutcome::Rejected => (false, None),
        };
    if entered_tier {
        ctx.accounts.position.in_tier = true;
        ctx.accounts.position.tier_checkpoint = acc_tier;
    }

    let blanks_before = ctx.accounts.pool.blanks;
    let targets = rebalance::evaluate(
        ctx.accounts.pool.n_real,
        ctx.accounts.pool.w_real,
        ctx.accounts.pool.blank_faces,
        ctx.accounts.pool.ticket_target,
    )?;
    ctx.accounts.pool.blanks = [targets.b1, targets.b2, targets.b5];

    // `lock_until` is deliberately **not** a field here — it is account-sourced and write-once,
    // so duplicating it into the event would give a reader a second, cheaper-looking source for
    // a value the account already holds. `BelowFloorRetained{lock_until}` remains the only event
    // that logs the field.
    emit!(DepositApproved {
        pool: pool_key,
        slot: clock.slot,
        authority,
        position: position_key,
        value,
        observed_at,
        observed_age_seconds: now
            .checked_sub(observed_at)
            .ok_or(ByeMachineError::ArithmeticFailure)?,
    });
    emit!(RebalanceEvaluated {
        pool: pool_key,
        slot: clock.slot,
        n_real: ctx.accounts.pool.n_real,
        w_real: ctx.accounts.pool.w_real,
        blanks_before,
        blanks_after: ctx.accounts.pool.blanks,
    });
    if entered_tier {
        emit!(TierChanged {
            pool: pool_key,
            slot: clock.slot,
            left: displaced,
            entered: Some(position_key),
        });
    }

    Ok(())
}

#[derive(Accounts)]
pub struct ApproveDeposit<'info> {
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
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump
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

    // --- structural pins over the four deposit-domain handlers -------------------------------

    const DEPOSIT_SRC: &str = include_str!("deposit.rs");
    const APPROVE_SRC: &str = include_str!("approve_deposit.rs");
    const REJECT_SRC: &str = include_str!("reject_deposit.rs");
    const RETURN_SRC: &str = include_str!("return_rejected.rs");

    use crate::common::test_support::{calls, flat, production, writes};

    const OPERATOR_CONSTRAINT: &str =
        "constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized";

    /// A whole-field-set sweep caught this class in `settle_displaced_member`, but not in
    /// `handler`, which takes a `Context` and so is not unit-testable the
    /// same way. Each deletion below leaves the position activated in the tree, in `w_real`
    /// and in `wallet_stats` while its own record disagrees — and three of them are silent.
    #[test]
    fn approve_deposit_writes_every_activation_field() {
        for field in [
            "position.state",
            "position.recorded_value",
            "position.value_observed_at",
            "position.deposit_value",
            "position.slot_index",
            "position.activated_at",
            "position.equal_checkpoint",
            "position.lock_until",
        ] {
            assert_eq!(writes(APPROVE_SRC, field), 1, "{field} is not assigned");
        }
        assert!(production(APPROVE_SRC).contains("position.state = PositionState::Active"));

        for field in ["position.in_tier", "position.tier_checkpoint"] {
            assert_eq!(
                writes(APPROVE_SRC, &format!("ctx.accounts.{field}")),
                1,
                "{field} is not assigned on tier entry"
            );
        }

        assert_eq!(writes(APPROVE_SRC, "wallet_stats.active_positions"), 1);
        assert_eq!(writes(APPROVE_SRC, "wallet_stats.active_value"), 1);
        assert_eq!(writes(APPROVE_SRC, "ctx.accounts.pool.blanks"), 1);
        assert_eq!(writes(APPROVE_SRC, "ctx.accounts.pool.w_real"), 1);
        assert_eq!(writes(APPROVE_SRC, "ctx.accounts.pool.n_real"), 1);

        assert_eq!(calls(APPROVE_SRC, "validate_admission_limits"), 1);
    }

    /// `(value: u64, observed_at: i64, lock_until: i64)` ends in two adjacent `i64`s, so a
    /// transposition compiles, encodes to the same 24 arg bytes, and fails in the quiet direction:
    /// `lock_until` takes the attestation timestamp, `now >= lock_until` passes within seconds, and
    /// the Season-0 lock silently does not exist. **The IDL oracle cannot see it** — it compares
    /// `scripts/lib/deposit.ts` against an IDL generated from this file, so a swap here moves both
    /// sides together and they still agree (`tests/unit/idl-oracle-support.ts` states that scope).
    ///
    /// Two independent forms produce the fault, so both are pinned: the parameter *declarations*
    /// swapping, and the two *assignments* swapping. `writes(...) == 1` on each field is blind to
    /// the second — both fields are still assigned exactly once — so the assignments are asserted
    /// as whole exact statements, not by counting.
    #[test]
    fn approve_deposit_binds_its_two_i64_args_uncrossed() {
        let src = production(APPROVE_SRC);

        assert!(
            src.contains("    value: u64,\n    observed_at: i64,\n    lock_until: i64,\n"),
            "approve_deposit's parameter order moved. It must stay (value, observed_at, \
             lock_until): the two i64s are transposable with no width change and no compile error, \
             and swapping them writes the lock into value_observed_at and the attestation time \
             into lock_until — a position with no lock at all"
        );

        assert!(
            src.contains("position.value_observed_at = observed_at;"),
            "value_observed_at must be assigned from `observed_at`, not from `lock_until`"
        );
        assert!(
            src.contains("position.lock_until = lock_until;"),
            "lock_until must be assigned from `lock_until`, not from `observed_at` and not from a \
             literal — a literal 0 passes every presence pin and silently drops the Season-0 lock"
        );

        // The handler forwards nothing else into these two fields, so the two statements above are
        // the complete binding. Anchored on `= observed_at` / `= lock_until` as right-hand sides so
        // a third field taking either value fails here rather than passing silently.
        assert_eq!(
            src.matches("= observed_at;").count(),
            1,
            "`observed_at` is bound in exactly one place"
        );
        assert_eq!(
            src.matches("= lock_until;").count(),
            1,
            "`lock_until` is bound in exactly one place"
        );
    }

    /// `DepositApproved` must **not** gain a `lock_until` field: `positions.lock_until` is
    /// account-sourced and write-once, and the backend reads it off the `Position` account
    /// rather than off this event. Adding the field is a one-line "improvement" that breaks that
    /// contract, so the omission is pinned rather than left to a comment nobody reads.
    /// `BelowFloorRetained` stays the only event logging it.
    #[test]
    fn deposit_approved_does_not_publish_the_lock() {
        let emit_site = production(APPROVE_SRC)
            .split("emit!(DepositApproved {")
            .nth(1)
            .expect("approve_deposit must emit DepositApproved")
            .split("});")
            .next()
            .expect("the DepositApproved emit must be terminated")
            .to_string();

        // Positive control on the extractor, not decoration: this test's whole claim is an
        // absence, and a split that silently captured the wrong span — or nothing — would report
        // one. Assert a field that *is* published before asserting the one that is not.
        assert!(
            emit_site.contains("observed_age_seconds"),
            "the extracted span is not DepositApproved's emit body, so its absence claim below \
             would be vacuous — the split markers moved"
        );

        assert!(
            !emit_site.contains("lock_until"),
            "DepositApproved gained a lock_until field — the backend reads the value off the \
             Position account instead, and publishing it here duplicates a write-once source"
        );
    }

    /// The fields every later instruction re-derives or ranks by. Unwritten,
    /// `position_id` degenerates the tier tie-break and `depositor` sends a returned NFT to
    /// the default pubkey. The zero-init-duplicating writes (`state = Pending`, `accrued`,
    /// `lock_until`, `reject_reason`) are deliberately not pinned.
    #[test]
    fn deposit_writes_every_position_identity_field() {
        for field in [
            "position.pool",
            "position.depositor",
            "position.nft_mint",
            "position.position_id",
            "position.bump",
            "position.vault_bump",
        ] {
            assert_eq!(writes(DEPOSIT_SRC, field), 1, "{field} is not assigned");
        }
        for field in [
            "wallet_stats.pool",
            "wallet_stats.owner",
            "wallet_stats.bump",
        ] {
            assert_eq!(writes(DEPOSIT_SRC, field), 1, "{field} is not assigned");
        }
        assert_eq!(writes(DEPOSIT_SRC, "ctx.accounts.pool.position_counter"), 1);
    }

    /// Without both writes the position stays `Pending` while `DepositRejected` announces it
    /// rejected — and `return_rejected` requires `Rejected`, so the NFT never comes back.
    #[test]
    fn reject_deposit_records_the_rejection_it_announces() {
        assert!(production(REJECT_SRC).contains("position.state = PositionState::Rejected"));
        assert_eq!(writes(REJECT_SRC, "position.reject_reason"), 1);
    }

    /// `validate_admission` and `validate_admission_limits` are each covered
    /// by their own tests, but coverage of a guard says nothing about whether it runs. Both
    /// are `pub` with other call sites, so deleting either call compiles clean and silently
    /// admits any NFT of any collection and standard.
    #[test]
    fn deposit_runs_the_on_chain_admission_gate() {
        assert_eq!(calls(DEPOSIT_SRC, "validate_admission"), 1);
    }

    /// `deposit` **stores what it resolved** rather than the literal it used to assume. The
    /// write-count pin alone cannot see the difference — `position.standard = STANDARD_PNFT` is
    /// also one write — and it stopped being harmless the moment a second standard was admitted.
    #[test]
    fn deposit_stores_the_resolved_standard_rather_than_a_literal() {
        assert!(production(DEPOSIT_SRC).contains("position.standard = standard;"));
    }

    /// The transfer branch's both halves. **The absent half is the one with no other
    /// instrument**: `transfer_spl` ignores the five pNFT accounts, so a legacy deposit that
    /// supplies a token record would succeed with the account carried unread in a transaction
    /// whose shape claims pNFT — no CPI fails, no state is wrong, and nothing else in this crate
    /// looks. Five `is_none()` guards, counted, because four would leave one account unchecked
    /// and every behavioural test would still pass.
    ///
    /// The codes are Anchor's own: `ConstraintRaw` for supplied-when-absent, and
    /// `AccountNotEnoughKeys` for the three accounts the pNFT leg needs and `Option` no longer
    /// guarantees. Counted rather than merely present — dropping one `ok_or` gives the pNFT branch
    /// a silent `None` deref site.
    #[test]
    fn the_transfer_branch_requires_the_five_pnft_accounts_present_and_absent_by_standard() {
        let prod = production(DEPOSIT_SRC);
        assert_eq!(calls(DEPOSIT_SRC, "transfer_pnft"), 1);
        assert_eq!(calls(DEPOSIT_SRC, "transfer_spl"), 1);
        for account in [
            "depositor_token_record",
            "position_token_record",
            "sysvar_instructions",
            "authorization_rules_program",
            "authorization_rules",
        ] {
            assert_eq!(
                flat(DEPOSIT_SRC)
                    .matches(&format!(
                        "require!( ctx.accounts.{account}.is_none(), ErrorCode::ConstraintRaw );"
                    ))
                    .count(),
                1,
                "{account} is not required-absent on the legacy branch"
            );
        }
        assert_eq!(prod.matches("ErrorCode::ConstraintRaw").count(), 5);
        assert_eq!(prod.matches("ErrorCode::AccountNotEnoughKeys").count(), 3);
        assert!(flat(DEPOSIT_SRC).contains("match standard { STANDARD_PNFT =>"));
        assert!(flat(DEPOSIT_SRC).contains("STANDARD_LEGACY => {"));
    }

    /// `withdraw` and `claim_nft` each pin their own 16 `transfer_pnft` arguments as an ordered
    /// literal, and `deposit` did not — the same-typed-positional hazard, unpinned at the one call
    /// site that moves a card *into* escrow. Both legs are pinned here now, `transfer_spl`'s four
    /// included: three `AccountInfo` and a program, so `from`/`to` transposed reverses custody and
    /// `authority`/`token_program` transposed hands the CPI the wrong signer. Injecting a
    /// source/destination swap and a four-argument permutation both passed a green suite.
    #[test]
    fn deposits_two_transfer_legs_pin_their_own_argument_order() {
        assert!(flat(DEPOSIT_SRC).contains(
            "transfer_pnft( &ctx.accounts.depositor_token.to_account_info(), \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.position_vault.to_account_info(), \
             &ctx.accounts.position.to_account_info(), \
             &ctx.accounts.nft_mint.to_account_info(), &ctx.accounts.metadata, \
             &ctx.accounts.master_edition, depositor_token_record, position_token_record, \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.system_program.to_account_info(), sysvar_instructions, \
             &ctx.accounts.token_program.to_account_info(), \
             &ctx.accounts.associated_token_program.to_account_info(), \
             &ctx.accounts.token_metadata_program, \
             ctx.accounts.authorization_rules_program.as_deref(), \
             ctx.accounts.authorization_rules.as_deref(), &[], )?;"
        ));
        assert!(flat(DEPOSIT_SRC).contains(
            "transfer_spl( &ctx.accounts.depositor_token.to_account_info(), \
             &ctx.accounts.position_vault.to_account_info(), \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.token_program.to_account_info(), &[], )?;"
        ));
    }

    /// The arm the `u8` match forces. It is unreachable while `resolve_standard` returns 0 or 1,
    /// and it must **reject** rather than fall through: a fall-through opens a `Position` and
    /// emits `DepositPending` over an escrow that never happened. Replacing it with `_ => {}`
    /// left the whole suite green.
    #[test]
    fn the_unreachable_standard_arm_rejects_rather_than_falling_through() {
        assert!(flat(DEPOSIT_SRC)
            .contains("_ => return Err(ByeMachineError::WrongStandardForInstruction.into()),"));
    }

    /// Without this call the `Position` is created and `DepositPending` emitted
    /// while the NFT never leaves the depositor, who can then sell it out from under a pool
    /// that believes it holds the escrow.
    #[test]
    fn deposit_escrows_the_nft_before_opening_a_position() {
        assert_eq!(calls(DEPOSIT_SRC, "transfer_pnft"), 1);
        // `return_rejected` reaches its transfer through the shared standard-dispatch branch,
        // which is what makes the leg depend on the *stored* standard. A direct `transfer_pnft`
        // here would move a legacy card down the pNFT path, so its absence is asserted alongside
        // the call.
        assert_eq!(calls(RETURN_SRC, "release_from_vault"), 1);
        assert_eq!(calls(RETURN_SRC, "transfer_pnft"), 0);
    }

    /// The Operator-only pair carries the signer constraint; the permissionless
    /// pair carries no authority binding at all. Both halves matter — a constraint added to
    /// `return_rejected` would break access control as surely as one dropped from `approve_deposit`.
    #[test]
    fn the_authority_matrix_is_operator_only_on_approve_and_reject() {
        for source in [APPROVE_SRC, REJECT_SRC] {
            assert_eq!(production(source).matches(OPERATOR_CONSTRAINT).count(), 1);
        }
        for source in [DEPOSIT_SRC, RETURN_SRC] {
            assert_eq!(production(source).matches("protocol_config").count(), 0);
            assert_eq!(production(source).matches("Unauthorized").count(), 0);
        }
    }

    /// Every state transition is gated on the state it leaves. Without these the
    /// tree takes a second `insert` for one position, or a live NFT walks out of escrow.
    #[test]
    fn every_transition_out_of_a_state_is_gated_on_that_state() {
        for source in [APPROVE_SRC, REJECT_SRC] {
            assert!(production(source).contains("position.state == PositionState::Pending"));
            assert!(production(source).contains("ByeMachineError::PositionNotPending"));
        }
        assert!(production(RETURN_SRC).contains(
            "constraint = position.state == PositionState::Rejected @ ByeMachineError::InvalidPositionState"
        ));
    }

    /// The `WeightIndex` is a bare keypair account, not a PDA: `AccountLoader` checks only owner
    /// and discriminator, so another pool's index would load cleanly. Only this address binding
    /// stops it, and `insert`'s desync guard cannot — it validates against the caller's own
    /// `expected_total`.
    #[test]
    fn the_weight_index_is_pinned_to_the_pools_own_account() {
        assert!(production(APPROVE_SRC).contains("#[account(mut, address = pool.weight_index)]"));
        assert!(
            production(APPROVE_SRC).contains("pub weight_index: AccountLoader<'info, WeightIndex>")
        );
    }

    /// `return_rejected` is permissionless, so nothing but this constraint stops a caller from
    /// naming itself as the destination of the NFT and both rent refunds.
    #[test]
    fn return_rejected_pins_the_destination_to_the_recorded_depositor() {
        assert!(production(RETURN_SRC).contains(
            "constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
    }

    /// The pause flag gates entry only. Approval, rejection and the return of an
    /// already-escrowed NFT must all stay reachable while deposits are paused.
    #[test]
    fn only_deposit_is_gated_on_the_deposits_paused_flag() {
        assert!(production(DEPOSIT_SRC).contains("!ctx.accounts.pool.deposits_paused"));
        assert!(production(DEPOSIT_SRC).contains("ByeMachineError::DepositsPaused"));
        for source in [APPROVE_SRC, REJECT_SRC, RETURN_SRC] {
            assert_eq!(production(source).matches("deposits_paused").count(), 0);
        }
    }

    /// The vault holds the escrowed NFT under the *position PDA's* authority.
    /// A vault minted to the depositor's authority would leave custody with the depositor
    /// while the program believed the NFT was escrowed.
    #[test]
    fn the_escrow_vault_is_authorised_by_the_position_pda() {
        assert!(production(DEPOSIT_SRC).contains("token::mint = nft_mint"));
        assert!(production(DEPOSIT_SRC).contains("token::authority = position"));
    }

    /// Both CPI-performing handlers pin the programs they invoke through. Unpinned, a caller
    /// substitutes its own program for Token Metadata: the `TransferV1` CPI succeeds without
    /// moving anything, and `deposit` still creates a `Position` over an NFT never escrowed.
    #[test]
    fn both_cpi_handlers_pin_token_metadata_and_the_instructions_sysvar() {
        for source in [DEPOSIT_SRC, RETURN_SRC] {
            assert!(production(source).contains("#[account(address = TOKEN_METADATA_ID)]"));
            assert!(production(source).contains("#[account(address = SYSVAR_INSTRUCTIONS_ID)]"));
            assert!(production(source).contains("pub token_program: Program<'info, Token>"));
            assert!(production(source)
                .contains("pub associated_token_program: Program<'info, AssociatedToken>"));
            assert!(production(source).contains("pub system_program: Program<'info, System>"));
        }
    }

    /// The NFT leaving the depositor must be the mint the position is opened over, held by
    /// the signer — not some third party's account that happens to be writable.
    #[test]
    fn deposit_pins_the_source_token_account_to_the_mint_and_the_depositor() {
        assert!(production(DEPOSIT_SRC).contains(
            "constraint = depositor_token.mint == nft_mint.key() @ ByeMachineError::StandardNotAdmitted"
        ));
        assert!(production(DEPOSIT_SRC).contains(
            "constraint = depositor_token.owner == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
    }

    /// `return_rejected` is permissionless and reads its mint from an account the caller
    /// supplies, so the mint is pinned to the one the position recorded at deposit.
    #[test]
    fn return_rejected_pins_the_mint_to_the_positions_recorded_mint() {
        assert!(production(RETURN_SRC).contains(
            "#[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]"
        ));
        assert!(production(RETURN_SRC)
            .contains("seeds = [POSITION_VAULT_SEED, position.key().as_ref()]"));
    }

    /// Every account in the domain that is a PDA declares its seeds. A dropped `seeds =`
    /// turns a derived account into one the caller chooses freely, and Anchor reports nothing.
    #[test]
    fn every_pda_in_the_deposit_domain_declares_its_seeds() {
        let expected = [
            (DEPOSIT_SRC, 4),
            (APPROVE_SRC, 5),
            (REJECT_SRC, 2),
            (RETURN_SRC, 2),
        ];
        for (source, count) in expected {
            assert_eq!(production(source).matches("seeds = [").count(), count);
        }
        for source in [APPROVE_SRC, REJECT_SRC] {
            assert!(production(source).contains("seeds = [PROTOCOL_CONFIG_SEED]"));
        }
        for source in [DEPOSIT_SRC, APPROVE_SRC] {
            assert!(production(source)
                .contains("seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()]"));
        }
    }

    /// The position and its vault derive from the mint and the position, so a
    /// re-deposit of the same mint lands on the same two addresses by construction.
    #[test]
    fn the_deposit_domain_pdas_derive_from_their_declared_seeds() {
        let position_seeds =
            "seeds = [POSITION_SEED, pool.key().as_ref(), nft_mint.key().as_ref()]";
        assert!(production(DEPOSIT_SRC).contains(position_seeds));
        assert!(production(DEPOSIT_SRC)
            .contains("seeds = [POSITION_VAULT_SEED, position.key().as_ref()]"));
        assert!(production(DEPOSIT_SRC).contains(
            "seeds = [WALLET_STATS_SEED, pool.key().as_ref(), depositor.key().as_ref()]"
        ));
        assert!(production(APPROVE_SRC).contains(
            "seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()]"
        ));
        assert!(production(APPROVE_SRC).contains("seeds = [TOP_TIER_SEED, pool.key().as_ref()]"));
        for source in [APPROVE_SRC, REJECT_SRC, RETURN_SRC] {
            assert_eq!(
                production(source)
                    .matches("seeds = [POSITION_SEED, ")
                    .count(),
                1
            );
        }
    }

    /// Both Operator-driven transitions out of `Pending` refuse a position whose depositor is
    /// the treasury_02 PDA.
    #[test]
    fn approve_and_reject_both_call_the_treasury_exclusion() {
        assert_eq!(calls(APPROVE_SRC, "assert_not_treasury_position"), 1);
        assert_eq!(calls(REJECT_SRC, "assert_not_treasury_position"), 1);
        for source in [DEPOSIT_SRC, RETURN_SRC] {
            assert_eq!(calls(source, "assert_not_treasury_position"), 0);
        }
    }

    /// One event per handler, each in its own handler and no other.
    #[test]
    fn each_deposit_handler_emits_exactly_its_own_event() {
        let map = [
            (DEPOSIT_SRC, "DepositPending"),
            (APPROVE_SRC, "DepositApproved"),
            (REJECT_SRC, "DepositRejected"),
            (RETURN_SRC, "NftReturned"),
        ];
        for (source, event) in map {
            assert_eq!(
                production(source)
                    .matches(&format!("emit!({event}"))
                    .count(),
                1
            );
            for (other, _) in map {
                if !std::ptr::eq(other, source) {
                    assert_eq!(
                        production(other).matches(&format!("emit!({event}")).count(),
                        0
                    );
                }
            }
        }
    }

    /// The position and vault PDAs both re-derive on a re-deposit of the same mint, so an
    /// emptied-but-surviving vault would make that re-deposit permanently unsatisfiable.
    #[test]
    fn return_rejected_closes_the_vault_and_refunds_both_rents_to_the_depositor() {
        assert_eq!(calls(RETURN_SRC, "close_account"), 1);
        assert!(production(RETURN_SRC)
            .contains("destination: ctx.accounts.depositor.to_account_info()"));
        assert!(production(RETURN_SRC).contains("close = depositor"));
        assert!(!production(RETURN_SRC).contains("close = payer"));
    }

    /// `deposit` creates `WalletStats` but never resets its counters on a repeat deposit.
    #[test]
    fn deposit_never_resets_the_wallet_stats_counters_on_a_repeat_deposit() {
        let source = production(DEPOSIT_SRC);
        assert!(!source.contains("active_positions ="));
        assert!(!source.contains("active_value ="));
    }

    /// The 3xxx codes the client checks by number; `u32::from` alone would not pin them.
    #[test]
    fn missing_and_read_only_remaining_accounts_return_anchor_3005_and_3006() {
        assert_eq!(u32::from(ErrorCode::AccountNotEnoughKeys), 3005);
        assert_eq!(u32::from(ErrorCode::AccountNotMutable), 3006);
    }
}
