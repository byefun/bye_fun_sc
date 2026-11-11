use anchor_lang::prelude::*;

/// Explicit discriminants pin each group's first variant to its hundred (61xx, 62xx, ...);
/// every other variant in a group auto-increments from there. A variant is only ever **appended**
/// to its group: inserting one mid-group renumbers every variant below it inside the same
/// hundred, moving codes clients are already keyed on while leaving every group boundary intact.
#[error_code]
pub enum ByeMachineError {
    #[msg("This asset's collection is not admitted, or the collection accounts supplied do not match the one the asset declares")]
    CollectionNotAdmitted = 100,
    #[msg("Token standard is not one this program admits")]
    StandardNotAdmitted,
    #[msg("Compressed assets are not accepted")]
    CompressedAssetNotSupported,
    #[msg("A pending deposit already exists for this NFT")]
    DepositAlreadyPending,
    #[msg("Value is below the pool's admission floor")]
    BelowAdmissionFloor,
    #[msg("Value exceeds the pool's admission ceiling")]
    AboveAdmissionCeiling,
    #[msg("Deposit would exceed this wallet's value cap")]
    WalletValueCapExceeded,
    #[msg("Deposits are currently paused")]
    DepositsPaused,
    #[msg("This collection is not admitted for the presented asset's token standard")]
    StandardNotAdmittedForCollection,

    /// One shared code for every instruction that refuses while a batch is open — the condition
    /// a caller retries on is the same one in each, so a per-domain code would give a client
    /// several numbers meaning "wait for the batch" and no way to tell they are one condition.
    #[msg("An open roll batch must resolve before this action")]
    RollBatchInFlight = 200,
    #[msg("Rolls are currently paused")]
    RollsPaused,
    #[msg("A value sweep is pending — it must close before rolls are committed")]
    SweepPending,
    #[msg("Roll count must be between 1 and the pool's max N")]
    RollCountOutOfBounds,
    #[msg("The pool holds no real card to draw against")]
    RealPoolEmpty,
    #[msg("Rolls settle in index order and this is not the next one")]
    RollOutOfOrder,
    #[msg("Batch randomness has not been delivered yet")]
    BatchNotResolved,
    #[msg("Tier holds fewer members than the pool's real cards require")]
    TierNotFull,
    #[msg("The batch's recovery timeout has not elapsed")]
    RecoveryTimeoutNotElapsed,
    #[msg("The batch's VRF queue item is still present — purge it again, then retry")]
    QueueItemStillPresent,
    #[msg("Supplied batch belongs to a different pool")]
    BatchPoolMismatch,
    #[msg("Batch must be Complete or Recovered before it can be closed")]
    BatchNotClosable,
    #[msg("Pool weight or blank set moved outside this batch since the previous roll")]
    BatchExpectationMismatch,
    #[msg("Blank outcomes settle through settle_roll, not through this instruction")]
    BlankSettlesElsewhere,
    #[msg("Supplied position is not the one this roll drew")]
    DrawnPositionMismatch,
    #[msg("This batch is not the head of the pool's settlement queue")]
    BatchNotHeadOfQueue,

    #[msg("Position is locked until a later time")]
    PositionLocked = 300,
    #[msg("Position is not in the required state for this action")]
    InvalidPositionState,
    #[msg("Signer is not this position's depositor")]
    NotPositionOwner,
    #[msg("Positions acquired at a sale are activated only by activate_acquired")]
    TreasuryPositionNotEligible,
    #[msg("Supplied position belongs to a different pool")]
    PositionPoolMismatch,
    #[msg("Candidate does not outrank the tier's lowest member")]
    CandidateOutranked,
    #[msg("This instruction does not serve the position's stored token standard")]
    WrongStandardForInstruction,
    #[msg("Escrowed collateral is gone — call close_seized")]
    CollateralAbsent,
    #[msg("Escrowed collateral is still in the vault — there is nothing to close")]
    CollateralPresent,
    #[msg("Escrowed collateral is frozen and cannot be transferred — call close_seized")]
    CollateralFrozen,
    #[msg("Escrowed collateral carries a transfer surface this program cannot decide")]
    CollateralUndecidable,
    #[msg("Position was not acquired at a sale")]
    NotATreasuryPosition,

