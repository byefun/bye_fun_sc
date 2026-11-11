// Read-side Anchor codec: one account decoder per program account and one event decoder per
// event. `anchor.ts`'s `Borsh` is write-only, so the reader lives here instead of there —
// this keeps `anchor.ts` a single-writer, program-agnostic file.
//
// Every layout below is hand-mirrored from the Rust structs in
// `programs/bye_machine/src/state/` and the events in `programs/bye_machine/src/common/events.rs`,
// from the same source in the same week — a reorder that moves both together is invisible to this
// file's own tests. That is a stated, accepted limit, not something the golden-byte tests below
// close.

import { getAddressDecoder, type Address } from "@solana/kit";
import { Borsh, accountDiscriminator, eventDiscriminator } from "./anchor.js";

const addrDec = getAddressDecoder();

// ── Borsh reader ─────────────────────────────────────────────────────────────
export class BorshReader {
  private buf: Buffer;
  private offset = 0;

  constructor(data: Buffer | Uint8Array) {
    this.buf = Buffer.from(data);
  }

  private take(n: number): Buffer {
    if (this.offset + n > this.buf.length) {
      throw new Error(
        `expected ${n} byte(s) at offset ${this.offset}, only ${this.buf.length - this.offset} remain`,
      );
    }
    const b = this.buf.subarray(this.offset, this.offset + n);
    this.offset += n;
    return b;
  }

  u8(): number { return this.take(1).readUInt8(0); }
  u16(): number { return this.take(2).readUInt16LE(0); }
  u32(): number { return this.take(4).readUInt32LE(0); }
  u64(): bigint { return this.take(8).readBigUInt64LE(0); }
  i64(): bigint { return this.take(8).readBigInt64LE(0); }
  u128(): bigint {
    const lo = this.take(8).readBigUInt64LE(0);
    const hi = this.take(8).readBigUInt64LE(0);
    return lo | (hi << 64n);
  }
  bool(): boolean { return this.u8() !== 0; }
  pubkey(): Address { return addrDec.decode(this.take(32)); }
  /** Option<T>: 0x00 = None, 0x01 = Some, mirroring `Borsh.optionPubkey`'s write side. */
  optionPubkey(): Address | null { return this.u8() === 0 ? null : this.pubkey(); }
  optionU16(): number | null { return this.u8() === 0 ? null : this.u16(); }
  /** Borsh `String`: u32 byte-length prefix ‖ UTF-8 bytes. */
  string(): string {
    const len = this.u32();
    return this.take(len).toString("utf8");
  }
  /** Fixed-size array `[u64; N]`: elements only, no length prefix (unlike Vec<T>). */
  arrayU64(len: number): bigint[] {
    return Array.from({ length: len }, () => this.u64());
  }
  /** Fixed-size array `[u32; N]`: elements only, no length prefix (unlike Vec<T>). */
  arrayU32(len: number): number[] {
    return Array.from({ length: len }, () => this.u32());
  }
  /** Fixed-size raw byte array — for an opaque `[u8; N]` that is not a `Pubkey` (e.g.
   * `RollBatch.caller_seed`, `RollBatch.randomness`), so a decoder does not imply base58
   * pubkey semantics where the Rust type carries none. */
  bytes(n: number): Uint8Array {
    return new Uint8Array(this.take(n));
  }
  /** Throws unless every byte has been consumed — a decoder that stops short of the account's or
   * event's own end would silently drop a trailing field a Rust-side addition appended. */
  assertConsumed(): void {
    if (this.offset !== this.buf.length) {
      throw new Error(`${this.buf.length - this.offset} unconsumed byte(s) after decode`);
    }
  }
}

function checkAccountDiscriminator(data: Buffer | Uint8Array, name: string): BorshReader {
  const buf = Buffer.from(data);
  const actual = buf.subarray(0, 8);
  const expected = accountDiscriminator(name);
  if (!actual.equals(expected)) {
    throw new Error(
      `${name}: account discriminator mismatch (got ${actual.toString("hex")}, want ${expected.toString("hex")})`,
    );
  }
  return new BorshReader(buf.subarray(8));
}

/** Events are plain `emit!`, not `emit_cpi!` — the payload is the event's own bytes with no
 * self-CPI instruction-data wrapper, so this reads the same as an account's own discriminator
 * prefix, just from `sha256("event:<PascalCaseName>")` instead of `"account:<PascalCaseName>"`. */
function checkEventDiscriminator(data: Buffer | Uint8Array, name: string): BorshReader {
  const buf = Buffer.from(data);
  const actual = buf.subarray(0, 8);
  const expected = eventDiscriminator(name);
  if (!actual.equals(expected)) {
    throw new Error(
      `${name}: event discriminator mismatch (got ${actual.toString("hex")}, want ${expected.toString("hex")})`,
    );
  }
  return new BorshReader(buf.subarray(8));
}

// ── Fieldless / tagged enums ─────────────────────────────────────────────────
// Each enum's wire byte is its *declaration order* in the Rust source, not the IDL's alphabetical
// listing — verified at `state/position.rs` and `common/events.rs` respectively.

export type PositionState = "Pending" | "Active" | "ClosedBelowFloor" | "Rejected" | "Seized";

/** `PositionSeized`'s cause tag. `Burned`/`Transferred` are *gone*; `Frozen` and `Undecidable`
 * both leave the card in the vault — four variants because the remedies differ, and the byte is
 * the only record of which one happened. Pinned at `common/events.rs`'s declaration.
 *
 * `Undecidable` is the open-ended one. The other three are facts read off the asset account; this
 * one is the absence of a fact — the asset's or its collection's plugin registry holds a record
 * whose effect on a transfer the program cannot decide: a `Royalties` rule set, an external
 * `Oracle` or `LifecycleHook` adapter that declares a Transfer check, a `BubblegumV2` plugin, or a
 * `plugin_type` byte a later `mpl-core` ships. It is not a claim that the transfer would fail, so
 * an indexer must not report it as one; a position seized on this cause may well have had a
 * movable card, and its claim is callable either way. */
export type SeizureKind = "Burned" | "Transferred" | "Frozen" | "Undecidable";
const SEIZURE_KIND_BY_TAG: readonly SeizureKind[] = [
  "Burned",
  "Transferred",
  "Frozen",
  "Undecidable",
];
function decodeSeizureKind(r: BorshReader): SeizureKind {
  const tag = r.u8();
  const kind = SEIZURE_KIND_BY_TAG[tag];
  if (kind === undefined) throw new Error(`unknown SeizureKind tag: ${tag}`);
  return kind;
}
const POSITION_STATE_BY_TAG: readonly PositionState[] = [
  "Pending",
  "Active",
  "ClosedBelowFloor",
  "Rejected",
  "Seized",
];
function decodePositionState(r: BorshReader): PositionState {
  const tag = r.u8();
  const state = POSITION_STATE_BY_TAG[tag];
  if (state === undefined) throw new Error(`unknown PositionState tag: ${tag}`);
  return state;
}

/** What a settled roll drew. `None` is what a pending or refunded roll carries — a refunded roll
 * has no outcome, which is not the same as having drawn a blank, so an indexer must not fold the
 * two together. */
export type RollOutcome = "None" | "RealCard" | "Blank";
const ROLL_OUTCOME_BY_TAG: readonly RollOutcome[] = ["None", "RealCard", "Blank"];
function decodeRollOutcome(r: BorshReader): RollOutcome {
  const tag = r.u8();
  const outcome = ROLL_OUTCOME_BY_TAG[tag];
  if (outcome === undefined) throw new Error(`unknown RollOutcome tag: ${tag}`);
  return outcome;
}

/** Where a batch sits in its lifecycle. Pinned at `state/roll_batch.rs`'s declaration order. */
export type BatchState = "Committed" | "Resolved" | "Recovered" | "Complete";
const BATCH_STATE_BY_TAG: readonly BatchState[] = ["Committed", "Resolved", "Recovered", "Complete"];
function decodeBatchState(r: BorshReader): BatchState {
  const tag = r.u8();
  const state = BATCH_STATE_BY_TAG[tag];
  if (state === undefined) throw new Error(`unknown BatchState tag: ${tag}`);
  return state;
}

/** Whether one roll of a batch has reached a terminal, and which. Pinned at the same
 * declaration as `BatchState`. */
