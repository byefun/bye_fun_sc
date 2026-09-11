use anchor_lang::prelude::*;
use mpl_core::ID as MPL_CORE_ID;

use crate::common::core_asset::{release_core_from_vault, require_collateral_settleable};
use crate::common::errors::ByeMachineError;
use crate::common::events::NftClaimed;
use crate::common::seeds::{POSITION_SEED, POSITION_VAULT_SEED};
use crate::common::standards::STANDARD_CORE;
use crate::state::{Position, PositionState};

/// `claim_nft`'s MPL-Core twin.
///
/// **The one exit that must survive a seizure, which is why its two source states are not
/// symmetrical decoration.** `ClosedBelowFloor` keeps its account and its vault until the claim,
/// so a card seized *after* closure must still be claimable;
/// `Seized` is `close_seized`'s own terminal, and `close_seized` closes no account on any source
/// state, so the claim is still owed afterwards. A twin that admitted only the first
/// would refuse the very call its own sibling's `CollateralAbsent` message sends the depositor to
/// make.
///
/// **Never gated, on any standard.** No pause flag, no `open_batches`, no `lock_until` —
/// nothing may gate an exit here, and that promise is strongest exactly here, where the pool
/// has already taken everything it is going to take from this position.
pub fn handler(ctx: Context<ClaimNftCore>) -> Result<()> {
    // Before the CPI, so a depositor whose card is gone or frozen is told which of the two it is
    // — and told to call `close_seized` — instead of reading an MPL Core error number. The
    // collection is passed because the freeze can live in *its* plugin registry rather than the
    // asset's, and the gate pins it to the collection the asset declares — which is why this
    // handler no longer re-states that pin around a second read of the same account.
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
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        &position_key,
        ctx.accounts.position.vault_bump,
    )?;

    emit!(NftClaimed {
        pool,
        slot: Clock::get()?.slot,
        position: position_key,
        depositor: ctx.accounts.depositor.key(),
        nft_mint,
    });

    Ok(())
}

