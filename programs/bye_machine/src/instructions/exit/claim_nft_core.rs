use anchor_lang::error::ErrorCode;
use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};
use mpl_core::ID as MPL_CORE_ID;

use crate::common::core_asset::{release_core_from_vault, require_collateral_settleable};
use crate::common::errors::ByeMachineError;
use crate::common::events::NftClaimed;
use crate::common::seeds::{POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PRINCIPAL_VAULT_SEED};
use crate::common::standards::STANDARD_CORE;
use crate::instructions::exit::claim_fee::pay_closed_accrued;
use crate::state::{Pool, Position, PositionState};

/// `claim_nft`'s MPL-Core twin.
///
/// **The one exit that must survive a seizure.** `ClosedBelowFloor` keeps its account and vault
/// until the claim, so a card seized after closure must still be claimable; `Seized` is
/// `close_seized`'s terminal and `close_seized` closes no account, so the claim is still owed
/// afterwards. Refusing either would refuse the call its sibling's `CollateralAbsent` message
/// sends the depositor to make.
///
/// **Never gated, on any standard**: nothing may gate an exit here, and that is strongest where
/// the pool has already taken everything it will take from this position.
pub fn handler(ctx: Context<ClaimNftCore>) -> Result<()> {
    // Before the CPI, so a depositor whose card is gone or frozen is told which, and to call
    // `close_seized`, instead of reading an MPL Core error number. The gate pins the collection
    // to the one the asset declares.
    let collection = require_collateral_settleable(
        &ctx.accounts.asset,
        &ctx.accounts.collection,
        &ctx.accounts.position_vault.key(),
    )?;

    let pool = ctx.accounts.position.pool;
    let nft_mint = ctx.accounts.position.nft_mint;
    let position_key = ctx.accounts.position.key();

    // Paid before the account closes over it: once `close = depositor` runs, nothing records
    // the balance. A `Seized` position is paid alike.
    let fees_paid = pay_closed_accrued(&mut ctx.accounts.pool, &mut ctx.accounts.position)?;
    if fees_paid > 0 {
        let depositor_usdc = ctx
            .accounts
            .depositor_usdc
            .as_ref()
            .ok_or(ErrorCode::AccountNotEnoughKeys)?;
        let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();
        let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];
        let pool_signer: &[&[&[u8]]] = &[pool_seeds];

        transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.principal_vault.to_account_info(),
                    to: depositor_usdc.to_account_info(),
                    authority: ctx.accounts.pool.to_account_info(),
                },
                pool_signer,
            ),
            fees_paid,
        )?;
    }

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
        fees_paid,
    });

    Ok(())
}