export type RollStatus = "Pending" | "Settled" | "Refunded";
const ROLL_STATUS_BY_TAG: readonly RollStatus[] = ["Pending", "Settled", "Refunded"];
function decodeRollStatus(r: BorshReader): RollStatus {
  const tag = r.u8();
  const status = ROLL_STATUS_BY_TAG[tag];
  if (status === undefined) throw new Error(`unknown RollStatus tag: ${tag}`);
  return status;
}

// `T05` decodes tag 2, the tag Treasury 03's authority carried before that role was retired and
// this one took the slot. Dropping a variant rather than reusing it would re-read every `T02`
// rotation as something else. A historical `AuthorityRotated` from before the reuse decodes here
// as `T05`, which is a dating problem rather than an ambiguity — no other tag moved.
export type AuthorityRole = "Administrator" | "Operator" | "T05" | "T02";
const AUTHORITY_ROLE_BY_TAG: readonly AuthorityRole[] = ["Administrator", "Operator", "T05", "T02"];
function decodeAuthorityRole(r: BorshReader): AuthorityRole {
  const tag = r.u8();
  const role = AUTHORITY_ROLE_BY_TAG[tag];
  if (role === undefined) throw new Error(`unknown AuthorityRole tag: ${tag}`);
  return role;
}

// ── Blank slots ──────────────────────────────────────────────────────────────
/** Blank-face slots per pool: 0–2 hold faces below the ticket, 3 the one face above it (0 = none),
 * and 4–7 are reserved at 0. Every per-slot array on `Pool`, `RollBatch`, `RollRecord` and the
 * events carrying them is this long. */
export const BLANK_SLOTS = 8;
/** Slots `0..BELOW_TICKET_SLOTS` hold the faces below the ticket. */
export const BELOW_TICKET_SLOTS = 3;
/** The one slot holding a face above the ticket; 0 means none. */
export const ABOVE_TICKET_SLOT = 3;
/** The least the above-ticket face (slot 3) may sit over the ticket, in USDC base units. */
export const MIN_ABOVE_TICKET_GAP = 1_000_000n;
/** Blank slots in the layout `Pool` and `RollBatch` had before the eight-slot upgrade. */
const LEGACY_BLANK_SLOTS = 3;

/** The program's `slot_is_empty`: slot 3 or a reserved slot with a face of 0 has no count, weight
 * or draw. Slots 0–2 always hold a face, so a 0 there is never empty — it reaches `weightOf` and
 * throws, as the program fails. */
export function slotIsEmpty(slot: number, face: bigint): boolean {
  return slot >= ABOVE_TICKET_SLOT && face === 0n;
}
export type BlankFaces = [bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint];
export type BlankCounts = [number, number, number, number, number, number, number, number];

export function readBlankFaces(r: BorshReader): BlankFaces {
  return [r.u64(), r.u64(), r.u64(), r.u64(), r.u64(), r.u64(), r.u64(), r.u64()];
}

export function readBlankCounts(r: BorshReader): BlankCounts {
  return [r.u32(), r.u32(), r.u32(), r.u32(), r.u32(), r.u32(), r.u32(), r.u32()];
}

/** `ConfigChanged`'s payload tag. Variant order is load-bearing: `Hours` is index **4**, not
 * `Bps` — decoding an hour count as basis points publishes a wrong unit inside the audit record
 * the invariant exists to make trustworthy. Pinned at `common/events.rs`'s declaration, not the
 * IDL's alphabetical listing. */
export type ConfigValue =
  | { kind: "Amount"; value: bigint }
  | { kind: "Bps"; value: number }
  | { kind: "Count"; value: number }
  | { kind: "Faces"; value: BlankFaces }
  | { kind: "Hours"; value: number };

function decodeConfigValue(r: BorshReader): ConfigValue {
  const tag = r.u8();
  switch (tag) {
    case 0: return { kind: "Amount", value: r.u64() };
    case 1: return { kind: "Bps", value: r.u16() };
    case 2: return { kind: "Count", value: r.u8() };
    case 3: return { kind: "Faces", value: readBlankFaces(r) };
    case 4: return { kind: "Hours", value: r.u16() };
    default: throw new Error(`unknown ConfigValue tag: ${tag}`);
  }
}

// ── Accounts ─────────────────────────────────────────────────────────────────

export interface ProtocolConfig {
  administrator: Address;
  /** Treasury 04's protocol-wide ceiling, in USDC base units; 0 until the Administrator sets it. */
  t04Ceiling: bigint;
  /** The `reserved*` fields are never read or written; they keep the account layout. */
  reserved0: Uint8Array;
  reserved1: Address;
  reserved2: Address;
  reserved3: Address;
  operator: Address;
  /** Retired; kept because the deployed account's layout is. */
  t05Authority: Address;
  t02Authority: Address;
  protocolRevenue: Address;
  usdcMint: Address;
  byeMint: Address;
  vrfProgram: Address;
  oracleQueue: Address;
  swapPool: Address;
  maxSwapSlippageBps: number;
  poolCounter: number;
  bump: number;
}

export function decodeProtocolConfig(data: Buffer | Uint8Array): ProtocolConfig {
  const r = checkAccountDiscriminator(data, "ProtocolConfig");
  const config: ProtocolConfig = {
    administrator: r.pubkey(),
    t04Ceiling: r.u64(),
    reserved0: r.bytes(24),
    reserved1: r.pubkey(),
    reserved2: r.pubkey(),
    reserved3: r.pubkey(),
    operator: r.pubkey(),
    t05Authority: r.pubkey(),
    t02Authority: r.pubkey(),
    protocolRevenue: r.pubkey(),
    usdcMint: r.pubkey(),
    byeMint: r.pubkey(),
    vrfProgram: r.pubkey(),
    oracleQueue: r.pubkey(),
    swapPool: r.pubkey(),
    maxSwapSlippageBps: r.u16(),
    poolCounter: r.u16(),
    bump: r.u8(),
  };
  r.assertConsumed();
  return config;
}

export interface Pool {
  poolId: number;
  weightIndex: Address;
  treasury03: Address;
  price: bigint;
  ticketTarget: bigint;
  fee: bigint;
  allocEqualBps: number;
  allocTierBps: number;
  allocProtocolBps: number;
  admissionFloor: bigint;
  admissionCeiling: bigint;
  walletValueCap: bigint;
  maxN: number;
  buybackRateBps: number;
  blankFaces: BlankFaces;
  t04Ceiling: bigint;
  sweepCadenceHours: number;
  tierSize: number;
  depositsPaused: boolean;
  rollsPaused: boolean;
  pauseReason: number;
  nReal: number;
  blanks: BlankCounts;
  wReal: bigint;
  openBatches: number;
  batchCounter: bigint;
  positionCounter: bigint;
  pendingRollLiability: bigint;
  owedFees: bigint;
  accEqual: bigint;
  sweepPending: boolean;
  sweepEpoch: bigint;
  lastSweepAt: bigint;
  sweepUpdates: number;
  bump: number;
  escrowBidBps: number;
  /** The shared Treasury 04; the default address on a pool not yet consolidated. */
  treasury04: Address;
  reserved: Uint8Array;
}

/** A pool's account length, discriminator included, in today's eight-slot layout. */
export const POOL_LEN = 402;
/** A pool's account length in the three-slot layout every pool had before the eight-slot upgrade,
 * until `migrate_pool` grows it. */
export const LEGACY_POOL_LEN = 342;

/**
 * Decodes a pool in either layout, chosen by length: 402 bytes is today's, 342 the three-slot one,
 * whose blank arrays come back widened to eight slots with zeros. Any other length is refused
 * rather than read against the wrong layout — the arrays sit mid-account, so a misread would shift
 * every later field without failing.
 */
