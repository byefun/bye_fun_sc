// Tier-membership resolution — the ONLY file in scripts/lib/ that does I/O.
//
// scripts/lib/*.ts encoders stay pure and synchronous (entry criterion 5: codec tests run
// with no validator). Deciding who a tier activation displaces, or whether a closing
// position's vacancy should be offered a successor, needs a live read of `Pool`/`TopTier` —
// so that resolution lives here, once, and every encoder that needs it (T-40/T-41/T-42/T-43)
// takes the answer as a required `Address | null` parameter instead of computing it itself.
// `anchor.ts` stays program-agnostic; this file is bye_machine-specific by design.
//
// Both exports throw on a read failure — never `null`. `null` means "resolved, and there is
// no displacement/successor"; a failed read must not collapse into that same value, or a
// transient RPC error silently becomes "nobody displaced".

import { getAddressDecoder, getAddressEncoder, type Address } from "@solana/kit";
import type { Connection } from "../_common.js";
import { fetchAccountData } from "../_common.js";
import { PROGRAM_ID } from "../constants.js";
import { accountDiscriminator, derivePda, seedFromPubkey } from "./anchor.js";

// `common/seeds.rs`: TOP_TIER_SEED = b"tier", TopTier's PDA is [TOP_TIER_SEED, pool].
const TOP_TIER_SEED = "tier";

// ── The rank order (common/tier.rs::rank), ported faithfully ────────────────────────────────
//
// value desc, then activated_at asc, then position_id asc — TopTier.entries is kept sorted in
// this order, best rank first (tier.rs:9-16).
export type TierEntryLike = { value: bigint; activatedAt: bigint; positionId: bigint };

/** `Ordering` as `rank` would return it: -1 Less, 0 Equal, 1 Greater. */
export function rank(a: TierEntryLike, b: TierEntryLike): -1 | 0 | 1 {
  if (a.value !== b.value) return a.value > b.value ? -1 : 1; // b.value.cmp(&a.value): higher value ranks first
  if (a.activatedAt !== b.activatedAt) return a.activatedAt < b.activatedAt ? -1 : 1;
  if (a.positionId !== b.positionId) return a.positionId < b.positionId ? -1 : 1;
  return 0;
}

// The candidate side of a rank comparison. `approve_deposit` activates with `activated_at =
// now`, which is always >= every incumbent's stored `activated_at`, and assigns `position_id`
// from `pool.position_counter`, which is monotonic — so a newly-activating candidate never
// wins a tie on either later key. `record_value`/`update_tier` carry a real, already-stored
// `activated_at`/`position_id`, where an older candidate can still evict on a value tie
// (decision 4, the F-1 repair). One resolver, all sites: `activating` is encoded as the exact
// ordering-equivalent of "loses every later-key tie" rather than special-cased per call site.
export type RankKey =
  | { kind: "activating"; value: bigint } // approve_deposit: activated_at = now
  | { kind: "stored"; value: bigint; activatedAt: bigint; positionId: bigint }; // record_value, update_tier

const I64_MAX = (1n << 63n) - 1n;
const U64_MAX = (1n << 64n) - 1n;

function candidateKey(key: RankKey): TierEntryLike {
  return key.kind === "activating"
    ? { value: key.value, activatedAt: I64_MAX, positionId: U64_MAX }
    : { value: key.value, activatedAt: key.activatedAt, positionId: key.positionId };
}

// ── Minimal account reads ────────────────────────────────────────────────────────────────────
//
// Not a general Anchor account reader — T-44's decoders.ts owns that (decision 8). This file
// needs exactly two fields (`Pool.tier_size`, `TopTier.len`/`entries`) and lands before T-44 in
// the batch, so it reads them itself rather than depend on a sibling parallel task's file.

const addrDec = getAddressDecoder();
const addrEnc = getAddressEncoder();

