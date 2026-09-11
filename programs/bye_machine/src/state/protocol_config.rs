use anchor_lang::prelude::*;

/// Singleton protocol-wide configuration. Seeds: `[PROTOCOL_CONFIG_SEED]`.
#[account]
#[derive(InitSpace)]
pub struct ProtocolConfig {
    pub administrator: Pubkey,
    pub pending_administrator: Pubkey,
    pub pending_operator: Pubkey,
    pub pending_t03_authority: Pubkey,
    pub pending_t02_authority: Pubkey,
    pub operator: Pubkey,
    pub t03_authority: Pubkey,
    pub t02_authority: Pubkey,
    pub protocol_revenue: Pubkey,
    pub usdc_mint: Pubkey,
    pub bye_mint: Pubkey,
    pub vrf_program: Pubkey,
    pub oracle_queue: Pubkey,
    pub swap_pool: Pubkey,
    pub max_swap_slippage_bps: u16,
    pub pool_counter: u16,
    pub bump: u8,
}
