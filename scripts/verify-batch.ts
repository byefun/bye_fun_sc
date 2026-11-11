// A third-party replay verifier for a settled roll batch.
//
// The claim this file exists to substantiate is that a settled batch is independently
// re-derivable: given only the batch account (its single 32-byte randomness, the per-roll
// records, and the snapshotted blank faces) plus position history, anyone outside the protocol
// can recompute every draw and confirm it resolved as recorded. Nothing here reads pool state
// live — a draw's modulus and blank prefix are the ones persisted on the roll record, which is
// what makes the result checkable after the fact.
//
// **This is written against the written formula, never against the program.** The expansion and
// the draw mapping below are a separate implementation of the specification, in a different
// language, with their own arithmetic. That is the whole value of the artifact: a verifier that
// imported the program's own expression would evaluate the same expression twice and agree with
// a wrong program just as readily as a right one. It would not discriminate — it would panic in
// the one case it exists to catch. So this file must never import the draw from the program, and
// must never be "fixed" by copying what the program does; a disagreement is a finding to
// investigate from the specification, not a diff to reconcile.
//
// Decoding is a different axis and is shared deliberately: `decodeRollBatch` reads a persisted
// byte layout, it does not recompute a draw, so reusing it narrows what this file re-derives to
// exactly the arithmetic under test.
//
// The verifier is checked against the committed expansion vector before it is pointed at any
// on-chain batch — an implementation that cannot reproduce the published cases has no standing to
// report a discrepancy against a real one.
//
// **What it does and does not establish.** It re-derives every settled draw and, given the leaf
// ordering that was live at a roll, resolves a real draw to the position it awarded. Rolls whose
// awarded card was not resolved are counted and reported separately from agreement, and the exit
// code distinguishes them, so a clean run is never mistaken for a check that did not happen.
//
// **The accrual leg is checked the same way, when a ledger is supplied.** Neither
// the domain a roll drew from nor which positions held tier membership at that roll survives on
// this program's own accounts once the batch settles, so — like the leaf ordering above — both are
// taken as input rather than reconstructed. What is checked is a set comparison, not a fee
// recomputation: every settled roll bumps the pool's equal accumulator regardless of outcome, and
// every real position live at that roll is entitled to a share of it, so "the set of positions
// whose accumulator advanced" and "the set that roll could have drawn" are the same set by
// construction — a roll where they diverge is either a position billed for a roll it was not
// exposed to, or one exposed to a roll that never credited it. The tier leg is the same comparison
// against tier membership instead of the real-leaf domain. A roll with no ledger row supplied for
// it is reported as accrual-unverified, never as agreeing.

import { createHash } from "node:crypto";
import { fetchAccountData, connectCluster } from "./_common.js";
import {
  BLANK_SLOTS,
  decodeRollBatch,
  slotIsEmpty,
  type RollBatch,
  type RollRecord,
} from "./lib/decoders.js";
import { weightOf } from "./lib/rebalance.js";
import type { Address } from "@solana/kit";

/** The sampler's retry cap. After this many rejected words the last one is reduced regardless,
 * trading a bias below 2^-78 for a bounded instruction. */
export const EXPANSION_MAX_ATTEMPTS = 4;

const TWO_POW_128 = 1n << 128n;


/** One draw's derivation: the reduced value, how many words it took, and whether the retry cap
 * was exhausted (in which case the final word was reduced despite falling in the rejection
 * region). `attempts` is 1-based, so the attempt byte on the deciding hash is `attempts - 1`. */
export interface DrawExpansion {
  r: bigint;
  attempts: number;
  fallback: boolean;
}

/**
 * The word for roll `k` at attempt `a`: sha256 over the batch randomness with the roll index and
 * attempt appended as single bytes, read as a u128 from the digest's low 16 bytes, little-endian.
 */
