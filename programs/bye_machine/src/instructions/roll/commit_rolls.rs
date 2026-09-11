use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Mint, Token, TokenAccount, Transfer};
use ephemeral_vrf_sdk::anchor::{vrf, VrfProgram};
use ephemeral_vrf_sdk::instructions::{
    create_request_scoped_randomness_ix, RequestRandomnessParams,
};
use ephemeral_vrf_sdk::types::SerializableAccountMeta;

use crate::common::constants::MAX_BATCH_CAPACITY;
use crate::common::errors::ByeMachineError;
use crate::common::events::BatchCommitted;
use crate::common::guards::assert_weight_open;
use crate::common::seeds::{
    BATCH_SEED, POOL_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED,
    VRF_IDENTITY_SEED,
};
use crate::state::{BatchState, Pool, ProtocolConfig, RollBatch, RollOutcome, RollRecord, RollStatus, TopTier};

/// Every roll of the batch starts here: nothing drawn, nothing settled, no refund branch taken.
/// Written out rather than derived from a `Default` impl so the zero value lands in the account
/// where a reader can see it.
const UNDRAWN: RollRecord = RollRecord {
    status: RollStatus::Pending,
    outcome: RollOutcome::None,
    selected_mint: Pubkey::new_from_array([0u8; 32]),
    blank_face_idx: 0,
    draw_value: 0,
    attempts: 0,
    w_at_draw: 0,
    n_real_at_draw: 0,
    blanks_at_draw: [0; 3],
    refund_reason: 0,
};

/// How many of the pool's real cards the tier must hold before a batch may open.
///
/// The same expression `settle_roll` enforces before it accrues. Checked here as well because
/// the two closing paths that can leave the tier short take their candidate *optionally* and
/// `update_tier` runs only while no batch is open: a pool can sit short with no batch in flight,
/// and without this a roller pays `n × price` for a batch whose first settlement fails the gate
/// and freezes every withdrawal, activation and sweep pool-wide until an Operator intervenes.
fn required_tier_len(tier_size: u8, n_real: u32) -> u8 {
    (tier_size as u32).min(n_real) as u8
}

/// Every account `#[vrf]` would otherwise inject is declared here instead, so the request's
/// five-account list is visible in this file and to the crate-wide source pins rather than
/// appearing only after macro expansion. The macro checks each field by name and injects only
/// what is missing, so it adds nothing here and contributes just `invoke_signed_vrf`.
#[vrf]
#[derive(Accounts)]
pub struct CommitRolls<'info> {
    #[account(mut)]
    pub roller: Signer<'info>,

    #[account(seeds = [PROTOCOL_CONFIG_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    /// Seeded on the pool's *pre-increment* counter, so the same roller committing twice in a
    /// row cannot land on one address: `init` fails closed if the counter ever repeats.
    #[account(
        init,
        payer = roller,
        space = 8 + RollBatch::INIT_SPACE,
        seeds = [BATCH_SEED, pool.key().as_ref(), pool.batch_counter.to_le_bytes().as_ref()],
        bump
    )]
    pub batch: Box<Account<'info, RollBatch>>,

    #[account(
        mut,
        constraint = roller_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = roller_usdc.owner == roller.key() @ ByeMachineError::Unauthorized
    )]
    pub roller_usdc: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = protocol_config.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    /// CHECK: the provider's queue, pinned to the one the protocol config names. Written by the
    /// request CPI — both the enqueued item and the 0.0005 SOL fee land here.
    #[account(mut, address = protocol_config.oracle_queue)]
    pub oracle_queue: UncheckedAccount<'info>,

    /// CHECK: the request-side identity PDA. Derived rather than address-pinned so it follows
    /// the program id across environments, and signs the request CPI.
    #[account(seeds = [VRF_IDENTITY_SEED], bump)]
    pub program_identity: UncheckedAccount<'info>,

    pub vrf_program: Program<'info, VrfProgram>,

    /// CHECK: the sysvar the provider reads its request entropy from.
    #[account(address = anchor_lang::solana_program::sysvar::slot_hashes::ID)]
    pub slot_hashes: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// Every entry condition, and the pool-side ledger move they authorise, over plain state.
