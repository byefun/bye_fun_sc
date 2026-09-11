// Codec tests for scripts/lib/tier.ts — the update_tier encoder. Mirrors admin.test.ts's
// pattern: discriminator derivation, the full ordered account/role list (independent of the
// encoder's own PDA derivation calls), and the remaining_accounts by-key convention.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { AccountRole, getAddressDecoder, type Address, type Instruction } from "@solana/kit";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import {
  updateTier,
  assertUpdateTierRankKey,
  type UpdateTierRankKey,
} from "../../scripts/lib/tier.js";

// ── Fixtures ──────────────────────────────────────────────────────────────────────────────────
const addrDec = getAddressDecoder();

/** A distinct, valid `Address` per `n` — arbitrary 32-byte pubkeys, not real keys. */
function dummy(n: number): Address {
  return addrDec.decode(Buffer.alloc(32, n));
}

// Seeds, sourced independently from programs/bye_machine/src/common/seeds.rs — not copied from
// tier.ts — so a seed typo or a swapped seed constant in production shows up as a derived-
// address mismatch below rather than passing by construction.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const TOP_TIER_SEED = "tier";
const POSITION_SEED = "position";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

function ixDiscriminatorBytes(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

/** `buildAnchorIx` always sets `data`/`accounts` — the base `Instruction` type just allows for
 * instructions with neither. */
function dataOf(ix: Instruction): Buffer {
  assert.ok(ix.data, "instruction has no data");
  return Buffer.from(ix.data);
}

function accountsOf(ix: Instruction) {
  assert.ok(ix.accounts, "instruction has no accounts");
  return ix.accounts;
}

function assertDiscriminator(ix: Instruction, name: string) {
  assert.deepEqual(dataOf(ix).subarray(0, 8), ixDiscriminatorBytes(name));
}

// ── update_tier ───────────────────────────────────────────────────────────────────────────────
describe("updateTier", () => {
  const operator = dummy(1);
  const poolId = 3;
  const nftMint = dummy(2);

  async function pdas() {
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(nftMint),
    ]);
    return { protocolConfig, pool, topTier, position };
  }

  it("has the update_tier discriminator", async () => {
    const ix = await updateTier({ operator, poolId, nftMint, displaced: null });
    assertDiscriminator(ix, "update_tier");
  });

  it("carries no args — instruction data is exactly the 8-byte discriminator", async () => {
    const ix = await updateTier({ operator, poolId, nftMint, displaced: null });
    assert.equal(dataOf(ix).length, 8);
  });

  it("has 5 accounts in IDL order: operator, protocol_config, pool, top_tier, position", async () => {
    const ix = await updateTier({ operator, poolId, nftMint, displaced: null });
    const { protocolConfig, pool, topTier, position } = await pdas();
    const expectedAddresses = [operator, protocolConfig, pool, topTier, position];

    assert.equal(accountsOf(ix).length, 5);
    // Order-sensitive, not just membership: top_tier and position are both writable PDAs
    // derived from the same `pool` seed prefix (position also keys on nft_mint) — a length or
    // Set-membership check alone would not catch them being swapped, only an ordered address
    // comparison does.
    assert.deepEqual(
      accountsOf(ix).map((a) => a.address),
      expectedAddresses,
    );
    assert.equal(new Set(expectedAddresses).size, expectedAddresses.length);
    assert.deepEqual(
      accountsOf(ix).map((a) => a.role),
      [
        AccountRole.READONLY_SIGNER, // operator
        AccountRole.READONLY, // protocol_config
        AccountRole.READONLY, // pool — update_tier writes no Pool field
        AccountRole.WRITABLE, // top_tier
        AccountRole.WRITABLE, // position
      ],
    );
  });

  it("a swapped top_tier/position pair fails the ordered comparison, not merely a length check", async () => {
    const ix = await updateTier({ operator, poolId, nftMint, displaced: null });
    const { protocolConfig, pool, topTier, position } = await pdas();
    const correct = [operator, protocolConfig, pool, topTier, position];
    const swapped = [operator, protocolConfig, pool, position, topTier];

    const actual = accountsOf(ix).map((a) => a.address);
    assert.deepEqual(actual, correct);
    assert.notDeepEqual(actual, swapped);
    assert.equal(actual.length, swapped.length, "a length check alone would not distinguish these");
  });

  it("appends the displaced position as a single writable remaining_accounts entry, by key", async () => {
    const displaced = dummy(9);
    const ix = await updateTier({ operator, poolId, nftMint, displaced });
    assert.equal(accountsOf(ix).length, 6);
    assert.equal(accountsOf(ix)[5].address, displaced);
    assert.equal(accountsOf(ix)[5].role, AccountRole.WRITABLE);
  });

  it("omits remaining_accounts entirely when displaced is null", async () => {
    const ix = await updateTier({ operator, poolId, nftMint, displaced: null });
    assert.equal(accountsOf(ix).length, 5);
  });

  it("derives pool/top_tier/position from the caller-supplied poolId and nftMint, not hardcoded", async () => {
    const otherNftMint = dummy(20);
    const ixA = await updateTier({ operator, poolId, nftMint, displaced: null });
    const ixB = await updateTier({ operator, poolId, nftMint: otherNftMint, displaced: null });
    // Only the position account (index 4) should change; everything before it is pool-derived.
    assert.deepEqual(
      accountsOf(ixA).slice(0, 4).map((a) => a.address),
      accountsOf(ixB).slice(0, 4).map((a) => a.address),
    );
    assert.notEqual(accountsOf(ixA)[4].address, accountsOf(ixB)[4].address);
  });

  // ── F-1: the encoder's `displaced` cannot be produced from an "activating" RankKey ──────────
  //
  // update_tier never activates anything — its candidate TierEntry is built from the position's
  // *stored* activated_at/position_id (update_tier.rs:19-24), never `now`. tier-resolve.ts's
  // `resolveDisplaced` accepts either RankKey kind, so nothing in that shared file stops a
  // caller resolving update_tier's `displaced` with an "activating" key — the exact call that
  // would silently reintroduce F-1 at this site (a real tie-eviction resolves to `null`, and the
  // program then fails loudly at 3005 rather than settling the eviction). `tier.ts` closes that
  // gap on its own side of the boundary: `UpdateTierRankKey`/`assertUpdateTierRankKey` narrow the
  // type a caller may feed into `resolveDisplaced` for this instruction to `"stored"` only.
  //
  // This is a compile-time guard, not a runtime one: `pnpm test` runs under tsx/esm, which does
  // not type-check, so the `@ts-expect-error` below is inert at `node:test` runtime — it is
  // `pnpm typecheck` (tsc --noEmit) that fails if this line stops being a type error (an unused
  // `@ts-expect-error` is itself a tsc error). Stated per the campaign rule: name the gate that
  // actually closes the case.
  it("assertUpdateTierRankKey accepts only a stored RankKey — enforced by pnpm typecheck", () => {
    const stored: UpdateTierRankKey = {
      kind: "stored",
      value: 100n,
      activatedAt: 5n,
      positionId: 7n,
    };
    assert.deepEqual(assertUpdateTierRankKey(stored), stored);

    // @ts-expect-error — an "activating" RankKey is approve_deposit's shape, not update_tier's;
    // feeding it into resolveDisplaced here would silently reintroduce F-1 (see comment above).
    // If this stops erroring, the narrowing regressed and F-1 is reopened at this call site.
    assertUpdateTierRankKey({ kind: "activating", value: 100n });
  });
});