    #[msg("Position is not in the Pending state")]
    PositionNotPending = 400,
    #[msg("Recorded value must be greater than zero")]
    ZeroValue,
    #[msg("A value sweep must be open for this action")]
    SweepNotOpen,
    #[msg("Position is locked and cannot be deactivated below the admission floor")]
    LockedDeactivationRejected,
    #[msg("A value sweep is already open")]
    SweepAlreadyOpen,

    /// Raised by nothing. It holds the group's explicit discriminant, so removing it would renumber
    /// every code after it.
    #[msg("Admission floor must remain above the ticket target")]
    AdmissionFloorNotAboveTicket = 500,
    #[msg("Allocation basis points must sum to 10,000")]
    AllocationBpsNotFullyAllocated,
    #[msg("Every blank face value must stay below the ticket target")]
    BlankFaceNotBelowTicket,
    #[msg("Signer is not authorized for this action")]
    Unauthorized,
    #[msg("Tier size must be between 1 and 20")]
    TierSizeOutOfBounds,
    #[msg("Fee must equal price minus ticket target")]
    FeeNotPriceMinusTicket,
    #[msg("Max N must be between 1 and 10")]
    MaxNOutOfBounds,
    #[msg("Admitted standards must name at least one supported standard and no other bit")]
    StandardsMaskInvalid,
    #[msg("Admitted standards already hold this value")]
    StandardsUpdateNoOp,
    #[msg("Clearing an admitted standard requires a reason")]
    NarrowingReasonRequired,
    #[msg("A reason is meaningless when no admitted standard is cleared")]
    ReasonNotApplicable,
    #[msg("Every blank face value must be greater than zero")]
    BlankFaceZero,
    #[msg("Pinned swap pool is not a DAMM v2 pool for this program's $BYE/USDC pair")]
    SwapPoolInvalid,
    #[msg("Pinned swap pool is not currently swappable")]
    SwapPoolNotSwappable,
    #[msg("Max swap slippage must be below 10,000 basis points")]
    SwapSlippageOutOfBounds,
    #[msg("Treasury 05's authority must be set; Treasury 05 is derived from it")]
    T05AuthorityUnset,
    #[msg("The account currently named as Treasury 03 must hold zero before migrating to the program PDA")]
    LegacyTreasury03NotEmpty,
    #[msg("An authority cannot be set to the default key")]
    AuthorityUnset,
    #[msg("The role already holds this authority")]
    AuthorityUnchanged,
    #[msg("A Treasury 04 transfer needs two different pools")]
    T04TransferSamePool,
    #[msg("The transfer would put the destination Treasury 04 above its ceiling")]
    T04TransferAboveCeiling,
    #[msg("The transfer would leave the source Treasury 04 below its largest blank face")]
    T04TransferBelowReserve,
    #[msg("A Treasury 04 transfer must move a non-zero amount")]
    T04TransferAmountZero,
    #[msg("A Treasury 03 transfer needs two different pools")]
    T03TransferSamePool,
    #[msg("A Treasury 03 transfer must move a non-zero amount")]
    T03TransferAmountZero,
    #[msg("The buyback rate must not exceed 10,000 bps or the pool's escrow bid")]
    BuybackRateOutOfRange,
    #[msg("The escrow bid must lie between the pool's buyback rate and 10,000 bps")]
    EscrowBidOutOfRange,
    #[msg("An admission floor at or below the ticket target needs a blank face above the ticket")]
    FloorNeedsAboveTicketBlank,
    #[msg("The above-ticket blank face must sit at least 1 USDC above the ticket target")]
    AboveTicketBlankTooClose,
    #[msg("Reserved blank-face slots must hold zero")]
    ReservedBlankSlotSet,
    #[msg("Treasury 04's ceiling is protocol-wide; set it with set_t04_ceiling")]
    T04CeilingIsProtocolWide,
    #[msg("Treasury 04's ceiling is 0; the sweep stays refused until the Administrator sets it")]
    T04CeilingUnset,
    #[msg("The sweep must name every pool exactly once, each at its own address")]
    SweepPoolSetIncomplete,
    #[msg(
        "The pool's Treasury 03 is not its per-pool account, so there is nothing to consolidate"
    )]
    TreasuryNotPerPool,

    #[msg("Principal vault does not cover this batch's roll liability")]
    PrincipalShortfall = 600,
    #[msg("T04 does not cover the amount owed")]
    T04Shortfall,
    #[msg("T03 does not cover the amount owed")]
    T03Shortfall,
    #[msg("Swap slippage exceeds the configured maximum")]
    SlippageExceeded,

    #[msg("Weight index total does not match the pool's aggregate weight")]
    WeightDesync = 700,
    #[msg("Weight index has no free or unallocated slots left")]
    WeightIndexFull,
    #[msg("Weight index account is not a blank, correctly sized, program-owned account")]
    WeightIndexNotBlank,
    #[msg("Checked arithmetic failed or produced a degenerate result")]
    ArithmeticFailure,

    #[msg("An escrow withdrawal must move a non-zero amount")]
    EscrowAmountZero = 800,
    #[msg("The escrow's balance does not cover this amount")]
    EscrowBalanceShort,
    #[msg("The loader's escrow, vault and wallet stats are not one owner's complete set")]
    EscrowAccountsMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every variant's wire number, as a wildcard-free `match`: a variant added to the enum fails
    /// to compile here until it declares the number clients key on. Hand-written rather than read
    /// off the enum, because it is compared against `u32::from`, which *does* read the enum's own
    /// discriminants — the two disagree the moment a declaration moves, which is what makes this
    /// a differential rather than a restatement of the thing it checks.
    ///
    /// **Pinning every number, not each group's first, is what catches a mid-group insertion.** A
    /// variant spliced into the middle of a group renumbers every variant below it while leaving
    /// all of them inside their own hundred, so an assertion that only checks group boundaries
    /// passes on precisely the edit that moves codes already on the wire.
    fn expected_code(variant: ByeMachineError) -> u32 {
        match variant {
            ByeMachineError::CollectionNotAdmitted => 6100,
            ByeMachineError::StandardNotAdmitted => 6101,
            ByeMachineError::CompressedAssetNotSupported => 6102,
            ByeMachineError::DepositAlreadyPending => 6103,
            ByeMachineError::BelowAdmissionFloor => 6104,
            ByeMachineError::AboveAdmissionCeiling => 6105,
            ByeMachineError::WalletValueCapExceeded => 6106,
            ByeMachineError::DepositsPaused => 6107,
            ByeMachineError::StandardNotAdmittedForCollection => 6108,

            ByeMachineError::RollBatchInFlight => 6200,
            ByeMachineError::RollsPaused => 6201,
            ByeMachineError::SweepPending => 6202,
            ByeMachineError::RollCountOutOfBounds => 6203,
            ByeMachineError::RealPoolEmpty => 6204,
            ByeMachineError::RollOutOfOrder => 6205,
            ByeMachineError::BatchNotResolved => 6206,
            ByeMachineError::TierNotFull => 6207,
            ByeMachineError::RecoveryTimeoutNotElapsed => 6208,
            ByeMachineError::QueueItemStillPresent => 6209,
            ByeMachineError::BatchPoolMismatch => 6210,
            ByeMachineError::BatchNotClosable => 6211,
            ByeMachineError::BatchExpectationMismatch => 6212,
            ByeMachineError::BlankSettlesElsewhere => 6213,
            ByeMachineError::DrawnPositionMismatch => 6214,
            ByeMachineError::BatchNotHeadOfQueue => 6215,

            ByeMachineError::PositionLocked => 6300,
            ByeMachineError::InvalidPositionState => 6301,
            ByeMachineError::NotPositionOwner => 6302,
            ByeMachineError::TreasuryPositionNotEligible => 6303,
            ByeMachineError::PositionPoolMismatch => 6304,
            ByeMachineError::CandidateOutranked => 6305,
            ByeMachineError::WrongStandardForInstruction => 6306,
            ByeMachineError::CollateralAbsent => 6307,
            ByeMachineError::CollateralPresent => 6308,
            ByeMachineError::CollateralFrozen => 6309,
            ByeMachineError::CollateralUndecidable => 6310,
            ByeMachineError::NotATreasuryPosition => 6311,

            ByeMachineError::PositionNotPending => 6400,
            ByeMachineError::ZeroValue => 6401,
            ByeMachineError::SweepNotOpen => 6402,
            ByeMachineError::LockedDeactivationRejected => 6403,
            ByeMachineError::SweepAlreadyOpen => 6404,

            ByeMachineError::AdmissionFloorNotAboveTicket => 6500,
            ByeMachineError::AllocationBpsNotFullyAllocated => 6501,
            ByeMachineError::BlankFaceNotBelowTicket => 6502,
            ByeMachineError::Unauthorized => 6503,
            ByeMachineError::TierSizeOutOfBounds => 6504,
            ByeMachineError::FeeNotPriceMinusTicket => 6505,
            ByeMachineError::MaxNOutOfBounds => 6506,
            ByeMachineError::StandardsMaskInvalid => 6507,
            ByeMachineError::StandardsUpdateNoOp => 6508,
            ByeMachineError::NarrowingReasonRequired => 6509,
            ByeMachineError::ReasonNotApplicable => 6510,
            ByeMachineError::BlankFaceZero => 6511,
            ByeMachineError::SwapPoolInvalid => 6512,
            ByeMachineError::SwapPoolNotSwappable => 6513,
            ByeMachineError::SwapSlippageOutOfBounds => 6514,
            ByeMachineError::T05AuthorityUnset => 6515,
            ByeMachineError::LegacyTreasury03NotEmpty => 6516,
            ByeMachineError::AuthorityUnset => 6517,
            ByeMachineError::AuthorityUnchanged => 6518,
            ByeMachineError::T04TransferSamePool => 6519,
            ByeMachineError::T04TransferAboveCeiling => 6520,
            ByeMachineError::T04TransferBelowReserve => 6521,
            ByeMachineError::T04TransferAmountZero => 6522,
            ByeMachineError::T03TransferSamePool => 6523,
            ByeMachineError::T03TransferAmountZero => 6524,
            ByeMachineError::BuybackRateOutOfRange => 6525,
            ByeMachineError::EscrowBidOutOfRange => 6526,
            ByeMachineError::FloorNeedsAboveTicketBlank => 6527,
            ByeMachineError::AboveTicketBlankTooClose => 6528,
            ByeMachineError::ReservedBlankSlotSet => 6529,
            ByeMachineError::T04CeilingIsProtocolWide => 6530,
            ByeMachineError::T04CeilingUnset => 6531,
            ByeMachineError::SweepPoolSetIncomplete => 6532,
            ByeMachineError::TreasuryNotPerPool => 6533,

            ByeMachineError::PrincipalShortfall => 6600,
            ByeMachineError::T04Shortfall => 6601,
            ByeMachineError::T03Shortfall => 6602,
            ByeMachineError::SlippageExceeded => 6603,

            ByeMachineError::WeightDesync => 6700,
            ByeMachineError::WeightIndexFull => 6701,
            ByeMachineError::WeightIndexNotBlank => 6702,
            ByeMachineError::ArithmeticFailure => 6703,

            ByeMachineError::EscrowAmountZero => 6800,
            ByeMachineError::EscrowBalanceShort => 6801,
            ByeMachineError::EscrowAccountsMismatch => 6802,
        }
    }

    /// The iteration source. `expected_code`'s exhaustive match forces a new variant to declare a
    /// number, and `GROUP_SIZES` forces it to appear here, since each group's membership is
    /// counted against a pinned size rather than sampled.
    const ALL: &[ByeMachineError] = &[
        ByeMachineError::CollectionNotAdmitted,
        ByeMachineError::StandardNotAdmitted,
        ByeMachineError::CompressedAssetNotSupported,
        ByeMachineError::DepositAlreadyPending,
        ByeMachineError::BelowAdmissionFloor,
        ByeMachineError::AboveAdmissionCeiling,
        ByeMachineError::WalletValueCapExceeded,
        ByeMachineError::DepositsPaused,
        ByeMachineError::StandardNotAdmittedForCollection,
        ByeMachineError::RollBatchInFlight,
        ByeMachineError::RollsPaused,
        ByeMachineError::SweepPending,
        ByeMachineError::RollCountOutOfBounds,
        ByeMachineError::RealPoolEmpty,
        ByeMachineError::RollOutOfOrder,
        ByeMachineError::BatchNotResolved,
        ByeMachineError::TierNotFull,
        ByeMachineError::RecoveryTimeoutNotElapsed,
        ByeMachineError::QueueItemStillPresent,
        ByeMachineError::BatchPoolMismatch,
        ByeMachineError::BatchNotClosable,
        ByeMachineError::BatchExpectationMismatch,
        ByeMachineError::BlankSettlesElsewhere,
        ByeMachineError::DrawnPositionMismatch,
        ByeMachineError::BatchNotHeadOfQueue,
        ByeMachineError::PositionLocked,
        ByeMachineError::InvalidPositionState,
        ByeMachineError::NotPositionOwner,
        ByeMachineError::TreasuryPositionNotEligible,
        ByeMachineError::PositionPoolMismatch,
        ByeMachineError::CandidateOutranked,
        ByeMachineError::WrongStandardForInstruction,
        ByeMachineError::CollateralAbsent,
        ByeMachineError::CollateralPresent,
        ByeMachineError::CollateralFrozen,
        ByeMachineError::CollateralUndecidable,
        ByeMachineError::NotATreasuryPosition,
        ByeMachineError::PositionNotPending,
        ByeMachineError::ZeroValue,
        ByeMachineError::SweepNotOpen,
        ByeMachineError::LockedDeactivationRejected,
        ByeMachineError::SweepAlreadyOpen,
        ByeMachineError::AdmissionFloorNotAboveTicket,
        ByeMachineError::AllocationBpsNotFullyAllocated,
        ByeMachineError::BlankFaceNotBelowTicket,
        ByeMachineError::Unauthorized,
        ByeMachineError::TierSizeOutOfBounds,
        ByeMachineError::FeeNotPriceMinusTicket,
        ByeMachineError::MaxNOutOfBounds,
        ByeMachineError::StandardsMaskInvalid,
        ByeMachineError::StandardsUpdateNoOp,
        ByeMachineError::NarrowingReasonRequired,
        ByeMachineError::ReasonNotApplicable,
        ByeMachineError::BlankFaceZero,
        ByeMachineError::SwapPoolInvalid,
        ByeMachineError::SwapPoolNotSwappable,
        ByeMachineError::SwapSlippageOutOfBounds,
        ByeMachineError::T05AuthorityUnset,
        ByeMachineError::LegacyTreasury03NotEmpty,
        ByeMachineError::AuthorityUnset,
        ByeMachineError::AuthorityUnchanged,
        ByeMachineError::T04TransferSamePool,
        ByeMachineError::T04TransferAboveCeiling,
        ByeMachineError::T04TransferBelowReserve,
        ByeMachineError::T04TransferAmountZero,
        ByeMachineError::T03TransferSamePool,
        ByeMachineError::T03TransferAmountZero,
        ByeMachineError::BuybackRateOutOfRange,
        ByeMachineError::EscrowBidOutOfRange,
        ByeMachineError::FloorNeedsAboveTicketBlank,
        ByeMachineError::AboveTicketBlankTooClose,
        ByeMachineError::ReservedBlankSlotSet,
        ByeMachineError::T04CeilingIsProtocolWide,
        ByeMachineError::T04CeilingUnset,
        ByeMachineError::SweepPoolSetIncomplete,
        ByeMachineError::TreasuryNotPerPool,
        ByeMachineError::PrincipalShortfall,
        ByeMachineError::T04Shortfall,
        ByeMachineError::T03Shortfall,
        ByeMachineError::SlippageExceeded,
        ByeMachineError::WeightDesync,
        ByeMachineError::WeightIndexFull,
        ByeMachineError::WeightIndexNotBlank,
        ByeMachineError::ArithmeticFailure,
        ByeMachineError::EscrowAmountZero,
        ByeMachineError::EscrowBalanceShort,
        ByeMachineError::EscrowAccountsMismatch,
    ];

    /// The enum's total variant count. Kept as a literal on its own line because two instruction
    /// modules grep exactly this text out of this file as their own tripwire against a variant
    /// added without being classified here; `every_variant_is_listed_for_iteration` ties it to
    /// `GROUP_SIZES` so the two cannot drift apart.
    const VARIANT_COUNT: usize = 87;

    /// Each group's base and how many variants sit in it. The group is **counted**, not sampled:
    /// the shape this replaced asserted a hand-picked subset of members, so a variant named in no
    /// assertion at all still passed every one of them.
    const GROUP_SIZES: &[(u32, usize)] = &[
        (6100, 9),
        (6200, 16),
        (6300, 12),
        (6400, 5),
        (6500, 34),
        (6600, 4),
        (6700, 4),
        (6800, 3),
    ];

    fn codes_by_group() -> BTreeMap<u32, Vec<u32>> {
        let mut groups: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for variant in ALL {
            let code = expected_code(*variant);
            groups.entry(code - code % 100).or_default().push(code);
        }
        groups
    }

    #[test]
    fn every_variant_is_pinned_to_its_wire_number() {
        for variant in ALL {
            assert_eq!(
                u32::from(*variant),
                expected_code(*variant),
                "{variant:?}'s wire number moved — a variant was inserted into the middle of a \
                 group rather than appended after its last member, which renumbers every code \
                 below it inside the same hundred"
            );
        }
    }

    #[test]
    fn every_group_holds_exactly_its_pinned_count() {
        let groups = codes_by_group();
        assert_eq!(
            groups.keys().copied().collect::<Vec<_>>(),
            GROUP_SIZES
                .iter()
                .map(|(base, _)| *base)
                .collect::<Vec<_>>(),
            "a group was opened or emptied without a pinned size"
        );
        for (base, size) in GROUP_SIZES {
            assert_eq!(
                groups[base].len(),
                *size,
                "group {base} no longer holds {size} variants"
            );
        }
    }

    /// Each group fills its hundred with no gap and no repeat. This is what makes the count above
    /// load-bearing: a count alone passes on a group riddled with holes, and contiguity alone
    /// passes on a group that grew or shrank at its end.
    #[test]
    fn each_group_is_contiguous_from_its_base() {
        for (base, codes) in codes_by_group() {
            let mut sorted = codes.clone();
            sorted.sort_unstable();
            let expected: Vec<u32> = (0..codes.len() as u32).map(|i| base + i).collect();
            assert_eq!(
                sorted, expected,
                "group {base} has a hole or a duplicate — a hole means a variant is missing from \
                 ALL, a duplicate means two variants claim one number"
            );
        }
    }

    /// 6500 stays in the enum for its number only; no production code raises it.
    #[test]
    fn the_retired_floor_error_is_raised_nowhere() {
        use crate::common::test_support::{code, crate_sources};
        let sources = crate_sources();
        assert!(
            sources.len() > 90,
            "the scan found {} sources",
            sources.len()
        );
        for path in sources {
            if path.ends_with("common/errors.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let prod = code(&source);
            assert!(
                !prod.contains("AdmissionFloorNotAboveTicket"),
                "{} raises the retired floor error",
                path.display()
            );
        }
    }

    #[test]
    fn every_variant_is_listed_for_iteration() {
        assert_eq!(
            ALL.len(),
            VARIANT_COUNT,
            "a variant was added to the enum without being listed in ALL"
        );
        assert_eq!(
            VARIANT_COUNT,
            GROUP_SIZES.iter().map(|(_, size)| *size).sum::<usize>(),
            "VARIANT_COUNT and GROUP_SIZES disagree on how many variants the enum has — the \
             per-group sizes are the ones derived from the declarations, so fix the total"
        );
    }
}