class Cursor {
  private offset = 0;
  constructor(private readonly buf: Buffer) {}
  skip(n: number): this {
    this.offset += n;
    return this;
  }
  u8(): number {
    const v = this.buf.readUInt8(this.offset);
    this.offset += 1;
    return v;
  }
  u64(): bigint {
    const v = this.buf.readBigUInt64LE(this.offset);
    this.offset += 8;
    return v;
  }
  i64(): bigint {
    const v = this.buf.readBigInt64LE(this.offset);
    this.offset += 8;
    return v;
  }
  pubkey(): Address {
    const v = addrDec.decode(this.buf.subarray(this.offset, this.offset + 32));
    this.offset += 32;
    return v;
  }
}

async function requireAccountData(rpc: Connection, addr: Address, what: string): Promise<Buffer> {
  const data = await fetchAccountData(rpc, addr);
  if (!data) throw new Error(`tier-resolve: ${what} account not found at ${addr}`);
  return data;
}

// `Pool` field order, `state/pool.rs`'s struct — skip everything before `tier_size`:
// disc(8) pool_id(2) weight_index(32) treasury_03(32) price(8)
// ticket_target(8) fee(8) alloc_equal_bps(2) alloc_tier_bps(2) alloc_protocol_bps(2)
// admission_floor(8) admission_ceiling(8) wallet_value_cap(8) max_n(1) buyback_rate_bps(2)
// blank_faces(3*8=24) t04_ceiling(8) sweep_cadence_hours(2) → tier_size(1).
const POOL_TIER_SIZE_SKIP = 8 + 2 + 32 * 2 + 8 * 3 + 2 * 3 + 8 * 3 + 1 + 2 + 8 * 3 + 8 + 2;

async function readTierSize(rpc: Connection, pool: Address): Promise<number> {
  const data = await requireAccountData(rpc, pool, "Pool");
  return new Cursor(data).skip(POOL_TIER_SIZE_SKIP).u8();
}

export type TierEntryData = TierEntryLike & { position: Address };

// `TopTier` field order, top_tier.rs:16-22: disc(8) pool(32) len(1) entries([TierEntry;20]).
// `TierEntry`, top_tier.rs:5-10: position(32) value(8) activated_at(8) position_id(8) = 56B.
// Only `entries[0..len]` is live — the rest of the fixed array is stale (decision, P7 open).
async function readTopTier(
  rpc: Connection,
  topTier: Address,
): Promise<{ len: number; entries: TierEntryData[] }> {
  const data = await requireAccountData(rpc, topTier, "TopTier");
  const cursor = new Cursor(data).skip(8 + 32);
  const len = cursor.u8();
  const entries: TierEntryData[] = [];
  for (let i = 0; i < len; i++) {
    entries.push({
      position: cursor.pubkey(),
      value: cursor.u64(),
      activatedAt: cursor.i64(),
      positionId: cursor.u64(),
    });
  }
  return { len, entries };
}

async function deriveTopTier(pool: Address): Promise<Address> {
  const { address } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  return address;
}

// ── Successor scan ───────────────────────────────────────────────────────────────────────────
//
// `Position` field order, pinned byte-for-byte by `state/mod.rs`'s own
// `position_field_order` test in `state/mod.rs`, cited by name — a line range rots on every
// field this account gains, which is what P9 did to the range that stood here. Offsets below are
// that test's field order plus
// the 8-byte Anchor account discriminator every on-chain account carries (the pin's own
// `try_to_vec()` is the bare struct, with no discriminator):
// disc(8) pool(32) depositor(32) nft_mint(32) position_id(8) state(1) recorded_value(8)
// value_observed_at(8) deposit_value(8) slot_index(4) activated_at(8) ...
const POSITION_POOL_OFFSET = 8n;
const POSITION_STATE_OFFSET = 8n + 32n * 3n + 8n; // 112
const POSITION_IN_TIER_OFFSET = POSITION_STATE_OFFSET + 1n + 8n + 8n + 8n + 4n + 8n + 16n + 8n; // 173
// `PositionState::Active`'s own wire byte — pinned by
// `position.rs`'s `position_state_variant_order_is_the_wire_discriminant` test, not counted by
// declaration order (a fieldless-enum reorder would otherwise move this silently).
const POSITION_STATE_ACTIVE_BYTE = 1;

