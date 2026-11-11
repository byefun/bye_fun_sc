use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum PositionState {
    Pending,
    Active,
    ClosedBelowFloor,
    Rejected,
    Seized,
}

/// Every `PositionState`, in declaration order — the list the crate's exhaustiveness sweeps
/// iterate. A hand-written list cannot fail to compile on a new variant;
/// `position_state_rejects_out_of_range_discriminant` fails if one is added without listing it.
///
/// Test-only: nothing on chain enumerates the states.
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

    // The declaration index is the borsh wire byte of `Position.state`; reordering variants
    // compiles clean but changes what off-chain readers see. The match below is wildcard-free
    // and independent of declaration order, so a new variant fails to compile.

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

    // `ALL_POSITION_STATES` is hand-written; the first byte past the range must fail to
    // deserialize so a variant added without a list entry is caught.
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
