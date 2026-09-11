// Issue #7 / D-156 — `init_protocol` accepts only `INITIAL_ADMINISTRATOR` as `payer`, so that
// constant is what decides who holds the root of the privilege set. Two things about it are
// checkable without a validator, and both are checked here.
//
// 1. BOTH LOCALNET BRANCHES MATCH THE KEY THE SUITE SIGNS WITH. There are two, not one, and that
//    is the subtle part: `anchor build` passes no features and lands on the `else` arm, while
//    `docker-compose.yml`'s `build` service passes `--features dev,testing` and lands on the
//    `testing` arm. Each carries its own base58 literal; `tests/helpers/env.ts`'s `loadSuiteAdmin`
//    loads `accounts/dev/deployer.json`. Nothing links the three. Rotate that keypair file, or fix
//    up only one arm, and every integration file fails at once inside `ensureProtocol` as a wall
//    of unrelated-looking 6503s. This test turns that into one failure that names the cause.
//
// 2. THE PROD BRANCH IS NOT THE COMMITTED THROWAWAY. `accounts/dev/deployer.json`'s SECRET IS IN
//    THE REPOSITORY. If it ever reached a `--features prod` build it would be a mainnet
//    Administrator anybody could sign as. The `dev` branch is deliberately NOT constrained the
//    same way — devnet custody is the operator's call, and reusing the throwaway there is a
//    legitimate choice for a devnet rehearsal.
//
// The scanner below reads the `.rs` source off disk, in the same spirit as
// idl-vs-rust-accounts.test.ts. Its own controls are at the bottom of the file: a mutated source
// must report the mutated value (proving it reads rather than remembers), and a source with the
// declaration deleted must throw rather than return a default. Without those, a regex that
// silently matched nothing would read as a passing test.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { loadSuiteAdmin } from "../helpers/env.js";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const PROGRAM_ID_RS = join(REPO_ROOT, "programs/bye_machine/src/program_id.rs");

type Branch = "prod" | "testing" | "dev" | "default";

/** The arms a localnet integration run can be built from: `anchor build` takes no features and
 * lands on `default`, while `docker-compose.yml`'s `build` service passes `--features dev,testing`
 * and lands on `testing`. Both must bootstrap with the same key or the two paths disagree. */
const LOCALNET_BRANCHES = ["testing", "default"] as const;

/**
 * Extracts every `INITIAL_ADMINISTRATOR` declaration from `program_id.rs`, keyed by the cfg
 * branch that guards it. Walks lines and tracks which arm of the `cfg_if!` is open rather than
 * matching one regex over the whole file — the three declarations are textually identical apart
 * from the literal, so a whole-file regex cannot tell them apart.
 */
function extractInitialAdministrators(source: string): Record<Branch, string> {
  const out: Partial<Record<Branch, string>> = {};
  let branch: Branch | null = null;
  const lines = source.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const t = lines[i].trim();
    const opens = /^(?:}\s*else\s+)?if\s+#\[cfg\(feature\s*=\s*"(prod|testing|dev)"\)\]/.exec(t);
    if (opens !== null) branch = opens[1] as Branch;
    else if (/^}\s*else\s*\{/.test(t)) branch = "default";

    if (!t.startsWith("pub const INITIAL_ADMINISTRATOR")) continue;
    if (branch === null) {
      throw new Error(
        `INITIAL_ADMINISTRATOR declared at line ${i + 1} outside any cfg_if branch — ` +
          `the scanner cannot attribute it, so the pin below would check the wrong build`,
      );
    }
    const rest = lines.slice(i, i + 3).join("\n");
    const lit = /pubkey!\("([1-9A-HJ-NP-Za-km-z]{32,44})"\)/.exec(rest);
    if (lit === null) {
      throw new Error(
        `INITIAL_ADMINISTRATOR (${branch} branch, line ${i + 1}) has no pubkey!("…") literal ` +
          `within 3 lines — the scanner found the declaration but not its value`,
      );
    }
    if (out[branch] !== undefined) {
      throw new Error(`INITIAL_ADMINISTRATOR declared twice for the ${branch} branch`);
    }
    out[branch] = lit[1];
  }

  for (const b of ["prod", "testing", "dev", "default"] as const) {
    if (out[b] === undefined) {
      throw new Error(`no INITIAL_ADMINISTRATOR found for the ${b} branch of program_id.rs`);
    }
  }
  return out as Record<Branch, string>;
}

/**
 * The `cfg_if!` block that declares `INITIAL_ADMINISTRATOR`, isolated from the `PROGRAM_ID` block
 * above it. Anchored on the last `cfg_if::cfg_if!` opening that precedes the first declaration —
 * slicing by a fraction of the file, or counting cfg arms across the whole file, would silently
 * follow `PROGRAM_ID`'s arms instead if either block gained or lost one.
 */
