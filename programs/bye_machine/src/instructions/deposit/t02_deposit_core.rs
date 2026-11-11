use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use mpl_core::ID as MPL_CORE_ID;

use crate::common::core_asset::{declared_core_collection, read_core_asset, transfer_core};
use crate::common::errors::ByeMachineError;
use crate::common::events::DepositPending;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PROTOCOL_CONFIG_SEED, TREASURY_02_SEED,
    WALLET_STATS_SEED,
};
use crate::common::standards::STANDARD_CORE;
use crate::state::{Pool, PoolCollection, Position, PositionState, ProtocolConfig, WalletStats};

/// `t02_deposit`'s MPL-Core twin.
///
/// **Three things it does not do, each deliberate.** There is no master-edition guard: MPL Core has
/// no edition mechanism at all, so the print attack the Token Metadata path blocks has no Core
/// analogue and asserting one would reject every Core deposit. There is no vault **token account**
/// — `PDA(["vault", position])` holds the asset directly as `AssetV1.owner`, which makes this the
/// one twin that is *cheaper* than its principal in both accounts and rent. And there is no
/// `token_standard` resolution: the owner program *is* the standard on this family, so
/// `Position.standard` is written as `STANDARD_CORE` from the branch the caller reached rather
/// than from a field read.
///
/// **`newOwner` is an account that does not exist, and that premise is measured.** On localnet
/// MPL Core `TransferV1` accepts a `getAccountInfo -> null` PDA as `[4] newOwner`, writes it
/// into `AssetV1.owner` and does not allocate it, so the vault stays accountless, no rent is
/// paid for it, and nobody can close it.
pub fn handler(ctx: Context<T02DepositCore>) -> Result<()> {
    require!(
        !ctx.accounts.pool.deposits_paused,
        ByeMachineError::DepositsPaused
    );

    let asset_key = ctx.accounts.asset.key();
    let asset = read_core_asset(&ctx.accounts.asset)?;

    // The T02 authority must actually hold the card. MPL Core would refuse the CPI anyway — with
    // its own `NoApprovals` (26), which says nothing a client can act on.
    require_keys_eq!(
        asset.owner,
        ctx.accounts.t02_authority.key(),
        ByeMachineError::NotPositionOwner
    );

    // The collection comes from `UpdateAuthority::Collection`, which MPL Core makes the
    // collection's own authority sign for at mint. `grouping.collection` is a DAS field with no
    // on-chain existence.
    let collection = declared_core_collection(&asset)?;
    PoolCollection::require_admits(
        &ctx.accounts.pool.key(),
        &collection,
        &ctx.accounts.pool_collection.key(),
        &ctx.accounts.pool_collection,
        STANDARD_CORE,
    )?;

    // The record decision above is already safe — it is derived from what the *asset* declares,
    // never from this account — so this pins the CPI's collection to the same key rather than the
    // admission. MPL Core rejects a mismatch too, with `InvalidCollection` (19); a named code
    // beats depending on the dependency to catch what this program can check itself.
    require_keys_eq!(
        ctx.accounts.collection.key(),
        collection,
        ByeMachineError::CollectionNotAdmitted
    );

    // `Some` unconditionally: the handler has just required the asset to declare this
    // collection, so no arm needs `None` here. The exits resolve the argument from the
    // collateral read instead.
    transfer_core(
        &ctx.accounts.asset,
        Some(&ctx.accounts.collection),
        &ctx.accounts.position_vault,
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        &[],
    )?;

    let position_id = ctx.accounts.pool.position_counter;
    ctx.accounts.pool.position_counter = position_id
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    let pool_key = ctx.accounts.pool.key();
    let t02_identity = ctx.accounts.t02_identity.key();
    let position_key = ctx.accounts.position.key();
    let bump = ctx.bumps.position;
    let vault_bump = ctx.bumps.position_vault;

    let position = &mut ctx.accounts.position;
    position.pool = pool_key;
    position.depositor = t02_identity;
    // `nft_mint` names the escrowed asset. On this family there is no mint — the `AssetV1`
    // account *is* the instrument — and the field is not renamed because it is a shipped layout
    // read by three TS decoders and by `tier-resolve`'s memcmp offsets.
    position.nft_mint = asset_key;
    position.position_id = position_id;
    position.state = PositionState::Pending;
    position.recorded_value = 0;
    position.value_observed_at = 0;
    position.deposit_value = 0;
    position.slot_index = 0;
    position.activated_at = 0;
    position.equal_checkpoint = 0;
    position.accrued = 0;
    position.in_tier = false;
    position.tier_checkpoint = 0;
    position.lock_until = 0;
    position.reject_reason = 0;
    position.standard = STANDARD_CORE;
    position.bump = bump;
    // Stored on all three standards. What is absent here is the token account, not the bump: the
    // exit CPI signs as `PDA(["vault", position])` under `invoke_signed`, and program signing does
    // not require the signing account to exist.
    position.vault_bump = vault_bump;

    let wallet_stats = &mut ctx.accounts.wallet_stats;
    wallet_stats.pool = pool_key;
    wallet_stats.owner = t02_identity;
    wallet_stats.bump = ctx.bumps.wallet_stats;

    // Read back off the position, not re-stated as `STANDARD_CORE`: the event reports the
    // standard the account holds, and a literal would be a second independent claim.
    emit!(DepositPending {
        pool: pool_key,
        slot: Clock::get()?.slot,
        position: position_key,
        depositor: t02_identity,
        nft_mint: asset_key,
        standard: ctx.accounts.position.standard,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct T02DepositCore<'info> {
    #[account(mut)]
    pub t02_authority: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = t02_authority.key() == protocol_config.t02_authority @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// CHECK: the Treasury 02 identity PDA — an address, not an account. It becomes
    /// `Position.depositor` and the owner of the wallet rollup.
    #[account(seeds = [TREASURY_02_SEED, pool.key().as_ref()], bump)]
    pub t02_identity: UncheckedAccount<'info>,

    /// The admission record for the collection the presented asset declares. Its address is
    /// re-derived in the handler from this pool's key and the collection the *asset* names, so
    /// neither another pool's record for this collection nor this pool's record for another
    /// collection is accepted.
    pub pool_collection: Box<Account<'info, PoolCollection>>,

    #[account(
        init,
        payer = t02_authority,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, pool.key().as_ref(), asset.key().as_ref()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: the escrow identity, and **not an account**. Derived here so Anchor pins the address
    /// and yields the `vault_bump` the exit path signs with; it is deliberately never `init`,
    /// because a Core asset needs no token account and allocating one would create rent nobody can
    /// reclaim. Its only on-chain expression is `AssetV1.owner`.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    #[account(
        init_if_needed,
        payer = t02_authority,
        space = 8 + WalletStats::INIT_SPACE,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), t02_identity.key().as_ref()],
        bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    /// CHECK: owner program, discriminator and contents are asserted by `read_core_asset`; the
    /// handler then pins its owner to the T02 authority. Writable because `TransferV1` rewrites
    /// `AssetV1.owner`.
    #[account(mut)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: pinned in the handler to the collection the asset itself declares, which is also the
    /// collection the admission record was derived for.
    pub collection: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, production};

    const SRC: &str = include_str!("t02_deposit_core.rs");

    #[test]
    fn the_t02_authority_is_the_only_signer_and_is_checked_against_config() {
        let prod = production(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(prod.contains(
            "constraint = t02_authority.key() == protocol_config.t02_authority @ ByeMachineError::Unauthorized"
        ));
        assert_eq!(prod.matches("payer = t02_authority").count(), 2);
    }

    #[test]
    fn the_position_and_rollup_belong_to_the_treasury_identity_and_carry_no_value() {
        let prod = production(SRC);
        assert!(prod.contains("position.depositor = t02_identity;"));
        assert!(prod.contains("wallet_stats.owner = t02_identity;"));
        assert!(prod.contains("position.recorded_value = 0;"));
        assert!(prod.contains("position.state = PositionState::Pending;"));
    }

    #[test]
    fn nothing_is_skipped_and_nothing_is_paid() {
        let prod = production(SRC);
        assert!(prod.contains("!ctx.accounts.pool.deposits_paused"));
        assert_eq!(prod.matches("emit!(DepositPending").count(), 1);
        assert_eq!(calls(SRC, "validate_admission_limits"), 0);
        for absent in ["treasury_03", "usdc", "operator"] {
            assert!(
                !accounts_body(SRC).contains(absent),
                "unexpectedly names {absent}"
            );
        }
    }

    #[test]
    fn the_asset_is_held_by_the_authority_and_admitted_before_the_custody_leg() {
        let prod = production(SRC);
        let held = prod.find("ctx.accounts.t02_authority.key()").unwrap();
        let admitted = prod.find("PoolCollection::require_admits(").unwrap();
        let custody = prod.find("transfer_core(").unwrap();
        assert!(held < admitted && admitted < custody);
        assert!(prod.contains("position.standard = STANDARD_CORE;"));
    }
}