///
/// Returns the batch's `(batch_id, liability)` — the id is read before the counter advances, so
/// it is the value the batch PDA was already derived from.
fn apply_commit(pool: &mut Pool, tier_len: u8, n: u8) -> Result<(u64, u64)> {
    require!(!pool.rolls_paused, ByeMachineError::RollsPaused);
    require!(!pool.sweep_pending, ByeMachineError::SweepPending);
    // Batches serialize: a second one may not open while the first is unfinished, so no batch's
    // draws can move `W` for another's unrun rolls.
    assert_weight_open(pool)?;
    require!(
        n >= 1 && n <= pool.max_n && (n as usize) <= MAX_BATCH_CAPACITY,
        ByeMachineError::RollCountOutOfBounds
    );
    require!(pool.n_real > 0, ByeMachineError::RealPoolEmpty);
    require_eq!(
        tier_len,
        required_tier_len(pool.tier_size, pool.n_real),
        ByeMachineError::TierNotFull
    );

    let liability = (n as u64)
        .checked_mul(pool.price)
        .ok_or(ByeMachineError::RollCountOutOfBounds)?;
    let batch_id = pool.batch_counter;

    pool.pending_roll_liability = pool
        .pending_roll_liability
        .checked_add(liability)
        .ok_or(ByeMachineError::PrincipalShortfall)?;
    pool.open_batches = pool
        .open_batches
        .checked_add(1)
        .ok_or(ByeMachineError::RollBatchInFlight)?;
    pool.batch_counter = batch_id
        .checked_add(1)
        .ok_or(ByeMachineError::RollCountOutOfBounds)?;

    Ok((batch_id, liability))
}

