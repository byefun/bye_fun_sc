#![allow(deprecated)]

// `testing` compiles in `test_resolve_batch`, which lets its caller choose a batch's randomness
// without the VRF provider's signature. Combined with `prod` that instruction would ship to
// mainnet, and it would not even look wrong on the way: `INITIAL_ADMINISTRATOR` resolves `prod`
// before `testing`, so such a build takes the production administrator rather than the suite's
// throwaway key. A convention in a README cannot hold a boundary this sharp.
//
// `dev` + `testing` stays legal — it is the localnet suite's own artifact.
#[cfg(all(feature = "prod", feature = "testing"))]
compile_error!(
    "features `prod` and `testing` are mutually exclusive: `testing` compiles in \
     test_resolve_batch, which resolves a batch to caller-chosen randomness with no VRF \
     signature. Build the mainnet artifact with `--features prod` alone."
);

use anchor_lang::prelude::*;

mod program_id;
use program_id::PROGRAM_ID;

pub mod common;
pub mod instructions;
pub mod state;

use common::events::AuthorityRole;
use instructions::admin::init_pool::PoolConfigArgs;
use instructions::admin::init_protocol::ProtocolConfigArgs;
use instructions::admin::update_config::ConfigUpdate;
use instructions::admin::{
    AdmitCollection, ConsolidateTreasuries, InitPool, InitProtocol, MigratePool, SetAuthorities,
    SetPause, SetT04Ceiling, SweepT04Overflow, UpdateByeMint, UpdateConfig, UpdateProtocolRevenue,
    UpdateSwapPool, UpdateVrfConfig, WithdrawCollection,
};
use instructions::buyback::{ActivateAcquired, InstantSell, InstantSellCore};
use instructions::deposit::{
    ApproveDeposit, Deposit, DepositCore, RejectDeposit, ReturnRejected, ReturnRejectedCore,
    T02Deposit, T02DepositCore,
};
use instructions::escrow::{
    ClaimFeeToEscrow, EscrowDeposit, EscrowWithdraw, OpenEscrow, SetEscrowArmed,
};
use instructions::exit::{
    ClaimFee, ClaimNft, ClaimNftCore, CloseSeized, T02ClaimFee, T02ClaimNft, T02ClaimNftCore,
    T02Withdraw, T02WithdrawCore, Withdraw, WithdrawCore,
};
#[cfg(feature = "testing")]
use instructions::roll::TestResolveBatch;
use instructions::roll::{
    CloseBatch, CommitRolls, RecoverBatch, SettleRoll, SettleRollCore, VrfCallback,
};
use instructions::tier::UpdateTier;
use instructions::value::{BeginSweep, EndSweep, RecordValue};

