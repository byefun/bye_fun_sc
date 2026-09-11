pub mod admin;
pub mod deposit;
pub mod exit;
pub mod roll;
pub mod tier;
pub mod value;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::common::errors::ByeMachineError;
    use crate::common::seeds::PROTOCOL_CONFIG_SEED;
    use crate::common::test_support::{
        accounts_body, calls, is_assignment, production, without_doc_comments,
    };

    // One crate-wide `mut`-declaration enumerator, rather than a pin per instruction file.
    // Without `mut`, a field write lands on the in-memory copy and Anchor never persists the
    // account back — the transaction succeeds and changes nothing on chain. A pin that lives in
    // the file being written misses that: a new handler starts with no such test and nothing
    // notices. This one instead walks every instruction source mechanically and checks its own
    // input list against the domain module count, so an unlisted handler fails this test rather
    // than passing it silently.

    const ADMIN_MOD_SRC: &str = include_str!("admin/mod.rs");
    const DEPOSIT_MOD_SRC: &str = include_str!("deposit/mod.rs");
    const VALUE_MOD_SRC: &str = include_str!("value/mod.rs");
    const EXIT_MOD_SRC: &str = include_str!("exit/mod.rs");
    const TIER_MOD_SRC: &str = include_str!("tier/mod.rs");
    const ROLL_MOD_SRC: &str = include_str!("roll/mod.rs");
    const LIB_SRC: &str = include_str!("../lib.rs");

    const ADMIT_COLLECTION_SRC: &str = include_str!("admin/admit_collection.rs");
    const INIT_POOL_SRC: &str = include_str!("admin/init_pool.rs");
    const INIT_PROTOCOL_SRC: &str = include_str!("admin/init_protocol.rs");
    const SET_AUTHORITIES_SRC: &str = include_str!("admin/set_authorities.rs");
    const SET_PAUSE_SRC: &str = include_str!("admin/set_pause.rs");
    const UPDATE_CONFIG_SRC: &str = include_str!("admin/update_config.rs");
    const UPDATE_VRF_CONFIG_SRC: &str = include_str!("admin/update_vrf_config.rs");
    const WITHDRAW_COLLECTION_SRC: &str = include_str!("admin/withdraw_collection.rs");
    const DEPOSIT_SRC: &str = include_str!("deposit/deposit.rs");
    const DEPOSIT_CORE_SRC: &str = include_str!("deposit/deposit_core.rs");
    const APPROVE_DEPOSIT_SRC: &str = include_str!("deposit/approve_deposit.rs");
    const REJECT_DEPOSIT_SRC: &str = include_str!("deposit/reject_deposit.rs");
    const RETURN_REJECTED_SRC: &str = include_str!("deposit/return_rejected.rs");
    const RETURN_REJECTED_CORE_SRC: &str = include_str!("deposit/return_rejected_core.rs");
    const BEGIN_SWEEP_SRC: &str = include_str!("value/begin_sweep.rs");
    const END_SWEEP_SRC: &str = include_str!("value/end_sweep.rs");
    const RECORD_VALUE_SRC: &str = include_str!("value/record_value.rs");
    const CLAIM_NFT_SRC: &str = include_str!("exit/claim_nft.rs");
    const WITHDRAW_SRC: &str = include_str!("exit/withdraw.rs");
    const CLAIM_NFT_CORE_SRC: &str = include_str!("exit/claim_nft_core.rs");
    const CLOSE_SEIZED_SRC: &str = include_str!("exit/close_seized.rs");
    const WITHDRAW_CORE_SRC: &str = include_str!("exit/withdraw_core.rs");
    const UPDATE_TIER_SRC: &str = include_str!("tier/update_tier.rs");
    const COMMIT_ROLLS_SRC: &str = include_str!("roll/commit_rolls.rs");
    const VRF_CALLBACK_SRC: &str = include_str!("roll/vrf_callback.rs");

    /// Not an instruction either — the MPL Core read surface, where the collateral
    /// classification and the release leg both live. Read here so the claim "close_seized forms
    /// no CPI of its own" can be paired with where the one it is authorised by actually sits.
    const CORE_ASSET_SRC: &str = include_str!("../common/core_asset.rs");

    /// Not an instruction — the accounting half `withdraw` and `withdraw_core` reach the weight
    /// freeze guard through. Deliberately outside `ALL_INSTRUCTIONS`, which is pinned against
    /// the domain `pub mod` count.
    const EXIT_ACCOUNTING_SRC: &str = include_str!("../common/exit_accounting.rs");

    /// Every instruction file in the crate today, paired with a label used in failure
    /// messages. Its length is asserted against the `pub mod` count summed across the five
    /// domain `mod.rs` files below — a handler added to a domain without a matching entry
    /// here fails `the_instruction_list_covers_every_domain_module` instead of passing
    /// silently.
    const ALL_INSTRUCTIONS: &[(&str, &str)] = &[
        ("admin/admit_collection.rs", ADMIT_COLLECTION_SRC),
        ("admin/init_pool.rs", INIT_POOL_SRC),
        ("admin/init_protocol.rs", INIT_PROTOCOL_SRC),
        ("admin/set_authorities.rs", SET_AUTHORITIES_SRC),
        ("admin/set_pause.rs", SET_PAUSE_SRC),
        ("admin/update_config.rs", UPDATE_CONFIG_SRC),
        ("admin/update_vrf_config.rs", UPDATE_VRF_CONFIG_SRC),
        ("admin/withdraw_collection.rs", WITHDRAW_COLLECTION_SRC),
        ("deposit/deposit.rs", DEPOSIT_SRC),
        ("deposit/deposit_core.rs", DEPOSIT_CORE_SRC),
        ("deposit/approve_deposit.rs", APPROVE_DEPOSIT_SRC),
        ("deposit/reject_deposit.rs", REJECT_DEPOSIT_SRC),
        ("deposit/return_rejected.rs", RETURN_REJECTED_SRC),
        ("deposit/return_rejected_core.rs", RETURN_REJECTED_CORE_SRC),
        ("value/begin_sweep.rs", BEGIN_SWEEP_SRC),
        ("value/end_sweep.rs", END_SWEEP_SRC),
        ("value/record_value.rs", RECORD_VALUE_SRC),
        ("exit/claim_nft.rs", CLAIM_NFT_SRC),
        ("exit/withdraw.rs", WITHDRAW_SRC),
        ("exit/claim_nft_core.rs", CLAIM_NFT_CORE_SRC),
        ("exit/close_seized.rs", CLOSE_SEIZED_SRC),
        ("exit/withdraw_core.rs", WITHDRAW_CORE_SRC),
        ("tier/update_tier.rs", UPDATE_TIER_SRC),
        ("roll/commit_rolls.rs", COMMIT_ROLLS_SRC),
        ("roll/vrf_callback.rs", VRF_CALLBACK_SRC),
    ];

    fn pub_mod_count(mod_src: &str) -> usize {
        production(mod_src)
            .lines()
            .filter(|line| line.trim_start().starts_with("pub mod "))
            .count()
    }

    /// The six domain sources below are hardcoded, so a seventh domain would contribute
    /// zero to `domain_total` and zero required `ALL_INSTRUCTIONS` entries — both assertions
    /// would pass while its handlers went unenumerated. Pinning this file's own count makes
    /// declaring a domain fail until it is listed here.
    #[test]
    fn the_domain_list_covers_every_declared_domain() {
        assert_eq!(
            pub_mod_count(include_str!("mod.rs")),
            6,
            "a domain was declared without being added to the six *_MOD_SRC consts below"
        );
    }

    /// The list's **length** is pinned against the domain module count above, but nothing pins
    /// its **entries** on its own — an entry could name one file and read another, a label
    /// repointed at an already-listed source leaves the length correct, every downstream count
    /// unchanged, and one handler swept twice while another goes unswept. One level deeper:
    /// an enumerator is only as strong as the pinning of its input list, and a list is only as
    /// strong as the pinning of its entries.
    ///
    /// Two properties together. Pairwise distinctness catches a duplicate source; the label →
    /// `Accounts` struct correspondence catches a swap between two files that are both still
    /// listed once. Neither alone is sufficient: swapping two labels keeps every source distinct.
    #[test]
    fn every_instruction_entry_reads_the_file_its_label_names() {
        for (i, (label_a, src_a)) in ALL_INSTRUCTIONS.iter().enumerate() {
            for (label_b, src_b) in ALL_INSTRUCTIONS.iter().skip(i + 1) {
                assert!(
                    !std::ptr::eq(*src_a, *src_b),
                    "{label_a} and {label_b} read the same source"
                );
            }

            let stem = label_a
                .rsplit('/')
                .next()
                .unwrap()
                .strip_suffix(".rs")
                .expect("every label names a .rs file");
            let accounts_struct: String = stem
                .split('_')
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                        None => String::new(),
                    }
                })
                .collect();
            assert!(
                production(src_a).contains(&format!("pub struct {accounts_struct}")),
                "{label_a} does not declare `pub struct {accounts_struct}` — its entry reads \
                 some other file's source"
            );
        }
    }

    #[test]
    fn the_instruction_list_covers_every_domain_module() {
        let domain_total = pub_mod_count(ADMIN_MOD_SRC)
            + pub_mod_count(DEPOSIT_MOD_SRC)
            + pub_mod_count(VALUE_MOD_SRC)
            + pub_mod_count(EXIT_MOD_SRC)
            + pub_mod_count(TIER_MOD_SRC)
            + pub_mod_count(ROLL_MOD_SRC);
        assert_eq!(
            domain_total, 25,
            "8 (admin) + 6 (deposit) + 3 (value) + 5 (exit) + 1 (tier) + 2 (roll) = 25 today; \
             a change here means ALL_INSTRUCTIONS is now out of date too"
        );
        assert_eq!(
            ALL_INSTRUCTIONS.len(),
            domain_total,
            "ALL_INSTRUCTIONS is missing a handler that one of the domain mod.rs files declares"
        );
    }

    /// **Every declared `Accounts` context is reachable from the entrypoint.** An `Accounts`
    /// type a domain re-exports and `lib.rs` never delegates to is dead code — it compiles, it
    /// is unit-tested, it has no discriminator and no IDL entry, and nothing says so. A
    /// hand-counted per-domain pin misses that, since it is a number someone has to remember to
    /// raise; this one derives its membership from the `pub use` lines instead.
    ///
    /// Two obligations per context, and both are load-bearing. The delegating call is what gives
    /// the handler a discriminator and an IDL entry. The `__client_accounts_*` import is what
    /// Anchor 0.31.1's `#[program]` macro needs in scope (anchor#2772) — without it the crate
    /// does not build, so it cannot rot silently, but listing it here keeps the two halves of
    /// "wired" stated in one place rather than one in a test and one in a compiler error.
    #[test]
    fn every_declared_accounts_context_is_wired_into_the_entrypoint() {
        let lib = production(LIB_SRC);
        let mut wired = 0usize;

        for (domain, mod_src) in [
            ("admin", ADMIN_MOD_SRC),
            ("deposit", DEPOSIT_MOD_SRC),
            ("value", VALUE_MOD_SRC),
            ("exit", EXIT_MOD_SRC),
            ("tier", TIER_MOD_SRC),
            ("roll", ROLL_MOD_SRC),
        ] {
            for line in production(mod_src).lines() {
                let Some(rest) = line.trim().strip_prefix("pub use ") else {
                    continue;
                };
                let module = rest
                    .split("::")
                    .next()
                    .expect("a `pub use` names a module before `::`");

                assert!(
                    lib.contains(&format!("instructions::{domain}::{module}::handler(")),
                    "instructions::{domain}::{module} declares an `Accounts` context that \
                     lib.rs never delegates to — an unwired handler is dead code with no \
                     discriminator and no IDL entry"
                );
                assert!(
                    lib.contains(&format!(
                        "use instructions::{domain}::{module}::__client_accounts_{module};"
                    )),
                    "instructions::{domain}::{module} has no `__client_accounts_{module}` \
                     import in lib.rs"
                );
                wired += 1;
            }
        }

        // The control: the loop above vacuously passes over an empty `pub use` set, which is
        // exactly what a `production()` filter regression would produce. Every handler in
        // `ALL_INSTRUCTIONS` is re-exported by its domain, so the two counts are the same 24.
        assert_eq!(
            wired,
            ALL_INSTRUCTIONS.len(),
            "the domains re-export {wired} `Accounts` contexts for {} handler files",
            ALL_INSTRUCTIONS.len()
        );
    }

    /// **One instrument over the whole set: both collateral codes, by exact numeric value, on
    /// all three Core exits.**
    ///
    /// The chain has three links and each is asserted here. Membership is **derived** — every
    /// production file that releases a Core asset from the vault — so a fourth Core exit joins
    /// the set by existing rather than by being remembered. Each member calls
    /// `require_collateral_settleable` exactly once, and that call precedes **every** CPI in the
    /// file, which is the claim `withdraw_core` makes non-trivial: its fee payout is a CPI that
    /// runs before the card is touched. And the gate returns exactly `CollateralAbsent` (6307)
    /// or `CollateralFrozen` (6309), stated here by value and driven over every classification
    /// by `core_asset.rs`'s own `no_classification_is_refused_by_both_gates` — which is also
    /// where the gate's third answer lives: on `Undecidable` it **accepts**, because nothing the
    /// program can decide blocks the move and the `TransferV1` two lines later is what finds out.
    #[test]
    fn every_core_exit_gates_its_collateral_before_any_cpi() {
        let core_exits: Vec<_> = ALL_INSTRUCTIONS
            .iter()
            .filter(|(_, source)| calls(source, "release_core_from_vault") > 0)
            .collect();

        assert_eq!(
            core_exits.len(),
            3,
            "the Core exit set is return_rejected_core, withdraw_core and claim_nft_core; a \
             fourth releasing handler owes this instrument nothing but must appear in it"
        );
        let mut labels: Vec<&str> = core_exits.iter().map(|(label, _)| *label).collect();
        labels.sort();
        assert_eq!(
            labels,
            [
                "deposit/return_rejected_core.rs",
                "exit/claim_nft_core.rs",
                "exit/withdraw_core.rs"
            ]
        );

        for (label, source) in &core_exits {
            let prod = production(source);
            assert_eq!(
                calls(source, "require_collateral_settleable"),
                1,
                "{label} must gate its collateral exactly once"
            );
            let gate = prod.find("require_collateral_settleable(").unwrap();

            // Every CPI-forming call in the file, not only the release: a gate that follows one
            // of them returns its named code after a transfer has already been built.
            for cpi in [
                "release_core_from_vault(",
                "transfer(",
                "close_account(",
                "invoke(",
            ] {
                if let Some(at) = prod.find(cpi) {
                    assert!(gate < at, "{label}: the collateral gate must precede {cpi}");
                }
            }

            // And the gate is the only classification consumer here — the inverse belongs to
            // `close_seized`, and an exit that took it would refuse exactly the positions it
            // exists to serve.
            assert_eq!(
                calls(source, "require_collateral_unreachable"),
                0,
                "{label} takes the inverse gate — that is close_seized's, not an exit's"
            );
        }

        assert_eq!(u32::from(ByeMachineError::CollateralAbsent), 6307);
        assert_eq!(u32::from(ByeMachineError::CollateralFrozen), 6309);
    }

    /// **The counterpart instrument, over the inverse gate.** The gate and its inverse are exact
    /// complements over `core_asset.rs`'s classification, so any gap between them is a position
    /// that can neither be withdrawn nor closed — the depositor stuck and the phantom leaf
    /// permanent. That property is derived here rather than asserted: membership on **both**
    /// sides comes from the call, the two sides are required disjoint, and their union is
    /// required to be the whole set of handlers that read the classification at all.
    ///
    /// **The two gates are complements in the weak sense that matters — no classification is
    /// refused by both.** The forward gate establishes only that nothing the program can
    /// *decide* blocks the move; `core_asset.rs`'s plugin census answers `Undecidable` for
    /// everything else, including a `plugin_type` byte a later `mpl-core` ships, and **both**
    /// gates admit that classification — a position both gates admit is served by both.
    ///
    /// `close_seized` is the sole caller of the inverse, it releases nothing, and it forms **no
    /// CPI of any kind** — so it never has a "before the CPI" question to answer.
    /// `CollateralPresent` (6308) sits between 6307 and 6309 by value so a client keys the
    /// causes on adjacent codes.
    #[test]
    fn the_inverse_collateral_gate_has_exactly_one_caller_and_it_forms_no_cpi() {
        let inverse: Vec<_> = ALL_INSTRUCTIONS
            .iter()
            .filter(|(_, source)| calls(source, "require_collateral_unreachable") > 0)
            .collect();
        let forward: Vec<_> = ALL_INSTRUCTIONS
            .iter()
            .filter(|(_, source)| calls(source, "require_collateral_settleable") > 0)
            .collect();

        assert_eq!(
            inverse.len(),
            1,
            "close_seized is the only instruction that acts on collateral being beyond reach"
        );
        let (label, source) = inverse[0];
        assert_eq!(*label, "exit/close_seized.rs");
        assert_eq!(calls(source, "require_collateral_unreachable"), 1);

        // Disjoint: a handler holding both predicates would be one that refuses the positions it
        // serves and serves the positions it refuses.
        for (forward_label, _) in &forward {
            assert_ne!(
                forward_label, label,
                "{forward_label} holds both the gate and its inverse"
            );
        }
        // And exhaustive: 3 + 1, over every handler that reads the classification at all.
        assert_eq!(forward.len() + inverse.len(), 4);

        let code = without_doc_comments(production(source));
        for cpi in [
            "release_core_from_vault(",
            "transfer(",
            "close_account(",
            "invoke(",
        ] {
            assert!(
                !code.contains(cpi),
                "close_seized forms {cpi} — it moves no custody on any source state"
            );
        }
        // Doc comments stripped: `core_asset.rs`'s prose names the call on purpose, to record
        // why the release is not attempted there.
        assert_eq!(
            without_doc_comments(production(CORE_ASSET_SRC))
                .matches("release_core_from_vault(")
                .count(),
            0,
            "and neither does the classification it calls — the release leg is declared in \
             core_asset.rs and called only by the three exits, which is what a CPI measured \
             unrecoverable leaves this module able to claim"
        );
        // The positive control: the leg is still declared, so the absence above is about call
        // sites and not about the function having been deleted. The declaration reads
        // `fn release_core_from_vault<'info>(`, which is why it does not match the form counted.
        assert_eq!(
            production(CORE_ASSET_SRC)
                .matches("pub fn release_core_from_vault")
                .count(),
            1
        );

        assert_eq!(u32::from(ByeMachineError::CollateralPresent), 6308);
    }

    /// One account declared by a `#[derive(Accounts)]` struct: its field name and whether the
    /// `#[account(..)]` attribute immediately preceding it marks the account writable. `init`,
    /// `init_if_needed` and `zero` persist the account exactly as `mut` does — Anchor writes
    /// all four back on exit — so all four count here; a bare `seeds`/`bump`/`constraint`/
    /// `address` account, or a field with no `#[account(..)]` at all, does not.
    struct DeclaredAccount<'a> {
        name: &'a str,
        writable: bool,
    }

    fn declared_accounts(source: &str) -> Vec<DeclaredAccount<'_>> {
        let body = production(source)
            .split("#[derive(Accounts)]")
            .nth(1)
            .expect("every instruction file has exactly one #[derive(Accounts)] struct");

        let mut accounts = Vec::new();
        let mut attr = String::new();
        let mut in_attr = false;

        for line in body.lines() {
            let trimmed = line.trim();
            if in_attr {
                attr.push_str(trimmed);
                if trimmed.ends_with(")]") {
                    in_attr = false;
                }
                continue;
            }
            if trimmed.starts_with("#[account(") {
                attr.clear();
                attr.push_str(trimmed);
                in_attr = !trimmed.ends_with(")]");
                continue;
            }
            if trimmed.starts_with("pub ") && trimmed.contains(':') {
                let name = trimmed
                    .trim_start_matches("pub ")
                    .split(':')
                    .next()
                    .unwrap()
                    .trim();
                let writable =
                    attr.contains("mut") || attr.contains("init") || attr.contains("zero");
                accounts.push(DeclaredAccount { name, writable });
                attr.clear();
            }
        }
        accounts
    }

    /// Whether the handler mutates `ctx.accounts.<name>`, over the forms this tree actually
    /// uses: a direct field write (`ctx.accounts.<name>.<field> = `, tolerant of an indexed
    /// suffix and of the assignment wrapping onto the next line — see
    /// [`test_support::is_assignment`]), a mutable borrow or move (`&mut ctx.accounts.<name>`,
    /// whether bound to a local or passed straight into a function), and the two
    /// `AccountLoader` entry points that hand back a writable guard (`load_mut()`,
    /// `load_init()`).
    ///
    /// **Scope limit — an account written only by a callee is invisible here.** An account
    /// handed to a CPI as `.to_account_info()` is never written Rust-side, so Anchor's
    /// write-back is not in play and none of the forms above match it: `return_rejected`'s
    /// `position_vault` is `mut`-declared and moved by `transfer_pnft`/`close_account` alone,
    /// and dropping its `mut` leaves this test green. That is deliberate rather than a hole to
    /// patch. The class this test exists for is a Rust-side write that Anchor silently discards
    /// — the transaction succeeds and changes nothing on chain — whereas a CPI-passed account
    /// missing `mut` either works unvalidated or fails the CPI loudly. Deciding it from source
    /// would mean modelling each callee's account semantics, and `transfer_pnft` takes
    /// genuinely read-only accounts (`master_edition`, `sysvar_instructions`,
    /// `authorization_rules`) through the same form.
    /// Whether `needle` occurs in `haystack` **not** followed by another identifier character.
    ///
    /// A bare `contains` is a prefix match: with one account named `pool` and another named
    /// `pool_collection` in the same struct, `&mut ctx.accounts.pool_collection` would report
    /// `pool` as mutated. That is a false positive on a *name relationship*, so it appears the
    /// moment two accounts share a prefix and never before.
    fn occurs_at_identifier_boundary(haystack: &str, needle: &str) -> bool {
        let mut from = 0usize;
        while let Some(rel) = haystack[from..].find(needle) {
            let end = from + rel + needle.len();
            let next = haystack[end..].chars().next();
            if !matches!(next, Some(c) if c.is_ascii_alphanumeric() || c == '_') {
                return true;
            }
            from = end;
        }
        false
    }

    fn is_mutated(prod_flat: &str, name: &str) -> bool {
        if occurs_at_identifier_boundary(prod_flat, &format!("&mut ctx.accounts.{name}")) {
            return true;
        }
        if prod_flat.contains(&format!("ctx.accounts.{name}.load_mut()"))
            || prod_flat.contains(&format!("ctx.accounts.{name}.load_init()"))
        {
            return true;
        }

        let prefix = format!("ctx.accounts.{name}.");
        let mut rest = prod_flat;
        while let Some(pos) = rest.find(&prefix) {
            let after = &rest[pos + prefix.len()..];
            let ident_end = after
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            if is_assignment(&after[ident_end..]) {
                return true;
            }
            rest = &after[ident_end..];
        }
        false
    }

    /// Counted over `ALL_INSTRUCTIONS` rather than a separately hand-listed source array — a
    /// hand-listed array with no assertion on its own length lets a `.remove(` or a
    /// `recorded_value = value` added in a new file go invisible and both tests still pass.
    /// `ALL_INSTRUCTIONS`'s length is pinned against the domain module count above, so a handler
    /// added anywhere fails a test rather than escaping the sweep.
    #[test]
    fn remove_has_exactly_four_production_callers_crate_wide() {
        let total: usize = ALL_INSTRUCTIONS
            .iter()
            .map(|(_, src)| production(src).matches(".remove(").count())
            .sum();
        assert_eq!(
            total, 4,
            "record_value's below-floor close, the two withdraw twins' exits and \
             close_seized's Active row are the only production callers of \
             WeightIndex::remove; a fifth leaf-removing site owes this count an update and its \
             own ordered-argument pin. close_seized is the one that reaches it through an \
             `Option`: on its other three source states there is no slot index to remove with, \
             which is what stops the cleanup instruction freeing an already-free leaf"
        );
    }

    /// `deposit`'s own `position.recorded_value = 0;` is the zero-init-duplicating write
    /// `approve_deposit.rs`'s own tests already decline to pin (it duplicates what
    /// `#[account(init)]` zero-initializes for free) — this checks the meaningful writers,
    /// matching `= value`, not `deposit`'s degenerate `= 0`.
    #[test]
    fn recorded_value_has_exactly_two_production_writers_crate_wide() {
        let total: usize = ALL_INSTRUCTIONS
            .iter()
            .map(|(_, src)| production(src).matches("recorded_value = value").count())
            .sum();
        assert_eq!(
            total, 2,
            "approve_deposit and record_value, and no third writer"
        );
    }

    #[test]
    fn every_mutated_account_is_declared_writable_crate_wide() {
        for (label, source) in ALL_INSTRUCTIONS {
            let prod_flat = production(source).replace('\n', " ");
            for account in declared_accounts(source) {
                if is_mutated(&prod_flat, account.name) {
                    assert!(
                        account.writable,
                        "{label}: `{}` is mutated but its #[account(..)] carries none of \
                         mut/init/init_if_needed/zero — Anchor will not persist the write",
                        account.name
                    );
                }
            }
        }
    }

    /// The crate-wide partition over which instructions call `assert_weight_open`, rather than
    /// a per-file enumerator hand-listing which handlers to check. A hand list is only as
    /// current as the last time someone updated it when a new caller was added.
    ///
    /// The classification is a wildcard-free-except-panic match on the label (the `expected_code`
    /// shape in `common/errors.rs`, cited by name): every one of today's 17 labels is listed on
    /// one side or the other, and an 18th added to `ALL_INSTRUCTIONS` without being added here
    /// panics this test rather than silently landing on whichever side is convenient.
    ///
    /// How an instruction reaches the weight freeze. **`Reaches` is not a weaker `Direct`** — it
    /// is the same guard behind one more call: `withdraw`'s accounting half, and the guard with
    /// it, lives in `common/exit_accounting.rs` so `withdraw_core` can share it. Classifying
    /// `withdraw.rs` as a non-caller would record a handler that still refuses a batch in flight
    /// as one that does not check, and the partition's whole purpose is to say which
    /// instructions carry the freeze.
    ///
    /// **`Serializes` is a third ground, not a third strength.** `Direct` and `Reaches` both
    /// name handlers that would otherwise *move* `W` inside an open batch's window.
    /// `commit_rolls` moves none — it calls the identical guard because it would open a second
    /// pipeline whose draws move `W` for the first batch's unrun rolls. Recording it as
    /// `Direct` would make this comment's own account of the partition false.
    #[derive(PartialEq, Eq)]
    enum WeightGuard {
        Direct,
        /// Through `apply_withdraw`, whose own call to the guard is asserted below.
        Reaches,
        /// Calls the guard to keep batches serial, not because it moves weight.
        Serializes,
        None,
    }

    fn weight_guard(label: &str) -> WeightGuard {
        match label {
            "admin/update_config.rs"
            | "deposit/approve_deposit.rs"
            | "value/record_value.rs"
            | "exit/close_seized.rs" => WeightGuard::Direct,
            "exit/withdraw.rs" | "exit/withdraw_core.rs" => WeightGuard::Reaches,
            "roll/commit_rolls.rs" => WeightGuard::Serializes,
            "admin/admit_collection.rs"
            | "admin/init_pool.rs"
            | "admin/init_protocol.rs"
            | "admin/set_authorities.rs"
            | "admin/set_pause.rs"
            | "admin/update_vrf_config.rs"
            | "admin/withdraw_collection.rs"
            | "deposit/deposit.rs"
            | "deposit/deposit_core.rs"
            | "deposit/reject_deposit.rs"
            | "deposit/return_rejected.rs"
            | "value/begin_sweep.rs"
            | "value/end_sweep.rs"
            | "deposit/return_rejected_core.rs"
            | "exit/claim_nft.rs"
            | "exit/claim_nft_core.rs"
            | "roll/vrf_callback.rs"
            | "tier/update_tier.rs" => WeightGuard::None,
            other => panic!("{other} is not classified for the assert_weight_open partition"),
        }
    }

    /// `update_tier` moves no weight, so it carries no freeze guard.
    ///
    /// **Five handlers call the guard, split 4 + 1.** Four call it in their own body —
    /// `update_config`, `approve_deposit`, `record_value`, and `close_seized`'s `Active` row,
    /// the only row of that instruction that departs weight; `withdraw` and `withdraw_core`
    /// reach it through the `apply_withdraw` they share instead — the handler calls
    /// `apply_withdraw` exactly once, that function calls the guard exactly once, and the
    /// handler's own body calls it not at all. A `Reaches` handler that stopped calling
    /// `apply_withdraw`, or an `apply_withdraw` that stopped calling the guard, reds here.
    ///
    /// **`withdraw_core` carries the `open_batches == 0` guard identically to `withdraw`**,
    /// because weight rules know nothing about standards. Sharing the accounting is what makes
    /// that true by construction rather than by a second copy of the assertion. The two claim
    /// twins (`return_rejected_core`, `claim_nft_core`) stay unguarded on the same authority.
    ///
    /// Every other name on the non-caller side is here because it touches no weight at all —
    /// a distinction this classifier cannot draw, so it is drawn here. `vrf_callback` is among
    /// them: it writes randomness and the batch's opening expectation, and moves no weight.
    #[test]
    fn assert_weight_open_partitions_as_seven_guarded_and_eighteen_unguarded() {
        let guarded: Vec<_> = ALL_INSTRUCTIONS
            .iter()
            .filter(|(label, _)| weight_guard(label) != WeightGuard::None)
            .collect();
        let non_callers: Vec<_> = ALL_INSTRUCTIONS
            .iter()
            .filter(|(label, _)| weight_guard(label) == WeightGuard::None)
            .collect();

        assert_eq!(guarded.len(), 7);
        assert_eq!(non_callers.len(), 18);
        assert_eq!(guarded.len() + non_callers.len(), ALL_INSTRUCTIONS.len());

        for (label, source) in &guarded {
            match weight_guard(label) {
                WeightGuard::Direct => assert_eq!(
                    calls(source, "assert_weight_open"),
                    1,
                    "{label} is classified Direct but does not call assert_weight_open exactly \
                     once"
                ),
                WeightGuard::Serializes => assert_eq!(
                    calls(source, "assert_weight_open"),
                    1,
                    "{label} is classified Serializes but does not call assert_weight_open \
                     exactly once — batches would stop serializing"
                ),
                WeightGuard::Reaches => {
                    assert_eq!(
                        calls(source, "apply_withdraw"),
                        1,
                        "{label} is classified Reaches but does not call apply_withdraw exactly \
                         once"
                    );
                    assert_eq!(
                        calls(source, "assert_weight_open"),
                        0,
                        "{label} is classified Reaches but also calls the guard directly — one \
                         of the two is a second guard nobody re-derived"
                    );
                    assert_eq!(
                        calls(EXIT_ACCOUNTING_SRC, "assert_weight_open"),
                        1,
                        "apply_withdraw is the reach path and does not carry the guard"
                    );
                }
                WeightGuard::None => unreachable!("filtered out above"),
            }
        }
        for (label, source) in &non_callers {
            // A deleted or gutted handler satisfies "does not call assert_weight_open" exactly
            // as a correct one does — the absence is only evidence read against a live body.
            // Every slice-1 handler audits its own action, so "emits at least one event" is the
            // generic liveness signal available crate-wide.
            assert!(
                production(source).contains("emit!("),
                "{label} is classified as a non-caller but emits nothing — a x0 over a gutted \
                 handler proves nothing"
            );
            assert_eq!(
                calls(source, "assert_weight_open"),
                0,
                "{label} is classified as a non-caller but calls assert_weight_open"
            );
        }
    }

    /// `RebalanceEvaluated` fires only where a value-changing activation/exit/refresh can move
    /// the blank set — `update_config`'s shrink and `update_tier`'s swap are the two tier
    /// mutations with no rebalance.
    ///
    /// Every Core twin carries its principal's event row unchanged: `withdraw_core`
    /// re-evaluates the blank set exactly as `withdraw` does. The two claim twins
    /// (`return_rejected_core`, `claim_nft_core`) emit none, as their principals do not —
    /// nothing about a closed position's custody release moves weight.
    ///
    /// **The fifth emitter, `close_seized`, emits it conditionally.** It re-evaluates the blank
    /// set on its `Active` source state and on none of the other three, so the per-emitter count
    /// below is the *site* count and not a claim that every execution emits it — an
    /// unconditional emit there would publish a blank-set re-evaluation that did not happen.
    #[test]
    fn rebalance_evaluated_partitions_as_five_emitters() {
        let emitters: Vec<_> = ALL_INSTRUCTIONS
            .iter()
            .filter(|(_, source)| production(source).contains("emit!(RebalanceEvaluated"))
            .collect();
        assert_eq!(emitters.len(), 5);
        for (label, source) in &emitters {
            assert_eq!(
                production(source)
                    .matches("emit!(RebalanceEvaluated")
                    .count(),
                1,
                "{label} emits RebalanceEvaluated more than once"
            );
        }
        for (label, source) in ALL_INSTRUCTIONS {
            if !emitters.iter().any(|(l, _)| l == label) {
                assert!(
                    production(source).contains("emit!("),
                    "{label} is classified as a non-emitter but emits nothing at all"
                );
            }
        }
    }

    /// One `TierChanged` per tier mutation: `update_config`'s shrink (per member demoted),
    /// `approve_deposit`/`record_value`'s crossing, `withdraw`'s exit, `update_tier`'s
    /// promotion/swap, and `withdraw_core`'s.
    ///
    /// The seventh: a seized `Active` member vacates its tier seat exactly as a withdrawing one
    /// does, and takes the same optional successor from `remaining_accounts`. Doubly conditional
    /// there — `Active` source *and* the position was a member — which is why this instrument
    /// counts emit sites rather than executions.
    #[test]
    fn tier_changed_partitions_as_seven_emitters() {
        let emitters: Vec<_> = ALL_INSTRUCTIONS
            .iter()
            .filter(|(_, source)| production(source).contains("emit!(TierChanged"))
            .collect();
        assert_eq!(emitters.len(), 7);
        for (label, source) in &emitters {
            assert!(
                production(source).contains("emit!(TierChanged"),
                "{label} does not emit TierChanged"
            );
        }
        for (label, source) in ALL_INSTRUCTIONS {
            if !emitters.iter().any(|(l, _)| l == label) {
                assert!(
                    production(source).contains("emit!("),
                    "{label} is classified as a non-emitter but emits nothing at all"
                );
            }
        }
    }

    // --- the accounts struct's declaration text, unpinned until here --------------------------
    //
    // Two gaps, one root cause: a swapped field *type* (a documented `UncheckedAccount`
    // substituting for `Signer`) and a swapped *attribute* (`seeds = [...]`, `bump = X`) are both
    // the same unpinned surface — the accounts struct's declaration text — so this lands as one
    // instrument, not two. `init_pool`'s `principal_vault`/`treasury_04` attribute
    // blocks are byte-identical but for one seed constant, so a per-account `contains` pin on
    // either block (`update_tier.rs`'s `every_account_attribute_block_is_pinned_verbatim`
    // shape) stays green when the two are swapped and custody silently exchanges. That is why
    // this is a whole-body `assert_eq!`, crate-wide, rather than an extension of that template.

    /// The literal counterpart of `ALL_INSTRUCTIONS`: one verbatim copy of each
    /// handler's `#[derive(Accounts)]` body, compared for exact equality (not
    /// `contains`) in `every_accounts_struct_is_pinned_verbatim` below. Equality sees
    /// text added *between* accounts (a spurious attribute, an inserted account with
    /// no `#[account(..)]`) that `contains` cannot, and a reformat is a real change to
    /// what this pins, not noise to tolerate — no normalisation, raw text only.
    fn expected_accounts_body(label: &str) -> &'static str {
        const INIT_POOL_ACCOUNTS: &str = r#"
