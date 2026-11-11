// The wrong-signer matrix swept against the program's own declared signers.
//
// The matrix table is hand-written, so on its own it proves nothing about extent: a table that
// forgot an instruction would be as green as a complete one. This file removes that freedom by
// enumerating the signers out of the IDL — which is generated from the Rust, and is pinned
// against it by the accounts differential — and requiring the table to match that enumeration in
// both directions. An instruction added without a matrix cell reds here, and so does a table
// entry naming a signer the program no longer declares.
//
// The sweep is over the class, not over a list. Nothing below names how many instructions the
// crate has except as a control that the walk actually happened.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { loadIdl } from "./idl-oracle-support.js";
import {
  DEFERRED_CELLS,
  EXECUTED_ROWS,
  ROLL_PIPELINE_INSTRUCTIONS,
  SIGNER_CELLS,
  cellKey,
  type SignerCell,
} from "../helpers/authority-matrix.js";

/** Every (instruction, signer) pair the program declares, read out of the IDL. */
function declaredCells(): { instruction: string; signer: string }[] {
  return loadIdl().instructions.flatMap((ix) =>
    ix.accounts
      .filter((a) => "signer" in a && a.signer === true)
      .map((a) => ({ instruction: ix.name, signer: a.name })),
  );
}

describe("the signer enumeration this census sweeps", () => {
  it("found the program's instructions at all", () => {
    // Anti-vacuity: every assertion below iterates this walk, and an empty walk would satisfy
    // all of them. The floor is deliberately loose — this guards a broken read, not a count.
    const idl = loadIdl();
    assert.ok(
      idl.instructions.length > 25,
      `expected the IDL to carry the program's instructions, found ${idl.instructions.length}`,
    );
    assert.ok(declaredCells().length > 25, "no signer accounts were found in the IDL");
  });

  it("finds a signer for every instruction — none is unsigned", () => {
    const withoutSigner = loadIdl()
      .instructions.filter(
        (ix) => !ix.accounts.some((a) => "signer" in a && a.signer === true),
      )
      .map((ix) => ix.name);
    assert.deepEqual(
      withoutSigner,
      [],
      `these instructions declare no signer at all, so no authority row can attack them: ${withoutSigner.join(", ")}`,
    );
  });
});

describe("the wrong-signer matrix covers the class", () => {
  it("classifies every signer the program declares", () => {
    const classified = new Set(SIGNER_CELLS.map(cellKey));
    const missing = declaredCells()
      .filter((c) => !classified.has(`${c.instruction}.${c.signer}`))
      .map((c) => `${c.instruction}.${c.signer}`);

    assert.deepEqual(
      missing,
      [],
      `these signers carry no authority classification, so a new instruction could join the ` +
        `program without ever entering the matrix: ${missing.join(", ")}`,
    );
  });

  it("classifies no signer the program does not declare", () => {
    // The other direction. A stale entry is not harmless: it makes the table look complete while
    // the cell it describes no longer exists, and it is how a renamed signer goes unnoticed.
    const declared = new Set(declaredCells().map((c) => `${c.instruction}.${c.signer}`));
    const phantom = SIGNER_CELLS.map(cellKey).filter((k) => !declared.has(k));

    assert.deepEqual(
      phantom,
      [],
      `these table entries name a signer the program does not declare: ${phantom.join(", ")}`,
    );
  });

  it("accounts for every cell exactly once — executed or deferred, never both, never neither", () => {
    const executed = new Set(EXECUTED_ROWS.map(cellKey));
    const deferred = new Set(DEFERRED_CELLS.map(cellKey));

    const both = [...executed].filter((k) => deferred.has(k));
    assert.deepEqual(both, [], `cells both executed and deferred: ${both.join(", ")}`);

    const neither = SIGNER_CELLS.map(cellKey).filter(
      (k) => !executed.has(k) && !deferred.has(k),
    );
    assert.deepEqual(
      neither,
      [],
      `these cells are neither executed nor declared as debt — silently uncovered: ${neither.join(", ")}`,
    );
  });

  it("executes nothing that is not a real cell", () => {
    // Without this, deleting a cell from the table orphans its executed row instead of reding:
    // the row keeps running against an instruction the matrix no longer claims to classify, and
    // every coverage assertion here iterates the table, so none of them would see it.
    const known = new Set(SIGNER_CELLS.map(cellKey));
    const orphaned = EXECUTED_ROWS.map(cellKey).filter((k) => !known.has(k));
    assert.deepEqual(
      orphaned,
      [],
      `these executed rows name a cell the matrix does not classify: ${orphaned.join(", ")}`,
    );
  });

  it("agrees with itself about each executed cell's authority", () => {
    // A row attacks a class. If the row and the table disagree about which class the cell
    // carries, the row's expectation was chosen against the wrong one.
    const byKey = new Map(SIGNER_CELLS.map((c) => [cellKey(c), c.authority]));
    for (const row of EXECUTED_ROWS) {
      const declared = byKey.get(cellKey(row));
      if (declared === undefined) continue;
      assert.equal(
        row.authority,
        declared,
        `${cellKey(row)}: the row attacks ${row.authority}, the table classifies it ${declared}`,
      );
    }
  });

  it("defers nothing that is not a real cell", () => {
    const known = new Set(SIGNER_CELLS.map(cellKey));
    const bogus = DEFERRED_CELLS.map(cellKey).filter((k) => !known.has(k));
    assert.deepEqual(
      bogus,
      [],
      `a cell cannot be parked in the debt list by naming something that does not exist: ${bogus.join(", ")}`,
    );
  });

  it("gives every deferred cell a reason a reader can weigh", () => {
    for (const cell of DEFERRED_CELLS) {
      assert.ok(
        cell.reason.trim().length > 20,
        `${cellKey(cell)} is deferred without a usable reason`,
      );
    }
  });
});

