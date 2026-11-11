use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};

use crate::common::constants::VRF_TIMEOUT_SLOTS;
use crate::common::errors::ByeMachineError;
use crate::common::events::BatchRecovered;
use crate::common::seeds::{BATCH_SEED, POOL_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED};
use crate::common::vrf_queue;
use crate::state::{BatchState, Pool, ProtocolConfig, RollBatch, RollOutcome, RollStatus};

/// Persisted on every roll this recovery refunds. Anchor's `#[error_code]` macro adds 6000 to a
/// variant's own discriminant to produce its wire number (see `common/errors.rs`'s
/// `expected_code`), so this is `RecoveryTimeoutNotElapsed`'s own code, reused as the refund
/// branch identifier rather than a second number invented for the same condition.
const VRF_TIMEOUT_REFUND_REASON: u16 = ByeMachineError::RecoveryTimeoutNotElapsed as u16 + 6000;

#[derive(Accounts)]
pub struct RecoverBatch<'info> {
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
        seeds = [BATCH_SEED, batch.pool.as_ref(), batch.batch_id.to_le_bytes().as_ref()],
        bump = batch.bump,
        constraint = pool.key() == batch.pool @ ByeMachineError::BatchPoolMismatch
    )]
    pub batch: Box<Account<'info, RollBatch>>,

    /// Deliberately not constrained to `batch.roller`'s canonical associated token account —
    /// any existing token account of the right mint that they own is accepted. This lets the
    /// caller route the refund to a different account of theirs if the usual one is closed or
    /// frozen; the caller is responsible for resolving and supplying a destination that is
    /// actually able to receive the transfer.
    #[account(
        mut,
        constraint = roller_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = roller_usdc.owner == batch.roller @ ByeMachineError::Unauthorized
    )]
    pub roller_usdc: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    /// CHECK: the provider's queue, pinned to the one the protocol config names — any substitute
    /// is rejected by the `address` constraint itself, before the scan below ever runs. Without
    /// this pin an attacker-supplied empty account would scan clean and this instruction would
    /// refund a batch whose request is still live in the real queue.
    #[account(address = protocol_config.oracle_queue)]
    pub oracle_queue: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
}

