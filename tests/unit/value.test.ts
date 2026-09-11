// Codec tests for scripts/lib/value.ts — begin_sweep, record_value, end_sweep.
// Mirrors tests/unit/admin.test.ts's pattern: discriminator derivation, fixed golden bytes for
// the arg encoding (independent of the encoder's own Borsh calls), and the account list's exact
// length and order. record_value additionally covers its two `remaining_accounts` conventions
// via `ValueEffect`, a swapped-account regression, and a type-level pin that the union's two
// variants cannot carry each other's field.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  AccountRole,
  getAddressDecoder,
  getAddressEncoder,
  type Address,
  type Instruction,
} from "@solana/kit";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { beginSweep, endSweep, recordValue, type ValueEffect } from "../../scripts/lib/value.js";

// ── Fixtures ──────────────────────────────────────────────────────────────────────────────────
const addrEnc = getAddressEncoder();
const addrDec = getAddressDecoder();

/** A distinct, valid `Address` per `n` — arbitrary 32-byte pubkeys, not real keys. */
function dummy(n: number): Address {
  return addrDec.decode(Buffer.alloc(32, n));
}

// Seeds, sourced independently from programs/bye_machine/src/common/seeds.rs — not copied from
// value.ts — so a seed typo or a swapped seed constant in production shows up as a derived-
// address mismatch below rather than passing by construction.
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

// Buffer-level primitives, written independently of scripts/lib/anchor.ts's Borsh class, so the
// "golden bytes" below are a second path over the same spec rather than a round-trip.
const u64Buf = (n: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(n);
  return b;
};
const i64Buf = (n: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigInt64LE(n);
  return b;
};

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

function argsOf(ix: Instruction): Buffer {
  return dataOf(ix).subarray(8);
}

async function deriveProtocolConfig() {
  return (await derivePda([PROTOCOL_CONFIG_SEED])).address;
}

async function derivePool(poolId: number) {
  return (await derivePda([POOL_SEED, seedFromU16Le(poolId)])).address;
}

// ── begin_sweep ──────────────────────────────────────────────────────────────────────────────
describe("beginSweep", () => {
  const operator = dummy(1);
  const poolId = 3;

  it("has the begin_sweep discriminator and no args", async () => {
    const ix = await beginSweep({ operator, poolId });
    assertDiscriminator(ix, "begin_sweep");
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 3 accounts in order: operator, protocol_config, pool", async () => {
    const ix = await beginSweep({ operator, poolId });
    const protocolConfig = await deriveProtocolConfig();
    const pool = await derivePool(poolId);

    assert.equal(accountsOf(ix).length, 3);
    assert.deepEqual(
      accountsOf(ix).map((a) => a.address),
      [operator, protocolConfig, pool],
    );
    assert.deepEqual(
      accountsOf(ix).map((a) => a.role),
      [AccountRole.READONLY_SIGNER, AccountRole.READONLY, AccountRole.WRITABLE],
    );
  });
});

// ── end_sweep ────────────────────────────────────────────────────────────────────────────────
describe("endSweep", () => {
  const operator = dummy(2);
  const poolId = 4;

  it("has the end_sweep discriminator and no args", async () => {
    const ix = await endSweep({ operator, poolId });
    assertDiscriminator(ix, "end_sweep");
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 3 accounts in order: operator, protocol_config, pool", async () => {
    const ix = await endSweep({ operator, poolId });
    const protocolConfig = await deriveProtocolConfig();
    const pool = await derivePool(poolId);

    assert.equal(accountsOf(ix).length, 3);
    assert.deepEqual(
      accountsOf(ix).map((a) => a.address),
      [operator, protocolConfig, pool],
    );
    assert.deepEqual(
      accountsOf(ix).map((a) => a.role),
      [AccountRole.READONLY_SIGNER, AccountRole.READONLY, AccountRole.WRITABLE],
    );
  });

  it("derives a distinct pool address from beginSweep's for a different poolId", async () => {
    const beginIx = await beginSweep({ operator: dummy(1), poolId: 3 });
    const endIx = await endSweep({ operator, poolId });
    assert.notEqual(accountsOf(beginIx)[2].address, accountsOf(endIx)[2].address);
  });
});

