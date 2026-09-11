use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum PositionState {
    Pending,
    Active,
    ClosedBelowFloor,
    Rejected,
    Seized,
}

/// **Every `PositionState`, in declaration order — the list the crate's exhaustiveness sweeps
/// iterate instead of writing their own.**
///
/// Three sweeps hand-listed the variants and three of them still said four after `Seized` was
/// added: `apply_withdraw`'s "every state variant" guard, `update_tier`'s eligibility product,
/// and `tier.rs`'s candidate admissibility — one of which promised in prose that a new variant
/// would fail to compile, which a hand-listed array cannot do. A shared list does not deliver
/// that promise either, and pretending otherwise is how the same defect returns; what closes it
/// is the pair below. `position_state_rejects_out_of_range_discriminant` reds the moment a
/// variant is added without being listed here, and every sweep reads this list, so one edit
/// carries all of them.
///
/// Test-only: nothing on chain enumerates the states, and a `const` the program never reads is
/// weight in the `.so`.
#[cfg(test)]
pub const ALL_POSITION_STATES: [PositionState; 5] = [
    PositionState::Pending,
    PositionState::Active,
    PositionState::ClosedBelowFloor,
    PositionState::Rejected,
    PositionState::Seized,
];

/// One deposited NFT's custody + accrual record. Seeds:
/// `[POSITION_SEED, pool, nft_mint]`.
#[account]
#[derive(InitSpace)]
pub struct Position {
    pub pool: Pubkey,
    pub depositor: Pubkey,
    pub nft_mint: Pubkey,
    pub position_id: u64,
    pub state: PositionState,
    pub recorded_value: u64,
    pub value_observed_at: i64,
    pub deposit_value: u64,
    pub slot_index: u32,
    pub activated_at: i64,
    pub equal_checkpoint: u128,
    pub accrued: u64,
    pub in_tier: bool,
    pub tier_checkpoint: u128,
    pub lock_until: i64,
    pub reject_reason: u16,
    /// Custody dispatch: 0 pNFT, 1 legacy `V1_NFT`, 2 MPL Core. Derived at intake from the
    /// presented asset account's **owner program** and — on the Token Metadata branch, where one
    /// owner program covers two standards — from `Metadata.token_standard` against a closed set,
    /// then written once and never taken as an instruction argument: a caller-supplied
    /// discriminant would let a depositor pick the weaker transfer path for a stronger asset.
    pub standard: u8,
    pub bump: u8,
    pub vault_bump: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    // `PositionState` is fieldless and its declaration index is the wire byte
    // `state` serializes to, with no test module and no explicit discriminant. A swap (e.g.
    // `Active` ⇄ `ClosedBelowFloor`) compiles clean and relocates which state every gate that
    // reads `position.state` through borsh — an off-chain indexer, a future CPI reader — sees.
    // Same shape as `common/events.rs`'s pin: hand-written, wildcard-free match, independent of
    // declaration order, so a fifth variant fails the crate to compile rather than dropping out.

    fn expected_position_state_byte(state: PositionState) -> u8 {
        match state {
            PositionState::Pending => 0,
            PositionState::Active => 1,
            PositionState::ClosedBelowFloor => 2,
            PositionState::Rejected => 3,
            PositionState::Seized => 4,
        }
    }

    #[test]
    fn position_state_variant_order_is_the_wire_discriminant() {
        for state in ALL_POSITION_STATES {
            let expected = expected_position_state_byte(state);
            assert_eq!(
                state.try_to_vec().unwrap(),
                vec![expected],
                "PositionState's wire byte moved for the variant this pin expected at index \
                 {expected} — a declaration-order swap relocates which state a borsh reader of \
                 `Position.state` sees"
            );
        }
    }

    // `ALL_POSITION_STATES` is written by hand; a variant added without a matching entry
    // compiles clean and stays invisible to every sweep that reads it. Asserting that the first
    // byte past the current range still fails to deserialize closes that gap — and now closes it
    // for the whole crate rather than for the pin above, because the list has three other
    // readers.
    #[test]
    fn position_state_rejects_out_of_range_discriminant() {
        assert_eq!(ALL_POSITION_STATES.len(), 5);
        assert!(
            PositionState::try_from_slice(&[5u8]).is_err(),
            "byte 5 deserialized into a PositionState — a 6th variant was added; add it to \
             ALL_POSITION_STATES, which every exhaustiveness sweep in the crate iterates"
        );
    }
}
