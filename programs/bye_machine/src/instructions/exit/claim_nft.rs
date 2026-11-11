use anchor_lang::error::ErrorCode;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::ID as SYSVAR_INSTRUCTIONS_ID;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{
    close_account, transfer, CloseAccount, Mint, Token, TokenAccount, Transfer,
};
use mpl_token_metadata::ID as TOKEN_METADATA_ID;

use crate::common::errors::ByeMachineError;
use crate::common::escrow::{release_from_vault, PnftRelease};
use crate::common::events::NftClaimed;
use crate::common::seeds::{POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PRINCIPAL_VAULT_SEED};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::instructions::exit::claim_fee::pay_closed_accrued;
use crate::state::{Pool, Position, PositionState};

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

    // Paid before the account closes over it: once `close = depositor` runs, nothing records
    // the balance.
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
        fees_paid,
    });

    Ok(())
}

/// Depositor-signed: the custody release of a position `record_value` already closed below the
/// floor, paying first the `accrued` the closure stored. No weight, tier or counter surface —
/// every one of those moved when the position was closed; the pool is here only for
/// `owed_fees` and the vault the balance is paid from. Never pause-gated, and it never reaches
/// the weight freeze either: nothing here moves weight.
#[derive(Accounts)]
pub struct ClaimNft<'info> {
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
        // Token Metadata family only; on the accounts struct so it runs before any CPI, as in
        // the other exits.
        constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)
            @ ByeMachineError::WrongStandardForInstruction,
        // One terminal: `Seized` is written by `close_seized` alone, which requires
        // `standard == 2`, so a `Seized` arm here could never run.
        //
        // The Token Metadata family has no seizure terminal, which is a recorded gap: a Metaplex
        // rule-set revision that denies the exit (none of the live ones does) would strand a pNFT
        // with weight behind it, and nothing could clear it, since `close_seized` refuses the
        // standard and widening it needs a new release leg and account list. This needs a decision
        // before this family goes to mainnet.
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

    /// Asserts the handler still performs its four positive obligations, so the absence
    /// assertions in this file are read against a live body: a gutted handler would satisfy
    /// "does not touch the tier" as well as a correct one. Callers still count absences over the
    /// whole production half.
    fn assert_handler_is_live(source: &str) {
        let prod = production(source);
        let body =
            &prod[prod.find("pub fn handler").unwrap()..prod.find("#[derive(Accounts)]").unwrap()];
        assert_eq!(
            body.matches("pay_closed_accrued(").count(),
            1,
            "accrued payout absent"
        );
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

    /// `ClosedBelowFloor` is the only admitted state; the other four reject at 6301, enumerated
    /// from `ALL_POSITION_STATES` so a new variant cannot escape the case.
    ///
    /// `Seized` is not admitted: its only writer, `close_seized`, requires `standard == 2`, which
    /// this instruction refuses, so the arm could never run. `claim_nft_core` admits both
    /// terminals, and the contrast is asserted.
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

    /// Depositor-signed: `return_rejected`'s permissionless `payer` + `UncheckedAccount` shape
    /// must not be copied here, so it is asserted absent.
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
    /// position. Touching any of them here would move them a second time. The stored balance is
    /// reached only through `pay_closed_accrued`, which settles no accumulator leg.
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
            "acc_equal",
            "acc_tier",
            "position_fee_payout",
            "apply_claim_fee",
        ] {
            assert_eq!(
                prod.matches(surface).count(),
                0,
                "{surface} is not this handler's — the close already ran"
            );
        }
    }

    /// The pool is writable for `owed_fees` and seeds both the position and the principal
    /// vault, so a caller cannot pay this position out of another pool's vault or discharge
    /// another pool's debt. No `protocol_config`: nothing here reads configuration.
    #[test]
    fn the_position_and_the_principal_vault_are_bound_through_the_supplied_pool() {
        let accounts = accounts_body(CLAIM_SRC);
        assert_eq!(
            accounts
                .matches("pub pool: Box<Account<'info, Pool>>")
                .count(),
            1
        );
        assert!(accounts.contains(
            "mut,\n        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],\n        bump = pool.bump"
        ));
        assert!(accounts
            .contains("seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],"));
        assert!(accounts.contains("seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],"));
        assert_eq!(
            accounts.matches("position.pool.as_ref()").count(),
            0,
            "a self-anchored position would let the caller pick the paying pool"
        );
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("pub protocol_config:").count(), 0);
        assert_eq!(prod.matches("PROTOCOL_CONFIG_SEED").count(), 0);
    }

    // --- the emit surface ---------------------------------------------------------------------

    /// `RebalanceEvaluated` belongs only where the blank set is re-evaluated; releasing custody
    /// of an already-closed position evaluates nothing.
    #[test]
    fn emits_nft_claimed_once_carrying_the_mint_and_the_fees_paid() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("emit!(").count(), 1);
        assert_eq!(prod.matches("emit!(NftClaimed").count(), 1);
        assert!(flat(CLAIM_SRC).contains(
            "emit!(NftClaimed { pool, slot: Clock::get()?.slot, \
             position: ctx.accounts.position.key(), \
             depositor: ctx.accounts.depositor.key(), nft_mint, fees_paid, });"
        ));
        assert_eq!(prod.matches("RebalanceEvaluated").count(), 0);
        assert_eq!(prod.matches("TierChanged").count(), 0);
    }

    // --- the CPI surfaces ---------------------------------------------------------------------

    /// `release_from_vault` takes 13 positional `&AccountInfo` of identical type, so a swap
    /// compiles; the order is asserted. The direction is vault to depositor, and the `payer` slot
    /// is the depositor. The five pNFT accounts are grouped in `PnftRelease`, whose named fields
    /// cannot be transposed.
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

    /// Both rent refunds go to the position's own depositor.
    #[test]
    fn the_position_closes_to_the_depositor_and_to_nobody_else() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("close = ").count(), 1);
        assert!(prod.contains("close = depositor,"));
        assert_eq!(prod.matches("close = payer").count(), 0);
        assert_eq!(prod.matches("close = signer").count(), 0);
    }

    // --- the account surface and the accrued payout -------------------------------------------

    #[test]
    fn every_state_account_is_boxed_and_every_pda_declares_its_seeds() {
        let prod = production(CLAIM_SRC);
        // pool, position, position_vault, principal_vault, depositor_usdc, nft_mint.
        assert_eq!(prod.matches("Box<Account<").count(), 6);
        assert_eq!(prod.matches("seeds = [").count(), 4);
        assert!(prod.contains("seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()]"));
        assert!(prod
            .contains("seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()]"));
        assert!(prod.contains("seeds = [POSITION_VAULT_SEED, position.key().as_ref()]"));
        assert!(prod.contains("seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()]"));
        assert!(prod.contains("bump = pool.bump"));
        assert!(prod.contains("bump = position.bump,"));
        assert!(prod.contains("bump = position.vault_bump"));
        // Five optional accounts; the count keeps an account from quietly becoming optional.
        assert_eq!(prod.matches("Option<UncheckedAccount<'info>>").count(), 5);
    }

    #[test]
    fn handler_has_exactly_one_ok_and_zero_return_statements() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("Ok(())").count(), 1);
        assert_eq!(prod.matches("return ").count(), 0);
    }

    /// The closure's stored `accrued` is paid out of the pool's principal vault, under the
    /// pool's signature, to the depositor's own USDC account, through the same shared function
    /// the fee claims use. Nothing owed moves nothing and needs no USDC account; owed without
    /// one fails before any CPI rather than closing the position over the balance.
    #[test]
    fn the_stored_accrual_is_paid_to_the_depositor_before_anything_else_moves() {
        assert_handler_is_live(CLAIM_SRC);
        let flat = flat(CLAIM_SRC);
        assert_eq!(calls(CLAIM_SRC, "pay_closed_accrued"), 1);
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
        assert!(flat.contains(
            "let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];"
        ));
        assert_eq!(calls(CLAIM_SRC, "transfer"), 1);
        let prod = production(CLAIM_SRC);
        let paid = prod.find("pay_closed_accrued(").unwrap();
        assert!(paid < prod.find("release_from_vault(").unwrap());
        assert!(paid < prod.find("close_account(").unwrap());
    }

    /// The same mint and owner pins `claim_fee` puts on its `depositor_usdc`, so the balance can
    /// land only in the depositor's own USDC account.
    #[test]
    fn the_usdc_account_is_optional_and_pinned_to_the_depositor_and_the_vault_mint() {
        let accounts = accounts_body(CLAIM_SRC);
        assert!(accounts.contains("pub depositor_usdc: Option<Box<Account<'info, TokenAccount>>>"));
        assert!(accounts.contains(
            "constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted"
        ));
        assert!(accounts.contains(
            "constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
        assert_eq!(u32::from(ErrorCode::AccountNotEnoughKeys), 3005);
    }

    /// No account here is mutated Rust-side (every write goes through a CPI), so the crate-wide
    /// `mut` enumerator cannot see this file; the `mut` declarations are pinned by exact count.
    /// `position` is also covered by Anchor, which rejects `close` without `mut`.
    #[test]
    fn every_writable_account_declaration_is_pinned_by_count() {
        let accounts = accounts_body(CLAIM_SRC);
        assert_eq!(accounts.matches("mut").count(), 10);
        for declaration in [
            "#[account(mut)]\n    pub depositor: Signer<'info>",
            "mut,\n        seeds = [POOL_SEED",
            "mut,\n        seeds = [PRINCIPAL_VAULT_SEED",
            "mut,\n        constraint = depositor_usdc.mint",
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

    /// Both bindings feed the position's own signer seeds and the `NftClaimed` payload and are
    /// read out of `position`, so one guard does not depend on the `address` pin below.
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
    /// signature.
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

    /// The vault is closed once empty: closing it while it still holds the pNFT fails the SPL
    /// non-empty check.
    #[test]
    fn the_custody_release_precedes_the_vault_close() {
        let prod = production(CLAIM_SRC);
        assert!(prod.find("release_from_vault(").unwrap() < prod.find("close_account(").unwrap());
    }

    /// No account field is written in this file: the only writes, `accrued` and `owed_fees`,
    /// happen inside `pay_closed_accrued`, and any other write would be discarded by
    /// `close = depositor` or re-home state another handler owns.
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

    /// `6403` has no emitter here. The position's three constraints and the USDC account's two
    /// are this instruction's own error surface, with `AccountNotEnoughKeys` for an owed balance
    /// and no account to pay it to.
    #[test]
    fn the_deactivation_code_has_no_emitter_and_the_error_surface_is_exactly_two_constraints() {
        let prod = production(CLAIM_SRC);
        assert_eq!(prod.matches("LockedDeactivationRejected").count(), 0);
        assert_eq!(prod.matches("PositionLocked").count(), 0);
        // The `standard in {0,1}` constraint is on the accounts struct so it runs before any CPI.
        assert_eq!(prod.matches("ByeMachineError::").count(), 6);
        assert_eq!(prod.matches("ErrorCode::").count(), 1);
        assert_eq!(prod.matches("require!").count(), 0);
    }
}