/// Every guard and every effect except the queue scan and the token transfer, over plain state —
/// those two need the queue account's raw bytes and a CPI respectively, neither of which a unit
/// test can drive without a validator.
///
/// **The head-of-queue check runs first.** Recovery is Operator-signed, same as the
/// tier vacancy and missing-callback conditions that could wedge a head — an Operator present
/// enough to recover a batch behind the head is present enough to clear the head instead, so
/// out-of-order recovery buys no liveness a computed cursor doesn't already give for free. It
/// would cost something the cursor cannot afford: a terminal below the head that is not the
/// *next* batch to decrement `open_batches` desyncs `head_batch_id` from `batch_counter -
/// open_batches`, since that identity assumes terminals happen in commit order.
fn apply_recover(pool: &mut Pool, batch: &mut RollBatch, now_slot: u64) -> Result<u64> {
    require_eq!(
        batch.batch_id,
        pool.head_batch_id()?,
        ByeMachineError::BatchNotHeadOfQueue
    );
    require!(
        batch.state == BatchState::Committed,
        ByeMachineError::BatchNotResolved
    );
    let timeout_slot = batch
        .request_slot
        .checked_add(VRF_TIMEOUT_SLOTS)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    require!(
        now_slot >= timeout_slot,
        ByeMachineError::RecoveryTimeoutNotElapsed
    );

    let n = batch.n as usize;
    let refund = (batch.n as u64)
        .checked_mul(batch.s_price)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    for roll in batch.rolls[..n].iter_mut() {
        roll.status = RollStatus::Refunded;
        roll.outcome = RollOutcome::None;
        roll.refund_reason = VRF_TIMEOUT_REFUND_REASON;
    }
    batch.state = BatchState::Recovered;

    pool.pending_roll_liability = pool
        .pending_roll_liability
        .checked_sub(refund)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    pool.open_batches = pool
        .open_batches
        .checked_sub(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    Ok(refund)
}

/// Whether the batch's VRF request is still live in the pinned queue. Any parse failure is
/// treated as "still present" — a queue this program cannot read is not evidence that the
/// request is gone, and the failure mode this guard exists to prevent is exactly a false
/// "absent" verdict.
fn assert_queue_clear(oracle_queue_data: &[u8], batch_key: &Pubkey) -> Result<()> {
    let is_live = vrf_queue::batch_request_is_live(
        oracle_queue_data,
        &crate::ID.to_bytes(),
        &batch_key.to_bytes(),
    )
    .map(|(is_live, _stats)| is_live)
    .unwrap_or(true);
    require!(!is_live, ByeMachineError::QueueItemStillPresent);
    Ok(())
}

pub fn handler(ctx: Context<RecoverBatch>) -> Result<()> {
    let now_slot = Clock::get()?.slot;
    let batch_key = ctx.accounts.batch.key();

    // Check queue position before parsing the external queue account. A batch behind the head
    // must always report its own actionable condition, even when its VRF item is still live.
    // `apply_recover` repeats this pure-state guard so its unit-tested transition also fails
    // closed if a future caller bypasses this handler.
    require_eq!(
        ctx.accounts.batch.batch_id,
        ctx.accounts.pool.head_batch_id()?,
        ByeMachineError::BatchNotHeadOfQueue
    );
    assert_queue_clear(&ctx.accounts.oracle_queue.try_borrow_data()?, &batch_key)?;

    let refund = apply_recover(&mut ctx.accounts.pool, &mut ctx.accounts.batch, now_slot)?;

    let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();
    let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];
    let pool_signer: &[&[&[u8]]] = &[pool_seeds];

    transfer(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.principal_vault.to_account_info(),
                to: ctx.accounts.roller_usdc.to_account_info(),
                authority: ctx.accounts.pool.to_account_info(),
            },
            pool_signer,
        ),
        refund,
    )?;

    emit!(BatchRecovered {
        pool: ctx.accounts.pool.key(),
        slot: now_slot,
        batch: batch_key,
        wallet: ctx.accounts.batch.roller,
        open_batches_remaining: ctx.accounts.pool.open_batches,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{calls, production};

    const SRC: &str = include_str!("recover_batch.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn undrawn() -> crate::state::RollRecord {
        crate::state::RollRecord {
            status: RollStatus::Pending,
            outcome: RollOutcome::None,
            selected_mint: Pubkey::default(),
            blank_face_idx: 0,
            draw_value: 0,
            attempts: 0,
            w_at_draw: 0,
            n_real_at_draw: 0,
            blanks_at_draw: [0; 8],
            refund_reason: 0,
        }
    }

    fn committed_batch() -> RollBatch {
        RollBatch {
            pool: Pubkey::new_unique(),
            roller: Pubkey::new_unique(),
            batch_id: 3,
            state: BatchState::Committed,
            n: 4,
            next_roll: 0,
            settled: 0,
            refunded: 0,
            request_slot: 1_000,
            caller_seed: [7u8; 32],
            randomness: [0u8; 32],
            expected_w: 0,
            expected_blanks: [0; 8],
            s_price: 9_900_000,
            s_ticket: 9_000_000,
            s_fee: 900_000,
            s_alloc_bps: [7_500, 500, 2_000],
            s_blank_faces: [1_000_000, 2_000_000, 5_000_000, 0, 0, 0, 0, 0],
            rolls: [undrawn(); 10],
            bump: 254,
        }
    }

    /// `batch_counter = 4, open_batches = 1` puts the queue head at batch id 3 — matching
    /// `committed_batch()`'s `batch_id: 3`, so `apply_recover`'s head-of-queue check passes for
    /// every test here unless it deliberately moves the counters to test the gate itself.
    fn pool_with_liability(liability: u64) -> Pool {
        let mut pool = crate::state::Pool::blank();
        pool.pending_roll_liability = liability;
        pool.open_batches = 1;
        pool.batch_counter = 4;
        pool
    }

    // --- the success path ------------------------------------------------------------------

    #[test]
    fn recovers_a_timed_out_batch_and_refunds_every_roll() {
        let mut batch = committed_batch();
        let mut pool = pool_with_liability(4 * 9_900_000);

        let refund = apply_recover(&mut pool, &mut batch, 1_000 + 240).unwrap();

        assert_eq!(refund, 4 * 9_900_000);
        assert!(batch.state == BatchState::Recovered);
        assert_eq!(pool.pending_roll_liability, 0);
        assert_eq!(pool.open_batches, 0);
        for roll in &batch.rolls[..4] {
            assert!(roll.status == RollStatus::Refunded);
            assert!(roll.outcome == RollOutcome::None);
            assert_eq!(roll.refund_reason, 6208);
        }
        // Rolls past `n` are untouched — they were never part of this batch's commitment.
        for roll in &batch.rolls[4..] {
            assert!(roll.status == RollStatus::Pending);
            assert_eq!(roll.refund_reason, 0);
        }
    }

    #[test]
    fn liability_and_open_batches_move_by_exactly_this_batchs_share() {
        let mut batch = committed_batch();
        let mut pool = pool_with_liability(4 * 9_900_000 + 1_000);
        pool.open_batches = 1;

        apply_recover(&mut pool, &mut batch, 1_240).unwrap();

        assert_eq!(pool.pending_roll_liability, 1_000);
    }

    // --- the timeout boundary ---------------------------------------------------------------

    #[test]
    fn rejects_one_slot_before_the_timeout_at_6208() {
        let mut batch = committed_batch();
        let mut pool = pool_with_liability(4 * 9_900_000);
        assert_eq!(
            error_code(apply_recover(&mut pool, &mut batch, 1_000 + 239).unwrap_err()),
            6208
        );
    }

    #[test]
    fn accepts_exactly_at_the_timeout_slot() {
        let mut batch = committed_batch();
        let mut pool = pool_with_liability(4 * 9_900_000);
        assert!(apply_recover(&mut pool, &mut batch, 1_000 + 240).is_ok());
    }

    #[test]
    fn a_refused_recovery_leaves_state_and_ledger_untouched() {
        let mut batch = committed_batch();
        let mut pool = pool_with_liability(4 * 9_900_000);
        let before_liability = pool.pending_roll_liability;
        let before_open = pool.open_batches;

        assert!(apply_recover(&mut pool, &mut batch, 1_000 + 239).is_err());

        assert!(batch.state == BatchState::Committed);
        assert!(batch.rolls[0].status == RollStatus::Pending);
        assert_eq!(pool.pending_roll_liability, before_liability);
        assert_eq!(pool.open_batches, before_open);
    }

    // --- head-of-queue gating -------------------------------------------------

    /// A batch queued behind the head is refused before the `Committed`/timeout checks — in
    /// particular ahead of `BatchNotResolved`, so a queued-but-not-yet-head batch is never
    /// reported as being in the wrong state.
    #[test]
    fn rejects_a_batch_that_is_not_yet_the_queue_head_at_6215() {
        let mut batch = committed_batch();
        batch.batch_id = 3;
        let mut pool = pool_with_liability(4 * 9_900_000);
        pool.batch_counter = 5;
        pool.open_batches = 3; // head is batch 2 (5 - 3); this batch (id 3) is queued behind it

        assert_eq!(
            error_code(apply_recover(&mut pool, &mut batch, 1_000 + 240).unwrap_err()),
            6215
        );
    }

    /// Once the former head terminates, the next committed batch becomes head and may recover —
    /// the gate advances off the same `open_batches` decrement `apply_recover` itself performs.
    #[test]
    fn admits_recovery_once_the_batch_becomes_the_queue_head() {
        let mut batch = committed_batch();
        batch.batch_id = 3;
        let mut pool = pool_with_liability(4 * 9_900_000);
        pool.batch_counter = 4;
        pool.open_batches = 1; // batch 3 is exactly the head

        assert!(apply_recover(&mut pool, &mut batch, 1_000 + 240).is_ok());
    }

    /// Textually first in `apply_recover` — ahead of the `Committed` state check and the timeout.
    #[test]
    fn the_head_of_queue_check_precedes_the_committed_and_timeout_checks_in_source() {
        let prod = production(SRC);
        let body = &prod[prod.find("fn apply_recover").expect("apply_recover exists")..];
        let head_check = body
            .find("BatchNotHeadOfQueue")
            .expect("apply_recover checks BatchNotHeadOfQueue");
        let state_check = body
            .find("BatchState::Committed")
            .expect("apply_recover checks the Committed state");
        let timeout_check = body
            .find("RecoveryTimeoutNotElapsed")
            .expect("apply_recover checks the timeout");
        assert!(head_check < state_check);
        assert!(head_check < timeout_check);
    }

    // --- the state guard ---------------------------------------------------------------------

    #[test]
    fn every_non_committed_state_is_refused_at_6206() {
        for state in [
            BatchState::Resolved,
            BatchState::Recovered,
            BatchState::Complete,
        ] {
            let mut batch = committed_batch();
            batch.state = state;
            let mut pool = pool_with_liability(4 * 9_900_000);
            assert_eq!(
                error_code(apply_recover(&mut pool, &mut batch, 1_000 + 240).unwrap_err()),
                6206
            );
        }
    }

    // --- the queue scan, driven against the captured fixtures in both directions -----------------

    const BYEFUN_PRESENT: &[u8] =
        include_bytes!("../../../../../tests/fixtures/vrf/queue-byefun-present.bin");
    const BYEFUN_ABSENT: &[u8] =
        include_bytes!("../../../../../tests/fixtures/vrf/queue-byefun-absent.bin");

    /// The batch key `queue-byefun-present.bin` carries in its live item's `callback_args`, and
    /// `queue-byefun-absent.bin` does not — see `tests/fixtures/vrf/PROVENANCE.md`. An opaque,
    /// off-curve 32 bytes; the scan matches these wherever they come from.
    const FIXTURE_BATCH_KEY: [u8; 32] = [
        0x9c, 0x2f, 0x68, 0x7e, 0x8a, 0xaf, 0x31, 0xb0, 0xb3, 0x7d, 0x1c, 0x04, 0xcc, 0xcb, 0x1b,
        0xf2, 0x53, 0xa6, 0x93, 0x3c, 0xec, 0xfe, 0x12, 0x43, 0xf8, 0x94, 0x44, 0x91, 0xbc, 0x86,
        0x43, 0x5e,
    ];

    #[test]
    #[cfg_attr(
        any(feature = "testing", feature = "prod", not(feature = "dev")),
        ignore = "pinned to the devnet program id the queue fixtures were captured under"
    )]
    fn refuses_recovery_while_the_batch_key_is_still_live_in_the_queue() {
        let key = Pubkey::new_from_array(FIXTURE_BATCH_KEY);
        assert_eq!(
            error_code(assert_queue_clear(BYEFUN_PRESENT, &key).unwrap_err()),
            6209
        );
    }

    #[test]
    fn allows_recovery_once_the_same_key_is_absent_from_the_queue() {
        let key = Pubkey::new_from_array(FIXTURE_BATCH_KEY);
        assert!(assert_queue_clear(BYEFUN_ABSENT, &key).is_ok());
    }

    /// A key the present fixture does not carry passes even against the fixture proving the scan
    /// can find a hit at all — the positive control from the other direction.
    #[test]
    fn a_different_key_is_clear_even_against_the_present_fixture() {
        let key = Pubkey::new_unique();
        assert!(assert_queue_clear(BYEFUN_PRESENT, &key).is_ok());
    }

    /// A read the parser cannot make sense of is refused, not treated as an empty queue — the
    /// fail-closed branch `unwrap_or(true)` exists to take.
    #[test]
    fn an_unparseable_queue_account_is_treated_as_still_present() {
        let garbage = [0u8; 4]; // shorter than the 8-byte discriminator
        let key = Pubkey::new_unique();
        assert_eq!(
            error_code(assert_queue_clear(&garbage, &key).unwrap_err()),
            6209
        );
    }

    // --- structural --------------------------------------------------------------------------

    #[test]
    fn the_oracle_queue_is_pinned_to_protocol_config_with_no_scan_running_over_a_substitute() {
        assert!(production(SRC).contains("address = protocol_config.oracle_queue"));
    }

    #[test]
    fn the_pool_is_cross_checked_against_the_batch() {
        assert!(production(SRC).contains(
            "constraint = pool.key() == batch.pool @ ByeMachineError::BatchPoolMismatch"
        ));
    }

    #[test]
    fn the_signer_is_checked_against_the_protocol_operator() {
        assert!(production(SRC).contains(
            "constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized"
        ));
    }

    #[test]
    fn the_refund_destination_is_checked_against_the_batchs_own_roller() {
        assert!(production(SRC).contains("roller_usdc.owner == batch.roller"));
    }

    #[test]
    fn issues_exactly_one_transfer_and_it_is_signed_by_the_pool() {
        assert_eq!(calls(SRC, "transfer"), 1);
        assert!(production(SRC).contains("CpiContext::new_with_signer("));
        assert!(production(SRC).contains("authority: ctx.accounts.pool.to_account_info(),"));
    }

    #[test]
    fn queue_clear_is_checked_before_any_state_or_ledger_write() {
        let prod = production(SRC);
        let handler = &prod[prod.find("pub fn handler").expect("handler is declared")..];
        let scan_at = handler
            .find("assert_queue_clear(")
            .expect("the handler calls the queue guard");
        let recover_at = handler
            .find("apply_recover(")
            .expect("the handler calls apply_recover");
        assert!(
            scan_at < recover_at,
            "the queue must be proven clear before any state is mutated"
        );
    }

    #[test]
    fn handler_checks_queue_head_before_scanning_the_external_queue() {
        let prod = production(SRC);
        let handler = &prod[prod.find("pub fn handler").expect("handler is declared")..];
        let head_at = handler
            .find("BatchNotHeadOfQueue")
            .expect("the handler checks the queue head");
        let scan_at = handler
            .find("assert_queue_clear(")
            .expect("the handler calls the queue guard");
        assert!(
            head_at < scan_at,
            "a non-head batch must be refused before the external queue is scanned"
        );
    }
}
