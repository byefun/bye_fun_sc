use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use mpl_core::ID as MPL_CORE_ID;

use crate::common::core_asset::{declared_core_collection, read_core_asset, transfer_core};
use crate::common::errors::ByeMachineError;
use crate::common::events::DepositPending;
use crate::common::seeds::{POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, WALLET_STATS_SEED};
use crate::common::standards::STANDARD_CORE;
use crate::state::{Pool, PoolCollection, Position, PositionState, WalletStats};

/// `deposit`'s MPL-Core twin.
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
pub fn handler(ctx: Context<DepositCore>) -> Result<()> {
    require!(
        !ctx.accounts.pool.deposits_paused,
        ByeMachineError::DepositsPaused
    );

    let asset_key = ctx.accounts.asset.key();
    let asset = read_core_asset(&ctx.accounts.asset)?;

    // The depositor must actually hold the card. MPL Core would refuse the CPI anyway — with its
    // own `NoApprovals` (26), which says nothing a client can act on — and this is the exact
    // analogue of `deposit`'s `depositor_token.owner == depositor.key()` constraint.
    require_keys_eq!(
        asset.owner,
        ctx.accounts.depositor.key(),
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
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        &[],
    )?;

    let position_id = ctx.accounts.pool.position_counter;
    ctx.accounts.pool.position_counter = position_id
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    let pool_key = ctx.accounts.pool.key();
    let depositor = ctx.accounts.depositor.key();
    let position_key = ctx.accounts.position.key();
    let bump = ctx.bumps.position;
    let vault_bump = ctx.bumps.position_vault;

    let position = &mut ctx.accounts.position;
    position.pool = pool_key;
    position.depositor = depositor;
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
    wallet_stats.owner = depositor;
    wallet_stats.bump = ctx.bumps.wallet_stats;

    // Read back off the position, not re-stated as `STANDARD_CORE`: the event reports the
    // standard the account holds, and a literal would be a second independent claim.
    emit!(DepositPending {
        pool: pool_key,
        slot: Clock::get()?.slot,
        position: position_key,
        depositor,
        nft_mint: asset_key,
        standard: ctx.accounts.position.standard,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct DepositCore<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// The admission record for the collection the presented asset declares. Its address is
    /// re-derived in the handler from this pool's key and the collection the *asset* names, so
    /// neither another pool's record for this collection nor this pool's record for another
    /// collection is accepted.
    pub pool_collection: Box<Account<'info, PoolCollection>>,

    #[account(
        init,
        payer = depositor,
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
        payer = depositor,
        space = 8 + WalletStats::INIT_SPACE,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), depositor.key().as_ref()],
        bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    /// CHECK: owner program, discriminator and contents are asserted by `read_core_asset`; the
    /// handler then pins its owner to the depositor. Writable because `TransferV1` rewrites
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
    use crate::common::test_support::{calls, field_writes, flat, production};

    const DEPOSIT_CORE_SRC: &str = include_str!("deposit_core.rs");
    const DEPOSIT_SRC: &str = include_str!("deposit.rs");

    // --- collection admission ----------------------------------------------------------------
    //
    // The predicate lives in `PoolCollection::require_admits`. What stays here is the claim only
    // this file can make: the handler reaches it with the collection the asset declares and
    // with the Core standard.

    /// The call, pinned by exact argument order: three of the four values are `&Pubkey`, so a
    /// transposition compiles (the record's own key passed as `declared_collection`, say).
    #[test]
    fn the_handler_reaches_the_shared_predicate_with_the_declared_collection_and_the_core_standard()
    {
        assert!(flat(DEPOSIT_CORE_SRC).contains(
            "PoolCollection::require_admits( &ctx.accounts.pool.key(), &collection, \
             &ctx.accounts.pool_collection.key(), &ctx.accounts.pool_collection, STANDARD_CORE, )?;"
        ));
        assert_eq!(calls(DEPOSIT_CORE_SRC, "PoolCollection::require_admits"), 1);
    }

    /// And that it does not derive a record address itself; the crate-wide form of this claim is
    /// `state/pool_collection.rs`'s sweep over `src/`.
    #[test]
    fn the_handler_derives_no_record_address_of_its_own() {
        assert_eq!(
            production(DEPOSIT_CORE_SRC)
                .matches("PoolCollection::address")
                .count(),
            0,
            "the record address is derived once, in PoolCollection::require_admits"
        );
    }

    // --- source pins ---------------------------------------------------------------------------

    /// The CPI call site. `core_asset.rs` pins the parameter to builder mapping, but not the
    /// eight positional `&AccountInfo` arguments handed to it from here: `asset`/`collection`
    /// sends MPL Core the wrong pair, and `position_vault`/`depositor` makes the depositor the
    /// new owner, so the card never enters escrow while a `Position` opens over it.
    #[test]
    fn every_transfer_core_argument_is_bound_at_this_call_site() {
        assert!(flat(DEPOSIT_CORE_SRC).contains(
            "transfer_core( &ctx.accounts.asset, Some(&ctx.accounts.collection), \
             &ctx.accounts.position_vault, &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.system_program.to_account_info(), &ctx.accounts.mpl_core_program, \
             &[], )?;"
        ));
        assert_eq!(calls(DEPOSIT_CORE_SRC, "transfer_core"), 1);
    }

    /// The Token Metadata legs must not appear here. A Core asset is not an SPL mint, so a
    /// `transfer_spl` reaching this file would be a custody move against an account that does not
    /// exist — and `transfer_pnft` would need a token record MPL Core never creates.
    #[test]
    fn no_token_metadata_transfer_leg_reaches_the_core_twin() {
        assert_eq!(calls(DEPOSIT_CORE_SRC, "transfer_pnft"), 0);
        assert_eq!(calls(DEPOSIT_CORE_SRC, "transfer_spl"), 0);
        assert_eq!(calls(DEPOSIT_CORE_SRC, "validate_admission"), 0);
    }

    /// The two handler-side identity checks, pinned by source because there is no `Context` to
    /// drive off-chain. They protect the error surface, not custody: deleting either leaves the
    /// deposit refused by MPL Core itself (`NoApprovals`, 26 for the authority;
    /// `InvalidCollection`, 19 for the collection account), but the depositor gets a bare MPL
    /// Core number instead of a code this program can explain. The admission decision rests on
    /// `require_record_admits`, which reads only what the asset declares.
    #[test]
    fn the_two_handler_side_identity_checks_are_present() {
        let flat_src = flat(DEPOSIT_CORE_SRC);
        assert!(flat_src.contains(
            "require_keys_eq!( asset.owner, ctx.accounts.depositor.key(), \
             ByeMachineError::NotPositionOwner );"
        ));
        assert!(flat_src.contains(
            "require_keys_eq!( ctx.accounts.collection.key(), collection, \
             ByeMachineError::CollectionNotAdmitted );"
        ));
        // Ordering: both must precede the CPI, or they are checks the transfer already made.
        let prod = production(DEPOSIT_CORE_SRC);
        assert!(prod.find("NotPositionOwner").unwrap() < prod.find("transfer_core(").unwrap());
        assert!(
            prod.find("ctx.accounts.collection.key()").unwrap()
                < prod.find("transfer_core(").unwrap()
        );
    }

    /// A deliberate absence: MPL Core has no edition mechanism, so the print attack `deposit`'s
    /// guard blocks has no Core analogue, and a guard added here for symmetry would reject every
    /// Core deposit.
    #[test]
    fn there_is_no_master_edition_guard_and_that_is_the_specification() {
        let prod = production(DEPOSIT_CORE_SRC);
        assert_eq!(prod.matches("master_edition").count(), 0);
        assert_eq!(prod.matches("check_master_edition").count(), 0);
        assert!(
            production(DEPOSIT_SRC).contains("master_edition"),
            "the Token Metadata twin must still carry the guard this one omits, or the omission \
             above is measuring nothing"
        );
    }

    /// No vault token account: `position_vault` is an address, not an allocation, since `init`
    /// would create a token account for a mint that does not exist, and rent nobody can
    /// reclaim. Two claims, because "not initialised" and "not a token account" are different
    /// mistakes.
    #[test]
    fn the_vault_is_an_address_and_never_an_allocation() {
        let body = crate::common::test_support::accounts_body(DEPOSIT_CORE_SRC);
        assert!(flat(DEPOSIT_CORE_SRC).contains(
            "#[account( seeds = [POSITION_VAULT_SEED, position.key().as_ref()], bump )] \
             pub position_vault: UncheckedAccount<'info>,"
        ));
        assert_eq!(
            body.matches("token::").count(),
            0,
            "a token-account constraint here would allocate the vault the Core path exists \
             without"
        );
        // Anchored on the attribute's own comma; the doc comments say the vault is never `init`.
        assert_eq!(
            body.matches("init,").count(),
            1,
            "exactly one unconditional allocation — the position; the vault is not one"
        );
        assert_eq!(
            body.matches("init_if_needed,").count(),
            1,
            "wallet_stats, as in the twin"
        );
    }

    /// The standard is the branch the caller reached, not a field this handler read: on the Core
    /// family the owner program *is* the standard. Pinned as a literal because
    /// `position.standard = standard` — the Token Metadata form — would compile here against a
    /// local of any provenance.
    #[test]
    fn the_stored_standard_is_the_core_discriminant_and_is_never_resolved_from_data() {
        assert!(flat(DEPOSIT_CORE_SRC).contains("position.standard = STANDARD_CORE;"));
        assert_eq!(
            crate::common::test_support::writes(DEPOSIT_CORE_SRC, "position.standard"),
            1
        );
        // The field access, not the bare word — the handler's own doc comment names
        // `token_standard` in order to say this family has none.
        assert_eq!(
            production(DEPOSIT_CORE_SRC)
                .matches(".token_standard")
                .count(),
            0
        );
        assert_eq!(calls(DEPOSIT_CORE_SRC, "resolve_standard"), 0);
    }

    /// Every field the Token Metadata twin initialises, this one initialises too: a `Position`
    /// is `init`-allocated zeroed, so a forgotten field is a plausible zero some later handler
    /// reads as real. Derived from `deposit.rs` so a field added later cannot be wired into one
    /// twin only.
    #[test]
    fn the_core_twin_initialises_exactly_the_fields_the_token_metadata_twin_does() {
        assert_eq!(
            field_writes(DEPOSIT_CORE_SRC, "position"),
            field_writes(DEPOSIT_SRC, "position")
        );
        assert_eq!(
            field_writes(DEPOSIT_CORE_SRC, "wallet_stats"),
            field_writes(DEPOSIT_SRC, "wallet_stats")
        );
        assert!(
            field_writes(DEPOSIT_CORE_SRC, "position").len() >= 18,
            "the comparison above is only worth anything if the list is non-trivial"
        );
    }

    /// The pool counter and the pause flag are the two pool-level effects, identical to the
    /// principal's. A twin that skipped the counter would hand two positions the same id.
    #[test]
    fn the_pool_effects_match_the_token_metadata_twin() {
        assert_eq!(
            field_writes(DEPOSIT_CORE_SRC, "ctx.accounts.pool"),
            field_writes(DEPOSIT_SRC, "ctx.accounts.pool")
        );
        assert!(production(DEPOSIT_CORE_SRC).contains("deposits_paused"));
    }
}
