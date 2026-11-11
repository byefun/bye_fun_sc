// Off-chain mirror of the published blank-target controller in
// programs/bye_machine/src/common/rebalance.rs. Deployment tooling uses this only to refuse an
// activation whose stored blank set is stale; it never writes pool state.

import { ABOVE_TICKET_SLOT, slotIsEmpty, type BlankCounts, type BlankFaces } from "./decoders.js";

/** `common/constants.rs`'s `WEIGHT_C`: a value `V` weighs `WEIGHT_C / V`, floored. */
export const WEIGHT_C = 1_000_000_000_000_000_000n;
const A1_MAX = 2n;
const A5_MAX = 8n;

export type BlankTargets = BlankCounts;

export type HarmonicMeanMetrics = {
  totalCount: bigint;
  weightedCount: bigint;
  /** `floor(WEIGHT_C * totalCount / weightedCount)`, or null for an empty pool. */
  harmonicMean: bigint | null;
  /** `|harmonicMean - ticketTarget| / ticketTarget`, in exact integer parts per million. */
  targetErrorPpm: bigint | null;
};

/** The program's `weight_of`: `WEIGHT_C / value`, floored, refusing a value of 0 as it does. */
export function weightOf(value: bigint): bigint {
  if (value <= 0n) throw new Error(`blank face must be positive, got ${value}`);
  const weight = WEIGHT_C / value;
  if (weight === 0n) throw new Error(`blank face has zero weight, got ${value}`);
  return weight;
}

function absDiff(a: bigint, b: bigint): bigint {
  return a >= b ? a - b : b - a;
}

/** Exact pool harmonic-mean metrics for the current real positions plus stored blanks. */
export function harmonicMeanMetrics(pool: {
  nReal: number;
  wReal: bigint;
  blankFaces: readonly bigint[];
  blanks: readonly number[];
  ticketTarget: bigint;
}): HarmonicMeanMetrics {
  let totalCount = BigInt(pool.nReal);
  let weightedCount = pool.wReal;
  for (let index = 0; index < pool.blankFaces.length; index += 1) {
    const count = pool.blanks[index];
    if (count === undefined || !Number.isInteger(count) || count < 0) {
      throw new Error(`blank count must be a non-negative integer, got ${count}`);
    }
    if (slotIsEmpty(index, pool.blankFaces[index]!)) continue;
    totalCount += BigInt(count);
    weightedCount += BigInt(count) * weightOf(pool.blankFaces[index]!);
  }
  if (weightedCount === 0n) {
    if (totalCount !== 0n) throw new Error("non-empty pool has zero harmonic-mean denominator");
    return { totalCount, weightedCount, harmonicMean: null, targetErrorPpm: null };
  }
  const numerator = WEIGHT_C * totalCount;
  const harmonicMean = numerator / weightedCount;
  const targetErrorPpm = absDiff(numerator, pool.ticketTarget * weightedCount) * 1_000_000n /
    (pool.ticketTarget * weightedCount);
  return { totalCount, weightedCount, harmonicMean, targetErrorPpm };
}

function isBefore(a: readonly bigint[], b: readonly bigint[]): boolean {
  for (let index = 0; index < a.length; index += 1) {
    if (a[index] !== b[index]) return a[index]! < b[index]!;
  }
  return false;
}

/**
 * Returns the deterministic eight-slot target for the pool's current real-position aggregate.
 * With `H = C·nReal / wReal`: above the ticket, the below-ticket search sets slots 0–2; at or
 * below it, a pool with an above-ticket face in slot 3 sets only that slot, to the whole count
 * minimising the same score (ties to fewer); a pool without one sets nothing.
 */