// Anchor 0.31.1: the `#[program]` macro needs each instruction's `__client_accounts_*` module
// in scope or it emits E0432 (anchor#2772).
use instructions::admin::admit_collection::__client_accounts_admit_collection;
use instructions::admin::consolidate_treasuries::__client_accounts_consolidate_treasuries;
use instructions::admin::init_pool::__client_accounts_init_pool;
use instructions::admin::init_protocol::__client_accounts_init_protocol;
use instructions::admin::migrate_pool::__client_accounts_migrate_pool;
use instructions::admin::set_authorities::__client_accounts_set_authorities;
use instructions::admin::set_pause::__client_accounts_set_pause;
use instructions::admin::set_t04_ceiling::__client_accounts_set_t04_ceiling;
use instructions::admin::sweep_t04_overflow::__client_accounts_sweep_t04_overflow;
use instructions::admin::update_bye_mint::__client_accounts_update_bye_mint;
use instructions::admin::update_config::__client_accounts_update_config;
use instructions::admin::update_protocol_revenue::__client_accounts_update_protocol_revenue;
use instructions::admin::update_swap_pool::__client_accounts_update_swap_pool;
use instructions::admin::update_vrf_config::__client_accounts_update_vrf_config;
use instructions::admin::withdraw_collection::__client_accounts_withdraw_collection;
use instructions::buyback::activate_acquired::__client_accounts_activate_acquired;
use instructions::buyback::instant_sell::__client_accounts_instant_sell;
use instructions::buyback::instant_sell_core::__client_accounts_instant_sell_core;
use instructions::deposit::approve_deposit::__client_accounts_approve_deposit;
use instructions::deposit::deposit::__client_accounts_deposit;
use instructions::deposit::deposit_core::__client_accounts_deposit_core;
use instructions::deposit::reject_deposit::__client_accounts_reject_deposit;
use instructions::deposit::return_rejected::__client_accounts_return_rejected;
use instructions::deposit::return_rejected_core::__client_accounts_return_rejected_core;
use instructions::deposit::t02_deposit::__client_accounts_t02_deposit;
use instructions::deposit::t02_deposit_core::__client_accounts_t02_deposit_core;
use instructions::escrow::claim_fee_to_escrow::__client_accounts_claim_fee_to_escrow;
use instructions::escrow::escrow_deposit::__client_accounts_escrow_deposit;
use instructions::escrow::escrow_withdraw::__client_accounts_escrow_withdraw;
use instructions::escrow::open_escrow::__client_accounts_open_escrow;
use instructions::escrow::set_escrow_armed::__client_accounts_set_escrow_armed;
use instructions::exit::claim_fee::__client_accounts_claim_fee;
use instructions::exit::claim_nft::__client_accounts_claim_nft;
use instructions::exit::claim_nft_core::__client_accounts_claim_nft_core;
use instructions::exit::close_seized::__client_accounts_close_seized;
use instructions::exit::t02_claim_fee::__client_accounts_t02_claim_fee;
use instructions::exit::t02_claim_nft::__client_accounts_t02_claim_nft;
use instructions::exit::t02_claim_nft_core::__client_accounts_t02_claim_nft_core;
use instructions::exit::t02_withdraw::__client_accounts_t02_withdraw;
use instructions::exit::t02_withdraw_core::__client_accounts_t02_withdraw_core;
use instructions::exit::withdraw::__client_accounts_withdraw;
use instructions::exit::withdraw_core::__client_accounts_withdraw_core;
use instructions::roll::close_batch::__client_accounts_close_batch;
use instructions::roll::commit_rolls::__client_accounts_commit_rolls;
use instructions::roll::recover_batch::__client_accounts_recover_batch;
use instructions::roll::settle_roll::__client_accounts_settle_roll;
use instructions::roll::settle_roll_core::__client_accounts_settle_roll_core;
#[cfg(feature = "testing")]
use instructions::roll::test_resolve_batch::__client_accounts_test_resolve_batch;
use instructions::roll::vrf_callback::__client_accounts_vrf_callback;
use instructions::tier::update_tier::__client_accounts_update_tier;
use instructions::value::begin_sweep::__client_accounts_begin_sweep;
use instructions::value::end_sweep::__client_accounts_end_sweep;
use instructions::value::record_value::__client_accounts_record_value;

declare_id!(PROGRAM_ID);

#[program]
pub mod bye_machine {
    use super::*;

    pub fn init_protocol(ctx: Context<InitProtocol>, args: ProtocolConfigArgs) -> Result<()> {
        instructions::admin::init_protocol::handler(ctx, args)
    }

    pub fn init_pool(ctx: Context<InitPool>, args: PoolConfigArgs) -> Result<()> {
        instructions::admin::init_pool::handler(ctx, args)
    }