type MemcmpFilter = { memcmp: { offset: bigint; bytes: string; encoding: "base64" } };

function memcmpFilter(offset: bigint, bytes: Buffer): MemcmpFilter {
  return { memcmp: { offset, bytes: bytes.toString("base64"), encoding: "base64" } };
}

/** The one field this file reads off a `getProgramAccounts` hit — kept local and unbranded. */
type ProgramAccountHit = { pubkey: Address; account: { data: [string, string] } };

// Reads exactly the fields `rank` needs off an already-filtered `Position` account: skip past
// `pool`/`depositor`/`nft_mint` to `position_id`, skip the (already-matched) `state` byte, then
// `recorded_value`/`activated_at` — the same field order the offsets above are derived from.
function readEligiblePosition(address: Address, data: Buffer): TierEntryData {
  const cursor = new Cursor(data).skip(8 + 32 * 3);
  const positionId = cursor.u64();
  cursor.skip(1); // state — already constrained to Active by the memcmp filter
  const value = cursor.u64(); // recorded_value
  cursor.skip(8 + 8 + 4); // value_observed_at, deposit_value, slot_index
  const activatedAt = cursor.i64();
  return { position: address, value, activatedAt, positionId };
}

/**
 * The highest-ranked `Active`, non-member `Position` for `pool` — AC-30's "highest-ranked
 * non-member promotes" — or `null` if none is eligible. `fill_vacancy_from_candidate`
 * (common/tier.rs:134-163) validates only pool/state/membership on whatever candidate it is
 * given; it never ranks alternatives, so AC-30's ranking clause has **no on-chain backstop** —
 * an eligible-but-not-highest candidate is accepted and promoted with no error, no event
 * anomaly, and no failure signature anywhere on chain. This scan is the sole enforcement of
 * that clause, which is why `resolveSuccessor` cannot resolve its answer from `Pool`/`TopTier`
 * alone the way `resolveDisplaced` does.
 *
 * The four memcmp filters (account discriminator, `pool`, `state`, `in_tier`) are the RPC
 * filter cap (`getProgramAccountsApi`'s own "up to 4 filters" limit) — one more dimension would
 * need a second call. `getProgramAccounts` has no cursor in the JSON-RPC spec: it returns the
 * complete filtered set in one response or the RPC call itself throws (e.g. a response-size
 * limit on the node) — there is no silent-truncation path to guard against beyond letting that
 * error propagate, which it does here (no try/catch).
 *
 * `getProgramAccounts`'s TS overloads are keyed to whichever `@solana/rpc-types` copy
 * `Connection` (solana-kite) pins, which can be a different installed copy than the one
 * `@solana/kit` exports here — two nominally-branded types for the same wire format, not
 * assignable to each other by name alone. Bypassing overload resolution for this one call is
 * the narrow fix; the result is recast to `ProgramAccountHit`, a local, unbranded shape, before
 * anything else touches it.
 */
async function findHighestRankedEligibleNonMember(
  rpc: Connection,
  pool: Address,
): Promise<TierEntryData | null> {
  const filters: MemcmpFilter[] = [
    memcmpFilter(0n, accountDiscriminator("Position")),
    memcmpFilter(POSITION_POOL_OFFSET, Buffer.from(addrEnc.encode(pool))),
    memcmpFilter(POSITION_STATE_OFFSET, Buffer.from([POSITION_STATE_ACTIVE_BYTE])),
    memcmpFilter(POSITION_IN_TIER_OFFSET, Buffer.from([0])),
  ];
  const getProgramAccounts = rpc.rpc.getProgramAccounts as unknown as (
    programId: Address,
    config: { encoding: "base64"; filters: MemcmpFilter[] },
  ) => { send(): Promise<ProgramAccountHit[]> };
  const accounts = await getProgramAccounts(PROGRAM_ID, { encoding: "base64", filters }).send();
  const candidates = accounts.map(({ pubkey, account }) =>
    readEligiblePosition(pubkey, Buffer.from(account.data[0], "base64")),
  );
  if (candidates.length === 0) return null;
  return candidates.reduce((best, current) => (rank(current, best) === -1 ? current : best));
}