/// Depositor-signed: the custody release of a position that is
/// already closed. No `Pool`, no weight, no tier and no counter surface — every one of those
/// moved when the position closed, whether `record_value` closed it below the floor or
/// `close_seized` seized it. This instruction returns the card and the rent and nothing else.
#[derive(Accounts)]
pub struct ClaimNftCore<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // The MPL Core family only, on the accounts struct so it runs before any
        // CPI — the placement every twin and principal shares.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction,
        // Deactivation keeps the account whatever closed it, so the claim is callable from
        // either terminal. Unlike `claim_nft`'s widening, **both arms are live here** — `Seized`
        // is written only by `close_seized`, which asserts `standard == 2`, so every seized
        // position is one this instruction serves and no other can.
        constraint = matches!(
            position.state,
            PositionState::ClosedBelowFloor | PositionState::Seized
        ) @ ByeMachineError::InvalidPositionState
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
    /// make.** `read_collateral` reads whatever account it is handed, so without this address a
    /// depositor could present a settleable asset of their own and claim against a position
    /// whose card is elsewhere. Writable because `TransferV1` rewrites `AssetV1.owner`.
    #[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: pinned in the handler to the collection the asset itself declares. Required because
    /// MPL Core needs the collection account present to run its plugins over the transfer.
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
    use crate::state::PositionState;

    const CLAIM_NFT_CORE_SRC: &str = include_str!("claim_nft_core.rs");
    const CLAIM_NFT_SRC: &str = include_str!("claim_nft.rs");

    /// The gate, against this position's vault, before the release. `core_asset.rs` owns the
    /// classification and both numeric codes; this file owns only the reach and the ordering.
    #[test]
    fn the_collateral_gate_runs_against_this_positions_vault_before_the_release() {
        assert!(flat(CLAIM_NFT_CORE_SRC).contains(
            "require_collateral_settleable( &ctx.accounts.asset, &ctx.accounts.collection, \
             &ctx.accounts.position_vault.key(), )?;"
        ));
        assert_eq!(
            calls(CLAIM_NFT_CORE_SRC, "require_collateral_settleable"),
            1
        );
        let prod = production(CLAIM_NFT_CORE_SRC);
        assert!(
            prod.find("require_collateral_settleable(").unwrap()
                < prod.find("release_core_from_vault(").unwrap()
        );
    }

    /// **This is the exit that must work on a card the pool has already lost, so the gate here
    /// is not a formality.** `claim_nft_core` runs on a closed position whose vault survives
    /// until the claim, and a seizure at that point must still leave the card claimable: the
    /// depositor gets `CollateralAbsent` or `CollateralFrozen` naming `close_seized`, and
    /// `close_seized` admits the `ClosedBelowFloor` source precisely so that it does not refuse
    /// the call its sibling's error message sent.
    #[test]
    fn the_release_arguments_are_pinned_in_order_and_the_depositor_pays() {
        assert!(flat(CLAIM_NFT_CORE_SRC).contains(
            "release_core_from_vault( &ctx.accounts.asset, collection, \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.position_vault.to_account_info(), \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.system_program.to_account_info(), &ctx.accounts.mpl_core_program, \
             &position_key, ctx.accounts.position.vault_bump, )?;"
        ));
        assert_eq!(calls(CLAIM_NFT_CORE_SRC, "release_core_from_vault"), 1);
    }

    /// **Both source states, and unlike `claim_nft`'s widening both of these are live.**
    /// `PositionState::Seized` is written by `close_seized` alone, which asserts
    /// `standard == 2` — so every seized position carries the standard `claim_nft` refuses and
    /// this twin is the only instruction that can ever serve one. The count over
    /// `PositionState`'s own variants is what makes "two of five" a claim rather than a spelling.
    #[test]
    fn both_terminal_source_states_are_admitted_and_no_third() {
        let accounts = accounts_body(CLAIM_NFT_CORE_SRC);
        assert!(accounts.contains(
            "constraint = matches!(\n            position.state,\n            PositionState::ClosedBelowFloor | PositionState::Seized\n        ) @ ByeMachineError::InvalidPositionState"
        ));
        let admitted = [PositionState::ClosedBelowFloor, PositionState::Seized];
        let all = [
            ("Pending", PositionState::Pending),
            ("Active", PositionState::Active),
            ("ClosedBelowFloor", PositionState::ClosedBelowFloor),
            ("Rejected", PositionState::Rejected),
            ("Seized", PositionState::Seized),
        ];
        assert_eq!(
            all.len(),
            5,
            "a sixth PositionState variant owes this file a decision"
        );
        for (name, state) in all {
            let named = accounts.contains(&format!("PositionState::{name}"));
            assert_eq!(
                named,
                admitted.contains(&state),
                "PositionState::{name} is on the wrong side of this instruction's state guard"
            );
        }
    }

    /// The two remaining accounts-struct guards. Never pause-gated and never roll-gated:
    /// nothing may gate an exit on any standard, and the absence is pinned by count — a
    /// fourth constraint here is a gate somebody added to a claim.
    #[test]
    fn the_standard_and_the_depositor_are_asserted_and_nothing_else_is() {
        let accounts = accounts_body(CLAIM_NFT_CORE_SRC);
        assert!(accounts.contains(
            "constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
        assert!(accounts.contains(
            "constraint = position.standard == STANDARD_CORE\n            @ ByeMachineError::WrongStandardForInstruction"
        ));
        assert_eq!(accounts.matches("constraint = ").count(), 3);
        let prod = production(CLAIM_NFT_CORE_SRC);
        // Counted over the code alone: this file's own doc comment says it reads no
        // `open_batches` and no `lock_until`, and a raw-word count reads that sentence as the
        // gate it denies.
        let code = without_doc_comments(prod);
        for gate in [
            "deposits_paused",
            "claims_paused",
            "open_batches",
            "assert_weight_open",
            "lock_until",
        ] {
            assert_eq!(
                code.matches(gate).count(),
                0,
                "{gate} gates no claim, on any standard"
            );
        }
    }

    /// **This instruction touches no pool-level account at all**, and the empty list is the
    /// claim: every counter, the weight, the tier membership and the fee accrual moved when the
    /// position closed, so a `Pool` or `WeightIndex` slot appearing here would be a second
    /// removal of state already removed — `close_seized`'s double-decrement hazard reached
    /// through the claim instead.
    #[test]
    fn no_pool_weight_or_tier_surface_reaches_the_claim() {
        let accounts = accounts_body(CLAIM_NFT_CORE_SRC);
        for absent in [
            "pub pool:",
            "weight_index",
            "top_tier",
            "wallet_stats",
            "principal_vault",
        ] {
            assert_eq!(
                accounts.matches(absent).count(),
                0,
                "{absent} is not the claim's"
            );
        }
        let principal = accounts_body(CLAIM_NFT_SRC);
        for absent in ["pub pool:", "weight_index", "top_tier"] {
            assert_eq!(
                principal.matches(absent).count(),
                0,
                "the Token Metadata twin carries {absent} — the comparison above is stale"
            );
        }
    }

    /// No Token Metadata leg and no vault close, with the principal carrying both so the
    /// absences are a comparison rather than a spelling.
    #[test]
    fn no_token_metadata_leg_and_no_vault_close_reach_the_core_twin() {
        for callee in [
            "release_from_vault",
            "transfer_pnft",
            "transfer_spl",
            "close_account",
        ] {
            assert_eq!(
                calls(CLAIM_NFT_CORE_SRC, callee),
                0,
                "{callee} reaches the Core twin"
            );
        }
        assert!(
            calls(CLAIM_NFT_SRC, "close_account") == 1
                && calls(CLAIM_NFT_SRC, "release_from_vault") == 1,
            "the Token Metadata twin must still carry both, or the absences above measure nothing"
        );
    }

    /// The asset pinned to the position's own card — the binding `read_collateral` cannot make
    /// for itself, and the one that stops a depositor presenting a settleable asset of their own
    /// to claim against a position whose card has been taken.
    #[test]
    fn the_asset_is_pinned_to_the_positions_own_card() {
        assert!(accounts_body(CLAIM_NFT_CORE_SRC).contains(
            "#[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]\n    pub asset: UncheckedAccount<'info>,"
        ));
    }

    /// The vault is an address signed under the bump the deposit stored, never an allocation.
    #[test]
    fn the_vault_is_an_address_signed_under_the_stored_bump() {
        let accounts = accounts_body(CLAIM_NFT_CORE_SRC);
        assert!(accounts.contains(
            "#[account(\n        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],\n        bump = position.vault_bump\n    )]\n    pub position_vault: UncheckedAccount<'info>,"
        ));
        let attributes = without_doc_comments(accounts);
        for form in ["init", "init_if_needed", "zero", "token::"] {
            assert_eq!(
                attributes.matches(form).count(),
                0,
                "{form} allocates nothing here"
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
            calls(CLAIM_NFT_CORE_SRC, "declared_core_collection"),
            0,
            "the declaration is read inside the gate now; a second read here would be a second \
             answer to which collection this asset belongs to"
        );
        let prod = production(CLAIM_NFT_CORE_SRC);
        assert_eq!(
            prod.matches("&ctx.accounts.collection").count(),
            1,
            "the account is named once, where it is handed to the gate; the release takes what \
             the gate returned"
        );
        assert!(flat(CLAIM_NFT_CORE_SRC).contains(
            "let collection = require_collateral_settleable( &ctx.accounts.asset, \
                           &ctx.accounts.collection, &ctx.accounts.position_vault.key(), )?;"
        ));
        assert!(
            prod.find("require_collateral_settleable(").unwrap()
                < prod.find("release_core_from_vault(").unwrap(),
            "the checked read must precede the release that trusts it"
        );
    }

    /// `NftClaimed`, field by field and identical to its principal's payload.
    #[test]
    fn the_event_payload_is_pinned_field_by_field() {
        assert!(flat(CLAIM_NFT_CORE_SRC).contains(
            "emit!(NftClaimed { pool, slot: Clock::get()?.slot, position: position_key, \
             depositor: ctx.accounts.depositor.key(), nft_mint, });"
        ));
        assert_eq!(production(CLAIM_NFT_CORE_SRC).matches("emit!(").count(), 1);
    }

    #[test]
    fn the_position_closes_to_the_depositor_and_to_nobody_else() {
        let accounts = accounts_body(CLAIM_NFT_CORE_SRC);
        assert!(accounts.contains("close = depositor,"));
        assert_eq!(accounts.matches("close = ").count(), 1);
    }
}
