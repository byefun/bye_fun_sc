// Instruction encoders for the value pipeline's three sweep instructions: begin_sweep,
// record_value, end_sweep. Account order and arg shapes are derived mechanically from
// target/idl/bye_machine.json, cross-checked against each instruction's #[derive(Accounts)]
// struct in programs/bye_machine/src/instructions/value/.
//
// Pure and synchronous with respect to I/O (the P7 open, decision 1): every state-dependent
// account — the pool id, the position's `nft_mint`/`depositor`, the `WeightIndex` address — is
// a required, non-defaulted parameter. `record_value` has two `remaining_accounts` obligations
// with incompatible conventions (one positional and silent on omission, one by-key and loud at
// 3005), so they are exposed as one discriminated
// union, `ValueEffect`, rather than two same-typed `Address | null` fields — a transposition
// between them is a `tsc` compile error instead of a silent vacancy or an unintended promotion
// (F-3).

import type { Address, AccountMeta, Instruction } from "@solana/kit";
import {
  Borsh,
  buildAnchorIx,
  derivePda,
  readonly,
  seedFromPubkey,
  signer,
  writable,
} from "./anchor.js";

// ── Seeds (bye_machine-specific — anchor.ts stays program-agnostic) ─────────────────────────
// Verbatim against programs/bye_machine/src/common/seeds.rs. Redeclared here rather than
// imported from admin.ts — each domain file owns its own copies (tier-resolve.ts's precedent),
// so the parallel P7 files carry no cross-file dependency on one another.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const POSITION_SEED = "position";
const WALLET_STATS_SEED = "wallet";
const TOP_TIER_SEED = "tier";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

// ── begin_sweep ──────────────────────────────────────────────────────────────────────────────
// No args — its entire wire correctness is the account list. Same three-account shape as
// admin.ts's `setPause`: operator signer (read-only), protocol_config read-only, pool writable.
export async function beginSweep(params: {
  operator: Address;
  poolId: number;
}): Promise<Instruction> {
  const { operator, poolId } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);

  return buildAnchorIx({
    name: "begin_sweep",
    accounts: [signer(operator), readonly(protocolConfig), writable(pool)],
  });
}

// ── end_sweep ────────────────────────────────────────────────────────────────────────────────
// No args, same three-account shape as `beginSweep`.
export async function endSweep(params: {
  operator: Address;
  poolId: number;
}): Promise<Instruction> {
  const { operator, poolId } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);

  return buildAnchorIx({
    name: "end_sweep",
    accounts: [signer(operator), readonly(protocolConfig), writable(pool)],
  });
}

// ── record_value ─────────────────────────────────────────────────────────────────────────────

/**
 * `record_value`'s two `remaining_accounts` obligations — mutually exclusive per call
 * (record_value.rs:211's below-floor close of a tier member vs. record_value.rs:248's
 * non-member crossing into the tier; no single call reaches both branches), so one
 * discriminated union covers both sites without a shared `Address | null` field a caller could
 * transpose between them.
 *
 * - `close`: the re-attested value fell below `admission_floor` on a position already in the
 *   tier. `successor` is `common/tier.rs::fill_vacancy_from_candidate`'s positional
 *   `remaining_accounts[0]` — `null` reads as "no successor owed" and the program returns
 *   `Ok(None)` with **no error**, so an omission here is silent, not loud. `successor` must be
 *   `tier-resolve.ts`'s `resolveSuccessor` result, never an arbitrary eligible address — the
 *   program never ranks the candidate itself (AC-30 has no on-chain backstop).
 * - `cross`: the re-attested value crossed the admission floor on a position not yet in the
 *   tier, and the tier was already full. `displaced` is
 *   `common/tier.rs::settle_displaced_member`'s by-key match against `remaining_accounts` —
 *   `null` when one was owed fails loudly at 3005.
 */
export type ValueEffect =
  | { kind: "close"; successor: Address | null }
  | { kind: "cross"; displaced: Address | null };

function valueEffectRemaining(effect: ValueEffect): AccountMeta[] {
  switch (effect.kind) {
    case "close":
      return effect.successor === null ? [] : [writable(effect.successor)];
    case "cross":
      return effect.displaced === null ? [] : [writable(effect.displaced)];
  }
}

/**
 * `nftMint`/`depositor` are the position's own fields — `position`/`wallet_stats` are derived
 * PDAs from them, matching admin.ts's precedent of deriving every seed-known PDA rather than
 * taking it as a bare address. `weightIndex` is `pool.weight_index` — a pre-created keypair
 * account (T-45), not a PDA of this program — so it is a required, non-defaulted parameter.
 */
export async function recordValue(params: {
  operator: Address;
  poolId: number;
  nftMint: Address;
  depositor: Address;
  weightIndex: Address;
  value: bigint;
  observedAt: bigint;
  effect: ValueEffect;
}): Promise<Instruction> {
  const { operator, poolId, nftMint, depositor, weightIndex, value, observedAt, effect } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(nftMint),
  ]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: walletStats } = await derivePda([
    WALLET_STATS_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(depositor),
  ]);

  return buildAnchorIx({
    name: "record_value",
    args: new Borsh().u64(value).i64(observedAt).build(),
    accounts: [
      signer(operator),
      readonly(protocolConfig),
      writable(pool),
      writable(position),
      writable(weightIndex),
      writable(topTier),
      writable(walletStats),
    ],
    remaining: valueEffectRemaining(effect),
  });
}