export function rebalanceTargets(
  nReal: number,
  wReal: bigint,
  blankFaces: BlankFaces,
  ticketTarget: bigint,
): BlankTargets {
  if (!Number.isInteger(nReal) || nReal < 0 || nReal > 0xffff_ffff) {
    throw new Error(`nReal must be a u32, got ${nReal}`);
  }
  const counts: BlankTargets = [0, 0, 0, 0, 0, 0, 0, 0];
  if (nReal === 0) return counts;

  const n = BigInt(nReal);
  const aboveFace = blankFaces[ABOVE_TICKET_SLOT];
  if (!slotIsEmpty(ABOVE_TICKET_SLOT, aboveFace) && n * WEIGHT_C <= ticketTarget * wReal) {
    counts[ABOVE_TICKET_SLOT] = aboveTicketCount(n, wReal, aboveFace, ticketTarget);
    return counts;
  }

  const [b1, b2, b5] = belowTicketTargets(n, wReal, [blankFaces[0], blankFaces[1], blankFaces[2]], ticketTarget);
  counts[0] = b1;
  counts[1] = b2;
  counts[2] = b5;
  return counts;
}

function aboveTicketCount(n: bigint, wReal: bigint, face: bigint, ticketTarget: bigint): number {
  if (face <= ticketTarget) throw new Error("above-ticket blank face is not above the ticket target");
  const d = ticketTarget * wReal - n * WEIGHT_C;
  const step = WEIGHT_C - ticketTarget * weightOf(face);
  const floor = d / step;
  const remainder = d % step;
  const count = step - remainder < remainder ? floor + 1n : floor;
  // Saturates as on chain, where a failing count would freeze every exit through the pool.
  return Number(count > 0xffff_ffffn ? 0xffff_ffffn : count);
}

function belowTicketTargets(
  n: bigint,
  wReal: bigint,
  blankFaces: readonly [bigint, bigint, bigint],
  ticketTarget: bigint,
): [number, number, number] {
  const weights = blankFaces.map(weightOf) as [bigint, bigint, bigint];
  const num = n * WEIGHT_C > ticketTarget * wReal ? n * WEIGHT_C - ticketTarget * wReal : 0n;
  const dens = weights.map((weight) => {
    const den = ticketTarget * weight - WEIGHT_C;
    if (den <= 0n) throw new Error("blank face is not below the ticket target");
    return den;
  }) as [bigint, bigint, bigint];

  let best: { key: [bigint, bigint, bigint, bigint]; targets: [number, number, number] } | undefined;
  for (let b1 = 0n; b1 <= A1_MAX; b1 += 1n) {
    for (let b5 = 0n; b5 <= A5_MAX; b5 += 1n) {
      const spent = dens[0] * b1 + dens[2] * b5;
      const b2Star = (num > spent ? num - spent : 0n) / dens[1];
      for (const delta of [0n, 1n]) {
        const b2 = b2Star + delta;
        if (b2 > 0xffff_ffffn) throw new Error("blank target exceeds u32");
        const weighted = wReal + weights[0] * b1 + weights[1] * b2 + weights[2] * b5;
        const total = n + b1 + b2 + b5;
        const key: [bigint, bigint, bigint, bigint] = [
          absDiff(ticketTarget * weighted, WEIGHT_C * total), b1 + b2 + b5, b1, b5,
        ];
        const candidate = { key, targets: [Number(b1), Number(b2), Number(b5)] as [number, number, number] };
        if (!best || isBefore(key, best.key)) {
          best = candidate;
        }
      }
    }
  }
  return best!.targets;
}

/** Throws instead of allowing a resumed roll to consume stale blank targets. */
export function assertBlanksCurrent(pool: {
  nReal: number;
  wReal: bigint;
  blankFaces: BlankFaces;
  ticketTarget: bigint;
  blanks: readonly number[];
}): void {
  const expected = rebalanceTargets(pool.nReal, pool.wReal, pool.blankFaces, pool.ticketTarget);
  if (expected.some((value, index) => value !== pool.blanks[index])) {
    throw new Error(
      `refusing to activate rolls: stale blanks [${pool.blanks}] (expected [${expected}]); ` +
      "perform a normal position lifecycle action that rebalances blanks first",
    );
  }
}
