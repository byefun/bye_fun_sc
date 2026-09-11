// Instruction encoder for update_tier — the smallest surface in the T-39..T-43 batch: 5
// accounts, no args. Account order and shape are derived mechanically from
// target/idl/bye_machine.json, cross-checked against UpdateTier's #[derive(Accounts)] struct in
// programs/bye_machine/src/instructions/tier/update_tier.rs. With no args, the instruction's
// entire wire correctness *is* the account list and its order.
//
// Pure and synchronous with respect to I/O, per entry criterion 5: `displaced` is a required,
// non-defaulted `Address | null` — the *resolved result* of scripts/lib/tier-resolve.ts's
// `resolveDisplaced`, never computed here. This file does no I/O and derives no rank ordering of
// its own.

import type { Address, Instruction } from "@solana/kit";
import type { RankKey } from "./tier-resolve.js";
import {
  buildAnchorIx,
  derivePda,
  readonly,
  seedFromPubkey,
  signer,
  writable,
} from "./anchor.js";

// ── Seeds (bye_machine-specific — anchor.ts stays program-agnostic) ─────────────────────────
// Verbatim against programs/bye_machine/src/common/seeds.rs.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const TOP_TIER_SEED = "tier";
const POSITION_SEED = "position";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

// ── update_tier ──────────────────────────────────────────────────────────────────────────────

/**
 * The only `RankKey` shape valid as input to `resolveDisplaced` when resolving `update_tier`'s
 * `displaced` — type-only, so this costs nothing at runtime (the import above is `import type`;
 * tier.ts stays free of tier-resolve.ts's I/O).
 *
 * F-1: `update_tier`'s candidate `TierEntry`
 * is built from the position's *stored* `activated_at`/`position_id` (`update_tier.rs:19-24`),
 * never `now` — so a value tie against the tier's minimum can still evict, the older position
 * winning. `RankKey`'s `"activating"` variant encodes the opposite case (`approve_deposit`,
 * where `activated_at = now` always loses a tie) and would silently under-report a real
 * eviction here: the client computes no displacement, passes `displaced: null`, and the program
 * evicts anyway — `settle_displaced_member`'s `.find()` over an empty `remaining_accounts` then
 * fails loudly with 3005. This alias makes an `"activating"` key a compile error at this call
 * boundary rather than a reachable runtime bug.
 */
export type UpdateTierRankKey = Extract<RankKey, { kind: "stored" }>;

/** Identity function narrowing a `RankKey` to the `"stored"` shape `update_tier` requires — pass
 * its result to `resolveDisplaced` when computing this instruction's `displaced`. */
export function assertUpdateTierRankKey(key: UpdateTierRankKey): UpdateTierRankKey {
  return key;
}

/**
 * `displaced` is `resolveDisplaced`'s result for this position, called with a `{ kind: "stored",
 * value, activatedAt, positionId }` key (see `UpdateTierRankKey`) — `null` when no member is
 * displaced, otherwise the `TopTier` entry's `position` address. Carried by key in
 * `remaining_accounts`, absent entirely when `null` (the by-key convention — see decision 5;
 * `update_config`'s TierSize-shrink convention is positional and unrelated).
 */
export async function updateTier(params: {
  operator: Address;
  poolId: number;
  nftMint: Address;
  displaced: Address | null;
}): Promise<Instruction> {
  const { operator, poolId, nftMint, displaced } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(nftMint),
  ]);

  return buildAnchorIx({
    name: "update_tier",
    accounts: [
      signer(operator),
      readonly(protocolConfig),
      readonly(pool),
      writable(topTier),
      writable(position),
    ],
    remaining: displaced === null ? [] : [writable(displaced)],
  });
}
