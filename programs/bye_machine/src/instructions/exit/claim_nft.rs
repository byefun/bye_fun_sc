use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::ID as SYSVAR_INSTRUCTIONS_ID;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{close_account, CloseAccount, Mint, Token, TokenAccount};
use mpl_token_metadata::ID as TOKEN_METADATA_ID;

use crate::common::errors::ByeMachineError;
use crate::common::escrow::{release_from_vault, PnftRelease};
use crate::common::events::NftClaimed;
use crate::common::seeds::{POSITION_SEED, POSITION_VAULT_SEED};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::state::{Position, PositionState};

pub fn handler(ctx: Context<ClaimNft>) -> Result<()> {
    let pool = ctx.accounts.position.pool;
    let nft_mint = ctx.accounts.position.nft_mint;
    let position_seeds: &[&[u8]] = &[
        POSITION_SEED,
        pool.as_ref(),
        nft_mint.as_ref(),
        &[ctx.accounts.position.bump],
    ];
    let signer_seeds: &[&[&[u8]]] = &[position_seeds];

    release_from_vault(
        ctx.accounts.position.standard,
        &ctx.accounts.position_vault.to_account_info(),
        &ctx.accounts.position.to_account_info(),
        &ctx.accounts.depositor_token,
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.nft_mint.to_account_info(),
        &ctx.accounts.metadata,
        &ctx.accounts.master_edition,
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.associated_token_program.to_account_info(),
        &ctx.accounts.token_metadata_program,
        PnftRelease {
            position_token_record: ctx.accounts.position_token_record.as_deref(),
            depositor_token_record: ctx.accounts.depositor_token_record.as_deref(),
            sysvar_instructions: ctx.accounts.sysvar_instructions.as_deref(),
            authorization_rules_program: ctx.accounts.authorization_rules_program.as_deref(),
            authorization_rules: ctx.accounts.authorization_rules.as_deref(),
        },
        signer_seeds,
    )?;

    // The vault PDA is derived from the position, which the same seeds re-create on a
    // re-deposit — leaving an emptied token account behind would make that re-deposit
    // permanently unsatisfiable.
    close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        CloseAccount {
            account: ctx.accounts.position_vault.to_account_info(),
            destination: ctx.accounts.depositor.to_account_info(),
            authority: ctx.accounts.position.to_account_info(),
        },
        signer_seeds,
    ))?;

    emit!(NftClaimed {
        pool,
        slot: Clock::get()?.slot,
        position: ctx.accounts.position.key(),
        depositor: ctx.accounts.depositor.key(),
        nft_mint,
    });

    Ok(())
}