function initialAdministratorBlock(source: string): string {
  const decl = source.indexOf("pub const INITIAL_ADMINISTRATOR");
  if (decl === -1) throw new Error("no INITIAL_ADMINISTRATOR declaration in program_id.rs");
  const open = source.lastIndexOf("cfg_if::cfg_if!", decl);
  if (open === -1) {
    throw new Error("INITIAL_ADMINISTRATOR is not inside a cfg_if! block");
  }
  return source.slice(open);
}

const SOURCE = readFileSync(PROGRAM_ID_RS, "utf8");

describe("INITIAL_ADMINISTRATOR", () => {
  it("declares exactly the four cfg branches", () => {
    const found = extractInitialAdministrators(SOURCE);
    assert.deepEqual(Object.keys(found).sort(), ["default", "dev", "prod", "testing"]);
  });

  it("resolves `testing` before `dev`, so --features dev,testing is a localnet build", () => {
    // docker-compose.yml's `build` service passes BOTH. If `dev` were tested first, the
    // dockerised integration run would bootstrap with the devnet key and every file would fail
    // inside ensureProtocol — the arm order is the fix, so it is pinned rather than trusted.
    const arms = [...initialAdministratorBlock(SOURCE).matchAll(
      /#\[cfg\(feature\s*=\s*"(prod|testing|dev)"\)\]/g,
    )].map((m) => m[1]);
    assert.deepEqual(arms, ["prod", "testing", "dev"]);
  });

  for (const branch of LOCALNET_BRANCHES) {
    it(`\`${branch}\` branch is accounts/dev/deployer.json — the key the suite signs init_protocol with`, async () => {
      const found = extractInitialAdministrators(SOURCE);
      const admin = await loadSuiteAdmin();
      assert.equal(
        found[branch],
        admin.address,
        `program_id.rs's ${branch}-branch INITIAL_ADMINISTRATOR no longer matches ` +
          "accounts/dev/deployer.json — rotate one and you must rotate the other, or every " +
          "integration file fails inside ensureProtocol with 6503",
      );
    });
  }

  it("prod branch is never the committed throwaway, whose secret is in the repo", async () => {
    const { prod } = extractInitialAdministrators(SOURCE);
    const admin = await loadSuiteAdmin();
    assert.notEqual(
      prod,
      admin.address,
      "the prod INITIAL_ADMINISTRATOR is accounts/dev/deployer.json — its secret key is " +
        "committed, so this would make the mainnet Administrator signable by anyone",
    );
  });
});

// ── the scanner's own controls ──────────────────────────────────────────────────────────────
// Both are the discriminating kind. The first mutates ONLY the default branch's literal and
// asserts the scanner reports the mutation — a scanner that returned a remembered value, or that
// matched the prod branch by accident, fails it. The second deletes the declaration and asserts a
// throw — a scanner that returned `undefined` would let the pins above pass on an empty read.
describe("extractInitialAdministrators (controls)", () => {
  const MUTANT = "8pM1kQhCYaNKQ4KcJ6yqUqCzt3EJhVXBLmVnpvWzDdKu";

  it("reports mutated localnet literals, not remembered ones", () => {
    // `testing` and `default` legitimately carry the SAME literal, so a single-occurrence
    // `replace` would only hit the first of them — mutate every occurrence and require both arms
    // to move. A scanner that attributed one arm's value to the other fails this.
    const original = extractInitialAdministrators(SOURCE);
    const mutated = SOURCE.replaceAll(
      `pubkey!("${original.default}")`,
      `pubkey!("${MUTANT}")`,
    );
    assert.notEqual(mutated, SOURCE, "control did not mutate the source — the pin is vacuous");

    const found = extractInitialAdministrators(mutated);
    assert.equal(found.default, MUTANT);
    assert.equal(found.testing, MUTANT);
    assert.equal(found.prod, original.prod, "mutating the localnet arms moved the prod arm");
    assert.equal(found.dev, original.dev, "mutating the localnet arms moved the dev arm");
  });

  it("throws when the declaration is absent rather than reporting nothing", () => {
    const stripped = SOURCE.split("\n")
      .filter((l) => !l.trim().startsWith("pub const INITIAL_ADMINISTRATOR"))
      .join("\n");
    assert.throws(() => extractInitialAdministrators(stripped), /no INITIAL_ADMINISTRATOR found/);
  });

  it("throws when a declaration has no pubkey literal near it", () => {
    const broken = SOURCE.replace(
      /pub const INITIAL_ADMINISTRATOR: Pubkey =\n\s*pubkey!\("[1-9A-HJ-NP-Za-km-z]{32,44}"\);/,
      "pub const INITIAL_ADMINISTRATOR: Pubkey = SOMETHING_ELSE;",
    );
    assert.notEqual(broken, SOURCE, "control did not mutate the source — the pin is vacuous");
    assert.throws(() => extractInitialAdministrators(broken), /has no pubkey!/);
  });
});