pub struct InitPool<'info> {
    #[account(mut)]
    pub administrator: Signer<'info>,

    #[account(
        mut,
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        init,
        payer = administrator,
        space = 8 + Pool::INIT_SPACE,
        seeds = [POOL_SEED, protocol_config.pool_counter.to_le_bytes().as_ref()],
        bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(zero)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    #[account(
        init,
        payer = administrator,
        space = 8 + TopTier::INIT_SPACE,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(
        init,
        payer = administrator,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = pool
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init,
        payer = administrator,
        seeds = [TREASURY_04_SEED, pool.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = pool
    )]
    pub treasury_04: Box<Account<'info, TokenAccount>>,

    #[account(address = protocol_config.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}"#;
        const INIT_PROTOCOL_ACCOUNTS: &str = r#"
pub struct InitProtocol<'info> {
    // Pinned to `INITIAL_ADMINISTRATOR`, and becomes `protocol_config.administrator` below.
    // The `init` on `protocol_config` already makes this instruction once-only; what it does NOT
    // do is say *who* — deploy and init are separate transactions, and an unpinned `payer` hands
    // the root of the privilege set to whoever signs first in that window.
    //
    // Two reasons this is a `//` comment and a `constraint`, not a `///` doc and an `address =`:
    // anchor emits BOTH doc comments and `address =` constants into the IDL, `target/idl/
    // bye_machine.json` is tracked, and `INITIAL_ADMINISTRATOR` varies per build feature — so
    // either spelling would make the committed IDL environment-specific and force a regeneration
    // commit on every build-flag change. See `program_id.rs`.
    #[account(
        mut,
        constraint = payer.key() == INITIAL_ADMINISTRATOR @ ByeMachineError::Unauthorized,
    )]
    pub payer: Signer<'info>,

    #[account(
        init,
        payer = payer,
        space = 8 + ProtocolConfig::INIT_SPACE,
        seeds = [PROTOCOL_CONFIG_SEED],
        bump
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    pub system_program: Program<'info, System>,
}"#;
        const SET_AUTHORITIES_ACCOUNTS: &str = r#"
