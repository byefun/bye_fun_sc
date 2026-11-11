// Settling a roll means naming the position it drew, and the program will not tell you which one
// that is.
//
// The Fenwick tree stores weights only: there is no on-chain slot → position map, so the drawn
// leaf names no account. `settle_roll` takes the position the caller *predicts* and verifies it
// against its own range search, rejecting a wrong guess with `DrawnPositionMismatch` (6214). An
// off-chain caller that has not reimplemented the expansion therefore has one honest move — try
// each live candidate until one is accepted — and that is what this helper does.
//
// It is deliberately NOT an independent reimplementation of the draw. A helper that recomputed
// which position the randomness selects would be the same expression the program evaluates,
// written twice, and a test built on it would agree with a wrong program. Retrying is slower and
// proves the on-chain search is what decided.
//
// An independent reconstruction of the draw is a worthwhile artifact, but it is a verifier, not a
// test helper: its whole value is being written against the specification rather than against this
// program, and nothing here should be mistaken for one.

import type { Address } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import type { Connection, TransactionSigner } from "./env.js";
import type { MintedAsset } from "./fixtures.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { settleRoll } from "../../scripts/lib/roll.js";
import { decodeProtocolConfig } from "../../scripts/lib/decoders.js";
import { deriveAssociatedTokenAddress } from "../../scripts/lib/anchor.js";

/** A roll that settled no position: the pool held no real card the draw could select. */
export const NO_POSITION = -1;

/** `DrawnPositionMismatch` — the predicted position is not the one the range search selected. */
export const DRAWN_POSITION_MISMATCH = 6214;

/**
 * Anchor's own code for a short account list. A real-card prediction supplies sixteen accounts a
 * no-position prediction omits, so a wrong guess in that direction fails here rather than at 6214.
 */
export const ACCOUNT_NOT_ENOUGH_KEYS = 3005;

/** Everything a settlement needs that does not change between rolls of the same suite. */
export type RollSuiteContext = {
  connection: Connection;
  operator: TransactionSigner;
  pool: Address;
  protocolConfig: Address;
  weightIndex: Address;
  /** The batch's roller — the card's destination, pinned on-chain against `batch.roller`. */
  roller: Address;
  rollerUsdc: Address;
  /** Every asset deposited into this pool, indexed as the `live` list below indexes them. */
  assets: MintedAsset[];
};

/**
 * Settles roll `rollIndex` of `batchId`, returning which candidate the draw selected, or
 * `NO_POSITION` when it settled no position.
 *
 * `live` is the subset of `ctx.assets` still holding an open position — a caller settling a batch
 * that closes positions as it goes must narrow it, or the search will keep offering a position the
 * pool no longer has. It defaults to every asset.
 *
 * Throws if no candidate is accepted, carrying the mismatch count: that is a real failure (a guard
 * refused for some other reason), not an exhausted search.
 */
export async function settleDrawnRoll(
  ctx: RollSuiteContext,
  batchId: bigint,
  rollIndex: number,
  live: number[] = ctx.assets.map((_, i) => i),
): Promise<number> {
  const { connection, operator, pool, protocolConfig, weightIndex, roller, rollerUsdc, assets } =
    ctx;
  const cfg = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);

  // `null` first: a roll over an emptied pool draws nothing, and that prediction is cheapest to
  // refute — it is the only one whose account list the program can reject on length alone.
  const candidates: (number | null)[] = [null, ...live];
  let mismatches = 0;

  for (const candidate of candidates) {
    const asset = candidate === null ? null : assets[candidate]!;
    const rollerToken =
      asset === null ? null : await getTokenAccountAddress(roller, asset.mint, false);
    try {
      await sendIxs(connection, operator, [
        await settleRoll({
          operator: operator.address,
          pool,
          batchId,
          rollIndex,
          rollerUsdc,
          protocolRevenue: cfg.protocolRevenue,
          treasury05: await deriveAssociatedTokenAddress(cfg.t05Authority, cfg.usdcMint),
          weightIndex,
          roller,
          nftMint: asset?.mint ?? null,
          depositor: asset === null ? null : roller,
          settlementUsdc: asset === null ? null : rollerUsdc,
          rollerToken,
          metadata: asset?.metadata ?? null,
          masterEdition: asset?.masterEdition ?? null,
          positionTokenRecord: null,
          rollerTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
          successor: null,
          blank: null,
        }),
      ]);
      return candidate ?? NO_POSITION;
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      const missed =
        message.includes(String(DRAWN_POSITION_MISMATCH)) ||
        message.includes(String(ACCOUNT_NOT_ENOUGH_KEYS));
      if (!missed) throw e;
      mismatches += 1;
    }
  }
  throw new Error(`no candidate settled roll ${rollIndex} (${mismatches} mismatches)`);
}
