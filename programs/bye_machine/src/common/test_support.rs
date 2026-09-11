//! Shared predicates for the crate's source-introspection tests.
//!
//! These pins are the only instrument available where behaviour is not — `#[derive(Accounts)]`
//! attributes, CPI argument ordering, and the set of fields a handler writes have no runtime
//! expression without a validator. When the only instrument is a source predicate, the predicate
//! has to be right, and it has to be right in exactly one place: a per-file copy of
//! [`is_assignment`] can be corrected in one file while others keep the broken version, which is
//! the drift this module exists to make impossible.

/// Whether the text immediately following a field reference begins an **assignment** to it.
///
/// Covers the simple form and all ten of Rust's compound assignment operators. A predicate
/// testing `starts_with('=') && !starts_with("==")` would be blind to every compound
/// form: for `pool.open_batches += 1` the remainder begins `+`, so the write would never be seen
/// and the set pins would stay green on the weight-freeze release.
///
/// Also skips a leading run of indexed suffixes (`[0]`, `[idx]`) and dotted field segments
/// (`.value`), so `entries[0] = e` and `entries[0].value = 5` are both still seen as a write to
/// `entries`. Without this, `TopTier.entries` — the array `update_tier` owns, with no
/// behavioural backstop — could be written by index, or by field on an indexed entry, and neither
/// this predicate nor the crate-wide `mut`-declaration enumerator would notice.
///
/// Tokenised rather than prefix-matched, because the boundaries collide: `<<=` and `>>=` share a
/// prefix with the comparisons `<` and `>`, and `<=`/`>=` sit between them. The compound operators
/// are matched first as whole tokens, so `==`, `!=`, `<=` and `>=` all reach the simple-form test
/// and are rejected there — a comparison is never counted as a write.
///
/// `=>` is rejected explicitly. These helpers read raw text and callers strip only the
/// `#[cfg(test)]` tail, so match arms in doc comments and string literals are in scope. A false
/// positive here could only ever over-count against an exact assertion and so fails closed, but
/// the guard costs two characters and spares the next reader that argument.
pub fn is_assignment(after: &str) -> bool {
    let rest = skip_accessor_chain(after.trim_start());
    for op in ["<<=", ">>=", "+=", "-=", "*=", "/=", "%=", "|=", "&=", "^="] {
        if rest.starts_with(op) {
            return true;
        }
    }
    rest.starts_with('=') && !rest.starts_with("==") && !rest.starts_with("=>")
}

/// Skips a leading run of `[index]` and `.field` accessors, so the assignment test in
/// [`is_assignment`] lands after the last one — `entries[0].value = 5` and `pool.a.b = c` both
/// reach the `=`. Stops at the first accessor that is not one of these two forms: in particular a
/// `.ident` immediately followed by `(` is a method call, not a field, so `entries.len() == 0`
/// stops there and is never mistaken for an assignment target.
fn skip_accessor_chain(mut rest: &str) -> &str {
    loop {
        rest = rest.trim_start();
        if rest.starts_with('[') {
            let skipped = skip_index(rest);
            if skipped.len() == rest.len() {
                return rest; // unbalanced brackets; skip_index gave up, so stop here too.
            }
            rest = skipped;
            continue;
        }
        let Some(dotted) = rest.strip_prefix('.') else {
            return rest;
        };
        let ident_end = dotted
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(dotted.len());
        if ident_end == 0 {
            return rest; // `.` not followed by an identifier
        }
        let after_ident = &dotted[ident_end..];
        if after_ident.trim_start().starts_with('(') {
            return rest; // a method call, not a field access
        }
        rest = after_ident;
    }
}

