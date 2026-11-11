use anchor_lang::prelude::*;

/// A single ranked slot in `TopTier.entries`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace)]
pub struct TierEntry {
    pub position: Pubkey,
    pub value: u64,
    pub activated_at: i64,
    pub position_id: u64,
}

/// Top-value ranking, sorted `value` desc, `activated_at` asc, `position_id` asc.
/// Seeds: `[TOP_TIER_SEED, pool]`.
#[account]
#[derive(InitSpace)]
pub struct TopTier {
    pub pool: Pubkey,
    pub len: u8,
    pub entries: [TierEntry; 20],
    pub acc_tier: u128,
    pub bump: u8,
}
