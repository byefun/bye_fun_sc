use anchor_lang::prelude::*;

use crate::common::constants::BLANK_SLOTS;

/// Where a batch sits in its lifecycle.
///
/// **Declaration order is the borsh wire byte**, not an implementation detail: a fieldless enum
/// serializes as its declaration index, so reordering these variants silently reinterprets every
/// already-written `RollBatch` and every client decode. Append only.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum BatchState {
    Committed,
    Resolved,
    Recovered,
    Complete,
}

/// Whether one roll of a batch has reached a terminal, and which.
///
/// Declaration order is the wire byte — see [`BatchState`].
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum RollStatus {
    Pending,
    Settled,
    Refunded,
}

/// What a settled roll drew. `None` is the value a `Pending` or `Refunded` roll carries — a
/// refunded roll has no outcome, which is distinct from having drawn a blank.
///
/// Declaration order is the wire byte — see [`BatchState`].
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum RollOutcome {
    None,
    RealCard,
    Blank,
}

/// One roll's commitment and result, inlined ten times into [`RollBatch`].
///
/// The draw inputs are persisted rather than recomputed so a third party can re-derive each result
/// from the batch account and position history alone, reproducing the randomness expansion
/// verbatim. `w_at_draw`, `n_real_at_draw` and `blanks_at_draw` are
/// the pool snapshot *at that draw*, which is not the snapshot at commitment: the pipeline removes
/// the drawn position and rebalances between rolls, so every roll after the first sees a
/// different pool.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace)]
pub struct RollRecord {
    pub status: RollStatus,
    pub outcome: RollOutcome,
    /// Real-card result. `Pubkey::default()` when the roll drew a blank or has not settled.
    pub selected_mint: Pubkey,
    /// Blank result: index into the batch's snapshotted `s_blank_faces`.
    pub blank_face_idx: u8,
    /// `r`, persisted so the result can be re-derived.
    pub draw_value: u128,
    /// Rejection-sampling attempts used, persisted so the fallback branch is visible.
    pub attempts: u8,
    pub w_at_draw: u128,
    pub n_real_at_draw: u32,
    pub blanks_at_draw: [u32; BLANK_SLOTS],
    /// `0` = settled. Non-zero identifies which refund branch was taken.
    pub refund_reason: u16,
}

/// One roll batch: the account the whole roll pipeline writes to.
/// Seeds: `[BATCH_SEED, pool, batch_id: u64 le]`.
///
/// **Field order is the on-chain byte layout.** Everything downstream of `commit_rolls` decodes
/// this account positionally. Every field before `expected_blanks` sits at the same offset in the
/// 1,100-byte three-slot layout and in this one, and `bump` is the last byte of both, which is
/// what `close_batch` reads either size by.
#[account]
#[derive(InitSpace)]
pub struct RollBatch {
    pub pool: Pubkey,
    pub roller: Pubkey,
    pub batch_id: u64,

    pub state: BatchState,

    pub n: u8,
    pub next_roll: u8,
    pub settled: u8,
    pub refunded: u8,

    /// Timeout anchor for the 240-slot recovery window.
    pub request_slot: u64,
    /// The batch PDA's own bytes, passed as the VRF request's `callback_args` — the globally
    /// unique key `recover_batch`'s queue scan matches on.
    pub caller_seed: [u8; 32],
    /// Written exactly once, by the callback. One 32-byte value expands to all `n` draws through
    /// the roll index `k`, so there is one VRF request per batch regardless of `n`.
    pub randomness: [u8; 32],

    /// Running expectation of `W` and the blank counts, seeded from live state at this batch's own
    /// `roll_index == 0` by `settle_roll`/`settle_roll_core` — not by `vrf_callback`, which could
    /// seed it while an earlier queued batch was still moving `W`. Rolls `1..n-1` assert
    /// `current == expected` *before* drawing; every roll rewrites both from its own draw and
    /// rebalance afterward.
    ///
    /// **Not a commitment snapshot.** The pipeline legitimately moves `W` between rolls, so a
    /// snapshot would fail the batch at roll 1; a batch-local running expectation passes on the
    /// batch's own mutations and fails on everything else.
    pub expected_w: u128,
    pub expected_blanks: [u32; BLANK_SLOTS],

    /// Economic terms snapshotted at commitment; the buyback rate is deliberately absent.
    /// **No `s_W`**: the draw reduces over live `W` rather than a range pinned here, and live `W`
    /// is the modulus third parties reproduce.
    pub s_price: u64,
    pub s_ticket: u64,
    pub s_fee: u64,
    pub s_alloc_bps: [u16; 3],
    pub s_blank_faces: [u64; BLANK_SLOTS],

    pub rolls: [RollRecord; 10],
    pub bump: u8,
}
