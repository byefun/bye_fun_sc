use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_lang::solana_program::program_memory::sol_memcmp;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::common::constants::MAX_BATCH_CAPACITY;
use crate::common::errors::ByeMachineError;
use crate::common::events::PoolInitialized;
use crate::common::seeds::{
    POOL_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED, TREASURY_04_SEED,
};
use crate::state::{Pool, ProtocolConfig, TopTier, WeightIndex};

/// `TopTier.entries` capacity — a `tier_size` above it would make `settle_roll`'s tier gate
/// permanently unsatisfiable.
const MAX_TIER_SIZE: u8 = 20;

/// Comparison window for the payload scan. `sol_memcmp` is charged per 250 bytes rather than
/// per byte, so the whole 196,664-byte payload costs ~49 syscalls instead of a 172k-CU loop.
static ZERO_CHUNK: [u8; 4096] = [0u8; 4096];

/// The genesis config a pool is created with — every parameter that lives on `Pool`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PoolConfigArgs {
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
}

/// `admission_ceiling == 0` and `wallet_value_cap == 0` are the "no limit" encodings and are
/// therefore not bounded here.
fn validate_config(args: &PoolConfigArgs) -> Result<()> {
    let alloc_sum = u32::from(args.alloc_equal_bps)
        + u32::from(args.alloc_tier_bps)
        + u32::from(args.alloc_protocol_bps);
    require!(
        alloc_sum == 10_000,
        ByeMachineError::AllocationBpsNotFullyAllocated
    );

    require!(
        args.price.checked_sub(args.ticket_target) == Some(args.fee),
        ByeMachineError::FeeNotPriceMinusTicket
    );

    require!(
        args.admission_floor > args.ticket_target,
        ByeMachineError::AdmissionFloorNotAboveTicket
    );

    for face in args.blank_faces {
        require!(face > 0, ByeMachineError::BlankFaceZero);
        require!(
            face < args.ticket_target,
            ByeMachineError::BlankFaceNotBelowTicket
        );
    }

    require!(
        (1..=(MAX_BATCH_CAPACITY as u8)).contains(&args.max_n),
        ByeMachineError::MaxNOutOfBounds
    );
    require!(
        (1..=MAX_TIER_SIZE).contains(&args.tier_size),
        ByeMachineError::TierSizeOutOfBounds
    );

    Ok(())
}

/// Rejects any pinned `WeightIndex` account that is not blank at the exact layout size. The
/// owner and discriminator halves come from `#[account(zero)]`; length and payload do not, and
/// pre-seeded leaves would be invisible to the desync assert if `total_weight` were seeded to
/// match them.
fn assert_weight_index_blank(weight_index: &AccountInfo) -> Result<()> {
    require!(
        weight_index.data_len() == 8 + WeightIndex::INIT_SPACE,
        ByeMachineError::WeightIndexNotBlank
    );

    let data = weight_index.try_borrow_data()?;
    for chunk in data[8..].chunks(ZERO_CHUNK.len()) {
        require!(
            sol_memcmp(chunk, &ZERO_CHUNK[..chunk.len()], chunk.len()) == 0,
            ByeMachineError::WeightIndexNotBlank
        );
    }

    Ok(())
}

