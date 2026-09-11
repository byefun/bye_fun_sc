use anchor_lang::prelude::*;
use mpl_core::ID as MPL_CORE_ID;

use crate::common::core_asset::{release_core_from_vault, require_collateral_settleable};
use crate::common::errors::ByeMachineError;
use crate::common::events::NftReturned;
use crate::common::seeds::{POSITION_SEED, POSITION_VAULT_SEED};
use crate::common::standards::STANDARD_CORE;
use crate::state::{Position, PositionState};

/// `return_rejected`'s MPL-Core twin.
///
/// **Three absences, all of them the account model rather than a relaxed guard.** There is no
/// vault token account to close: `PDA(["vault", position])` holds the asset directly as
/// `AssetV1.owner`, so the `close_account` its principal needs — and the re-deposit hazard that
/// call exists for — has nothing to act on here. There is no `nft_mint`, because a Core asset is
/// a single account with no mint behind it. And there is no destination token account: MPL Core
/// writes the new owner into the asset, so nothing has to be created for the card to land in.
///
/// **What it gains instead is the collateral gate**, which no Token Metadata exit carries. A Core
/// asset can be burned, transferred out from under the vault, or frozen by a third party holding
/// `permanent_freeze_delegate`, and this instruction moves an asset out of
/// the vault — so the opaque-revert argument applies to it identically to `withdraw_core`'s.
/// The gate runs **before the CPI** so a caller is told `CollateralAbsent`
/// (6307) or `CollateralFrozen` (6309) naming `close_seized` as the remedy, rather than being
/// handed an MPL Core error number from inside a reverted transaction.
pub fn handler(ctx: Context<ReturnRejectedCore>) -> Result<()> {
    // Before anything, and this handler makes exactly one CPI: an absent or frozen card fails
    // here with a named code instead of inside MPL Core's `TransferV1`.
    //
    // The collection is one of the gate's arguments because a `permanent_freeze_delegate` on the
    // collection freezes every asset in it, and because the gate pins the supplied account to the
    // collection the asset itself declares — a mismatch is `CollectionNotAdmitted` (6100) instead
    // of MPL Core's `InvalidCollection` (19), the same trade `deposit_core` makes, and this
    // handler no longer re-states it around a second read of the same account.
    let collection = require_collateral_settleable(
        &ctx.accounts.asset,
        &ctx.accounts.collection,
        &ctx.accounts.position_vault.key(),
    )?;

    let pool = ctx.accounts.position.pool;
    let nft_mint = ctx.accounts.position.nft_mint;
    let position_key = ctx.accounts.position.key();

    release_core_from_vault(
        &ctx.accounts.asset,
        collection,
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.position_vault.to_account_info(),
        &ctx.accounts.payer.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        &position_key,
        ctx.accounts.position.vault_bump,
    )?;

    emit!(NftReturned {
        pool,
        slot: Clock::get()?.slot,
        position: position_key,
        depositor: ctx.accounts.depositor.key(),
        nft_mint,
    });

    Ok(())
}

