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
/// **`newOwner` is an account that does not exist, and that premise is measured rather than
/// assumed.** Against a genuine `AssetV1` on localnet,
/// MPL Core `TransferV1` accepts a `getAccountInfo → null` PDA as `[4] newOwner`, writes it into
/// `AssetV1.owner`, and does **not** allocate it — so the vault stays accountless, no rent is paid
/// for it, and nobody can close it. See `tests/integration/core-accountless-newowner.test.ts`.
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

    // `Some` unconditionally, and this is the only call site where that is right: the handler
    // has just required the asset to declare this collection, so there is no arm on which the
    // intake transfer must be given `None`. The exits resolve the argument from the collateral
    // read instead, so an asset on either of `UpdateAuthority`'s other two arms would still
    // exit — a case `mpl-core 0.11.1` gives no way to reach, and the exits handle anyway.
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

    emit!(DepositPending {
        pool: pool_key,
        slot: Clock::get()?.slot,
        position: position_key,
        depositor,
        nft_mint: asset_key,
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
    ///
    /// The sentence above names `vault_bump` in full on purpose: the crate-wide PDA-declaration
    /// pin reads this struct's raw text, doc comments included, and counts the bare suffix.
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

    // --- collection admission is no longer this file's to assert ------------------------------
    //
    // The predicate lives in `PoolCollection::require_admits` and its cases moved with it — one
    // home for one guard. What stays here is the claim only this file can make: that
    // the handler **reaches** the predicate, with the collection the asset declares and with the
    // Core standard. The behavioural half is `tests/integration/core-standard-deposit.test.ts`,
    // which drives the same two rejections through the real handler on localnet.

    /// The call, pinned by its exact argument order. Every one of the four values is derivable
    /// from `ctx` and three of them are `&Pubkey`, so a transposition compiles: passing the
    /// record's own key as `declared_collection` reproduces exactly the fault the extracted
    /// method exists to prevent, from the outside.
    #[test]
    fn the_handler_reaches_the_shared_predicate_with_the_declared_collection_and_the_core_standard()
    {
        assert!(flat(DEPOSIT_CORE_SRC).contains(
            "PoolCollection::require_admits( &ctx.accounts.pool.key(), &collection, \
             &ctx.accounts.pool_collection.key(), &ctx.accounts.pool_collection, STANDARD_CORE, )?;"
        ));
        assert_eq!(calls(DEPOSIT_CORE_SRC, "PoolCollection::require_admits"), 1);
    }

    /// And that it does not derive a record address itself. The crate-wide form of this claim is
    /// `state/pool_collection.rs`'s sweep over `src/`; this is the local one, so a reintroduced
    /// copy reds in the file that grew it as well as in the sweep.
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

    /// The CPI call site. `core_asset.rs` pins the parameter → builder mapping; nothing there can
    /// see the eight positional `&AccountInfo` arguments handed to it from here, and the
    /// interesting transpositions all compile: `asset`/`collection` sends MPL Core the wrong pair,
    /// and `position_vault`/`depositor` — adjacent, both `AccountInfo` — makes the **depositor**
    /// the new owner, so the card never enters escrow while a `Position` opens over it — the
    /// exact same fault an argument-order slip left unpinned at `deposit`'s own 16-argument
    /// call site.
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

    /// **The two handler-side identity checks, pinned by source because nothing else here can
    /// see them.** Both were injected away and both left a green suite: there is no `Context` to
    /// drive off-chain, and a handler with no on-chain caller wired up yet has no other way to
    /// be exercised.
    ///
    /// Stated at their real weight rather than as custody guards, because they are not.
    /// Deleting either leaves the deposit **refused** — MPL Core checks the transfer authority
    /// against `AssetV1.owner` itself (`NoApprovals`, 26) and the collection account against
    /// `update_authority` (`InvalidCollection`, 19). What is lost is the reason: a depositor who
    /// does not hold the card, or a caller who named the wrong collection account, would get a
    /// bare MPL Core number instead of a code this program's own surfaces can explain. The
    /// admission decision does not rest on either — it rests on `require_record_admits`, which
    /// reads only what the asset declares — so this pin protects the error surface, not custody.
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

    /// **A deliberate absence, pinned so it stays deliberate.** There is no master
    /// edition on MPL Core and no edition mechanism at all, so the print attack `deposit`'s guard
    /// blocks has no Core analogue — and a guard added here in the name of symmetry would reject
    /// every Core deposit. A reader comparing the twins will notice the missing check; this is
    /// what tells them it is missing on purpose.
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

    /// **No vault token account, which is the twin's whole cost advantage.** `position_vault` is
    /// an address, not an allocation: `init` would create a token account for a mint that does not
    /// exist, and rent nobody can reclaim. Two claims, because "it is not initialised" and "it is
    /// not a token account" are different mistakes.
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
        // Anchored on the attribute's own comma, not the bare word: this struct's doc comments
        // say the vault is never `init`, and a raw-word count reads that sentence as a third
        // allocation.
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

    /// **Every field the Token Metadata twin initialises, this one initialises too.** A `Position`
    /// is `init`-allocated zeroed, so a field the Core path forgets is not obviously wrong — it is
    /// a plausible zero that some later handler reads as real. Derived from `deposit.rs` rather
    /// than listed, so a field added to `Position` in a later phase cannot be wired into one twin
    /// and missed in the other.
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