export function decodePool(data: Buffer | Uint8Array): Pool {
  let slots: number;
  if (data.length === POOL_LEN) slots = BLANK_SLOTS;
  else if (data.length === LEGACY_POOL_LEN) slots = LEGACY_BLANK_SLOTS;
  else throw new Error(`Pool is ${data.length} bytes, expected ${POOL_LEN} or ${LEGACY_POOL_LEN}`);
  const r = checkAccountDiscriminator(data, "Pool");
  const pool: Pool = {
    poolId: r.u16(),
    weightIndex: r.pubkey(),
    treasury03: r.pubkey(),
    price: r.u64(),
    ticketTarget: r.u64(),
    fee: r.u64(),
    allocEqualBps: r.u16(),
    allocTierBps: r.u16(),
    allocProtocolBps: r.u16(),
    admissionFloor: r.u64(),
    admissionCeiling: r.u64(),
    walletValueCap: r.u64(),
    maxN: r.u8(),
    buybackRateBps: r.u16(),
    blankFaces: readFacesWidened(r, slots),
    t04Ceiling: r.u64(),
    sweepCadenceHours: r.u16(),
    tierSize: r.u8(),
    depositsPaused: r.bool(),
    rollsPaused: r.bool(),
    pauseReason: r.u16(),
    nReal: r.u32(),
    blanks: readCountsWidened(r, slots),
    wReal: r.u128(),
    openBatches: r.u32(),
    batchCounter: r.u64(),
    positionCounter: r.u64(),
    pendingRollLiability: r.u64(),
    owedFees: r.u64(),
    accEqual: r.u128(),
    sweepPending: r.bool(),
    sweepEpoch: r.u64(),
    lastSweepAt: r.i64(),
    sweepUpdates: r.u32(),
    bump: r.u8(),
    escrowBidBps: r.u16(),
    treasury04: r.pubkey(),
    reserved: r.bytes(32),
  };
  r.assertConsumed();
  return pool;
}

/** The account bytes of `pool`, discriminator included — `decodePool`'s inverse, for fixtures that
 * must hold a whole, current-layout pool rather than a hand-cut prefix of one. */
export function encodePool(pool: Pool): Buffer {
  const body = new Borsh()
    .u16(pool.poolId)
    .pubkey(pool.weightIndex)
    .pubkey(pool.treasury03)
    .u64(pool.price)
    .u64(pool.ticketTarget)
    .u64(pool.fee)
    .u16(pool.allocEqualBps)
    .u16(pool.allocTierBps)
    .u16(pool.allocProtocolBps)
    .u64(pool.admissionFloor)
    .u64(pool.admissionCeiling)
    .u64(pool.walletValueCap)
    .u8(pool.maxN)
    .u16(pool.buybackRateBps)
    .arrayU64(pool.blankFaces, BLANK_SLOTS)
    .u64(pool.t04Ceiling)
    .u16(pool.sweepCadenceHours)
    .u8(pool.tierSize)
    .bool(pool.depositsPaused)
    .bool(pool.rollsPaused)
    .u16(pool.pauseReason)
    .u32(pool.nReal)
    .arrayU32(pool.blanks, BLANK_SLOTS)
    .u128(pool.wReal)
    .u32(pool.openBatches)
    .u64(pool.batchCounter)
    .u64(pool.positionCounter)
    .u64(pool.pendingRollLiability)
    .u64(pool.owedFees)
    .u128(pool.accEqual)
    .bool(pool.sweepPending)
    .u64(pool.sweepEpoch)
    .i64(pool.lastSweepAt)
    .u32(pool.sweepUpdates)
    .u8(pool.bump)
    .u16(pool.escrowBidBps)
    .pubkey(pool.treasury04)
    .bytes(pool.reserved)
    .build();
  return Buffer.concat([accountDiscriminator("Pool"), body]);
}

export interface Position {
  pool: Address;
  depositor: Address;
  nftMint: Address;
  positionId: bigint;
  state: PositionState;
  recordedValue: bigint;
  valueObservedAt: bigint;
  depositValue: bigint;
  slotIndex: number;
  activatedAt: bigint;
  equalCheckpoint: bigint;
  accrued: bigint;
  inTier: boolean;
  tierCheckpoint: bigint;
  lockUntil: bigint;
  rejectReason: number;
  standard: number;
  bump: number;
  vaultBump: number;
}

export function decodePosition(data: Buffer | Uint8Array): Position {
  const r = checkAccountDiscriminator(data, "Position");
  const position: Position = {
    pool: r.pubkey(),
    depositor: r.pubkey(),
    nftMint: r.pubkey(),
    positionId: r.u64(),
    state: decodePositionState(r),
    recordedValue: r.u64(),
    valueObservedAt: r.i64(),
    depositValue: r.u64(),
    slotIndex: r.u32(),
    activatedAt: r.i64(),
    equalCheckpoint: r.u128(),
    accrued: r.u64(),
    inTier: r.bool(),
    tierCheckpoint: r.u128(),
    lockUntil: r.i64(),
    rejectReason: r.u16(),
    standard: r.u8(),
    bump: r.u8(),
    vaultBump: r.u8(),
  };
  r.assertConsumed();
  return position;
}

export interface PoolCollection {
  pool: Address;
  collection: Address;
  standards: number;
  admittedAt: bigint;
  bump: number;
}

export function decodePoolCollection(data: Buffer | Uint8Array): PoolCollection {
  const r = checkAccountDiscriminator(data, "PoolCollection");
  const record: PoolCollection = {
    pool: r.pubkey(),
    collection: r.pubkey(),
    standards: r.u8(),
    admittedAt: r.i64(),
    bump: r.u8(),
  };
  r.assertConsumed();
  return record;
}

export interface WalletStats {
  pool: Address;
  owner: Address;
  activePositions: number;
  activeValue: bigint;
  bump: number;
}

/** A wallet's escrow. It stores no balance: the balance is the token amount of its vault. */
export interface Escrow {
  owner: Address;
  armed: boolean;
  bump: number;
  vaultBump: number;
  reserved: Uint8Array;
}

export function decodeEscrow(data: Buffer | Uint8Array): Escrow {
  const r = checkAccountDiscriminator(data, "Escrow");
  const escrow: Escrow = {
    owner: r.pubkey(),
    armed: r.bool(),
    bump: r.u8(),
    vaultBump: r.u8(),
    reserved: r.bytes(64),
  };
  r.assertConsumed();
  return escrow;
}

export function decodeWalletStats(data: Buffer | Uint8Array): WalletStats {
  const r = checkAccountDiscriminator(data, "WalletStats");
  const stats: WalletStats = {
    pool: r.pubkey(),
    owner: r.pubkey(),
    activePositions: r.u32(),
    activeValue: r.u64(),
    bump: r.u8(),
  };
  r.assertConsumed();
  return stats;
}

export interface TierEntry {
  position: Address;
  value: bigint;
  activatedAt: bigint;
  positionId: bigint;
}

function decodeTierEntry(r: BorshReader): TierEntry {
  return {
    position: r.pubkey(),
    value: r.u64(),
    activatedAt: r.i64(),
    positionId: r.u64(),
  };
}

const TOP_TIER_CAPACITY = 20;

export interface TopTier {
  pool: Address;
  len: number;
  /** Only `entries[0..len]` are live; `[TierEntry; 20]`'s slots past `len` are stale and are not
   * included here — returning all 20 would surface stale members as live tier data. */
  entries: TierEntry[];
  accTier: bigint;
  bump: number;
}

export function decodeTopTier(data: Buffer | Uint8Array): TopTier {
  const r = checkAccountDiscriminator(data, "TopTier");
  const pool = r.pubkey();
  const len = r.u8();
  if (len > TOP_TIER_CAPACITY) {
    throw new Error(`TopTier.len ${len} exceeds the fixed capacity ${TOP_TIER_CAPACITY}`);
  }
  const allEntries: TierEntry[] = [];
  for (let i = 0; i < TOP_TIER_CAPACITY; i++) allEntries.push(decodeTierEntry(r));
  const accTier = r.u128();
  const bump = r.u8();
  r.assertConsumed();
  return { pool, len, entries: allEntries.slice(0, len), accTier, bump };
}

/** `WeightIndex`'s Fenwick tree + free-slot stack capacity — `FENWICK_CAPACITY` in
 * `common/constants.rs`. */
export const FENWICK_CAPACITY = 16_384;

export interface WeightIndex {
  pool: Address;
  totalWeight: bigint;
  highWater: number;
  freeCount: number;
  tree: bigint[];
  freeStack: number[];
}

// `WeightIndex` is `#[account(zero_copy)]`/`#[repr(C)]` — `bytemuck` bytes, not borsh framing —
// so it is read at pinned byte offsets, never with a sequential borsh cursor. The offsets below
// mirror `weight_index_field_order`'s pin in `state/weight_index.rs` exactly: every header field
// here happens to already sit at these offsets under a sequential reader too (nothing in this
// particular struct needs `repr(C)` padding beyond the explicit `_padding` field), but that is
// this struct's own layout, not something a reader is entitled to assume of `repr(C)` in general.
const WI_TOTAL_WEIGHT_OFFSET = 32;
const WI_HIGH_WATER_OFFSET = 40;
const WI_FREE_COUNT_OFFSET = 44;
const WI_TREE_OFFSET = 56; // 32 (pool) + 8 (total_weight) + 4 + 4 (high_water, free_count) + 8 (_padding)
const WI_FREE_STACK_OFFSET = WI_TREE_OFFSET + 8 * FENWICK_CAPACITY;
const WI_STRUCT_SIZE = WI_FREE_STACK_OFFSET + 4 * FENWICK_CAPACITY;
const WI_ACCOUNT_SIZE = 8 + WI_STRUCT_SIZE; // 8-byte Anchor discriminator ‖ struct

