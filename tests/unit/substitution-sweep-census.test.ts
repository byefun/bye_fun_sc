// The account-substitution sweep, checked against the crate's own census two ways.
//
// The totals below (314 slots, 205/109, 6/43/16/96/153) are an aggregate check — cheap, but blind to
// two slots trading classifications, a field renamed to another real field's name, or one slot
// dropped while an unrelated one is added: every total involved survives all three. The exact
// check is `tests/fixtures/sweep-census.json`, a committed artifact both languages compare
// against independently — `programs/bye_machine/src/instructions/source_audit.rs`'s own
// `sweep_census_matches_the_committed_fixture` asserts the crate's *current* census still
// matches it, and this file's own last describe block asserts `SWEEP_SLOTS` matches the same
// entries, sorted the same way, with nothing missing and nothing invented. `SWEEP_SLOTS` is
// transcribed off the crate, not re-derived here — the fixture is what makes a transcription
// error visible instead of silently drifting from what it was transcribed against.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  DEFERRED_SWEEP_SLOTS,
  EXECUTED_SWEEP_ROWS,
  SWEEP_SLOTS,
  sweepSlotKey,
  type SweepSlot,
  type SweepClass,
  type SeedAnchorKind,
} from "../helpers/substitution-sweep.js";

const COMMITTED_CENSUS: readonly SweepSlot[] = JSON.parse(
  readFileSync(fileURLToPath(new URL("../fixtures/sweep-census.json", import.meta.url)), "utf8"),
) as SweepSlot[];

describe("the substitution sweep matches the crate's own pinned totals", () => {
  it("has 314 slots total, matching the sweep the crate's census sizes", () => {
    assert.equal(SWEEP_SLOTS.length, 314);
  });

  it("partitions 205 state / 109 token-or-metadata", () => {
    const counts: Record<SweepClass, number> = { State: 0, TokenOrMetadata: 0 };
    for (const slot of SWEEP_SLOTS) counts[slot.class] += 1;
    assert.deepEqual(counts, { State: 205, TokenOrMetadata: 109 });
  });

  it("partitions 6 singleton / 43 fully-self / 16 partial / 96 external", () => {
    const counts: Record<SeedAnchorKind, number> = {
      Singleton: 0,
      FullySelf: 0,
      Partial: 0,
      External: 0,
      NotSeeded: 0,
    };
    for (const slot of SWEEP_SLOTS) counts[slot.anchor] += 1;
    // The singletons here are the shared Treasury 03 and Treasury 04, wherever they are seeded.
    // protocol_config and the VRF request identity, singletons too, are excluded from
    // State/TokenOrMetadata by the crate's own slot_class, before seed_anchors ever runs.
    assert.deepEqual(counts, { Singleton: 6, FullySelf: 43, Partial: 16, External: 96, NotSeeded: 153 });
  });

  it("lists each slot exactly once", () => {
    const keys = SWEEP_SLOTS.map(sweepSlotKey);
    assert.equal(new Set(keys).size, keys.length, "a slot is listed twice");
  });
});