/// Depositor-signed: the custody release of a position that is already closed, paying first the
/// `accrued` the closure stored. No weight, tier or counter surface — every one of those moved
/// when the position closed, whether `record_value` closed it below the floor or `close_seized`
/// seized it; the pool is here only for `owed_fees` and the vault the balance is paid from.
#[derive(Accounts)]
pub struct ClaimNftCore<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// Keyed on the supplied pool, so the balance is paid from, and discharged against, the
    /// position's own pool and no other.
    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // MPL Core only; on the accounts struct so it runs before any CPI.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction,
        // Both terminals are live: `Seized` is written only by `close_seized`, which requires
        // `standard == 2`, so this instruction alone serves every seized position.
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

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    /// Required only when the position is owed a non-zero `accrued`; the client creates it, at
    /// the depositor's expense, in the same transaction when it does not exist.
    #[account(
        mut,
        constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub depositor_usdc: Option<Box<Account<'info, TokenAccount>>>,

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

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use anchor_lang::error::ErrorCode;

    use crate::common::test_support::{accounts_body, calls, field_writes, flat, production};
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

    /// This exit must work on a card the pool has already lost, so the gate is not a formality:
    /// a seizure after closure leaves the card claimable, and the depositor gets
    /// `CollateralAbsent` or `CollateralFrozen` naming `close_seized`, which admits
    /// `ClosedBelowFloor` so it does not refuse the call its sibling's error sent.
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

    /// Both source states are live, unlike `claim_nft`: `Seized` is written by `close_seized`
    /// alone, which requires `standard == 2`, so this twin alone serves a seized position. The
    /// count over `PositionState`'s variants makes "two of five" a claim rather than a spelling.
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

    /// The two remaining guards on the position, and the USDC account's mint and owner pins.
    /// Nothing may gate an exit, so a sixth constraint here is a gate somebody added to a claim.
    #[test]
    fn the_standard_and_the_depositor_are_asserted_and_nothing_else_is() {
        let accounts = accounts_body(CLAIM_NFT_CORE_SRC);
        assert!(accounts.contains(
            "constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
        assert!(accounts.contains(
            "constraint = position.standard == STANDARD_CORE\n            @ ByeMachineError::WrongStandardForInstruction"
        ));
        assert_eq!(accounts.matches("constraint = ").count(), 5);
        let prod = production(CLAIM_NFT_CORE_SRC);
        // Counted over code alone, so this file's prose about the absent gates is not read as
        // the gate.
        let code = prod;
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

    /// Every counter, the weight and tier membership moved when the position closed, so a
    /// `WeightIndex`, `TopTier` or `WalletStats` slot here would be a second removal. The pool is
    /// here for `owed_fees` and its principal vault alone, and it seeds the position, so the
    /// balance is paid from the position's own pool.
    #[test]
    fn the_pool_is_here_for_the_balance_alone_and_binds_the_position() {
        let accounts = accounts_body(CLAIM_NFT_CORE_SRC);
        for absent in [
            "weight_index",
            "top_tier",
            "wallet_stats",
            "protocol_config",
        ] {
            assert_eq!(
                accounts.matches(absent).count(),
                0,
                "{absent} is not the claim's"
            );
        }
        assert!(accounts
            .contains("seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],"));
        assert!(accounts.contains("seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],"));
        assert_eq!(
            accounts.matches("position.pool.as_ref()").count(),
            0,
            "a self-anchored position would let the caller pick the paying pool"
        );
        for receiver in [
            "pool",
            "position",
            "ctx.accounts.pool",
            "ctx.accounts.position",
        ] {
            assert!(
                field_writes(CLAIM_NFT_CORE_SRC, receiver).is_empty(),
                "{receiver} is written here rather than in pay_closed_accrued"
            );
        }
        let principal = accounts_body(CLAIM_NFT_SRC);
        for absent in ["weight_index", "top_tier", "wallet_stats"] {
            assert_eq!(
                principal.matches(absent).count(),
                0,
                "the Token Metadata twin carries {absent} — the comparison above is stale"
            );
        }
    }

    /// Paid through the fee claims' shared function, out of the pool's principal vault under the
    /// pool's signature, to the depositor's own USDC account, after the collateral gate and before
    /// the release. A `Seized` position is paid alike; a burned card fails the gate, and its
    /// balance is left to the fee claim, which reads no asset.
    #[test]
    fn the_stored_accrual_is_paid_after_the_gate_and_before_the_release() {
        let flat = flat(CLAIM_NFT_CORE_SRC);
        assert_eq!(calls(CLAIM_NFT_CORE_SRC, "pay_closed_accrued"), 1);
        assert!(flat.contains(
            "let fees_paid = pay_closed_accrued(&mut ctx.accounts.pool, &mut ctx.accounts.position)?;"
        ));
        assert!(flat.contains("if fees_paid > 0 {"));
        assert!(flat.contains(
            "let depositor_usdc = ctx .accounts .depositor_usdc .as_ref() \
             .ok_or(ErrorCode::AccountNotEnoughKeys)?;"
        ));
        assert!(flat.contains(
            "Transfer { from: ctx.accounts.principal_vault.to_account_info(), \
             to: depositor_usdc.to_account_info(), \
             authority: ctx.accounts.pool.to_account_info(), }, pool_signer, ), fees_paid, )?;"
        ));
        assert_eq!(calls(CLAIM_NFT_CORE_SRC, "transfer"), 1);
        let prod = production(CLAIM_NFT_CORE_SRC);
        let paid = prod.find("pay_closed_accrued(").unwrap();
        assert!(prod.find("require_collateral_settleable(").unwrap() < paid);
        assert!(paid < prod.find("release_core_from_vault(").unwrap());
        let accounts = accounts_body(CLAIM_NFT_CORE_SRC);
        assert!(accounts.contains("pub depositor_usdc: Option<Box<Account<'info, TokenAccount>>>"));
        assert!(accounts.contains(
            "constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted"
        ));
        assert!(accounts.contains(
            "constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
        assert_eq!(u32::from(ErrorCode::AccountNotEnoughKeys), 3005);
    }

    /// No Token Metadata leg and no vault close, with the principal carrying both so the
    /// absences are a comparison rather than a spelling. The SPL `transfer` of the balance is the
    /// one token-program CPI here.
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

    /// The asset is pinned to the position's own card, which `read_collateral` cannot do for
    /// itself: otherwise a depositor could present a settleable asset of their own.
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
        let attributes = accounts;
        for form in ["init", "init_if_needed", "zero", "token::"] {
            assert_eq!(
                attributes.matches(form).count(),
                0,
                "{form} allocates nothing here"
            );
        }
    }

    /// The collection is pinned to what the asset declares, and the pin is the gate's.
    /// `require_collateral_settleable` requires the supplied account to be the collection the
    /// asset names and returns the argument `TransferV1` must be given: the account while the
    /// asset belongs to a collection, `None` once its collection authority has removed it (MPL
    /// Core refuses a collection account the asset does not name). Passing
    /// `&ctx.accounts.collection` to the release instead would ignore the gate's answer.
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
             depositor: ctx.accounts.depositor.key(), nft_mint, fees_paid, });"
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