pub struct SetAuthorities<'info> {
    pub signer: Signer<'info>,

    #[account(mut, seeds = [PROTOCOL_CONFIG_SEED], bump = protocol_config.bump)]
    pub protocol_config: Account<'info, ProtocolConfig>,
}"#;
        const SET_PAUSE_ACCOUNTS: &str = r#"
pub struct SetPause<'info> {
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
}"#;
        const UPDATE_CONFIG_ACCOUNTS: &str = r#"
pub struct UpdateConfig<'info> {
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
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
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,
}"#;
        const DEPOSIT_ACCOUNTS: &str = r#"
pub struct Deposit<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// The admission record for the collection the presented asset declares. Its address is
    /// re-derived in the handler from this pool's key and the collection the *asset's* metadata
    /// names, so neither another pool's record for this collection nor this pool's record for
    /// another collection is accepted.
    pub pool_collection: Box<Account<'info, PoolCollection>>,

    #[account(
        init,
        payer = depositor,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, pool.key().as_ref(), nft_mint.key().as_ref()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        init,
        payer = depositor,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump,
        token::mint = nft_mint,
        token::authority = position
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = depositor,
        space = 8 + WalletStats::INIT_SPACE,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), depositor.key().as_ref()],
        bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    pub nft_mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        constraint = depositor_token.mint == nft_mint.key() @ ByeMachineError::StandardNotAdmitted,
        constraint = depositor_token.owner == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub depositor_token: Box<Account<'info, TokenAccount>>,

    /// CHECK: owner, PDA derivation and contents are asserted by `validate_admission`.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: owner, PDA derivation and contents are asserted by `validate_admission`.
    pub master_edition: UncheckedAccount<'info>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1, which is a condition no account
    /// constraint can express — the standard is not resolved until the handler reads the
    /// metadata.
    #[account(mut)]
    pub depositor_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `depositor_token_record`.
    #[account(mut)]
    pub position_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the Token Metadata program this pool's pNFTs are governed by.
    #[account(address = TOKEN_METADATA_ID)]
    pub token_metadata_program: UncheckedAccount<'info>,

    /// CHECK: optional, forwarded to `TransferV1`, which pins it to Token Auth Rules.
    pub authorization_rules_program: Option<UncheckedAccount<'info>>,

    /// CHECK: optional, forwarded to `TransferV1`, which asserts it matches the mint's rule set.
    pub authorization_rules: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the instructions sysvar `TransferV1` reads for its CPI guard. Optional on
    /// the same terms as the two token records — SPL `Transfer` has no CPI guard to read it.
    #[account(address = SYSVAR_INSTRUCTIONS_ID)]
    pub sysvar_instructions: Option<UncheckedAccount<'info>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}"#;
        const DEPOSIT_CORE_ACCOUNTS: &str = r#"
