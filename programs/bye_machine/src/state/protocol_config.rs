use anchor_lang::prelude::*;

/// Singleton protocol-wide configuration. Seeds: `[PROTOCOL_CONFIG_SEED]`.
#[account]
#[derive(InitSpace)]
pub struct ProtocolConfig {
    pub administrator: Pubkey,
    /// The balance Treasury 04 is swept back to, in micro-USDC, for every pool at once. 0 until
    /// the Administrator sets it, and `sweep_t04_overflow` refuses while it is 0.
    pub t04_ceiling: u64,
    /// Unused; zero on mainnet. The `reserved_*` fields keep the account's size and every later
    /// offset.
    pub reserved_0: [u8; 24],
    pub reserved_1: Pubkey,
    pub reserved_2: Pubkey,
    pub reserved_3: Pubkey,
    pub operator: Pubkey,
    /// The delegated $BYE-purchase key. Treasury 05 is this key's USDC associated token account,
    /// so the authority is what the address is derived from rather than a second stored field —
    /// `settle_roll` credits Treasury 05 on every roll and never debits it, and this key spends
    /// it outside any instruction of this program.
    ///
    /// Occupies the byte position Treasury 03's authority vacated (T03 is now a program PDA),
    /// so the account layout is unchanged. Until an Administrator sets it, it holds the default
    /// key and the derived Treasury 05 address is the default key's ATA.
    pub t05_authority: Pubkey,
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