export function decodeWeightIndex(data: Buffer | Uint8Array): WeightIndex {
  const buf = Buffer.from(data);
  const actual = buf.subarray(0, 8);
  const expected = accountDiscriminator("WeightIndex");
  if (!actual.equals(expected)) {
    throw new Error(
      `WeightIndex: account discriminator mismatch (got ${actual.toString("hex")}, want ${expected.toString("hex")})`,
    );
  }
  if (buf.length !== WI_ACCOUNT_SIZE) {
    throw new Error(`WeightIndex: expected ${WI_ACCOUNT_SIZE} bytes, got ${buf.length}`);
  }
  const body = buf.subarray(8);

  const pool = addrDec.decode(body.subarray(0, 32));
  const totalWeight = body.readBigUInt64LE(WI_TOTAL_WEIGHT_OFFSET);
  const highWater = body.readUInt32LE(WI_HIGH_WATER_OFFSET);
  const freeCount = body.readUInt32LE(WI_FREE_COUNT_OFFSET);

  const tree: bigint[] = new Array(FENWICK_CAPACITY);
  for (let i = 0; i < FENWICK_CAPACITY; i++) {
    tree[i] = body.readBigUInt64LE(WI_TREE_OFFSET + i * 8);
  }
  const freeStack: number[] = new Array(FENWICK_CAPACITY);
  for (let i = 0; i < FENWICK_CAPACITY; i++) {
    freeStack[i] = body.readUInt32LE(WI_FREE_STACK_OFFSET + i * 4);
  }

  return { pool, totalWeight, highWater, freeCount, tree, freeStack };
}

/** One roll's commitment and result, inlined ten times into `RollBatch`. Not an account or event
 * in its own right — there is no discriminator to check, so this is a plain field-order reader
 * rather than an exported `decode*` entry point. */
export interface RollRecord {
  status: RollStatus;
  outcome: RollOutcome;
  /** `Pubkey.default()` when the roll drew a blank or has not settled. */
  selectedMint: Address;
  blankFaceIdx: number;
  drawValue: bigint;
  attempts: number;
  wAtDraw: bigint;
  nRealAtDraw: number;
  blanksAtDraw: BlankCounts;
  /** `0` = settled; non-zero identifies which refund branch was taken. */
  refundReason: number;
}

/** `blanks` read as `slots` counts and widened to `BLANK_SLOTS` with zeros. */
function readCountsWidened(r: BorshReader, slots: number): BlankCounts {
  const counts: BlankCounts = [0, 0, 0, 0, 0, 0, 0, 0];
  for (let i = 0; i < slots; i += 1) counts[i] = r.u32();
  return counts;
}

/** `faces` read as `slots` faces and widened to `BLANK_SLOTS` with zeros. */
function readFacesWidened(r: BorshReader, slots: number): BlankFaces {
  const faces: BlankFaces = [0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n];
  for (let i = 0; i < slots; i += 1) faces[i] = r.u64();
  return faces;
}

function decodeRollRecord(r: BorshReader, slots: number = BLANK_SLOTS): RollRecord {
  return {
    status: decodeRollStatus(r),
    outcome: decodeRollOutcome(r),
    selectedMint: r.pubkey(),
    blankFaceIdx: r.u8(),
    drawValue: r.u128(),
    attempts: r.u8(),
    wAtDraw: r.u128(),
    nRealAtDraw: r.u32(),
    blanksAtDraw: readCountsWidened(r, slots),
    refundReason: r.u16(),
  };
}

/** How many `RollRecord`s a batch carries — `[RollRecord; 10]` in `state/roll_batch.rs`, and
 * `max_n`'s hard ceiling (`common/constants.rs`'s `MAX_BATCH_CAPACITY`). */
export const ROLL_BATCH_CAPACITY = 10;

export interface RollBatch {
  pool: Address;
  roller: Address;
  batchId: bigint;
  state: BatchState;
  n: number;
  nextRoll: number;
  settled: number;
  refunded: number;
  requestSlot: bigint;
  /** The batch PDA's own bytes, not a `Pubkey` — the value `recover_batch`'s queue scan matches
   * `callback_args` on. */
  callerSeed: Uint8Array;
  /** Written exactly once, by `vrf_callback`. All-zero before delivery. */
  randomness: Uint8Array;
  expectedW: bigint;
  expectedBlanks: BlankCounts;
  sPrice: bigint;
  sTicket: bigint;
  sFee: bigint;
  sAllocBps: [number, number, number];
  sBlankFaces: BlankFaces;
  rolls: RollRecord[];
  bump: number;
}

/** A batch's account length, discriminator included, in today's eight-slot layout. */
export const ROLL_BATCH_LEN = 1_360;
/** A batch's account length in the three-slot layout every batch before the eight-slot upgrade
 * was written in. Such a batch can still be read and closed. */
export const LEGACY_ROLL_BATCH_LEN = 1_100;
/** One `RollRecord`'s length in the three-slot layout. */
export const LEGACY_ROLL_RECORD_LEN = 86;

/**
 * Decodes a batch in either layout, chosen by length: 1,360 bytes is today's, 1,100 the
 * three-slot one, whose blank arrays come back widened to eight slots with zeros. Any other
 * length is refused rather than read against the wrong layout — the arrays sit mid-record, so a
 * misread would shift every later field without failing.
 */
export function decodeRollBatch(data: Buffer | Uint8Array): RollBatch {
  let slots: number;
  if (data.length === ROLL_BATCH_LEN) slots = BLANK_SLOTS;
  else if (data.length === LEGACY_ROLL_BATCH_LEN) slots = LEGACY_BLANK_SLOTS;
  else {
    throw new Error(
      `RollBatch is ${data.length} bytes, expected ${ROLL_BATCH_LEN} or ${LEGACY_ROLL_BATCH_LEN}`,
    );
  }
  const r = checkAccountDiscriminator(data, "RollBatch");
  const batch: RollBatch = {
    pool: r.pubkey(),
    roller: r.pubkey(),
    batchId: r.u64(),
    state: decodeBatchState(r),
    n: r.u8(),
    nextRoll: r.u8(),
    settled: r.u8(),
    refunded: r.u8(),
    requestSlot: r.u64(),
    callerSeed: r.bytes(32),
    randomness: r.bytes(32),
    expectedW: r.u128(),
    expectedBlanks: readCountsWidened(r, slots),
    sPrice: r.u64(),
    sTicket: r.u64(),
    sFee: r.u64(),
    sAllocBps: [r.u16(), r.u16(), r.u16()],
    sBlankFaces: readFacesWidened(r, slots),
    rolls: Array.from({ length: ROLL_BATCH_CAPACITY }, () => decodeRollRecord(r, slots)),
    bump: r.u8(),
  };
  r.assertConsumed();
  return batch;
}

// ── Events ───────────────────────────────────────────────────────────────────

export interface ProtocolInitialized {
  slot: bigint;
  administrator: Address;
  operator: Address;
  t02Authority: Address;
  /** Retired; always the default on a fresh init, kept for the emitted wire shape. */
  t05Authority: Address;
  protocolRevenue: Address;
  usdcMint: Address;
  byeMint: Address;
  vrfProgram: Address;
  oracleQueue: Address;
  swapPool: Address;
  maxSwapSlippageBps: number;
}

export function decodeProtocolInitialized(data: Buffer | Uint8Array): ProtocolInitialized {
  const r = checkEventDiscriminator(data, "ProtocolInitialized");
  const event: ProtocolInitialized = {
    slot: r.u64(),
    administrator: r.pubkey(),
    operator: r.pubkey(),
    t02Authority: r.pubkey(),
    t05Authority: r.pubkey(),
    protocolRevenue: r.pubkey(),
    usdcMint: r.pubkey(),
    byeMint: r.pubkey(),
    vrfProgram: r.pubkey(),
    oracleQueue: r.pubkey(),
    swapPool: r.pubkey(),
    maxSwapSlippageBps: r.u16(),
  };
  r.assertConsumed();
  return event;
}

