//! Source-text audits of the instruction files: structural properties Anchor gives no runtime
//! hook for. Each test names a security invariant and walks every instruction source, so a new
//! handler is covered by being added to `ALL_INSTRUCTIONS`.

use crate::common::errors::ByeMachineError;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::common::test_support::{accounts_body, calls, code, is_assignment, writes};

const ADMIT_COLLECTION_SRC: &str = include_str!("admin/admit_collection.rs");
const CONSOLIDATE_TREASURIES_SRC: &str = include_str!("admin/consolidate_treasuries.rs");
const INIT_POOL_SRC: &str = include_str!("admin/init_pool.rs");
const INIT_PROTOCOL_SRC: &str = include_str!("admin/init_protocol.rs");
const MIGRATE_POOL_SRC: &str = include_str!("admin/migrate_pool.rs");
const SET_AUTHORITIES_SRC: &str = include_str!("admin/set_authorities.rs");
const SET_PAUSE_SRC: &str = include_str!("admin/set_pause.rs");
const SET_T04_CEILING_SRC: &str = include_str!("admin/set_t04_ceiling.rs");
const SWEEP_T04_OVERFLOW_SRC: &str = include_str!("admin/sweep_t04_overflow.rs");
const UPDATE_CONFIG_SRC: &str = include_str!("admin/update_config.rs");
const UPDATE_BYE_MINT_SRC: &str = include_str!("admin/update_bye_mint.rs");
const UPDATE_PROTOCOL_REVENUE_SRC: &str = include_str!("admin/update_protocol_revenue.rs");
const UPDATE_SWAP_POOL_SRC: &str = include_str!("admin/update_swap_pool.rs");
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
const CLAIM_FEE_SRC: &str = include_str!("exit/claim_fee.rs");
const CLAIM_NFT_SRC: &str = include_str!("exit/claim_nft.rs");
const WITHDRAW_SRC: &str = include_str!("exit/withdraw.rs");
const CLAIM_NFT_CORE_SRC: &str = include_str!("exit/claim_nft_core.rs");
const CLOSE_SEIZED_SRC: &str = include_str!("exit/close_seized.rs");
const WITHDRAW_CORE_SRC: &str = include_str!("exit/withdraw_core.rs");
const T02_CLAIM_FEE_SRC: &str = include_str!("exit/t02_claim_fee.rs");
const T02_CLAIM_NFT_SRC: &str = include_str!("exit/t02_claim_nft.rs");
const T02_CLAIM_NFT_CORE_SRC: &str = include_str!("exit/t02_claim_nft_core.rs");
const T02_WITHDRAW_SRC: &str = include_str!("exit/t02_withdraw.rs");
const T02_WITHDRAW_CORE_SRC: &str = include_str!("exit/t02_withdraw_core.rs");
const T02_DEPOSIT_SRC: &str = include_str!("deposit/t02_deposit.rs");
const T02_DEPOSIT_CORE_SRC: &str = include_str!("deposit/t02_deposit_core.rs");
const UPDATE_TIER_SRC: &str = include_str!("tier/update_tier.rs");
const COMMIT_ROLLS_SRC: &str = include_str!("roll/commit_rolls.rs");
const SETTLE_ROLL_SRC: &str = include_str!("roll/settle_roll.rs");
const SETTLE_ROLL_CORE_SRC: &str = include_str!("roll/settle_roll_core.rs");
const VRF_CALLBACK_SRC: &str = include_str!("roll/vrf_callback.rs");
const RECOVER_BATCH_SRC: &str = include_str!("roll/recover_batch.rs");
const CLOSE_BATCH_SRC: &str = include_str!("roll/close_batch.rs");
const TEST_RESOLVE_BATCH_SRC: &str = include_str!("roll/test_resolve_batch.rs");
const ACTIVATE_ACQUIRED_SRC: &str = include_str!("buyback/activate_acquired.rs");
const INSTANT_SELL_SRC: &str = include_str!("buyback/instant_sell.rs");
const INSTANT_SELL_CORE_SRC: &str = include_str!("buyback/instant_sell_core.rs");
const OPEN_ESCROW_SRC: &str = include_str!("escrow/open_escrow.rs");
const ESCROW_DEPOSIT_SRC: &str = include_str!("escrow/escrow_deposit.rs");
const ESCROW_WITHDRAW_SRC: &str = include_str!("escrow/escrow_withdraw.rs");
const SET_ESCROW_ARMED_SRC: &str = include_str!("escrow/set_escrow_armed.rs");
const CLAIM_FEE_TO_ESCROW_SRC: &str = include_str!("escrow/claim_fee_to_escrow.rs");

/// Not an instruction — the MPL Core read surface, where the collateral classification and the
/// release leg live.
const CORE_ASSET_SRC: &str = include_str!("../common/core_asset.rs");

/// Not an instruction — the activation half every path into `Active` reaches its weight,
/// tier and rebalance effects through.
const ACTIVATION_SRC: &str = include_str!("../common/activation.rs");

/// Not an instruction — the buyback's non-custody logic, shared in full by `instant_sell` and
/// `instant_sell_core` and used by `activate_acquired` for its guards and read-back.
const BUYBACK_SRC: &str = include_str!("../common/buyback.rs");

/// Not an instruction — the accounting half `withdraw` and `withdraw_core` reach the weight
/// freeze guard through.
const EXIT_ACCOUNTING_SRC: &str = include_str!("../common/exit_accounting.rs");