pub struct DepositCore<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// The admission record for the collection the presented asset declares. Its address is
    /// re-derived in the handler from this pool's key and the collection the *asset* names, so
    /// neither another pool's record for this collection nor this pool's record for another
    /// collection is accepted.
    pub pool_collection: Box<Account<'info, PoolCollection>>,

    #[account(
        init,
        payer = depositor,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, pool.key().as_ref(), asset.key().as_ref()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: the escrow identity, and **not an account**. Derived here so Anchor pins the address
    /// and yields the `vault_bump` the exit path signs with; it is deliberately never `init`,
    /// because a Core asset needs no token account and allocating one would create rent nobody can
    /// reclaim. Its only on-chain expression is `AssetV1.owner`.
    ///
    /// The sentence above names `vault_bump` in full on purpose: the crate-wide PDA-declaration
    /// pin reads this struct's raw text, doc comments included, and counts the bare suffix.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    #[account(
        init_if_needed,
        payer = depositor,
        space = 8 + WalletStats::INIT_SPACE,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), depositor.key().as_ref()],
        bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    /// CHECK: owner program, discriminator and contents are asserted by `read_core_asset`; the
    /// handler then pins its owner to the depositor. Writable because `TransferV1` rewrites
    /// `AssetV1.owner`.
    #[account(mut)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: pinned in the handler to the collection the asset itself declares, which is also the
    /// collection the admission record was derived for.
    pub collection: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}"#;

        const APPROVE_DEPOSIT_ACCOUNTS: &str = r#"
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
}"#;
        const REJECT_DEPOSIT_ACCOUNTS: &str = r#"
pub struct RejectDeposit<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump
    )]
    pub position: Box<Account<'info, Position>>,
}"#;
        const RETURN_REJECTED_ACCOUNTS: &str = r#"
pub struct ReturnRejected<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: pinned to the position's recorded depositor; receives the NFT and the rent.
    #[account(mut)]
    pub depositor: UncheckedAccount<'info>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // This instruction serves the Token Metadata family only. On the accounts struct rather
        // than in the handler so all three exits carry the guard in the same place and it runs
        // before any CPI — which is load-bearing in `withdraw`, where the fee payout moves USDC
        // before the card is touched, and uniform here so the three do not drift into two shapes.
        constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)
            @ ByeMachineError::WrongStandardForInstruction,
        constraint = position.state == PositionState::Rejected @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        mut,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub nft_mint: Box<Account<'info, Mint>>,

    /// CHECK: created as the depositor's associated token account by `TransferV1` when empty,
    /// and checked against `depositor` by that same processor when it already exists.
    #[account(mut)]
    pub depositor_token: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    pub master_edition: UncheckedAccount<'info>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1.
    #[account(mut)]
    pub position_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `position_token_record`.
    #[account(mut)]
    pub depositor_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the Token Metadata program this pool's pNFTs are governed by.
    #[account(address = TOKEN_METADATA_ID)]
    pub token_metadata_program: UncheckedAccount<'info>,

    /// CHECK: optional, forwarded to `TransferV1`, which pins it to Token Auth Rules.
    pub authorization_rules_program: Option<UncheckedAccount<'info>>,

    /// CHECK: optional, forwarded to `TransferV1`, which asserts it matches the mint's rule set.
    pub authorization_rules: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the instructions sysvar `TransferV1` reads for its CPI guard. Optional on
    /// the same terms as the two token records — SPL `Transfer` has no CPI guard to read it.
    #[account(address = SYSVAR_INSTRUCTIONS_ID)]
    pub sysvar_instructions: Option<UncheckedAccount<'info>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}"#;

        const RETURN_REJECTED_CORE_ACCOUNTS: &str = r#"
pub struct ReturnRejectedCore<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: pinned to the position's recorded depositor; receives the asset and the rent.
    #[account(mut)]
    pub depositor: UncheckedAccount<'info>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // This instruction serves the MPL Core family only, and the assert is on the accounts
        // struct so it runs before any CPI — the same placement all three Token Metadata exits
        // use for their own bound, so the six do not drift into two shapes.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction,
        constraint = position.state == PositionState::Rejected @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: the escrow identity, and **not an account** — its only on-chain expression is
    /// `AssetV1.owner`. Derived here so Anchor pins the address the collateral gate reads against
    /// and so the seeds the release signs under are this position's own. Never `init` and never
    /// `mut`: there is nothing to allocate and nothing to write.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    /// CHECK: **the position's own card, and this pin is the one the collateral gate cannot
    /// make.** `read_collateral` reads whatever account it is handed, so without this address
    /// the caller chooses which asset is tested — and a permissionless instruction would then
    /// accept any settleable Core asset in place of a burned one. Writable because `TransferV1`
    /// rewrites `AssetV1.owner`.
    #[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: the collection the asset itself declares, checked by the collateral gate rather
    /// than here — `Position` stores no collection, so the only truth to compare against is the
    /// asset's own `update_authority`, which account validation cannot read. The slot is always
    /// present because MPL Core needs the account to run the collection's plugins over the
    /// transfer; **what reaches the CPI is what the gate returns**, which is `None` for an asset
    /// that declares no collection — a state `mpl-core 0.11.1` gives no way to reach, and one the
    /// exits handle rather than assume away. On such an asset this account would be unread.
    pub collection: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}"#;
        const BEGIN_SWEEP_ACCOUNTS: &str = r#"
pub struct BeginSweep<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
}"#;
        const END_SWEEP_ACCOUNTS: &str = r#"
pub struct EndSweep<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
}"#;
        const RECORD_VALUE_ACCOUNTS: &str = r#"
pub struct RecordValue<'info> {
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
}"#;
        const CLAIM_NFT_ACCOUNTS: &str = r#"