// ── Resolvers ────────────────────────────────────────────────────────────────────────────────

/**
 * The `Address` of the `TopTier` member `key` would displace on activation/crossing, or `null`
 * if there is none. Mirrors `tier::promote`'s eviction condition exactly: the tier must be full
 * (`len === tier_size`) and `key` must outrank the current minimum (`entries[len - 1]`, since
 * entries are kept sorted best-first). `activatedAt`/`positionId` for a `stored` key come off
 * the `Position`/`TierEntry` the caller already has — no extra RPC, no clock (decision 4).
 */
export async function resolveDisplaced(
  rpc: Connection,
  pool: Address,
  key: RankKey,
): Promise<Address | null> {
  const [tierSize, topTier] = await Promise.all([readTierSize(rpc, pool), deriveTopTier(pool)]);
  const { len, entries } = await readTopTier(rpc, topTier);
  if (len === 0 || len !== tierSize) return null;
  const minimum = entries[len - 1];
  return rank(candidateKey(key), minimum) === -1 ? minimum.position : null;
}

// `RankKey`'s "activating" variant is genuinely correct at `approve_deposit` (decision 4) — it
// is not a defect to accept both kinds in `resolveDisplaced` itself, and this narrowing must not
// become the general contract. `update_tier` and `record_value`'s crossing arm both build their
// candidate `TierEntry` from a position's *stored* `activated_at`/`position_id`
// (`update_tier.rs:19-24`, `record_value.rs`'s crossing branch), never `now`, so an "activating"
// key at either call site would silently under-report a real tie-eviction: the client computes
// no displacement, passes `displaced: null`, and the program evicts anyway —
// `settle_displaced_member`'s `.find()` over an empty `remaining_accounts` then fails loudly at
// 3005. `tier.ts` already closed this from its own side for `update_tier`
// (`UpdateTierRankKey`/`assertUpdateTierRankKey`) — that guard only helps a caller who opts in.
// These two exports make the correctly-typed path the only one `resolveDisplaced` exposes at
// each site, so nothing routes an "activating" key there by omission.
type StoredRankKey = Extract<RankKey, { kind: "stored" }>;

/** `resolveDisplaced`, narrowed to the one `RankKey` shape valid at `update_tier`'s call site. */
export async function resolveDisplacedForUpdateTier(
  rpc: Connection,
  pool: Address,
  key: StoredRankKey,
): Promise<Address | null> {
  return resolveDisplaced(rpc, pool, key);
}

/**
 * `resolveDisplaced`, narrowed to the one `RankKey` shape valid at `record_value`'s crossing arm
 * (`ValueEffect`'s `"cross"` case in `scripts/lib/value.ts`) — the non-member-crossing branch,
 * which carries the same stored-timestamp property as `update_tier`.
 */
export async function resolveDisplacedForRecordValueCrossing(
  rpc: Connection,
  pool: Address,
  key: StoredRankKey,
): Promise<Address | null> {
  return resolveDisplaced(rpc, pool, key);
}

/**
 * The `Address` that should fill `closing`'s vacancy, or `null` if none is owed.
 * `common/tier.rs::fill_vacancy_from_candidate` is only reached when the closing position was
 * itself a tier member — `withdraw.rs` and `record_value.rs` both gate the vacate+fill pair
 * behind `position.in_tier` — so a non-member departure resolves to `null` without a scan.
 * Otherwise this is AC-30's "highest-ranked non-member promotes" clause, which the program does
 * not enforce on its own (see [`findHighestRankedEligibleNonMember`]): resolves via the same
 * pool-wide scan and the same `rank` used everywhere else in this file.
 */
export async function resolveSuccessor(
  rpc: Connection,
  pool: Address,
  closing: Address,
): Promise<Address | null> {
  const topTier = await deriveTopTier(pool);
  const { len, entries } = await readTopTier(rpc, topTier);
  const wasMember = entries.slice(0, len).some((e) => e.position === closing);
  if (!wasMember) return null;
  const successor = await findHighestRankedEligibleNonMember(rpc, pool);
  return successor?.position ?? null;
}