/// Every instruction file in the crate, paired with a label used in failure messages. A new
/// handler must be added here to be covered by the audits below.
const ALL_INSTRUCTIONS: &[(&str, &str)] = &[
    ("admin/admit_collection.rs", ADMIT_COLLECTION_SRC),
    (
        "admin/consolidate_treasuries.rs",
        CONSOLIDATE_TREASURIES_SRC,
    ),
    ("admin/init_pool.rs", INIT_POOL_SRC),
    ("admin/init_protocol.rs", INIT_PROTOCOL_SRC),
    ("admin/migrate_pool.rs", MIGRATE_POOL_SRC),
    ("admin/set_authorities.rs", SET_AUTHORITIES_SRC),
    ("admin/set_pause.rs", SET_PAUSE_SRC),
    ("admin/set_t04_ceiling.rs", SET_T04_CEILING_SRC),
    ("admin/sweep_t04_overflow.rs", SWEEP_T04_OVERFLOW_SRC),
    ("admin/update_config.rs", UPDATE_CONFIG_SRC),
    ("admin/update_bye_mint.rs", UPDATE_BYE_MINT_SRC),
    (
        "admin/update_protocol_revenue.rs",
        UPDATE_PROTOCOL_REVENUE_SRC,
    ),
    ("admin/update_swap_pool.rs", UPDATE_SWAP_POOL_SRC),
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
    ("exit/claim_fee.rs", CLAIM_FEE_SRC),
    ("exit/claim_nft.rs", CLAIM_NFT_SRC),
    ("exit/withdraw.rs", WITHDRAW_SRC),
    ("exit/claim_nft_core.rs", CLAIM_NFT_CORE_SRC),
    ("exit/close_seized.rs", CLOSE_SEIZED_SRC),
    ("exit/withdraw_core.rs", WITHDRAW_CORE_SRC),
    ("exit/t02_claim_fee.rs", T02_CLAIM_FEE_SRC),
    ("exit/t02_claim_nft.rs", T02_CLAIM_NFT_SRC),
    ("exit/t02_claim_nft_core.rs", T02_CLAIM_NFT_CORE_SRC),
    ("exit/t02_withdraw.rs", T02_WITHDRAW_SRC),
    ("exit/t02_withdraw_core.rs", T02_WITHDRAW_CORE_SRC),
    ("deposit/t02_deposit.rs", T02_DEPOSIT_SRC),
    ("deposit/t02_deposit_core.rs", T02_DEPOSIT_CORE_SRC),
    ("tier/update_tier.rs", UPDATE_TIER_SRC),
    ("roll/commit_rolls.rs", COMMIT_ROLLS_SRC),
    ("roll/settle_roll.rs", SETTLE_ROLL_SRC),
    ("roll/settle_roll_core.rs", SETTLE_ROLL_CORE_SRC),
    ("roll/vrf_callback.rs", VRF_CALLBACK_SRC),
    ("roll/recover_batch.rs", RECOVER_BATCH_SRC),
    ("roll/close_batch.rs", CLOSE_BATCH_SRC),
    ("roll/test_resolve_batch.rs", TEST_RESOLVE_BATCH_SRC),
    ("buyback/activate_acquired.rs", ACTIVATE_ACQUIRED_SRC),
    ("buyback/instant_sell.rs", INSTANT_SELL_SRC),
    ("buyback/instant_sell_core.rs", INSTANT_SELL_CORE_SRC),
    ("escrow/open_escrow.rs", OPEN_ESCROW_SRC),
    ("escrow/escrow_deposit.rs", ESCROW_DEPOSIT_SRC),
    ("escrow/escrow_withdraw.rs", ESCROW_WITHDRAW_SRC),
    ("escrow/set_escrow_armed.rs", SET_ESCROW_ARMED_SRC),
    ("escrow/claim_fee_to_escrow.rs", CLAIM_FEE_TO_ESCROW_SRC),
];

/// `ALL_INSTRUCTIONS` is the registry every audit walks, so it must list exactly the instruction
/// files the domain modules declare. The expected set is derived from the filesystem — every
/// `pub mod` of every domain directory's `mod.rs` — so a new instruction, or a new domain, that is
/// not registered fails here instead of silently escaping every audit.
#[test]
fn all_instructions_lists_exactly_the_files_the_domains_declare() {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/instructions");
    let mut declared = BTreeSet::new();
    for entry in fs::read_dir(&root).unwrap() {
        let dir = entry.unwrap().path();
        let mod_file = dir.join("mod.rs");
        if !dir.is_dir() || !mod_file.exists() {
            continue;
        }
        let domain = dir.file_name().unwrap().to_str().unwrap().to_string();
        let mod_src = fs::read_to_string(&mod_file).unwrap();
        for line in code(&mod_src).lines() {
            if let Some(module) = line.trim().strip_prefix("pub mod ") {
                declared.insert(format!("{domain}/{}.rs", module.trim_end_matches(';')));
            }
        }
    }
    assert!(
        declared.len() > 30,
        "found only {} declared instruction modules — the discovery is broken",
        declared.len()
    );

    let listed: Vec<&str> = ALL_INSTRUCTIONS.iter().map(|(label, _)| *label).collect();
    let listed_set: BTreeSet<String> = listed.iter().map(|l| l.to_string()).collect();
    assert_eq!(
        listed.len(),
        listed_set.len(),
        "ALL_INSTRUCTIONS lists a file twice"
    );
    let unregistered: Vec<_> = declared.difference(&listed_set).collect();
    let stale: Vec<_> = listed_set.difference(&declared).collect();
    assert!(
        unregistered.is_empty() && stale.is_empty(),
        "ALL_INSTRUCTIONS is out of step with the domain modules — declared but unregistered: \
         {unregistered:?}; registered but not declared: {stale:?}"
    );

    for (label, source) in ALL_INSTRUCTIONS {
        assert_eq!(
            fs::read_to_string(root.join(label)).unwrap(),
            *source,
            "ALL_INSTRUCTIONS pairs the label {label} with another file's source"
        );
    }
}