pub struct ClaimNft<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // This instruction serves the Token Metadata family only. On the accounts struct rather
        // than in the handler so all three exits carry the guard in the same place and it runs
        // before any CPI — which is load-bearing in `withdraw`, where the fee payout moves USDC
        // before the card is touched, and uniform here so the three do not drift into two shapes.
        constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)
            @ ByeMachineError::WrongStandardForInstruction,
        // **One terminal, because one terminal is reachable.** `Seized` is written by
        // `close_seized` alone, which asserts `standard == 2`; this instruction serves `{0, 1}`,
        // so the two constraints can never both hold and a `Seized` arm here could not run.
        //
        // **The Token Metadata family has no seizure terminal at all, and that is a recorded gap
        // rather than an oversight.** The mechanism that would strand a pNFT is a Metaplex
        // rule-set revision that denies the exit — `rule-set-replay` measures the nine live
        // revisions and none of them does, which is the whole basis for calling it inoperative.
        // If one ever did, the position would hold weight behind a card the exits cannot move
        // and **nothing** would clear it: `close_seized` refuses the standard, and widening it is
        // a new release leg, a new account list and a decision, not a match arm. This needs
        // ruling before this family goes to mainnet.
        constraint = position.state == PositionState::ClosedBelowFloor
            @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        mut,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub nft_mint: Box<Account<'info, Mint>>,

    /// CHECK: created as the depositor's associated token account by `TransferV1` when empty,
    /// and checked against `depositor` by that same processor when it already exists.
    #[account(mut)]
    pub depositor_token: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    pub master_edition: UncheckedAccount<'info>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1.
    #[account(mut)]
    pub position_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `position_token_record`.
    #[account(mut)]
    pub depositor_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the Token Metadata program this pool's pNFTs are governed by.
    #[account(address = TOKEN_METADATA_ID)]
    pub token_metadata_program: UncheckedAccount<'info>,

    /// CHECK: optional, forwarded to `TransferV1`, which pins it to Token Auth Rules.
    pub authorization_rules_program: Option<UncheckedAccount<'info>>,

    /// CHECK: optional, forwarded to `TransferV1`, which asserts it matches the mint's rule set.
    pub authorization_rules: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the instructions sysvar `TransferV1` reads for its CPI guard. Optional on
    /// the same terms as the two token records — SPL `Transfer` has no CPI guard to read it.
    #[account(address = SYSVAR_INSTRUCTIONS_ID)]
    pub sysvar_instructions: Option<UncheckedAccount<'info>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}"#;

        const CLAIM_NFT_CORE_ACCOUNTS: &str = r#"
pub struct ClaimNftCore<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, position.pool.as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // The MPL Core family only, on the accounts struct so it runs before any
        // CPI — the placement every twin and principal shares.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction,
        // Deactivation keeps the account whatever closed it, so the claim is callable from
        // either terminal. Unlike `claim_nft`'s widening, **both arms are live here** — `Seized`
        // is written only by `close_seized`, which asserts `standard == 2`, so every seized
        // position is one this instruction serves and no other can.
        constraint = matches!(
            position.state,
            PositionState::ClosedBelowFloor | PositionState::Seized
        ) @ ByeMachineError::InvalidPositionState
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: the escrow identity, and **not an account** — its only on-chain expression is
    /// `AssetV1.owner`. Derived here so Anchor pins the address the collateral gate reads against
    /// and so the seeds the release signs under are this position's own. Never `init` and never
    /// `mut`: there is nothing to allocate and nothing to write.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    /// CHECK: **the position's own card, and this pin is the one the collateral gate cannot
    /// make.** `read_collateral` reads whatever account it is handed, so without this address a
    /// depositor could present a settleable asset of their own and claim against a position
    /// whose card is elsewhere. Writable because `TransferV1` rewrites `AssetV1.owner`.
    #[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: pinned in the handler to the collection the asset itself declares. Required because
    /// MPL Core needs the collection account present to run its plugins over the transfer.
    pub collection: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}"#;
        const CLOSE_SEIZED_ACCOUNTS: &str = r#"
pub struct CloseSeized<'info> {
    pub caller: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        // The MPL Core family only. On the accounts struct, as every twin and
        // principal places it, so it runs before the collateral read.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction,
        // Every state whose card is still in the vault, which is every state but `Seized`
        // itself — escrow begins before activation, outlives closure, and outlives rejection.
        // The admitted set is derived from one rule rather than listed: a
        // sibling that refuses a burned or frozen card returns 6307/6309 **naming this
        // instruction**, so any state a sibling can strand has to be a state this instruction
        // accepts. `ClosedBelowFloor` is `claim_nft_core`'s, and `Rejected` is
        // `return_rejected_core`'s.
        constraint = matches!(
            position.state,
            PositionState::Pending
                | PositionState::Active
                | PositionState::ClosedBelowFloor
                | PositionState::Rejected
        ) @ ByeMachineError::InvalidPositionState
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

    /// CHECK: the escrow identity, and **not an account** — its only on-chain expression is
    /// `AssetV1.owner`, which is precisely what the collateral read compares against. Derived
    /// here so that comparison is against this position's own vault. Never `mut`: nothing signs
    /// under these seeds here, because nothing is transferred.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    /// CHECK: **the position's own card, and this pin is the one the collateral read cannot make
    /// for itself.** `read_collateral` classifies whatever account it is handed, so without this
    /// address a caller could present some unrelated burned asset and seize a position whose
    /// card is safely escrowed. Not `mut`: this instruction reads the asset and never moves it.
    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: the asset's collection, whose **own** plugin registry is half the freeze read —
    /// `permanent_freeze_delegate` sits on either an asset or a collection, and this pool's live
    /// one sits on the collection, held by a third party. Without this
    /// account the seizure this instruction exists for is undetectable: the card is present,
    /// vault-owned and unfrozen in its own registry, so the read would answer `Settleable` and
    /// refuse to close a position whose card MPL Core will not move.
    ///
    /// **Pinned in the classification rather than here, and it has to be.** `Position` stores no
    /// collection, so there is no stored address for `#[account(address = ..)]` to compare
    /// against; the only truth is the asset's own `update_authority`, which is not readable at
    /// account-validation time. `read_collateral` requires this account to be exactly that
    /// collection before it walks the registry, which is what stops a caller naming any frozen
    /// collection and seizing a healthy position with it. Never `mut`: read only, and no CPI.
    ///
    /// **The census walks this account's registry as well as the asset's**, so it is not only
    /// where a freeze can hide — it is where a rejecting `Royalties` rule set or an external
    /// adapter that declares a Transfer check can hide too, and either of those is what makes
    /// the classification `Undecidable` rather than `Settleable`. A `Royalties` record whose
    /// rule set is `None` hides nothing and is `Clear`: the live production collection carries
    /// one, and reading it by plugin type rather than by rule set is what once put the whole
    /// cohort on the undecidable arm.
    pub collection: UncheckedAccount<'info>,
}"#;
        const WITHDRAW_ACCOUNTS: &str = r#"
pub struct Withdraw<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // This instruction serves the Token Metadata family only, and the assert is on the
        // accounts struct so it runs **before any CPI** — the fee payout moves USDC out of the
        // principal vault before this handler touches the card, so a handler-side check would
        // come after a transfer that had already happened.
        constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)
            @ ByeMachineError::WrongStandardForInstruction
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

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    /// Optional so that withdrawal availability never depends on the depositor holding a USDC
    /// account: the payout is structurally zero today, and requiring the destination to exist
    /// for a transfer that never runs would gate an exit on something that has no permission to
    /// gate it. Required the moment the payout is non-zero.
    #[account(
        mut,
        constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub depositor_usdc: Option<Box<Account<'info, TokenAccount>>>,

    #[account(
        mut,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub nft_mint: Box<Account<'info, Mint>>,

    /// CHECK: created as the depositor's associated token account by `TransferV1` when empty,
    /// and checked against `depositor` by that same processor when it already exists.
    #[account(mut)]
    pub depositor_token: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    pub master_edition: UncheckedAccount<'info>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1.
    #[account(mut)]
    pub position_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `position_token_record`.
    #[account(mut)]
    pub depositor_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the Token Metadata program this pool's pNFTs are governed by.
    #[account(address = TOKEN_METADATA_ID)]
    pub token_metadata_program: UncheckedAccount<'info>,

    /// CHECK: optional, forwarded to `TransferV1`, which pins it to Token Auth Rules.
    pub authorization_rules_program: Option<UncheckedAccount<'info>>,

    /// CHECK: optional, forwarded to `TransferV1`, which asserts it matches the mint's rule set.
    pub authorization_rules: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the instructions sysvar `TransferV1` reads for its CPI guard. Optional on
    /// the same terms as the two token records — SPL `Transfer` has no CPI guard to read it.
    #[account(address = SYSVAR_INSTRUCTIONS_ID)]
    pub sysvar_instructions: Option<UncheckedAccount<'info>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}"#;

        const WITHDRAW_CORE_ACCOUNTS: &str = r#"
pub struct WithdrawCore<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // The MPL Core family only, and on the accounts struct so it runs before any CPI — this
        // handler moves USDC out of the principal vault before it touches the card, so a
        // handler-side check would come after a transfer that had already happened.
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction
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

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    /// Optional on exactly its principal's terms: the payout is structurally zero today, and
    /// requiring a USDC account for a transfer that never runs would gate an exit on something
    /// that has no permission to gate it. Required the moment the payout is non-zero.
    #[account(
        mut,
        constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub depositor_usdc: Option<Box<Account<'info, TokenAccount>>>,

    /// CHECK: the escrow identity, and **not an account** — its only on-chain expression is
    /// `AssetV1.owner`. Derived here so Anchor pins the address the collateral gate reads against
    /// and so the seeds the release signs under are this position's own. Never `init` and never
    /// `mut`: there is nothing to allocate and nothing to write.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    /// CHECK: **the position's own card, and this pin is the one the collateral gate cannot
    /// make.** `read_collateral` reads whatever account it is handed, so without this address a
    /// depositor could present a settleable asset of their own and withdraw a position whose
    /// card has been seized — taking the weight out and leaving the pool's own record of the
    /// seizure unwritten. Writable because `TransferV1` rewrites `AssetV1.owner`.
    #[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: pinned in the handler to the collection the asset itself declares. Required because
    /// MPL Core needs the collection account present to run its plugins over the transfer.
    pub collection: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}"#;
        const UPDATE_TIER_ACCOUNTS: &str = r#"
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
}"#;

        const ADMIT_COLLECTION_ACCOUNTS: &str = r#"
#[instruction(collection: Pubkey)]
pub struct AdmitCollection<'info> {
    #[account(mut)]
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    #[account(
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,

    #[account(
        init_if_needed,
        payer = administrator,
        space = 8 + PoolCollection::INIT_SPACE,
        seeds = [COLLECTION_SEED, pool.key().as_ref(), collection.as_ref()],
        bump
    )]
    pub pool_collection: Account<'info, PoolCollection>,

    pub system_program: Program<'info, System>,
}"#;

        const UPDATE_VRF_CONFIG_ACCOUNTS: &str = r#"
pub struct UpdateVrfConfig<'info> {
    pub administrator: Signer<'info>,

    #[account(
        mut,
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,
}"#;

        const WITHDRAW_COLLECTION_ACCOUNTS: &str = r#"
#[instruction(collection: Pubkey)]
pub struct WithdrawCollection<'info> {
    #[account(mut)]
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    #[account(
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,

    #[account(
        mut,
        close = administrator,
        seeds = [COLLECTION_SEED, pool.key().as_ref(), collection.as_ref()],
        bump = pool_collection.bump
    )]
    pub pool_collection: Account<'info, PoolCollection>,
}"#;

        const COMMIT_ROLLS_ACCOUNTS: &str = r#"
