// Differential pin for the ported `rank` order (tier.rs's own test vectors), plus behavioural
// tests for the two resolvers against a fake RPC — no validator required. The fake RPC lets
// these run in CI alongside codec.test.ts while still exercising the resolvers' real I/O path.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { address } from "@solana/kit";
import type { Address } from "@solana/kit";
import { Borsh, accountDiscriminator } from "../../scripts/lib/anchor.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import {
  rank,
  resolveDisplaced,
  resolveDisplacedForRecordValueCrossing,
  resolveDisplacedForUpdateTier,
  resolveSuccessor,
  type RankKey,
  type TierEntryData,
} from "../../scripts/lib/tier-resolve.js";
import type { Connection } from "../../scripts/_common.js";
import {
  SYSTEM_PROGRAM,
  TOKEN_PROGRAM,
  TOKEN_2022_PROGRAM,
  ASSOCIATED_TOKEN_PROGRAM,
  ED25519_PROGRAM,
  PROGRAM_ID,
} from "../../scripts/constants.js";

// Five distinct, valid pubkeys to stand in for `position` — reused constants rather than a
// keypair generator, since only distinctness (never the value) matters here.
const POS = [SYSTEM_PROGRAM, TOKEN_PROGRAM, TOKEN_2022_PROGRAM, ASSOCIATED_TOKEN_PROGRAM, ED25519_PROGRAM];
const POOL = address("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
const OTHER_POOL = address("CCryptWBYktukHDQ2vHGtVcmtjXxYzvw8XNVY64YN2Yf");

// `PositionState`'s wire bytes — pinned by `position.rs`'s own
// `position_state_variant_order_is_the_wire_discriminant` test.
const STATE_PENDING = 0;
const STATE_ACTIVE = 1;

function entry(position: Address, value: bigint, activatedAt: bigint, positionId: bigint): TierEntryData {
  return { position, value, activatedAt, positionId };
}

// ── Fixture builders — independent of tier-resolve.ts's own Cursor, built field-by-field in
// the exact declared order (`state/pool.rs`'s and `state/top_tier.rs`'s structs, by name) via
// the already-tested Borsh writer.

function buildPoolBytes(tierSize: number): Buffer {
  return new Borsh()
    .fixed(Buffer.alloc(8), 8) // discriminator
    .u16(1) // pool_id
    .pubkey(POOL) // weight_index
    .pubkey(POOL) // treasury_03
    .u64(0n) // price
    .u64(0n) // ticket_target
    .u64(0n) // fee
    .u16(0) // alloc_equal_bps
    .u16(0) // alloc_tier_bps
    .u16(0) // alloc_protocol_bps
    .u64(0n) // admission_floor
    .u64(0n) // admission_ceiling
    .u64(0n) // wallet_value_cap
    .u8(0) // max_n
    .u16(0) // buyback_rate_bps
    .arrayU64([0n, 0n, 0n], 3) // blank_faces
    .u64(0n) // t04_ceiling
    .u16(0) // sweep_cadence_hours
    .u8(tierSize) // tier_size
    .build();
}

function buildTopTierBytes(entries: TierEntryData[]): Buffer {
  const b = new Borsh()
    .fixed(Buffer.alloc(8), 8) // discriminator
    .pubkey(POOL) // pool
    .u8(entries.length); // len
  for (const e of entries) {
    b.pubkey(e.position).u64(e.value).i64(e.activatedAt).u64(e.positionId);
  }
  return b.build();
}

async function topTierAddress(pool: Address): Promise<Address> {
  const { address: addr } = await derivePda(["tier", seedFromPubkey(pool)]);
  return addr;
}

// `Position` field order, `state/position.rs`'s struct, in the account's own real discriminator so the
// scan's discriminator filter is exercised against genuine bytes rather than a placeholder.
function buildPositionBytes(opts: {
  pool: Address;
  state: number;
  value: bigint;
  activatedAt: bigint;
  positionId: bigint;
  inTier: boolean;
}): Buffer {
  return new Borsh()
    .fixed(accountDiscriminator("Position"), 8)
    .pubkey(opts.pool)
    .pubkey(POS[4]) // depositor — irrelevant to the scan
    .pubkey(POS[4]) // nft_mint — irrelevant to the scan
    .u64(opts.positionId)
    .u8(opts.state)
    .u64(opts.value) // recorded_value
    .i64(0n) // value_observed_at
    .u64(0n) // deposit_value
    .u32(0) // slot_index
    .i64(opts.activatedAt)
    .u128(0n) // equal_checkpoint
    .u64(0n) // accrued
    .bool(opts.inTier)
    .u128(0n) // tier_checkpoint
    .i64(0n) // lock_until
    .u16(0) // reject_reason
    .u8(0) // bump
    .u8(0) // vault_bump
    .build();
}

type MemcmpFilter = { memcmp: { offset: bigint; bytes: string; encoding: "base64" } };

/** The same comparison a real RPC node applies for a `memcmp` `getProgramAccounts` filter. */
function matchesFilters(data: Buffer, filters: readonly MemcmpFilter[]): boolean {
  return filters.every(({ memcmp }) => {
    const want = Buffer.from(memcmp.bytes, "base64");
    const offset = Number(memcmp.offset);
    return data.subarray(offset, offset + want.length).equals(want);
  });
}

/**
 * A `Connection` whose `rpc.getAccountInfo` is served from `byAddress` and whose
 * `rpc.getProgramAccounts` runs the real `memcmp` comparison above against `programAccounts` —
 * not a canned list keyed by call — so a wrong offset in production genuinely changes what the
 * scan returns, the discriminating positive control the offsets need.
 */
function fakeConnection(
  byAddress: Partial<Record<Address, Buffer | null>>,
  programAccounts: { pubkey: Address; data: Buffer }[] = [],
): Connection {
  return {
    rpc: {
      getAccountInfo: (addr: Address) => ({
        send: async () => {
          if (!(addr in byAddress)) throw new Error(`fakeConnection: unexpected read of ${addr}`);
          const data = byAddress[addr];
          return { value: data ? { data: [data.toString("base64"), "base64"] } : null };
        },
      }),
      getProgramAccounts: (_programId: Address, opts: { filters?: readonly MemcmpFilter[] }) => ({
        send: async () =>
          programAccounts
            .filter(({ data }) => matchesFilters(data, opts.filters ?? []))
            .map(({ pubkey, data }) => ({
              pubkey,
              account: { data: [data.toString("base64"), "base64"] },
            })),
      }),
    },
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  } as any as Connection;
}

// ── rank: differential pin against tier.rs's own vectors (rank_orders_..., sorting_by_rank_...)

describe("rank (tier.rs::rank port)", () => {
  it("orders value desc, then activated_at asc, then position_id asc", () => {
    const higherValue = entry(POS[0], 200n, 100n, 1n);
    const lowerValue = entry(POS[1], 100n, 50n, 1n);
    assert.equal(rank(higherValue, lowerValue), -1);

    const earlier = entry(POS[0], 100n, 50n, 1n);
    const later = entry(POS[1], 100n, 60n, 1n);
    assert.equal(rank(earlier, later), -1);

    const lowerId = entry(POS[0], 100n, 50n, 1n);
    const higherId = entry(POS[1], 100n, 50n, 2n);
    assert.equal(rank(lowerId, higherId), -1);
  });

  it("sorts the same four entries into tier.rs's expected full order", () => {
    const entries = [
      entry(POS[0], 100n, 10n, 5n),
      entry(POS[1], 300n, 20n, 1n),
      entry(POS[2], 200n, 5n, 2n),
      entry(POS[3], 300n, 20n, 0n),
    ];
    entries.sort((a, b) => rank(a, b));
    // 300@t20#0 < 300@t20#1 (lower position_id) < 200@t5#2 < 100@t10#5 — tier.rs's own comment.
    assert.deepEqual(
      entries.map((e) => e.position),
      [POS[3], POS[1], POS[2], POS[0]],
    );
  });

  it("breaks a value tie on activated_at: older wins", () => {
    assert.equal(rank(entry(POS[0], 100n, 5n, 9n), entry(POS[1], 100n, 10n, 1n)), -1);
    assert.equal(rank(entry(POS[0], 100n, 10n, 1n), entry(POS[1], 100n, 5n, 9n)), 1);
  });

  it("breaks a value+activated_at tie on position_id: lower wins", () => {
    assert.equal(rank(entry(POS[0], 100n, 10n, 1n), entry(POS[1], 100n, 10n, 2n)), -1);
    assert.equal(rank(entry(POS[0], 100n, 10n, 2n), entry(POS[1], 100n, 10n, 1n)), 1);
  });

  it("is Equal for identical keys", () => {
    assert.equal(rank(entry(POS[0], 100n, 10n, 1n), entry(POS[1], 100n, 10n, 1n)), 0);
  });
});

// ── resolveDisplaced

describe("resolveDisplaced", () => {
  it("returns null when the tier has room (len < tier_size)", async () => {
    const incumbent = entry(POS[0], 100n, 10n, 1n);
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({
      [POOL]: buildPoolBytes(2),
      [topTier]: buildTopTierBytes([incumbent]),
    });
    const key: RankKey = { kind: "stored", value: 1_000n, activatedAt: 0n, positionId: 0n };
    assert.equal(await resolveDisplaced(rpc, POOL, key), null);
  });

  it("returns the minimum's Address when the tier is full and the candidate outranks it", async () => {
    const worst = entry(POS[0], 100n, 10n, 1n);
    const best = entry(POS[1], 300n, 5n, 2n);
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({
      [POOL]: buildPoolBytes(2),
      [topTier]: buildTopTierBytes([best, worst]), // sorted best-first, as the real account is
    });
    const key: RankKey = { kind: "stored", value: 150n, activatedAt: 10n, positionId: 1n };
    assert.equal(await resolveDisplaced(rpc, POOL, key), worst.position);
  });

  it("returns null when the tier is full and the candidate does not outrank the minimum", async () => {
    const worst = entry(POS[0], 100n, 10n, 1n);
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({
      [POOL]: buildPoolBytes(1),
      [topTier]: buildTopTierBytes([worst]),
    });
    const key: RankKey = { kind: "stored", value: 50n, activatedAt: 10n, positionId: 1n };
    assert.equal(await resolveDisplaced(rpc, POOL, key), null);
  });

  // --- F-1: the discriminated union's whole point -------------------------------------------
  it("F-1: an `activating` candidate loses a pure value tie that a `stored` candidate would win", async () => {
    const incumbent = entry(POS[0], 100n, 50n, 5n);
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({
      [POOL]: buildPoolBytes(1),
      [topTier]: buildTopTierBytes([incumbent]),
    });

    // approve_deposit-shaped: same value, activated_at = now (>= incumbent's) — always loses.
    const activating: RankKey = { kind: "activating", value: 100n };
    assert.equal(
      await resolveDisplaced(rpc, POOL, activating),
      null,
      "a value tie must not displace when the candidate is activating now",
    );

    // record_value/update_tier-shaped: same value, an older stored activated_at — evicts.
    const stored: RankKey = { kind: "stored", value: 100n, activatedAt: 10n, positionId: 5n };
    assert.equal(
      await resolveDisplaced(rpc, POOL, stored),
      incumbent.position,
      "an older stored candidate must win the same value tie an activating one loses",
    );
  });

  it("throws, never returns null, when the Pool account cannot be read", async () => {
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({ [POOL]: null, [topTier]: buildTopTierBytes([]) });
    await assert.rejects(
      resolveDisplaced(rpc, POOL, { kind: "activating", value: 1n }),
      /Pool account not found/,
    );
  });

  it("throws, never returns null, when the TopTier account cannot be read", async () => {
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({ [POOL]: buildPoolBytes(1), [topTier]: null });
    await assert.rejects(
      resolveDisplaced(rpc, POOL, { kind: "activating", value: 1n }),
      /TopTier account not found/,
    );
  });
});

// ── resolveDisplaced, narrowed per call site (F-1 reopened one layer up)
//
// `resolveDisplaced` itself must keep accepting either `RankKey` kind — `"activating"` is
// genuinely correct at `approve_deposit` (decision 4). But `update_tier` and `record_value`'s
// crossing arm both build their candidate from a *stored* activated_at/position_id
// (update_tier.rs:19-24), never `now`, so nothing stopped a caller resolving either site's
// `displaced` with an `"activating"` key — silently under-reporting a real tie-eviction (F-1,
// reopened one layer up from tier.ts's own `UpdateTierRankKey` guard, tests/unit/tier.test.ts).
// These two exports make the correctly-typed path the only one available at each site.

describe("resolveDisplaced, narrowed per call site", () => {
  it("resolveDisplacedForUpdateTier behaves like resolveDisplaced for a stored key", async () => {
    const incumbent = entry(POS[0], 100n, 50n, 5n);
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({ [POOL]: buildPoolBytes(1), [topTier]: buildTopTierBytes([incumbent]) });
    const stored: Extract<RankKey, { kind: "stored" }> = {
      kind: "stored",
      value: 100n,
      activatedAt: 10n,
      positionId: 5n,
    };
    assert.equal(await resolveDisplacedForUpdateTier(rpc, POOL, stored), incumbent.position);
  });

  it("resolveDisplacedForRecordValueCrossing behaves like resolveDisplaced for a stored key", async () => {
    const incumbent = entry(POS[0], 100n, 50n, 5n);
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({ [POOL]: buildPoolBytes(1), [topTier]: buildTopTierBytes([incumbent]) });
    const stored: Extract<RankKey, { kind: "stored" }> = {
      kind: "stored",
      value: 100n,
      activatedAt: 10n,
      positionId: 5n,
    };
    assert.equal(await resolveDisplacedForRecordValueCrossing(rpc, POOL, stored), incumbent.position);
  });

  // Compile-time guard, not a runtime one — same shape as tier.test.ts's identical pin: `pnpm
  // test` runs under tsx/esm, which does not type-check, so the `@ts-expect-error` lines below
  // are inert at `node:test` runtime (the calls genuinely execute, which is why each is awaited
  // and asserted to reject — `rpc` below has no accounts registered, so `resolveDisplaced`
  // throws "Pool account not found" rather than silently resolving). It is `pnpm typecheck`
  // (tsc --noEmit) that fails if either line stops being a type error (an unused
  // `@ts-expect-error` is itself a tsc error) — the gate that actually closes this case.
  it("both narrowed resolvers reject an `activating` key — enforced by pnpm typecheck", async () => {
    const rpc = fakeConnection({});

    // @ts-expect-error — an "activating" RankKey is approve_deposit's shape; update_tier builds
    // its candidate from a stored activated_at/position_id (F-1) and would silently under-report
    // a real tie-eviction if this compiled. If this stops erroring, the narrowing regressed.
    await assert.rejects(resolveDisplacedForUpdateTier(rpc, POOL, { kind: "activating", value: 100n }));

    // @ts-expect-error — same F-1 shape, record_value's crossing arm.
    await assert.rejects(resolveDisplacedForRecordValueCrossing(rpc, POOL, { kind: "activating", value: 100n }));
  });
});

// ── resolveSuccessor

describe("resolveSuccessor", () => {
  it("returns null without scanning when the closing position is not a tier member", async () => {
    const otherMember = entry(POS[2], 100n, 10n, 1n);
    const topTier = await topTierAddress(POOL);
    // No `programAccounts` supplied — a scan attempt would return `[]` rather than throw, so
    // this alone would not prove the scan was skipped; the real proof is the ranking test below
    // returning a *specific* eligible candidate only when membership holds.
    const rpc = fakeConnection({ [topTier]: buildTopTierBytes([otherMember]) });
    assert.equal(await resolveSuccessor(rpc, POOL, POS[0]), null);
  });

  it("returns null when the closing position is a member but no eligible non-member exists", async () => {
    const closingMember = entry(POS[0], 100n, 10n, 1n);
    const topTier = await topTierAddress(POOL);
    const ineligible = buildPositionBytes({
      pool: POOL,
      state: STATE_ACTIVE,
      value: 999n,
      activatedAt: 1n,
      positionId: 1n,
      inTier: true, // already a member — not eligible
    });
    // A scan that found nothing here must mean "no eligible candidate", not "the filters are
    // broken" — `ineligible` proves the scan actually ran and correctly excluded it.
    const rpc = fakeConnection(
      { [topTier]: buildTopTierBytes([closingMember]) },
      [{ pubkey: POS[1], data: ineligible }],
    );
    assert.equal(await resolveSuccessor(rpc, POOL, POS[0]), null);
  });

  it("returns the highest-ranked eligible non-member when the closing position is a member", async () => {
    const closingMember = entry(POS[0], 500n, 1n, 1n);
    const topTier = await topTierAddress(POOL);
    const low = buildPositionBytes({
      pool: POOL,
      state: STATE_ACTIVE,
      value: 100n,
      activatedAt: 10n,
      positionId: 2n,
      inTier: false,
    });
    const highest = buildPositionBytes({
      pool: POOL,
      state: STATE_ACTIVE,
      value: 300n,
      activatedAt: 5n,
      positionId: 3n,
      inTier: false,
    });
    const rpc = fakeConnection(
      { [topTier]: buildTopTierBytes([closingMember]) },
      [
        { pubkey: POS[1], data: low },
        { pubkey: POS[2], data: highest },
      ],
    );
    assert.equal(await resolveSuccessor(rpc, POOL, POS[0]), POS[2]);
  });

  it("throws, never returns null, when the TopTier account cannot be read", async () => {
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({ [topTier]: null });
    await assert.rejects(resolveSuccessor(rpc, POOL, POS[0]), /TopTier account not found/);
  });
});

// ── findHighestRankedEligibleNonMember's scan, exercised through resolveSuccessor
//
// AC-30 finding: `fill_vacancy_from_candidate` (common/tier.rs:134-163) validates only
// pool/state/membership on whatever candidate it is given — it never ranks alternatives, so the
// "highest-ranked non-member promotes" clause has no on-chain backstop. This scan is the sole
// enforcement, which is why each filter dimension below is pinned as its own counterfactual
// (the campaign's most repeated defect class is exactly this kind of adjacent-field confusion).

describe("the successor scan's memcmp filters", () => {
  const closingMember = entry(POS[0], 500n, 1n, 1n);
  const eligible = {
    pool: POOL,
    state: STATE_ACTIVE,
    value: 100n,
    activatedAt: 10n,
    positionId: 2n,
    inTier: false,
  };

  async function scanFor(programAccounts: { pubkey: Address; data: Buffer }[]): Promise<Address | null> {
    const topTier = await topTierAddress(POOL);
    const rpc = fakeConnection({ [topTier]: buildTopTierBytes([closingMember]) }, programAccounts);
    return resolveSuccessor(rpc, POOL, POS[0]);
  }

  it("includes a genuinely eligible position — the baseline every counterfactual below flips", async () => {
    assert.equal(await scanFor([{ pubkey: POS[1], data: buildPositionBytes(eligible) }]), POS[1]);
  });

  it("excludes a position for the wrong pool", async () => {
    const wrongPool = buildPositionBytes({ ...eligible, pool: OTHER_POOL });
    assert.equal(await scanFor([{ pubkey: POS[1], data: wrongPool }]), null);
  });

  it("excludes a non-Active position", async () => {
    const pending = buildPositionBytes({ ...eligible, state: STATE_PENDING });
    assert.equal(await scanFor([{ pubkey: POS[1], data: pending }]), null);
  });

  it("excludes a position that is already a tier member", async () => {
    const member = buildPositionBytes({ ...eligible, inTier: true });
    assert.equal(await scanFor([{ pubkey: POS[1], data: member }]), null);
  });

  it("excludes an account whose discriminator is not Position's", async () => {
    const bytes = buildPositionBytes(eligible);
    accountDiscriminator("TopTier").copy(bytes, 0); // same shape, a different account type's disc
    assert.equal(await scanFor([{ pubkey: POS[1], data: bytes }]), null);
  });
});