// ── record_value ─────────────────────────────────────────────────────────────────────────────
describe("recordValue", () => {
  const operator = dummy(10);
  const poolId = 7;
  const nftMint = dummy(11);
  const depositor = dummy(12);
  const weightIndex = dummy(13);
  const value = 42_000_000n;
  const observedAt = 1_234_567_890n;

  async function expectedMainAccounts() {
    const protocolConfig = await deriveProtocolConfig();
    const pool = await derivePool(poolId);
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
    return { protocolConfig, pool, position, topTier, walletStats };
  }

  const noOpEffect: ValueEffect = { kind: "cross", displaced: null };

  it("has the record_value discriminator", async () => {
    const ix = await recordValue({
      operator,
      poolId,
      nftMint,
      depositor,
      weightIndex,
      value,
      observedAt,
      effect: noOpEffect,
    });
    assertDiscriminator(ix, "record_value");
  });

  it("encodes value (u64) then observed_at (i64), IDL declaration order", async () => {
    const ix = await recordValue({
      operator,
      poolId,
      nftMint,
      depositor,
      weightIndex,
      value,
      observedAt,
      effect: noOpEffect,
    });
    const expected = Buffer.concat([u64Buf(value), i64Buf(observedAt)]);
    assert.deepEqual(argsOf(ix), expected);
    assert.equal(argsOf(ix).length, 16);
  });

  it("has 7 accounts in IDL order, none of them the same address", async () => {
    const ix = await recordValue({
      operator,
      poolId,
      nftMint,
      depositor,
      weightIndex,
      value,
      observedAt,
      effect: noOpEffect,
    });
    const { protocolConfig, pool, position, topTier, walletStats } = await expectedMainAccounts();
    const expectedAddresses = [
      operator,
      protocolConfig,
      pool,
      position,
      weightIndex,
      topTier,
      walletStats,
    ];

    // Only the accounts.first(7) — main account list, before any remaining_accounts.
    const main = accountsOf(ix).slice(0, 7);
    assert.equal(main.length, 7);
    assert.deepEqual(
      main.map((a) => a.address),
      expectedAddresses,
    );
    assert.equal(new Set(expectedAddresses).size, expectedAddresses.length);
    assert.deepEqual(
      main.map((a) => a.role),
      [
        AccountRole.READONLY_SIGNER, // operator
        AccountRole.READONLY, // protocol_config
        AccountRole.WRITABLE, // pool
        AccountRole.WRITABLE, // position
        AccountRole.WRITABLE, // weight_index
        AccountRole.WRITABLE, // top_tier
        AccountRole.WRITABLE, // wallet_stats
      ],
    );
  });

  // Order-sensitive regression: pool and top_tier are both writable PDAs of the same pool.
  // A length check or a Set-membership check over the account list passes even if their
  // positions (or position/wallet_stats's) were swapped; only an ordered deepEqual catches it.
  it("fails to match if two same-role accounts are swapped", async () => {
    const ix = await recordValue({
      operator,
      poolId,
      nftMint,
      depositor,
      weightIndex,
      value,
      observedAt,
      effect: noOpEffect,
    });
    const { protocolConfig, pool, position, topTier, walletStats } = await expectedMainAccounts();
    const swapped = [operator, protocolConfig, topTier, position, weightIndex, pool, walletStats];
    const actual = accountsOf(ix)
      .slice(0, 7)
      .map((a) => a.address);

    assert.notDeepEqual(actual, swapped);
    // The swap is still a length-7 permutation of the same address set — a set/length check
    // alone would pass it; only the ordered assertion above discriminates.
    assert.equal(new Set(actual).size, new Set(swapped).size);
  });

  // ── ValueEffect: close ──────────────────────────────────────────────────────────────────────
  describe("effect: close", () => {
    it("appends no remaining account when successor is null (positional-absent convention)", async () => {
      const ix = await recordValue({
        operator,
        poolId,
        nftMint,
        depositor,
        weightIndex,
        value,
        observedAt,
        effect: { kind: "close", successor: null },
      });
      assert.equal(accountsOf(ix).length, 7);
    });

    it("appends the successor as remaining_accounts[0], writable", async () => {
      const successor = dummy(20);
      const ix = await recordValue({
        operator,
        poolId,
        nftMint,
        depositor,
        weightIndex,
        value,
        observedAt,
        effect: { kind: "close", successor },
      });
      assert.equal(accountsOf(ix).length, 8);
      assert.equal(accountsOf(ix)[7].address, successor);
      assert.equal(accountsOf(ix)[7].role, AccountRole.WRITABLE);
    });
  });

  // ── ValueEffect: cross ──────────────────────────────────────────────────────────────────────
  describe("effect: cross", () => {
    it("appends no remaining account when displaced is null", async () => {
      const ix = await recordValue({
        operator,
        poolId,
        nftMint,
        depositor,
        weightIndex,
        value,
        observedAt,
        effect: { kind: "cross", displaced: null },
      });
      assert.equal(accountsOf(ix).length, 7);
    });

    it("appends the displaced member as a remaining account, writable", async () => {
      const displaced = dummy(21);
      const ix = await recordValue({
        operator,
        poolId,
        nftMint,
        depositor,
        weightIndex,
        value,
        observedAt,
        effect: { kind: "cross", displaced },
      });
      assert.equal(accountsOf(ix).length, 8);
      assert.equal(accountsOf(ix)[7].address, displaced);
      assert.equal(accountsOf(ix)[7].role, AccountRole.WRITABLE);
    });
  });

  // ── F-3: the union, not a shared `Address | null` pair ─────────────────────────────────────
  // Discriminating control: a `close`/`cross` transposition must be unreachable at compile time,
  // not merely absent from today's test fixtures. Each assignment below is invalid on its own —
  // `close` has no `displaced` field, `cross` has no `successor` field — so if either ever
  // compiled, the union has regressed to two same-shaped `Address | null` fields (F-3) and
  // `@ts-expect-error` would itself fail as an unused directive, going red for the right reason.
  it("F-3 pin: a close effect cannot carry `displaced`, a cross effect cannot carry `successor`", () => {
    // @ts-expect-error — ValueEffect's "close" variant has no `displaced` field.
    const badClose: ValueEffect = { kind: "close", displaced: dummy(30) };
    // @ts-expect-error — ValueEffect's "cross" variant has no `successor` field.
    const badCross: ValueEffect = { kind: "cross", successor: dummy(31) };
    void badClose;
    void badCross;
  });
});