function wordFor(randomness: Uint8Array, k: number, a: number): bigint {
  if (randomness.length !== 32) {
    throw new Error(`randomness must be 32 bytes, got ${randomness.length}`);
  }
  if (!Number.isInteger(k) || k < 0 || k > 0xff) {
    throw new Error(`roll index must fit one byte, got ${k}`);
  }
  if (!Number.isInteger(a) || a < 0 || a > 0xff) {
    throw new Error(`attempt must fit one byte, got ${a}`);
  }

  const preimage = new Uint8Array(34);
  preimage.set(randomness, 0);
  preimage[32] = k;
  preimage[33] = a;

  const h = createHash("sha256").update(preimage).digest();

  let x = 0n;
  for (let i = 15; i >= 0; i--) {
    x = (x << 8n) | BigInt(h[i]!);
  }
  return x;
}

/**
 * Rejection-sampled reduction of the batch randomness to a draw in `[0, w)`.
 *
 * Words at or above the largest multiple of `w` below 2^128 are rejected and the attempt byte is
 * advanced, so accepted values are uniform over the range rather than biased toward its low end.
 * The rejection region is `2^128 mod w`, which for any realistic pool weight is a vanishing
 * fraction of the range — the retries exist for correctness at extreme weights, not because they
 * are expected to fire.
 */
export function expandDraw(randomness: Uint8Array, k: number, w: bigint): DrawExpansion {
  if (w <= 0n) {
    throw new Error(`draw weight must be positive, got ${w}`);
  }
  if (w >= TWO_POW_128) {
    throw new Error(`draw weight must fit a u128, got ${w}`);
  }

  const bound = TWO_POW_128 - (TWO_POW_128 % w);

  let x = 0n;
  for (let a = 0; a < EXPANSION_MAX_ATTEMPTS; a += 1) {
    x = wordFor(randomness, k, a);
    if (x < bound) {
      return { r: x % w, attempts: a + 1, fallback: false };
    }
  }

  return { r: x % w, attempts: EXPANSION_MAX_ATTEMPTS, fallback: true };
}

/** What a draw resolved to: one of the blank slots, or a real card at `residual`
 * places into the Fenwick order over live leaves. */
export type DrawTarget =
  | { kind: "blank"; faceIndex: number }
  | { kind: "real"; residual: bigint };

/**
 * Where a draw lands, in the published order: the blank slots occupy the low end of the range in
 * slot order, and everything above their combined weight is a real card.
 *
 * `blanks` is how many of each slot the pool held at the draw and `faces` the batch's snapshotted
 * face values in micro-USDC, each weighing `weightOf(face)` as it does on chain; both are
 * persisted, so this mapping is reproducible from the batch alone. As on chain, slot 3 or a
 * reserved slot with a face of 0 is skipped, and a 0 in slots 0–2 throws. The residual
 * returned for a real card is the offset into the real-leaf weight — resolving it to a specific
 * position needs the leaf ordering, which the batch does not carry.
 */
export function classifyDraw(
  r: bigint,
  blanks: readonly number[],
  faces: readonly bigint[],
): DrawTarget {
  let prefix = 0n;
  for (let face = 0; face < BLANK_SLOTS; face += 1) {
    if (slotIsEmpty(face, faces[face]!)) continue;
    prefix += BigInt(blanks[face]!) * weightOf(faces[face]!);
    if (r < prefix) {
      return { kind: "blank", faceIndex: face };
    }
  }
  return { kind: "real", residual: r - prefix };
}

/** The combined weight of every blank the pool held at a draw. */
export function blankWeight(
  blanks: readonly number[],
  faces: readonly bigint[],
): bigint {
  let total = 0n;
  for (let face = 0; face < BLANK_SLOTS; face += 1) {
    if (slotIsEmpty(face, faces[face]!)) continue;
    total += BigInt(blanks[face]!) * weightOf(faces[face]!);
  }
  return total;
}

/** One real leaf as it stood at a draw: the position's mint and the weight it carried, in the
 * order the on-chain Fenwick tree holds them. Ordering is the whole content of this type — the
 * prefix search walks it, so two leaves swapped is a different pool. */
