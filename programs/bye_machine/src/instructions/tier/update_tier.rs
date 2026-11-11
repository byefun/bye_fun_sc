use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::TierChanged;
use crate::common::seeds::{POOL_SEED, POSITION_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED};
use crate::common::tier;
use crate::state::{Pool, Position, ProtocolConfig, TierEntry, TopTier};

pub fn handler<'info>(ctx: Context<'_, '_, 'info, 'info, UpdateTier<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();

    require!(
        tier::is_eligible_candidate(ctx.accounts.position.state, ctx.accounts.position.in_tier),
        ByeMachineError::InvalidPositionState
    );

    let candidate = TierEntry {
        position: position_key,
        value: ctx.accounts.position.recorded_value,
        activated_at: ctx.accounts.position.activated_at,
        position_id: ctx.accounts.position.position_id,
    };

    let acc_tier = ctx.accounts.top_tier.acc_tier;
    let tier_size = ctx.accounts.pool.tier_size;

    let (left, entered) = match tier::cross_into_tier(
        &mut ctx.accounts.top_tier,
        tier_size,
        &mut ctx.accounts.position,
        candidate,
        acc_tier,
    ) {
        tier::CrossOutcome::Inserted => (None, Some(position_key)),
        tier::CrossOutcome::Swapped(evicted) => {
            tier::settle_displaced_member(ctx.remaining_accounts, &evicted, acc_tier)?;
            (Some(evicted.position), Some(position_key))
        }
        tier::CrossOutcome::Rejected => return Err(ByeMachineError::CandidateOutranked.into()),
    };

    emit!(TierChanged {
        pool: pool_key,
        slot: clock.slot,
        left,
        entered,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct UpdateTier<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump
    )]
    pub position: Box<Account<'info, Position>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{accounts_body, calls, field_writes, flat, production};
    use crate::state::position::ALL_POSITION_STATES;
    use crate::state::PositionState;

    const UPDATE_TIER_SRC: &str = include_str!("update_tier.rs");

    const OPERATOR_CONSTRAINT: &str =
        "constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized";

    /// Asserts the handler still performs its three positive obligations, so an absence
    /// assertion below is read against a live body rather than a deleted one.
    fn assert_handler_is_live(source: &str) {
        let prod = production(source);
        let body =
            &prod[prod.find("pub fn handler").unwrap()..prod.find("#[derive(Accounts)]").unwrap()];
        assert_eq!(
            body.matches("tier::cross_into_tier(").count(),
            1,
            "the promotion attempt is absent"
        );
        assert_eq!(
            body.matches("emit!(TierChanged").count(),
            1,
            "the audit event is absent"
        );
        assert_eq!(
            body.matches("tier::is_eligible_candidate(").count(),
            1,
            "the eligibility guard is absent"
        );
    }

    /// The whole handler body as one literal, prologue included, so no statement of any form can
    /// be inserted unnoticed (the arithmetic-needle sweep in `touches_no_weight_surface` is
    /// open-ended and cannot name them all). Update the literal when the body intentionally
    /// changes.
    #[test]
    fn the_handler_body_is_pinned_verbatim_leaving_no_room_for_an_inserted_statement() {
        let body_unchanged = flat(UPDATE_TIER_SRC).contains(
            "pub fn handler<'info>(ctx: Context<'_, '_, 'info, 'info, UpdateTier<'info>>) -> \
             Result<()> { \
             let clock = Clock::get()?; \
             let pool_key = ctx.accounts.pool.key(); \
             let position_key = ctx.accounts.position.key(); \
             require!( \
             tier::is_eligible_candidate(ctx.accounts.position.state, ctx.accounts.position.in_tier), \
             ByeMachineError::InvalidPositionState \
             ); \
             let candidate = TierEntry { \
             position: position_key, \
             value: ctx.accounts.position.recorded_value, \
             activated_at: ctx.accounts.position.activated_at, \
             position_id: ctx.accounts.position.position_id, \
             }; \
             let acc_tier = ctx.accounts.top_tier.acc_tier; \
             let tier_size = ctx.accounts.pool.tier_size; \
             let (left, entered) = match tier::cross_into_tier( \
             &mut ctx.accounts.top_tier, \
             tier_size, \
             &mut ctx.accounts.position, \
             candidate, \
             acc_tier, \
             ) { \
             tier::CrossOutcome::Inserted => (None, Some(position_key)), \
             tier::CrossOutcome::Swapped(evicted) => { \
             tier::settle_displaced_member(ctx.remaining_accounts, &evicted, acc_tier)?; \
             (Some(evicted.position), Some(position_key)) \
             } \
             tier::CrossOutcome::Rejected => return Err(ByeMachineError::CandidateOutranked.into()), \
             }; \
             emit!(TierChanged { \
             pool: pool_key, \
             slot: clock.slot, \
             left, \
             entered, \
             }); \
             Ok(()) \
             }",
        );
        assert!(
            body_unchanged,
            "the handler body changed — a statement was inserted, removed or reordered; if \
             intended, update this literal, do not delete the assertion"
        );
    }

    // --- eligibility, every PositionState variant x both in_tier values --------------------

    /// Enumerated from `ALL_POSITION_STATES`, the list `position.rs` keeps complete, so a new
    /// state variant cannot silently escape the sweep.
    #[test]
    fn eligibility_admits_exactly_one_of_ten_state_x_in_tier_combinations() {
        let admissible = ALL_POSITION_STATES
            .into_iter()
            .flat_map(|state| [(state, false), (state, true)])
            .filter(|(state, in_tier)| tier::is_eligible_candidate(*state, *in_tier))
            .count();
        assert_eq!(
            admissible, 1,
            "exactly Active + not-already-a-member passes"
        );
        assert_eq!(
            ALL_POSITION_STATES.len() * 2,
            10,
            "the product's size is the claim in this test's name"
        );
        assert!(tier::is_eligible_candidate(PositionState::Active, false));
        assert_eq!(calls(UPDATE_TIER_SRC, "tier::is_eligible_candidate"), 1);
        assert!(production(UPDATE_TIER_SRC).contains("ByeMachineError::InvalidPositionState"));
        assert_eq!(u32::from(ByeMachineError::InvalidPositionState), 6301);
    }

    /// The guard's polarity: `require!(!tier::is_eligible_candidate(..), ..)` would admit every
    /// ineligible candidate, so the full guard text is matched with no leading `!`.
    #[test]
    fn the_eligibility_guard_requires_true_not_its_negation() {
        assert!(flat(UPDATE_TIER_SRC).contains(
            "require!( tier::is_eligible_candidate(ctx.accounts.position.state, \
             ctx.accounts.position.in_tier), ByeMachineError::InvalidPositionState );"
        ));
    }

    // --- the Rejected arm converts to an Err at the rank-loss code, not a silent no-op ---------

    #[test]
    fn rejected_outcome_returns_candidate_outranked_rather_than_falling_through() {
        assert_eq!(u32::from(ByeMachineError::CandidateOutranked), 6305);
        assert!(flat(UPDATE_TIER_SRC).contains(
            "tier::CrossOutcome::Rejected => return Err(ByeMachineError::CandidateOutranked.into()),"
        ));
    }

    // --- the Inserted/Swapped arms carry the correct left/entered polarity ---------------------

    #[test]
    fn inserted_arm_reports_no_departure_and_the_candidate_entering() {
        assert!(flat(UPDATE_TIER_SRC)
            .contains("tier::CrossOutcome::Inserted => (None, Some(position_key)),"));
    }

    #[test]
    fn swapped_arm_settles_the_displaced_member_before_reporting_both_sides() {
        assert_eq!(calls(UPDATE_TIER_SRC, "tier::settle_displaced_member"), 1);
        assert!(flat(UPDATE_TIER_SRC).contains(
            "tier::CrossOutcome::Swapped(evicted) => { \
             tier::settle_displaced_member(ctx.remaining_accounts, &evicted, acc_tier)?; \
             (Some(evicted.position), Some(position_key)) }"
        ));
    }

    #[test]
    fn tier_changed_is_emitted_from_a_single_call_site_after_the_match() {
        let prod = production(UPDATE_TIER_SRC);
        assert_eq!(prod.matches("emit!(TierChanged").count(), 1);
        assert!(
            prod.find("emit!(TierChanged").unwrap()
                > prod.find("match tier::cross_into_tier(").unwrap(),
            "the event must follow the outcome match, not precede or straddle it"
        );
    }

    /// The emit site binds `left`/`entered` by shorthand, so a swapped mapping
    /// (`left: entered`) breaks the literal match; the IDL decoder reads these by field name.
    #[test]
    fn tier_changed_binds_left_and_entered_by_shorthand_not_a_swapped_mapping() {
        assert!(flat(UPDATE_TIER_SRC)
            .contains("emit!(TierChanged { pool: pool_key, slot: clock.slot, left, entered, });"));
    }

    /// The emit is unconditional: nothing but whitespace sits between the match's closing `};`
    /// and the emit call, so no wrapping condition can silently drop the audit event.
    #[test]
    fn tier_changed_is_unconditional_immediately_after_the_match_closes() {
        assert!(flat(UPDATE_TIER_SRC).contains("}; emit!(TierChanged {"));
    }

    /// The eligibility guard runs before the promotion attempt. Located by what it checks
    /// (`tier::is_eligible_candidate(`), so a decoy `require!` cannot stand in for it.
    #[test]
    fn the_eligibility_guard_runs_before_the_promotion_attempt() {
        let prod = production(UPDATE_TIER_SRC);
        assert!(
            prod.find("tier::is_eligible_candidate(").unwrap()
                < prod.find("match tier::cross_into_tier(").unwrap(),
            "the eligibility guard must run before the promotion attempt, not after"
        );
    }

    // --- the TierEntry is built from persisted state, never the clock -------------------------

    #[test]
    fn tier_entry_is_built_from_recorded_value_activated_at_and_position_id_in_order() {
        assert!(flat(UPDATE_TIER_SRC).contains(
            "let candidate = TierEntry { \
             position: position_key, \
             value: ctx.accounts.position.recorded_value, \
             activated_at: ctx.accounts.position.activated_at, \
             position_id: ctx.accounts.position.position_id, };"
        ));
    }

    #[test]
    fn clock_is_read_exactly_once_and_never_inside_the_tier_entry() {
        let prod = production(UPDATE_TIER_SRC);
        assert_eq!(prod.matches("Clock::get()").count(), 1);
        let entry_start = prod.find("let candidate = TierEntry {").unwrap();
        let entry_end = prod[entry_start..].find("};").unwrap() + entry_start;
        assert_eq!(
            prod[entry_start..entry_end].matches("clock").count(),
            0,
            "activated_at must never be sourced from Clock::get(), which is not reproducible \
             off-chain"
        );
    }

    // --- the settle/rank inputs are read from their source fields ----------------------------

    #[test]
    fn acc_tier_and_tier_size_are_read_from_the_pinned_source_fields() {
        let prod = production(UPDATE_TIER_SRC);
        assert!(prod.contains("let acc_tier = ctx.accounts.top_tier.acc_tier;"));
        assert!(prod.contains("let tier_size = ctx.accounts.pool.tier_size;"));
    }

    // --- the candidate is bound to the pool by PDA derivation, not a manual key check -------

    #[test]
    fn the_position_is_bound_to_the_pool_by_seeds_not_a_manual_key_check() {
        assert!(accounts_body(UPDATE_TIER_SRC)
            .contains("seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()]"));
        assert_eq!(
            production(UPDATE_TIER_SRC)
                .matches("require_keys_eq!")
                .count(),
            0
        );
    }

    // --- Operator-signed, and no other signer class -------------------------------------------

    #[test]
    fn operator_constraint_present_once_and_no_other_signer_class() {
        let prod = production(UPDATE_TIER_SRC);
        assert_eq!(prod.matches(OPERATOR_CONSTRAINT).count(), 1);
        assert_eq!(prod.matches("administrator").count(), 0);
        assert_eq!(prod.matches("depositor").count(), 0);
        assert_eq!(prod.matches("pub payer").count(), 0);
    }

    // --- the absences, each read against a live handler body ---------------------------------

    #[test]
    fn touches_no_weight_surface() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        let prod = production(UPDATE_TIER_SRC);
        for needle in [
            "weight_index",
            "w_real",
            "n_real",
            "blanks",
            ".insert(",
            ".remove(",
            ".update(",
            "WeightIndex",
            // No arithmetic lives here (it is in common/tier.rs and common/math.rs). One
            // family-generic needle per safety family (`checked_`, `saturating_`, ...) plus the
            // space-padded bare operators, so they don't collide with `->` or lifetimes.
            // Scoped to this file: other handlers carry legitimate bare arithmetic.
            "checked_",
            "saturating_",
            "wrapping_",
            "overflowing_",
            "unchecked_",
            "pow(",
            "abs_diff(",
            " + ",
            " - ",
            " * ",
            " / ",
            " % ",
        ] {
            assert_eq!(prod.matches(needle).count(), 0, "found {needle}");
        }
    }

    /// `update_tier` moves no weight, so it carries no freeze guard.
    #[test]
    fn reads_no_weight_freeze_guard() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        assert_eq!(calls(UPDATE_TIER_SRC, "assert_weight_open"), 0);
    }

    #[test]
    fn reads_no_pause_flag() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        let prod = production(UPDATE_TIER_SRC);
        assert_eq!(prod.matches("deposits_paused").count(), 0);
        assert_eq!(prod.matches("rolls_paused").count(), 0);
    }

    #[test]
    fn no_rebalance_and_no_rebalance_evaluated() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        assert_eq!(calls(UPDATE_TIER_SRC, "rebalance::evaluate"), 0);
        assert_eq!(
            production(UPDATE_TIER_SRC)
                .matches("emit!(RebalanceEvaluated")
                .count(),
            0
        );
    }

    #[test]
    fn no_custody_no_closure_no_cpi() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        let prod = production(UPDATE_TIER_SRC);
        for needle in [
            "transfer_pnft",
            "close_account(",
            "close = ",
            "CpiContext",
            "invoke(",
        ] {
            assert_eq!(prod.matches(needle).count(), 0, "found {needle}");
        }
    }

    #[test]
    fn wallet_stats_untouched() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        let prod = production(UPDATE_TIER_SRC);
        assert_eq!(prod.matches("wallet_stats").count(), 0);
        assert_eq!(prod.matches("active_positions").count(), 0);
        assert_eq!(prod.matches("active_value").count(), 0);
    }

    #[test]
    fn no_admission_recheck_and_no_lock_check() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        let prod = production(UPDATE_TIER_SRC);
        for needle in [
            "admission_floor",
            "admission_ceiling",
            "wallet_value_cap",
            "lock_until",
            "PositionLocked",
        ] {
            assert_eq!(prod.matches(needle).count(), 0, "found {needle}");
        }
    }

    #[test]
    fn no_position_state_or_value_write_and_no_slot_index_touch() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        let prod = production(UPDATE_TIER_SRC);
        assert_eq!(prod.matches("position.state = ").count(), 0);
        assert_eq!(prod.matches("recorded_value = ").count(), 0);
        assert_eq!(prod.matches("slot_index = ").count(), 0);
    }

    #[test]
    fn sweep_and_batch_state_untouched() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        let prod = production(UPDATE_TIER_SRC);
        for needle in [
            "sweep_pending",
            "sweep_epoch",
            "sweep_updates",
            "last_sweep_at",
            "open_batches",
            "owed_fees",
            "acc_equal",
        ] {
            assert_eq!(prod.matches(needle).count(), 0, "found {needle}");
        }
    }

    /// This handler delegates to `cross_into_tier`/`settle_displaced_member` only; the other tier
    /// operations belong to other handlers.
    #[test]
    fn delegates_to_the_checked_tier_operations_only() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        assert_eq!(calls(UPDATE_TIER_SRC, "tier::promote"), 0);
        assert_eq!(calls(UPDATE_TIER_SRC, "tier::demote"), 0);
        assert_eq!(calls(UPDATE_TIER_SRC, "tier::vacate"), 0);
        assert_eq!(calls(UPDATE_TIER_SRC, "tier::refresh_member"), 0);
        assert_eq!(calls(UPDATE_TIER_SRC, "tier::shrink"), 0);
        assert_eq!(calls(UPDATE_TIER_SRC, "tier::cross_into_tier"), 1);
        assert_eq!(calls(UPDATE_TIER_SRC, "tier::settle_displaced_member"), 1);
    }

    /// Every write to `pool`/`top_tier`/`position` belongs to `common/tier.rs`; this file assigns
    /// none of their fields directly, and never indexes `top_tier.entries[` or `pool.blanks[`.
    #[test]
    fn no_direct_field_writes_on_any_account_and_no_indexed_write() {
        assert_handler_is_live(UPDATE_TIER_SRC);
        assert!(field_writes(UPDATE_TIER_SRC, "pool").is_empty());
        assert!(field_writes(UPDATE_TIER_SRC, "top_tier").is_empty());
        assert!(field_writes(UPDATE_TIER_SRC, "position").is_empty());
        assert!(field_writes(UPDATE_TIER_SRC, "ctx.accounts.pool").is_empty());
        assert!(field_writes(UPDATE_TIER_SRC, "ctx.accounts.top_tier").is_empty());
        assert!(field_writes(UPDATE_TIER_SRC, "ctx.accounts.position").is_empty());
        let prod = production(UPDATE_TIER_SRC);
        assert_eq!(prod.matches("top_tier.entries[").count(), 0);
        assert_eq!(prod.matches("pool.blanks[").count(), 0);
    }

    // --- the accounts surface -------------------------------------------------------------------

    #[test]
    fn every_account_is_boxed_every_pda_declares_seeds_and_pool_carries_no_mut() {
        let accounts = accounts_body(UPDATE_TIER_SRC);
        assert_eq!(accounts.matches("Box<Account<").count(), 4);
        assert_eq!(accounts.matches("seeds = [").count(), 4);
        assert!(accounts.contains(OPERATOR_CONSTRAINT));
        assert!(accounts.contains("seeds = [TOP_TIER_SEED, pool.key().as_ref()]"));

        let pool_field = accounts.split("pub pool:").next().unwrap();
        let pool_attr = pool_field.rsplit("#[account(").next().unwrap();
        assert!(
            !pool_attr.contains("mut"),
            "pool is the crate's first read-only Pool — update_tier writes no Pool field"
        );

        assert!(accounts.contains("#[account(\n        mut,\n        seeds = [TOP_TIER_SEED"));
        assert!(accounts.contains("#[account(\n        mut,\n        seeds = [POSITION_SEED"));
    }

    /// Each of the four `#[account(..)]` attribute blocks verbatim: catches a dropped seed
    /// discriminator, a swapped seed constant, or a bump cross-referenced to the wrong account,
    /// none of which the line-level checks above see. Update the literal when the account
    /// surface intentionally changes.
    #[test]
    fn every_account_attribute_block_is_pinned_verbatim() {
        let accounts = accounts_body(UPDATE_TIER_SRC);
        assert!(accounts.contains(
            "#[account(\n        seeds = [PROTOCOL_CONFIG_SEED],\n        bump = protocol_config.bump,\n        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized\n    )]"
        ), "protocol_config's attribute block changed");
        assert!(accounts.contains(
            "#[account(\n        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],\n        bump = pool.bump\n    )]"
        ), "pool's attribute block changed");
        assert!(accounts.contains(
            "#[account(\n        mut,\n        seeds = [TOP_TIER_SEED, pool.key().as_ref()],\n        bump = top_tier.bump\n    )]"
        ), "top_tier's attribute block changed");
        assert!(accounts.contains(
            "#[account(\n        mut,\n        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],\n        bump = position.bump\n    )]"
        ), "position's attribute block changed");
    }

    // --- single exit -----------------------------------------------------------------------------

    #[test]
    fn single_ok_exit_and_the_one_named_early_return() {
        let prod = production(UPDATE_TIER_SRC);
        assert_eq!(prod.matches("Ok(())").count(), 1);
        assert_eq!(
            prod.matches("return ").count(),
            1,
            "the only early return is the Rejected arm's error"
        );
        assert!(prod.contains("=> return Err(ByeMachineError::CandidateOutranked.into()),"));
    }
}