pub fn handler(ctx: Context<InitPool>, args: PoolConfigArgs) -> Result<()> {
    validate_config(&args)?;

    let weight_index_info = ctx.accounts.weight_index.to_account_info();
    assert_weight_index_blank(&weight_index_info)?;

    let pool_id = ctx.accounts.protocol_config.pool_counter;
    let pool_key = ctx.accounts.pool.key();
    let weight_index_key = ctx.accounts.weight_index.key();

    ctx.accounts.weight_index.load_init()?.pool = pool_key;

    let pool = &mut ctx.accounts.pool;
    pool.pool_id = pool_id;
    pool.weight_index = weight_index_key;
    pool.treasury_03 = args.treasury_03;
    pool.price = args.price;
    pool.ticket_target = args.ticket_target;
    pool.fee = args.fee;
    pool.alloc_equal_bps = args.alloc_equal_bps;
    pool.alloc_tier_bps = args.alloc_tier_bps;
    pool.alloc_protocol_bps = args.alloc_protocol_bps;
    pool.admission_floor = args.admission_floor;
    pool.admission_ceiling = args.admission_ceiling;
    pool.wallet_value_cap = args.wallet_value_cap;
    pool.max_n = args.max_n;
    pool.buyback_rate_bps = args.buyback_rate_bps;
    pool.blank_faces = args.blank_faces;
    pool.t04_ceiling = args.t04_ceiling;
    pool.sweep_cadence_hours = args.sweep_cadence_hours;
    pool.tier_size = args.tier_size;
    pool.deposits_paused = false;
    pool.rolls_paused = false;
    pool.pause_reason = 0;
    pool.n_real = 0;
    pool.blanks = [0; 3];
    pool.w_real = 0;
    pool.open_batches = 0;
    pool.batch_counter = 0;
    pool.position_counter = 0;
    pool.pending_roll_liability = 0;
    pool.owed_fees = 0;
    pool.acc_equal = 0;
    pool.sweep_pending = false;
    pool.sweep_epoch = 0;
    pool.last_sweep_at = 0;
    pool.sweep_updates = 0;
    pool.bump = ctx.bumps.pool;

    let top_tier = &mut ctx.accounts.top_tier;
    top_tier.pool = pool_key;
    top_tier.len = 0;
    top_tier.acc_tier = 0;
    top_tier.bump = ctx.bumps.top_tier;

    let protocol_config = &mut ctx.accounts.protocol_config;
    protocol_config.pool_counter = pool_id
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    emit!(PoolInitialized {
        pool: pool_key,
        slot: Clock::get()?.slot,
        authority: ctx.accounts.administrator.key(),
        pool_id,
        weight_index: weight_index_key,
        treasury_03: args.treasury_03,
        price: args.price,
        ticket_target: args.ticket_target,
        fee: args.fee,
        alloc_equal_bps: args.alloc_equal_bps,
        alloc_tier_bps: args.alloc_tier_bps,
        alloc_protocol_bps: args.alloc_protocol_bps,
        admission_floor: args.admission_floor,
        admission_ceiling: args.admission_ceiling,
        wallet_value_cap: args.wallet_value_cap,
        max_n: args.max_n,
        buyback_rate_bps: args.buyback_rate_bps,
        blank_faces: args.blank_faces,
        t04_ceiling: args.t04_ceiling,
        sweep_cadence_hours: args.sweep_cadence_hours,
        tier_size: args.tier_size,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct InitPool<'info> {
    #[account(mut)]
    pub administrator: Signer<'info>,

    #[account(
        mut,
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        init,
        payer = administrator,
        space = 8 + Pool::INIT_SPACE,
        seeds = [POOL_SEED, protocol_config.pool_counter.to_le_bytes().as_ref()],
        bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(zero)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    #[account(
        init,
        payer = administrator,
        space = 8 + TopTier::INIT_SPACE,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(
        init,
        payer = administrator,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = pool
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init,
        payer = administrator,
        seeds = [TREASURY_04_SEED, pool.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = pool
    )]
    pub treasury_04: Box<Account<'info, TokenAccount>>,

    #[account(address = protocol_config.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        DEFAULT_ADMISSION_FLOOR, DEFAULT_ALLOC_EQUAL_BPS, DEFAULT_ALLOC_PROTOCOL_BPS,
        DEFAULT_ALLOC_TIER_BPS, DEFAULT_BLANK_FACES, DEFAULT_BUYBACK_RATE_BPS, DEFAULT_FEE,
        DEFAULT_PRICE, DEFAULT_SWEEP_CADENCE_HOURS, DEFAULT_T04_CEILING, DEFAULT_TICKET_TARGET,
        DEFAULT_TIER_SIZE, FENWICK_CAPACITY,
    };

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn launch_args() -> PoolConfigArgs {
        PoolConfigArgs {
            treasury_03: Pubkey::default(),
            price: DEFAULT_PRICE,
            ticket_target: DEFAULT_TICKET_TARGET,
            fee: DEFAULT_FEE,
            alloc_equal_bps: DEFAULT_ALLOC_EQUAL_BPS,
            alloc_tier_bps: DEFAULT_ALLOC_TIER_BPS,
            alloc_protocol_bps: DEFAULT_ALLOC_PROTOCOL_BPS,
            admission_floor: DEFAULT_ADMISSION_FLOOR,
            admission_ceiling: 0,
            wallet_value_cap: 0,
            max_n: MAX_BATCH_CAPACITY as u8,
            buyback_rate_bps: DEFAULT_BUYBACK_RATE_BPS,
            blank_faces: DEFAULT_BLANK_FACES,
            t04_ceiling: DEFAULT_T04_CEILING,
            sweep_cadence_hours: DEFAULT_SWEEP_CADENCE_HOURS,
            tier_size: DEFAULT_TIER_SIZE,
        }
    }

    // --- the launch profile and its "no limit" encodings -------------------------------------

    #[test]
    fn accepts_the_launch_config() {
        assert!(validate_config(&launch_args()).is_ok());
    }

    #[test]
    fn accepts_zero_ceiling_and_zero_wallet_cap_as_no_limit() {
        let mut args = launch_args();
        args.admission_ceiling = 0;
        args.wallet_value_cap = 0;
        assert!(validate_config(&args).is_ok());
    }

    #[test]
    fn accepts_a_configured_ceiling_and_wallet_cap() {
        let mut args = launch_args();
        args.admission_ceiling = DEFAULT_ADMISSION_FLOOR;
        args.wallet_value_cap = DEFAULT_ADMISSION_FLOOR * 4;
        assert!(validate_config(&args).is_ok());
    }

    // --- every constraint, by exact numeric code ---------------------------------------------

    #[test]
    fn rejects_allocation_bps_not_summing_to_ten_thousand() {
        let mut args = launch_args();
        args.alloc_tier_bps += 1;
        assert_eq!(error_code(validate_config(&args).unwrap_err()), 6501);
    }

    #[test]
    fn rejects_allocation_bps_that_would_overflow_a_u16_sum() {
        let mut args = launch_args();
        args.alloc_equal_bps = u16::MAX;
        args.alloc_tier_bps = u16::MAX;
        args.alloc_protocol_bps = u16::MAX;
        assert_eq!(error_code(validate_config(&args).unwrap_err()), 6501);
    }

    #[test]
    fn rejects_fee_that_is_not_price_minus_ticket_target() {
        let mut args = launch_args();
        args.fee += 1;
        assert_eq!(error_code(validate_config(&args).unwrap_err()), 6505);
    }

    #[test]
    fn rejects_price_below_ticket_target_without_panicking() {
        let mut args = launch_args();
        args.price = args.ticket_target - 1;
        args.fee = 0;
        assert_eq!(error_code(validate_config(&args).unwrap_err()), 6505);
    }

    #[test]
    fn rejects_admission_floor_at_or_below_the_ticket_target() {
        for floor in [DEFAULT_TICKET_TARGET, DEFAULT_TICKET_TARGET - 1] {
            let mut args = launch_args();
            args.admission_floor = floor;
            assert_eq!(error_code(validate_config(&args).unwrap_err()), 6500);
        }
    }

    #[test]
    fn rejects_any_blank_face_at_or_above_the_ticket_target() {
        for idx in 0..3 {
            let mut args = launch_args();
            args.blank_faces[idx] = DEFAULT_TICKET_TARGET;
            assert_eq!(error_code(validate_config(&args).unwrap_err()), 6502);
        }
    }

    #[test]
    fn rejects_any_blank_face_at_zero() {
        for idx in 0..3 {
            let mut args = launch_args();
            args.blank_faces[idx] = 0;
            assert_eq!(error_code(validate_config(&args).unwrap_err()), 6511);
        }
    }

    #[test]
    fn rejects_max_n_outside_one_through_batch_capacity() {
        for max_n in [0u8, MAX_BATCH_CAPACITY as u8 + 1, u8::MAX] {
            let mut args = launch_args();
            args.max_n = max_n;
            assert_eq!(error_code(validate_config(&args).unwrap_err()), 6506);
        }
    }

    #[test]
    fn rejects_tier_size_outside_one_through_twenty() {
        for tier_size in [0u8, MAX_TIER_SIZE + 1, u8::MAX] {
            let mut args = launch_args();
            args.tier_size = tier_size;
            assert_eq!(error_code(validate_config(&args).unwrap_err()), 6504);
        }
    }

    // --- The WeightIndex pinning guard ----------------------------------------------------

    const DISCRIMINATOR: usize = 8;
    const ACCOUNT_LEN: usize = DISCRIMINATOR + WeightIndex::INIT_SPACE;
    const HEADER_END: usize = DISCRIMINATOR + 56;
    const TREE_END: usize = HEADER_END + 8 * FENWICK_CAPACITY;

    fn blank_account() -> Vec<u8> {
        vec![0u8; ACCOUNT_LEN]
    }

    fn check_blank(data: &mut [u8]) -> Result<()> {
        let key = Pubkey::new_unique();
        let owner = crate::ID;
        let mut lamports = 0u64;
        let info = AccountInfo::new(&key, false, true, &mut lamports, data, &owner, false, 0);
        assert_weight_index_blank(&info)
    }

    fn rejection_code_for_non_zero_byte_at(offset: usize) -> u32 {
        let mut data = blank_account();
        data[offset] = 1;
        error_code(check_blank(&mut data).unwrap_err())
    }

    #[test]
    fn weight_index_account_size_is_the_design_figure() {
        assert_eq!(ACCOUNT_LEN, 196_672);
        assert_eq!(TREE_END + 4 * FENWICK_CAPACITY, ACCOUNT_LEN);
    }

    #[test]
    fn the_zero_scan_covers_the_payload_without_a_remainder() {
        assert_eq!(WeightIndex::INIT_SPACE % 8, 0);
        assert_eq!(WeightIndex::INIT_SPACE / 8, 24_583);
    }

    #[test]
    fn accepts_a_blank_account_at_the_exact_layout_size() {
        assert!(check_blank(&mut blank_account()).is_ok());
    }

    #[test]
    fn rejects_an_account_one_byte_short_of_the_layout_size() {
        let mut data = vec![0u8; ACCOUNT_LEN - 1];
        assert_eq!(error_code(check_blank(&mut data).unwrap_err()), 6702);
    }

    #[test]
    fn rejects_an_account_one_byte_over_the_layout_size() {
        let mut data = vec![0u8; ACCOUNT_LEN + 1];
        assert_eq!(error_code(check_blank(&mut data).unwrap_err()), 6702);
    }

    #[test]
    fn rejects_a_pre_seeded_header() {
        for offset in [DISCRIMINATOR, DISCRIMINATOR + 32, HEADER_END - 1] {
            assert_eq!(rejection_code_for_non_zero_byte_at(offset), 6702);
        }
    }

    #[test]
    fn rejects_a_pre_seeded_tree_leaf() {
        for offset in [
            HEADER_END,
            HEADER_END + 8 * (FENWICK_CAPACITY / 2),
            TREE_END - 1,
        ] {
            assert_eq!(rejection_code_for_non_zero_byte_at(offset), 6702);
        }
    }

    #[test]
    fn rejects_a_pre_seeded_free_stack() {
        for offset in [TREE_END, TREE_END + 4 * (FENWICK_CAPACITY / 2)] {
            assert_eq!(rejection_code_for_non_zero_byte_at(offset), 6702);
        }
    }

    #[test]
    fn rejects_a_non_zero_final_payload_byte() {
        assert_eq!(rejection_code_for_non_zero_byte_at(ACCOUNT_LEN - 1), 6702);
    }

    #[test]
    fn rejects_a_non_zero_byte_at_a_stride_across_the_whole_payload() {
        let mut data = blank_account();
        // 4093 is prime and coprime with 8, so the sample lands on every byte position within
        // a word and spreads over the header, the tree and the free stack alike.
        for offset in (DISCRIMINATOR..ACCOUNT_LEN).step_by(4093) {
            data[offset] = 1;
            let code = error_code(check_blank(&mut data).unwrap_err());
            assert_eq!(
                code, 6702,
                "a non-zero byte at offset {offset} was accepted"
            );
            data[offset] = 0;
        }
    }
}