export interface LeafSnapshot {
  mint: Address;
  weight: bigint;
}

/**
 * Which real leaf a draw's residual falls in, by prefix search over the leaves in tree order.
 *
 * Returns `null` when the residual lies past the last leaf, which means the snapshot does not
 * describe the pool the draw was reduced against — a caller must treat that as a failure to
 * verify, never as a blank.
 */
export function resolveRealDraw(
  residual: bigint,
  leaves: readonly LeafSnapshot[],
): Address | null {
  let prefix = 0n;
  for (const leaf of leaves) {
    prefix += leaf.weight;
    if (residual < prefix) return leaf.mint;
  }
  return null;
}

/** The combined weight of every real leaf in a snapshot. */
export function realWeight(leaves: readonly LeafSnapshot[]): bigint {
  return leaves.reduce((total, leaf) => total + leaf.weight, 0n);
}

/**
 * The leaf ordering that was live at each roll, keyed by roll index.
 *
 * The batch does not persist this and neither does any account after the fact: positions carry
 * only their current state, and a settled batch has already moved them. So it is supplied rather
 * than reconstructed, and each snapshot is checked against the weight its roll was actually
 * reduced against before anything is concluded from it — a snapshot that does not add up is
 * reported as a failure to verify, not quietly used.
 */
export type PositionHistory = ReadonlyMap<number, readonly LeafSnapshot[]>;

/**
 * One per-roll fee accrual for one position: `mint`, `equalDelta`, `tierDelta`. A row's mere *presence* for a (position, roll) pair is the signal
 * the accrual check reads: every real position accrues an equal share of every roll it is live for, floored
 * to `ACC_PRECISION` and possibly zero, so `equalDelta`'s value cannot distinguish a live position
 * from an absent one — only whether the row exists can. `tierDelta` carries the same distinction
 * for the tier leg on the same row: `null` when the position held no tier membership at that roll,
 * a (possibly zero) amount when it did — tier membership, unlike the equal share, is not universal
 * to every live position, so it needs its own presence signal independent of the row's.
 *
 * **`mint`, not `Position.position_id`.** `position_id` is a `u64` counter, never an NFT mint,
 * and it appears in no event a third party can observe; every other identity this file works with
 * (`LeafSnapshot.mint`, `RollRecord.selectedMint`, [[TierHistory]]) is the mint, because that is
 * what a batch's own records and this file's draw domain are keyed on. A caller holding accruals
 * keyed by `position_id` resolves each to its NFT mint first — the same step it already takes to
 * key [[PositionHistory]]'s leaves.
 */
export interface AccrualRow {
  mint: Address;
  equalDelta: bigint;
  tierDelta: bigint | null;
}

/** The ledger rows for one settled roll, keyed by roll index. A roll absent from this map is
 * reported as accrual-unverified, the same way a real-card roll with no [[PositionHistory]] entry
 * is reported as unresolved rather than as agreeing. */
export type AccrualLedger = ReadonlyMap<number, readonly AccrualRow[]>;

/**
 * Which positions carried tier membership at a roll, keyed by roll index.
 *
 * Unlike [[PositionHistory]]'s leaf order, tier accrual is a flat per-member share with no prefix
 * search behind it, so membership is a plain set rather than an ordering — two tier snapshots with
 * the same members in a different order describe the same pool, where two leaf snapshots would not.
 */
export type TierHistory = ReadonlyMap<number, ReadonlySet<Address>>;

/** One roll's verdict. `ok` is the conjunction of every check that applied to it. */
export interface RollVerdict {
  k: number;
  ok: boolean;
  /** Absent when the roll was not settled — an unsettled roll is skipped, not failed. */
  checked: boolean;
  /** The roll drew a real card and no position snapshot was supplied, so which card it awarded
   * was not verified. Reported separately from `ok`: this is an unanswered question, not a pass. */
  unresolved: boolean;
  /** Either accrual leg (equal or tier) had no domain snapshot, or no ledger row, supplied for
   * this roll, so the accrual leg was not checked here. Reported separately from `ok` for the same reason
   * `unresolved` is: an unanswered question is not a pass. */
  accrualUnresolved: boolean;
  findings: string[];
}

