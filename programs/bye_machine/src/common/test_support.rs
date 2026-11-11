//! Shared predicates for the crate's source-introspection tests.
//!
//! These predicates cover what has no runtime expression without a validator:
//! `#[derive(Accounts)]` attributes, CPI argument ordering, and the set of fields a handler
//! writes. They live in one place so that a correction (e.g. to [`is_assignment`]) applies to
//! every file that uses them.

/// Whether the text immediately following a field reference begins an **assignment** to it.
///
/// Covers the simple form and all ten of Rust's compound assignment operators; a plain
/// `starts_with('=')` test would miss every compound form (`pool.open_batches += 1`).
///
/// Also skips a leading run of indexed suffixes (`[0]`, `[idx]`) and dotted field segments
/// (`.value`), so `entries[0] = e` and `entries[0].value = 5` are both seen as a write to
/// `entries`.
///
/// Tokenised rather than prefix-matched, because the boundaries collide: `<<=` and `>>=` share a
/// prefix with the comparisons `<` and `>`, and `<=`/`>=` sit between them. The compound operators
/// are matched first as whole tokens, so `==`, `!=`, `<=` and `>=` all reach the simple-form test
/// and are rejected there — a comparison is never counted as a write.
///
/// `=>` is rejected explicitly, so a match arm is never read as an assignment.
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