pub fn handler(ctx: Context<CommitRolls>, n: u8) -> Result<()> {
    let tier_len = ctx.accounts.top_tier.len;
    let (batch_id, liability) = apply_commit(&mut ctx.accounts.pool, tier_len, n)?;

    transfer(
        CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.roller_usdc.to_account_info(),
                to: ctx.accounts.principal_vault.to_account_info(),
                authority: ctx.accounts.roller.to_account_info(),
            },
        ),
        liability,
    )?;

    let batch_key = ctx.accounts.batch.key();
    let pool_key = ctx.accounts.pool.key();
    let slot = Clock::get()?.slot;
    let pool = &ctx.accounts.pool;
    let (price, ticket, fee) = (pool.price, pool.ticket_target, pool.fee);
    let alloc_bps = [
        pool.alloc_equal_bps,
        pool.alloc_tier_bps,
        pool.alloc_protocol_bps,
    ];
    let blank_faces = pool.blank_faces;

    let batch = &mut ctx.accounts.batch;
    batch.pool = pool_key;
    batch.roller = ctx.accounts.roller.key();
    batch.batch_id = batch_id;
    batch.state = BatchState::Committed;
    batch.n = n;
    batch.next_roll = 0;
    batch.settled = 0;
    batch.refunded = 0;
    batch.request_slot = slot;
    batch.caller_seed = batch_key.to_bytes();
    batch.randomness = [0u8; 32];
    // Seeded by the callback from live state, not here: a value written at commitment would be
    // stale by the time randomness arrives, and the assert it feeds compares against the pool as
    // the batch's first draw finds it.
    batch.expected_w = 0;
    batch.expected_blanks = [0; 3];
    batch.s_price = price;
    batch.s_ticket = ticket;
    batch.s_fee = fee;
    batch.s_alloc_bps = alloc_bps;
    batch.s_blank_faces = blank_faces;
    batch.rolls = [UNDRAWN; MAX_BATCH_CAPACITY];
    batch.bump = ctx.bumps.batch;

    // One request for the whole batch regardless of `n` — the 32 bytes it returns expand to all
    // `n` draws through the roll index. The batch key is both the request's `caller_seed` and
    // its `callback_args`: recovery scans the queue for a live item whose args carry this key,
    // and the batch PDA is globally unique, so no two pools can match each other's.
    let request = create_request_scoped_randomness_ix(RequestRandomnessParams {
        payer: ctx.accounts.roller.key(),
        oracle_queue: ctx.accounts.oracle_queue.key(),
        callback_program_id: crate::ID,
        callback_discriminator: crate::instruction::VrfCallback::DISCRIMINATOR.to_vec(),
        caller_seed: batch_key.to_bytes(),
        accounts_metas: Some(vec![
            SerializableAccountMeta {
                pubkey: batch_key,
                is_signer: false,
                is_writable: true,
            },
            SerializableAccountMeta {
                pubkey: pool_key,
                is_signer: false,
                is_writable: false,
            },
        ]),
        callback_args: Some(batch_key.to_bytes().to_vec()),
    });
    // The helper passes four account infos for a five-account instruction, omitting
    // `system_program`. That is correct rather than a subset bug: the runtime resolves
    // executable accounts from the instruction context and skips the `account_infos` lookup for
    // them entirely, so a program account never needs to be passed. Only the four
    // non-executable accounts do, and those are exactly the four it passes.
    ctx.accounts
        .invoke_signed_vrf(&ctx.accounts.roller.to_account_info(), &request)?;

    emit!(BatchCommitted {
        pool: pool_key,
        slot,
        roller: ctx.accounts.roller.key(),
        batch: batch_key,
        n,
        s_price: ctx.accounts.batch.s_price,
        s_ticket: ctx.accounts.batch.s_ticket,
        s_fee: ctx.accounts.batch.s_fee,
        s_alloc_bps: ctx.accounts.batch.s_alloc_bps,
        s_blank_faces: ctx.accounts.batch.s_blank_faces,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{calls, production};

    const SRC: &str = include_str!("commit_rolls.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    /// A pool that passes every guard, so each test below turns exactly one condition off and
    /// the failure it reads is attributable to that condition alone.
    fn committable_pool() -> Pool {
        let mut pool = Pool::blank();
        pool.price = 9_900_000;
        pool.ticket_target = 9_000_000;
        pool.fee = 900_000;
        pool.alloc_equal_bps = 7_500;
        pool.alloc_tier_bps = 500;
        pool.alloc_protocol_bps = 2_000;
        pool.blank_faces = [1_000_000, 2_000_000, 5_000_000];
        pool.max_n = 10;
        pool.tier_size = 20;
        pool.n_real = 3;
        pool
    }

    /// `tier_size = 20`, `n_real = 3`, so the tier is full at 3.
    const FULL_TIER: u8 = 3;

    // --- The success path first: a refusal-only matrix cannot tell a guard that rejects
    //     everything from one that rejects the right thing --------------------------------

    #[test]
    fn opens_a_batch_and_moves_the_pool_ledger_once() {
        let mut pool = committable_pool();
        let (batch_id, liability) = apply_commit(&mut pool, FULL_TIER, 4).unwrap();

        assert_eq!(batch_id, 0, "the id is read before the counter advances");
        assert_eq!(liability, 4 * 9_900_000);
        assert_eq!(pool.pending_roll_liability, 4 * 9_900_000);
        assert_eq!(pool.open_batches, 1);
        assert_eq!(pool.batch_counter, 1);
    }

    /// The id a batch PDA was derived from is the pre-increment counter. A batch seeded on the
    /// post-increment value would collide with the next batch's address.
    #[test]
    fn successive_batches_take_successive_ids() {
        let mut pool = committable_pool();
        let (first, _) = apply_commit(&mut pool, FULL_TIER, 1).unwrap();
        pool.open_batches = 0; // the first batch completed
        let (second, _) = apply_commit(&mut pool, FULL_TIER, 1).unwrap();
        assert_eq!((first, second), (0, 1));
        assert_eq!(pool.batch_counter, 2);
    }

    #[test]
    fn liability_accumulates_rather_than_replacing() {
        let mut pool = committable_pool();
        pool.pending_roll_liability = 1_000;
        apply_commit(&mut pool, FULL_TIER, 2).unwrap();
        assert_eq!(pool.pending_roll_liability, 1_000 + 2 * 9_900_000);
    }

    // --- Every guard, from both directions ----------------------------------------------

    #[test]
    fn rejects_while_rolls_are_paused_at_6201() {
        let mut pool = committable_pool();
        pool.rolls_paused = true;
        assert_eq!(
            error_code(apply_commit(&mut pool, FULL_TIER, 1).unwrap_err()),
            6201
        );
    }

    /// The deposit flag is a different switch and must not reach this instruction.
    #[test]
    fn the_deposit_pause_does_not_gate_a_commitment() {
        let mut pool = committable_pool();
        pool.deposits_paused = true;
        assert!(apply_commit(&mut pool, FULL_TIER, 1).is_ok());
    }

    #[test]
    fn rejects_while_a_sweep_is_pending_at_6202() {
        let mut pool = committable_pool();
        pool.sweep_pending = true;
        assert_eq!(
            error_code(apply_commit(&mut pool, FULL_TIER, 1).unwrap_err()),
            6202
        );
    }

    /// Batches serialize — only one may be open at a time, so the weight domain a batch resolves
    /// against is fixed for that batch alone rather than shared across an overlapping pipeline.
    #[test]
    fn rejects_while_a_batch_is_already_open_at_6200() {
        for open in [1u32, 2, u32::MAX] {
            let mut pool = committable_pool();
            pool.open_batches = open;
            assert_eq!(
                error_code(apply_commit(&mut pool, FULL_TIER, 1).unwrap_err()),
                6200,
                "open_batches = {open}"
            );
        }
    }

    #[test]
    fn rejects_n_outside_one_to_max_n_at_6203() {
        for n in [0u8, 11, 255] {
            let mut pool = committable_pool();
            assert_eq!(
                error_code(apply_commit(&mut pool, FULL_TIER, n).unwrap_err()),
                6203,
                "n = {n}"
            );
        }
    }

    #[test]
    fn accepts_both_ends_of_the_n_range() {
        for n in [1u8, 10] {
            let mut pool = committable_pool();
            assert!(apply_commit(&mut pool, FULL_TIER, n).is_ok(), "n = {n}");
        }
    }

    /// `max_n` is the pool's own cap and binds below the array capacity.
    #[test]
    fn rejects_above_the_pools_own_max_n_even_when_within_capacity() {
        let mut pool = committable_pool();
        pool.max_n = 3;
        assert_eq!(
            error_code(apply_commit(&mut pool, FULL_TIER, 4).unwrap_err()),
            6203
        );
        assert!(apply_commit(&mut pool, FULL_TIER, 3).is_ok());
    }

    /// A configured `max_n` above the `RollRecord` array's capacity must not let `n` past it —
    /// the array is fixed at ten and a write beyond it has nowhere to land.
    #[test]
    fn capacity_binds_even_if_max_n_is_configured_above_it() {
        let mut pool = committable_pool();
        pool.max_n = u8::MAX;
        assert_eq!(
            error_code(apply_commit(&mut pool, FULL_TIER, 11).unwrap_err()),
            6203
        );
        assert_eq!(MAX_BATCH_CAPACITY, 10);
    }

    #[test]
    fn rejects_an_empty_real_pool_at_6204() {
        let mut pool = committable_pool();
        pool.n_real = 0;
        assert_eq!(error_code(apply_commit(&mut pool, 0, 1).unwrap_err()), 6204);
    }

    #[test]
    fn rejects_a_short_tier_at_6207() {
        let mut pool = committable_pool();
        assert_eq!(
            error_code(apply_commit(&mut pool, FULL_TIER - 1, 1).unwrap_err()),
            6207
        );
    }

    /// The gate is an equality, not a floor: a tier holding more than the pool's real cards is
    /// as wrong as one holding fewer, and both block settlement.
    #[test]
    fn rejects_a_tier_longer_than_the_pool_requires_at_6207() {
        let mut pool = committable_pool();
        assert_eq!(
            error_code(apply_commit(&mut pool, FULL_TIER + 1, 1).unwrap_err()),
            6207
        );
    }

    #[test]
    fn the_tier_requirement_is_the_lesser_of_tier_size_and_n_real() {
        assert_eq!(required_tier_len(20, 3), 3, "a small pool fills its tier");
        assert_eq!(required_tier_len(3, 20), 3, "a large pool fills tier_size");
        assert_eq!(required_tier_len(5, 5), 5);
        assert_eq!(required_tier_len(20, 0), 0);
        assert_eq!(
            required_tier_len(20, u32::MAX),
            20,
            "n_real above u8::MAX must not wrap when narrowed"
        );
    }

    /// Refused before any money moves, not after: every guard runs inside `apply_commit`, and
    /// the handler performs the transfer only on its `Ok`.
    #[test]
    fn a_refused_commitment_leaves_the_pool_ledger_untouched() {
        let mut pool = committable_pool();
        pool.rolls_paused = true;
        let before = (
            pool.pending_roll_liability,
            pool.open_batches,
            pool.batch_counter,
        );
        assert!(apply_commit(&mut pool, FULL_TIER, 1).is_err());
        assert_eq!(
            (
                pool.pending_roll_liability,
                pool.open_batches,
                pool.batch_counter
            ),
            before
        );
    }

    // --- Structural: facts a validator-free behavioural test cannot reach ----------------

    /// **One VRF request per batch regardless of `n`.** The request is built once and invoked
    /// once, and neither sits inside a loop — the property is that `n` never reaches the
    /// request path, which a count of 1 over a straight-line body establishes.
    #[test]
    fn issues_exactly_one_vrf_request_and_not_inside_a_loop() {
        let prod = production(SRC);
        let handler = &prod[prod.find("pub fn handler").expect("handler is declared")..];
        assert_eq!(calls(handler, "create_request_scoped_randomness_ix"), 1);
        assert_eq!(calls(handler, "invoke_signed_vrf"), 1);
        // Line-anchored, not a bare `contains`: "for the whole batch" and "for the queue scan"
        // are prose in this handler's own comments, and a substring needle counts them as loops.
        let loops = handler
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                t.starts_with("for ") || t.starts_with("while ") || t.starts_with("loop {")
            })
            .count();
        assert_eq!(loops, 0, "the request path must not sit inside a loop");
    }

    /// The batch key is the queue-scan key recovery matches on, and it must be both the
    /// request's `caller_seed` and its `callback_args` — recovery reads the latter.
    #[test]
    fn the_request_carries_the_batch_key_as_both_seed_and_args() {
        let prod = production(SRC);
        assert!(prod.contains("caller_seed: batch_key.to_bytes()"));
        assert!(prod.contains("callback_args: Some(batch_key.to_bytes().to_vec())"));
    }

    /// Two stored callback metas, in the order `vrf_callback` declares its accounts: the batch
    /// writable, the pool read-only. The list is fixed here and cannot be changed afterwards,
    /// so a batch committed with the wrong metas could never resolve.
    #[test]
    fn the_callback_meta_list_is_the_batch_then_the_pool() {
        let prod = production(SRC);
        let batch_at = prod.find("pubkey: batch_key,").expect("batch meta");
        let pool_at = prod.find("pubkey: pool_key,").expect("pool meta");
        assert!(batch_at < pool_at, "the batch meta must come first");
        assert_eq!(prod.matches("SerializableAccountMeta {").count(), 2);
        assert_eq!(
            prod[batch_at..pool_at].matches("is_writable: true").count(),
            1,
            "the batch is the one writable callback account"
        );
    }

    /// The snapshot is the priced terms only. The buyback rate is deliberately absent, and so
    /// is any `s_W` — the draw reduces over live `W`.
    #[test]
    fn snapshots_the_five_priced_terms_and_nothing_else() {
        let prod = production(SRC);
        for field in [
            "batch.s_price = ",
            "batch.s_ticket = ",
            "batch.s_fee = ",
            "batch.s_alloc_bps = ",
            "batch.s_blank_faces = ",
        ] {
            assert_eq!(prod.matches(field).count(), 1, "{field}");
        }
        assert_eq!(prod.matches("buyback_rate_bps").count(), 0);
        // `batch.s_w`, not a bare `s_w` — the latter is a substring of `is_writable`, which the
        // callback meta list uses twice, so the loose needle passes for the wrong reason.
        assert_eq!(prod.matches("batch.s_w").count(), 0, "no s_W is snapshotted");
    }

    /// The expectation pair is seeded by the callback, not here: a commitment-time value would
    /// already be stale when randomness arrives.
    #[test]
    fn writes_the_expectation_pair_as_zero_and_leaves_it_to_the_callback() {
        let prod = production(SRC);
        assert!(prod.contains("batch.expected_w = 0;"));
        assert!(prod.contains("batch.expected_blanks = [0; 3];"));
        assert_eq!(prod.matches("aggregate_weight").count(), 0);
    }

    /// Every roll starts undrawn, and the array is written in full — a partial write would
    /// leave the tail holding whatever the account previously held.
    #[test]
    fn zeroes_the_whole_roll_array() {
        // `matches!` rather than `assert_eq!`: the wire enums deliberately derive no `Debug`.
        assert!(UNDRAWN.status == RollStatus::Pending);
        assert!(UNDRAWN.outcome == RollOutcome::None);
        assert_eq!(UNDRAWN.refund_reason, 0);
        assert_eq!(UNDRAWN.selected_mint, Pubkey::default());
        assert!(production(SRC).contains("batch.rolls = [UNDRAWN; MAX_BATCH_CAPACITY];"));
    }

    /// The batch opens `Committed` — `vrf_callback`'s write-once guard is stated against this
    /// value, so a batch opened in any other state could never resolve.
    #[test]
    fn opens_the_batch_in_the_committed_state() {
        assert!(production(SRC).contains("batch.state = BatchState::Committed;"));
    }

    #[test]
    fn carries_the_serialization_guard_exactly_once() {
        assert_eq!(calls(SRC, "assert_weight_open"), 1);
    }
}
