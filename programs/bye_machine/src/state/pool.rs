use anchor_lang::prelude::*;

use crate::common::constants::BLANK_SLOTS;
use crate::common::errors::ByeMachineError;

/// A single pool's config, live counters, and accrual state. Seeds:
/// `[POOL_SEED, pool_id: u16 le]`. The `Pool` PDA also signs as its own vaults'
/// transfer authority.
#[account]
#[derive(InitSpace)]
pub struct Pool {
    pub pool_id: u16,
    pub weight_index: Pubkey,
    pub treasury_03: Pubkey,

    pub price: u64,
    pub ticket_target: u64,
    pub fee: u64,
    pub alloc_equal_bps: u16,
    pub alloc_tier_bps: u16,
    pub alloc_protocol_bps: u16,
    pub admission_floor: u64,
    pub admission_ceiling: u64,
    pub wallet_value_cap: u64,
    pub max_n: u8,
    pub buyback_rate_bps: u16,
    /// Slots 0–2 below the ticket, slot 3 above it or 0, slots 4–7 always 0.
    pub blank_faces: [u64; BLANK_SLOTS],
    /// No longer read or written: Treasury 04's ceiling is `ProtocolConfig.t04_ceiling`, for every
    /// pool. The field keeps its bytes so every later offset holds.
    pub t04_ceiling: u64,
    pub sweep_cadence_hours: u16,
    pub tier_size: u8,

    pub deposits_paused: bool,
    pub rolls_paused: bool,
    pub pause_reason: u16,

    pub n_real: u32,
    /// One count per `blank_faces` slot; 0 wherever the face is 0.
    pub blanks: [u32; BLANK_SLOTS],
    pub w_real: u128,
    pub open_batches: u32,
    pub batch_counter: u64,
    pub position_counter: u64,

    pub pending_roll_liability: u64,
    pub owed_fees: u64,

    pub acc_equal: u128,

    pub sweep_pending: bool,
    pub sweep_epoch: u64,
    pub last_sweep_at: i64,
    pub sweep_updates: u32,

    pub bump: u8,

    /// What an armed escrow pays for a card sold back into this pool, in bps of its value. 0 means
    /// the pool has no escrow path; once set it stays within `buyback_rate_bps..=10_000`.
    pub escrow_bid_bps: u16,
    /// The protocol-wide Treasury 04, `PDA(["t04"])`. Default on a pool created before Treasury 04
    /// was shared, until `consolidate_treasuries` writes it; no instruction that pins Treasury 04
    /// can run on such a pool before then.
    pub treasury_04: Pubkey,
    /// Zero, so a later field can be appended without reallocating every pool.
    pub reserved: [u8; 32],
}

impl Pool {
    /// The batch id of the pool's settlement queue head — the earliest-committed batch not yet
    /// `Complete`/`Recovered`. Computed rather than stored: `commit_rolls` is the only writer of
    /// `batch_counter`, always alongside `open_batches`, and `settle_roll`/`settle_roll_core`'s
    /// completion path plus `recover_batch` are the only decrementers of `open_batches`, each by
    /// exactly one on a batch's terminal — so `batch_counter - open_batches` tracks the head with
    /// no independent state to desync.
    pub fn head_batch_id(&self) -> Result<u64> {
        self.batch_counter
            .checked_sub(self.open_batches as u64)
            .ok_or(ByeMachineError::ArithmeticFailure.into())
    }
}

#[cfg(test)]
impl Pool {
    /// Every field zeroed, for tests that set only the handful they care about. Not a `Default`
    /// impl: an all-zero `Pool` is not a valid pool (`tier_size` and `max_n` of 0 are out of
    /// range), and a `Default` would let it reach code that assumes a configured one.
    pub(crate) fn blank() -> Self {
        Self {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_batch_id_is_batch_counter_minus_open_batches() {
        let mut pool = Pool::blank();
        pool.batch_counter = 5;
        pool.open_batches = 2;
        assert_eq!(pool.head_batch_id().unwrap(), 3);
    }

    #[test]
    fn head_batch_id_is_zero_at_pool_genesis() {
        let pool = Pool::blank();
        assert_eq!(pool.head_batch_id().unwrap(), 0);
    }

    #[test]
    fn head_batch_id_fails_closed_rather_than_underflowing() {
        let mut pool = Pool::blank();
        pool.batch_counter = 1;
        pool.open_batches = 2;
        assert!(pool.head_batch_id().is_err());
    }
}