/// Source text with every comment blanked to spaces — and, when `blank_strings` is set, the
/// contents of every string and character literal too. Newlines and character positions are kept,
/// so offsets and line structure survive.
///
/// A lexer rather than a regex: `//` inside a string is not a comment, a `'"'` char literal does
/// not open a string, `'a` is a lifetime and not a char literal, block comments nest, and raw
/// strings (`r#"…"#`) end only at their matching hash count.
fn blank(source: &str, blank_strings: bool) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let space = |c: char| if c == '\n' { '\n' } else { ' ' };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0usize;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(space(chars[i]));
                    i += 1;
                }
            }
        } else if c == 'r'
            && matches!(next, Some('"') | Some('#'))
            && {
                let mut j = i + 1;
                while chars.get(j) == Some(&'#') {
                    j += 1;
                }
                chars.get(j) == Some(&'"')
            }
            && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'))
        {
            let mut hashes = 0;
            let mut j = i + 1;
            while chars[j] == '#' {
                hashes += 1;
                j += 1;
            }
            out.extend(&chars[i..=j]);
            i = j + 1;
            while i < chars.len() {
                let closes =
                    chars[i] == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#'));
                if closes {
                    out.extend(&chars[i..=i + hashes]);
                    i += hashes + 1;
                    break;
                }
                out.push(if blank_strings {
                    space(chars[i])
                } else {
                    chars[i]
                });
                i += 1;
            }
        } else if c == '"' {
            out.push('"');
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    out.push(if blank_strings { ' ' } else { chars[i] });
                    i += 1;
                }
                out.push(if blank_strings {
                    space(chars[i])
                } else {
                    chars[i]
                });
                i += 1;
            }
            if i < chars.len() {
                out.push('"');
                i += 1;
            }
        } else if c == '\'' {
            let escaped = next == Some('\\');
            let plain = chars.get(i + 2) == Some(&'\'');
            if escaped || plain {
                let mut j = i + 1;
                if escaped {
                    j += 1;
                }
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                out.push('\'');
                for &ch in &chars[i + 1..j.min(chars.len())] {
                    out.push(if blank_strings { ' ' } else { ch });
                }
                if j < chars.len() {
                    out.push('\'');
                }
                i = j + 1;
            } else {
                out.push(c);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// The production half of a source file: everything before the first `#[cfg(test)]`, with
/// comments blanked so that what an assertion reads is code, never prose. String literals are
/// kept; counters that must ignore them use [`code`].
///
/// The split happens after blanking, so a `#[cfg(test)]` mentioned in a comment cannot cut the
/// production half short.
pub fn production(source: &str) -> String {
    blank(source, false)
        .split("#[cfg(test)]")
        .next()
        .unwrap()
        .to_string()
}

/// [`production`] with string and character literal contents blanked as well — the text a call
/// or assignment counter should scan, so a callee named inside a message cannot be counted.
pub fn code(source: &str) -> String {
    blank(source, true)
        .split("#[cfg(test)]")
        .next()
        .unwrap()
        .to_string()
}

/// A `#[derive(Accounts)]` struct's declaration text — from directly after the derive attribute
/// through its closing brace, inclusive, comments blanked. Depth-counted rather than anchored on
/// the first `\n}`, so any code that ever follows the struct in the production half cannot extend
/// the span, and asserts there is exactly one such struct before returning: a bare `.nth(1)`
/// would otherwise silently ignore a second one.
pub fn accounts_body(source: &str) -> String {
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
    after[..end.expect("accounts struct has no matching closing brace")].to_string()
}

/// How many times `callee` is invoked in the production half, excluding the callee's own `fn`
/// declaration line — so a handler that calls itself recursively, or merely declares a function
/// of the same name, is not counted as calling it.
pub fn calls(source: &str, callee: &str) -> usize {
    let code = code(source);
    code.matches(&format!("{callee}(")).count() - code.matches(&format!("fn {callee}(")).count()
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
    let flat = code(source).replace('\n', " ");
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
    let flat = code(source).replace('\n', " ");
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
/// The length floor is a positive control: a walk that silently returned nothing would pass
/// every prohibition built on it, so too few files is a failure.
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

/// How many times `seed` is named in the production half outside the places that only identify
/// an account: a `use` import, the constant's own `const` definition, and an exact
/// `seeds = [seed]` constraint inside an `#[account(...)]` attribute. What remains is every place
/// the seed is built into signer seeds or a derivation, however that expression is spelled.
pub fn seed_uses_outside_constraints(source: &str, seed: &str) -> usize {
    let code = code(source);

    let mut kept = String::with_capacity(code.len());
    let mut skipping = false;
    for line in code.lines() {
        let item = line.trim_start();
        let item = item
            .strip_prefix("pub(crate) ")
            .or_else(|| item.strip_prefix("pub "))
            .unwrap_or(item);
        if !skipping && (item.starts_with("use ") || item.starts_with(&format!("const {seed}"))) {
            skipping = true;
        }
        if skipping {
            if line.contains(';') {
                skipping = false;
            }
            kept.push('\n');
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }

    let constraint = format!("seeds=[{seed}]");
    let mut constraints = 0;
    let mut rest = kept.as_str();
    while let Some(start) = rest.find("#[account(") {
        let body = &rest[start + "#[account".len()..];
        let mut depth = 0usize;
        let mut end = body.len();
        for (i, c) in body.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let flat: String = body[..end].chars().filter(|c| !c.is_whitespace()).collect();
        constraints += flat.matches(&constraint).count();
        rest = &body[end..];
    }

    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let named = kept
        .match_indices(seed)
        .filter(|(at, _)| {
            !kept[..*at].ends_with(is_ident) && !kept[at + seed.len()..].starts_with(is_ident)
        })
        .count();
    named - constraints
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

    /// Rust has exactly ten compound assignment operators; the list covers all of them.
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

    // --- `accounts_body` and `production` -----------------------------------------------------

    #[test]
    #[should_panic(expected = "expected exactly one")]
    fn accounts_body_panics_on_a_second_derive_accounts_struct() {
        let src = "#[derive(Accounts)]\n\
                   pub struct A<'info> { pub x: Signer<'info>, }\n\
                   #[derive(Accounts)]\n\
                   pub struct B<'info> { pub y: Signer<'info>, }\n";
        // A silent `.nth(1)` would return struct A's own body here and never notice B exists.
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
             it, so a check built on it would read text the handler never declared"
        );
    }

    #[test]
    fn production_ignores_a_cfg_test_that_only_appears_in_a_comment_or_string() {
        let src = "/// docs mention `#[cfg(test)]` as an example\n\
                   fn real_handler() { let m = \"#[cfg(test)]\"; do_thing(); }\n\n\
                   #[cfg(test)]\n\
                   mod tests {\n    fn t() {}\n}\n";
        let prod = production(src);
        assert!(
            prod.contains("real_handler"),
            "cut at a false boundary: {prod:?}"
        );
        assert!(
            !prod.contains("fn t()"),
            "kept the real test module: {prod:?}"
        );
    }

    #[test]
    fn a_callee_named_only_in_a_comment_or_string_is_not_counted() {
        let src = "/// calls `assert_weight_open(` first\n\
                   // and again: assert_weight_open(pool)\n\
                   /* assert_weight_open(pool) */\n\
                   fn h() { let m = \"assert_weight_open(\"; assert_weight_open(pool); }\n";
        assert_eq!(calls(src, "assert_weight_open"), 1);
        assert_eq!(
            writes(
                "// pool.a = 1;\nfn h() { let s = \"pool.a = 2\"; pool.a = 3; }",
                "pool.a"
            ),
            1
        );
        assert_eq!(
            field_writes("/// pool.x = 1\nfn h() { pool.y = 2; }", "pool"),
            vec!["y".to_string()]
        );
    }

    #[test]
    fn the_blanker_reads_rust_lexically() {
        // `//` inside a string is not a comment; `'"'` does not open a string; `'a` is a lifetime;
        // block comments nest; raw strings end at their own hash count.
        let src = "let u = \"http://x\"; let q = '\"'; fn f<'a>(x: &'a str) {} /* a /* b */ c */ \
                   let r = r#\"a \"quoted\" // b\"#; end();";
        let out = code(src);
        assert!(out.contains("end();"), "{out:?}");
        assert!(out.contains("fn f<'a>(x: &'a str)"), "{out:?}");
        assert!(!out.contains("http"), "{out:?}");
        assert!(!out.contains("quoted"), "{out:?}");
        assert!(!out.contains(" c "), "{out:?}");
        assert_eq!(out.chars().count(), src.chars().count());
    }

    /// Every spelling of a signer-seed expression counts; only imports, the definition and an
    /// account's own `seeds = [..]` constraint do not.
    #[test]
    fn seed_uses_count_every_spelling_of_a_signer_seed_and_no_constraint() {
        let seed = "PROTOCOL_CONFIG_SEED";
        let count = |src: &str| seed_uses_outside_constraints(src, seed);

        assert_eq!(
            count("let s: &[&[u8]] = &[PROTOCOL_CONFIG_SEED, &[bump]];"),
            1
        );
        assert_eq!(count("let s = [PROTOCOL_CONFIG_SEED, &[bump][..]];"), 1);
        assert_eq!(
            count("let s = [crate::common::seeds::PROTOCOL_CONFIG_SEED, b];"),
            1
        );
        assert_eq!(count("let seeds = [PROTOCOL_CONFIG_SEED];"), 1);
        assert_eq!(
            count("let a = [PROTOCOL_CONFIG_SEED];\nlet b = [PROTOCOL_CONFIG_SEED, &[x]];"),
            2
        );

        assert_eq!(
            count("use crate::common::seeds::{A, PROTOCOL_CONFIG_SEED};"),
            0
        );
        assert_eq!(
            count("use crate::common::seeds::{\n    A,\n    PROTOCOL_CONFIG_SEED,\n};"),
            0
        );
        assert_eq!(
            count("pub const PROTOCOL_CONFIG_SEED: &[u8] = b\"protocol\";"),
            0
        );
        assert_eq!(
            count("#[account(seeds = [PROTOCOL_CONFIG_SEED], bump = c.bump)]"),
            0
        );
        assert_eq!(
            count("#[account(\n    mut,\n    seeds = [ PROTOCOL_CONFIG_SEED ],\n    bump\n)]"),
            0
        );
        assert_eq!(
            count("// [PROTOCOL_CONFIG_SEED, &[bump]]\nlet m = \"PROTOCOL_CONFIG_SEED\";"),
            0
        );
        assert_eq!(count("let x = MY_PROTOCOL_CONFIG_SEED_COPY;"), 0);

        // A constraint naming the seed beside another component is a derivation, not the singleton.
        assert_eq!(
            count("#[account(seeds = [PROTOCOL_CONFIG_SEED, k.as_ref()], bump)]"),
            1
        );
        // A constraint-shaped expression outside any attribute is still a use.
        assert_eq!(
            count("let v = vec![]; let seeds = [PROTOCOL_CONFIG_SEED]; f(&seeds);"),
            1
        );
    }
}
