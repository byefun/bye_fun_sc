use anchor_lang::prelude::*;

/// Where a batch sits in its lifecycle (SM-03).
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

/// Whether one roll of a batch has reached a terminal, and which (AC-57, INV-12).
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
/// The draw inputs are persisted rather than recomputed because INV-10 requires a third party to
/// re-derive the result from the batch account and position history alone, and AC-55 requires the
/// expansion to be reproducible verbatim. `w_at_draw`, `n_real_at_draw` and `blanks_at_draw` are
/// the pool snapshot *at that draw*, which is not the snapshot at commitment: the pipeline removes
/// the drawn position and rebalances between rolls (D-010a), so every roll after the first sees a
/// different pool.
///
/// This struct has no `Default` impl on purpose — `RollBatch` zeroes its array explicitly at
/// creation so the zero value is written where it can be read, not inferred from a trait.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace)]
pub struct RollRecord {
    pub status: RollStatus,
    pub outcome: RollOutcome,
    /// Real-card result. `Pubkey::default()` when the roll drew a blank or has not settled.
    pub selected_mint: Pubkey,
    /// Blank result: index into the batch's snapshotted `s_blank_faces`.
    pub blank_face_idx: u8,
    /// `r`, persisted for INV-10 re-derivation.
    pub draw_value: u128,
    /// Rejection-sampling attempts used, persisted per SD-5 so the fallback branch is visible.
    pub attempts: u8,
    pub w_at_draw: u128,
    pub n_real_at_draw: u32,
    pub blanks_at_draw: [u32; 3],
    /// `0` = settled. Non-zero identifies which refund branch of the §7 matrix was taken.
    pub refund_reason: u16,
}

/// One roll batch: the account the whole roll pipeline writes to.
/// Seeds: `[BATCH_SEED, pool, batch_id: u64 le]`.
///
/// **Field order is the on-chain byte layout and is append-only from the moment this ships.**
/// Everything downstream of `commit_rolls` decodes this account positionally.
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

    /// Timeout anchor for the 240-slot recovery window (D-003, INV-48).
    pub request_slot: u64,
    /// The batch PDA's own bytes, passed as the VRF request's `callback_args` — the globally
    /// unique key `recover_batch`'s queue scan matches on.
    pub caller_seed: [u8; 32],
    /// Written exactly once, by the callback (INV-09). One 32-byte value expands to all `n`
    /// draws through the roll index `k` (INV-49), so there is one VRF request per batch
    /// regardless of `n` (D-010c, AC-55).
    pub randomness: [u8; 32],

    /// INV-57's running expectation, seeded from live state by `vrf_callback` alongside
    /// `randomness` (D-155). Each `settle_roll` asserts `current == expected` *before* drawing,
    /// then rewrites both from its own draw and rebalance.
    ///
    /// **Not a commitment snapshot.** Equality with the value at callback time would be the wrong
    /// assertion: the pipeline legitimately moves `W` between rolls (D-010a), so a snapshot form
    /// would fail the batch at roll 1. A batch-local running expectation passes on the batch's own
    /// mutations and fails on everything else, which is exactly INV-57's claim. Widths track
    /// `RollRecord`'s `w_at_draw` and `blanks_at_draw` so the assert compares like with like.
    pub expected_w: u128,
    pub expected_blanks: [u32; 3],

    /// Economic terms snapshotted at commitment (INV-51); the buyback rate is deliberately absent
    /// (D-006). **No `s_W`**: D-057 closed as option B, so the draw reduces over live `W` rather
    /// than a range pinned here, and live `W` is the modulus third parties reproduce (AC-55).
    pub s_price: u64,
    pub s_ticket: u64,
    pub s_fee: u64,
    pub s_alloc_bps: [u16; 3],
    pub s_blank_faces: [u64; 3],

    pub rolls: [RollRecord; 10],
    pub bump: u8,
}