/// Permissionless, exactly as its principal: `payer` funds the transaction and any account MPL
/// Core creates for it, and the card plus the position's rent go to the position's own recorded
/// depositor. A stranger can return a rejected card and cannot redirect it.
#[derive(Accounts)]
pub struct ReturnRejectedCore<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: pinned to the position's recorded depositor; receives the asset and the rent.
    #[account(mut)]
    pub depositor: UncheckedAccount<'info>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // This instruction serves the MPL Core family only, and the assert is on the accounts
        // struct so it runs before any CPI — the same placement all three Token Metadata exits
        // use for their own bound, so the six do not drift into two shapes.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction,
        constraint = position.state == PositionState::Rejected @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: the escrow identity, and **not an account** — its only on-chain expression is
    /// `AssetV1.owner`. Derived here so Anchor pins the address the collateral gate reads against
    /// and so the seeds the release signs under are this position's own. Never `init` and never
    /// `mut`: there is nothing to allocate and nothing to write.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    /// CHECK: **the position's own card, and this pin is the one the collateral gate cannot
    /// make.** `read_collateral` reads whatever account it is handed, so without this address
    /// the caller chooses which asset is tested — and a permissionless instruction would then
    /// accept any settleable Core asset in place of a burned one. Writable because `TransferV1`
    /// rewrites `AssetV1.owner`.
    #[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: the collection the asset itself declares, checked by the collateral gate rather
    /// than here — `Position` stores no collection, so the only truth to compare against is the
    /// asset's own `update_authority`, which account validation cannot read. The slot is always
    /// present because MPL Core needs the account to run the collection's plugins over the
    /// transfer; **what reaches the CPI is what the gate returns**, which is `None` for an asset
    /// that declares no collection — a state `mpl-core 0.11.1` gives no way to reach, and one the
    /// exits handle rather than assume away. On such an asset this account would be unread.
    pub collection: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{
        accounts_body, calls, flat, production, without_doc_comments,
    };

    const RETURN_REJECTED_CORE_SRC: &str = include_str!("return_rejected_core.rs");
    const RETURN_REJECTED_SRC: &str = include_str!("return_rejected.rs");

    /// **The gate, and that nothing in this handler runs before it.** `require_collateral_settleable`
    /// is `core_asset.rs`'s, tested there over all four classifications and both codes by exact
    /// numeric value; what only this file can claim is that the handler reaches it, with **this
    /// position's** vault, before the CPI that would otherwise fail inside MPL Core with the
    /// phantom leaf still in place. The crate-wide form — that all three Core exits do — is
    /// `instructions/mod.rs`'s `every_core_exit_gates_its_collateral_before_any_cpi`.
    #[test]
    fn the_collateral_gate_runs_against_this_positions_vault_before_the_release() {
        assert!(flat(RETURN_REJECTED_CORE_SRC).contains(
            "require_collateral_settleable( &ctx.accounts.asset, &ctx.accounts.collection, \
             &ctx.accounts.position_vault.key(), )?;"
        ));
        assert_eq!(
            calls(RETURN_REJECTED_CORE_SRC, "require_collateral_settleable"),
            1
        );
        let prod = production(RETURN_REJECTED_CORE_SRC);
        assert!(
            prod.find("require_collateral_settleable(").unwrap()
                < prod.find("release_core_from_vault(").unwrap()
        );
    }

    /// The release, pinned by exact argument order. `core_asset.rs` pins the parameter → builder
    /// mapping and the vault-seed construction; nothing there can see the nine positional
    /// arguments handed to it from here, and the interesting transpositions all compile.
    /// `depositor`/`position_vault` are adjacent and both `AccountInfo`: swapped, the vault
    /// becomes the new owner and the depositor signs — a release that moves the card nowhere and
    /// closes the position anyway. `payer` is the third `AccountInfo` in a row, and here it is
    /// deliberately **not** the depositor: this instruction is permissionless.
    #[test]
    fn every_release_argument_is_bound_at_this_call_site() {
        assert!(flat(RETURN_REJECTED_CORE_SRC).contains(
            "release_core_from_vault( &ctx.accounts.asset, collection, \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.position_vault.to_account_info(), \
             &ctx.accounts.payer.to_account_info(), \
             &ctx.accounts.system_program.to_account_info(), &ctx.accounts.mpl_core_program, \
             &position_key, ctx.accounts.position.vault_bump, )?;"
        ));
        assert_eq!(
            calls(RETURN_REJECTED_CORE_SRC, "release_core_from_vault"),
            1
        );
    }

    /// **The binding the gate cannot make, and the one this instruction needs most.**
    /// `read_collateral` reads whatever account it is handed, so the address pin is what ties
    /// the tested asset to the position — and this handler is *permissionless*, so without it any
    /// caller could present a settleable asset of their own and release a position whose card is
    /// elsewhere. Pinned as the whole attribute, because `address = position.nft_mint` without
    /// `mut` would fail the CPI and `mut` without the address would pass the wrong asset.
    #[test]
    fn the_asset_is_pinned_to_the_positions_own_card() {
        assert!(accounts_body(RETURN_REJECTED_CORE_SRC).contains(
            "#[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]\n    pub asset: UncheckedAccount<'info>,"
        ));
    }

    /// The three accounts-struct guards, each by its exact code, and all three ahead of every
    /// CPI by construction — Anchor runs the constraints before the handler body.
    #[test]
    fn the_standard_the_state_and_the_depositor_are_asserted_on_the_accounts_struct() {
        let accounts = accounts_body(RETURN_REJECTED_CORE_SRC);
        for constraint in [
            "constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner",
            "constraint = position.standard == STANDARD_CORE\n            @ ByeMachineError::WrongStandardForInstruction",
            "constraint = position.state == PositionState::Rejected @ ByeMachineError::InvalidPositionState",
        ] {
            assert!(accounts.contains(constraint), "missing: {constraint}");
        }
        assert_eq!(accounts.matches("constraint = ").count(), 3);
    }

    /// **No Token Metadata leg, and no vault close.** A Core asset is not an SPL mint, so
    /// `transfer_spl` here would move a token account that does not exist and `release_from_vault`
    /// would branch on a standard this instruction has already refused. `close_account` is the
    /// sharper absence: its principal must close the emptied vault or a re-deposit derives the
    /// same PDA and is permanently unsatisfiable, and here there is no account to close — so the
    /// hazard is absent rather than the guard.
    #[test]
    fn no_token_metadata_leg_and_no_vault_close_reach_the_core_twin() {
        for callee in [
            "release_from_vault",
            "transfer_pnft",
            "transfer_spl",
            "close_account",
        ] {
            assert_eq!(
                calls(RETURN_REJECTED_CORE_SRC, callee),
                0,
                "{callee} reaches the Core twin"
            );
        }
        assert!(
            calls(RETURN_REJECTED_SRC, "close_account") == 1
                && calls(RETURN_REJECTED_SRC, "release_from_vault") == 1,
            "the Token Metadata twin must still carry both, or the absences above measure nothing"
        );
    }

    /// The five Token Metadata accounts this family has no analogue for, absent by name — and
    /// the principal carrying them, so the absence is a comparison rather than a spelling.
    #[test]
    fn the_token_metadata_account_surface_is_absent_and_the_principal_still_has_it() {
        let accounts = accounts_body(RETURN_REJECTED_CORE_SRC);
        for absent in [
            "nft_mint:",
            "depositor_token",
            "metadata",
            "master_edition",
            "token_record",
            "authorization_rules",
            "sysvar_instructions",
            "token_program",
        ] {
            assert_eq!(
                accounts.matches(absent).count(),
                0,
                "{absent} has no MPL Core analogue and must not appear"
            );
        }
        let principal = accounts_body(RETURN_REJECTED_SRC);
        for present in ["nft_mint:", "master_edition", "token_record"] {
            assert!(
                principal.contains(present),
                "the Token Metadata twin must still declare {present}"
            );
        }
    }

    /// The vault is an address, not an allocation — the exit direction of `deposit_core`'s claim.
    /// `bump = position.vault_bump` rather than a bare `bump`: the exit must sign under the bump
    /// the deposit stored, and a re-derived canonical bump is the same value by luck rather than
    /// by record.
    #[test]
    fn the_vault_is_an_address_signed_under_the_stored_bump() {
        let accounts = accounts_body(RETURN_REJECTED_CORE_SRC);
        assert!(accounts.contains(
            "#[account(\n        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],\n        bump = position.vault_bump\n    )]\n    pub position_vault: UncheckedAccount<'info>,"
        ));
        // Counted over the attributes alone: this struct's doc comment says the vault is
        // never `init`, and a raw-word count reads that sentence as an allocation. Stripping
        // the prose closes that hazard rather than re-anchoring the pin on syntax the prose
        // merely happens not to use.
        let attributes = without_doc_comments(accounts);
        for form in ["init", "init_if_needed", "zero", "token::"] {
            assert_eq!(
                attributes.matches(form).count(),
                0,
                "an allocation here would create rent nobody can reclaim"
            );
        }
    }

    /// **The collection is pinned to what the *asset* declares, and the pin is the gate's, not
    /// this handler's — which is also how the release gets the right argument.**
    /// `require_collateral_settleable` requires the supplied account to be the collection the
    /// asset itself names before it walks that collection's plugin registry, and **returns the
    /// argument `TransferV1` must be given**: the account on an asset that still belongs to a
    /// collection, and `None` on one its collection authority has removed from it, where MPL Core
    /// refuses a collection account the asset does not name.
    ///
    /// That return value is what this test pins, and it is stronger than the text it replaced: a
    /// handler that passed `&ctx.accounts.collection` to the release instead would compile only
    /// by ignoring the gate's answer, so the account the CPI uses is the account the gate checked
    /// by construction rather than by a matching pair of source lines.
    #[test]
    fn the_collection_is_pinned_to_the_one_the_asset_declares() {
        assert_eq!(
            calls(RETURN_REJECTED_CORE_SRC, "declared_core_collection"),
            0,
            "the declaration is read inside the gate now; a second read here would be a second \
             answer to which collection this asset belongs to"
        );
        let prod = production(RETURN_REJECTED_CORE_SRC);
        assert_eq!(
            prod.matches("&ctx.accounts.collection").count(),
            1,
            "the account is named once, where it is handed to the gate; the release takes what \
             the gate returned"
        );
        assert!(flat(RETURN_REJECTED_CORE_SRC).contains(
            "let collection = require_collateral_settleable( &ctx.accounts.asset, \
                           &ctx.accounts.collection, &ctx.accounts.position_vault.key(), )?;"
        ));
        assert!(
            prod.find("require_collateral_settleable(").unwrap()
                < prod.find("release_core_from_vault(").unwrap(),
            "the checked read must precede the release that trusts it"
        );
    }

    /// `NftReturned`, field by field, and identical to its principal's payload — every twin
    /// carries its principal's event row unchanged, so a field sourced differently here is a
    /// twin whose audit trail cannot be read beside the family it belongs to.
    #[test]
    fn the_event_payload_is_pinned_field_by_field() {
        assert!(flat(RETURN_REJECTED_CORE_SRC).contains(
            "emit!(NftReturned { pool, slot: Clock::get()?.slot, position: position_key, \
             depositor: ctx.accounts.depositor.key(), nft_mint, });"
        ));
        assert_eq!(
            production(RETURN_REJECTED_CORE_SRC)
                .matches("emit!(")
                .count(),
            1
        );
    }

    /// The position closes to the depositor and to nobody else — the signer here is `payer`, and
    /// `close = payer` would pay a stranger the depositor's rent on a permissionless call.
    #[test]
    fn the_position_closes_to_the_depositor_and_never_to_the_payer() {
        let accounts = accounts_body(RETURN_REJECTED_CORE_SRC);
        assert!(accounts.contains("close = depositor,"));
        assert_eq!(accounts.matches("close = ").count(), 1);
    }
}