describe("every slot is accounted for — executed or deferred, never both, never neither", () => {
  it("accounts for every slot exactly once", () => {
    const executed = new Set(EXECUTED_SWEEP_ROWS.map(sweepSlotKey));
    const deferred = new Set(DEFERRED_SWEEP_SLOTS.map(sweepSlotKey));

    const both = [...executed].filter((k) => deferred.has(k));
    assert.deepEqual(both, [], `slots both executed and deferred: ${both.join(", ")}`);

    const neither = SWEEP_SLOTS.map(sweepSlotKey).filter((k) => !executed.has(k) && !deferred.has(k));
    assert.deepEqual(neither, [], `slots neither executed nor declared as debt: ${neither.join(", ")}`);
  });

  it("executes and defers nothing that is not a real slot", () => {
    const known = new Set(SWEEP_SLOTS.map(sweepSlotKey));
    const orphanedExecuted = EXECUTED_SWEEP_ROWS.map(sweepSlotKey).filter((k) => !known.has(k));
    const orphanedDeferred = DEFERRED_SWEEP_SLOTS.map(sweepSlotKey).filter((k) => !known.has(k));
    assert.deepEqual(orphanedExecuted, [], `executed rows naming a slot not in SWEEP_SLOTS: ${orphanedExecuted.join(", ")}`);
    assert.deepEqual(orphanedDeferred, [], `deferred slots naming a slot not in SWEEP_SLOTS: ${orphanedDeferred.join(", ")}`);
  });

  it("gives every deferred slot a reason a reader can weigh", () => {
    for (const slot of DEFERRED_SWEEP_SLOTS) {
      assert.ok(slot.reason.trim().length > 20, `${sweepSlotKey(slot)} is deferred without a usable reason`);
    }
  });

  it("pins a specific error code on every executed row", () => {
    for (const row of EXECUTED_SWEEP_ROWS) {
      assert.ok(
        Number.isInteger(row.code) && row.code > 0,
        `${sweepSlotKey(row)} names no error code — any failure would satisfy it`,
      );
      assert.ok(row.mechanism.trim().length > 10, `${sweepSlotKey(row)} names no rejecting mechanism`);
    }
  });

  it("executes only Partial slots so far, and names the mechanism rather than assuming the anchor rejects", () => {
    // The sweep's own blind spot: FullySelf always passes seeds, and a same-pool Partial sibling
    // does too — so every row executed so far had to identify a mechanism other than `seeds`.
    for (const row of EXECUTED_SWEEP_ROWS) {
      assert.equal(row.anchor, "Partial", `${sweepSlotKey(row)}: only Partial slots are executed so far`);
    }
  });
});

describe("SWEEP_SLOTS matches the committed fixture exactly, entry for entry", () => {
  function sortedByKey(slots: readonly SweepSlot[]): SweepSlot[] {
    return [...slots].sort((a, b) => sweepSlotKey(a).localeCompare(sweepSlotKey(b)));
  }

  it("found the fixture at all", () => {
    // Anti-vacuity: every assertion below iterates this array, and an empty read (a wrong path,
    // a JSON parse that silently produced `[]`) would satisfy all of them.
    assert.ok(COMMITTED_CENSUS.length > 100, `expected the fixture to carry ~210 entries, found ${COMMITTED_CENSUS.length}`);
  });

  it("classifies every slot the fixture declares", () => {
    const known = new Set(SWEEP_SLOTS.map(sweepSlotKey));
    const missing = COMMITTED_CENSUS.map(sweepSlotKey).filter((k) => !known.has(k));
    assert.deepEqual(missing, [], `these fixture slots are absent from SWEEP_SLOTS: ${missing.join(", ")}`);
  });

  it("classifies no slot the fixture does not declare", () => {
    // The other direction. A phantom entry is not harmless: it makes SWEEP_SLOTS look like it
    // covers a slot that does not exist, the same way a stale signer-matrix row would.
    const declared = new Set(COMMITTED_CENSUS.map(sweepSlotKey));
    const phantom = SWEEP_SLOTS.map(sweepSlotKey).filter((k) => !declared.has(k));
    assert.deepEqual(phantom, [], `these SWEEP_SLOTS entries name a slot the fixture does not declare: ${phantom.join(", ")}`);
  });

  it("agrees with the fixture on every slot's class and anchor — not just its existence", () => {
    // The finding this test exists to close: two slots could swap classifications, or a field
    // could be transcribed under the wrong instruction, and the two "no orphans" checks above
    // would not see it — cellKey only names the slot, not what SWEEP_SLOTS claims about it. A
    // full sorted deep-equal is the only check that catches a transcription silently disagreeing
    // with the crate while still naming every real cell.
    assert.deepEqual(
      sortedByKey(SWEEP_SLOTS),
      sortedByKey(COMMITTED_CENSUS),
      "SWEEP_SLOTS disagrees with tests/fixtures/sweep-census.json on at least one slot's class or anchor",
    );
  });
});