    pub fn update_config<'info>(
        ctx: Context<'_, '_, 'info, 'info, UpdateConfig<'info>>,
        update: ConfigUpdate,
    ) -> Result<()> {
        instructions::admin::update_config::handler(ctx, update)
    }

    pub fn update_vrf_config(
        ctx: Context<UpdateVrfConfig>,
        vrf_program: Pubkey,
        oracle_queue: Pubkey,
        swap_pool: Pubkey,
        max_swap_slippage_bps: u16,
    ) -> Result<()> {
        instructions::admin::update_vrf_config::handler(
            ctx,
            vrf_program,
            oracle_queue,
            swap_pool,
            max_swap_slippage_bps,
        )
    }

    pub fn update_bye_mint(ctx: Context<UpdateByeMint>, bye_mint: Option<Pubkey>) -> Result<()> {
        instructions::admin::update_bye_mint::handler(ctx, bye_mint)
    }

    pub fn update_protocol_revenue(ctx: Context<UpdateProtocolRevenue>) -> Result<()> {
        instructions::admin::update_protocol_revenue::handler(ctx)
    }

    pub fn update_swap_pool(ctx: Context<UpdateSwapPool>, swap_pool: Option<Pubkey>) -> Result<()> {
        instructions::admin::update_swap_pool::handler(ctx, swap_pool)
    }

    pub fn admit_collection(
        ctx: Context<AdmitCollection>,
        collection: Pubkey,
        standards: u8,
        reason: Option<u16>,
    ) -> Result<()> {
        instructions::admin::admit_collection::handler(ctx, collection, standards, reason)
    }

    pub fn withdraw_collection(
        ctx: Context<WithdrawCollection>,
        collection: Pubkey,
        reason: u16,
    ) -> Result<()> {
        instructions::admin::withdraw_collection::handler(ctx, collection, reason)
    }

    pub fn set_pause(
        ctx: Context<SetPause>,
        deposits: bool,
        rolls: bool,
        reason: u16,
    ) -> Result<()> {
        instructions::admin::set_pause::handler(ctx, deposits, rolls, reason)
    }

    pub fn set_authorities(
        ctx: Context<SetAuthorities>,
        role: AuthorityRole,
        new_authority: Pubkey,
    ) -> Result<()> {
        instructions::admin::set_authorities::handler(ctx, role, new_authority)
    }

    pub fn sweep_t04_overflow<'info>(
        ctx: Context<'_, '_, 'info, 'info, SweepT04Overflow<'info>>,
    ) -> Result<()> {
        instructions::admin::sweep_t04_overflow::handler(ctx)
    }

    pub fn set_t04_ceiling(ctx: Context<SetT04Ceiling>, ceiling: u64) -> Result<()> {
        instructions::admin::set_t04_ceiling::handler(ctx, ceiling)
    }

    pub fn consolidate_treasuries(ctx: Context<ConsolidateTreasuries>) -> Result<()> {
        instructions::admin::consolidate_treasuries::handler(ctx)
    }

    pub fn migrate_pool(ctx: Context<MigratePool>) -> Result<()> {
        instructions::admin::migrate_pool::handler(ctx)
    }

    pub fn deposit(ctx: Context<Deposit>) -> Result<()> {
        instructions::deposit::deposit::handler(ctx)
    }

    pub fn deposit_core(ctx: Context<DepositCore>) -> Result<()> {
        instructions::deposit::deposit_core::handler(ctx)
    }

    pub fn approve_deposit<'info>(
        ctx: Context<'_, '_, 'info, 'info, ApproveDeposit<'info>>,
        value: u64,
        observed_at: i64,
        lock_until: i64,
    ) -> Result<()> {
        instructions::deposit::approve_deposit::handler(ctx, value, observed_at, lock_until)
    }

    pub fn reject_deposit(ctx: Context<RejectDeposit>, reason: u16) -> Result<()> {
        instructions::deposit::reject_deposit::handler(ctx, reason)
    }

    pub fn return_rejected(ctx: Context<ReturnRejected>) -> Result<()> {
        instructions::deposit::return_rejected::handler(ctx)
    }

    pub fn return_rejected_core(ctx: Context<ReturnRejectedCore>) -> Result<()> {
        instructions::deposit::return_rejected_core::handler(ctx)
    }

    pub fn t02_deposit(ctx: Context<T02Deposit>) -> Result<()> {
        instructions::deposit::t02_deposit::handler(ctx)
    }

    pub fn t02_deposit_core(ctx: Context<T02DepositCore>) -> Result<()> {
        instructions::deposit::t02_deposit_core::handler(ctx)
    }

    pub fn instant_sell<'info>(
        ctx: Context<'_, '_, 'info, 'info, InstantSell<'info>>,
        value: u64,
        observed_at: i64,
    ) -> Result<()> {
        instructions::buyback::instant_sell::handler(ctx, value, observed_at)
    }

    pub fn instant_sell_core<'info>(
        ctx: Context<'_, '_, 'info, 'info, InstantSellCore<'info>>,
        value: u64,
        observed_at: i64,
    ) -> Result<()> {
        instructions::buyback::instant_sell_core::handler(ctx, value, observed_at)
    }

    pub fn activate_acquired<'info>(
        ctx: Context<'_, '_, 'info, 'info, ActivateAcquired<'info>>,
    ) -> Result<()> {
        instructions::buyback::activate_acquired::handler(ctx)
    }

    pub fn begin_sweep(ctx: Context<BeginSweep>) -> Result<()> {
        instructions::value::begin_sweep::handler(ctx)
    }

    pub fn record_value<'info>(
        ctx: Context<'_, '_, 'info, 'info, RecordValue<'info>>,
        value: u64,
        observed_at: i64,
    ) -> Result<()> {
        instructions::value::record_value::handler(ctx, value, observed_at)
    }

    pub fn end_sweep(ctx: Context<EndSweep>) -> Result<()> {
        instructions::value::end_sweep::handler(ctx)
    }

    pub fn withdraw<'info>(ctx: Context<'_, '_, 'info, 'info, Withdraw<'info>>) -> Result<()> {
        instructions::exit::withdraw::handler(ctx)
    }

    pub fn withdraw_core<'info>(
        ctx: Context<'_, '_, 'info, 'info, WithdrawCore<'info>>,
    ) -> Result<()> {
        instructions::exit::withdraw_core::handler(ctx)
    }

    pub fn t02_withdraw<'info>(
        ctx: Context<'_, '_, 'info, 'info, T02Withdraw<'info>>,
    ) -> Result<()> {
        instructions::exit::t02_withdraw::handler(ctx)
    }

    pub fn t02_withdraw_core<'info>(
        ctx: Context<'_, '_, 'info, 'info, T02WithdrawCore<'info>>,
    ) -> Result<()> {
        instructions::exit::t02_withdraw_core::handler(ctx)
    }

    pub fn claim_fee(ctx: Context<ClaimFee>) -> Result<()> {
        instructions::exit::claim_fee::handler(ctx)
    }

    pub fn t02_claim_fee(ctx: Context<T02ClaimFee>) -> Result<()> {
        instructions::exit::t02_claim_fee::handler(ctx)
    }

    pub fn t02_claim_nft(ctx: Context<T02ClaimNft>) -> Result<()> {
        instructions::exit::t02_claim_nft::handler(ctx)
    }

    pub fn t02_claim_nft_core(ctx: Context<T02ClaimNftCore>) -> Result<()> {
        instructions::exit::t02_claim_nft_core::handler(ctx)
    }

    pub fn claim_nft(ctx: Context<ClaimNft>) -> Result<()> {
        instructions::exit::claim_nft::handler(ctx)
    }

    pub fn claim_nft_core(ctx: Context<ClaimNftCore>) -> Result<()> {
        instructions::exit::claim_nft_core::handler(ctx)
    }

    pub fn close_seized(ctx: Context<CloseSeized>) -> Result<()> {
        instructions::exit::close_seized::handler(ctx)
    }

    pub fn open_escrow(ctx: Context<OpenEscrow>) -> Result<()> {
        instructions::escrow::open_escrow::handler(ctx)
    }

    pub fn escrow_deposit(ctx: Context<EscrowDeposit>, amount: u64) -> Result<()> {
        instructions::escrow::escrow_deposit::handler(ctx, amount)
    }

    pub fn escrow_withdraw(ctx: Context<EscrowWithdraw>, amount: u64) -> Result<()> {
        instructions::escrow::escrow_withdraw::handler(ctx, amount)
    }

    pub fn set_escrow_armed(ctx: Context<SetEscrowArmed>, armed: bool) -> Result<()> {
        instructions::escrow::set_escrow_armed::handler(ctx, armed)
    }

    pub fn claim_fee_to_escrow(ctx: Context<ClaimFeeToEscrow>) -> Result<()> {
        instructions::escrow::claim_fee_to_escrow::handler(ctx)
    }

    pub fn commit_rolls(ctx: Context<CommitRolls>, n: u8) -> Result<()> {
        instructions::roll::commit_rolls::handler(ctx, n)
    }

    pub fn vrf_callback(ctx: Context<VrfCallback>, randomness: [u8; 32]) -> Result<()> {
        instructions::roll::vrf_callback::handler(ctx, randomness)
    }

    pub fn settle_roll<'info>(
        ctx: Context<'_, '_, 'info, 'info, SettleRoll<'info>>,
        roll_index: u8,
    ) -> Result<()> {
        instructions::roll::settle_roll::handler(ctx, roll_index)
    }

    pub fn settle_roll_core<'info>(
        ctx: Context<'_, '_, 'info, 'info, SettleRollCore<'info>>,
        roll_index: u8,
    ) -> Result<()> {
        instructions::roll::settle_roll_core::handler(ctx, roll_index)
    }

    pub fn recover_batch(ctx: Context<RecoverBatch>) -> Result<()> {
        instructions::roll::recover_batch::handler(ctx)
    }

    pub fn close_batch(ctx: Context<CloseBatch>) -> Result<()> {
        instructions::roll::close_batch::handler(ctx)
    }

    #[cfg(feature = "testing")]
    pub fn test_resolve_batch(ctx: Context<TestResolveBatch>, randomness: [u8; 32]) -> Result<()> {
        instructions::roll::test_resolve_batch::handler(ctx, randomness)
    }

    pub fn update_tier<'info>(ctx: Context<'_, '_, 'info, 'info, UpdateTier<'info>>) -> Result<()> {
        instructions::tier::update_tier::handler(ctx)
    }
}
