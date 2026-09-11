use std::cmp::Ordering;

use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::math::position_fee_payout;
use crate::state::{Position, PositionState, TierEntry, TopTier};

/// The tier rank order: `value` desc, `activated_at` asc, `position_id` asc — the same order
/// `TopTier.entries` is kept sorted in, best rank first.
pub fn rank(a: &TierEntry, b: &TierEntry) -> Ordering {
    b.value
        .cmp(&a.value)
        .then(a.activated_at.cmp(&b.activated_at))
        .then(a.position_id.cmp(&b.position_id))
}

/// A candidate is admissible to promote only while active and not already a member. "Real" is
/// implied by `Active` — a position only reaches that state through activation.
pub fn is_eligible_candidate(state: PositionState, in_tier: bool) -> bool {
    state == PositionState::Active && !in_tier
}

/// The outcome of [`promote`].
pub enum PromoteOutcome {
    /// The tier had room; `candidate` was added with no eviction.
    Inserted,
    /// The tier was full and `candidate` outranked the evicted entry, replacing it.
    Swapped(TierEntry),
    /// The tier was full and `candidate` did not outrank its lowest-ranked member.
    Rejected,
}

/// Inserts `candidate` into `top_tier`, keeping `entries[..len]` sorted best-first, evicting
/// the lowest-ranked member if the tier is already at `tier_size` and `candidate` outranks it.
pub fn promote(top_tier: &mut TopTier, tier_size: u8, candidate: TierEntry) -> PromoteOutcome {
    let len = top_tier.len as usize;

    if len < tier_size as usize {
        let mut idx = len;
        while idx > 0 && rank(&candidate, &top_tier.entries[idx - 1]) == Ordering::Less {
            top_tier.entries[idx] = top_tier.entries[idx - 1];
            idx -= 1;
        }
        top_tier.entries[idx] = candidate;
        top_tier.len += 1;
        return PromoteOutcome::Inserted;
    }

    let min_idx = len - 1;
    if rank(&candidate, &top_tier.entries[min_idx]) != Ordering::Less {
        return PromoteOutcome::Rejected;
    }

    let evicted = top_tier.entries[min_idx];
    let mut idx = min_idx;
    while idx > 0 && rank(&candidate, &top_tier.entries[idx - 1]) == Ordering::Less {
        top_tier.entries[idx] = top_tier.entries[idx - 1];
        idx -= 1;
    }
    top_tier.entries[idx] = candidate;
    PromoteOutcome::Swapped(evicted)
}

/// Removes the member at `position`, if any, shifting later entries up and shrinking `len`.
pub fn demote(top_tier: &mut TopTier, position: Pubkey) -> Option<TierEntry> {
    let len = top_tier.len as usize;
    let idx = top_tier.entries[..len]
        .iter()
        .position(|e| e.position == position)?;
    let removed = top_tier.entries[idx];
    for i in idx..len - 1 {
        top_tier.entries[i] = top_tier.entries[i + 1];
    }
    top_tier.len -= 1;
    Some(removed)
}

/// Re-establishes rank order for a member whose value moved, in place: a checked [`demote`]
/// (which must find the member) followed by a checked [`promote`] back in (which must have
/// room — `demote` just freed the slot it occupied). Does not settle, does not re-stamp
/// `tier_checkpoint`, and does not touch `in_tier` — the member never left the tier, and
/// re-stamping without settling would silently forfeit its accrual.
pub fn refresh_member(
    top_tier: &mut TopTier,
    tier_size: u8,
    position: Pubkey,
    new_entry: TierEntry,
) -> Result<()> {
    let demoted = demote(top_tier, position);
    require!(demoted.is_some(), ByeMachineError::InvalidPositionState);
    let promoted = promote(top_tier, tier_size, new_entry);
    require!(
        matches!(promoted, PromoteOutcome::Inserted),
        ByeMachineError::InvalidPositionState
    );
    Ok(())
}

/// The close path's departure: a checked [`demote`], settling the departing member's tier delta
/// to `acc_tier` and clearing its membership flag. `position_key` identifies the `TopTier` entry
/// (a PDA address `Position` does not itself store); `position` is the account data to settle.
/// Returns the removed entry.
pub fn vacate(
    top_tier: &mut TopTier,
    position_key: Pubkey,
    position: &mut Position,
    acc_tier: u128,
) -> Result<TierEntry> {
    let removed = demote(top_tier, position_key);
    require!(removed.is_some(), ByeMachineError::InvalidPositionState);
    position.accrued = position_fee_payout(acc_tier, position.tier_checkpoint, position.accrued)?;
    position.tier_checkpoint = acc_tier;
    position.in_tier = false;
    Ok(removed.unwrap())
}