/// Every production file that releases a Core asset from the vault (derived, so a new Core
/// exit joins the set automatically) takes exactly one collateral gate, and that gate precedes
/// every CPI in the file. That ordering matters for `withdraw_core`, whose fee payout is a CPI
/// that runs before the card is touched. The gate's error codes are fixed by value:
/// `CollateralAbsent` (6307) and `CollateralFrozen` (6309). On `Undecidable` the forward gate
/// accepts, because nothing the program can decide blocks the move and the `TransferV1` is
/// what finds out.
#[test]
fn every_core_exit_gates_its_collateral_before_any_cpi() {
    let core_exits: Vec<_> = ALL_INSTRUCTIONS
        .iter()
        .filter(|(_, source)| calls(source, "release_core_from_vault") > 0)
        .collect();

    assert_eq!(
        core_exits.len(),
        6,
        "the releasing set is return_rejected_core, withdraw_core, t02_withdraw_core, \
         claim_nft_core, t02_claim_nft_core and settle_roll_core; a seventh releasing handler owes this instrument nothing but must \
         appear in it"
    );
    let mut labels: Vec<&str> = core_exits.iter().map(|(label, _)| *label).collect();
    labels.sort();
    assert_eq!(
        labels,
        [
            "deposit/return_rejected_core.rs",
            "exit/claim_nft_core.rs",
            "exit/t02_claim_nft_core.rs",
            "exit/t02_withdraw_core.rs",
            "exit/withdraw_core.rs",
            "roll/settle_roll_core.rs",
        ]
    );

    for (label, source) in &core_exits {
        let prod = code(source);
        // The depositor-facing exits proceed on an undecidable classification and let MPL Core
        // answer, since a refused transfer costs the caller alone. A settlement cannot: a
        // refused transfer reverts the roll, which stays `Pending` and freezes every action
        // gated on the open batch. So settlement takes a gate that closes on that
        // classification. Each handler takes exactly one of the two gates.
        let gate_name = if calls(source, "classify_for_settlement") > 0 {
            "classify_for_settlement"
        } else {
            "require_collateral_settleable"
        };
        assert_eq!(
            calls(source, gate_name),
            1,
            "{label} must gate its collateral exactly once"
        );
        assert_eq!(
            calls(source, "require_collateral_settleable")
                + calls(source, "classify_for_settlement"),
            1,
            "{label} takes more than one forward gate — two readings of one classification \
             is two answers to whether the card can move"
        );
        let gate = prod.find(&format!("{gate_name}(")).unwrap();

        // Every CPI-forming call, not only the release: a gate after any of them would return
        // its named code after a transfer has already been built.
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

        // The inverse gate belongs to `close_seized`; an exit that took it would refuse
        // exactly the positions it exists to serve.
        assert_eq!(
            calls(source, "require_collateral_unreachable"),
            0,
            "{label} takes the inverse gate — that is close_seized's, not an exit's"
        );
    }

    assert_eq!(u32::from(ByeMachineError::CollateralAbsent), 6307);
    assert_eq!(u32::from(ByeMachineError::CollateralFrozen), 6309);
}

/// The forward gates and the inverse gate must leave no gap: a position that can neither be
/// withdrawn nor closed would strand the depositor and leave a permanent phantom leaf.
/// Membership on both sides is derived from the calls; the sides are disjoint and their union
/// is every handler that reads the classification.
///
/// The gates are complements only in the weak sense that no classification is refused by both:
/// the plugin census answers `Undecidable` for anything it cannot decide (including a
/// `plugin_type` a later `mpl-core` ships), and both gates admit that.
///
/// `close_seized` is the sole caller of the inverse and forms no CPI of any kind.
/// `CollateralPresent` (6308) sits between 6307 and 6309 so a client keys the causes on
/// adjacent codes.
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
    let settlement: Vec<_> = ALL_INSTRUCTIONS
        .iter()
        .filter(|(_, source)| calls(source, "classify_for_settlement") > 0)
        .collect();

    assert_eq!(
        inverse.len(),
        1,
        "close_seized is the only instruction that acts on collateral being beyond reach"
    );
    let (label, source) = inverse[0];
    assert_eq!(*label, "exit/close_seized.rs");
    assert_eq!(calls(source, "require_collateral_unreachable"), 1);

    // Disjoint: a handler holding both would refuse the positions it serves.
    for (forward_label, _) in forward.iter().chain(settlement.iter()) {
        assert_ne!(
            forward_label, label,
            "{forward_label} holds both a forward gate and the inverse"
        );
    }
    // Exhaustive: 5 forward + 1 inverse + 1 settlement. The settlement gate is on neither
    // other side: it closes on the undecided classification, because a settlement that cannot
    // move the card cannot leave the roll for a retry that will fail identically.
    assert_eq!(settlement.len(), 1);
    assert_eq!(settlement[0].0, "roll/settle_roll_core.rs");
    assert_eq!(forward.len() + inverse.len() + settlement.len(), 7);
    assert_eq!(u32::from(ByeMachineError::CollateralUndecidable), 6310);

    let close_seized_code = code(source);
    for cpi in [
        "release_core_from_vault(",
        "transfer(",
        "close_account(",
        "invoke(",
    ] {
        assert!(
            !close_seized_code.contains(cpi),
            "close_seized forms {cpi} — it moves no custody on any source state"
        );
    }
    assert_eq!(
        code(CORE_ASSET_SRC)
            .matches("release_core_from_vault(")
            .count(),
        0,
        "and neither does the classification it calls — the release leg is declared in \
         core_asset.rs and called only by the three exits, which is what a CPI measured \
         unrecoverable leaves this module able to claim"
    );
    // Positive control: the leg is still declared (as `fn release_core_from_vault<'info>(`,
    // which does not match the call form counted above).
    assert_eq!(
        code(CORE_ASSET_SRC)
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
struct DeclaredAccount {
    name: String,
    writable: bool,
}

fn declared_accounts(source: &str) -> Vec<DeclaredAccount> {
    let prod = code(source);
    let body = prod
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
            let writable = attr.contains("mut") || attr.contains("init") || attr.contains("zero");
            accounts.push(DeclaredAccount {
                name: name.to_string(),
                writable,
            });
            attr.clear();
        }
    }
    accounts
}