pub struct CommitRolls<'info> {
    #[account(mut)]
    pub roller: Signer<'info>,

    #[account(seeds = [PROTOCOL_CONFIG_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    /// Seeded on the pool's *pre-increment* counter, so the same roller committing twice in a
    /// row cannot land on one address: `init` fails closed if the counter ever repeats.
    #[account(
        init,
        payer = roller,
        space = 8 + RollBatch::INIT_SPACE,
        seeds = [BATCH_SEED, pool.key().as_ref(), pool.batch_counter.to_le_bytes().as_ref()],
        bump
    )]
    pub batch: Box<Account<'info, RollBatch>>,

    #[account(
        mut,
        constraint = roller_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = roller_usdc.owner == roller.key() @ ByeMachineError::Unauthorized
    )]
    pub roller_usdc: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = protocol_config.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    /// CHECK: the provider's queue, pinned to the one the protocol config names. Written by the
    /// request CPI — both the enqueued item and the 0.0005 SOL fee land here.
    #[account(mut, address = protocol_config.oracle_queue)]
    pub oracle_queue: UncheckedAccount<'info>,

    /// CHECK: the request-side identity PDA. Derived rather than address-pinned so it follows
    /// the program id across environments, and signs the request CPI.
    #[account(seeds = [VRF_IDENTITY_SEED], bump)]
    pub program_identity: UncheckedAccount<'info>,

    pub vrf_program: Program<'info, VrfProgram>,

    /// CHECK: the sysvar the provider reads its request entropy from.
    #[account(address = anchor_lang::solana_program::sysvar::slot_hashes::ID)]
    pub slot_hashes: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}"#;
        const VRF_CALLBACK_ACCOUNTS: &str = r#"
