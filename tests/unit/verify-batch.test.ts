// The replay verifier checked against the published expansion vector, and against itself.
//
// The vector is the artifact that gives this verifier standing: it is generated from the written
// formula rather than from the program, so reproducing it establishes that the TypeScript
// reduction below agrees with the specification — not merely with the Rust that also implements
// it. Until that holds, a disagreement between this verifier and a real batch would be
// uninterpretable, because either side could be the wrong one.
//
// Every positive assertion here is paired with a control that perturbs one input and requires the
// check to red. An all-green suite of agreements proves nothing on its own: a reduction that
// ignored its arguments and returned the recorded value would pass every case in the vector.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  EXPANSION_MAX_ATTEMPTS,
  blankWeight,
  classifyDraw,
  expandDraw,
  realWeight,
  reportFor,
  resolveRealDraw,
  verifyBatchDraws,
} from "../../scripts/verify-batch.js";
import type {
  AccrualLedger,
  AccrualRow,
  LeafSnapshot,
  PositionHistory,
  TierHistory,
} from "../../scripts/verify-batch.js";
import type { BlankCounts, BlankFaces, RollBatch, RollRecord } from "../../scripts/lib/decoders.js";
import { weightOf } from "../../scripts/lib/rebalance.js";
import type { Address } from "@solana/kit";

interface VectorCase {
  name: string;
  note: string;
  randomness: string;
  k: number;
  w: string;
  r: string;
  attempts: number;
  fallback: boolean;
}

interface Vector {
  max_attempts: number;
  cases: VectorCase[];
}

const VECTOR: Vector = JSON.parse(
  readFileSync(fileURLToPath(new URL("../fixtures/expansion-vector.json", import.meta.url)), "utf8"),
) as Vector;