export interface PoolInitialized {
  pool: Address;
  slot: bigint;
  authority: Address;
  poolId: number;
  weightIndex: Address;
  treasury03: Address;
  price: bigint;
  ticketTarget: bigint;
  fee: bigint;
  allocEqualBps: number;
  allocTierBps: number;
  allocProtocolBps: number;
  admissionFloor: bigint;
  admissionCeiling: bigint;
  walletValueCap: bigint;
  maxN: number;
  buybackRateBps: number;
  blankFaces: BlankFaces;
  t04Ceiling: bigint;
  sweepCadenceHours: number;
  tierSize: number;
}

export function decodePoolInitialized(data: Buffer | Uint8Array): PoolInitialized {
  const r = checkEventDiscriminator(data, "PoolInitialized");
  const event: PoolInitialized = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    poolId: r.u16(),
    weightIndex: r.pubkey(),
    treasury03: r.pubkey(),
    price: r.u64(),
    ticketTarget: r.u64(),
    fee: r.u64(),
    allocEqualBps: r.u16(),
    allocTierBps: r.u16(),
    allocProtocolBps: r.u16(),
    admissionFloor: r.u64(),
    admissionCeiling: r.u64(),
    walletValueCap: r.u64(),
    maxN: r.u8(),
    buybackRateBps: r.u16(),
    blankFaces: readBlankFaces(r),
    t04Ceiling: r.u64(),
    sweepCadenceHours: r.u16(),
    tierSize: r.u8(),
  };
  r.assertConsumed();
  return event;
}

export interface ConfigChanged {
  pool: Address;
  slot: bigint;
  authority: Address;
  param: string;
  old: ConfigValue;
  new: ConfigValue;
}

export function decodeConfigChanged(data: Buffer | Uint8Array): ConfigChanged {
  const r = checkEventDiscriminator(data, "ConfigChanged");
  const event: ConfigChanged = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    param: r.string(),
    old: decodeConfigValue(r),
    new: decodeConfigValue(r),
  };
  r.assertConsumed();
  return event;
}

export interface VrfConfigChanged {
  slot: bigint;
  authority: Address;
  oldVrfProgram: Address;
  oldOracleQueue: Address;
  oldSwapPool: Address;
  vrfProgram: Address;
  oracleQueue: Address;
  swapPool: Address;
  oldMaxSwapSlippageBps: number;
  maxSwapSlippageBps: number;
}

export function decodeVrfConfigChanged(data: Buffer | Uint8Array): VrfConfigChanged {
  const r = checkEventDiscriminator(data, "VrfConfigChanged");
  const event: VrfConfigChanged = {
    slot: r.u64(),
    authority: r.pubkey(),
    oldVrfProgram: r.pubkey(),
    oldOracleQueue: r.pubkey(),
    oldSwapPool: r.pubkey(),
    vrfProgram: r.pubkey(),
    oracleQueue: r.pubkey(),
    swapPool: r.pubkey(),
    oldMaxSwapSlippageBps: r.u16(),
    maxSwapSlippageBps: r.u16(),
  };
  r.assertConsumed();
  return event;
}

export interface ByeMintChanged { slot: bigint; authority: Address; oldByeMint: Address; byeMint: Address; }
export function decodeByeMintChanged(data: Buffer | Uint8Array): ByeMintChanged {
  const r = checkEventDiscriminator(data, "ByeMintChanged");
  const event = { slot: r.u64(), authority: r.pubkey(), oldByeMint: r.pubkey(), byeMint: r.pubkey() };
  r.assertConsumed();
  return event;
}

export interface SwapPoolChanged { slot: bigint; authority: Address; oldSwapPool: Address; swapPool: Address; }
export function decodeSwapPoolChanged(data: Buffer | Uint8Array): SwapPoolChanged {
  const r = checkEventDiscriminator(data, "SwapPoolChanged");
  const event = { slot: r.u64(), authority: r.pubkey(), oldSwapPool: r.pubkey(), swapPool: r.pubkey() };
  r.assertConsumed();
  return event;
}

export interface ProtocolRevenueChanged { slot: bigint; authority: Address; oldProtocolRevenue: Address; protocolRevenue: Address; }
export function decodeProtocolRevenueChanged(data: Buffer | Uint8Array): ProtocolRevenueChanged {
  const r = checkEventDiscriminator(data, "ProtocolRevenueChanged");
  const event = { slot: r.u64(), authority: r.pubkey(), oldProtocolRevenue: r.pubkey(), protocolRevenue: r.pubkey() };
  r.assertConsumed();
  return event;
}

export interface PauseSet {
  pool: Address;
  slot: bigint;
  authority: Address;
  depositsPaused: boolean;
  rollsPaused: boolean;
  reason: number;
}

export function decodePauseSet(data: Buffer | Uint8Array): PauseSet {
  const r = checkEventDiscriminator(data, "PauseSet");
  const event: PauseSet = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    depositsPaused: r.bool(),
    rollsPaused: r.bool(),
    reason: r.u16(),
  };
  r.assertConsumed();
  return event;
}

export interface AuthorityRotated {
  slot: bigint;
  authority: Address;
  role: AuthorityRole;
  old: Address;
  new: Address;
}

export function decodeAuthorityRotated(data: Buffer | Uint8Array): AuthorityRotated {
  const r = checkEventDiscriminator(data, "AuthorityRotated");
  const event: AuthorityRotated = {
    slot: r.u64(),
    authority: r.pubkey(),
    role: decodeAuthorityRole(r),
    old: r.pubkey(),
    new: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

/** `standard` is read **last**, mirroring its appended position on the wire: every field before
 * it sits at the offset a pre-`standard` decoder already expected. It is the only field that
 * tells a `deposit` intake from a `deposit_core` one — the other five are identical at both
 * emit sites. */
export interface DepositPending {
  pool: Address;
  slot: bigint;
  position: Address;
  depositor: Address;
  nftMint: Address;
  standard: number;
}

export function decodeDepositPending(data: Buffer | Uint8Array): DepositPending {
  const r = checkEventDiscriminator(data, "DepositPending");
  const event: DepositPending = {
    pool: r.pubkey(),
    slot: r.u64(),
    position: r.pubkey(),
    depositor: r.pubkey(),
    nftMint: r.pubkey(),
    standard: r.u8(),
  };
  r.assertConsumed();
  return event;
}

export interface DepositApproved {
  pool: Address;
  slot: bigint;
  authority: Address;
  position: Address;
  value: bigint;
  observedAt: bigint;
  observedAgeSeconds: bigint;
}

export function decodeDepositApproved(data: Buffer | Uint8Array): DepositApproved {
  const r = checkEventDiscriminator(data, "DepositApproved");
  const event: DepositApproved = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    position: r.pubkey(),
    value: r.u64(),
    observedAt: r.i64(),
    observedAgeSeconds: r.i64(),
  };
  r.assertConsumed();
  return event;
}

export interface RebalanceEvaluated {
  pool: Address;
  slot: bigint;
  nReal: number;
  wReal: bigint;
  blanksBefore: BlankCounts;
  blanksAfter: BlankCounts;
}

export function decodeRebalanceEvaluated(data: Buffer | Uint8Array): RebalanceEvaluated {
  const r = checkEventDiscriminator(data, "RebalanceEvaluated");
  const event: RebalanceEvaluated = {
    pool: r.pubkey(),
    slot: r.u64(),
    nReal: r.u32(),
    wReal: r.u128(),
    blanksBefore: readBlankCounts(r),
    blanksAfter: readBlankCounts(r),
  };
  r.assertConsumed();
  return event;
}

export interface TierChanged {
  pool: Address;
  slot: bigint;
  left: Address | null;
  entered: Address | null;
}

export function decodeTierChanged(data: Buffer | Uint8Array): TierChanged {
  const r = checkEventDiscriminator(data, "TierChanged");
  const event: TierChanged = {
    pool: r.pubkey(),
    slot: r.u64(),
    left: r.optionPubkey(),
    entered: r.optionPubkey(),
  };
  r.assertConsumed();
  return event;
}

export interface DepositRejected {
  pool: Address;
  slot: bigint;
  authority: Address;
  position: Address;
  reason: number;
}

export function decodeDepositRejected(data: Buffer | Uint8Array): DepositRejected {
  const r = checkEventDiscriminator(data, "DepositRejected");
  const event: DepositRejected = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    position: r.pubkey(),
    reason: r.u16(),
  };
  r.assertConsumed();
  return event;
}

