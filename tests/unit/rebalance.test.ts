import assert from "node:assert/strict";
import { describe, it } from "node:test";

import type { BlankFaces } from "../../scripts/lib/decoders.js";
import { assertBlanksCurrent, harmonicMeanMetrics, rebalanceTargets } from "../../scripts/lib/rebalance.js";

const FACES: BlankFaces = [1_000_000n, 2_000_000n, 5_000_000n, 0n, 0n, 0n, 0n, 0n];
const GOLD_FACES: BlankFaces = [5_000_000n, 10_000_000n, 25_000_000n, 100_000_000n, 0n, 0n, 0n, 0n];
const GOLD_TARGET = 45_000_000n;
const TARGET = 9_000_000n;
const WEIGHT_C = 1_000_000_000_000_000_000n;

describe("deployment blank-target verifier", () => {
  it("uses the protective all-zero target when no real positions are active", () => {
    assert.deepEqual(rebalanceTargets(0, 999_999n, FACES, TARGET), [0, 0, 0, 0, 0, 0, 0, 0]);
  });

  it("matches the controller's default-face target and rejects stale stored blanks", () => {
    const wReal = 20n * (WEIGHT_C / 12_000_000n);
    const targets = rebalanceTargets(20, wReal, FACES, TARGET);
    assert.deepEqual(targets, [0, 1, 2, 0, 0, 0, 0, 0]);
    assert.throws(
      () => assertBlanksCurrent({ nReal: 20, wReal, blankFaces: FACES, ticketTarget: TARGET, blanks: [0, 0, 0, 0, 0, 0, 0, 0] }),
      /stale blanks/,
    );
    assert.doesNotThrow(() =>
      assertBlanksCurrent({ nReal: 20, wReal, blankFaces: FACES, ticketTarget: TARGET, blanks: targets }),
    );
  });

  it("keeps the controller's lexicographic tie-break order", () => {
    assert.deepEqual(
      rebalanceTargets(5, 263_888_888_888n, [2_000_000n, 2_000_000n, 2_000_000n, 0n, 0n, 0n, 0n, 0n], TARGET),
      [0, 1, 0, 0, 0, 0, 0, 0],
    );
  });

  it("reports the exact harmonic mean and target error for the stored blank set", () => {
    const metrics = harmonicMeanMetrics({
      nReal: 1,
      wReal: WEIGHT_C / 10_000_000n,
      blankFaces: FACES,
      blanks: [0, 1, 0, 0, 0, 0, 0, 0],
      ticketTarget: TARGET,
    });
    assert.equal(metrics.totalCount, 2n);
    assert.equal(metrics.weightedCount, WEIGHT_C / 10_000_000n + WEIGHT_C / 2_000_000n);
    assert.equal(metrics.harmonicMean, WEIGHT_C * metrics.totalCount / metrics.weightedCount);
    assert.equal(metrics.targetErrorPpm, 629_629n);
  });

  // The same fixtures as the program's own controller tests, so the two agree by construction.
  it("sets Gold's below-ticket blanks only while its cards average above its ticket", () => {
    const wReal = 10n * (WEIGHT_C / 100_000_000n);
    assert.deepEqual(rebalanceTargets(10, wReal, GOLD_FACES, GOLD_TARGET), [0, 0, 7, 0, 0, 0, 0, 0]);
  });

  it("sets no blanks when Gold's cards average exactly its ticket", () => {
    const wReal = 5n * (WEIGHT_C / 50_000_000n) + 4n * (WEIGHT_C / 40_000_000n);
    assert.equal(GOLD_TARGET * wReal, 9n * WEIGHT_C);
    assert.deepEqual(rebalanceTargets(9, wReal, GOLD_FACES, GOLD_TARGET), [0, 0, 0, 0, 0, 0, 0, 0]);
  });

  it("sets only the 100 USDC blank below Gold's ticket, the whole count nearest the ticket", () => {
    const wReal = 7n * (WEIGHT_C / 30_000_000n) + 3n * (WEIGHT_C / 500_000_000n);
    const targets = rebalanceTargets(10, wReal, GOLD_FACES, GOLD_TARGET);
    assert.deepEqual(targets, [0, 0, 0, 1, 0, 0, 0, 0]);
    assert.doesNotThrow(() =>
      assertBlanksCurrent({ nReal: 10, wReal, blankFaces: GOLD_FACES, ticketTarget: GOLD_TARGET, blanks: targets }),
    );
    const metrics = harmonicMeanMetrics({
      nReal: 10, wReal, blankFaces: GOLD_FACES, blanks: targets, ticketTarget: GOLD_TARGET,
    });
    assert.equal(metrics.totalCount, 11n);
  });

  it("breaks an exact above-ticket tie towards fewer blanks", () => {
    const faces: BlankFaces = [1_000_000n, 2_000_000n, 5_000_000n, 50_000_000n, 0n, 0n, 0n, 0n];
    const card = WEIGHT_C / 25_000_000n;
    assert.deepEqual(rebalanceTargets(1, card, faces, 30_000_000n), [0, 0, 0, 0, 0, 0, 0, 0]);
    assert.deepEqual(rebalanceTargets(2, 2n * card, faces, 30_000_000n), [0, 0, 0, 1, 0, 0, 0, 0]);
  });

  it("never sets the above-ticket slot for a Bronze pool whose cards fall below its ticket", () => {
    const wReal = WEIGHT_C / 1_000_000n + 3n * (WEIGHT_C / 12_000_000n);
    assert.deepEqual(rebalanceTargets(4, wReal, FACES, TARGET), [0, 0, 0, 0, 0, 0, 0, 0]);
  });

  it("saturates an above-ticket count past u32 rather than throwing, as the program does", () => {
    const faces: BlankFaces = [1_000_000n, 2_000_000n, 5_000_000n, 100_000_000n, 0n, 0n, 0n, 0n];
    assert.deepEqual(rebalanceTargets(1, WEIGHT_C, faces, 99_000_000n), [0, 0, 0, 0xffff_ffff, 0, 0, 0, 0]);
  });

  it("harmonicMeanMetrics throws on a 0 face in slots 0-2 and skips an empty slot 3", () => {
    const missing: BlankFaces = [1_000_000n, 0n, 5_000_000n, 0n, 0n, 0n, 0n, 0n];
    assert.throws(
      () => harmonicMeanMetrics({ nReal: 1, wReal: WEIGHT_C / 10_000_000n, blankFaces: missing, blanks: [0, 0, 0, 0, 0, 0, 0, 0], ticketTarget: TARGET }),
      /must be positive/,
    );
    const metrics = harmonicMeanMetrics({
      nReal: 1, wReal: WEIGHT_C / 10_000_000n, blankFaces: FACES, blanks: [0, 0, 0, 9, 9, 0, 0, 0], ticketTarget: TARGET,
    });
    assert.equal(metrics.totalCount, 1n);
  });
});
