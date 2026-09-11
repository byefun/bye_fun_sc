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

    /// Asserts the handler still performs its three positive obligations. Every `x0` assertion
    /// below has an inverted failure mode — a deleted or gutted handler satisfies "does not
    /// touch the tier" exactly as a correct one does — so an absence is only evidence when read
    /// against a live body. The `claim_nft.rs` `assert_handler_is_live` pattern.
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

    /// The arithmetic-needle sweep (`touches_no_weight_surface`) is a safety-family
    /// closure over an open-ended operation dimension — `pow`, `abs_diff`, `div_euclid`,
    /// `rem_euclid`, `isqrt`, `midpoint`, `ilog2`, `signum`, the bitwise operators, and whatever
    /// `core`/`std` adds next. A 21st needle buys the same false confidence the 20th did — that
    /// list has been wrong before. The
    /// fault class underneath every one of them is not "an operator appeared" — it is "a
    /// statement was added to a fully-pinned body," and that closes form-independently and
    /// exhaustively by construction if the body has no room left for one. Every statement here
    /// except the three prologue `let`s is already pinned by a dedicated test below; this pins
    /// the whole body, those three included, as one literal — the same whole-block technique
    /// `every_account_attribute_block_is_pinned_verbatim` uses for the accounts struct. Any
    /// inserted statement, of any form the needle sweep can or cannot name, breaks this match.
    ///
    /// **When this fails:** update the literal to match the handler's new text — never delete or
    /// weaken this assertion. A failure here means the body changed in a way none of the
    /// per-statement pins below caught on their own; confirm the change is intended (usually it
    /// is, on a real handler edit) before updating the literal.
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

    /// **Enumerated from `ALL_POSITION_STATES`, which is the list `position.rs` guards, because
    /// this test used to say that and be listed by eye.** It carried four variants and the
    /// promise that "a fifth variant added later fails to compile rather than escaping the
    /// sweep" — a promise a hand-written array cannot keep, and did not: `Seized` arrived and the
    /// product silently stayed at eight combinations instead of ten, with the one state a Core
    /// position ends in never asked. The list is now shared, and
    /// `position_state_rejects_out_of_range_discriminant` is what reds when a variant is added
    /// without joining it.
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

    /// Every existing pin above counts occurrences of
    /// `tier::is_eligible_candidate(` and never inspects the guard's polarity, so
    /// `require!(!tier::is_eligible_candidate(..), ..)` — admitting every ineligible candidate
    /// and rejecting every eligible one — passed the whole suite. A literal pin on the guard's
    /// full text, with no leading `!`, closes it.
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

    /// The arm-tuple pins above (`inserted_arm_reports_...`,
    /// `swapped_arm_settles_...`) check only that `(left, entered)` is bound correctly from each
    /// `CrossOutcome` — they say nothing about the emit site itself. `emit!(TierChanged { ..
    /// left: entered, entered: left })` would pass the whole suite. The shorthand field-init form
    /// (`left,` / `entered,`, meaning `left: left, entered: entered`) is what a rebinding like
    /// `left: entered` cannot survive without breaking this literal match — the IDL-decoder
    /// contract is read by field name, and this is the one place the event is actually built.
    #[test]
    fn tier_changed_binds_left_and_entered_by_shorthand_not_a_swapped_mapping() {
        assert!(flat(UPDATE_TIER_SRC)
            .contains("emit!(TierChanged { pool: pool_key, slot: clock.slot, left, entered, });"));
    }

    /// The count-and-ordering pin above does not notice a
    /// conditional wrapped around the sole `emit!` call site — `if left.is_none() { emit!(..) }`
    /// (silently dropping the audit event on every `Swapped` displacement) still emits exactly
    /// once, still after the match, and passed the whole suite. Pinning byte-adjacency between
    /// the match's closing `};` and the emit call closes it: nothing but whitespace may sit
    /// between them.
    #[test]
    fn tier_changed_is_unconditional_immediately_after_the_match_closes() {
        assert!(flat(UPDATE_TIER_SRC).contains("}; emit!(TierChanged {"));
    }

    /// Nothing else pins that the eligibility guard runs before the promotion attempt. Moving
    /// `require!(..)` from before the `match` to immediately after it — validate-after-mutate —
    /// would pass the rest of the suite (Anchor's atomic rollback on `Err` means no on-chain
    /// state persists either way, but the ordering itself still matters and nothing else pins it).
    ///
    /// `prod.find("require!(")` alone is name-blind — it finds the *first* `require!`
    /// anywhere in the file, not specifically the eligibility guard's. A decoy `require!(..)`
    /// left before the match (with the real guard moved after it) would satisfy that
    /// assertion. Searching for `tier::is_eligible_candidate(` instead finds
    /// the guard by what it actually checks, which no decoy can stand in for.
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

    // --- the settle/rank inputs are pinned by their literal source expression ----------------

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
            // No arithmetic lives here (it is all
            // in common/tier.rs and common/math.rs, already swept there) — a **predicate**, not
            // a literal, so the closure has to sweep the axis rather than one instance. The axis
            // is operation (add/sub/mul/div/rem) x safety-family (bare operator, `checked_`,
            // `saturating_`, `wrapping_`, `overflowing_`) — 2-D, but each needle below is
            // family-generic (`"checked_"` matches `checked_add`/`checked_sub`/... alike), so
            // five family needles close the whole grid rather than one per operation. Covering
            // only some families is defeated by forms like `tier_size + 1`, `.wrapping_add`,
            // `.overflowing_add` and `acc_tier * 2` slipping through the gaps.
            // Space-padded so the bare operators don't collide with `->`, lifetimes, or
            // `Context<'_, ...>` — verified absent from the file's production text before this
            // sweep existed, not merely assumed safe. `unchecked_`, `pow(` and `abs_diff(` are
            // the remaining named integer-arithmetic surfaces in `core`/`std` this axis has,
            // added the same way and verified absent first.
            //
            // Scoped to this file only, deliberately: five other handlers carry legitimate bare
            // arithmetic, so this needle set does not generalise crate-wide.
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

    /// `update_tier` moves no weight, so it carries no freeze guard here — the
    /// crate-wide call-site count is the partition in `instructions/mod.rs`, not checkable from
    /// this file alone.
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

    /// The checked-operations pattern applied here: `promote`/`demote`/`vacate`/
    /// `refresh_member`/`shrink` are all owned by other handlers or by `cross_into_tier`
    /// itself — this handler delegates to `cross_into_tier`/`settle_displaced_member` only.
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

    /// Set-based, not named-field: every write to `pool`/`top_tier`/`position`
    /// belongs to `common/tier.rs`, called on borrowed references — this file assigns none of
    /// their fields directly. `top_tier.entries[`
    /// and `pool.blanks[` never appear here either, asserted directly as well as through the
    /// widened `field_writes` predicate, since the predicate fix is what is being trusted.
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

    /// The sweep above pins `top_tier`'s and `position`'s seeds lines, `protocol_config`'s
    /// constraint, and that `pool` carries no `mut` — but that leaves 6 of the 9
    /// seeds/bump/constraint lines across the four PDA accounts with no literal pin at all. All
    /// three of these compile and pass the rest of the suite: `pool`'s seeds discriminator dropped
    /// (`seeds = [POOL_SEED, ..]` → `seeds = [..]`), `protocol_config`'s discriminator constant
    /// swapped for another account's (`PROTOCOL_CONFIG_SEED` → `POOL_SEED`), and a bump
    /// cross-referenced to the wrong account (`bump = pool.bump` → `bump = top_tier.bump` —
    /// Anchor validates bumps after every account is parsed, so this compiles regardless of
    /// declaration order; the same swept true for all four bump lines, not just this one pair).
    /// Every one of the four `#[account(..)]` blocks, pinned verbatim, closes all nine lines
    /// (and every same-typed cross-reference between them) in one pass rather than one line at
    /// a time.
    ///
    /// **When this fails:** update the literal to match the account's new attributes — never
    /// delete or weaken this assertion. A failure here is this instrument doing its job; confirm
    /// the change to the account surface is intended before updating the literal.
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