/// Whether `needle` occurs in `haystack` **not** followed by another identifier character.
///
/// A bare `contains` is a prefix match: with accounts `pool` and `pool_collection`,
/// `&mut ctx.accounts.pool_collection` would report `pool` as mutated.
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

/// Whether the handler mutates `ctx.accounts.<name>`: a direct field write (see
/// [`test_support::is_assignment`]), a `&mut ctx.accounts.<name>` borrow or move, or
/// `load_mut()`/`load_init()`.
///
/// **Scope limit — an account written only by a callee is invisible here.** An account handed
/// to a CPI as `.to_account_info()` is never written Rust-side, so none of these forms match
/// it. That is deliberate: the class this test targets is a Rust-side write that Anchor
/// silently discards, whereas a CPI-passed account missing `mut` either works unvalidated or
/// fails the CPI loudly, and deciding it from source would mean modelling each callee's
/// account semantics.
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

#[test]
fn every_mutated_account_is_declared_writable_crate_wide() {
    for (label, source) in ALL_INSTRUCTIONS {
        let prod_flat = code(source).replace('\n', " ");
        for account in declared_accounts(source) {
            if is_mutated(&prod_flat, &account.name) {
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

/// How an instruction relates to the weight freeze (`assert_weight_open`). The classification
/// is a match on the label that panics on an unlisted one, so a new entry in
/// `ALL_INSTRUCTIONS` cannot land on a convenient side silently.
///
/// `Reaches` is the same guard behind one more call: `withdraw`'s accounting half, and the
/// guard with it, lives in `common/exit_accounting.rs` so `withdraw_core` can share it.
///
/// `Serializes` is a separate ground, not a weaker `Direct`: `Direct` and `Reaches` name
/// handlers that would otherwise move `W` inside an open batch's window, while
/// `sweep_t04_overflow` and `consolidate_treasuries` move none and call the guard for their own
/// reason: each moves the Treasury 04 balance an open batch's blanks settle against.
///
/// `commit_rolls` is `None`: commits queue behind any open batch rather than being refused, so
/// it moves no weight and enforces no serialization. Queue ordering is
/// `Pool::head_batch_id()`'s job, checked by `settle_roll`, `settle_roll_core` and
/// `recover_batch`.
#[derive(PartialEq, Eq)]
enum WeightGuard {
    Direct,
    /// Through `apply_withdraw`, whose own call to the guard is asserted below.
    Reaches,
    /// Through `activate_acquired_position`, whose own call to the guard is asserted below.
    /// Kept apart from `Reaches` because the two go through different shared halves.
    ReachesThroughAcquisition,
    /// Calls the guard for a reason other than moving weight (`sweep_t04_overflow`,
    /// `consolidate_treasuries`).
    Serializes,
    /// **Moves weight and must not carry the guard.** The guard refuses while a batch is open
    /// and this runs only while one is: it is the pipeline the freeze protects. Kept apart from
    /// `None`, whose claim is "touches no weight at all".
    Exempt,
    None,
}

/// The audits' liveness signal: a handler that emits an event is doing something, so a "does not
/// call X" pin is not resting on a gutted body. `migrate_pool` emits nothing by design, since it
/// changes only a pool's length, so its signal is the resize itself.
fn is_live(label: &str, source: &str) -> bool {
    match label {
        "admin/migrate_pool.rs" => calls(source, "resize") == 1,
        _ => code(source).contains("emit!("),
    }
}

fn weight_guard(label: &str) -> WeightGuard {
    match label {
        "admin/update_config.rs"
        | "deposit/approve_deposit.rs"
        | "value/record_value.rs"
        | "exit/close_seized.rs" => WeightGuard::Direct,
        "exit/withdraw.rs"
        | "exit/withdraw_core.rs"
        | "exit/t02_withdraw.rs"
        | "exit/t02_withdraw_core.rs" => WeightGuard::Reaches,
        // The buyback's weight-moving half. The sell itself is unguarded (paying the seller
        // and moving the card change no position's weight); this is the step that enters the
        // draw domain, and it reaches the guard through the shared half it delegates to.
        "buyback/activate_acquired.rs" => WeightGuard::ReachesThroughAcquisition,
        "admin/sweep_t04_overflow.rs" | "admin/consolidate_treasuries.rs" => {
            WeightGuard::Serializes
        }
        "roll/settle_roll.rs" | "roll/settle_roll_core.rs" => WeightGuard::Exempt,
        "admin/admit_collection.rs"
        | "admin/init_pool.rs"
        | "admin/init_protocol.rs"
        | "admin/migrate_pool.rs"
        | "admin/set_authorities.rs"
        | "admin/set_pause.rs"
        | "admin/set_t04_ceiling.rs"
        | "admin/update_bye_mint.rs"
        | "admin/update_protocol_revenue.rs"
        | "admin/update_swap_pool.rs"
        | "admin/update_vrf_config.rs"
        | "admin/withdraw_collection.rs"
        | "buyback/instant_sell.rs"
        | "buyback/instant_sell_core.rs"
        | "deposit/deposit.rs"
        | "deposit/deposit_core.rs"
        | "deposit/reject_deposit.rs"
        | "deposit/return_rejected.rs"
        | "value/begin_sweep.rs"
        | "value/end_sweep.rs"
        | "deposit/return_rejected_core.rs"
        | "exit/claim_fee.rs"
        | "exit/t02_claim_fee.rs"
        | "exit/t02_claim_nft.rs"
        | "exit/t02_claim_nft_core.rs"
        | "deposit/t02_deposit.rs"
        | "deposit/t02_deposit_core.rs"
        | "exit/claim_nft.rs"
        | "exit/claim_nft_core.rs"
        | "roll/commit_rolls.rs"
        | "roll/vrf_callback.rs"
        | "roll/test_resolve_batch.rs"
        | "roll/recover_batch.rs"
        | "roll/close_batch.rs"
        | "tier/update_tier.rs"
        | "escrow/open_escrow.rs"
        | "escrow/escrow_deposit.rs"
        | "escrow/escrow_withdraw.rs"
        | "escrow/set_escrow_armed.rs"
        | "escrow/claim_fee_to_escrow.rs" => WeightGuard::None,
        other => panic!("{other} is not classified for the assert_weight_open partition"),
    }
}

/// Eleven handlers carry the freeze guard. Four call it in their own body (`update_config`,
/// `approve_deposit`, `record_value`, and `close_seized`'s `Active` row, the only row that
/// departs weight) and `sweep_t04_overflow` and `consolidate_treasuries` call it to serialize;
/// `withdraw`, `withdraw_core` and the two T02 withdrawals reach it through the `apply_withdraw`
/// they share, and `activate_acquired` through `activate_acquired_position`.
///
/// The buyback splits across both sides by design. `instant_sell` and its Core twin are
/// non-callers: payment and custody move no weight, so an open batch must not refuse a
/// purchase the pool can safely make. `activate_acquired` is guarded because entering the draw
/// domain does move weight; unguarded, an acquisition could join a batch's draw domain after
/// it opened.
///
/// `settle_roll` (and its Core twin) moves weight and must not carry the guard: it runs only
/// while a batch is open, so the guard would make the pipeline refuse itself and freeze the
/// pool. The two claim twins (`return_rejected_core`, `claim_nft_core`) stay unguarded.
///
/// Every other non-caller touches no weight at all, e.g. `vrf_callback` (writes randomness),
/// `commit_rolls` (queues) and the five escrow instructions (an escrow belongs to no pool).
#[test]
fn assert_weight_open_partitions_as_eleven_guarded_two_exempt_and_forty_unguarded() {
    let guarded: Vec<_> = ALL_INSTRUCTIONS
        .iter()
        .filter(|(label, _)| {
            !matches!(weight_guard(label), WeightGuard::None | WeightGuard::Exempt)
        })
        .collect();
    let exempt: Vec<_> = ALL_INSTRUCTIONS
        .iter()
        .filter(|(label, _)| weight_guard(label) == WeightGuard::Exempt)
        .collect();
    let non_callers: Vec<_> = ALL_INSTRUCTIONS
        .iter()
        .filter(|(label, _)| weight_guard(label) == WeightGuard::None)
        .collect();

    assert_eq!(guarded.len(), 11);
    assert_eq!(exempt.len(), 2);
    assert_eq!(non_callers.len(), 40);
    assert_eq!(
        guarded.len() + exempt.len() + non_callers.len(),
        ALL_INSTRUCTIONS.len()
    );

    for (label, source) in &exempt {
        assert_eq!(
            calls(source, "assert_weight_open"),
            0,
            "{label} is classified Exempt but carries the guard — it runs only while a \
             batch is open, so the guard would refuse every one of its rolls and leave the \
             pool frozen behind the batch it cannot finish"
        );
        // The Core twin reaches the evaluation through the step it shares with its principal.
        assert!(
            code(source).contains("rebalance::evaluate") || calls(source, "apply_rebalance") == 1,
            "{label} is classified Exempt on the grounds that it moves weight, and it does \
             not — an exemption granted to a handler that moves none is an unguarded \
             handler with a reason attached"
        );
    }

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
            WeightGuard::ReachesThroughAcquisition => {
                assert_eq!(
                    calls(source, "activate_acquired_position"),
                    1,
                    "{label} is classified as reaching the guard through the acquisition \
                     half but does not call activate_acquired_position exactly once"
                );
                assert_eq!(
                    calls(source, "assert_weight_open"),
                    0,
                    "{label} reaches the guard through the shared half and also calls it \
                     directly — one of the two is a second guard nobody re-derived"
                );
                assert_eq!(
                    calls(BUYBACK_SRC, "assert_weight_open"),
                    1,
                    "activate_acquired_position is the reach path and does not carry the \
                     guard"
                );
            }
            WeightGuard::Exempt | WeightGuard::None => {
                unreachable!("filtered out above")
            }
        }
    }
    for (label, source) in &non_callers {
        // A gutted handler satisfies "does not call assert_weight_open" too; emitting at least
        // one event is the crate-wide liveness signal.
        assert!(
            is_live(label, source),
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

/// A handler emits `RebalanceEvaluated` if and only if it reaches `rebalance::evaluate`, in its
/// own body or through a shared helper. An emission without an evaluation publishes a
/// recomputation that did not happen; an evaluation without an emission silently moves the
/// draw domain, which breaks a third party's reconstruction of a batch.
///
/// Conditional emitters are counted by site, not by execution: `close_seized` re-evaluates on
/// its `Active` source state only, and its emit site is paired with that evaluation.
/// The escrow instructions move only an owner's own funds, so none of them may wait on a pool:
/// the owner can always withdraw, deposit, arm or claim into their escrow whatever the pause
/// flags, an open roll batch or a sweep say. The set is every file the escrow domain declares,
/// with a floor so a registry that lost them cannot pass vacuously.
#[test]
fn no_escrow_instruction_reads_a_pause_flag_a_roll_batch_or_the_weight_guard() {
    let escrow: Vec<_> = ALL_INSTRUCTIONS
        .iter()
        .filter(|(label, _)| label.starts_with("escrow/"))
        .collect();
    assert_eq!(escrow.len(), 5, "the escrow domain's files went missing");

    for (label, source) in escrow {
        let prod = code(source);
        for forbidden in [
            "deposits_paused",
            "rolls_paused",
            "open_batches",
            "lock_until",
            "sweep_pending",
        ] {
            assert!(
                !prod.contains(forbidden),
                "{label} reads {forbidden} — an escrow instruction must never wait on a pool"
            );
        }
        assert_eq!(calls(source, "assert_weight_open"), 0, "{label}");
    }

    // Positive control: the scan sees these names where they are read.
    assert!(code(SET_PAUSE_SRC).contains("deposits_paused"));
    assert!(code(COMMIT_ROLLS_SRC).contains("open_batches"));
}

#[test]
fn rebalance_evaluated_is_emitted_exactly_where_the_blank_set_is_re_evaluated() {
    // The two withdrawal paths reach the evaluation through their shared accounting half; that
    // reach is asserted here.
    assert_eq!(
        calls(EXIT_ACCOUNTING_SRC, "rebalance::evaluate"),
        1,
        "apply_withdraw is the reach path for both withdrawal handlers and no longer \
         re-evaluates the blank set"
    );
    // The settlement twin reaches it through `apply_rebalance`, shared with its principal.
    assert_eq!(
        calls(SETTLE_ROLL_SRC, "rebalance::evaluate"),
        1,
        "apply_rebalance is the reach path for settle_roll_core and no longer re-evaluates \
         the blank set"
    );
    // Every activation path reaches the evaluation through `activate_position`. The buyback
    // half has two call sites: `record_acquisition` (a sell landing with no batch open) and
    // `activate_acquired_position` (one that did not). Counted, so a deleted call in either
    // is not masked by the other.
    assert_eq!(
        calls(BUYBACK_SRC, "activate_position"),
        2,
        "the buyback half is the reach path for every buyback handler and one of its two \
         activation sites no longer activates"
    );
    assert_eq!(
        calls(ACTIVATION_SRC, "rebalance::evaluate"),
        1,
        "activate_position is the reach path for every activating handler and no longer \
         re-evaluates the blank set"
    );
    let reaches_through_a_shared_helper = [
        "exit/withdraw.rs",
        "exit/withdraw_core.rs",
        "exit/t02_withdraw.rs",
        "exit/t02_withdraw_core.rs",
        "roll/settle_roll_core.rs",
        "deposit/approve_deposit.rs",
        // Reaches the evaluation through `activate_position` and emits unconditionally: it has
        // no non-activating branch.
        "buyback/activate_acquired.rs",
        // The sell reaches the evaluation through `record_acquisition`, which activates only
        // on the `open_batches == 0` branch; the emission is conditional on the same branch.
        "buyback/instant_sell.rs",
        "buyback/instant_sell_core.rs",
    ];

    let mut emitters = Vec::new();
    for (label, source) in ALL_INSTRUCTIONS {
        let prod = code(source);
        let emits = prod.matches("emit!(RebalanceEvaluated").count();
        let evaluates = calls(source, "rebalance::evaluate")
            + usize::from(reaches_through_a_shared_helper.contains(label));

        assert!(
            emits <= 1,
            "{label} emits RebalanceEvaluated {emits} times — one evaluation publishes one \
             event, so a second site is either a duplicate or an unpaired emission"
        );
        assert_eq!(
            emits, evaluates,
            "{label} re-evaluates the blank set {evaluates} time(s) but emits \
             RebalanceEvaluated {emits} time(s) — an emission without an evaluation \
             announces a recomputation that did not happen, and an evaluation without an \
             emission moves the draw domain silently"
        );

        if emits == 1 {
            emitters.push(*label);
        } else {
            // A gutted handler also emits no RebalanceEvaluated; require the generic liveness
            // signal rather than resting on an absence.
            assert!(
                is_live(label, source),
                "{label} is classified as a non-emitter but emits nothing at all"
            );
        }
    }

    // Exact membership, so a handler joining or leaving the set shows as a named change.
    emitters.sort_unstable();
    assert_eq!(
        emitters,
        vec![
            "admin/update_config.rs",
            "buyback/activate_acquired.rs",
            "buyback/instant_sell.rs",
            "buyback/instant_sell_core.rs",
            "deposit/approve_deposit.rs",
            "exit/close_seized.rs",
            "exit/t02_withdraw.rs",
            "exit/t02_withdraw_core.rs",
            "exit/withdraw.rs",
            "exit/withdraw_core.rs",
            "roll/settle_roll.rs",
            "roll/settle_roll_core.rs",
            "value/record_value.rs",
        ]
    );
}

/// **The `ProtocolConfig` PDA signs exactly three debits**, the ones out of the shared Treasury 03
/// and Treasury 04 it is the authority of: the buyback's Treasury 03 payout (`pay_for_sale`, which
/// `instant_sell` and `instant_sell_core` call), the blank payout's swap out of Treasury 04 in
/// `settle_roll`, and `sweep_t04_overflow`'s transfer. A fourth signer site would be a fourth way
/// to move those balances, and one of these three going missing is a debit that can no longer sign.
#[test]
fn the_protocol_config_pda_signs_exactly_the_shared_treasury_debits() {
    use crate::common::test_support::{crate_sources, seed_uses_outside_constraints};

    // Test-only modules, gated `#[cfg(test)]` where they are declared, so nothing in them signs.
    const TEST_ONLY: [&str; 2] = ["common/test_support.rs", "instructions/source_audit.rs"];
    assert!(include_str!("../common/mod.rs").contains("#[cfg(test)]\npub mod test_support;"));
    assert!(include_str!("mod.rs").contains("#[cfg(test)]\nmod source_audit;"));

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut signers: Vec<String> = Vec::new();
    for path in crate_sources() {
        let label = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if TEST_ONLY.contains(&label.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let sites = seed_uses_outside_constraints(&source, "PROTOCOL_CONFIG_SEED");
        if sites > 0 {
            assert_eq!(
                sites,
                1,
                "{} builds the config signer twice",
                path.display()
            );
            signers.push(label);
        }
    }
    signers.sort();
    assert_eq!(
        signers,
        [
            "common/buyback.rs",
            "instructions/admin/sweep_t04_overflow.rs",
            "instructions/roll/settle_roll.rs",
        ]
    );

    // Each site hands the config account to the CPI as its signing authority.
    for (label, source, authority) in [
        (
            "common/buyback.rs",
            BUYBACK_SRC,
            "authority: protocol_config.to_account_info(),",
        ),
        (
            "admin/sweep_t04_overflow.rs",
            SWEEP_T04_OVERFLOW_SRC,
            "authority: ctx.accounts.protocol_config.to_account_info(),",
        ),
        (
            "roll/settle_roll.rs",
            SETTLE_ROLL_SRC,
            "payer: accounts.protocol_config.to_account_info(),",
        ),
    ] {
        assert_eq!(code(source).matches(authority).count(), 1, "{label}");
    }

    // The buyback's signer is reached through `pay_for_sale`, from the two sales and nowhere else.
    let callers: Vec<&str> = ALL_INSTRUCTIONS
        .iter()
        .filter(|(_, source)| calls(source, "pay_for_sale") > 0)
        .map(|(label, _)| *label)
        .collect();
    assert_eq!(
        callers,
        ["buyback/instant_sell.rs", "buyback/instant_sell_core.rs"]
    );
}

/// `init_pool`'s two custody vaults have attribute blocks that differ only in one seed
/// constant, so swapping them compiles, passes every per-account `contains` check and silently
/// exchanges principal custody with the Treasury 04 float. Each vault must be seeded by its own
/// constant and by no other.
#[test]
fn init_pool_custody_vaults_are_seeded_by_their_own_constants() {
    let body = accounts_body(INIT_POOL_SRC);
    let mut block = String::new();
    let mut blocks: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("///") {
            continue;
        }
        if let Some(field) = trimmed.strip_prefix("pub ") {
            let name = field.split(':').next().unwrap().trim().to_string();
            blocks.push((name, std::mem::take(&mut block)));
        } else {
            block.push_str(trimmed);
            block.push('\n');
        }
    }
    let attrs = |name: &str| {
        &blocks
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("init_pool declares no `{name}` account"))
            .1
    };
    let principal = attrs("principal_vault");
    let treasury = attrs("treasury_04");
    assert!(principal.contains("PRINCIPAL_VAULT_SEED") && !principal.contains("TREASURY_04_SEED"));
    assert!(treasury.contains("TREASURY_04_SEED") && !treasury.contains("PRINCIPAL_VAULT_SEED"));
}

/// The test-only resolution must delegate to `apply_resolve` and not write the resolved fields
/// itself, so the state it produces stays the state a real delivery produces. It lives here
/// because the handler's module is gated on `testing`, so a test module inside it would not
/// compile under the featureless `cargo test` CI runs.
#[test]
fn the_test_only_resolution_delegates_to_the_callbacks_transition() {
    let prod = code(TEST_RESOLVE_BATCH_SRC);

    assert_eq!(
        calls(&prod, "apply_resolve"),
        1,
        "the handler must reach the callback's own transition exactly once"
    );
    for field in ["batch.state", "batch.randomness", "batch.expected_w"] {
        assert_eq!(
            writes(&prod, field),
            0,
            "{field} is written here rather than by apply_resolve — the resolved shape now \
             has two authors and they can disagree"
        );
    }
    assert!(
        prod.contains("operator.key() == protocol_config.operator"),
        "the test-only resolution must stay Operator-gated even in its own build"
    );
}

// --- the account-slot census ------------------------------------------------------------

/// The account-slot census, derived from the crate so a field added, renamed or moved between
/// classes fails a test instead of silently leaving a row unwritten.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
enum SlotClass {
    Signer,
    ProtocolConfig,
    ProgramOrSysvar,
    State,
    TokenOrMetadata,
    /// Accounts an external integration names; none has a sibling to substitute, so all are
    /// excluded from the substitution sweep.
    ///
    /// The VRF provider's queue and the swap venue are each fixed to the single address
    /// `protocol_config` records; our own VRF identity PDA has no discriminating seed
    /// component. The venue is the one of the three this program decodes, and it is guarded by
    /// a content check (owner, discriminator, mint pair, both vaults) where it is set and
    /// where it is read.
    PinnedExternal,
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

/// Wildcard-free except for the panic arm: an unclassified account name panics here instead of
/// landing on a convenient side.
///
/// The signer test is on the declared type, not the name, because `depositor` is a `Signer` in
/// `deposit`/`withdraw`/`claim_nft` and a non-signing `UncheckedAccount` in `return_rejected`.
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
        | "cp_amm_program"
        | "slot_hashes" => SlotClass::ProgramOrSysvar,
        // The venue's own accounts: its two vaults, which the handler pins to the addresses
        // the pool account itself records, its single global authority, and its event PDA.
        // None has a sibling to substitute.
        "oracle_queue"
        | "program_identity"
        | "swap_pool"
        | "swap_token_a_vault"
        | "swap_token_b_vault"
        | "cp_amm_pool_authority"
        | "cp_amm_event_authority" => SlotClass::PinnedExternal,
        // `t02_identity` is an address rather than an account: it holds no balance and is only
        // written into `Position.depositor`. `treasury_03`, `treasury_04` and `principal_vault`
        // are the pool's own value accounts, each pinned to the single address `Pool` records.
        "pool" | "pool_collection" | "position" | "top_tier" | "wallet_stats"
        | "weight_index" | "position_vault" | "principal_vault" | "treasury_03"
        | "treasury_04" | "t02_identity" | "batch"
        // A pool's own Treasury 03 and Treasury 04 from before the treasuries were shared.
        | "legacy_treasury_03" | "legacy_treasury_04"
        // A wallet's escrow and its vault, both derived from the owner's key.
        | "escrow" | "escrow_vault"
        // The loader's escrow, vault and rollup a sale may pay from, bound in the handler to one
        // owner rather than by seeds.
        | "loader_escrow" | "loader_escrow_vault" | "loader_wallet_stats" => SlotClass::State,
        "nft_mint"
        | "asset"
        | "collection"
        | "depositor_token"
        // The seller's card, token record and payout account: owned by the counterparty.
        | "seller_token"
        | "seller_token_record"
        | "seller_usdc"
        // The T02 authority's card and token record: owned by the signing authority.
        | "authority_token"
        | "authority_token_record"
        | "metadata"
        | "master_edition"
        | "depositor_token_record"
        | "position_token_record"
        | "roller_token"
        | "roller_token_record"
        | "usdc_mint"
        | "bye_mint"
        // The roller's $BYE account, created idempotently by the settlement.
        | "roller_bye"
        | "depositor_usdc"
        // The escrow owner's own USDC account, the source of a deposit and the destination of
        // a withdrawal.
        | "owner_usdc"
        | "roller_usdc"
        // A settled position's payout destination: the address `Pool` names for a Treasury-02
        // position, otherwise an account the depositor owns.
        | "settlement_usdc"
        | "roller"
        // An external USDC account pinned to the address `protocol_config` names, not a PDA
        // this program derives.
        | "protocol_revenue"
        // Derived by the Associated Token program from a key `protocol_config` names, not from
        // seeds this program owns (unlike `treasury_03`, which is this program's own PDA).
        | "treasury_05"
        | "depositor" => SlotClass::TokenOrMetadata,
        other => panic!("{other} is not classified for the account-slot census"),
    }
}

/// Where a PDA slot's seeds read their discriminating components from.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
enum SeedAnchor {
    /// No discriminating component at all — a singleton, so no sibling can exist.
    Singleton,
    /// Every discriminating component is read off the account being validated, so the derived
    /// key moves with the substitution and **any** sibling passes the seeds check.
    FullySelf,
    /// At least one component is external and at least one is self-read. A sibling sharing the
    /// external component passes; one that differs fails, so the sibling
    /// choice decides the outcome.
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

/// `protocol_config` contributes no substitutable slots: its seeds are `[PROTOCOL_CONFIG_SEED]`
/// with no discriminating component, so no sibling can exist. The exclusion is stated here
/// rather than left implicit, since a naive typed-account × instruction matrix would generate
/// impossible cases.
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
    assert_eq!(appearances, 41);
}

/// The account-substitution sweep's per-slot inventory, checked against the committed
/// `tests/fixtures/sweep-census.json` (an aggregate count cannot see two slots trading
/// classifications). This test checks the crate's census against the fixture;
/// `tests/unit/substitution-sweep-census.test.ts` checks the TS `SWEEP_SLOTS` against the same
/// entries. Regenerate the fixture from this test's computation; never hand-edit it or
/// `SWEEP_SLOTS` to match a stale one.
#[test]
fn sweep_census_matches_the_committed_fixture() {
    let mut entries: Vec<serde_json::Value> = Vec::new();
    for (label, source) in ALL_INSTRUCTIONS {
        let instruction = label.rsplit('/').next().unwrap().trim_end_matches(".rs");
        let anchors: std::collections::HashMap<String, SeedAnchor> =
            seed_anchors(source).into_iter().collect();
        for (name, ty) in account_slots(source) {
            let class = slot_class(&name, &ty);
            let class_str = match class {
                SlotClass::State => "State",
                SlotClass::TokenOrMetadata => "TokenOrMetadata",
                _ => continue,
            };
            let anchor_str = match anchors.get(&name) {
                Some(SeedAnchor::Singleton) => "Singleton",
                Some(SeedAnchor::FullySelf) => "FullySelf",
                Some(SeedAnchor::Partial) => "Partial",
                Some(SeedAnchor::External) => "External",
                None => "NotSeeded",
            };
            entries.push(serde_json::json!({
                "instruction": instruction,
                "field": name,
                "class": class_str,
                "anchor": anchor_str,
            }));
        }
    }
    entries.sort_by(|a, b| {
        let key = |v: &serde_json::Value| {
            (
                v["instruction"].as_str().unwrap().to_string(),
                v["field"].as_str().unwrap().to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    assert_eq!(
        entries.len(),
        314,
        "the sweep's own extent moved — regenerate tests/fixtures/sweep-census.json"
    );

    const COMMITTED: &str = include_str!("../../../../tests/fixtures/sweep-census.json");
    let committed: Vec<serde_json::Value> = serde_json::from_str(COMMITTED)
        .expect("tests/fixtures/sweep-census.json is not valid JSON");

    assert_eq!(
        entries, committed,
        "the crate's account-substitution sweep census no longer matches \
         tests/fixtures/sweep-census.json — regenerate it from this test's own computation \
         and update tests/helpers/substitution-sweep.ts's SWEEP_SLOTS to match"
    );
}