/// Lands a successor in the vacancy [`vacate`] just opened: a checked [`promote`] that must
/// return `Inserted` — a full tier cannot take a successor.
pub fn fill_vacancy(top_tier: &mut TopTier, tier_size: u8, entry: TierEntry) -> Result<()> {
    let promoted = promote(top_tier, tier_size, entry);
    require!(
        matches!(promoted, PromoteOutcome::Inserted),
        ByeMachineError::InvalidPositionState
    );
    Ok(())
}

/// The close path's optional successor: reads the first `remaining_accounts` entry as a
/// candidate, validates it against `pool` and tier eligibility, and lands it via
/// [`fill_vacancy`]. `Ok(None)` when the caller supplied no candidate — the departure alone is
/// a legitimate close. Unlike [`settle_displaced_member`]'s mandatory
/// displaced member, this path errors on nothing when the candidate is absent.
pub fn fill_vacancy_from_candidate<'info>(
    top_tier: &mut TopTier,
    tier_size: u8,
    remaining_accounts: &'info [AccountInfo<'info>],
    pool: Pubkey,
    acc_tier: u128,
) -> Result<Option<Pubkey>> {
    let Some(candidate_info) = remaining_accounts.first() else {
        return Ok(None);
    };
    require!(candidate_info.is_writable, ErrorCode::AccountNotMutable);
    let mut candidate = Account::<Position>::try_from(candidate_info)?;
    require_keys_eq!(candidate.pool, pool, ByeMachineError::PositionPoolMismatch);
    require!(
        is_eligible_candidate(candidate.state, candidate.in_tier),
        ByeMachineError::InvalidPositionState
    );
    let candidate_key = candidate_info.key();
    let candidate_entry = TierEntry {
        position: candidate_key,
        value: candidate.recorded_value,
        activated_at: candidate.activated_at,
        position_id: candidate.position_id,
    };
    fill_vacancy(top_tier, tier_size, candidate_entry)?;
    candidate.in_tier = true;
    candidate.tier_checkpoint = acc_tier;
    candidate.exit(&crate::ID)?;
    Ok(Some(candidate_key))
}

/// The outcome of [`cross_into_tier`], carrying back only what the handler cannot do itself:
/// `Swapped`'s evicted member still needs `settle_displaced_member`, which needs
/// `remaining_accounts` this module has no access to.
pub enum CrossOutcome {
    /// `position` entered the tier with no eviction.
    Inserted,
    /// `position` entered the tier, evicting `evicted` — the caller must settle it.
    Swapped(TierEntry),
    /// `position` did not outrank the tier's minimum; nothing changed.
    Rejected,
}

/// The crossing case: a non-member `position` whose value just crossed the tier threshold.
/// Attempts to promote it and applies the resulting membership effect to `position` directly —
/// `Inserted`/`Swapped` set `in_tier`/`tier_checkpoint`, `Rejected` touches neither, the true
/// no-op required (a non-member flagged `in_tier` would be unpromotable forever and
/// draw tier accrual it never earned).
pub fn cross_into_tier(
    top_tier: &mut TopTier,
    tier_size: u8,
    position: &mut Position,
    entry: TierEntry,
    acc_tier: u128,
) -> CrossOutcome {
    match promote(top_tier, tier_size, entry) {
        PromoteOutcome::Inserted => {
            position.in_tier = true;
            position.tier_checkpoint = acc_tier;
            CrossOutcome::Inserted
        }
        PromoteOutcome::Swapped(evicted) => {
            position.in_tier = true;
            position.tier_checkpoint = acc_tier;
            CrossOutcome::Swapped(evicted)
        }
        PromoteOutcome::Rejected => CrossOutcome::Rejected,
    }
}

/// The shrink path for a `tier_size` lowering: truncates `entries`/`len` to
/// `new_tier_size`, evicting the lowest-ranked members first since entries are kept sorted
/// best-first. Returns the evicted entries (best-of-the-evicted first) for the caller to
/// settle each one's tier delta against its own `Position` account — this helper has no
/// account access beyond `TopTier` itself. A no-op (empty result) if `new_tier_size >= len`.
pub fn shrink(top_tier: &mut TopTier, new_tier_size: u8) -> Vec<TierEntry> {
    let old_len = top_tier.len as usize;
    let new_len = new_tier_size as usize;
    if new_len >= old_len {
        return Vec::new();
    }
    let evicted = top_tier.entries[new_len..old_len].to_vec();
    top_tier.len = new_tier_size;
    evicted
}

