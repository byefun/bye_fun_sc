pub mod claim_nft;
pub mod claim_nft_core;
pub mod close_seized;
pub mod withdraw;
pub mod withdraw_core;

// Only the `Accounts` context types are re-exported; handlers are called by full module path
// in lib.rs, since every one of them is named `handler`.
pub use claim_nft::ClaimNft;
pub use claim_nft_core::ClaimNftCore;
pub use close_seized::CloseSeized;
pub use withdraw::Withdraw;
pub use withdraw_core::WithdrawCore;

#[cfg(test)]
mod tests {
    use crate::common::test_support::production;

    const MOD_SRC: &str = include_str!("mod.rs");
    const DEPOSIT_MOD_SRC: &str = include_str!("../deposit/mod.rs");
    const LIB_SRC: &str = include_str!("../../lib.rs");
    const ERRORS_SRC: &str = include_str!("../../common/errors.rs");

    fn pub_use_lines(source: &str) -> usize {
        production(source)
            .lines()
            .filter(|line| line.trim_start().starts_with("pub use "))
            .count()
    }

    // --- exit/mod.rs re-exports exactly 5 Context types, deposit/mod.rs's shape ----------------

    /// **Five, and the fifth is not an exit.** `close_seized` lives here:
    /// it moves no custody and makes no CPI, so what puts it in this domain is
    /// the terminal it writes and the sibling error message that sends a depositor to it. The
    /// count is the place that fact is visible from the module rather than only from the file's
    /// own doc comment.
    #[test]
    fn re_exports_exactly_five_context_types_matching_deposit_mod_shape() {
        assert_eq!(pub_use_lines(MOD_SRC), 5);
        assert!(
            pub_use_lines(DEPOSIT_MOD_SRC) > 0,
            "shape reference must itself use pub use"
        );
        for re_export in [
            "pub use claim_nft::ClaimNft;",
            "pub use claim_nft_core::ClaimNftCore;",
            "pub use close_seized::CloseSeized;",
            "pub use withdraw::Withdraw;",
            "pub use withdraw_core::WithdrawCore;",
        ] {
            assert!(
                production(MOD_SRC).contains(re_export),
                "missing {re_export}"
            );
        }
    }

    // --- the delegation forms lib.rs must use for this domain -----------------------------------

    /// **Which of the five delegations carries the explicit lifetime pair is decided by the
    /// handler, not by the file it lives in.** A handler that reads `ctx.remaining_accounts`
    /// needs the info and accounts lifetimes unified or the delegation fails E0621, so the two
    /// withdrawals take the explicit form and the three that read no remaining
    /// account stay elided. Both halves are asserted: an elided form on a handler that grew a
    /// `remaining_accounts` read stops compiling, but the reverse — the explicit pair spreading
    /// to a handler that does not need it — compiles silently, and that is what this pin's second
    /// list catches.
    ///
    /// **`CloseSeized` moved to the elided list when the tier fill left it.** It vacates a seat
    /// and no longer fills one, because its caller may be anybody and the candidate's rank is not
    /// checkable on chain — so it has no candidate account to read, and carrying the wider
    /// signature would advertise a slot that is not there.
    #[test]
    fn the_two_delegations_that_read_remaining_accounts_carry_the_explicit_lifetime_pair() {
        let prod = production(LIB_SRC);
        for context in ["Withdraw", "WithdrawCore"] {
            assert!(
                prod.contains(&format!("Context<'_, '_, 'info, 'info, {context}<'info>>")),
                "{context} reads ctx.remaining_accounts and needs the unified lifetimes"
            );
        }
        for context in ["ClaimNft", "ClaimNftCore", "CloseSeized"] {
            assert!(
                prod.contains(&format!("ctx: Context<{context}>")),
                "{context} reads no remaining account and must stay on the elided form"
            );
        }
    }

    // --- the error enum's variant count is pinned -------------------------------------------

    #[test]
    fn error_group_classification_stays_exhaustive_and_at_fifty_five_variants() {
        assert!(ERRORS_SRC.contains("const VARIANT_COUNT: usize = 55;"));
        assert!(
            !ERRORS_SRC.contains("_ => "),
            "expected_code must stay a wildcard-free match"
        );

        for variant in [
            "ByeMachineError::PositionLocked",
            "ByeMachineError::InvalidPositionState",
            "ByeMachineError::NotPositionOwner",
            "ByeMachineError::RollBatchInFlight",
            "ByeMachineError::ArithmeticFailure",
        ] {
            assert!(
                ERRORS_SRC.contains(variant),
                "{variant} is used here and must be classified in ALL/expected_code"
            );
        }
    }
}
