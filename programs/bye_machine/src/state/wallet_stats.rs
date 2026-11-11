use anchor_lang::prelude::*;

/// Per-wallet, per-pool rollup of active positions. Seeds:
/// `[WALLET_STATS_SEED, pool, owner]`.
#[account]
#[derive(InitSpace)]
pub struct WalletStats {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub active_positions: u32,
    pub active_value: u64,
    pub bump: u8,
}