/**
 * Re-derives every settled roll of a batch and reports where the recomputation disagrees with
 * what was recorded.
 *
 * A roll that never settled is reported as unchecked rather than as a pass: counting it as a pass
 * would let a batch that settled nothing report a clean verification.
 */
export function verifyBatchDraws(
  batch: RollBatch,
  history: PositionHistory = new Map(),
  tierHistory: TierHistory = new Map(),
  ledger: AccrualLedger = new Map(),
): RollVerdict[] {
  if (batch.randomness.every((b) => b === 0)) {
    throw new Error(
      "batch randomness is all zero — the callback has not delivered, so there is nothing to replay",
    );
  }

  const verdicts: RollVerdict[] = [];

  for (let k = 0; k < batch.n; k += 1) {
    const roll = batch.rolls[k]!;
    if (roll.status !== "Settled") {
      verdicts.push({
        k,
        ok: true,
        checked: false,
        unresolved: false,
        accrualUnresolved: false,
        findings: [],
      });
      continue;
    }
    verdicts.push(
      verifyRoll(batch, roll, k, history.get(k), tierHistory.get(k), ledger.get(k)),
    );
  }

  return verdicts;
}

function verifyRoll(
  batch: RollBatch,
  roll: RollRecord,
  k: number,
  leaves: readonly LeafSnapshot[] | undefined,
  tierDomain: ReadonlySet<Address> | undefined,
  ledgerRow: readonly AccrualRow[] | undefined,
): RollVerdict {
  const findings: string[] = [];
  let unresolved = false;
  let accrualUnresolved = false;

  const derived = expandDraw(batch.randomness, k, roll.wAtDraw);

  if (derived.r !== roll.drawValue) {
    findings.push(
      `draw value: recomputed ${derived.r}, batch recorded ${roll.drawValue} ` +
        `(weight ${roll.wAtDraw})`,
    );
  }
  if (derived.attempts !== roll.attempts) {
    findings.push(
      `attempts: recomputed ${derived.attempts}, batch recorded ${roll.attempts}`,
    );
  }

  const target = classifyDraw(roll.drawValue, roll.blanksAtDraw, batch.sBlankFaces);

  if (target.kind === "blank") {
    if (roll.outcome !== "Blank") {
      findings.push(
        `outcome: draw ${roll.drawValue} falls in the blank prefix ` +
          `(face ${target.faceIndex}), batch recorded ${roll.outcome}`,
      );
    } else if (roll.blankFaceIdx !== target.faceIndex) {
      findings.push(
        `blank face: recomputed ${target.faceIndex}, batch recorded ${roll.blankFaceIdx}`,
      );
    }
  } else if (roll.outcome !== "RealCard") {
    findings.push(
      `outcome: draw ${roll.drawValue} falls above the blank prefix ` +
        `(residual ${target.residual}), batch recorded ${roll.outcome}`,
    );
  }

  // The position half. Without a snapshot the verifier can say which *kind* of outcome the draw
  // produced but not which card — the residual names an offset into a leaf ordering the batch
  // does not persist. A roll verified without one is reported as partially checked rather than
  // as agreeing, so a clean run never implies the awarded card was confirmed.
  if (target.kind === "real") {
    if (leaves === undefined) {
      unresolved = true;
    } else {
      const expectedReal = roll.wAtDraw - blankWeight(roll.blanksAtDraw, batch.sBlankFaces);
      if (realWeight(leaves) !== expectedReal) {
        findings.push(
          `position snapshot totals ${realWeight(leaves)} of real weight, but this roll was ` +
            `reduced against ${expectedReal} — the snapshot does not describe the pool at the draw`,
        );
      } else if (leaves.length !== roll.nRealAtDraw) {
        findings.push(
          `position snapshot holds ${leaves.length} leaves, the roll recorded ${roll.nRealAtDraw}`,
        );
      } else {
        const resolved = resolveRealDraw(target.residual, leaves);
        if (resolved === null) {
          findings.push(
            `residual ${target.residual} lies past the last leaf of the supplied snapshot`,
          );
        } else if (resolved !== roll.selectedMint) {
          findings.push(
            `selected position: the draw resolves to ${resolved}, batch recorded ` +
              `${roll.selectedMint}`,
          );
        }
      }
    }
  }

  // A real card can only be drawn out of a non-empty pool, and a draw must land inside the
  // weight it was reduced against. Both are cheap to state and would be silent otherwise.
  if (roll.drawValue >= roll.wAtDraw) {
    findings.push(
      `draw ${roll.drawValue} is not below the weight it was reduced against (${roll.wAtDraw})`,
    );
  }
  if (target.kind === "real" && roll.nRealAtDraw === 0) {
    findings.push("draw resolved to a real card, but the pool held no real leaf at that draw");
  }

  // The accrual leg. Runs on every settled roll regardless of outcome: the
  // equal accumulator bumps whether the roll drew a blank or a real card, so the domain it is
  // checked against is the roll's full real-leaf snapshot, not just its resolved target. Both
  // legs require their own domain snapshot *and* a ledger row for this roll; either missing
  // reports the roll as accrual-unresolved rather than silently skipping the comparison.
  if (leaves === undefined || ledgerRow === undefined) {
    accrualUnresolved = true;
  } else {
    const expectedReal = roll.wAtDraw - blankWeight(roll.blanksAtDraw, batch.sBlankFaces);
    if (realWeight(leaves) !== expectedReal) {
      findings.push(
        `accrual domain snapshot totals ${realWeight(leaves)} of real weight, but this roll ` +
          `was reduced against ${expectedReal} — the snapshot does not describe the pool at the ` +
          `draw`,
      );
    } else if (leaves.length !== roll.nRealAtDraw) {
      findings.push(
        `accrual domain snapshot holds ${leaves.length} leaves, the roll recorded ` +
          `${roll.nRealAtDraw}`,
      );
    } else {
      const domainMints = new Set(leaves.map((l) => l.mint));
      const ledgerMints = new Set(ledgerRow.map((r) => r.mint));

      for (const mint of ledgerMints) {
        if (!domainMints.has(mint)) {
          findings.push(
            `equal leg: ${mint} accrued from roll ${k} but was not in the domain that roll ` +
              `could have drawn from`,
          );
        }
      }
      for (const mint of domainMints) {
        if (!ledgerMints.has(mint)) {
          findings.push(
            `equal leg: ${mint} was in the domain roll ${k} could have drawn from, but the ` +
              `ledger recorded no accrual for it`,
          );
        }
      }
    }
  }

  if (tierDomain === undefined || ledgerRow === undefined) {
    accrualUnresolved = true;
  } else {
    const tierMints = new Set(
      ledgerRow.filter((r) => r.tierDelta !== null).map((r) => r.mint),
    );

    for (const mint of tierMints) {
      if (!tierDomain.has(mint)) {
        findings.push(
          `tier leg: ${mint} accrued a tier delta from roll ${k} but was not a tier member at ` +
            `that roll`,
        );
      }
    }
    for (const mint of tierDomain) {
      if (!tierMints.has(mint)) {
        findings.push(
          `tier leg: ${mint} was a tier member at roll ${k}, but the ledger recorded no tier ` +
            `accrual for it`,
        );
      }
    }
  }

  return {
    k,
    ok: findings.length === 0,
    checked: true,
    unresolved,
    accrualUnresolved,
    findings,
  };
}