pub struct VrfCallback<'info> {
    /// Declared rather than left to `#[vrf_callback]` to inject, so the one signer is visible
    /// in this file and to the crate-wide source pins — an injected field is invisible to them,
    /// and a pinned body that omitted the signer would understate the very account surface the
    /// pin exists to fix. The constraint is the SDK's own derivation, unchanged.
    ///
    /// The macro is kept even though this declaration leaves it nothing to inject: it is the
    /// fail-safe. Delete this field and the macro puts the signer back; delete the macro too
    /// and the callback becomes invocable by anyone, silently.
    #[account(address = ephemeral_vrf_sdk::consts::scoped_vrf_identity(&crate::ID))]
    pub vrf_program_identity: Signer<'info>,

    #[account(
        mut,
        seeds = [BATCH_SEED, batch.pool.as_ref(), batch.batch_id.to_le_bytes().as_ref()],
        bump = batch.bump
    )]
    pub batch: Box<Account<'info, RollBatch>>,

    #[account(
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump,
        constraint = pool.key() == batch.pool @ ByeMachineError::BatchPoolMismatch
    )]
    pub pool: Box<Account<'info, Pool>>,
}"#;
        match label {
            "admin/admit_collection.rs" => ADMIT_COLLECTION_ACCOUNTS,
            "admin/init_pool.rs" => INIT_POOL_ACCOUNTS,
            "admin/init_protocol.rs" => INIT_PROTOCOL_ACCOUNTS,
            "admin/set_authorities.rs" => SET_AUTHORITIES_ACCOUNTS,
            "admin/set_pause.rs" => SET_PAUSE_ACCOUNTS,
            "admin/update_config.rs" => UPDATE_CONFIG_ACCOUNTS,
            "admin/update_vrf_config.rs" => UPDATE_VRF_CONFIG_ACCOUNTS,
            "admin/withdraw_collection.rs" => WITHDRAW_COLLECTION_ACCOUNTS,
            "deposit/deposit.rs" => DEPOSIT_ACCOUNTS,
            "deposit/deposit_core.rs" => DEPOSIT_CORE_ACCOUNTS,
            "deposit/approve_deposit.rs" => APPROVE_DEPOSIT_ACCOUNTS,
            "deposit/reject_deposit.rs" => REJECT_DEPOSIT_ACCOUNTS,
            "deposit/return_rejected.rs" => RETURN_REJECTED_ACCOUNTS,
            "deposit/return_rejected_core.rs" => RETURN_REJECTED_CORE_ACCOUNTS,
            "value/begin_sweep.rs" => BEGIN_SWEEP_ACCOUNTS,
            "value/end_sweep.rs" => END_SWEEP_ACCOUNTS,
            "value/record_value.rs" => RECORD_VALUE_ACCOUNTS,
            "exit/claim_nft.rs" => CLAIM_NFT_ACCOUNTS,
            "exit/withdraw.rs" => WITHDRAW_ACCOUNTS,
            "exit/claim_nft_core.rs" => CLAIM_NFT_CORE_ACCOUNTS,
            "exit/close_seized.rs" => CLOSE_SEIZED_ACCOUNTS,
            "exit/withdraw_core.rs" => WITHDRAW_CORE_ACCOUNTS,
            "tier/update_tier.rs" => UPDATE_TIER_ACCOUNTS,
            "roll/commit_rolls.rs" => COMMIT_ROLLS_ACCOUNTS,
            "roll/vrf_callback.rs" => VRF_CALLBACK_ACCOUNTS,
            other => panic!(
                "{other} has no pinned accounts-struct literal in expected_accounts_body \
                 — a 23rd handler must fail this match, not read some other handler's \
                 literal"
            ),
        }
    }

    /// The whole-body counterpart to `expected_accounts_body`. `contains` cannot
    /// see text added *between* accounts, which is where a `/// CHECK:` comment moved to the
    /// wrong account would hide, and cannot see a field-agnostic block swapped onto the wrong account (`init_pool`'s
    /// two custody vaults). Equality closes both by construction, over every handler
    /// `ALL_INSTRUCTIONS` lists — extent derived from its length, never a literal count.
    ///
    /// **When this fails:** update the literal in `expected_accounts_body` to match the struct's
    /// new text — never delete or weaken this assertion. A failure here is this instrument doing
    /// its job; confirm the change to the account surface is intended before updating the
    /// literal.
    #[test]
    fn every_accounts_struct_is_pinned_verbatim() {
        let mut swept = 0;
        for (label, source) in ALL_INSTRUCTIONS {
            assert_eq!(
                accounts_body(source),
                expected_accounts_body(label),
                "{label}: the accounts struct changed — a field, type or attribute was added, \
                 removed, substituted or reordered. If intended, update the literal; do not \
                 delete or weaken this assertion."
            );
            swept += 1;
        }
        assert_eq!(swept, ALL_INSTRUCTIONS.len());
    }

    /// The *type* dimension, crate-wide and by construction — no authority knowledge
    /// needed. Every handler declares exactly one `pub <name>: Signer<'info>`; a substitution to
    /// a documented `UncheckedAccount<'info>` drops this to zero for that
    /// handler rather than passing silently.
    #[test]
    fn exactly_one_signer_per_handler() {
        let total: usize = ALL_INSTRUCTIONS
            .iter()
            .map(|(label, source)| {
                let count = production(source).matches(": Signer<'info>").count();
                assert_eq!(
                    count, 1,
                    "{label}: expected exactly one Signer<'info> field"
                );
                count
            })
            .sum();
        assert_eq!(total, ALL_INSTRUCTIONS.len());
    }

    // --- sourcing the signer-identity expectation -----------------------------------------------

    /// Which authority slot a handler's signer is checked against, and where. `Administrator` /
    /// `Operator` / `Depositor` are enforced by an accounts-struct `constraint =`;
    /// `BodyAuthorized` is enforced in the handler body (`set_authorities` alone — its authority
    /// depends on the `role` argument and the rotation phase, so no static `constraint =` can
    /// express it); `AbsenceOfCheck` means neither form is present, which is correct for exactly
    /// two handlers and wrong for the other thirteen (see `declared_authority_class`). Those two
    /// absentees' further distinction — `init_protocol` is Deployer-once, `return_rejected` is
    /// Permissionless — is not itself derivable from an absence, so it lives in that function's
    /// comments rather than in a sixth variant here.
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum AuthorityClass {
        Administrator,
        Operator,
        Depositor,
        BodyAuthorized,
        /// The signer is pinned to one address by an `address =` constraint on its own
        /// attribute, naming no authority slot in `protocol_config`. Distinct from
        /// `AbsenceOfCheck`, which is the genuinely permissionless case: reading a pinned
        /// signer as an absence would record the crate's one anti-spoofing check as no check.
        AddressPinned,
        AbsenceOfCheck,
    }

    /// The `#[account(..)]` attribute attached to the sole signer, or `""` when it carries none.
    /// Scoped to that one field so an `address =` elsewhere in the struct cannot be read as a
    /// constraint on the signer.
    fn signer_attribute<'a>(body: &'a str, signer: &str) -> &'a str {
        let Some(decl) = body.find(&format!("pub {signer}: Signer<'info>")) else {
            return "";
        };
        let before = &body[..decl];
        match before.rfind("#[account(") {
            Some(open) if before[open..].trim_end().ends_with(")]") => &before[open..],
            _ => "",
        }
    }

    /// The field name of a handler's sole `Signer<'info>` account, read from its accounts struct
    /// rather than hand-listed, so a renamed signer field is picked up automatically instead of
    /// compared against a stale name.
    fn signer_identifier(body: &str) -> &str {
        let pos = body
            .find(": Signer<'info>")
            .expect("exactly_one_signer_per_handler already guarantees one is present");
        body[..pos]
            .rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .find(|segment| !segment.is_empty())
            .expect("no identifier precedes : Signer<'info>")
    }

    /// Classifies a handler's signer by which authority slot its enforcement names, over the two
    /// forms swept form-blind across every handler: an attribute `constraint =`, or a
    /// handler-body `require_keys_eq!`. Needle-based on the signer's own identifier rather than a
    /// bare `.key() ==` scan, so it is not confused by `update_config.rs:240`'s
    /// `.find(|account| account.key() == entry.position)` — a remaining-accounts lookup that
    /// names no `Signer` field (see the canary test below).
    fn derived_authority_class(source: &str, signer: &str) -> AuthorityClass {
        let prod = production(source);
        if prod.contains(&format!(
            "constraint = {signer}.key() == protocol_config.administrator"
        )) {
            AuthorityClass::Administrator
        } else if prod.contains(&format!(
            "constraint = {signer}.key() == protocol_config.operator"
        )) {
            AuthorityClass::Operator
        } else if prod.contains(&format!("position.depositor == {signer}.key()"))
            || prod.contains(&format!("depositor_token.owner == {signer}.key()"))
        {
            AuthorityClass::Depositor
        } else if prod.contains(&format!("require_keys_eq!({signer}, ")) {
            AuthorityClass::BodyAuthorized
        } else if signer_attribute(accounts_body(source), signer).contains("address = ") {
            AuthorityClass::AddressPinned
        } else {
            AuthorityClass::AbsenceOfCheck
        }
    }

    /// The hand-maintained transcription of the authority table — the
    /// only hand-written artifact in this classification, in the wildcard-free-except-`panic!`
    /// `expected_code` shape `calls_assert_weight_open` already uses. Asserted equal to
    /// `derived_authority_class`'s source-derived reading in the test below, so it cannot
    /// silently drift: if the code's enforcement moves, derived != declared and the test reds;
    /// if the table's own design changes, this row is updated and the code must follow.
    fn declared_authority_class(label: &str) -> AuthorityClass {
        use AuthorityClass::*;
        match label {
            "admin/admit_collection.rs"
            | "admin/init_pool.rs"
            | "admin/set_pause.rs"
            | "admin/update_config.rs"
            | "admin/update_vrf_config.rs"
            | "admin/withdraw_collection.rs" => Administrator,
            "tier/update_tier.rs"
            | "deposit/approve_deposit.rs"
            | "deposit/reject_deposit.rs"
            | "value/begin_sweep.rs"
            | "value/end_sweep.rs"
            | "value/record_value.rs" => Operator,
            "deposit/deposit.rs"
            | "deposit/deposit_core.rs"
            | "exit/withdraw.rs"
            | "exit/withdraw_core.rs"
            | "exit/claim_nft.rs"
            | "exit/claim_nft_core.rs" => Depositor,
            "admin/set_authorities.rs" => BodyAuthorized,
            // Deployer, once: init_protocol *sets* the authorities, so there is nothing yet to
            // check the payer against.
            "admin/init_protocol.rs" => AbsenceOfCheck,
            // Permissionless: return_rejected's position.depositor == check binds the
            // UncheckedAccount `depositor`, not the signer `payer` — and its
            // Core twin is permissionless on the same terms, with the same two-account shape.
            "deposit/return_rejected.rs" | "deposit/return_rejected_core.rs" => AbsenceOfCheck,
            // Permissionless on different authority and by a different mechanism:
            // `close_seized` is permissionless because the *asset account* authorises it — nothing
            // a caller could sign establishes that collateral is beyond reach, so the program
            // reads the card instead. Its signer `caller` binds nothing at all, where the two
            // return paths at least bind a recorded depositor they pay out to. An absence
            // cannot distinguish the two, which is why both live in this function's comments.
            "exit/close_seized.rs" => AbsenceOfCheck,
            // Permissionless, and deliberately so: a roller is any user. The signer `roller`
            // binds only the USDC account it is charged from, which is a custody check on the
            // funds and not an authority check on the caller — the pool's own guards decide
            // whether a batch may open, not who asks.
            "roll/commit_rolls.rs" => AbsenceOfCheck,
            // The one instruction no party to this system can invoke. Not an absence: the
            // signer is pinned to the VRF program's scoped identity PDA, which is what makes
            // the callback unspoofable.
            "roll/vrf_callback.rs" => AddressPinned,
            other => panic!("{other} is not classified for the authority table"),
        }
    }

    /// Field name does not determine authority class (`set_authorities`'s
    /// `signer` and `return_rejected`'s `payer` both break a name-keyed table). Cross-checks a
    /// source-derived reading against a minimal, hand-written transcription instead of
    /// trusting either alone, and separately reports the distinct-identifier partition
    /// (a different set from the 3-way site-class partition below; the two must not be tied to
    /// each other) and the 3-way site-class partition (attribute / body / absence).
    /// `AddressPinned` counts as attribute-constrained because that is where its enforcement
    /// sits, even though it names no authority slot in `protocol_config`.
    ///
    /// **The Core twins take their principals' classes and not their own.** `withdraw_core` and
    /// `claim_nft_core` are Depositor-authorised by an accounts-struct constraint;
    /// `return_rejected_core` is the second permissionless handler in the crate, where the
    /// `position.depositor ==` check binds the `UncheckedAccount` `depositor` and the signer
    /// `payer` is checked against nothing — every twin takes its principal's
    /// signer, so a twin whose class differed would be the row disagreeing with that design
    /// rather than the table drifting.
    #[test]
    fn the_signer_partition_matches_the_authority_table() {
        let mut identifier_counts: HashMap<&str, usize> = HashMap::new();
        let (mut attribute, mut body, mut absence) = (0usize, 0usize, 0usize);

        for (label, source) in ALL_INSTRUCTIONS {
            let signer = signer_identifier(accounts_body(source));
            *identifier_counts.entry(signer).or_insert(0) += 1;

            let derived = derived_authority_class(source, signer);
            let declared = declared_authority_class(label);
            assert_eq!(
                derived, declared,
                "{label}: source-derived authority class ({derived:?}) disagrees with the \
                 hand-transcribed authority table ({declared:?}) — either the enforcement moved \
                 or the table is stale"
            );

            match derived {
                AuthorityClass::Administrator
                | AuthorityClass::Operator
                | AuthorityClass::Depositor
                | AuthorityClass::AddressPinned => attribute += 1,
                AuthorityClass::BodyAuthorized => body += 1,
                AuthorityClass::AbsenceOfCheck => absence += 1,
            }
        }

        assert_eq!(attribute, 19, "attribute-constrained handlers");
        assert_eq!(body, 1, "body-authorized handlers (set_authorities)");
        assert_eq!(
            absence, 5,
            "deliberate-absence handlers (init_protocol: Deployer-once; return_rejected and \
             return_rejected_core: Permissionless, paying a recorded depositor; close_seized: \
             Permissionless, authorised by the asset account; commit_rolls: Permissionless, a \
             roller is any user)"
        );
        assert_eq!(attribute + body + absence, ALL_INSTRUCTIONS.len());

        for (identifier, expected) in [
            ("administrator", 6),
            ("operator", 6),
            ("payer", 3),
            ("depositor", 6),
            ("signer", 1),
            // `close_seized` allocates nothing, closes nothing and makes no CPI, so no
            // `payer` or `depositor` describes its signer. A sixth identifier is the honest
            // cost of naming it for what it is, and it is a fact about the crate rather than a
            // rule, so it is pinned as one.
            ("caller", 1),
            ("roller", 1),
            // The VRF program's scoped identity. Named for what it is rather than for a role in
            // this program, because no party to this system holds it.
            ("vrf_program_identity", 1),
        ] {
            assert_eq!(
                identifier_counts.get(identifier).copied().unwrap_or(0),
                expected,
                "signer identifier `{identifier}`"
            );
        }
        assert_eq!(
            identifier_counts.values().sum::<usize>(),
            ALL_INSTRUCTIONS.len()
        );
        assert_eq!(
            identifier_counts.len(),
            8,
            "exactly 8 distinct signer identifiers crate-wide"
        );
    }

    /// A parser trap, pinned directly: `update_config.rs`'s `.find(|account|
    /// account.key() == entry.position)` is a remaining-accounts lookup, not a signer binding,
    /// and a naive `.key() ==`-scanning extractor would misread it. `derived_authority_class`
    /// does not, because it searches for the signer's own identifier by name. This
    /// positive-controls that the trap text is still present before trusting the classification
    /// above.
    #[test]
    fn update_configs_remaining_accounts_lookup_does_not_confuse_the_classifier() {
        let (label, source) = ALL_INSTRUCTIONS
            .iter()
            .find(|(label, _)| *label == "admin/update_config.rs")
            .unwrap();
        assert!(
            production(source).contains(".find(|account| account.key() == entry.position)"),
            "{label}: the trap this test guards against is gone — safe to leave, but this test \
             now proves nothing"
        );
        let signer = signer_identifier(accounts_body(source));
        assert_eq!(signer, "administrator");
        assert_eq!(
            derived_authority_class(source, signer),
            AuthorityClass::Administrator
        );
    }

    // --- the seeds/bump structural identity -------------------------------------------------------

    /// `seeds = [` count, substring-based over the whole accounts-struct body — not
    /// line-anchored, so it counts `set_authorities.rs:77`'s single-line attribute the same as
    /// every multi-line one.
    fn seeds_count(body: &str) -> usize {
        body.matches("seeds = [").count()
    }

    /// `bump` counted only where it functions as the attribute keyword (`bump = <expr>` or a
    /// bare `bump`), not where it is a `.bump` field reference on an expression's right-hand
    /// side (`bump = pool.bump` has one of each). Word-bounded and substring-based, not
    /// line-anchored, so it also sees `set_authorities.rs:77`'s single-line form — a
    /// line-anchored version scores it 0, which is how every prior census in this campaign got
    /// this exact cell wrong.
    fn attribute_position_bump_count(body: &str) -> usize {
        let bytes = body.as_bytes();
        let mut count = 0;
        let mut from = 0;
        while let Some(rel) = body[from..].find("bump") {
            let start = from + rel;
            let end = start + "bump".len();
            let preceded_by_word_char = start > 0
                && matches!(bytes[start - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
            let followed_by_word_char = end < bytes.len()
                && matches!(bytes[end], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
            let is_field_reference = start > 0 && bytes[start - 1] == b'.';
            if !preceded_by_word_char && !followed_by_word_char && !is_field_reference {
                count += 1;
            }
            from = end;
        }
        count
    }

    /// A derivable structural invariant, no literal needed — every PDA account declares
    /// exactly one `bump` for every `seeds = [...]`, crate-wide, `set_authorities` included. The
    /// canary below is mandatory: a future re-anchoring of either counter to a line-based form
    /// reds it instead of silently zeroing this one handler's contribution.
    #[test]
    fn seeds_and_attribute_position_bump_are_declared_one_to_one() {
        let canary_body = accounts_body(SET_AUTHORITIES_SRC);
        assert_eq!(
            seeds_count(canary_body),
            1,
            "canary: set_authorities' single-line seeds = [...] must still be counted"
        );
        assert_eq!(
            attribute_position_bump_count(canary_body),
            1,
            "canary: set_authorities' single-line bump = ... must still be counted, and its \
             cross-referenced .bump field must not be"
        );

        let mut total_seeds = 0;
        let mut total_bump = 0;
        for (label, source) in ALL_INSTRUCTIONS {
            let body = accounts_body(source);
            let seeds = seeds_count(body);
            let bump = attribute_position_bump_count(body);
            assert_eq!(
                seeds, bump,
                "{label}: {seeds} seeds = [...] but {bump} attribute-position bump — every PDA \
                 must declare exactly one bump per seeds"
            );
            total_seeds += seeds;
            total_bump += bump;
        }
        assert_eq!(total_seeds, 80);
        assert_eq!(total_bump, 80);
    }

    /// The account-slot census, as a property of the crate rather than a hand-counted number in
    /// a document — a hand count is only as good as the last time someone re-verified it against
    /// the actual struct fields, and it drifts silently once a field is added, renamed, or moved
    /// between classes. Here the sweep's extent is derived from the crate instead, so a field
    /// change fails this test rather than quietly leaving a row nobody wrote.
    #[derive(PartialEq, Eq, Debug, Clone, Copy)]
    enum SlotClass {
        Signer,
        ProtocolConfig,
        ProgramOrSysvar,
        State,
        TokenOrMetadata,
        /// The VRF request's two non-program accounts: the provider's queue, which this program
        /// neither owns nor decodes, and our own stateless identity PDA, which exists only to
        /// sign the request CPI. Held apart from `State` deliberately — both are excluded from
        /// the substitution sweep denominator below, because neither carries a sibling in the
        /// sense the sweep means: the queue is pinned to the one `protocol_config` names, and
        /// the identity PDA is seed-derived with no discriminating component.
        VrfIntegration,
    }

    /// Field name + declared type for every `pub <name>: <type>` in an accounts struct body.
    fn account_slots(source: &str) -> Vec<(String, String)> {
        let body = accounts_body(source);
        let mut slots = Vec::new();
        for line in body.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("pub ") else {
                continue;
            };
            let Some((name, ty)) = rest.split_once(':') else {
                continue;
            };
            slots.push((
                name.trim().to_string(),
                ty.trim().trim_end_matches(',').to_string(),
            ));
        }
        assert!(
            !slots.is_empty(),
            "an accounts body yielded no `pub name: type` slots — the parser is broken, not the \
             struct empty"
        );
        slots
    }

    /// Wildcard-free except for the panic arm, the `expected_code` shape used throughout this
    /// module.
    /// A 16th account name added to any struct panics here rather than landing on whichever side
    /// happens to be convenient.
    ///
    /// The signer test is on the **declared type**, not the name, because `depositor` is a
    /// `Signer` in `deposit`/`withdraw`/`claim_nft` and a non-signing `UncheckedAccount` in
    /// `return_rejected` — the one slot whose class depends on where it appears.
    fn slot_class(name: &str, ty: &str) -> SlotClass {
        if ty.contains("Signer<") {
            return SlotClass::Signer;
        }
        match name {
            "protocol_config" => SlotClass::ProtocolConfig,
            "system_program"
            | "token_program"
            | "associated_token_program"
            | "token_metadata_program"
            | "mpl_core_program"
            | "authorization_rules_program"
            | "sysvar_instructions"
            | "authorization_rules"
            | "vrf_program"
            | "slot_hashes" => SlotClass::ProgramOrSysvar,
            "oracle_queue" | "program_identity" => SlotClass::VrfIntegration,
            "pool" | "pool_collection" | "position" | "top_tier" | "wallet_stats"
            | "weight_index" | "position_vault" | "principal_vault" | "treasury_04" | "batch" => {
                SlotClass::State
            }
            "nft_mint"
            | "asset"
            | "collection"
            | "depositor_token"
            | "metadata"
            | "master_edition"
            | "depositor_token_record"
            | "position_token_record"
            | "usdc_mint"
            | "depositor_usdc"
            | "roller_usdc"
            | "depositor" => SlotClass::TokenOrMetadata,
            other => panic!("{other} is not classified for the account-slot census"),
        }
    }

    /// **The census states what `close_seized` is, and it has moved exactly once.** Six `state`
    /// slots (`pool`, `position`, `weight_index`, `top_tier`, `wallet_stats`, `position_vault`)
    /// and two `token_or_metadata` (`asset` and its `collection`), with **no**
    /// `program_or_sysvar` — no CPI, so no program to invoke — and **no** `protocol_config` —
    /// permissionless, so no authority to check against.
    ///
    /// **The `collection` slot is load-bearing.** A `permanent_freeze_delegate` lives on an
    /// asset *or* on a collection, and this pool's live one sits on the collection — so a
    /// `close_seized` with no collection account cannot see the freeze it exists to clean up.
    ///
    /// **Attempt-and-classify — drive the exit's own release and take MPL Core's refusal as the
    /// cause — is not implementable and does not appear here.** It would need `depositor`,
    /// `mpl_core_program` and `system_program`, but on chain a refused CPI is not recoverable by
    /// its caller (`core-transfer-refused.test.ts`), so the attempt cannot be read at all. What
    /// this instruction uses instead is a plugin census inside the existing read, which adds no
    /// account.
    #[test]
    fn account_slot_census_partitions_as_25_15_45_72_41_2() {
        let mut signer = 0usize;
        let mut protocol_config = 0usize;
        let mut program_or_sysvar = 0usize;
        let mut state = 0usize;
        let mut token_or_metadata = 0usize;
        let mut vrf_integration = 0usize;

        for (label, source) in ALL_INSTRUCTIONS {
            let slots = account_slots(source);
            assert!(
                slots.iter().any(|(_, ty)| ty.contains("Signer<")),
                "{label} declares no Signer — the signer-count partition should have caught this \
                 first"
            );
            for (name, ty) in slots {
                match slot_class(&name, &ty) {
                    SlotClass::Signer => signer += 1,
                    SlotClass::ProtocolConfig => protocol_config += 1,
                    SlotClass::ProgramOrSysvar => program_or_sysvar += 1,
                    SlotClass::State => state += 1,
                    SlotClass::TokenOrMetadata => token_or_metadata += 1,
                    SlotClass::VrfIntegration => vrf_integration += 1,
                }
            }
        }

        // Exactly one signer per handler — the same fact `exactly_one_signer_per_handler` pins
        // by declaration, re-derived here from the census so the two instruments disagree
        // loudly if either drifts.
        assert_eq!(signer, ALL_INSTRUCTIONS.len());
        assert_eq!(protocol_config, 15);
        assert_eq!(program_or_sysvar, 45);
        assert_eq!(state, 72);
        assert_eq!(token_or_metadata, 41);
        assert_eq!(vrf_integration, 2);

        let total = signer
            + protocol_config
            + program_or_sysvar
            + state
            + token_or_metadata
            + vrf_integration;
        assert_eq!(total, 200);

        // The number the account-substitution sweep is sized by. Derived from the two classes
        // above, never written as a literal alongside them — a sweep row count and a census
        // that can disagree is the failure this test exists to make impossible.
        let sweep = state + token_or_metadata;
        assert_eq!(sweep, 113);
    }

    /// `protocol_config` contributes **zero** substitutable slots despite appearing in 11 of 15
    /// instructions: its seeds are `[PROTOCOL_CONFIG_SEED]` with no discriminating component, so
    /// no sibling can exist. Pinned because the exclusion must be *stated, not silent* — a naive
    /// typed-account × instruction matrix generates 11 impossible cases, and an executor then
    /// either fabricates a second `protocol_config` (which fails at the seeds constraint, the
    /// wrong reason, while `is_err()` passes) or drops them and under-reports the sweep's extent.
    /// Where a PDA slot's seeds read their discriminating components from.
    #[derive(PartialEq, Eq, Debug, Clone, Copy)]
    enum SeedAnchor {
        /// No discriminating component at all — a singleton, so no sibling can exist.
        Singleton,
        /// Every discriminating component is read off the account being validated, so the derived
        /// key moves with the substitution and **any** sibling passes the seeds check.
        FullySelf,
        /// At least one component is external and at least one is self-read. A sibling sharing the
        /// external component passes; one that differs fails. **The sibling choice decides the
        /// outcome**, which is exactly where a sweep's "the sibling dimension collapses" argument
        /// stops holding.
        Partial,
        /// Every discriminating component is supplied externally — any sibling fails at 2006.
        External,
    }

    /// Splits a seeds list on top-level commas, ignoring commas inside `()` or `[]`.
    fn seed_components(inner: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut cur = String::new();
        for ch in inner.chars() {
            match ch {
                '(' | '[' => {
                    depth += 1;
                    cur.push(ch);
                }
                ')' | ']' => {
                    depth -= 1;
                    cur.push(ch);
                }
                ',' if depth == 0 => {
                    out.push(cur.trim().to_string());
                    cur = String::new();
                }
                _ => cur.push(ch),
            }
        }
        if !cur.trim().is_empty() {
            out.push(cur.trim().to_string());
        }
        out.retain(|c| !c.is_empty());
        out
    }

    /// The leading identifier of a seed component. `POOL_SEED` → `POOL_SEED`;
    /// `position.nft_mint.as_ref()` → `position`; `pool.key().as_ref()` → `pool`.
    fn seed_root(component: &str) -> String {
        component
            .trim_start_matches('&')
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect()
    }

    /// Pairs every `seeds = [...]` with the `pub <field>:` it decorates and classifies its anchor.
    fn seed_anchors(source: &str) -> Vec<(String, SeedAnchor)> {
        let body = accounts_body(source);
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(rel) = body[from..].find("seeds = [") {
            let open = from + rel + "seeds = [".len();
            let mut depth = 1i32;
            let mut end = open;
            for (i, ch) in body[open..].char_indices() {
                match ch {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            assert!(
                end > open,
                "unterminated seeds = [ ... ] in an accounts body"
            );
            let inner = &body[open..end];

            // The field this attribute decorates is the next `pub <name>:` after it.
            let after = &body[end..];
            let field = after
                .lines()
                .find_map(|l| l.trim().strip_prefix("pub "))
                .and_then(|r| r.split_once(':'))
                .map(|(n, _)| n.trim().to_string())
                .expect("a seeds attribute with no following `pub <field>:`");

            // A discriminating component is one rooted at a lowercase identifier — an account or
            // arg. ALL-CAPS roots are the `*_SEED` consts, which discriminate nothing.
            let roots: Vec<String> = seed_components(inner)
                .iter()
                .map(|c| seed_root(c))
                .filter(|r| !r.is_empty() && r.chars().next().is_some_and(|c| c.is_lowercase()))
                .collect();

            let anchor = if roots.is_empty() {
                SeedAnchor::Singleton
            } else if roots.iter().all(|r| *r == field) {
                SeedAnchor::FullySelf
            } else if roots.contains(&field) {
                SeedAnchor::Partial
            } else {
                SeedAnchor::External
            };
            out.push((field, anchor));
            from = end;
        }
        out
    }

    /// The seed-anchor partition behind the account-substitution sweep.
    ///
    /// The sweep collapses its sibling dimension — one row per slot, not slot × sibling — on the
    /// argument that every sibling takes the same branch. **That argument holds only where the key
    /// the attribute compares against is supplied externally to the substituted account.** Where a
    /// discriminating seed component is read off the substituted account itself, the derived key
    /// moves with the substitution and a well-chosen sibling passes.
    ///
    /// **`position` slots that are partial** are anchored on
    /// the sibling `pool` but take `nft_mint` off the substituted account, so a *same-pool*
    /// sibling passes the seeds check while a cross-pool one fails. Pinned as a partition
    /// rather than a hand list, so a handler adding another moves a number here instead of
    /// quietly acquiring a sweep row nobody wrote:
    /// `withdraw_core` keys its `position` on `pool` while reading `nft_mint` off the
    /// substituted account, exactly as its principal does, and became the fifth.
    ///
    /// **`close_seized` is the sixth, and it is partial by choice rather than by
    /// inheritance.** Its sibling `claim_nft_core` anchors `position` on the *stored*
    /// `position.pool` and is fully self-referential, which is safe there because that handler
    /// carries no pool account for the stored value to disagree with. `close_seized` decrements
    /// `Pool` counters, so anchoring on the supplied `pool` is what stops one pool's position
    /// being seized against another pool's aggregates — and it accepts a sweep row to do it.
    #[test]
    fn seed_anchor_partition_is_16_singleton_22_self_6_partial_36_external() {
        let mut singleton = 0usize;
        let mut fully_self = 0usize;
        let mut partial = 0usize;
        let mut external = 0usize;
        let mut partial_slots: Vec<String> = Vec::new();

        for (label, source) in ALL_INSTRUCTIONS {
            for (field, anchor) in seed_anchors(source) {
                match anchor {
                    SeedAnchor::Singleton => singleton += 1,
                    SeedAnchor::FullySelf => fully_self += 1,
                    SeedAnchor::Partial => {
                        partial += 1;
                        partial_slots.push(format!("{label}::{field}"));
                    }
                    SeedAnchor::External => external += 1,
                }
            }
        }

        // Two const-only PDAs now: `protocol_config`, and the VRF request identity that
        // `commit_rolls` signs with — `[b"identity"]`, no discriminating component, because the
        // provider derives the same address from this program's id alone.
        assert_eq!(singleton, 16, "the const-only PDAs are protocol_config and program_identity");
        assert_eq!(fully_self, 22);
        assert_eq!(partial, 6);
        assert_eq!(external, 36);

        // Reconciles against the independently-derived total already pinned above.
        assert_eq!(singleton + fully_self + partial + external, 80);

        // The four are named, because the sweep must choose a SAME-POOL sibling on each: a
        // cross-pool sibling fails at the `pool` anchor, which passes the test for a reason that
        // has nothing to do with the position slot under test.
        partial_slots.sort();
        assert_eq!(
            partial_slots,
            vec![
                "deposit/approve_deposit.rs::position".to_string(),
                "exit/close_seized.rs::position".to_string(),
                "exit/withdraw.rs::position".to_string(),
                "exit/withdraw_core.rs::position".to_string(),
                "tier/update_tier.rs::position".to_string(),
                "value/record_value.rs::position".to_string(),
            ]
        );
    }

    #[test]
    fn protocol_config_is_a_singleton_and_so_contributes_no_substitution_slots() {
        assert_eq!(PROTOCOL_CONFIG_SEED, b"protocol");

        let mut appearances = 0usize;
        for (_, source) in ALL_INSTRUCTIONS {
            for (name, ty) in account_slots(source) {
                if slot_class(&name, &ty) == SlotClass::ProtocolConfig {
                    appearances += 1;
                }
            }
        }
        assert_eq!(appearances, 15);
    }
}
