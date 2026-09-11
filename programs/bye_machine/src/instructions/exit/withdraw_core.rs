use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};
use mpl_core::ID as MPL_CORE_ID;

use crate::common::core_asset::{release_core_from_vault, require_collateral_settleable};
use crate::common::errors::ByeMachineError;
use crate::common::events::{RebalanceEvaluated, TierChanged, Withdrawn};
use crate::common::exit_accounting::apply_withdraw;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PRINCIPAL_VAULT_SEED, TOP_TIER_SEED,
    WALLET_STATS_SEED,
};
use crate::common::standards::STANDARD_CORE;
use crate::common::tier;
use crate::state::{Pool, Position, TopTier, WalletStats, WeightIndex};

/// `withdraw`'s MPL-Core twin.
///
/// **The accounting is not restated here, it is called.** `apply_withdraw` in
/// `common/exit_accounting.rs` is the roll gate, the lock, the two-leg fee settle, the four
/// checked counter decrements and the rebalance evaluation, and it is the same function
/// `withdraw` calls, so a second copy of that arithmetic would be two places for the answer to
/// what a depositor is paid. What this file owns is the custody leg and the account set
/// around it.
///
/// **`open_batches == 0` therefore comes for free, and that is the correct reading rather than an
/// omission.** The weight-freeze guard knows nothing about standards, so the twin
/// carries it identically — through the shared accounting, where
/// `assert_weight_open` runs first of all.
///
/// **The collateral gate runs before `apply_withdraw`, one statement into the handler, and the
/// position is the only exit where that placement is load-bearing.** This handler pays fees out
/// of the principal vault before it touches the card, so a gate sitting next to the release
/// would return its named code after a USDC transfer had already been built — and the depositor
/// of a burned card would get an opaque MPL Core revert instead of `CollateralAbsent` (6307) or
/// `CollateralFrozen` (6309) naming `close_seized`.
pub fn handler<'info>(ctx: Context<'_, '_, 'info, 'info, WithdrawCore<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let depositor = ctx.accounts.depositor.key();
    let tier_size = ctx.accounts.pool.tier_size;
    let acc_tier = ctx.accounts.top_tier.acc_tier;
    let vault_bump = ctx.accounts.position.vault_bump;

    // First, and before both of this handler's CPIs — the fee payout moves USDC before the card
    // is touched, so this is the one Core exit where "before the CPI" is not the same statement
    // as "before the release". The collection goes in because the freeze can live in its plugin
    // registry rather than the asset's, and the gate pins it to the collection the asset declares
    // — which is why this handler no longer re-states that pin before the release below.
    let collection = require_collateral_settleable(
        &ctx.accounts.asset,
        &ctx.accounts.collection,
        &ctx.accounts.position_vault.key(),
    )?;

    let effect = apply_withdraw(
        &mut ctx.accounts.pool,
        &mut ctx.accounts.position,
        &mut ctx.accounts.wallet_stats,
        acc_tier,
        now,
    )?;

    ctx.accounts.weight_index.load_mut()?.remove(
        effect.slot_index,
        effect.recorded_value,
        effect.expected_total,
    )?;

    let mut tier_change: Option<(Option<Pubkey>, Option<Pubkey>)> = None;
    if effect.was_member {
        tier::vacate(
            &mut ctx.accounts.top_tier,
            position_key,
            &mut ctx.accounts.position,
            acc_tier,
        )?;
        let entered = tier::fill_vacancy_from_candidate(
            &mut ctx.accounts.top_tier,
            tier_size,
            ctx.remaining_accounts,
            pool_key,
            acc_tier,
        )?;
        tier_change = Some((Some(position_key), entered));
    }

    let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();
    let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];
    let pool_signer: &[&[&[u8]]] = &[pool_seeds];

    if effect.fees_paid > 0 {
        let destination = ctx
            .accounts
            .depositor_usdc
            .as_ref()
            .ok_or(ErrorCode::AccountNotEnoughKeys)?;
        transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.principal_vault.to_account_info(),
                    to: destination.to_account_info(),
                    authority: ctx.accounts.pool.to_account_info(),
                },
                pool_signer,
            ),
            effect.fees_paid,
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
        vault_bump,
    )?;

    emit!(Withdrawn {
        pool: pool_key,
        slot: clock.slot,
        position: position_key,
        depositor,
        fees_paid: effect.fees_paid,
    });

    emit!(RebalanceEvaluated {
        pool: pool_key,
        slot: clock.slot,
        n_real: ctx.accounts.pool.n_real,
        w_real: ctx.accounts.pool.w_real,
        blanks_before: effect.blanks_before,
        blanks_after: ctx.accounts.pool.blanks,
    });

    if let Some((left, entered)) = tier_change {
        emit!(TierChanged {
            pool: pool_key,
            slot: clock.slot,
            left,
            entered,
        });
    }

    Ok(())
}