describe("the roll-pipeline and buyback instructions are inside the matrix's extent", () => {
  it("names nine instructions", () => {
    assert.equal(ROLL_PIPELINE_INSTRUCTIONS.length, 9);
  });

  it("executes a row for every signer of every one of them", () => {
    // The census is what stops any of these nine slipping out of the matrix, and what refuses a
    // tenth arriving unattacked.
    const executed = new Set(EXECUTED_ROWS.map(cellKey));
    const uncovered = SIGNER_CELLS.filter(
      (c) => ROLL_PIPELINE_INSTRUCTIONS.includes(c.instruction) && !executed.has(cellKey(c)),
    ).map(cellKey);

    assert.deepEqual(
      uncovered,
      [],
      `roll-pipeline signer cells with no executed row: ${uncovered.join(", ")}`,
    );
  });

  it("defers none of their cells at all", () => {
    // The stronger statement, and the one that stops coverage being walked back: not
    // "every cell is accounted for" — which a wholly deferred matrix would satisfy — but that
    // none of these cells is carried as debt.
    const deferred = SIGNER_CELLS.filter(
      (c) => ROLL_PIPELINE_INSTRUCTIONS.includes(c.instruction),
    ).filter((c) => DEFERRED_CELLS.some((d) => cellKey(d) === cellKey(c)));

    assert.deepEqual(
      deferred.map(cellKey),
      [],
      "a roll-pipeline cell has been moved back into the deferred list",
    );
  });

  it("names the suite that drives each row, and drives them from more than one place", () => {
    // Rows live where their fixture lives. The count per suite is pinned so a row cannot be
    // quietly dropped from a file the census does not itself read.
    const bySuite = new Map<string, number>();
    for (const row of EXECUTED_ROWS) {
      assert.ok(row.suite, `${cellKey(row)} names no suite`);
      bySuite.set(row.suite, (bySuite.get(row.suite) ?? 0) + 1);
    }

    assert.deepEqual(
      Object.fromEntries([...bySuite].sort()),
      {
        "collection-admission": 3,
        "consolidate-treasuries": 1,
        "core-standard-exit": 7,
        escrow: 5,
        "instant-sell": 4,
        "legacy-standard-deposit": 1,
        "legacy-standard-exit": 2,
        "migrate-pool": 1,
        "pre-tge-config": 3,
        "t02-authority": 7,
        "treasury-activation": 1,
        "treasury-ceiling-sweep": 2,
        "unexecuted-encoders": 4,
        "wrong-signer-matrix": 7,
      },
      "the distribution of executed rows across suites has changed",
    );
  });

  it("attacks each of their cells with a party that is not the authorised one", () => {
    for (const row of EXECUTED_ROWS) {
      assert.notEqual(
        row.imposter,
        row.signer,
        `${cellKey(row)} is 'attacked' by its own authority — the row proves nothing`,
      );
    }
  });

  it("expects a success only where the program is permissionless by design", () => {
    // An inverted expectation is the one mistake this table can make that still looks green in
    // the integration suite: a row expecting success against a real authority would pass only
    // while the check was broken.
    for (const row of EXECUTED_ROWS) {
      if (row.expectation.kind === "succeeds") {
        assert.equal(
          row.authority,
          "AbsenceOfCheck",
          `${cellKey(row)} expects a stranger's call to succeed, but its signer carries ` +
            `${row.authority} — a row that would pass only while the check was missing`,
        );
      } else {
        assert.notEqual(
          row.authority,
          "AbsenceOfCheck",
          `${cellKey(row)} expects a rejection from a deliberately permissionless signer`,
        );
      }
    }
  });

  it("pins a specific error code on every rejection", () => {
    for (const row of EXECUTED_ROWS) {
      if (row.expectation.kind !== "rejects") continue;
      assert.ok(
        Number.isInteger(row.expectation.code) && row.expectation.code > 0,
        `${cellKey(row)} rejects without naming which error — any failure would satisfy it`,
      );
    }
  });
});

describe("the matrix's own shape", () => {
  it("lists each cell once", () => {
    const keys = SIGNER_CELLS.map(cellKey);
    assert.equal(new Set(keys).size, keys.length, "a signer cell is listed twice");
  });

  it("gives the two-signer instructions two distinct authorities", () => {
    // The buyback pair is the reason the matrix is keyed by cell rather than by instruction:
    // classifying the handler once would collapse two different releases into one row.
    for (const instruction of ["instant_sell", "instant_sell_core"]) {
      const cells = SIGNER_CELLS.filter((c: SignerCell) => c.instruction === instruction);
      assert.equal(cells.length, 2, `${instruction} should carry two signer cells`);
      assert.equal(
        new Set(cells.map((c) => c.authority)).size,
        2,
        `${instruction}'s two signers should carry two different authorities`,
      );
    }
  });
});
