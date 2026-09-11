// T-50 — the differential-oracle prototype from the P7 open, extended into the suite.
//
// This checks a DIFFERENT axis from idl-oracle.test.ts, and the two are not substitutes for one
// another: this file compares the on-disk IDL's account order against the Rust
// `#[derive(Accounts)]` declarations themselves — it catches the IDL going stale against Rust.
// idl-oracle.test.ts compares the hand-written TS encoder against the IDL — it catches the
// client drifting from the IDL. Neither subsumes the other.
//
// The extractor below reads `#[derive(Accounts)]` structs directly out of the `.rs` source with
// its own regex-based scanner, independent of the crate's own runtime accounts declarations
// (which would share its faults) and independent of anchor-cli's own IDL generation. Pure —
// reads files off disk, no validator, no Rust build, so this runs in `pnpm test` from day one.
//
// The control is the discriminating kind: it swaps two ADJACENT entries of `UpdateTier` — same
// length, same name set, order-only — the case a set- or length-based comparator passes. Keep
// that property in any further edit to this file; a control that only proves the comparator
// runs proves nothing.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { loadIdl } from "./idl-oracle-support.js";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const PROGRAM_SRC = join(REPO_ROOT, "programs/bye_machine/src");

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const p = join(dir, entry);
    const s = statSync(p);
    if (s.isDirectory()) walk(p, out);
    else if (p.endsWith(".rs")) out.push(p);
  }
  return out;
}

type RustAccountsStruct = { file: string; fields: string[] };

/**
 * Independent extractor: `#[derive(Accounts)]` -> `pub struct NAME` -> ordered `pub field:` at
 * depth 1 — a line-scanning parser, not the crate's own declared account list. `duplicates`
 * collects any struct name seen more than once (the map itself keeps the last one, matching the
 * prototype's own behavior); a caller decides whether that is fatal.
 */
function extractAccountsStructs(): { structs: Map<string, RustAccountsStruct>; duplicates: string[] } {
  const structs = new Map<string, RustAccountsStruct>();
  const duplicates: string[] = [];
  for (const f of walk(PROGRAM_SRC)) {
    const src = readFileSync(f, "utf8");
    const lines = src.split("\n");
    for (let i = 0; i < lines.length; i++) {
      if (!/^\s*#\[derive\(Accounts\)\]\s*$/.test(lines[i])) continue;
      let j = i + 1;
      while (j < lines.length && !/pub struct\s+(\w+)/.test(lines[j])) j++;
      const nameMatch = lines[j]?.match(/pub struct\s+(\w+)/);
      if (!nameMatch) continue;
      const name = nameMatch[1];

      let depth = 0;
      let started = false;
      let inAttr = false;
      const fields: string[] = [];
      for (; j < lines.length; j++) {
        const line = lines[j];
        if (started && depth === 1 && !inAttr) {
          const m = line.match(/^\s*pub\s+(\w+)\s*:/);
          if (m) fields.push(m[1]);
        }
        const opens = (line.match(/[{]/g) || []).length;
        const closes = (line.match(/[}]/g) || []).length;
        if (/^\s*#\[/.test(line)) {
          inAttr = !(line.includes("]") && (line.match(/\[/g) || []).length === (line.match(/\]/g) || []).length);
        } else if (inAttr && line.includes(")]")) {
          inAttr = false;
        }
        depth += opens - closes;
        if (opens) started = true;
        if (started && depth === 0) break;
      }

      if (structs.has(name)) duplicates.push(`${name} (${structs.get(name)!.file} vs ${f})`);
      structs.set(name, { file: f, fields });
    }
  }
  return { structs, duplicates };
}

function pascalCase(snake: string): string {
  return snake
    .split("_")
    .map((w) => w[0].toUpperCase() + w.slice(1))
    .join("");
}

const idl = loadIdl();
const { structs, duplicates } = extractAccountsStructs();

describe("Rust #[derive(Accounts)] order vs IDL account order", () => {
  it("has exactly one #[derive(Accounts)] struct per name crate-wide — no test-only shadow struct survives", () => {
    assert.deepEqual(duplicates, [], duplicates.join("\n"));
  });

  it("finds a matching struct for all 25 instructions", () => {
    assert.equal(idl.instructions.length, 25);
    const missing = idl.instructions
      .map((ix) => ({ name: ix.name, key: pascalCase(ix.name) }))
      .filter(({ key }) => !structs.has(key));
    assert.deepEqual(
      missing,
      [],
      missing.map(({ name, key }) => `MISSING struct for ${name} (looked for ${key})`).join("\n"),
    );
  });

  it("agrees, field for field and in order, for all 22 instructions", () => {
    const problems: string[] = [];
    for (const ix of idl.instructions) {
      const want = ix.accounts.map((a) => a.name);
      const got = structs.get(pascalCase(ix.name));
      if (!got) continue; // reported by the previous test; avoid a duplicate failure here
      const same = got.fields.length === want.length && got.fields.every((f, k) => f === want[k]);
      if (!same) {
        problems.push(
          `MISMATCH ${ix.name} (${got.file}): idl(${want.length})=[${want.join(", ")}] ` +
            `source(${got.fields.length})=[${got.fields.join(", ")}]`,
        );
      }
    }
    assert.deepEqual(problems, [], problems.join("\n"));
  });

  // The discriminating control: same length, same name set as UpdateTier's real fields — only
  // the ORDER of two adjacent entries is wrong. A length or Set-membership comparator passes
  // this; the comparator above must not.
  it("self-test: an adjacent swap in UpdateTier's extracted fields is detected as a mismatch", () => {
    const ctl = structs.get("UpdateTier");
    assert.ok(ctl, "UpdateTier struct not found");
    const swapped = [...ctl.fields];
    [swapped[2], swapped[3]] = [swapped[3], swapped[2]];
    const want = idl.instructions.find((i) => i.name === "update_tier")!.accounts.map((a) => a.name);
    const same = swapped.length === want.length && swapped.every((f, k) => f === want[k]);
    assert.equal(same, false, "control failed — comparator is blind to an order-only swap");
  });
});