function hexBytes(hex: string): Uint8Array {
  assert.equal(hex.length % 2, 0, `hex string has an odd length: ${hex}`);
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

describe("the published expansion vector", () => {
  it("carries the cases this suite believes it is checking", () => {
    // A fixture trimmed to nothing would let every assertion below pass over an empty loop.
    assert.ok(
      VECTOR.cases.length >= 8,
      `expected at least 8 published cases, found ${VECTOR.cases.length}`,
    );
    assert.equal(VECTOR.max_attempts, EXPANSION_MAX_ATTEMPTS);
  });

  it("exercises every branch of the reduction, not just the common one", () => {
    const attempts = new Set(VECTOR.cases.map((c) => c.attempts));
    assert.ok(attempts.has(1), "no case accepts on the first word");
    assert.ok(
      [...attempts].some((a) => a > 1 && a < EXPANSION_MAX_ATTEMPTS),
      "no case is rejected then accepted mid-run",
    );
    assert.ok(
      VECTOR.cases.some((c) => c.fallback),
      "no case exhausts the retry cap, so the fallback branch is unexercised",
    );
    assert.ok(
      VECTOR.cases.some((c) => BigInt(c.w) === 1n),
      "no case pins the degenerate single-unit weight",
    );
  });

  for (const c of VECTOR.cases) {
    it(`reproduces '${c.name}' exactly`, () => {
      const got = expandDraw(hexBytes(c.randomness), c.k, BigInt(c.w));
      assert.equal(got.r, BigInt(c.r), "reduced draw value");
      assert.equal(got.attempts, c.attempts, "attempt count");
      assert.equal(got.fallback, c.fallback, "fallback branch");
    });
  }

  it("discriminates — perturbing one input moves the draw", () => {
    // Without this, a reduction that returned a constant would pass every case above.
    const base = VECTOR.cases[0]!;
    const randomness = hexBytes(base.randomness);
    const w = BigInt(base.w);
    const expected = BigInt(base.r);

    const otherRoll = expandDraw(randomness, base.k + 1, w);
    assert.notEqual(otherRoll.r, expected, "a different roll index produced the same draw");

    const flipped = Uint8Array.from(randomness);
    flipped[0] ^= 0x01;
    const otherRandomness = expandDraw(flipped, base.k, w);
    assert.notEqual(otherRandomness.r, expected, "a flipped randomness bit produced the same draw");

    const otherWeight = expandDraw(randomness, base.k, w - 1n);
    assert.notEqual(otherWeight.r, expected, "a different weight produced the same draw");
  });

  it("rejects a weight outside the range the reduction is defined on", () => {
    const randomness = hexBytes(VECTOR.cases[0]!.randomness);
    assert.throws(() => expandDraw(randomness, 0, 0n), /must be positive/);
    assert.throws(() => expandDraw(randomness, 0, 1n << 128n), /must fit a u128/);
  });

  it("rejects a randomness that is not 32 bytes", () => {
    assert.throws(() => expandDraw(new Uint8Array(31), 0, 10n), /must be 32 bytes/);
  });
});

describe("the draw mapping", () => {
  // The launch faces, 1 / 2 / 5 USDC: each blank weighs C / face, as on chain.
  const blanks: BlankCounts = [3, 2, 1, 0, 0, 0, 0, 0];
  const faces: BlankFaces = [1_000_000n, 2_000_000n, 5_000_000n, 0n, 0n, 0n, 0n, 0n];
  const W1 = 1_000_000_000_000n;
  const W2 = 500_000_000_000n;
  const W5 = 200_000_000_000n;
  const wBlank = 3n * W1 + 2n * W2 + W5;

  it("weighs each face as the program's weight_of does", () => {
    assert.equal(weightOf(1_000_000n), W1);
    assert.equal(weightOf(100_000_000n), 10_000_000_000n);
    assert.equal(weightOf(45_000_000n), 22_222_222_222n);
  });

  it("throws on a zero face in slots 0-2, as the program fails the draw", () => {
    const missing: BlankFaces = [1_000_000n, 0n, 5_000_000n, 0n, 0n, 0n, 0n, 0n];
    assert.throws(() => classifyDraw(0n, [0, 0, 1, 0, 0, 0, 0, 0], missing), /must be positive/);
    assert.throws(() => blankWeight([0, 0, 0, 0, 0, 0, 0, 0], missing), /must be positive/);
  });

  it("sums the blank prefix the way the mapping reads it", () => {
    assert.equal(blankWeight(blanks, faces), wBlank);
  });

  it("places each denomination at its own half-open interval", () => {
    // Boundaries are the whole content of this mapping: an off-by-one moves a draw between a
    // blank and a real card, so each is pinned on both sides rather than at a midpoint.
    assert.deepEqual(classifyDraw(0n, blanks, faces), { kind: "blank", faceIndex: 0 });
    assert.deepEqual(classifyDraw(3n * W1 - 1n, blanks, faces), { kind: "blank", faceIndex: 0 });
    assert.deepEqual(classifyDraw(3n * W1, blanks, faces), { kind: "blank", faceIndex: 1 });
    assert.deepEqual(classifyDraw(3n * W1 + 2n * W2 - 1n, blanks, faces), { kind: "blank", faceIndex: 1 });
    assert.deepEqual(classifyDraw(3n * W1 + 2n * W2, blanks, faces), { kind: "blank", faceIndex: 2 });
    assert.deepEqual(classifyDraw(wBlank - 1n, blanks, faces), { kind: "blank", faceIndex: 2 });
    assert.deepEqual(classifyDraw(wBlank, blanks, faces), { kind: "real", residual: 0n });
    assert.deepEqual(classifyDraw(wBlank + 1n, blanks, faces), { kind: "real", residual: 1n });
  });

  it("skips a denomination the pool holds none of", () => {
    const none: BlankCounts = [0, 2, 1, 0, 0, 0, 0, 0];
    assert.deepEqual(classifyDraw(0n, none, faces), { kind: "blank", faceIndex: 1 });
    assert.deepEqual(classifyDraw(2n * W2 - 1n, none, faces), { kind: "blank", faceIndex: 1 });
    assert.deepEqual(classifyDraw(2n * W2, none, faces), { kind: "blank", faceIndex: 2 });
  });

  it("maps every draw to a real card when the pool holds no blanks", () => {
    const empty: BlankCounts = [0, 0, 0, 0, 0, 0, 0, 0];
    assert.deepEqual(classifyDraw(0n, empty, faces), { kind: "real", residual: 0n });
  });

  it("ignores a count in a slot whose face is 0", () => {
    const stale: BlankCounts = [0, 0, 0, 0, 9, 9, 9, 9];
    assert.equal(blankWeight(stale, faces), 0n);
    assert.deepEqual(classifyDraw(0n, stale, faces), { kind: "real", residual: 0n });
  });

  // A Gold batch in its low-value state: five 100 USDC blanks in slot 3 and nothing else. The
  // band is [0, 5·C/10⁸) and maps to slot 3, matching the program's `map_draw`
  // (`the_above_ticket_slot_bands_after_the_below_ticket_slots` in `common/expansion.rs`).
  it("bands a Gold batch's slot-3 blanks at C / 100 USDC each", () => {
    const goldFaces: BlankFaces = [5_000_000n, 10_000_000n, 25_000_000n, 100_000_000n, 0n, 0n, 0n, 0n];
    const goldBlanks: BlankCounts = [0, 0, 0, 5, 0, 0, 0, 0];
    const band = 5n * 10_000_000_000n;
    assert.equal(blankWeight(goldBlanks, goldFaces), band);
    assert.deepEqual(classifyDraw(0n, goldBlanks, goldFaces), { kind: "blank", faceIndex: 3 });
    assert.deepEqual(classifyDraw(band - 1n, goldBlanks, goldFaces), { kind: "blank", faceIndex: 3 });
    assert.deepEqual(classifyDraw(band, goldBlanks, goldFaces), { kind: "real", residual: 0n });
  });
});

const BATCH_ADDRESS = "11111111111111111111111111111111" as Address;
const NO_MINT = "11111111111111111111111111111111" as Address;

function emptyRoll(): RollRecord {
  return {
    status: "Pending",
    outcome: "None",
    selectedMint: NO_MINT,
    blankFaceIdx: 0,
    drawValue: 0n,
    attempts: 0,
    wAtDraw: 0n,
    nRealAtDraw: 0,
    blanksAtDraw: [0, 0, 0, 0, 0, 0, 0, 0],
    refundReason: 0,
  };
}

/** A batch whose rolls are genuinely derived from the randomness below, so the verifier has
 * something real to agree with rather than a hand-chosen pair of numbers.
 *
 * Note what this construction can and cannot show. It derives its records with the same
 * `expandDraw` the verifier re-derives them with, so no test built on it can detect an error in
 * the expansion itself — a wrong reduction would produce a wrong batch and then agree with it.
 * The external check on the arithmetic is the published vector above, and only that. What these
 * cases do establish is the layer the vector cannot reach: that the verifier notices when a
 * record and the randomness that produced it stop agreeing. Every one of them therefore tampers
 * with a settled batch and requires a specific finding, rather than asserting a clean replay. */
function settledBatch(rollCount: number): RollBatch {
  const randomness = hexBytes(
    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
  );
  const faces: BlankFaces = [1_000_000n, 2_000_000n, 5_000_000n, 0n, 0n, 0n, 0n, 0n];
  const blanks: BlankCounts = [3, 2, 1, 0, 0, 0, 0, 0];
  const w = 7_533_333_333_320n;

  const rolls = Array.from({ length: 10 }, emptyRoll);
  for (let k = 0; k < rollCount; k += 1) {
    const { r, attempts } = expandDraw(randomness, k, w);
    const target = classifyDraw(r, blanks, faces);
    rolls[k] = {
      status: "Settled",
      outcome: target.kind === "blank" ? "Blank" : "RealCard",
      selectedMint: NO_MINT,
      blankFaceIdx: target.kind === "blank" ? target.faceIndex : 0,
      drawValue: r,
      attempts,
      wAtDraw: w,
      nRealAtDraw: 40,
      blanksAtDraw: blanks,
      refundReason: 0,
    };
  }

  return {
    pool: BATCH_ADDRESS,
    roller: BATCH_ADDRESS,
    batchId: 1n,
    state: "Complete",
    n: rollCount,
    nextRoll: rollCount,
    settled: rollCount,
    refunded: 0,
    requestSlot: 100n,
    callerSeed: new Uint8Array(32),
    randomness,
    expectedW: w,
    expectedBlanks: blanks,
    sPrice: 0n,
    sTicket: 0n,
    sFee: 0n,
    sAllocBps: [0, 0, 0],
    sBlankFaces: faces,
    rolls,
    bump: 255,
  };
}

describe("replaying a batch", () => {
  it("agrees with a batch whose records were derived from its own randomness", () => {
    const report = reportFor(BATCH_ADDRESS, settledBatch(4));
    assert.equal(report.checked, 4, "every settled roll should have been replayed");
    assert.equal(report.failed, 0, report.verdicts.flatMap((v) => v.findings).join("; "));
  });

  it("catches a tampered draw value", () => {
    const batch = settledBatch(4);
    batch.rolls[2]!.drawValue += 1n;

    const report = reportFor(BATCH_ADDRESS, batch);
    assert.equal(report.failed, 1);
    const verdict = report.verdicts.find((v) => v.k === 2)!;
    assert.ok(
      verdict.findings.some((f) => f.startsWith("draw value:")),
      `expected a draw-value finding, got: ${verdict.findings.join("; ")}`,
    );
  });

  it("catches a tampered attempt count", () => {
    const batch = settledBatch(2);
    batch.rolls[0]!.attempts += 1;

    const report = reportFor(BATCH_ADDRESS, batch);
    assert.equal(report.failed, 1);
    assert.ok(report.verdicts[0]!.findings.some((f) => f.startsWith("attempts:")));
  });

  it("catches an outcome that contradicts where the draw landed", () => {
    const batch = settledBatch(2);
    const flipped = batch.rolls[0]!.outcome === "Blank" ? "RealCard" : "Blank";
    batch.rolls[0]!.outcome = flipped;

    const report = reportFor(BATCH_ADDRESS, batch);
    assert.equal(report.failed, 1);
    assert.ok(report.verdicts[0]!.findings.some((f) => f.startsWith("outcome:")));
  });

  it("catches a draw that does not lie below the weight it was reduced against", () => {
    const batch = settledBatch(1);
    batch.rolls[0]!.drawValue = batch.rolls[0]!.wAtDraw;

    const report = reportFor(BATCH_ADDRESS, batch);
    assert.equal(report.failed, 1);
    assert.ok(
      report.verdicts[0]!.findings.some((f) => f.includes("not below the weight")),
      report.verdicts[0]!.findings.join("; "),
    );
  });

  it("reports an unsettled roll as unchecked rather than as a pass", () => {
    const batch = settledBatch(4);
    batch.rolls[3]!.status = "Refunded";

    const report = reportFor(BATCH_ADDRESS, batch);
    assert.equal(report.checked, 3);
    assert.equal(report.failed, 0);
    assert.equal(report.verdicts[3]!.checked, false);
  });

  it("does not call a batch that replayed nothing a verified batch", () => {
    // The exit-code contract in the script depends on this: zero disagreements over zero
    // replayed rolls must not read as success.
    const batch = settledBatch(0);
    const report = reportFor(BATCH_ADDRESS, batch);
    assert.equal(report.checked, 0);
    assert.equal(report.failed, 0);
  });

  it("refuses a batch whose randomness has not been delivered", () => {
    const batch = settledBatch(2);
    batch.randomness = new Uint8Array(32);
    assert.throws(() => verifyBatchDraws(batch), /has not delivered/);
  });
});

describe("resolving a real draw to the position it awarded", () => {
  const leaves = [
    { mint: "Leaf1111111111111111111111111111111111111111" as Address, weight: 100n },
    { mint: "Leaf2222222222222222222222222222222222222222" as Address, weight: 250n },
    { mint: "Leaf3333333333333333333333333333333333333333" as Address, weight: 50n },
  ];

  it("walks the prefix in tree order", () => {
    assert.equal(resolveRealDraw(0n, leaves), leaves[0]!.mint);
    assert.equal(resolveRealDraw(99n, leaves), leaves[0]!.mint);
    assert.equal(resolveRealDraw(100n, leaves), leaves[1]!.mint);
    assert.equal(resolveRealDraw(349n, leaves), leaves[1]!.mint);
    assert.equal(resolveRealDraw(350n, leaves), leaves[2]!.mint);
    assert.equal(resolveRealDraw(399n, leaves), leaves[2]!.mint);
  });

  it("refuses a residual past the last leaf rather than naming one", () => {
    // Returning the final leaf here would make a snapshot that does not describe the pool look
    // like agreement, which is the failure this whole check exists to catch.
    assert.equal(resolveRealDraw(400n, leaves), null);
  });

  it("is order-sensitive — two leaves swapped is a different pool", () => {
    const swapped = [leaves[1]!, leaves[0]!, leaves[2]!];
    assert.notEqual(resolveRealDraw(0n, swapped), resolveRealDraw(0n, leaves));
  });

  it("sums the real weight the snapshot claims", () => {
    assert.equal(realWeight(leaves), 400n);
  });
});

describe("replaying a batch against position history", () => {
  /** The real-card rolls of a settled batch, with the leaf ordering each was drawn against. */
  function historyFor(batch: RollBatch): Map<number, { mint: Address; weight: bigint }[]> {
    const history = new Map<number, { mint: Address; weight: bigint }[]>();
    for (let k = 0; k < batch.n; k += 1) {
      const roll = batch.rolls[k]!;
      if (roll.outcome !== "RealCard") continue;
      const real = roll.wAtDraw - blankWeight(roll.blanksAtDraw, batch.sBlankFaces);
      const each = real / BigInt(roll.nRealAtDraw);
      const leaves = Array.from({ length: roll.nRealAtDraw }, (_, i) => ({
        mint: `Leaf${String(i).padStart(40, "0")}` as Address,
        weight: each,
      }));
      // The remainder rides on the last leaf so the snapshot totals exactly what the roll was
      // reduced against — an off-by-one here would be reported rather than silently absorbed.
      leaves[leaves.length - 1]!.weight += real - each * BigInt(roll.nRealAtDraw);
      const target = classifyDraw(roll.drawValue, roll.blanksAtDraw, batch.sBlankFaces);
      if (target.kind === "real") {
        roll.selectedMint = resolveRealDraw(target.residual, leaves)!;
      }
      history.set(k, leaves);
    }
    return history;
  }

  it("reports a real draw as unresolved when no snapshot is supplied", () => {
    const batch = settledBatch(4);
    const report = reportFor(BATCH_ADDRESS, batch);
    const realRolls = batch.rolls.slice(0, 4).filter((r) => r.outcome === "RealCard").length;

    assert.ok(realRolls > 0, "this fixture drew no real card, so the case is not exercised");
    assert.equal(report.unresolved, realRolls);
    assert.equal(report.failed, 0, "an unresolved roll must not be reported as a disagreement");
  });

  it("confirms the awarded position when the snapshot is supplied", () => {
    const batch = settledBatch(4);
    const history = historyFor(batch);
    const report = reportFor(BATCH_ADDRESS, batch, history);

    assert.equal(report.failed, 0, report.verdicts.flatMap((v) => v.findings).join("; "));
    assert.equal(report.unresolved, 0, "a supplied snapshot should leave nothing unresolved");
  });

  it("catches a batch that recorded a position the draw does not resolve to", () => {
    const batch = settledBatch(4);
    const history = historyFor(batch);
    const k = [...history.keys()][0]!;
    // Award the wrong leaf, changing nothing else: same draw value, same weight, same snapshot.
    const leaves = history.get(k)!;
    batch.rolls[k]!.selectedMint = leaves.find((l) => l.mint !== batch.rolls[k]!.selectedMint)!.mint;

    const report = reportFor(BATCH_ADDRESS, batch, history);
    assert.equal(report.failed, 1);
    assert.ok(
      report.verdicts.find((v) => v.k === k)!.findings.some((f) => f.startsWith("selected position:")),
      report.verdicts.find((v) => v.k === k)!.findings.join("; "),
    );
  });

  it("refuses a snapshot whose weight does not match the roll it claims to describe", () => {
    const batch = settledBatch(4);
    const history = historyFor(batch);
    const k = [...history.keys()][0]!;
    history.get(k)![0]!.weight += 1n;

    const report = reportFor(BATCH_ADDRESS, batch, history);
    assert.equal(report.failed, 1);
    assert.ok(
      report.verdicts.find((v) => v.k === k)!.findings.some((f) => f.includes("does not describe")),
    );
  });

  it("refuses a snapshot with the wrong number of leaves", () => {
    const batch = settledBatch(4);
    const history = historyFor(batch);
    const k = [...history.keys()][0]!;
    const leaves = history.get(k)!;
    const last = leaves.pop()!;
    leaves[leaves.length - 1]!.weight += last.weight;

    const report = reportFor(BATCH_ADDRESS, batch, history);
    assert.equal(report.failed, 1);
    assert.ok(report.verdicts.find((v) => v.k === k)!.findings.some((f) => f.includes("leaves")));
  });
});

describe("the accrual leg", () => {
  /** A 3-roll batch with no blanks, built so every draw resolves to a real card and the pool
   * *shrinks* between rolls.
   *
   * The adversarial case "activate a position while a batch is in flight" describes a state `activate_acquired_position` refuses outright —
   * `assert_weight_open` (`common/guards.rs`) rejects with `RollBatchInFlight` whenever
   * `pool.open_batches > 0`, and every real-weight-adding path runs through it. A position
   * cannot join a batch's draw domain mid-flight; the freeze is exactly what rules that out.
   * What *is* reachable within a single batch's own roll sequence is the domain **shrinking**:
   * `depart_drawn_position` removes the drawn real leaf after each settlement, so a position
   * drawn at roll `k` is live for `k` — "the drawing roll counts as the position's last active
   * roll" (`settle_roll.rs`) — and gone from every roll after it. That is the mid-batch
   * membership change this fixture drives, and the ledger must track both ends of it: credited
   * on the roll it was drawn, absent from every roll after. */
  function shrinkingBatch(): {
    batch: RollBatch;
    domain: Map<number, LeafSnapshot[]>;
  } {
    const randomness = hexBytes(
      "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
    );
    const faces: BlankFaces = [1_000_000n, 2_000_000n, 5_000_000n, 0n, 0n, 0n, 0n, 0n];
    const blanks: BlankCounts = [0, 0, 0, 0, 0, 0, 0, 0];
    const leafWeight = 1000n;
    const rollCount = 3;

    let live: LeafSnapshot[] = ["P1", "P2", "P3", "P4"].map(
      (name) => ({ mint: `Shrink${name}` as Address, weight: leafWeight }),
    );

    const rolls = Array.from({ length: 10 }, emptyRoll);
    const domain = new Map<number, LeafSnapshot[]>();

    for (let k = 0; k < rollCount; k += 1) {
      const w = leafWeight * BigInt(live.length);
      const { r, attempts } = expandDraw(randomness, k, w);
      // Blank weight is zero, so classifyDraw always lands in the real branch here.
      const target = classifyDraw(r, blanks, faces) as { kind: "real"; residual: bigint };
      const selectedMint = resolveRealDraw(target.residual, live)!;

      rolls[k] = {
        status: "Settled",
        outcome: "RealCard",
        selectedMint,
        blankFaceIdx: 0,
        drawValue: r,
        attempts,
        wAtDraw: w,
        nRealAtDraw: live.length,
        blanksAtDraw: blanks,
        refundReason: 0,
      };
      // The domain this roll could draw from includes the leaf it goes on to draw — departure
      // takes effect after settlement, not before.
      domain.set(k, live);
      live = live.filter((l) => l.mint !== selectedMint);
    }

    const batch: RollBatch = {
      pool: BATCH_ADDRESS,
      roller: BATCH_ADDRESS,
      batchId: 1n,
      state: "Complete",
      n: rollCount,
      nextRoll: rollCount,
      settled: rollCount,
      refunded: 0,
      requestSlot: 100n,
      callerSeed: new Uint8Array(32),
      randomness,
      expectedW: leafWeight * 4n,
      expectedBlanks: blanks,
      sPrice: 0n,
      sTicket: 0n,
      sFee: 0n,
      sAllocBps: [0, 0, 0],
      sBlankFaces: faces,
      rolls,
      bump: 255,
    };

    return { batch, domain };
  }

  /** A ledger row per (position, roll) for every leaf in `domain`, with `tierDelta` set for
   * whichever mints `tierMembers` names. Values are placeholders — the accrual check reads a row's
   * presence, not its amount, per [[AccrualRow]]'s contract. */
  function ledgerFor(
    domain: ReadonlyMap<number, readonly LeafSnapshot[]>,
    tierMembers: ReadonlySet<Address> = new Set(),
  ): Map<number, AccrualRow[]> {
    const ledger = new Map<number, AccrualRow[]>();
    for (const [k, leaves] of domain) {
      ledger.set(
        k,
        leaves.map((l) => ({
          mint: l.mint,
          equalDelta: 1n,
          tierDelta: tierMembers.has(l.mint) ? 1n : null,
        })),
      );
    }
    return ledger;
  }

  /** No tier in play for this scenario — an explicit empty set per roll, not an absent entry, so
   * the tier leg is fully resolved (and agrees trivially) rather than reported as unchecked. */
  function noTier(rollCount: number): TierHistory {
    const empty = new Set<Address>();
    return new Map(Array.from({ length: rollCount }, (_, k) => [k, empty] as const));
  }

  it(
    "agrees when the accrual set matches the domain each roll could have drawn from, " +
      "including a position departing mid-batch after it is drawn",
    () => {
      const { batch, domain } = shrinkingBatch();
      const ledger = ledgerFor(domain);

      const report = reportFor(BATCH_ADDRESS, batch, domain, noTier(domain.size), ledger);
      assert.equal(report.failed, 0, report.verdicts.flatMap((v) => v.findings).join("; "));
      assert.equal(report.accrualUnresolved, 0);
    },
  );

  it("catches a position billed for a roll after it departed the pool", () => {
    const { batch, domain } = shrinkingBatch();
    const ledger = ledgerFor(domain);
    // The position drawn (and departed) at roll 0 gets a row at roll 1 anyway — a zombie
    // accrual on a roll it could not have been drawn by, since it had already left.
    const departed = batch.rolls[0]!.selectedMint;
    ledger.get(1)!.push({ mint: departed, equalDelta: 1n, tierDelta: null });

    const report = reportFor(BATCH_ADDRESS, batch, domain, noTier(domain.size), ledger);
    assert.equal(report.failed, 1);
    const findings = report.verdicts.find((v) => v.k === 1)!.findings;
    assert.ok(
      findings.some((f) => f.includes("was not in the domain")),
      findings.join("; "),
    );
  });

  it("catches a position the ledger skipped on the roll it was drawn", () => {
    const { batch, domain } = shrinkingBatch();
    const ledger = ledgerFor(domain);
    // The position drawn at roll 0 was live for roll 0 — its own drawing roll counts as its
    // last active one — and belongs in that roll's accrual set even though it departs after.
    const departed = batch.rolls[0]!.selectedMint;
    const roll0 = ledger.get(0)!;
    roll0.splice(
      roll0.findIndex((r) => r.mint === departed),
      1,
    );

    const report = reportFor(BATCH_ADDRESS, batch, domain, noTier(domain.size), ledger);
    assert.equal(report.failed, 1);
    const findings = report.verdicts.find((v) => v.k === 0)!.findings;
    assert.ok(
      findings.some((f) => f.includes("recorded no accrual")),
      findings.join("; "),
    );
  });

  it("agrees on the tier leg when the ledger's tier deltas match tier membership", () => {
    const { batch, domain } = shrinkingBatch();
    // The survivors of every roll's domain are the only mints present at all three — the tier
    // membership this case exercises, independent of which mints the draws happened to select.
    const tierMembers = new Set(domain.get(2)!.map((l) => l.mint));
    const tierHistory: TierHistory = new Map([
      [0, tierMembers],
      [1, tierMembers],
      [2, tierMembers],
    ]);
    const ledger = ledgerFor(domain, tierMembers);

    const report = reportFor(BATCH_ADDRESS, batch, domain, tierHistory, ledger);
    assert.equal(report.failed, 0, report.verdicts.flatMap((v) => v.findings).join("; "));
    assert.equal(report.accrualUnresolved, 0);
  });

  it("catches a tier accrual credited to a non-member", () => {
    const { batch, domain } = shrinkingBatch();
    const tierMembers = new Set(domain.get(2)!.map((l) => l.mint));
    const tierHistory: TierHistory = new Map([
      [0, tierMembers],
      [1, tierMembers],
      [2, tierMembers],
    ]);
    const ledger = ledgerFor(domain, tierMembers);
    // The position drawn at roll 0 is in roll 0's domain but, being drawn there, is not one of
    // the survivors — crediting it a tier delta is a false membership.
    const nonMember = batch.rolls[0]!.selectedMint;
    const roll0 = ledger.get(0)!;
    roll0.find((r) => r.mint === nonMember)!.tierDelta = 1n;

    const report = reportFor(BATCH_ADDRESS, batch, domain, tierHistory, ledger);
    assert.equal(report.failed, 1);
    const findings = report.verdicts.find((v) => v.k === 0)!.findings;
    assert.ok(
      findings.some((f) => f.includes("was not a tier member")),
      findings.join("; "),
    );
  });

  it("catches a tier member the ledger recorded no tier accrual for", () => {
    const { batch, domain } = shrinkingBatch();
    const tierMembers = new Set(domain.get(2)!.map((l) => l.mint));
    const tierHistory: TierHistory = new Map([
      [0, tierMembers],
      [1, tierMembers],
      [2, tierMembers],
    ]);
    const ledger = ledgerFor(domain, tierMembers);
    const member = [...tierMembers][0]!;
    const roll0 = ledger.get(0)!;
    roll0.find((r) => r.mint === member)!.tierDelta = null;

    const report = reportFor(BATCH_ADDRESS, batch, domain, tierHistory, ledger);
    assert.equal(report.failed, 1);
    const findings = report.verdicts.find((v) => v.k === 0)!.findings;
    assert.ok(
      findings.some((f) => f.includes("recorded no tier accrual")),
      findings.join("; "),
    );
  });

  it("refuses an accrual domain snapshot whose weight does not match the roll it claims to describe", () => {
    const { batch, domain } = shrinkingBatch();
    const tampered = new Map(domain);
    const leaves0 = domain.get(0)!.map((l) => ({ ...l }));
    leaves0[0]!.weight += 1n;
    tampered.set(0, leaves0);
    const ledger = ledgerFor(domain);

    const report = reportFor(BATCH_ADDRESS, batch, tampered, new Map(), ledger);
    assert.equal(report.failed, 1);
    const findings = report.verdicts.find((v) => v.k === 0)!.findings;
    assert.ok(
      findings.some((f) => f.includes("does not describe the pool")),
      findings.join("; "),
    );
  });

  it("reports every settled roll as accrual-unresolved when no ledger is supplied", () => {
    const report = reportFor(BATCH_ADDRESS, settledBatch(4));
    assert.equal(report.accrualUnresolved, report.checked);
    assert.equal(report.failed, 0, "an unresolved accrual leg must not be reported as a disagreement");
  });
});
