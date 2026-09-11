use anchor_lang::prelude::*;

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
    pub blank_faces: [u64; 3],
    pub t04_ceiling: u64,
    pub sweep_cadence_hours: u16,
    pub tier_size: u8,

    pub deposits_paused: bool,
    pub rolls_paused: bool,
    pub pause_reason: u16,

    pub n_real: u32,
    pub blanks: [u32; 3],
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
}

#[cfg(test)]
impl Pool {
    /// Every field zeroed, for tests that set only the handful they care about. Not a `Default`
    /// impl: an all-zero `Pool` is not a valid pool (`tier_size` 0 and `max_n` 0 are both out of
    /// their pinned ranges), and giving the type a `Default` would let that value reach code that
    /// assumes a configured one.
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
}
