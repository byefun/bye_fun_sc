use anchor_lang::prelude::*;

use crate::common::activation::{activate_position, ActivationAccounts, ActivationTerms};
use crate::common::errors::ByeMachineError;
use crate::common::events::{DepositApproved, RebalanceEvaluated, TierChanged};
use crate::common::guards::{
    assert_not_acquired_position, assert_weight_open, validate_admission_limits,
};
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED, WALLET_STATS_SEED,
};
use crate::state::{
    Pool, Position, PositionState, ProtocolConfig, TopTier, WalletStats, WeightIndex,
};

/// `lock_until` is the Season-0 lock, supplied by the attestor as `season.tge_at + 14 days` for
/// a Season-0 deposit and `0` otherwise. It is the only writer of this field. The lock gates
/// departure the depositor chooses, and nothing else: `withdraw` guards on `now >= lock_until`,
/// and `close_seized` guards its `Active` row on every seizure cause a depositor could arrange
/// while still ending up with the card. `record_value` does not read it — a position below the
/// admission floor closes on the sweep that observes it, lock or no lock.
///
/// Taken as drawn and **not validated** — no bound, and no comparison against `observed_at` or
/// `now`. The Operator can therefore lock a position arbitrarily far out; that is not a new
/// authority, since the same signer already attests `value` and so decides admission, weight,
/// tier rank and payout on this very call, but nothing on chain bounds it.
///
/// `observed_at` and `lock_until` are adjacent `i64`s. Transposing them sets the lock to the
/// attestation timestamp, which is no lock at all, and no IDL comparison can see it: it
/// compares an encoder against an IDL generated from this same source. Do not reorder these
/// parameters or those two assignments.
pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, ApproveDeposit<'info>>,
    value: u64,
    observed_at: i64,
    lock_until: i64,
) -> Result<()> {
    require!(
        ctx.accounts.position.state == PositionState::Pending,
        ByeMachineError::PositionNotPending
    );
    assert_not_acquired_position(ctx.accounts.position.recorded_value)?;
    assert_weight_open(&ctx.accounts.pool)?;
    validate_admission_limits(
        &ctx.accounts.pool,
        value,
        ctx.accounts.wallet_stats.active_value,
    )?;

    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let authority = ctx.accounts.operator.key();

    let mut weight_index = ctx.accounts.weight_index.load_mut()?;
    let activation = activate_position(
        ActivationAccounts {
            pool: &mut ctx.accounts.pool,
            weight_index: &mut weight_index,
            top_tier: &mut ctx.accounts.top_tier,
            position: &mut ctx.accounts.position,
            position_key,
            wallet_stats: &mut ctx.accounts.wallet_stats,
            remaining_accounts: ctx.remaining_accounts,
        },
        ActivationTerms {
            value,
            observed_at,
            lock_until,
            now,
        },
    )?;

    // `lock_until` is deliberately not an event field: it is account-sourced and write-once, so
    // the event would be a second source for a value the account holds. Off-chain readers read it
    // off `Position`.
    emit!(DepositApproved {
        pool: pool_key,
        slot: clock.slot,
        authority,
        position: position_key,
        value,
        observed_at,
        observed_age_seconds: now
            .checked_sub(observed_at)
            .ok_or(ByeMachineError::ArithmeticFailure)?,
    });
    emit!(RebalanceEvaluated {
        pool: pool_key,
        slot: clock.slot,
        n_real: ctx.accounts.pool.n_real,
        w_real: ctx.accounts.pool.w_real,
        blanks_before: activation.blanks_before,
        blanks_after: ctx.accounts.pool.blanks,
    });
    if activation.entered_tier {
        emit!(TierChanged {
            pool: pool_key,
            slot: clock.slot,
            left: activation.displaced,
            entered: Some(position_key),
        });
    }

    Ok(())
}