/// Skips one leading `[...]` index suffix, depth-counted so a nested bracket does not close the
/// span early. Returns `rest` unchanged if it does not open with `[`, or if the brackets never
/// balance — a source-introspection predicate fails closed on input it should never see from
/// valid Rust, rather than guessing.
fn skip_index(rest: &str) -> &str {
    if !rest.starts_with('[') {
        return rest;
    }
    let mut depth = 0usize;
    for (i, c) in rest.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return rest[i + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    rest
}

/// The production half of a source file: everything before the first `#[cfg(test)]`.
pub fn production(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap()
}

/// A `#[derive(Accounts)]` struct's declaration text, verbatim — from directly after the derive
/// attribute through its closing brace, inclusive. Depth-counted rather than anchored on the
/// first `\n}`, so any code that ever follows the struct in the production half cannot extend
/// the span, and asserts there is exactly one such struct before returning: a bare `.nth(1)`
/// would otherwise silently ignore a second one.
///
/// This is what a whole-body `assert_eq!` pins per handler — see
/// `instructions/mod.rs`'s `every_accounts_struct_is_pinned_verbatim`.
pub fn accounts_body(source: &str) -> &str {
    let prod = production(source);
    assert_eq!(
        prod.split("#[derive(Accounts)]").count(),
        2,
        "expected exactly one #[derive(Accounts)] struct in the production source"
    );
    let after = prod.split("#[derive(Accounts)]").nth(1).unwrap();
    let open = after
        .find('{')
        .expect("accounts struct has no opening brace");
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in after[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    &after[..end.expect("accounts struct has no matching closing brace")]
}

/// The same text with every doc-comment line removed — the fix for a recurring hazard: several
/// pins read *raw source*, so a doc comment that names what it forbids reds its own pin (`bump`,
/// `init`, `token_standard`, `FreezeExecute`, `non-zero,` and `would return` have all triggered
/// it). Anchoring on syntax the prose does not use works but is fragile — the prose only has to
/// change. Stripping the prose is independent of both.
///
/// Deliberately **not** applied inside [`production`]: most pins want the doc comments, because a
/// `/// CHECK:` line is part of the account surface a whole-body pin needs verbatim. This is
/// opt-in, for the pins that assert an **absence**.
pub fn without_doc_comments(source: &str) -> String {
    source
        .lines()
        .filter(|line| {
            !line.trim_start().starts_with("///") && !line.trim_start().starts_with("//!")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// How many times `callee` is invoked in the production half, excluding the callee's own `fn`
/// declaration line — so a handler that calls itself recursively, or merely declares a function
/// of the same name, is not counted as calling it.
pub fn calls(source: &str, callee: &str) -> usize {
    production(source).matches(&format!("{callee}(")).count()
        - production(source).matches(&format!("fn {callee}(")).count()
}

/// Whitespace-collapsed production source, so an ordered-argument or ordered-field pin survives
/// rustfmt's line breaking while still asserting the order.
pub fn flat(source: &str) -> String {
    production(source)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// How many times `field` is assigned in the production half, over a newline-normalized copy so a
/// multi-line assignment (`field =\n    expr`) still counts. The trailing-boundary check stops
/// `pool.blank` from matching `pool.blank_faces`.
pub fn writes(source: &str, field: &str) -> usize {
    let flat = production(source).replace('\n', " ");
    let mut count = 0;
    let mut rest = flat.as_str();
    while let Some(pos) = rest.find(field) {
        let after = &rest[pos + field.len()..];
        let ends_the_reference =
            !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        if ends_the_reference && is_assignment(after) {
            count += 1;
        }
        rest = after;
    }
    count
}

/// Every `<receiver>.<field>` assigned in the production half, as a sorted set. [`writes`] answers
/// "how many times is *this* field assigned" and cannot see an assignment to a field the handler
/// does not own; this answers "which fields does it assign at all".
///
/// **Scope limit — keyed on the receiver's name.** A re-borrow (`let p = &mut ctx.accounts.pool;`)
/// is invisible here, as an aliased import is to a callee-name pin. Closing that needs binding-flow
/// analysis over the handler body, not a predicate change.
pub fn field_writes(source: &str, receiver: &str) -> Vec<String> {
    let flat = production(source).replace('\n', " ");
    let mut found: Vec<String> = Vec::new();
    for chunk in flat.split(&format!("{receiver}.")).skip(1) {
        let end = chunk
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(chunk.len());
        let (field, rest) = chunk.split_at(end);
        if is_assignment(rest) {
            found.push(field.to_string());
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Every `.rs` file under the crate's `src/`, so a whole-crate prohibition cannot be escaped by
/// putting the forbidden thing in a module the sweep's author had not thought of.
///
/// **Here rather than in one test module, because two sweeps needed it and only one had it.**
/// `core_asset.rs`'s ban on the two hooked account wrappers walked the crate; `close_seized.rs`'s
/// tripwire on `owed_fees` accrual listed five files by hand while its own comment claimed to be
/// "about the whole crate rather than this function". A sixth file accruing into `owed_fees` —
/// which is exactly what `commit_rolls` will bring — would have passed it. This module exists so
/// a shared predicate lives in one place; a source walk is one.
///
/// The length floor is the sweep's own positive control: a walk that silently returned nothing
/// would pass every prohibition built on it, so too few files is a failure rather than an empty
/// pass.
pub fn crate_sources() -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    assert!(
        found.len() > 30,
        "the crate walk found {} files, which is too few to be the whole crate — a broken walk \
         would pass the prohibitions built on it by scanning nothing",
        found.len()
    );
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_assignment_operator_is_separated_from_every_comparison() {
        for assignment in [
            " = 0;", "= 0;", " += 1", " -= 1", " *= 2", " /= 2", " %= 2", " |= 1", " &= 1",
            " ^= 1", " <<= 1", " >>= 1",
        ] {
            assert!(
                is_assignment(assignment),
                "missed an assignment: {assignment:?}"
            );
        }
        for comparison in [
            " == 0",
            "== 0",
            " != 0",
            " <= 0",
            " >= 0",
            " < 0",
            " > 0",
            ", 0",
            "; ",
            ")?",
            ".key()",
            " => Ok(())",
            "=> 6300,",
        ] {
            assert!(
                !is_assignment(comparison),
                "counted a non-write: {comparison:?}"
            );
        }
    }

    /// The operator list is complete, not merely long: Rust has exactly ten compound assignment
    /// operators, and a count pin is what makes "complete" checkable rather than asserted.
    #[test]
    fn the_operator_list_covers_all_ten_compound_forms() {
        let covered = ["+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>="];
        assert_eq!(covered.len(), 10);
        for op in covered {
            assert!(is_assignment(&format!(" {op} 1")), "uncovered: {op}");
        }
    }

    #[test]
    fn writes_counts_compound_forms_and_respects_the_trailing_boundary() {
        let src = "fn h() { pool.blanks = 1; pool.blanks += 2; pool.blanks_faces = 3; }";
        assert_eq!(writes(src, "pool.blanks"), 2);
        assert_eq!(writes(src, "pool.blanks_faces"), 1);
    }

    #[test]
    fn field_writes_sees_compound_assignments() {
        let src = "fn h() { pool.a = 1; pool.b += 2; if pool.c == 3 {} }";
        assert_eq!(
            field_writes(src, "pool"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    // --- indexed writes, the two forms `TopTier.entries` needs covered -----------------------

    #[test]
    fn is_assignment_recognizes_an_indexed_assignment() {
        assert!(is_assignment("[0] = e;"));
        assert!(is_assignment("[idx] += 1;"));
        assert!(
            !is_assignment("[0] == e"),
            "an indexed comparison is not a write"
        );
    }

    #[test]
    fn field_writes_sees_an_indexed_write() {
        let src = "fn h() { top_tier.entries[0] = e; if top_tier.len == 1 {} }";
        assert_eq!(field_writes(src, "top_tier"), vec!["entries".to_string()]);
    }

    // --- the segment after the index suffix, and where the chain must stop -------------------

    #[test]
    fn is_assignment_recognizes_a_field_write_on_an_indexed_entry() {
        assert!(is_assignment("[0].value = 5"));
        assert!(is_assignment(".b = c"), "a bare dotted segment is a write");
        assert!(
            !is_assignment("[0].value == 5"),
            "an indexed field comparison is not a write"
        );
        assert!(
            !is_assignment(".len() == 0"),
            "a method call is never mistaken for a field"
        );
        assert!(!is_assignment("[0].value"), "a bare access is not a write");
    }

    #[test]
    fn field_writes_sees_a_field_write_on_an_indexed_entry() {
        let src = "fn h() { top_tier.entries[0].value = 5; if top_tier.entries.len() == 0 {} }";
        assert_eq!(field_writes(src, "top_tier"), vec!["entries".to_string()]);
    }

    // --- `accounts_body` and `production` are this module's only two public helpers with no
    // test of their own — but a helper can be tested behaviourally rather than by a text pin of
    // a text pin: this module already tests three of `accounts_body`'s siblings, and a
    // behavioural test does not regress. ---------------------------------------------------------

    #[test]
    #[should_panic(expected = "expected exactly one")]
    fn accounts_body_panics_on_a_second_derive_accounts_struct() {
        let src = "#[derive(Accounts)]\n\
                   pub struct A<'info> { pub x: Signer<'info>, }\n\
                   #[derive(Accounts)]\n\
                   pub struct B<'info> { pub y: Signer<'info>, }\n";
        // The guard this weakens is `prod.split(\"#[derive(Accounts)]\").count() == 2`. A
        // silent `.nth(1)` over a weakened guard would instead return struct A's own body here
        // and never notice B exists.
        accounts_body(src);
    }

    #[test]
    fn accounts_body_stops_at_the_struct_s_own_closing_brace() {
        let src = "#[derive(Accounts)]\n\
                   pub struct A<'info> {\n    pub x: Signer<'info>,\n}\n\n\
                   fn trailing_code_after_the_struct() {}\n";
        let body = accounts_body(src);
        assert!(
            body.trim_end().ends_with('}'),
            "accounts_body's span should end at the struct's own closing brace: {body:?}"
        );
        assert!(
            !body.contains("trailing_code_after_the_struct"),
            "accounts_body's span leaked past the struct's closing brace into code that follows \
             it — the whole-body pin this backs (`every_accounts_struct_is_pinned_verbatim`) \
             would then compare against text the handler never declared"
        );
    }

    #[test]
    fn production_truncates_at_a_doc_comment_that_only_looks_like_the_real_attribute() {
        // `production` finds the first literal occurrence of "#[cfg(test)]" in the source —
        // this file's own doc comments mention that literal text twice (as documentation of
        // what production() strips) before the real attribute 172 lines later, which is exactly
        // this false-boundary shape and is why `production(TEST_SUPPORT_SRC)` would truncate
        // early were anything to `include_str!` this file. Latent today — nothing does — but the
        // same shape has landed in a handler before and failed closed loudly;
        // this pins the shape at its source rather than leaving it implicit.
        //
        // Whoever hardens `production` to stop at the real attribute instead of the first
        // literal match must UPDATE this assertion to expect `real_handler` to survive, not
        // delete it as obsolete — a red result here is that fix working, not this test rotting.
        let src = "/// docs mention `#[cfg(test)]` as an example of the boundary this strips\n\
                   fn real_handler() { do_thing(); }\n\n\
                   #[cfg(test)]\n\
                   mod tests {\n    fn t() {}\n}\n";
        let prod = production(src);
        assert!(
            !prod.contains("real_handler"),
            "production() should have truncated before the doc comment's literal \
             `#[cfg(test)]`, well above the real handler — instead it kept scanning past a \
             false boundary: {prod:?}"
        );
    }
}