/// Depositor-signed: the custody release of a position
/// `record_value` already closed below the floor. No `Pool`, no weight, no tier and no counter
/// surface — every one of those moved when the position was closed, so this instruction only
/// returns the NFT and the rent. Never pause-gated, and it never reaches the weight
/// freeze either: nothing here moves weight.
#[derive(Accounts)]
pub struct ClaimNft<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // This instruction serves the Token Metadata family only. On the accounts struct rather
        // than in the handler so all three exits carry the guard in the same place and it runs
        // before any CPI — which is load-bearing in `withdraw`, where the fee payout moves USDC
        // before the card is touched, and uniform here so the three do not drift into two shapes.
        constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)
            @ ByeMachineError::WrongStandardForInstruction,
        // **One terminal, because one terminal is reachable.** `Seized` is written by
        // `close_seized` alone, which asserts `standard == 2`; this instruction serves `{0, 1}`,
        // so the two constraints can never both hold and a `Seized` arm here could not run.
        //
        // **The Token Metadata family has no seizure terminal at all, and that is a recorded gap
        // rather than an oversight.** The mechanism that would strand a pNFT is a Metaplex
        // rule-set revision that denies the exit — `rule-set-replay` measures the nine live
        // revisions and none of them does, which is the whole basis for calling it inoperative.
        // If one ever did, the position would hold weight behind a card the exits cannot move
        // and **nothing** would clear it: `close_seized` refuses the standard, and widening it is
        // a new release leg, a new account list and a decision, not a match arm. This needs
        // ruling before this family goes to mainnet.
        constraint = position.state == PositionState::ClosedBelowFloor
            @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        mut,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub nft_mint: Box<Account<'info, Mint>>,

    /// CHECK: created as the depositor's associated token account by `TransferV1` when empty,
    /// and checked against `depositor` by that same processor when it already exists.
    #[account(mut)]
    pub depositor_token: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    pub master_edition: UncheckedAccount<'info>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1.
    #[account(mut)]
    pub position_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `position_token_record`.
    #[account(mut)]
    pub depositor_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the Token Metadata program this pool's pNFTs are governed by.
    #[account(address = TOKEN_METADATA_ID)]
    pub token_metadata_program: UncheckedAccount<'info>,

    /// CHECK: optional, forwarded to `TransferV1`, which pins it to Token Auth Rules.
    pub authorization_rules_program: Option<UncheckedAccount<'info>>,

    /// CHECK: optional, forwarded to `TransferV1`, which asserts it matches the mint's rule set.
    pub authorization_rules: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the instructions sysvar `TransferV1` reads for its CPI guard. Optional on
    /// the same terms as the two token records — SPL `Transfer` has no CPI guard to read it.
    #[account(address = SYSVAR_INSTRUCTIONS_ID)]
    pub sysvar_instructions: Option<UncheckedAccount<'info>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAIM_SRC: &str = include_str!("claim_nft.rs");
    /// The Core twin, read for exactly one contrast: it admits the seizure terminal and this
    /// instruction cannot.
    const CLAIM_NFT_CORE_SRC: &str = include_str!("claim_nft_core.rs");
    const RETURN_REJECTED_SRC: &str = include_str!("../deposit/return_rejected.rs");

    use crate::common::test_support::{accounts_body, calls, field_writes, flat, production};
    use crate::state::position::ALL_POSITION_STATES;

    /// Asserts the handler still performs its three positive obligations. **Every `x0`
    /// assertion in this file has an inverted failure mode**: a deleted or gutted handler
    /// satisfies "does not touch the tier" exactly as a correct one does, so an absence is only
    /// evidence when it is read against a live body. The 51-statement deletion sweep found no
    /// miss — the custody release, the vault close and the emit are each held up by their own
    /// ordered-literal pin — but nothing made the absence claims *depend* on that, which is
    /// what this does. A precondition, not a narrower scope: each caller still counts its
    /// absences over the **whole** production half, so a `top_tier` or `principal_vault`
    /// account added to the `#[derive(Accounts)]` struct is still caught.
    fn assert_handler_is_live(source: &str) {
        let prod = production(source);
        let body =
            &prod[prod.find("pub fn handler").unwrap()..prod.find("#[derive(Accounts)]").unwrap()];
        assert_eq!(
            body.matches("release_from_vault(").count(),
            1,
            "custody release absent"
        );
        assert_eq!(
            body.matches("close_account(").count(),
            1,
            "vault close absent"
        );
        assert_eq!(
            body.matches("emit!(NftClaimed").count(),
            1,
            "NftClaimed absent"
        );
    }

    // --- the state guard and the signer shape ------------------------------------------------

    /// **`ClosedBelowFloor` and nothing else**, with the other four rejecting at 6301 —
    /// enumerated from `ALL_POSITION_STATES`, the list `position.rs` guards, so a variant added
    /// later cannot escape the case.
    ///
    /// `Seized` used to be the second admitted state, and dropping it is a review fix rather
    /// than a narrowing. It could never run: `close_seized` is its only writer and asserts
    /// `standard == 2`, which this instruction refuses. What made keeping it wrong was not the
    /// dead code but the reading — the only thing in this file that looked like a terminal for a
    /// stranded pNFT was an arm that cannot be reached, so the absence of such a terminal read
    /// as its presence. The Token Metadata family has none; the gap is stated at the constraint.
    ///
    /// **The twins deliberately differ here, which is why the contrast is asserted.**
    /// `claim_nft_core` admits both terminals and both of its arms are live.
    #[test]
    fn the_claim_admits_one_deactivation_terminal_and_the_rest_reject_at_6301() {
        assert!(flat(CLAIM_SRC).contains(
            "constraint = position.state == PositionState::ClosedBelowFloor \
             @ ByeMachineError::InvalidPositionState"
        ));
        assert_eq!(u32::from(ByeMachineError::InvalidPositionState), 6301);
        let admissible = ALL_POSITION_STATES
            .into_iter()
            .filter(|state| match state {
                PositionState::ClosedBelowFloor => true,
                PositionState::Pending
                | PositionState::Active
                | PositionState::Rejected
                | PositionState::Seized => false,
            })
            .count();
        assert_eq!(
            admissible, 1,
            "exactly one of PositionState's variants passes the constraint above"
        );
        assert!(
            flat(CLAIM_NFT_CORE_SRC).contains(
                "constraint = matches!( position.state, \
                 PositionState::ClosedBelowFloor | PositionState::Seized \
                 ) @ ByeMachineError::InvalidPositionState"
            ),
            "the Core twin must still admit both terminals — its `Seized` arm is the live one, \
             and without that contrast this narrowing reads as a rule about claims"
        );
    }

    /// Depositor-signed: "permissionless" here means "no further *operator*
    /// action", not "no signer at all". `return_rejected`'s `payer` + `UncheckedAccount` shape is the genuinely
    /// permissionless one and must not be copied here — it is asserted absent rather than
    /// merely unused.
    #[test]
    fn the_depositor_signs_and_the_permissionless_payer_shape_is_absent() {
        let prod = production(CLAIM_SRC);
        assert!(prod.contains("pub depositor: Signer<'info>"));
        assert!(prod.contains(
            "constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
        assert_eq!(u32::from(ByeMachineError::NotPositionOwner), 6302);
        assert_eq!(prod.matches("pub payer").count(), 0);
        assert_eq!(prod.matches("pub depositor: UncheckedAccount").count(), 0);
        assert!(
            production(RETURN_REJECTED_SRC).contains("pub payer: Signer<'info>"),
            "the shape reference must itself carry the permissionless payer"
        );
    }

    // --- the absences, by exact count --------------------------------------------------------

    #[test]
    fn reads_no_pause_flag_and_never_reaches_the_weight_freeze() {
        assert_handler_is_live(CLAIM_SRC);
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("deposits_paused").count(), 0);
        assert_eq!(prod.matches("rolls_paused").count(), 0);
        assert_eq!(calls(CLAIM_SRC, "assert_weight_open"), 0);
    }

    /// The weight, tier and counter surfaces all moved when `record_value` closed the
    /// position. Touching any of them here would move them a second time.
    #[test]
    fn touches_no_weight_tier_or_counter_surface() {
        assert_handler_is_live(CLAIM_SRC);
        let prod = production(CLAIM_SRC);
        for surface in [
            "weight_index",
            "w_real",
            "n_real",
            "blanks",
            "top_tier",
            "wallet_stats",
            "rebalance",
            "owed_fees",
            "acc_equal",
            "acc_tier",
            "position_fee_payout",
            "accrued",
        ] {
            assert_eq!(
                prod.matches(surface).count(),
                0,
                "{surface} is not this handler's — the close already ran"
            );
        }
    }

    /// No `pool` and no `protocol_config` account, asserted at zero rather than inferred
    /// from the struct being short. `position.pool` is read off the position for the event.
    #[test]
    fn declares_neither_a_pool_nor_a_protocol_config_account() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("pub pool:").count(), 0);
        assert_eq!(prod.matches("pub protocol_config:").count(), 0);
        assert_eq!(prod.matches("POOL_SEED").count(), 0);
        assert_eq!(prod.matches("PROTOCOL_CONFIG_SEED").count(), 0);
        assert!(prod.contains("let pool = ctx.accounts.position.pool;"));
    }

    // --- the emit surface ---------------------------------------------------------------------

    /// `RebalanceEvaluated` belongs on `approve_deposit` and `withdraw` and nothing else in
    /// scope: this instruction changes no draw domain, so publishing one would announce a
    /// recomputation that did not happen.
    #[test]
    fn emits_nft_claimed_once_carrying_the_mint_and_nothing_else() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("emit!(").count(), 1);
        assert_eq!(prod.matches("emit!(NftClaimed").count(), 1);
        assert!(flat(CLAIM_SRC).contains(
            "emit!(NftClaimed { pool, slot: Clock::get()?.slot, \
             position: ctx.accounts.position.key(), \
             depositor: ctx.accounts.depositor.key(), nft_mint, });"
        ));
        assert_eq!(prod.matches("RebalanceEvaluated").count(), 0);
        assert_eq!(prod.matches("TierChanged").count(), 0);
    }

    // --- the CPI surfaces ---------------------------------------------------------------------

    /// `release_from_vault` takes 13 positional `&AccountInfo` of identical type, so any swap
    /// compiles and nothing else in the tree distinguishes a correct call from a permuted one.
    /// The direction is vault → depositor, and the `payer` slot is the depositor here — this is
    /// the depositor-signed claim, not `return_rejected`'s permissionless shape.
    ///
    /// The five pNFT accounts are **not** in that count: `PnftRelease`'s named fields cannot be
    /// transposed, which is why they were grouped rather than appended.
    #[test]
    fn the_release_arguments_are_pinned_in_order_and_move_vault_to_depositor() {
        assert!(flat(CLAIM_SRC).contains(
            "release_from_vault( ctx.accounts.position.standard, \
             &ctx.accounts.position_vault.to_account_info(), \
             &ctx.accounts.position.to_account_info(), &ctx.accounts.depositor_token, \
             &ctx.accounts.depositor.to_account_info(), &ctx.accounts.nft_mint.to_account_info(), \
             &ctx.accounts.metadata, &ctx.accounts.master_edition, \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.system_program.to_account_info(), \
             &ctx.accounts.token_program.to_account_info(), \
             &ctx.accounts.associated_token_program.to_account_info(), \
             &ctx.accounts.token_metadata_program, PnftRelease { \
             position_token_record: ctx.accounts.position_token_record.as_deref(), \
             depositor_token_record: ctx.accounts.depositor_token_record.as_deref(), \
             sysvar_instructions: ctx.accounts.sysvar_instructions.as_deref(), \
             authorization_rules_program: ctx.accounts.authorization_rules_program.as_deref(), \
             authorization_rules: ctx.accounts.authorization_rules.as_deref(), }, \
             signer_seeds, )?;"
        ));
        assert_eq!(calls(CLAIM_SRC, "release_from_vault"), 1);
        assert_eq!(
            calls(CLAIM_SRC, "transfer_pnft"),
            0,
            "the branch is the only route to a transfer; a direct call would skip the standard"
        );
    }

    #[test]
    fn the_position_signs_under_its_own_exact_seeds() {
        assert!(flat(CLAIM_SRC).contains(
            "let position_seeds: &[&[u8]] = &[ POSITION_SEED, pool.as_ref(), nft_mint.as_ref(), \
             &[ctx.accounts.position.bump], ];"
        ));
        assert!(flat(CLAIM_SRC).contains("let signer_seeds: &[&[&[u8]]] = &[position_seeds];"));
    }

    /// The vault PDA is re-derived by a later re-deposit, so an emptied account left
    /// behind makes that re-deposit permanently unsatisfiable.
    #[test]
    fn the_vault_is_closed_to_the_depositor_under_the_position_authority() {
        assert!(flat(CLAIM_SRC).contains(
            "CloseAccount { account: ctx.accounts.position_vault.to_account_info(), \
             destination: ctx.accounts.depositor.to_account_info(), \
             authority: ctx.accounts.position.to_account_info(), }, signer_seeds, ))?;"
        ));
        assert_eq!(calls(CLAIM_SRC, "close_account"), 1);
    }

    /// `close = payer` in place of `close = depositor` on a sibling file was missed
    /// at baseline. Both rent refunds go to the position's own depositor.
    #[test]
    fn the_position_closes_to_the_depositor_and_to_nobody_else() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("close = ").count(), 1);
        assert!(prod.contains("close = depositor,"));
        assert_eq!(prod.matches("close = payer").count(), 0);
        assert_eq!(prod.matches("close = signer").count(), 0);
    }

    // --- the account surface and the discarded accrual ---------------------------------------

    #[test]
    fn every_state_account_is_boxed_and_every_pda_declares_its_seeds() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("Box<Account<").count(), 3);
        assert_eq!(prod.matches("seeds = [").count(), 2);
        assert!(prod.contains(
            "seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()]"
        ));
        assert!(prod.contains("seeds = [POSITION_VAULT_SEED, position.key().as_ref()]"));
        assert!(prod.contains("bump = position.bump,"));
        assert!(prod.contains("bump = position.vault_bump"));
        // Five, not two: the two token records and `sysvar_instructions` are optional so the
        // standard branch can require them absent on a legacy release. The count is the pin —
        // an account quietly made optional is an account that stopped being required.
        assert_eq!(prod.matches("Option<UncheckedAccount<'info>>").count(), 5);
    }

    #[test]
    fn handler_has_exactly_one_ok_and_zero_return_statements() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("Ok(())").count(), 1);
        assert_eq!(prod.matches("return ").count(), 0);
    }

    /// `claim_nft` closes the account with fees
    /// already settled at closure, so a `ClosedBelowFloor` position's `accrued` is
    /// **discarded** with the account rather than paid. Structurally zero today — `acc_equal`
    /// only advances once `settle_roll` ships. This pins the absence so the forfeiture is a
    /// recorded decision and not an oversight: no payout account, no transfer, no `accrued`
    /// read.
    #[test]
    fn the_below_floor_accrual_is_forfeited_with_the_account_and_no_payout_is_attempted() {
        assert_handler_is_live(CLAIM_SRC);
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("principal_vault").count(), 0);
        assert_eq!(prod.matches("Transfer {").count(), 0);
        assert_eq!(prod.matches("position.accrued").count(), 0);
        assert_eq!(
            calls(CLAIM_SRC, "transfer"),
            0,
            "no SPL transfer: the only CPI that moves anything here is transfer_pnft"
        );
    }

    /// Not one account here is mutated Rust-side — every write goes through a CPI — which the
    /// crate-wide `mut`-declaration enumerator cannot see, since it is silent on this whole
    /// file. Dropping `position_vault`'s `mut` left that enumerator's test green.
    /// `position` is louder (Anchor's macro rejects `close` without `mut` at compile time); the
    /// rest are pinned here by exact count, so deleting any single one fails.
    #[test]
    fn every_writable_account_declaration_is_pinned_by_count() {
        let accounts = accounts_body(CLAIM_SRC);
        assert_eq!(accounts.matches("mut").count(), 7);
        for declaration in [
            "#[account(mut)]\n    pub depositor: Signer<'info>",
            "#[account(mut)]\n    pub depositor_token: UncheckedAccount<'info>",
            "#[account(mut)]\n    pub metadata: UncheckedAccount<'info>",
            "#[account(mut)]\n    pub position_token_record: Option<UncheckedAccount<'info>>",
            "#[account(mut)]\n    pub depositor_token_record: Option<UncheckedAccount<'info>>",
            "mut,\n        close = depositor,",
            "mut,\n        seeds = [POSITION_VAULT_SEED",
        ] {
            assert!(
                accounts.contains(declaration),
                "missing writable declaration: {declaration}"
            );
        }
    }
    // --- the handler seam, the address pins and the write set --------------------------------

    /// Both bindings feed the position's own signer seeds and the `NftClaimed` payload, and
    /// both are read out of `position` rather than off the accounts that mirror them —
    /// `nft_mint.key()` is equal only because of the `address` pin two tests below, so sourcing
    /// it there would make one guard depend on the other. The mis-sourcing was missed at
    /// baseline.
    #[test]
    fn the_handler_prologue_is_sourced_from_the_position_itself() {
        let flat = flat(CLAIM_SRC);
        assert!(flat.contains("let pool = ctx.accounts.position.pool;"));
        assert!(flat.contains("let nft_mint = ctx.accounts.position.nft_mint;"));
        let prod = production(CLAIM_SRC);
        let prologue =
            &prod[prod.find("pub fn handler").unwrap()..prod.find("let position_seeds").unwrap()];
        assert_eq!(prologue.matches("    let ").count(), 2);
    }

    /// `token_metadata_program` is the program `transfer_pnft` builds its CPI against while the
    /// position PDA signs, so an unpinned address hands a caller-supplied program the position's
    /// signature. All three deletions were missed at baseline.
    #[test]
    fn every_address_pinned_account_carries_its_exact_address() {
        let accounts = accounts_body(CLAIM_SRC);
        for pin in [
            "#[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]",
            "#[account(address = TOKEN_METADATA_ID)]",
            "#[account(address = SYSVAR_INSTRUCTIONS_ID)]",
        ] {
            assert!(accounts.contains(pin), "missing address pin: {pin}");
        }
        assert_eq!(accounts.matches("address = ").count(), 3);
    }

    /// The vault is closed once it is empty. Closing it while it still holds the pNFT
    /// fails the SPL non-empty check, so the order is load-bearing and nothing asserted it.
    #[test]
    fn the_custody_release_precedes_the_vault_close() {
        let prod = production(CLAIM_SRC);
        assert!(prod.find("release_from_vault(").unwrap() < prod.find("close_account(").unwrap());
    }

    /// The counts above assert that named surfaces are absent; this asserts that *no* account field
    /// is written at all, which is the property the handler actually has — the close already
    /// happened in `record_value`, so a write here is either discarded by `close = depositor`
    /// or a re-home of state another handler owns.
    #[test]
    fn the_handler_writes_no_account_field_at_all() {
        for receiver in [
            "ctx.accounts.position",
            "ctx.accounts.position_vault",
            "ctx.accounts.depositor",
            "position",
            "pool",
        ] {
            assert!(
                field_writes(CLAIM_SRC, receiver).is_empty(),
                "{receiver} is written here: {:?}",
                field_writes(CLAIM_SRC, receiver)
            );
        }
    }

    /// `6403` has no emitter anywhere today — the below-floor close succeeds rather than
    /// erroring. `withdraw.rs` pins its own absence; the exit domain's other file needs the same
    /// pin. The two constraints are this instruction's whole error surface.
    #[test]
    fn the_deactivation_code_has_no_emitter_and_the_error_surface_is_exactly_two_constraints() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("LockedDeactivationRejected").count(), 0);
        assert_eq!(prod.matches("PositionLocked").count(), 0);
        // Four, not three: the `standard in {0,1}` constraint is on the accounts struct rather
        // than in the handler precisely so it runs before any CPI, which is why the `require!`
        // count below is still zero.
        assert_eq!(prod.matches("ByeMachineError::").count(), 4);
        assert_eq!(prod.matches("require!").count(), 0);
    }
}