export interface CollectionAdmitted {
  pool: Address;
  slot: bigint;
  collection: Address;
  standards: number;
  oldStandards: number;
  reason: number | null;
  authority: Address;
}

export function decodeCollectionAdmitted(data: Buffer | Uint8Array): CollectionAdmitted {
  const r = checkEventDiscriminator(data, "CollectionAdmitted");
  const event: CollectionAdmitted = {
    pool: r.pubkey(),
    slot: r.u64(),
    collection: r.pubkey(),
    standards: r.u8(),
    oldStandards: r.u8(),
    reason: r.optionU16(),
    authority: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

export interface CollectionWithdrawn {
  pool: Address;
  slot: bigint;
  collection: Address;
  standards: number;
  reason: number;
  authority: Address;
}

export function decodeCollectionWithdrawn(data: Buffer | Uint8Array): CollectionWithdrawn {
  const r = checkEventDiscriminator(data, "CollectionWithdrawn");
  const event: CollectionWithdrawn = {
    pool: r.pubkey(),
    slot: r.u64(),
    collection: r.pubkey(),
    standards: r.u8(),
    reason: r.u16(),
    authority: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

export interface PositionSeized {
  pool: Address;
  slot: bigint;
  position: Address;
  kind: SeizureKind;
  ownerObserved: Address;
  sourceState: PositionState;
  recordedValue: bigint;
}

export function decodePositionSeized(data: Buffer | Uint8Array): PositionSeized {
  const r = checkEventDiscriminator(data, "PositionSeized");
  const event: PositionSeized = {
    pool: r.pubkey(),
    slot: r.u64(),
    position: r.pubkey(),
    kind: decodeSeizureKind(r),
    ownerObserved: r.pubkey(),
    sourceState: decodePositionState(r),
    recordedValue: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface NftReturned {
  pool: Address;
  slot: bigint;
  position: Address;
  depositor: Address;
  nftMint: Address;
}

export function decodeNftReturned(data: Buffer | Uint8Array): NftReturned {
  const r = checkEventDiscriminator(data, "NftReturned");
  const event: NftReturned = {
    pool: r.pubkey(),
    slot: r.u64(),
    position: r.pubkey(),
    depositor: r.pubkey(),
    nftMint: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

export interface Withdrawn {
  pool: Address;
  slot: bigint;
  position: Address;
  depositor: Address;
  feesPaid: bigint;
}

export function decodeWithdrawn(data: Buffer | Uint8Array): Withdrawn {
  const r = checkEventDiscriminator(data, "Withdrawn");
  const event: Withdrawn = {
    pool: r.pubkey(),
    slot: r.u64(),
    position: r.pubkey(),
    depositor: r.pubkey(),
    feesPaid: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface FeeClaimed {
  pool: Address;
  slot: bigint;
  position: Address;
  depositor: Address;
  feesPaid: bigint;
}

export function decodeFeeClaimed(data: Buffer | Uint8Array): FeeClaimed {
  const r = checkEventDiscriminator(data, "FeeClaimed");
  const event: FeeClaimed = {
    pool: r.pubkey(),
    slot: r.u64(),
    position: r.pubkey(),
    depositor: r.pubkey(),
    feesPaid: r.u64(),
  };
  r.assertConsumed();
  return event;
}

/** `feesPaid` is read **last**, mirroring its appended position on the wire: the stored `accrued`
 * the claim paid before closing the position, `0n` when nothing was owed. On the two T02 claims it
 * went to Treasury 03. */
export interface NftClaimed {
  pool: Address;
  slot: bigint;
  position: Address;
  depositor: Address;
  nftMint: Address;
  feesPaid: bigint;
}

export function decodeNftClaimed(data: Buffer | Uint8Array): NftClaimed {
  const r = checkEventDiscriminator(data, "NftClaimed");
  const event: NftClaimed = {
    pool: r.pubkey(),
    slot: r.u64(),
    position: r.pubkey(),
    depositor: r.pubkey(),
    nftMint: r.pubkey(),
    feesPaid: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface ValueRecorded {
  pool: Address;
  slot: bigint;
  authority: Address;
  position: Address;
  old: bigint;
  new: bigint;
  observedAt: bigint;
}

export function decodeValueRecorded(data: Buffer | Uint8Array): ValueRecorded {
  const r = checkEventDiscriminator(data, "ValueRecorded");
  const event: ValueRecorded = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    position: r.pubkey(),
    old: r.u64(),
    new: r.u64(),
    observedAt: r.i64(),
  };
  r.assertConsumed();
  return event;
}

export interface BelowFloorClosed {
  pool: Address;
  slot: bigint;
  authority: Address;
  position: Address;
  depositor: Address;
}

export function decodeBelowFloorClosed(data: Buffer | Uint8Array): BelowFloorClosed {
  const r = checkEventDiscriminator(data, "BelowFloorClosed");
  const event: BelowFloorClosed = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    position: r.pubkey(),
    depositor: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

export interface SweepBegun {
  pool: Address;
  slot: bigint;
  authority: Address;
  sweepEpoch: bigint;
}

export function decodeSweepBegun(data: Buffer | Uint8Array): SweepBegun {
  const r = checkEventDiscriminator(data, "SweepBegun");
  const event: SweepBegun = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    sweepEpoch: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface SweepEnded {
  pool: Address;
  slot: bigint;
  authority: Address;
  sweepEpoch: bigint;
  positionsUpdated: number;
  lastSweepAt: bigint;
}

export function decodeSweepEnded(data: Buffer | Uint8Array): SweepEnded {
  const r = checkEventDiscriminator(data, "SweepEnded");
  const event: SweepEnded = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    sweepEpoch: r.u64(),
    positionsUpdated: r.u32(),
    lastSweepAt: r.i64(),
  };
  r.assertConsumed();
  return event;
}

// ── Roll-pipeline events ─────────────────────────────────────────────────────

export interface BatchCommitted {
  pool: Address;
  slot: bigint;
  roller: Address;
  batch: Address;
  n: number;
  sPrice: bigint;
  sTicket: bigint;
  sFee: bigint;
  sAllocBps: [number, number, number];
  sBlankFaces: BlankFaces;
}

export function decodeBatchCommitted(data: Buffer | Uint8Array): BatchCommitted {
  const r = checkEventDiscriminator(data, "BatchCommitted");
  const event: BatchCommitted = {
    pool: r.pubkey(),
    slot: r.u64(),
    roller: r.pubkey(),
    batch: r.pubkey(),
    n: r.u8(),
    sPrice: r.u64(),
    sTicket: r.u64(),
    sFee: r.u64(),
    sAllocBps: [r.u16(), r.u16(), r.u16()],
    sBlankFaces: readBlankFaces(r),
  };
  r.assertConsumed();
  return event;
}

export interface BatchResolved {
  pool: Address;
  slot: bigint;
  batch: Address;
}

export function decodeBatchResolved(data: Buffer | Uint8Array): BatchResolved {
  const r = checkEventDiscriminator(data, "BatchResolved");
  const event: BatchResolved = {
    pool: r.pubkey(),
    slot: r.u64(),
    batch: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

export interface RollSettled {
  pool: Address;
  slot: bigint;
  batch: Address;
  rollIndex: number;
  outcome: RollOutcome;
  selectedMint: Address;
  blankFaceIdx: number;
  drawValue: bigint;
  attempts: number;
  wAtDraw: bigint;
  nRealAtDraw: number;
  blanksAtDraw: BlankCounts;
  feeEqual: bigint;
  feeTier: bigint;
  feeProtocol: bigint;
  feeT04: bigint;
}

export function decodeRollSettled(data: Buffer | Uint8Array): RollSettled {
  const r = checkEventDiscriminator(data, "RollSettled");
  const event: RollSettled = {
    pool: r.pubkey(),
    slot: r.u64(),
    batch: r.pubkey(),
    rollIndex: r.u8(),
    outcome: decodeRollOutcome(r),
    selectedMint: r.pubkey(),
    blankFaceIdx: r.u8(),
    drawValue: r.u128(),
    attempts: r.u8(),
    wAtDraw: r.u128(),
    nRealAtDraw: r.u32(),
    blanksAtDraw: readBlankCounts(r),
    feeEqual: r.u64(),
    feeTier: r.u64(),
    feeProtocol: r.u64(),
    feeT04: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface RollRefunded {
  pool: Address;
  slot: bigint;
  batch: Address;
  rollIndex: number;
  reason: number;
  wallet: Address;
}

export function decodeRollRefunded(data: Buffer | Uint8Array): RollRefunded {
  const r = checkEventDiscriminator(data, "RollRefunded");
  const event: RollRefunded = {
    pool: r.pubkey(),
    slot: r.u64(),
    batch: r.pubkey(),
    rollIndex: r.u8(),
    reason: r.u16(),
    wallet: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

export interface BatchCompleted {
  pool: Address;
  slot: bigint;
  batch: Address;
  openBatchesRemaining: number;
}

export function decodeBatchCompleted(data: Buffer | Uint8Array): BatchCompleted {
  const r = checkEventDiscriminator(data, "BatchCompleted");
  const event: BatchCompleted = {
    pool: r.pubkey(),
    slot: r.u64(),
    batch: r.pubkey(),
    openBatchesRemaining: r.u32(),
  };
  r.assertConsumed();
  return event;
}

export interface BatchRecovered {
  pool: Address;
  slot: bigint;
  batch: Address;
  wallet: Address;
  openBatchesRemaining: number;
}

export function decodeBatchRecovered(data: Buffer | Uint8Array): BatchRecovered {
  const r = checkEventDiscriminator(data, "BatchRecovered");
  const event: BatchRecovered = {
    pool: r.pubkey(),
    slot: r.u64(),
    batch: r.pubkey(),
    wallet: r.pubkey(),
    openBatchesRemaining: r.u32(),
  };
  r.assertConsumed();
  return event;
}

export interface BatchClosed {
  pool: Address;
  slot: bigint;
  batch: Address;
  rentRecipient: Address;
}

export function decodeBatchClosed(data: Buffer | Uint8Array): BatchClosed {
  const r = checkEventDiscriminator(data, "BatchClosed");
  const event: BatchClosed = {
    pool: r.pubkey(),
    slot: r.u64(),
    batch: r.pubkey(),
    rentRecipient: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

export interface AcquiredActivated {
  pool: Address;
  slot: bigint;
  authority: Address;
  position: Address;
  value: bigint;
  observedAt: bigint;
  observedAgeSeconds: bigint;
}

export function decodeAcquiredActivated(data: Buffer | Uint8Array): AcquiredActivated {
  const r = checkEventDiscriminator(data, "AcquiredActivated");
  const event: AcquiredActivated = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    position: r.pubkey(),
    value: r.u64(),
    observedAt: r.i64(),
    observedAgeSeconds: r.i64(),
  };
  r.assertConsumed();
  return event;
}

export interface T04CeilingOverflow {
  pool: Address;
  slot: bigint;
  amount: bigint;
}

export function decodeT04CeilingOverflow(data: Buffer | Uint8Array): T04CeilingOverflow {
  const r = checkEventDiscriminator(data, "T04CeilingOverflow");
  const event: T04CeilingOverflow = {
    pool: r.pubkey(),
    slot: r.u64(),
    amount: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface T04Swept {
  slot: bigint;
  amount: bigint;
  ceiling: bigint;
}

export function decodeT04Swept(data: Buffer | Uint8Array): T04Swept {
  const r = checkEventDiscriminator(data, "T04Swept");
  const event: T04Swept = {
    slot: r.u64(),
    amount: r.u64(),
    ceiling: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface T04CeilingSet {
  slot: bigint;
  authority: Address;
  old: bigint;
  new: bigint;
}

export function decodeT04CeilingSet(data: Buffer | Uint8Array): T04CeilingSet {
  const r = checkEventDiscriminator(data, "T04CeilingSet");
  const event: T04CeilingSet = {
    slot: r.u64(),
    authority: r.pubkey(),
    old: r.u64(),
    new: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface TreasuriesConsolidated {
  pool: Address;
  slot: bigint;
  authority: Address;
  oldTreasury03: Address;
  oldTreasury04: Address;
  treasury03: Address;
  treasury04: Address;
  amount03: bigint;
  amount04: bigint;
}

export function decodeTreasuriesConsolidated(data: Buffer | Uint8Array): TreasuriesConsolidated {
  const r = checkEventDiscriminator(data, "TreasuriesConsolidated");
  const event: TreasuriesConsolidated = {
    pool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    oldTreasury03: r.pubkey(),
    oldTreasury04: r.pubkey(),
    treasury03: r.pubkey(),
    treasury04: r.pubkey(),
    amount03: r.u64(),
    amount04: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface T03Transferred {
  sourcePool: Address;
  destinationPool: Address;
  slot: bigint;
  authority: Address;
  amount: bigint;
}

export function decodeT03Transferred(data: Buffer | Uint8Array): T03Transferred {
  const r = checkEventDiscriminator(data, "T03Transferred");
  const event: T03Transferred = {
    sourcePool: r.pubkey(),
    destinationPool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    amount: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface T04Transferred {
  sourcePool: Address;
  destinationPool: Address;
  slot: bigint;
  authority: Address;
  amount: bigint;
}

export function decodeT04Transferred(data: Buffer | Uint8Array): T04Transferred {
  const r = checkEventDiscriminator(data, "T04Transferred");
  const event: T04Transferred = {
    sourcePool: r.pubkey(),
    destinationPool: r.pubkey(),
    slot: r.u64(),
    authority: r.pubkey(),
    amount: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface Treasury03Migrated {
  pool: Address;
  slot: bigint;
  oldTreasury03: Address;
  newTreasury03: Address;
}

export function decodeTreasury03Migrated(data: Buffer | Uint8Array): Treasury03Migrated {
  const r = checkEventDiscriminator(data, "Treasury03Migrated");
  const event: Treasury03Migrated = {
    pool: r.pubkey(),
    slot: r.u64(),
    oldTreasury03: r.pubkey(),
    newTreasury03: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

/** Who paid for a sale: Treasury 03 at the buyback rate, or the loader's escrow at the pool's bid. */
export type BuybackPayer = "Treasury03" | "Escrow";
const BUYBACK_PAYER_BY_TAG: readonly BuybackPayer[] = ["Treasury03", "Escrow"];
function decodeBuybackPayer(r: BorshReader): BuybackPayer {
  const tag = r.u8();
  const payer = BUYBACK_PAYER_BY_TAG[tag];
  if (payer === undefined) throw new Error(`unknown BuybackPayer tag: ${tag}`);
  return payer;
}

/** `standard` and the four payer fields are read **last**, mirroring their appended position on
 * the wire: every field before them sits at the offset an older decoder already expected.
 * `paid` is the seller's amount on either path; on an escrow sale the loader paid `escrowPaid`
 * at `bidBps` and Treasury 03 received `escrowPaid − paid`. On a Treasury 03 sale `escrowOwner`
 * is the default address and `escrowPaid` and `bidBps` are zero. */
export interface InstantSold {
  pool: Address;
  slot: bigint;
  position: Address;
  seller: Address;
  value: bigint;
  observedAt: bigint;
  observedAgeSeconds: bigint;
  rateBps: number;
  paid: bigint;
  standard: number;
  payer: BuybackPayer;
  escrowOwner: Address;
  escrowPaid: bigint;
  bidBps: number;
}

export function decodeInstantSold(data: Buffer | Uint8Array): InstantSold {
  const r = checkEventDiscriminator(data, "InstantSold");
  const event: InstantSold = {
    pool: r.pubkey(),
    slot: r.u64(),
    position: r.pubkey(),
    seller: r.pubkey(),
    value: r.u64(),
    observedAt: r.i64(),
    observedAgeSeconds: r.i64(),
    rateBps: r.u16(),
    paid: r.u64(),
    standard: r.u8(),
    payer: decodeBuybackPayer(r),
    escrowOwner: r.pubkey(),
    escrowPaid: r.u64(),
    bidBps: r.u16(),
  };
  r.assertConsumed();
  return event;
}

/** Where an `EscrowDeposited`'s USDC came from: the owner's wallet, or one position's claimed fees. */
export type EscrowFundingSource = "Wallet" | "Fees";
const ESCROW_FUNDING_SOURCE_BY_TAG: readonly EscrowFundingSource[] = ["Wallet", "Fees"];
function decodeEscrowFundingSource(r: BorshReader): EscrowFundingSource {
  const tag = r.u8();
  const source = ESCROW_FUNDING_SOURCE_BY_TAG[tag];
  if (source === undefined) throw new Error(`unknown EscrowFundingSource tag: ${tag}`);
  return source;
}

export interface EscrowOpened {
  escrow: Address;
  slot: bigint;
  owner: Address;
}

export function decodeEscrowOpened(data: Buffer | Uint8Array): EscrowOpened {
  const r = checkEventDiscriminator(data, "EscrowOpened");
  const event: EscrowOpened = {
    escrow: r.pubkey(),
    slot: r.u64(),
    owner: r.pubkey(),
  };
  r.assertConsumed();
  return event;
}

/** `position` and `positionPool` are the default key for a `Wallet` deposit. */
export interface EscrowDeposited {
  escrow: Address;
  slot: bigint;
  owner: Address;
  amount: bigint;
  source: EscrowFundingSource;
  position: Address;
  positionPool: Address;
  balanceAfter: bigint;
}

export function decodeEscrowDeposited(data: Buffer | Uint8Array): EscrowDeposited {
  const r = checkEventDiscriminator(data, "EscrowDeposited");
  const event: EscrowDeposited = {
    escrow: r.pubkey(),
    slot: r.u64(),
    owner: r.pubkey(),
    amount: r.u64(),
    source: decodeEscrowFundingSource(r),
    position: r.pubkey(),
    positionPool: r.pubkey(),
    balanceAfter: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface EscrowWithdrawn {
  escrow: Address;
  slot: bigint;
  owner: Address;
  amount: bigint;
  balanceAfter: bigint;
}

export function decodeEscrowWithdrawn(data: Buffer | Uint8Array): EscrowWithdrawn {
  const r = checkEventDiscriminator(data, "EscrowWithdrawn");
  const event: EscrowWithdrawn = {
    escrow: r.pubkey(),
    slot: r.u64(),
    owner: r.pubkey(),
    amount: r.u64(),
    balanceAfter: r.u64(),
  };
  r.assertConsumed();
  return event;
}

export interface EscrowArmed {
  escrow: Address;
  slot: bigint;
  owner: Address;
  armed: boolean;
}

export function decodeEscrowArmed(data: Buffer | Uint8Array): EscrowArmed {
  const r = checkEventDiscriminator(data, "EscrowArmed");
  const event: EscrowArmed = {
    escrow: r.pubkey(),
    slot: r.u64(),
    owner: r.pubkey(),
    armed: r.bool(),
  };
  r.assertConsumed();
  return event;
}

// ── Event dispatch ───────────────────────────────────────────────────────────

export type DecodedEvent =
  | { name: "AcquiredActivated"; data: AcquiredActivated }
  | { name: "AuthorityRotated"; data: AuthorityRotated }
  | { name: "BatchClosed"; data: BatchClosed }
  | { name: "BatchCommitted"; data: BatchCommitted }
  | { name: "BatchCompleted"; data: BatchCompleted }
  | { name: "BatchRecovered"; data: BatchRecovered }
  | { name: "BatchResolved"; data: BatchResolved }
  | { name: "ByeMintChanged"; data: ByeMintChanged }
  | { name: "BelowFloorClosed"; data: BelowFloorClosed }
  | { name: "CollectionAdmitted"; data: CollectionAdmitted }
  | { name: "CollectionWithdrawn"; data: CollectionWithdrawn }
  | { name: "ConfigChanged"; data: ConfigChanged }
  | { name: "DepositApproved"; data: DepositApproved }
  | { name: "DepositPending"; data: DepositPending }
  | { name: "DepositRejected"; data: DepositRejected }
  | { name: "EscrowArmed"; data: EscrowArmed }
  | { name: "EscrowDeposited"; data: EscrowDeposited }
  | { name: "EscrowOpened"; data: EscrowOpened }
  | { name: "EscrowWithdrawn"; data: EscrowWithdrawn }
  | { name: "FeeClaimed"; data: FeeClaimed }
  | { name: "InstantSold"; data: InstantSold }
  | { name: "NftClaimed"; data: NftClaimed }
  | { name: "NftReturned"; data: NftReturned }
  | { name: "PauseSet"; data: PauseSet }
  | { name: "PoolInitialized"; data: PoolInitialized }
  | { name: "PositionSeized"; data: PositionSeized }
  | { name: "ProtocolInitialized"; data: ProtocolInitialized }
  | { name: "ProtocolRevenueChanged"; data: ProtocolRevenueChanged }
  | { name: "RebalanceEvaluated"; data: RebalanceEvaluated }
  | { name: "RollRefunded"; data: RollRefunded }
  | { name: "RollSettled"; data: RollSettled }
  | { name: "SweepBegun"; data: SweepBegun }
  | { name: "SweepEnded"; data: SweepEnded }
  | { name: "SwapPoolChanged"; data: SwapPoolChanged }
  | { name: "T03Transferred"; data: T03Transferred }
  | { name: "T04CeilingOverflow"; data: T04CeilingOverflow }
  | { name: "T04CeilingSet"; data: T04CeilingSet }
  | { name: "T04Swept"; data: T04Swept }
  | { name: "T04Transferred"; data: T04Transferred }
  | { name: "TierChanged"; data: TierChanged }
  | { name: "Treasury03Migrated"; data: Treasury03Migrated }
  | { name: "TreasuriesConsolidated"; data: TreasuriesConsolidated }
  | { name: "ValueRecorded"; data: ValueRecorded }
  | { name: "VrfConfigChanged"; data: VrfConfigChanged }
  | { name: "Withdrawn"; data: Withdrawn };

const EVENT_DECODERS: Record<DecodedEvent["name"], (data: Buffer | Uint8Array) => DecodedEvent["data"]> = {
  AcquiredActivated: decodeAcquiredActivated,
  AuthorityRotated: decodeAuthorityRotated,
  BatchClosed: decodeBatchClosed,
  BatchCommitted: decodeBatchCommitted,
  BatchCompleted: decodeBatchCompleted,
  BatchRecovered: decodeBatchRecovered,
  BatchResolved: decodeBatchResolved,
  ByeMintChanged: decodeByeMintChanged,
  BelowFloorClosed: decodeBelowFloorClosed,
  CollectionAdmitted: decodeCollectionAdmitted,
  CollectionWithdrawn: decodeCollectionWithdrawn,
  ConfigChanged: decodeConfigChanged,
  DepositApproved: decodeDepositApproved,
  DepositPending: decodeDepositPending,
  DepositRejected: decodeDepositRejected,
  EscrowArmed: decodeEscrowArmed,
  EscrowDeposited: decodeEscrowDeposited,
  EscrowOpened: decodeEscrowOpened,
  EscrowWithdrawn: decodeEscrowWithdrawn,
  FeeClaimed: decodeFeeClaimed,
  InstantSold: decodeInstantSold,
  NftClaimed: decodeNftClaimed,
  NftReturned: decodeNftReturned,
  PauseSet: decodePauseSet,
  PoolInitialized: decodePoolInitialized,
  PositionSeized: decodePositionSeized,
  ProtocolInitialized: decodeProtocolInitialized,
  ProtocolRevenueChanged: decodeProtocolRevenueChanged,
  RebalanceEvaluated: decodeRebalanceEvaluated,
  RollRefunded: decodeRollRefunded,
  RollSettled: decodeRollSettled,
  SweepBegun: decodeSweepBegun,
  SweepEnded: decodeSweepEnded,
  SwapPoolChanged: decodeSwapPoolChanged,
  T03Transferred: decodeT03Transferred,
  T04CeilingOverflow: decodeT04CeilingOverflow,
  T04CeilingSet: decodeT04CeilingSet,
  T04Swept: decodeT04Swept,
  T04Transferred: decodeT04Transferred,
  TierChanged: decodeTierChanged,
  Treasury03Migrated: decodeTreasury03Migrated,
  TreasuriesConsolidated: decodeTreasuriesConsolidated,
  ValueRecorded: decodeValueRecorded,
  VrfConfigChanged: decodeVrfConfigChanged,
  Withdrawn: decodeWithdrawn,
};

const EVENT_DECODER_BY_DISCRIMINATOR = new Map(
  (Object.keys(EVENT_DECODERS) as DecodedEvent["name"][]).map((name) => [
    eventDiscriminator(name).toString("hex"),
    name,
  ]),
);

/** Dispatches on the event's own 8-byte discriminator, so a caller need not already know which
 * event a log line carries. */
export function decodeEvent(data: Buffer | Uint8Array): DecodedEvent {
  const buf = Buffer.from(data);
  const disc = buf.subarray(0, 8).toString("hex");
  const name = EVENT_DECODER_BY_DISCRIMINATOR.get(disc);
  if (!name) throw new Error(`unknown event discriminator: ${disc}`);
  return { name, data: EVENT_DECODERS[name](buf) } as DecodedEvent;
}
