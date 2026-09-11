pub mod begin_sweep;
pub mod end_sweep;
pub mod record_value;

// Only the `Accounts` context types are re-exported; handlers are called by full module path
// in lib.rs, since every one of them is named `handler`.
pub use begin_sweep::BeginSweep;
pub use end_sweep::EndSweep;
pub use record_value::RecordValue;

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

    fn matching_lines(source: &str, needles: &[&str]) -> usize {
        production(source)
            .lines()
            .filter(|line| needles.iter().all(|needle| line.contains(needle)))
            .count()
    }

    // --- value/mod.rs re-exports exactly 3 Context types, deposit/mod.rs's shape ---------------

    #[test]
    fn re_exports_exactly_three_context_types_matching_deposit_mod_shape() {
        assert_eq!(pub_use_lines(MOD_SRC), 3);
        assert!(
            pub_use_lines(DEPOSIT_MOD_SRC) > 0,
            "shape reference must itself use pub use"
        );
        assert!(production(MOD_SRC).contains("pub use begin_sweep::BeginSweep;"));
        assert!(production(MOD_SRC).contains("pub use end_sweep::EndSweep;"));
        assert!(production(MOD_SRC).contains("pub use record_value::RecordValue;"));
    }

    // --- lib.rs gains exactly 3 delegating fns and 3 __client_accounts_* imports ---------------

    #[test]
    fn lib_wires_exactly_three_value_domain_fns_and_client_accounts_imports() {
        let prod = production(LIB_SRC);

        assert_eq!(
            matching_lines(prod, &["instructions::value::", "::handler("]),
            3,
            "expected exactly 3 delegating calls into instructions::value"
        );
        assert_eq!(prod.matches("pub fn begin_sweep(").count(), 1);
        assert_eq!(prod.matches("pub fn record_value<").count(), 1);
        assert_eq!(prod.matches("pub fn end_sweep(").count(), 1);

        assert_eq!(
            matching_lines(prod, &["instructions::value::", "__client_accounts_"]),
            3,
            "expected exactly 3 __client_accounts_* imports for the value domain"
        );
        assert!(prod.contains("instructions::value::begin_sweep::__client_accounts_begin_sweep"));
        assert!(prod.contains("instructions::value::end_sweep::__client_accounts_end_sweep"));
        assert!(prod.contains("instructions::value::record_value::__client_accounts_record_value"));
    }

    #[test]
    fn record_value_carries_the_explicit_lifetime_pair() {
        assert!(production(LIB_SRC).contains("Context<'_, '_, 'info, 'info, RecordValue<'info>>"));
    }

    // --- expected_code still compiles wildcard-free -------------------------------------------

    #[test]
    fn error_group_classification_stays_exhaustive_and_at_fifty_five_variants() {
        assert!(ERRORS_SRC.contains("const VARIANT_COUNT: usize = 55;"));
        assert!(
            !ERRORS_SRC.contains("_ => "),
            "expected_code must stay a wildcard-free match"
        );

        for variant in [
            "ByeMachineError::SweepAlreadyOpen",
            "ByeMachineError::SweepNotOpen",
            "ByeMachineError::ZeroValue",
            "ByeMachineError::InvalidPositionState",
            "ByeMachineError::PositionPoolMismatch",
            "ByeMachineError::ArithmeticFailure",
        ] {
            assert!(
                ERRORS_SRC.contains(variant),
                "{variant} is used here and must be classified in ALL/expected_code"
            );
        }
    }
}