/** A batch-level summary of a replay. */
export interface BatchReport {
  batch: Address;
  rolls: number;
  checked: number;
  failed: number;
  /** Rolls that drew a real card with no snapshot to resolve it against. Counted separately
   * because a run with any of these has not confirmed which cards were awarded. */
  unresolved: number;
  /** Rolls where the accrual leg (equal, tier, or both) had no domain snapshot or ledger
   * row supplied. Counted separately for the same reason `unresolved` is. */
  accrualUnresolved: number;
  verdicts: RollVerdict[];
}

export function reportFor(
  address: Address,
  batch: RollBatch,
  history: PositionHistory = new Map(),
  tierHistory: TierHistory = new Map(),
  ledger: AccrualLedger = new Map(),
): BatchReport {
  const verdicts = verifyBatchDraws(batch, history, tierHistory, ledger);
  return {
    batch: address,
    rolls: batch.n,
    checked: verdicts.filter((v) => v.checked).length,
    failed: verdicts.filter((v) => v.checked && !v.ok).length,
    unresolved: verdicts.filter((v) => v.unresolved).length,
    accrualUnresolved: verdicts.filter((v) => v.accrualUnresolved).length,
    verdicts,
  };
}

function renderReport(report: BatchReport): string {
  const lines: string[] = [
    `batch ${report.batch}`,
    `  rolls ${report.rolls} · replayed ${report.checked} · disagreed ${report.failed}` +
      ` · position unverified ${report.unresolved}` +
      ` · accrual unverified ${report.accrualUnresolved}`,
  ];

  for (const verdict of report.verdicts) {
    if (!verdict.checked) {
      lines.push(`  roll ${verdict.k}: not settled — skipped`);
      continue;
    }
    if (verdict.ok) {
      const caveats: string[] = [];
      if (verdict.unresolved) caveats.push("which card it awarded was NOT checked");
      if (verdict.accrualUnresolved) caveats.push("accrual was NOT checked");
      lines.push(
        caveats.length > 0
          ? `  roll ${verdict.k}: draw agrees — ${caveats.join("; ")}`
          : `  roll ${verdict.k}: agrees`,
      );
      continue;
    }
    lines.push(`  roll ${verdict.k}: DISAGREES`);
    for (const finding of verdict.findings) {
      lines.push(`    - ${finding}`);
    }
  }

  if (report.checked === 0) {
    lines.push("  nothing was replayed — this is not a passing verification");
  }
  if (report.unresolved > 0) {
    lines.push(
      "",
      "  No position history was supplied, so real-card draws were checked only as far as the",
      "  draw value and the blank/real split. Which position each awarded is unverified — the",
      "  leaf ordering a residual indexes into is not persisted on the batch.",
    );
  }
  if (report.accrualUnresolved > 0) {
    lines.push(
      "",
      "  No accrual ledger, domain snapshot, or tier snapshot was supplied for every roll, so",
      "  the accrual check — the set of positions whose accumulator advanced equals the set that",
      "  roll could have drawn — was not run on every roll. Neither the draw domain nor tier membership",
      "  at a past roll survives on this program's own accounts once a batch settles.",
    );
  }

  return lines.join("\n");
}

async function main(): Promise<void> {
  const address = process.argv[2] as Address | undefined;
  if (!address) {
    console.error("usage: tsx scripts/verify-batch.ts <batch-address>");
    console.error("exit 0 = fully verified · 1 = disagreement · 2 = partial (see below) or misuse");
    process.exit(64);
  }

  const connection = connectCluster();
  const data = await fetchAccountData(connection, address);
  if (data === null) {
    console.error(`no account at ${address} — nothing to replay`);
    process.exit(64);
  }

  const report = reportFor(address, decodeRollBatch(data));

  console.log(renderReport(report));

  // Three outcomes, because two would misreport one of them. Exit 0 is reserved for a batch
  // that was fully verified; a run that could not resolve which card a draw awarded exits 2, so
  // a caller treating zero as confirmation is not handed a partial check. Nothing replayed is
  // not success either: a batch whose rolls are all unsettled must not exit 0.
  if (report.failed > 0 || report.checked === 0) process.exit(1);
  process.exit(report.unresolved > 0 || report.accrualUnresolved > 0 ? 2 : 0);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  await main();
}