#[derive(Accounts)]
pub struct ApproveDeposit<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- structural pins over the four deposit-domain handlers -------------------------------

    const DEPOSIT_SRC: &str = include_str!("deposit.rs");
    const APPROVE_SRC: &str = include_str!("approve_deposit.rs");
    const REJECT_SRC: &str = include_str!("reject_deposit.rs");
    const RETURN_SRC: &str = include_str!("return_rejected.rs");

    use crate::common::test_support::{calls, flat, production, writes};

    const OPERATOR_CONSTRAINT: &str =
        "constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized";

    /// The activation field sweep lives in `common/activation.rs`, which drives the real
    /// function. What stays here is that the handler reaches the shared activation exactly once
    /// and publishes the two conditional events off that call's outcome. Deleting the
    /// `activate_position` call would leave a handler that emits `DepositApproved` for a position
    /// it never activated.
    #[test]
    fn approve_deposit_reaches_the_shared_activation_exactly_once() {
        assert_eq!(calls(APPROVE_SRC, "activate_position"), 1);
        assert_eq!(calls(APPROVE_SRC, "validate_admission_limits"), 1);

        let prod = production(APPROVE_SRC);
        assert_eq!(
            prod.matches("emit!(RebalanceEvaluated").count(),
            1,
            "one activation publishes one rebalance"
        );
        assert!(
            prod.contains("if activation.entered_tier {"),
            "TierChanged must be conditional on the activation's own outcome, not on a local              the handler recomputes"
        );
        assert!(
            prod.contains("blanks_before: activation.blanks_before"),
            "blanks_before must come off the activation — it is gone from the pool by the time              the event is built"
        );
    }

    /// `(value: u64, observed_at: i64, lock_until: i64)` ends in two adjacent `i64`s, so a
    /// transposition compiles and encodes to the same 24 arg bytes. The IDL oracle cannot see
    /// it: it compares `scripts/lib/deposit.ts` against an IDL generated from this file, so a
    /// swap moves both sides together.
    ///
    /// This pins the entrypoint's order, the wire ABI clients encode against. Past it the values
    /// are forwarded into `ActivationTerms`' named fields, where a transposition cannot be
    /// written.
    #[test]
    fn approve_deposit_declares_its_two_i64_args_in_wire_order() {
        let src = production(APPROVE_SRC);

        assert!(
            src.contains("    value: u64,\n    observed_at: i64,\n    lock_until: i64,\n"),
            "approve_deposit's parameter order moved. It must stay (value, observed_at, \
             lock_until): the two i64s are transposable with no width change and no compile error"
        );

        assert!(
            src.contains("            observed_at,\n            lock_until,\n"),
            "the two arguments must reach ActivationTerms by their own names — a rewrite to \
             positional or renamed bindings reopens the transposition this pin closes"
        );
    }

    /// `DepositApproved` must not gain a `lock_until` field: it is account-sourced and
    /// write-once, and off-chain readers read it off the `Position` account.
    #[test]
    fn deposit_approved_does_not_publish_the_lock() {
        let emit_site = production(APPROVE_SRC)
            .split("emit!(DepositApproved {")
            .nth(1)
            .expect("approve_deposit must emit DepositApproved")
            .split("});")
            .next()
            .expect("the DepositApproved emit must be terminated")
            .to_string();

        // Positive control: assert a field that is published before asserting the one that is
        // not.
        assert!(
            emit_site.contains("observed_age_seconds"),
            "the extracted span is not DepositApproved's emit body, so its absence claim below \
             would be vacuous — the split markers moved"
        );

        assert!(
            !emit_site.contains("lock_until"),
            "DepositApproved gained a lock_until field — off-chain readers read the value off the \
             Position account instead, and publishing it here duplicates a write-once source"
        );
    }

    /// The fields every later instruction re-derives or ranks by. Unwritten,
    /// `position_id` degenerates the tier tie-break and `depositor` sends a returned NFT to
    /// the default pubkey. The zero-init-duplicating writes (`state = Pending`, `accrued`,
    /// `lock_until`, `reject_reason`) are deliberately not pinned.
    #[test]
    fn deposit_writes_every_position_identity_field() {
        for field in [
            "position.pool",
            "position.depositor",
            "position.nft_mint",
            "position.position_id",
            "position.bump",
            "position.vault_bump",
        ] {
            assert_eq!(writes(DEPOSIT_SRC, field), 1, "{field} is not assigned");
        }
        for field in [
            "wallet_stats.pool",
            "wallet_stats.owner",
            "wallet_stats.bump",
        ] {
            assert_eq!(writes(DEPOSIT_SRC, field), 1, "{field} is not assigned");
        }
        assert_eq!(writes(DEPOSIT_SRC, "ctx.accounts.pool.position_counter"), 1);
    }

    /// Without both writes the position stays `Pending` while `DepositRejected` announces it
    /// rejected — and `return_rejected` requires `Rejected`, so the NFT never comes back.
    #[test]
    fn reject_deposit_records_the_rejection_it_announces() {
        assert!(production(REJECT_SRC).contains("position.state = PositionState::Rejected"));
        assert_eq!(writes(REJECT_SRC, "position.reject_reason"), 1);
    }

    /// `validate_admission` and `validate_admission_limits` are `pub` with other call sites, so
    /// deleting either call compiles clean and silently admits any NFT of any collection and
    /// standard.
    #[test]
    fn deposit_runs_the_on_chain_admission_gate() {
        assert_eq!(calls(DEPOSIT_SRC, "validate_admission"), 1);
    }

    /// `deposit` stores the standard it resolved rather than a literal: a hard-coded
    /// `position.standard = STANDARD_PNFT` is still one write, but wrong once a second standard
    /// is admitted.
    #[test]
    fn deposit_stores_the_resolved_standard_rather_than_a_literal() {
        assert!(production(DEPOSIT_SRC).contains("position.standard = standard;"));
    }

    /// The transfer branch's both halves. The absent half has no other instrument:
    /// `transfer_spl` ignores the five pNFT accounts, so a legacy deposit supplying a token
    /// record would succeed with the account carried unread. Five `is_none()` guards, counted,
    /// because four would leave one account unchecked.
    ///
    /// The codes are Anchor's own: `ConstraintRaw` for supplied-when-absent, and
    /// `AccountNotEnoughKeys` for the three accounts the pNFT leg needs. Counted, because
    /// dropping one `ok_or` leaves the pNFT branch a silent `None` deref site.
    #[test]
    fn the_transfer_branch_requires_the_five_pnft_accounts_present_and_absent_by_standard() {
        let prod = production(DEPOSIT_SRC);
        assert_eq!(calls(DEPOSIT_SRC, "transfer_pnft"), 1);
        assert_eq!(calls(DEPOSIT_SRC, "transfer_spl"), 1);
        for account in [
            "depositor_token_record",
            "position_token_record",
            "sysvar_instructions",
            "authorization_rules_program",
            "authorization_rules",
        ] {
            assert_eq!(
                flat(DEPOSIT_SRC)
                    .matches(&format!(
                        "require!( ctx.accounts.{account}.is_none(), ErrorCode::ConstraintRaw );"
                    ))
                    .count(),
                1,
                "{account} is not required-absent on the legacy branch"
            );
        }
        assert_eq!(prod.matches("ErrorCode::ConstraintRaw").count(), 5);
        assert_eq!(prod.matches("ErrorCode::AccountNotEnoughKeys").count(), 3);
        assert!(flat(DEPOSIT_SRC).contains("match standard { STANDARD_PNFT =>"));
        assert!(flat(DEPOSIT_SRC).contains("STANDARD_LEGACY => {"));
    }

    /// `transfer_pnft`'s 16 arguments and `transfer_spl`'s four are pinned as ordered literals:
    /// they are same-typed positionals, so `from`/`to` transposed reverses custody and
    /// `authority`/`token_program` transposed hands the CPI the wrong signer.
    #[test]
    fn deposits_two_transfer_legs_pin_their_own_argument_order() {
        assert!(flat(DEPOSIT_SRC).contains(
            "transfer_pnft( &ctx.accounts.depositor_token.to_account_info(), \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.position_vault.to_account_info(), \
             &ctx.accounts.position.to_account_info(), \
             &ctx.accounts.nft_mint.to_account_info(), &ctx.accounts.metadata, \
             &ctx.accounts.master_edition, depositor_token_record, position_token_record, \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.system_program.to_account_info(), sysvar_instructions, \
             &ctx.accounts.token_program.to_account_info(), \
             &ctx.accounts.associated_token_program.to_account_info(), \
             &ctx.accounts.token_metadata_program, \
             ctx.accounts.authorization_rules_program.as_deref(), \
             ctx.accounts.authorization_rules.as_deref(), &[], )?;"
        ));
        assert!(flat(DEPOSIT_SRC).contains(
            "transfer_spl( &ctx.accounts.depositor_token.to_account_info(), \
             &ctx.accounts.position_vault.to_account_info(), \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.token_program.to_account_info(), &[], )?;"
        ));
    }

    /// The arm the `u8` match forces. It is unreachable while `resolve_standard` returns 0 or 1
    /// and must reject rather than fall through, which would open a `Position` and emit
    /// `DepositPending` over an escrow that never happened.
    #[test]
    fn the_unreachable_standard_arm_rejects_rather_than_falling_through() {
        assert!(flat(DEPOSIT_SRC)
            .contains("_ => return Err(ByeMachineError::WrongStandardForInstruction.into()),"));
    }

    /// Without this call the `Position` is created and `DepositPending` emitted
    /// while the NFT never leaves the depositor, who can then sell it out from under a pool
    /// that believes it holds the escrow.
    #[test]
    fn deposit_escrows_the_nft_before_opening_a_position() {
        assert_eq!(calls(DEPOSIT_SRC, "transfer_pnft"), 1);
        // `return_rejected` reaches its transfer through the shared standard-dispatch branch, so
        // the leg follows the *stored* standard; a direct `transfer_pnft` would send a legacy
        // card down the pNFT path.
        assert_eq!(calls(RETURN_SRC, "release_from_vault"), 1);
        assert_eq!(calls(RETURN_SRC, "transfer_pnft"), 0);
    }
    /// The Operator-only pair carries the signer constraint; the permissionless pair carries no
    /// authority binding at all. Both halves matter: a constraint added to `return_rejected`
    /// would break access control as surely as one dropped from `approve_deposit`.
    /// `return_rejected` would break access control as surely as one dropped from `approve_deposit`.
    #[test]
    fn the_authority_matrix_is_operator_only_on_approve_and_reject() {
        for source in [APPROVE_SRC, REJECT_SRC] {
            assert_eq!(production(source).matches(OPERATOR_CONSTRAINT).count(), 1);
        }
        for source in [DEPOSIT_SRC, RETURN_SRC] {
            assert_eq!(production(source).matches("Unauthorized").count(), 0);
        }
        // `deposit` reads no configuration. `return_rejected` reads it only for the T02
        // authority's address, which fixes where a Treasury 02 card goes; it binds no signer to it.
        assert_eq!(
            production(DEPOSIT_SRC).matches("protocol_config").count(),
            0
        );
        assert!(production(RETURN_SRC)
            .contains("seeds = [PROTOCOL_CONFIG_SEED], bump = protocol_config.bump)]"));
        assert_eq!(production(RETURN_SRC).matches(": Signer<'info>").count(), 1);
    }

    /// Every state transition is gated on the state it leaves. Without these the
    /// tree takes a second `insert` for one position, or a live NFT walks out of escrow.
    #[test]
    fn every_transition_out_of_a_state_is_gated_on_that_state() {
        for source in [APPROVE_SRC, REJECT_SRC] {
            assert!(production(source).contains("position.state == PositionState::Pending"));
            assert!(production(source).contains("ByeMachineError::PositionNotPending"));
        }
        assert!(production(RETURN_SRC).contains(
            "constraint = position.state == PositionState::Rejected @ ByeMachineError::InvalidPositionState"
        ));
    }

    /// The `WeightIndex` is a bare keypair account, not a PDA: `AccountLoader` checks only owner
    /// and discriminator, so another pool's index would load cleanly. Only this address binding
    /// stops it, and `insert`'s desync guard cannot — it validates against the caller's own
    /// `expected_total`.
    #[test]
    fn the_weight_index_is_pinned_to_the_pools_own_account() {
        assert!(production(APPROVE_SRC).contains("#[account(mut, address = pool.weight_index)]"));
        assert!(
            production(APPROVE_SRC).contains("pub weight_index: AccountLoader<'info, WeightIndex>")
        );
    }

    /// `return_rejected` is permissionless, so nothing but this constraint stops a caller from
    /// naming itself as the destination of the NFT and both rent refunds.
    #[test]
    fn return_rejected_pins_the_destination_to_the_recorded_depositor() {
        assert!(flat(RETURN_SRC).contains(
            "constraint = card_recipient(&position.pool, &position.depositor, &protocol_config.t02_authority) == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
    }

    /// The pause flag gates entry only. Approval, rejection and the return of an
    /// already-escrowed NFT must all stay reachable while deposits are paused.
    #[test]
    fn only_deposit_is_gated_on_the_deposits_paused_flag() {
        assert!(production(DEPOSIT_SRC).contains("!ctx.accounts.pool.deposits_paused"));
        assert!(production(DEPOSIT_SRC).contains("ByeMachineError::DepositsPaused"));
        for source in [APPROVE_SRC, REJECT_SRC, RETURN_SRC] {
            assert_eq!(production(source).matches("deposits_paused").count(), 0);
        }
    }

    /// The vault holds the escrowed NFT under the *position PDA's* authority.
    /// A vault minted to the depositor's authority would leave custody with the depositor
    /// while the program believed the NFT was escrowed.
    #[test]
    fn the_escrow_vault_is_authorised_by_the_position_pda() {
        assert!(production(DEPOSIT_SRC).contains("token::mint = nft_mint"));
        assert!(production(DEPOSIT_SRC).contains("token::authority = position"));
    }

    /// Both CPI-performing handlers pin the programs they invoke through. Unpinned, a caller
    /// substitutes its own program for Token Metadata: the `TransferV1` CPI succeeds without
    /// moving anything, and `deposit` still creates a `Position` over an NFT never escrowed.
    #[test]
    fn both_cpi_handlers_pin_token_metadata_and_the_instructions_sysvar() {
        for source in [DEPOSIT_SRC, RETURN_SRC] {
            assert!(production(source).contains("#[account(address = TOKEN_METADATA_ID)]"));
            assert!(production(source).contains("#[account(address = SYSVAR_INSTRUCTIONS_ID)]"));
            assert!(production(source).contains("pub token_program: Program<'info, Token>"));
            assert!(production(source)
                .contains("pub associated_token_program: Program<'info, AssociatedToken>"));
            assert!(production(source).contains("pub system_program: Program<'info, System>"));
        }
    }

    /// The NFT leaving the depositor must be the mint the position is opened over, held by
    /// the signer — not some third party's account that happens to be writable.
    #[test]
    fn deposit_pins_the_source_token_account_to_the_mint_and_the_depositor() {
        assert!(production(DEPOSIT_SRC).contains(
            "constraint = depositor_token.mint == nft_mint.key() @ ByeMachineError::StandardNotAdmitted"
        ));
        assert!(production(DEPOSIT_SRC).contains(
            "constraint = depositor_token.owner == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
    }

    /// `return_rejected` is permissionless and reads its mint from an account the caller
    /// supplies, so the mint is pinned to the one the position recorded at deposit.
    #[test]
    fn return_rejected_pins_the_mint_to_the_positions_recorded_mint() {
        assert!(production(RETURN_SRC).contains(
            "#[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]"
        ));
        assert!(production(RETURN_SRC)
            .contains("seeds = [POSITION_VAULT_SEED, position.key().as_ref()]"));
    }

    /// Every account in the domain that is a PDA declares its seeds. A dropped `seeds =`
    /// turns a derived account into one the caller chooses freely, and Anchor reports nothing.
    #[test]
    fn every_pda_in_the_deposit_domain_declares_its_seeds() {
        let expected = [
            (DEPOSIT_SRC, 4),
            (APPROVE_SRC, 5),
            (REJECT_SRC, 2),
            (RETURN_SRC, 3),
        ];
        for (source, count) in expected {
            assert_eq!(production(source).matches("seeds = [").count(), count);
        }
        for source in [APPROVE_SRC, REJECT_SRC] {
            assert!(production(source).contains("seeds = [PROTOCOL_CONFIG_SEED]"));
        }
        for source in [DEPOSIT_SRC, APPROVE_SRC] {
            assert!(production(source)
                .contains("seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()]"));
        }
    }

    /// The position and its vault derive from the mint and the position, so a
    /// re-deposit of the same mint lands on the same two addresses by construction.
    #[test]
    fn the_deposit_domain_pdas_derive_from_their_declared_seeds() {
        let position_seeds =
            "seeds = [POSITION_SEED, pool.key().as_ref(), nft_mint.key().as_ref()]";
        assert!(production(DEPOSIT_SRC).contains(position_seeds));
        assert!(production(DEPOSIT_SRC)
            .contains("seeds = [POSITION_VAULT_SEED, position.key().as_ref()]"));
        assert!(production(DEPOSIT_SRC).contains(
            "seeds = [WALLET_STATS_SEED, pool.key().as_ref(), depositor.key().as_ref()]"
        ));
        assert!(production(APPROVE_SRC).contains(
            "seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()]"
        ));
        assert!(production(APPROVE_SRC).contains("seeds = [TOP_TIER_SEED, pool.key().as_ref()]"));
        for source in [APPROVE_SRC, REJECT_SRC, RETURN_SRC] {
            assert_eq!(
                production(source)
                    .matches("seeds = [POSITION_SEED, ")
                    .count(),
                1
            );
        }
    }

    /// Both Operator-driven transitions out of `Pending` refuse a position whose depositor is
    /// the treasury_02 PDA and carries the value `instant_sell` stored.
    #[test]
    fn approve_and_reject_both_call_the_treasury_exclusion() {
        assert_eq!(calls(APPROVE_SRC, "assert_not_acquired_position"), 1);
        assert_eq!(calls(REJECT_SRC, "assert_not_acquired_position"), 1);
        for source in [DEPOSIT_SRC, RETURN_SRC] {
            assert_eq!(calls(source, "assert_not_acquired_position"), 0);
        }
    }

    /// One event per handler, each in its own handler and no other.
    #[test]
    fn each_deposit_handler_emits_exactly_its_own_event() {
        let map = [
            (DEPOSIT_SRC, "DepositPending"),
            (APPROVE_SRC, "DepositApproved"),
            (REJECT_SRC, "DepositRejected"),
            (RETURN_SRC, "NftReturned"),
        ];
        for (source, event) in map {
            assert_eq!(
                production(source)
                    .matches(&format!("emit!({event}"))
                    .count(),
                1
            );
            for (other, _) in map {
                if !std::ptr::eq(other, source) {
                    assert_eq!(
                        production(other).matches(&format!("emit!({event}")).count(),
                        0
                    );
                }
            }
        }
    }

    /// The position and vault PDAs both re-derive on a re-deposit of the same mint, so an
    /// emptied-but-surviving vault would make that re-deposit permanently unsatisfiable.
    #[test]
    fn return_rejected_closes_the_vault_and_refunds_both_rents_to_the_depositor() {
        assert_eq!(calls(RETURN_SRC, "close_account"), 1);
        assert!(production(RETURN_SRC)
            .contains("destination: ctx.accounts.depositor.to_account_info()"));
        assert!(production(RETURN_SRC).contains("close = depositor"));
        assert!(!production(RETURN_SRC).contains("close = payer"));
    }

    /// `deposit` creates `WalletStats` but never resets its counters on a repeat deposit.
    #[test]
    fn deposit_never_resets_the_wallet_stats_counters_on_a_repeat_deposit() {
        let source = production(DEPOSIT_SRC);
        assert!(!source.contains("active_positions ="));
        assert!(!source.contains("active_value ="));
    }

    /// The 3xxx codes the client checks by number; `u32::from` alone would not pin them.
    #[test]
    fn missing_and_read_only_remaining_accounts_return_anchor_3005_and_3006() {
        assert_eq!(u32::from(ErrorCode::AccountNotEnoughKeys), 3005);
        assert_eq!(u32::from(ErrorCode::AccountNotMutable), 3006);
    }
}