/// Depositor-signed: the asset, the fee payout and the position's rent all
/// go to the position's own depositor, who is also the only signer. An optional tier successor
/// travels in `remaining_accounts` and is read only when the closing position was a member.
///
/// **Six accounts smaller than its principal**, and every one of the six is Token Metadata's
/// account model rather than a dropped check: no `nft_mint` (a Core asset has no mint), no
/// `position_vault` **token** account, no destination token account, no `metadata`, no
/// `master_edition`, and none of the five pNFT accounts. That is the MPL Core family's cost
/// advantage, in the exit direction.
#[derive(Accounts)]
pub struct WithdrawCore<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // The MPL Core family only, and on the accounts struct so it runs before any CPI — this
        // handler moves USDC out of the principal vault before it touches the card, so a
        // handler-side check would come after a transfer that had already happened.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(mut, address = pool.weight_index)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    #[account(
        mut,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(
        mut,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()],
        bump = wallet_stats.bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    /// Optional on exactly its principal's terms: the payout is structurally zero today, and
    /// requiring a USDC account for a transfer that never runs would gate an exit on something
    /// that has no permission to gate it. Required the moment the payout is non-zero.
    #[account(
        mut,
        constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub depositor_usdc: Option<Box<Account<'info, TokenAccount>>>,

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
    /// depositor could present a settleable asset of their own and withdraw a position whose
    /// card has been seized — taking the weight out and leaving the pool's own record of the
    /// seizure unwritten. Writable because `TransferV1` rewrites `AssetV1.owner`.
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
    use crate::common::test_support::{
        accounts_body, calls, field_writes, flat, production, without_doc_comments,
    };

    const WITHDRAW_CORE_SRC: &str = include_str!("withdraw_core.rs");
    const WITHDRAW_SRC: &str = include_str!("withdraw.rs");
    const EXIT_ACCOUNTING_SRC: &str = include_str!("../../common/exit_accounting.rs");

    // --- the seam with the shared accounting --------------------------------------------------

    /// **This handler writes no state field directly, and the empty set is the claim.** The
    /// accounting lives in `common/exit_accounting.rs` so both twins call it; a field
    /// written *here* would be a second decrement, a second checkpoint stamp or a counter this
    /// twin moves and its principal does not — the divergence the extraction exists to make
    /// impossible. `withdraw.rs` asserts the identical empty set, and the accounting module
    /// asserts the nine writes both of them delegate.
    #[test]
    fn the_twin_restates_none_of_its_principals_accounting() {
        for receiver in [
            "pool",
            "position",
            "wallet_stats",
            "ctx.accounts.pool",
            "ctx.accounts.position",
            "ctx.accounts.wallet_stats",
            "ctx.accounts.top_tier",
        ] {
            assert!(
                field_writes(WITHDRAW_CORE_SRC, receiver).is_empty(),
                "{receiver} is written directly; every state write is apply_withdraw's"
            );
        }
        assert_eq!(calls(WITHDRAW_CORE_SRC, "apply_withdraw"), 1);
        // The control: the set neither handler writes is written in exactly one place.
        assert_eq!(
            field_writes(EXIT_ACCOUNTING_SRC, "pool"),
            ["blanks", "n_real", "owed_fees", "w_real"]
        );
    }

    /// The call, argument for argument. All five values are derivable from `ctx` and the last two
    /// are the ones no unit case can see: `acc_tier` read off `pool.acc_equal` instead of
    /// `top_tier.acc_tier` is a mis-sourcing that a behavioural test alone would not catch, since
    /// every accumulator is structurally zero today and nothing behavioural can distinguish them.
    #[test]
    fn every_accounting_argument_is_sourced_from_its_own_account_and_field() {
        let flat_src = flat(WITHDRAW_CORE_SRC);
        assert!(flat_src.contains(
            "apply_withdraw( &mut ctx.accounts.pool, &mut ctx.accounts.position, \
             &mut ctx.accounts.wallet_stats, acc_tier, now, )?;"
        ));
        for binding in [
            "let clock = Clock::get()?;",
            "let now = clock.unix_timestamp;",
            "let acc_tier = ctx.accounts.top_tier.acc_tier;",
            "let tier_size = ctx.accounts.pool.tier_size;",
            "let vault_bump = ctx.accounts.position.vault_bump;",
        ] {
            assert!(
                flat_src.contains(binding),
                "mis-sourced or missing: {binding}"
            );
        }
    }

    /// **The roll gate is carried identically — through the shared accounting rather than by a
    /// copy of the assertion.** The twin carries `open_batches == 0` because the weight-freeze
    /// rule knows nothing about standards. The crate-wide partition in `instructions/mod.rs`
    /// classifies this file as reaching the guard and asserts all three links; this is the local
    /// half, and it is stated as an absence *plus* a reach so that "no direct call" can never
    /// read as "no gate".
    #[test]
    fn the_roll_gate_is_reached_and_never_restated() {
        assert_eq!(calls(WITHDRAW_CORE_SRC, "assert_weight_open"), 0);
        assert_eq!(calls(EXIT_ACCOUNTING_SRC, "assert_weight_open"), 1);
        assert_eq!(
            without_doc_comments(production(WITHDRAW_CORE_SRC))
                .matches("open_batches")
                .count(),
            0,
            "the guard reads open_batches; a second read here is a second gate"
        );
    }

    // --- the collateral gate, which is this twin's whole addition to the lifecycle ------------

    /// **The gate runs first, and on this handler that placement is the one that matters.**
    /// `withdraw_core` makes two CPIs and the fee payout is the earlier one, so a gate next to
    /// the release would return `CollateralAbsent` after USDC had already been moved — "tests
    /// the asset before the CPI" is a claim about which CPI, not just about the
    /// release. Ordered against **both**, and against `apply_withdraw` too, so the cheapest
    /// refusal is also the first.
    #[test]
    fn the_collateral_gate_precedes_the_accounting_and_both_cpis() {
        assert!(flat(WITHDRAW_CORE_SRC).contains(
            "require_collateral_settleable( &ctx.accounts.asset, &ctx.accounts.collection, \
             &ctx.accounts.position_vault.key(), )?;"
        ));
        assert_eq!(calls(WITHDRAW_CORE_SRC, "require_collateral_settleable"), 1);
        let prod = production(WITHDRAW_CORE_SRC);
        let gate = prod.find("require_collateral_settleable(").unwrap();
        for later in [
            "apply_withdraw(",
            "transfer(\n            CpiContext::new_with_signer(",
            "release_core_from_vault(",
        ] {
            assert!(
                gate < prod.find(later).unwrap(),
                "the collateral gate must precede {later}"
            );
        }
    }

    /// The release, argument for argument, with the depositor as both destination and payer — a
    /// depositor-signed exit pays for its own transaction, unlike the permissionless twin.
    #[test]
    fn every_release_argument_is_bound_at_this_call_site() {
        assert!(flat(WITHDRAW_CORE_SRC).contains(
            "release_core_from_vault( &ctx.accounts.asset, collection, \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.position_vault.to_account_info(), \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.system_program.to_account_info(), &ctx.accounts.mpl_core_program, \
             &position_key, vault_bump, )?;"
        ));
        assert_eq!(calls(WITHDRAW_CORE_SRC, "release_core_from_vault"), 1);
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
            calls(WITHDRAW_CORE_SRC, "declared_core_collection"),
            0,
            "the declaration is read inside the gate now; a second read here would be a second \
             answer to which collection this asset belongs to"
        );
        let prod = production(WITHDRAW_CORE_SRC);
        assert_eq!(
            prod.matches("&ctx.accounts.collection").count(),
            1,
            "the account is named once, where it is handed to the gate; the release takes what \
             the gate returned"
        );
        assert!(flat(WITHDRAW_CORE_SRC).contains(
            "let collection = require_collateral_settleable( &ctx.accounts.asset, \
                           &ctx.accounts.collection, &ctx.accounts.position_vault.key(), )?;"
        ));
        assert!(
            prod.find("require_collateral_settleable(").unwrap()
                < prod.find("release_core_from_vault(").unwrap(),
            "the checked read must precede the release that trusts it"
        );
    }

    /// The asset pinned to the position's own card — without it the gate tests an asset the
    /// caller chose, and a depositor could withdraw a position whose card has been seized by
    /// presenting a settleable one of their own: the weight leaves the pool and the seizure goes
    /// unrecorded.
    #[test]
    fn the_asset_is_pinned_to_the_positions_own_card() {
        assert!(accounts_body(WITHDRAW_CORE_SRC).contains(
            "#[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]\n    pub asset: UncheckedAccount<'info>,"
        ));
    }

    // --- the orchestration this file owns, pinned against its principal's ---------------------

    /// **The fourth production caller of `WeightIndex::remove`, and its ordered-argument pin.**
    /// `remove` derives the leaf weight itself but verifies against the value and the aggregate
    /// the caller hands it, so anything but the pre-close `recorded_value` and the
    /// post-decrement total is a `WeightDesync` at best and a verification against the wrong
    /// leaf at worst. Pinned identically to its principal's, which is why the crate-wide caller
    /// count names both twins.
    #[test]
    fn remove_receives_the_recorded_value_and_the_post_decrement_expected_total() {
        assert!(flat(WITHDRAW_CORE_SRC).contains(
            "ctx.accounts.weight_index.load_mut()?.remove( effect.slot_index, \
             effect.recorded_value, effect.expected_total, )?;"
        ));
        assert_eq!(calls(WITHDRAW_CORE_SRC, ".remove"), 1);
        assert_eq!(calls(WITHDRAW_CORE_SRC, ".insert"), 0);
        assert_eq!(calls(WITHDRAW_CORE_SRC, ".update"), 0);
    }

    /// Every tier transition delegates to the checked operations in `common/tier.rs`, and the
    /// two this exit uses are the two its principal uses — a twin that promoted or refreshed
    /// instead would move membership on a position that is leaving.
    #[test]
    fn every_tier_transition_delegates_to_the_checked_tier_operations() {
        for (callee, expected) in [
            ("tier::vacate", 1),
            ("tier::fill_vacancy_from_candidate", 1),
            ("tier::demote", 0),
            ("tier::promote", 0),
            ("tier::refresh_member", 0),
            ("tier::cross_into_tier", 0),
        ] {
            assert_eq!(
                calls(WITHDRAW_CORE_SRC, callee),
                expected,
                "{callee} in the Core twin"
            );
            assert_eq!(
                calls(WITHDRAW_SRC, callee),
                expected,
                "{callee} in the principal — the twin's tier surface is pinned against it"
            );
        }
        let prod = production(WITHDRAW_CORE_SRC);
        assert!(
            prod.find("tier::vacate(").unwrap()
                < prod.find("tier::fill_vacancy_from_candidate(").unwrap(),
            "the vacancy must exist before it is filled"
        );
        // The candidate is read only inside the membership branch: a non-member's withdrawal
        // must not consume a caller-supplied position at all.
        let branch = prod.find("if effect.was_member {").unwrap();
        assert!(branch < prod.find("ctx.remaining_accounts").unwrap());
        assert_eq!(prod.matches("ctx.remaining_accounts").count(), 1);
    }

    /// The fee payout: principal vault → the depositor's own USDC account, under the pool's
    /// signature, and skipped entirely at zero — which is every payout in slice 1.
    #[test]
    fn the_fee_payout_moves_principal_vault_to_the_depositor_and_is_skipped_at_zero() {
        let flat_src = flat(WITHDRAW_CORE_SRC);
        assert!(flat_src.contains("if effect.fees_paid > 0 {"));
        assert!(flat_src.contains(
            "Transfer { from: ctx.accounts.principal_vault.to_account_info(), \
             to: destination.to_account_info(), authority: ctx.accounts.pool.to_account_info(), }"
        ));
        assert!(flat_src.contains(
            "let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), \
             &[ctx.accounts.pool.bump]];"
        ));
        assert_eq!(calls(WITHDRAW_CORE_SRC, "transfer"), 1);
    }

    /// All three payloads, field by field and identical to their principal's — every twin
    /// carries its principal's event row unchanged, and `blanks_before == blanks_after` is
    /// the mis-sourcing that leaves `RebalanceEvaluated` well-formed and semantically empty.
    #[test]
    fn all_three_event_payloads_are_pinned_field_by_field() {
        let flat_src = flat(WITHDRAW_CORE_SRC);
        assert!(flat_src.contains(
            "emit!(Withdrawn { pool: pool_key, slot: clock.slot, position: position_key, \
             depositor, fees_paid: effect.fees_paid, });"
        ));
        assert!(flat_src.contains(
            "emit!(RebalanceEvaluated { pool: pool_key, slot: clock.slot, \
             n_real: ctx.accounts.pool.n_real, w_real: ctx.accounts.pool.w_real, \
             blanks_before: effect.blanks_before, blanks_after: ctx.accounts.pool.blanks, });"
        ));
        assert!(flat_src
            .contains("emit!(TierChanged { pool: pool_key, slot: clock.slot, left, entered, });"));
        let prod = production(WITHDRAW_CORE_SRC);
        assert_eq!(prod.matches("emit!(").count(), 3);
        assert!(
            prod.find("emit!(Withdrawn").unwrap() < prod.find("emit!(RebalanceEvaluated").unwrap(),
            "Withdrawn leads, matching its principal's event order"
        );
        // TierChanged is emitted from one site carrying both sides — two sites would report a
        // swap as a departure and an arrival that no decoder can pair.
        assert_eq!(prod.matches("emit!(TierChanged").count(), 1);
        assert!(prod.contains("if let Some((left, entered)) = tier_change {"));
    }

    // --- the account surface ------------------------------------------------------------------

    /// The two accounts-struct guards, and the count that stops a third being added quietly.
    /// The state and the lock are **not** here: they belong to the shared accounting, where the
    /// order between them is asserted against a `ClosedBelowFloor` position under a live lock.
    #[test]
    fn the_standard_and_the_depositor_are_asserted_on_the_accounts_struct() {
        let accounts = accounts_body(WITHDRAW_CORE_SRC);
        assert!(accounts.contains(
            "constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
        assert!(accounts.contains(
            "constraint = position.standard == STANDARD_CORE\n            @ ByeMachineError::WrongStandardForInstruction"
        ));
        // Four: the two above plus the payout destination's mint and owner.
        assert_eq!(accounts.matches("constraint = ").count(), 4);
        assert!(accounts.contains(
            "constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted"
        ));
        assert!(accounts.contains(
            "constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
    }

    /// **Six accounts smaller than its principal, and every one of the six is Token Metadata's
    /// account model.** Asserted as a comparison rather than a list of names: the principal must
    /// still carry each one, or the absence measures a spelling.
    #[test]
    fn the_token_metadata_account_surface_is_absent_and_the_principal_still_has_it() {
        let accounts = accounts_body(WITHDRAW_CORE_SRC);
        let principal = accounts_body(WITHDRAW_SRC);
        for absent in [
            "nft_mint:",
            "depositor_token:",
            "metadata:",
            "master_edition:",
            "token_record",
            "authorization_rules",
            "sysvar_instructions",
            "associated_token_program",
        ] {
            assert_eq!(
                accounts.matches(absent).count(),
                0,
                "{absent} has no MPL Core analogue and must not appear"
            );
            assert!(
                principal.contains(absent),
                "the Token Metadata twin must still declare {absent}"
            );
        }
        // And the state surface it keeps is its principal's, entire.
        for present in [
            "pub pool:",
            "pub weight_index:",
            "pub top_tier:",
            "pub wallet_stats:",
            "pub principal_vault:",
            "pub depositor_usdc:",
        ] {
            assert!(accounts.contains(present), "missing {present}");
            assert!(principal.contains(present), "the comparison above is stale");
        }
    }

    /// No Token Metadata leg and no vault close, with the principal carrying both.
    #[test]
    fn no_token_metadata_leg_and_no_vault_close_reach_the_core_twin() {
        for callee in [
            "release_from_vault",
            "transfer_pnft",
            "transfer_spl",
            "close_account",
        ] {
            assert_eq!(
                calls(WITHDRAW_CORE_SRC, callee),
                0,
                "{callee} reaches the Core twin"
            );
        }
        assert!(
            calls(WITHDRAW_SRC, "close_account") == 1
                && calls(WITHDRAW_SRC, "release_from_vault") == 1,
            "the Token Metadata twin must still carry both, or the absences above measure nothing"
        );
    }

    /// The vault is an address signed under the stored bump, never an allocation.
    #[test]
    fn the_vault_is_an_address_signed_under_the_stored_bump() {
        let accounts = accounts_body(WITHDRAW_CORE_SRC);
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

    #[test]
    fn the_position_closes_to_the_depositor_and_to_nobody_else() {
        let accounts = accounts_body(WITHDRAW_CORE_SRC);
        assert!(accounts.contains("close = depositor,"));
        assert_eq!(accounts.matches("close = ").count(), 1);
    }

    /// Every address-pinned account carries its exact address: an unpinned `weight_index` hands
    /// the removal a caller-supplied Fenwick tree, and an unpinned `mpl_core_program` hands a
    /// caller-supplied program the vault PDA's signature.
    #[test]
    fn every_address_pinned_account_carries_its_exact_address() {
        let accounts = accounts_body(WITHDRAW_CORE_SRC);
        for pin in [
            "#[account(mut, address = pool.weight_index)]",
            "#[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]",
            "#[account(address = MPL_CORE_ID)]",
        ] {
            assert!(accounts.contains(pin), "missing address pin: {pin}");
        }
        assert_eq!(accounts.matches("address = ").count(), 3);
    }

    #[test]
    fn handler_has_exactly_one_ok_and_zero_return_statements() {
        let code = without_doc_comments(production(WITHDRAW_CORE_SRC));
        assert_eq!(code.matches("Ok(())").count(), 1);
        assert_eq!(code.matches("return ").count(), 0);
    }
}
