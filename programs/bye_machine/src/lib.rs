#![allow(deprecated)]

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
    AdmitCollection, InitPool, InitProtocol, SetAuthorities, SetPause, UpdateConfig,
    UpdateVrfConfig, WithdrawCollection,
};
use instructions::deposit::{
    ApproveDeposit, Deposit, DepositCore, RejectDeposit, ReturnRejected, ReturnRejectedCore,
};
use instructions::exit::{ClaimNft, ClaimNftCore, CloseSeized, Withdraw, WithdrawCore};
use instructions::roll::{CommitRolls, VrfCallback};
use instructions::tier::UpdateTier;
use instructions::value::{BeginSweep, EndSweep, RecordValue};

// Anchor 0.31.1: the `#[program]` macro needs each instruction's `__client_accounts_*` module
// in scope or it emits E0432 (anchor#2772).
use instructions::admin::admit_collection::__client_accounts_admit_collection;
use instructions::admin::init_pool::__client_accounts_init_pool;
use instructions::admin::init_protocol::__client_accounts_init_protocol;
use instructions::admin::set_authorities::__client_accounts_set_authorities;
use instructions::admin::set_pause::__client_accounts_set_pause;
use instructions::admin::update_config::__client_accounts_update_config;
use instructions::admin::update_vrf_config::__client_accounts_update_vrf_config;
use instructions::admin::withdraw_collection::__client_accounts_withdraw_collection;
use instructions::deposit::approve_deposit::__client_accounts_approve_deposit;
use instructions::deposit::deposit::__client_accounts_deposit;
use instructions::deposit::deposit_core::__client_accounts_deposit_core;
use instructions::deposit::reject_deposit::__client_accounts_reject_deposit;
use instructions::deposit::return_rejected::__client_accounts_return_rejected;
use instructions::deposit::return_rejected_core::__client_accounts_return_rejected_core;
use instructions::exit::claim_nft::__client_accounts_claim_nft;
use instructions::exit::claim_nft_core::__client_accounts_claim_nft_core;
use instructions::exit::close_seized::__client_accounts_close_seized;
use instructions::exit::withdraw::__client_accounts_withdraw;
use instructions::exit::withdraw_core::__client_accounts_withdraw_core;
use instructions::roll::commit_rolls::__client_accounts_commit_rolls;
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
    ) -> Result<()> {
        instructions::admin::update_vrf_config::handler(ctx, vrf_program, oracle_queue, swap_pool)
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
        new_authority: Option<Pubkey>,
    ) -> Result<()> {
        instructions::admin::set_authorities::handler(ctx, role, new_authority)
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

    pub fn claim_nft(ctx: Context<ClaimNft>) -> Result<()> {
        instructions::exit::claim_nft::handler(ctx)
    }

    pub fn claim_nft_core(ctx: Context<ClaimNftCore>) -> Result<()> {
        instructions::exit::claim_nft_core::handler(ctx)
    }

    pub fn close_seized(ctx: Context<CloseSeized>) -> Result<()> {
        instructions::exit::close_seized::handler(ctx)
    }

    pub fn commit_rolls(ctx: Context<CommitRolls>, n: u8) -> Result<()> {
        instructions::roll::commit_rolls::handler(ctx, n)
    }

    pub fn vrf_callback(ctx: Context<VrfCallback>, randomness: [u8; 32]) -> Result<()> {
        instructions::roll::vrf_callback::handler(ctx, randomness)
    }

    pub fn update_tier<'info>(ctx: Context<'_, '_, 'info, 'info, UpdateTier<'info>>) -> Result<()> {
        instructions::tier::update_tier::handler(ctx)
    }
}
