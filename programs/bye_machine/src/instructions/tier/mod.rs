pub mod update_tier;

// Only the `Accounts` context types are re-exported; handlers are called by full module path
// in lib.rs, since every one of them is named `handler`.
pub use update_tier::UpdateTier;

#[cfg(test)]
mod tests {
    use crate::common::test_support::production;

    const MOD_SRC: &str = include_str!("mod.rs");
    const DEPOSIT_MOD_SRC: &str = include_str!("../deposit/mod.rs");
    const LIB_SRC: &str = include_str!("../../lib.rs");

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

    // --- tier/mod.rs re-exports exactly 1 Context type, deposit/mod.rs's shape ---------------

    #[test]
    fn re_exports_exactly_one_context_type_matching_deposit_mod_shape() {
        assert_eq!(pub_use_lines(MOD_SRC), 1);
        assert!(
            pub_use_lines(DEPOSIT_MOD_SRC) > 0,
            "shape reference must itself use pub use"
        );
        assert!(production(MOD_SRC).contains("pub use update_tier::UpdateTier;"));
    }

    // --- lib.rs gains exactly 1 delegating fn and 1 __client_accounts_* import ---------------

    #[test]
    fn lib_wires_exactly_one_tier_domain_fn_and_client_accounts_import() {
        let prod = production(LIB_SRC);

        assert_eq!(
            matching_lines(prod, &["instructions::tier::", "::handler("]),
            1,
            "expected exactly 1 delegating call into instructions::tier"
        );
        assert_eq!(prod.matches("pub fn update_tier<").count(), 1);

        assert_eq!(
            matching_lines(prod, &["instructions::tier::", "__client_accounts_"]),
            1,
            "expected exactly 1 __client_accounts_* import for the tier domain"
        );
        assert!(prod.contains("instructions::tier::update_tier::__client_accounts_update_tier"));
    }

    /// The displaced member travels in `remaining_accounts`, which needs the info and
    /// accounts lifetimes unified or the delegation fails E0621, same as
    /// `withdraw`.
    #[test]
    fn update_tier_carries_the_explicit_lifetime_pair() {
        assert!(production(LIB_SRC).contains("Context<'_, '_, 'info, 'info, UpdateTier<'info>>"));
    }
}