/// Settles the tier delta of a member displaced by an activation or crossing and clears its
/// membership flag, so it stays promotable afterwards. Its `Position` travels in
/// `remaining_accounts`, matched by key — shared by `approve_deposit`'s and `record_value`'s
/// crossing paths, the same convention `update_config`'s shrink uses.
pub fn settle_displaced_member<'info>(
    remaining_accounts: &'info [AccountInfo<'info>],
    evicted: &TierEntry,
    acc_tier: u128,
) -> Result<()> {
    let info = remaining_accounts
        .iter()
        .find(|account| account.key() == evicted.position)
        .ok_or(ErrorCode::AccountNotEnoughKeys)?;
    require!(info.is_writable, ErrorCode::AccountNotMutable);

    let mut position = Account::<Position>::try_from(info)?;
    position.accrued = position_fee_payout(acc_tier, position.tier_checkpoint, position.accrued)?;
    position.tier_checkpoint = acc_tier;
    position.in_tier = false;
    position.exit(&crate::ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{ACC_PRECISION, DEFAULT_ADMISSION_FLOOR};
    use crate::state::position::ALL_POSITION_STATES;

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn entry(position_byte: u8, value: u64, activated_at: i64, position_id: u64) -> TierEntry {
        TierEntry {
            position: Pubkey::new_from_array([position_byte; 32]),
            value,
            activated_at,
            position_id,
        }
    }

    // `PromoteOutcome` carries a `TierEntry`, which derives neither `PartialEq` nor `Debug`
    // (state/top_tier.rs, out of this task's scope) — assert by matching instead.
    fn assert_inserted(outcome: PromoteOutcome) {
        match outcome {
            PromoteOutcome::Inserted => {}
            _ => panic!("expected Inserted"),
        }
    }

    fn assert_rejected(outcome: PromoteOutcome) {
        match outcome {
            PromoteOutcome::Rejected => {}
            _ => panic!("expected Rejected"),
        }
    }

    fn assert_swapped(outcome: PromoteOutcome, expected_position: Pubkey) {
        match outcome {
            PromoteOutcome::Swapped(evicted) => assert_eq!(evicted.position, expected_position),
            _ => panic!("expected Swapped"),
        }
    }

    // `CrossOutcome` carries the same non-`Debug`/non-`PartialEq` `TierEntry` — same technique.
    fn assert_inserted_cross(outcome: CrossOutcome) {
        match outcome {
            CrossOutcome::Inserted => {}
            _ => panic!("expected Inserted"),
        }
    }

    fn assert_rejected_cross(outcome: CrossOutcome) {
        match outcome {
            CrossOutcome::Rejected => {}
            _ => panic!("expected Rejected"),
        }
    }

    fn assert_swapped_cross(outcome: CrossOutcome, expected_position: Pubkey) {
        match outcome {
            CrossOutcome::Swapped(evicted) => assert_eq!(evicted.position, expected_position),
            _ => panic!("expected Swapped"),
        }
    }

    fn empty_top_tier() -> TopTier {
        TopTier {
            pool: Pubkey::default(),
            len: 0,
            entries: [TierEntry {
                position: Pubkey::default(),
                value: 0,
                activated_at: 0,
                position_id: 0,
            }; 20],
            acc_tier: 0,
            bump: 0,
        }
    }

    // --- total order -------------------------------------------------------------------------

    #[test]
    fn rank_orders_value_desc_activated_at_asc_position_id_asc() {
        let higher_value = entry(1, 200, 100, 1);
        let lower_value = entry(2, 100, 50, 1);
        assert_eq!(rank(&higher_value, &lower_value), Ordering::Less);

        let earlier = entry(1, 100, 50, 1);
        let later = entry(2, 100, 60, 1);
        assert_eq!(rank(&earlier, &later), Ordering::Less);

        let lower_id = entry(1, 100, 50, 1);
        let higher_id = entry(2, 100, 50, 2);
        assert_eq!(rank(&lower_id, &higher_id), Ordering::Less);
    }

    #[test]
    fn sorting_by_rank_matches_the_expected_full_order() {
        let mut entries = [
            entry(1, 100, 10, 5),
            entry(2, 300, 20, 1),
            entry(3, 200, 5, 2),
            entry(4, 300, 20, 0),
        ];
        entries.sort_by(rank);
        let ordered: Vec<u8> = entries.iter().map(|e| e.position.to_bytes()[0]).collect();
        // 300@t20#0 < 300@t20#1 (lower position_id) < 200@t5#2 < 100@t10#5
        assert_eq!(ordered, vec![4, 2, 3, 1]);
    }

    // --- admissibility --------------------------------------------------------------------

    /// Both halves of the conjunction, over every state rather than the four this listed before
    /// `Seized` existed: an `Active` member is refused, and no other state is admitted whether it
    /// is flagged a member or not. Driven from `ALL_POSITION_STATES` so the sweep grows with the
    /// enum instead of with whoever remembers it.
    #[test]
    fn eligible_candidate_requires_active_and_non_member() {
        assert!(is_eligible_candidate(PositionState::Active, false));
        assert!(!is_eligible_candidate(PositionState::Active, true));
        for state in ALL_POSITION_STATES
            .into_iter()
            .filter(|state| *state != PositionState::Active)
        {
            for in_tier in [false, true] {
                assert!(
                    !is_eligible_candidate(state, in_tier),
                    "only Active positions may take a tier seat"
                );
            }
        }
    }

    // --- promote — room available --------------------------------------------------------

    #[test]
    fn promote_inserts_in_rank_order_when_tier_has_room() {
        let mut tier = empty_top_tier();
        assert_inserted(promote(&mut tier, 3, entry(1, 100, 10, 1)));
        assert_inserted(promote(&mut tier, 3, entry(2, 300, 10, 1)));
        assert_inserted(promote(&mut tier, 3, entry(3, 200, 10, 1)));

        assert_eq!(tier.len, 3);
        assert_eq!(tier.entries[0].value, 300);
        assert_eq!(tier.entries[1].value, 200);
        assert_eq!(tier.entries[2].value, 100);
    }

    // --- promote — tier full, candidate wins/loses -----------------------------------------

    #[test]
    fn promote_swaps_out_the_minimum_when_candidate_outranks_it() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 2, entry(1, 100, 10, 1));
        promote(&mut tier, 2, entry(2, 200, 10, 1));
        assert_eq!(tier.len, 2);

        let outcome = promote(&mut tier, 2, entry(3, 150, 10, 1));
        assert_swapped(outcome, Pubkey::new_from_array([1u8; 32]));
        assert_eq!(tier.len, 2);
        assert_eq!(tier.entries[0].value, 200);
        assert_eq!(tier.entries[1].value, 150);
    }

    #[test]
    fn promote_rejects_when_tier_full_and_candidate_does_not_outrank_the_minimum() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 2, entry(1, 100, 10, 1));
        promote(&mut tier, 2, entry(2, 200, 10, 1));

        let before_len = tier.len;
        let before_positions = [tier.entries[0].position, tier.entries[1].position];
        let outcome = promote(&mut tier, 2, entry(3, 50, 10, 1));
        assert_rejected(outcome);
        assert_eq!(tier.len, before_len);
        assert_eq!(tier.entries[0].position, before_positions[0]);
        assert_eq!(tier.entries[1].position, before_positions[1]);
    }

    // --- demote --------------------------------------------------------------------------------

    #[test]
    fn demote_removes_the_member_and_shifts_the_remainder_up() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 1));
        promote(&mut tier, 3, entry(2, 200, 10, 1));
        promote(&mut tier, 3, entry(3, 100, 10, 1));

        let removed = demote(&mut tier, Pubkey::new_from_array([2u8; 32])).unwrap();
        assert_eq!(removed.value, 200);
        assert_eq!(tier.len, 2);
        assert_eq!(tier.entries[0].value, 300);
        assert_eq!(tier.entries[1].value, 100);
    }

    #[test]
    fn demote_of_non_member_returns_none() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 1));
        assert!(demote(&mut tier, Pubkey::new_from_array([9u8; 32])).is_none());
        assert_eq!(tier.len, 1);
    }

    // --- shrink ------------------------------------------------------------------------------

    #[test]
    fn shrink_truncates_and_returns_the_lowest_ranked_excess_members() {
        let mut tier = empty_top_tier();
        for i in 1..=5u8 {
            promote(
                &mut tier,
                20,
                entry(i, 100 * u64::from(i), 10, u64::from(i)),
            );
        }
        assert_eq!(tier.len, 5);

        let evicted = shrink(&mut tier, 2);
        assert_eq!(tier.len, 2);
        assert_eq!(tier.entries[0].value, 500);
        assert_eq!(tier.entries[1].value, 400);

        let evicted_values: Vec<u64> = evicted.iter().map(|e| e.value).collect();
        assert_eq!(evicted_values, vec![300, 200, 100]);
    }

    #[test]
    fn shrink_is_a_no_op_when_the_new_size_is_not_smaller() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 20, entry(1, 100, 10, 1));
        let evicted = shrink(&mut tier, 20);
        assert!(evicted.is_empty());
        assert_eq!(tier.len, 1);
    }

    #[test]
    fn shrink_can_evict_up_to_nineteen_members_at_once() {
        let mut tier = empty_top_tier();
        for i in 1..=20u8 {
            promote(&mut tier, 20, entry(i, u64::from(i), 10, u64::from(i)));
        }
        let evicted = shrink(&mut tier, 1);
        assert_eq!(evicted.len(), 19);
        assert_eq!(tier.len, 1);
    }

    // --- refresh_member — reorder, behavioural ---------------------------------------------

    #[test]
    fn refresh_member_reorders_up_past_its_neighbours() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 1));
        promote(&mut tier, 3, entry(2, 200, 10, 1));
        promote(&mut tier, 3, entry(3, 100, 10, 1));

        let member = Pubkey::new_from_array([3u8; 32]);
        refresh_member(&mut tier, 3, member, entry(3, 400, 10, 1)).unwrap();

        assert_eq!(tier.len, 3);
        assert_eq!(
            tier.entries[0].position, member,
            "reordered member now ranks first"
        );
        assert_eq!(tier.entries[0].value, 400);
        assert_eq!(tier.entries[1].position, Pubkey::new_from_array([1u8; 32]));
        assert_eq!(tier.entries[2].position, Pubkey::new_from_array([2u8; 32]));
    }

    #[test]
    fn refresh_member_reorders_down_past_its_neighbours() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 1));
        promote(&mut tier, 3, entry(2, 200, 10, 1));
        promote(&mut tier, 3, entry(3, 100, 10, 1));

        let member = Pubkey::new_from_array([1u8; 32]);
        refresh_member(&mut tier, 3, member, entry(1, 50, 10, 1)).unwrap();

        assert_eq!(tier.len, 3);
        assert_eq!(tier.entries[0].position, Pubkey::new_from_array([2u8; 32]));
        assert_eq!(tier.entries[1].position, Pubkey::new_from_array([3u8; 32]));
        assert_eq!(
            tier.entries[2].position, member,
            "reordered member now ranks last"
        );
        assert_eq!(tier.entries[2].value, 50);
    }

    #[test]
    fn refresh_member_breaks_a_tie_on_activated_at_then_position_id() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 5));
        promote(&mut tier, 3, entry(2, 200, 20, 1));
        promote(&mut tier, 3, entry(3, 100, 10, 1));

        // Member 3 refreshes to tie member 1 on both value and activated_at; position_id 1 < 5
        // must break the tie, ranking the refreshed member ahead of member 1.
        let member = Pubkey::new_from_array([3u8; 32]);
        refresh_member(&mut tier, 3, member, entry(3, 300, 10, 1)).unwrap();

        assert_eq!(tier.len, 3);
        assert_eq!(tier.entries[0].position, member);
        assert_eq!(tier.entries[1].position, Pubkey::new_from_array([1u8; 32]));
        assert_eq!(tier.entries[2].position, Pubkey::new_from_array([2u8; 32]));
    }

    #[test]
    fn refresh_member_of_a_non_member_returns_the_error_behaviourally() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 1));

        let non_member = Pubkey::new_from_array([9u8; 32]);
        let err = refresh_member(&mut tier, 3, non_member, entry(9, 400, 10, 1)).unwrap_err();
        assert_eq!(
            error_code(err),
            u32::from(ByeMachineError::InvalidPositionState)
        );
        assert_eq!(tier.len, 1, "a rejected refresh must not mutate the tier");
    }

    // --- vacate — close, member departs -----------------------------------------------------

    #[test]
    fn vacate_removes_the_member_and_shifts_the_remainder_up() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 1));
        promote(&mut tier, 3, entry(2, 200, 10, 1));
        promote(&mut tier, 3, entry(3, 100, 10, 1));

        let mut departing = candidate_position(Pubkey::default());
        departing.in_tier = true;
        departing.tier_checkpoint = 0;
        departing.accrued = 5;
        let acc_tier = 2 * ACC_PRECISION;
        let removed = vacate(
            &mut tier,
            Pubkey::new_from_array([2u8; 32]),
            &mut departing,
            acc_tier,
        )
        .unwrap();
        assert_eq!(removed.value, 200);
        assert_eq!(tier.len, 2);
        assert_eq!(tier.entries[0].value, 300);
        assert_eq!(tier.entries[1].value, 100);
        assert!(
            !departing.in_tier,
            "the departing member's own flag is cleared, not just the tier entry"
        );
        assert_eq!(departing.tier_checkpoint, acc_tier);
        assert_eq!(
            departing.accrued, 7,
            "(2-0) ACC_PRECISION / ACC_PRECISION + 5"
        );
    }

    #[test]
    fn vacate_of_a_non_member_returns_the_error_behaviourally() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 1));
        let mut departing = candidate_position(Pubkey::default());

        // `TierEntry` derives neither `PartialEq` nor `Debug`, so `unwrap_err()` is unavailable
        // on a `Result<TierEntry, _>` — match instead, same as `assert_swapped` above.
        match vacate(
            &mut tier,
            Pubkey::new_from_array([9u8; 32]),
            &mut departing,
            0,
        ) {
            Err(err) => {
                assert_eq!(
                    error_code(err),
                    u32::from(ByeMachineError::InvalidPositionState)
                )
            }
            Ok(_) => panic!("expected an error for a non-member"),
        }
        assert_eq!(tier.len, 1);
    }

    // --- fill_vacancy — close, successor fills the vacancy -----------------------------------

    #[test]
    fn fill_vacancy_lands_the_candidate_in_its_correct_rank_slot() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 1));
        promote(&mut tier, 3, entry(2, 100, 10, 1));

        // A vacancy just opened at tier_size 3, len 2; the successor's value ranks between the
        // two existing members, so it must land in the middle slot, not be appended at the end.
        fill_vacancy(&mut tier, 3, entry(3, 200, 10, 1)).unwrap();

        assert_eq!(tier.len, 3);
        assert_eq!(tier.entries[0].value, 300);
        assert_eq!(
            tier.entries[1].value, 200,
            "lands in rank order, not appended"
        );
        assert_eq!(tier.entries[1].position, Pubkey::new_from_array([3u8; 32]));
        assert_eq!(tier.entries[2].value, 100);
    }

    #[test]
    fn fill_vacancy_with_no_room_returns_the_error_behaviourally() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 2, entry(1, 300, 10, 1));
        promote(&mut tier, 2, entry(2, 200, 10, 1));

        // Outranks the minimum, so `promote` would `Swap` it in — still not `Inserted`.
        let err = fill_vacancy(&mut tier, 2, entry(3, 250, 10, 1)).unwrap_err();
        assert_eq!(
            error_code(err),
            u32::from(ByeMachineError::InvalidPositionState)
        );
        assert_eq!(tier.len, 2, "a full tier cannot take a successor");
    }

    // --- fill_vacancy_from_candidate — absent and present candidate -------------------------

    fn serialized(position: &Position) -> Vec<u8> {
        let mut data = vec![0u8; 8 + Position::INIT_SPACE];
        position.try_serialize(&mut data.as_mut_slice()).unwrap();
        data
    }

    fn candidate_position(pool: Pubkey) -> Position {
        Position {
            pool,
            depositor: Pubkey::default(),
            nft_mint: Pubkey::default(),
            position_id: 7,
            state: PositionState::Active,
            recorded_value: DEFAULT_ADMISSION_FLOOR,
            value_observed_at: 0,
            deposit_value: DEFAULT_ADMISSION_FLOOR,
            slot_index: 0,
            activated_at: 20,
            equal_checkpoint: 0,
            accrued: 0,
            in_tier: false,
            tier_checkpoint: 0,
            lock_until: 0,
            reject_reason: 0,
            standard: 0,
            bump: 0,
            vault_bump: 0,
        }
    }

    #[test]
    fn fill_vacancy_from_candidate_returns_none_without_mutating_the_tier_when_absent() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 3, entry(1, 300, 10, 1));

        let entered = fill_vacancy_from_candidate(&mut tier, 3, &[], Pubkey::default(), 0).unwrap();
        assert!(entered.is_none());
        assert_eq!(
            tier.len, 1,
            "no candidate supplied — the departure alone is a legitimate close"
        );
    }

    #[test]
    fn fill_vacancy_from_candidate_lands_a_writable_eligible_candidate_and_settles_it() {
        let mut tier = empty_top_tier();
        let pool = Pubkey::new_unique();
        let key = Pubkey::new_unique();
        let owner = crate::ID;
        let mut lamports = 1u64;
        let mut data = serialized(&candidate_position(pool));
        let accounts = vec![AccountInfo::new(
            &key,
            false,
            true,
            &mut lamports,
            &mut data,
            &owner,
            false,
            0,
        )];

        let acc_tier = 5 * ACC_PRECISION;
        let entered = fill_vacancy_from_candidate(&mut tier, 3, &accounts, pool, acc_tier).unwrap();
        assert_eq!(entered, Some(key));
        assert_eq!(tier.len, 1);
        assert_eq!(tier.entries[0].position, key);

        let settled = Account::<Position>::try_from(&accounts[0]).unwrap();
        assert!(settled.in_tier);
        assert_eq!(settled.tier_checkpoint, acc_tier);
    }

    #[test]
    fn fill_vacancy_from_candidate_rejects_a_read_only_candidate_at_3006() {
        let mut tier = empty_top_tier();
        let pool = Pubkey::new_unique();
        let key = Pubkey::new_unique();
        let owner = crate::ID;
        let mut lamports = 1u64;
        let mut data = serialized(&candidate_position(pool));
        let accounts = vec![AccountInfo::new(
            &key,
            false,
            false,
            &mut lamports,
            &mut data,
            &owner,
            false,
            0,
        )];

        let err = fill_vacancy_from_candidate(&mut tier, 3, &accounts, pool, 0).unwrap_err();
        assert_eq!(error_code(err), u32::from(ErrorCode::AccountNotMutable));
    }

    #[test]
    fn fill_vacancy_from_candidate_rejects_a_wrong_pool_candidate_at_6304() {
        let mut tier = empty_top_tier();
        let pool = Pubkey::new_unique();
        let other_pool = Pubkey::new_unique();
        let key = Pubkey::new_unique();
        let owner = crate::ID;
        let mut lamports = 1u64;
        let mut data = serialized(&candidate_position(other_pool));
        let accounts = vec![AccountInfo::new(
            &key,
            false,
            true,
            &mut lamports,
            &mut data,
            &owner,
            false,
            0,
        )];

        let err = fill_vacancy_from_candidate(&mut tier, 3, &accounts, pool, 0).unwrap_err();
        assert_eq!(
            error_code(err),
            u32::from(ByeMachineError::PositionPoolMismatch)
        );
    }

    #[test]
    fn fill_vacancy_from_candidate_rejects_an_ineligible_candidate() {
        let mut tier = empty_top_tier();
        let pool = Pubkey::new_unique();
        let key = Pubkey::new_unique();
        let owner = crate::ID;
        let mut lamports = 1u64;
        let mut ineligible = candidate_position(pool);
        ineligible.in_tier = true;
        let mut data = serialized(&ineligible);
        let accounts = vec![AccountInfo::new(
            &key,
            false,
            true,
            &mut lamports,
            &mut data,
            &owner,
            false,
            0,
        )];

        let err = fill_vacancy_from_candidate(&mut tier, 3, &accounts, pool, 0).unwrap_err();
        assert_eq!(
            error_code(err),
            u32::from(ByeMachineError::InvalidPositionState)
        );
    }

    // --- cross_into_tier — the crossing case, behavioural ------------------------------------

    #[test]
    fn cross_into_tier_inserted_sets_in_tier_and_tier_checkpoint() {
        let mut tier = empty_top_tier();
        let mut position = candidate_position(Pubkey::default());
        let acc_tier = 4 * ACC_PRECISION;

        let outcome = cross_into_tier(&mut tier, 3, &mut position, entry(1, 200, 10, 1), acc_tier);
        assert_inserted_cross(outcome);
        assert_eq!(tier.len, 1);
        assert!(position.in_tier);
        assert_eq!(position.tier_checkpoint, acc_tier);
    }

    #[test]
    fn cross_into_tier_swapped_sets_in_tier_and_tier_checkpoint_and_returns_the_evicted_entry() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 2, entry(1, 100, 10, 1));
        promote(&mut tier, 2, entry(2, 200, 10, 1));
        let mut position = candidate_position(Pubkey::default());
        let acc_tier = 4 * ACC_PRECISION;

        let outcome = cross_into_tier(&mut tier, 2, &mut position, entry(3, 150, 10, 1), acc_tier);
        assert_swapped_cross(outcome, Pubkey::new_from_array([1u8; 32]));
        assert!(position.in_tier);
        assert_eq!(position.tier_checkpoint, acc_tier);
    }

    /// `Rejected` is the common outcome for a non-member refresh — most
    /// positions don't crack the tier. Setting `in_tier` there would flag a non-member as a
    /// member, forever unpromotable by `is_eligible_candidate` and drawing tier accrual it
    /// never earned. A behavioural assertion rather than a text pin on an empty `{}` block.
    #[test]
    fn cross_into_tier_rejected_is_a_true_no_op() {
        let mut tier = empty_top_tier();
        promote(&mut tier, 2, entry(1, 100, 10, 1));
        promote(&mut tier, 2, entry(2, 200, 10, 1));
        let mut position = candidate_position(Pubkey::default());
        position.tier_checkpoint = 7;

        let outcome = cross_into_tier(&mut tier, 2, &mut position, entry(3, 50, 10, 1), 99);
        assert_rejected_cross(outcome);
        assert_eq!(tier.len, 2, "a rejected crossing must not mutate the tier");
        assert!(
            !position.in_tier,
            "a rejected crossing must not flag membership"
        );
        assert_eq!(
            position.tier_checkpoint, 7,
            "a rejected crossing must not stamp a checkpoint it never earned"
        );
    }

    // --- settle_displaced_member ---------------------------------------------------------

    fn tier_member(accrued: u64, tier_checkpoint: u128) -> Position {
        Position {
            pool: Pubkey::default(),
            depositor: Pubkey::default(),
            nft_mint: Pubkey::default(),
            position_id: 1,
            state: PositionState::Active,
            recorded_value: DEFAULT_ADMISSION_FLOOR,
            value_observed_at: 0,
            deposit_value: DEFAULT_ADMISSION_FLOOR,
            slot_index: 0,
            activated_at: 0,
            equal_checkpoint: 0,
            accrued,
            in_tier: true,
            tier_checkpoint,
            lock_until: 0,
            reject_reason: 0,
            standard: 0,
            bump: 0,
            vault_bump: 0,
        }
    }

    fn evicted_entry(position: Pubkey) -> TierEntry {
        TierEntry {
            position,
            value: DEFAULT_ADMISSION_FLOOR,
            activated_at: 0,
            position_id: 1,
        }
    }

    #[test]
    fn settling_a_displaced_member_clears_in_tier_and_leaves_it_promotable() {
        let key = Pubkey::new_unique();
        let owner = crate::ID;
        let mut lamports = 1u64;
        let mut data = serialized(&tier_member(7, 0));
        let accounts = vec![AccountInfo::new(
            &key,
            false,
            true,
            &mut lamports,
            &mut data,
            &owner,
            false,
            0,
        )];

        let acc_tier = 3 * ACC_PRECISION;
        settle_displaced_member(&accounts, &evicted_entry(key), acc_tier).unwrap();

        let settled = Account::<Position>::try_from(&accounts[0]).unwrap();
        assert!(!settled.in_tier);
        assert_eq!(settled.tier_checkpoint, acc_tier);
        assert_eq!(settled.accrued, 10);
        assert!(is_eligible_candidate(settled.state, settled.in_tier));
    }

    #[test]
    fn settling_rejects_a_displaced_member_whose_position_was_not_supplied() {
        let key = Pubkey::new_unique();
        let owner = crate::ID;
        let mut lamports = 1u64;
        let mut data = serialized(&tier_member(0, 0));
        let accounts = vec![AccountInfo::new(
            &key,
            false,
            true,
            &mut lamports,
            &mut data,
            &owner,
            false,
            0,
        )];

        let missing = evicted_entry(Pubkey::new_unique());
        assert_eq!(
            error_code(settle_displaced_member(&accounts, &missing, 0).unwrap_err()),
            u32::from(ErrorCode::AccountNotEnoughKeys)
        );
    }

    #[test]
    fn settling_rejects_a_displaced_member_supplied_read_only() {
        let key = Pubkey::new_unique();
        let owner = crate::ID;
        let mut lamports = 1u64;
        let mut data = serialized(&tier_member(0, 0));
        let accounts = vec![AccountInfo::new(
            &key,
            false,
            false,
            &mut lamports,
            &mut data,
            &owner,
            false,
            0,
        )];

        assert_eq!(
            error_code(settle_displaced_member(&accounts, &evicted_entry(key), 0).unwrap_err()),
            u32::from(ErrorCode::AccountNotMutable)
        );
    }
}
