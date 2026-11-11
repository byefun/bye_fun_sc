// Read-side codec tests for scripts/lib/decoders.ts — the borsh reader, the 6 account decoders
// and the 20 event decoders.
//
// Every decoder gets a *fixed golden byte fixture* — built here with raw little-endian byte
// arithmetic (leU16/leU32/leU64/leU128 below), never by calling the production `Borsh` writer —
// as an oracle independent of both `decoders.ts` and `anchor.ts`. Round-trip tests against the
// existing `Borsh` writer are included in addition, for a representative spread of field shapes
// (enums, `Option<Pubkey>`, `String`, negative `i64`, `u128`, fixed arrays). A round-trip alone
// would stay green if the reader and writer shared a fault, so it
// never stands in for the golden-byte fixture.
//
// Every decoder here hand-mirrors the Rust layouts in `programs/bye_machine/src/state/` and
// `programs/bye_machine/src/common/events.rs`, from the same source in the same week. A reorder
// that moves both together is invisible to these tests too — that limit is stated, not closed.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { getAddressDecoder, type Address } from "@solana/kit";
import { Borsh, accountDiscriminator, eventDiscriminator } from "../../scripts/lib/anchor.js";
import {
  BorshReader,
  FENWICK_CAPACITY,
  type BlankCounts,
  type BlankFaces,
  decodeProtocolConfig,
  decodeRollBatch,
  LEGACY_ROLL_BATCH_LEN,
  ROLL_BATCH_LEN,
  encodePool,
  decodePool,
  decodePosition,
  decodeWalletStats,
  decodeTopTier,
  decodeWeightIndex,
  decodeProtocolInitialized,
  decodePoolInitialized,
  decodeConfigChanged,
  decodePauseSet,
  decodeAuthorityRotated,
  decodeDepositPending,
  decodeDepositApproved,
  decodeRebalanceEvaluated,
  decodeTierChanged,
  decodeDepositRejected,
  decodeNftReturned,
  decodeWithdrawn,
  decodeNftClaimed,
  decodeFeeClaimed,
  decodeValueRecorded,
  decodeBelowFloorClosed,
  decodeSweepBegun,
  decodeSweepEnded,
  decodeEvent,
} from "../../scripts/lib/decoders.js";
import { legacyBatchAccount } from "../../scripts/lib/legacy-batch-fixture.js";

// ── Fixture helpers — plain byte arithmetic, independent of both Borsh (write) and BorshReader
// (read); nothing here calls a Buffer `write*LE`/`read*LE` method. ──────────────────────────────

const addrDec = getAddressDecoder();

function leU16(n: number): number[] {
  return [n & 0xff, (n >> 8) & 0xff];
}
function leU32(n: number): number[] {
  return [n & 0xff, (n >>> 8) & 0xff, (n >>> 16) & 0xff, (n >>> 24) & 0xff];
}
function leU64(n: bigint): number[] {
  let x = n < 0n ? n + (1n << 64n) : n;
  const out: number[] = [];
  for (let i = 0; i < 8; i++) {
    out.push(Number(x & 0xffn));
    x >>= 8n;
  }
  return out;
}
const leI64 = leU64;
function leU128(n: bigint): number[] {
  let x = n;
  const out: number[] = [];
  for (let i = 0; i < 16; i++) {
    out.push(Number(x & 0xffn));
    x >>= 8n;
  }
  return out;
}
function pkBytes(fill: number): number[] {
  return new Array(32).fill(fill);
}
function pkAddr(fill: number): Address {
  return addrDec.decode(Buffer.from(pkBytes(fill)));
}
function strBytes(s: string): number[] {
  const utf8 = Array.from(Buffer.from(s, "utf8"));
  return [...leU32(utf8.length), ...utf8];
}
function bytes(...parts: number[][]): Buffer {
  return Buffer.from(parts.flat());
}

// Discriminators copied verbatim from `idl/bye_machine.json` — Anchor's own build output,
// independent of the sha256 derivation `accountDiscriminator`/`eventDiscriminator` compute.
const ACCOUNT_DISC: Record<string, number[]> = {
  Pool: [241, 154, 109, 4, 17, 177, 109, 188],
  Position: [170, 188, 143, 228, 122, 64, 247, 208],
  ProtocolConfig: [207, 91, 250, 28, 152, 179, 215, 209],
  TopTier: [125, 120, 234, 158, 93, 21, 47, 75],
  WalletStats: [96, 53, 96, 132, 136, 197, 236, 144],
  WeightIndex: [162, 133, 99, 158, 244, 251, 171, 71],
};
const EVENT_DISC: Record<string, number[]> = {
  AuthorityRotated: [89, 124, 120, 223, 3, 19, 185, 230],
  BelowFloorClosed: [22, 109, 132, 240, 126, 52, 221, 82],
  ConfigChanged: [147, 25, 86, 98, 98, 77, 78, 192],
  DepositApproved: [229, 217, 230, 247, 42, 220, 160, 243],
  DepositPending: [101, 208, 102, 93, 232, 105, 39, 35],
  DepositRejected: [142, 2, 235, 2, 171, 247, 250, 10],
  FeeClaimed: [202, 108, 15, 80, 101, 18, 217, 158],
  NftClaimed: [4, 232, 63, 124, 139, 205, 168, 234],
  NftReturned: [178, 221, 187, 172, 187, 206, 236, 229],
  PauseSet: [175, 57, 198, 136, 192, 66, 204, 73],
  PoolInitialized: [100, 118, 173, 87, 12, 198, 254, 229],
  ProtocolInitialized: [173, 122, 168, 254, 9, 118, 76, 132],
  RebalanceEvaluated: [122, 52, 229, 120, 7, 239, 128, 112],
  SweepBegun: [160, 229, 165, 59, 13, 136, 66, 145],
  SweepEnded: [205, 223, 178, 214, 13, 11, 22, 111],
  TierChanged: [126, 9, 150, 127, 199, 123, 136, 1],
  ValueRecorded: [199, 217, 250, 231, 39, 89, 141, 173],
  Withdrawn: [20, 89, 223, 198, 194, 124, 219, 13],
};

describe("discriminators match Anchor's own IDL, independent of the sha256 derivation", () => {
  it("account discriminators", () => {
    for (const [name, disc] of Object.entries(ACCOUNT_DISC)) {
      assert.deepEqual(accountDiscriminator(name), Buffer.from(disc), name);
    }
  });
  it("event discriminators", () => {
    for (const [name, disc] of Object.entries(EVENT_DISC)) {
      assert.deepEqual(eventDiscriminator(name), Buffer.from(disc), name);
    }
  });
});

// ── BorshReader primitives ───────────────────────────────────────────────────────────────────

describe("BorshReader primitives", () => {
  it("reads little-endian integers at their declared widths", () => {
    assert.equal(new BorshReader(Buffer.from([0x2a])).u8(), 0x2a);
    assert.equal(new BorshReader(Buffer.from([0x01, 0x02])).u16(), 0x0201);
    assert.equal(new BorshReader(Buffer.from([1, 2, 3, 4])).u32(), 0x04030201);
    assert.equal(new BorshReader(Buffer.from([1, 0, 0, 0, 0, 0, 0, 0])).u64(), 1n);
    assert.equal(new BorshReader(Buffer.alloc(8, 0xff)).i64(), -1n);
    assert.equal(new BorshReader(Buffer.from(leU128(340282366920938463463374607431768211455n))).u128(), 340282366920938463463374607431768211455n);
  });

  it("reads bool as any non-zero byte being true", () => {
    assert.equal(new BorshReader(Buffer.from([0])).bool(), false);
    assert.equal(new BorshReader(Buffer.from([1])).bool(), true);
  });

  it("reads a 32-byte pubkey", () => {
    assert.equal(new BorshReader(Buffer.from(pkBytes(7))).pubkey(), pkAddr(7));
  });

  it("frames Option<Pubkey> as 0x00 = None, 0x01 ‖ 32 bytes = Some", () => {
    assert.equal(new BorshReader(Buffer.from([0])).optionPubkey(), null);
    assert.equal(new BorshReader(bytes([1], pkBytes(3))).optionPubkey(), pkAddr(3));
  });

  it("reads a length-prefixed String", () => {
    assert.equal(new BorshReader(bytes(strBytes("bye"))).string(), "bye");
  });

  it("assertConsumed throws on trailing unread bytes", () => {
    const r = new BorshReader(Buffer.from([1, 2, 3]));
    r.u8();
    assert.throws(() => r.assertConsumed(), /2 unconsumed byte/);
  });

  it("throws reading past the end rather than returning undefined/garbage", () => {
    assert.throws(() => new BorshReader(Buffer.from([1, 2])).u32(), /expected 4 byte/);
  });
});

// ── Accounts ─────────────────────────────────────────────────────────────────────────────────

describe("ProtocolConfig", () => {
  // Distinct per byte, so a ceiling read from the wrong half of its 32 bytes shows.
  const CONFIG_RESERVED0 = Uint8Array.from({ length: 24 }, (_, i) => 0x40 | i);
  const golden = bytes(
    ACCOUNT_DISC.ProtocolConfig,
    pkBytes(1),
    leU64(0x0102_0304_0506_0708n), // t04_ceiling
    [...CONFIG_RESERVED0], // reserved_0
    pkBytes(3),
    pkBytes(4),
    pkBytes(5),
    pkBytes(6),
    pkBytes(7),
    pkBytes(8),
    pkBytes(9),
    pkBytes(10),
    pkBytes(11),
    pkBytes(12),
    pkBytes(13),
    pkBytes(14),
    leU16(15000),
    leU16(16000),
    [17],
  );
  const expected = {
    administrator: pkAddr(1),
    t04Ceiling: 0x0102_0304_0506_0708n,
    reserved0: CONFIG_RESERVED0,
    reserved1: pkAddr(3),
    reserved2: pkAddr(4),
    reserved3: pkAddr(5),
    operator: pkAddr(6),
    t05Authority: pkAddr(7),
    t02Authority: pkAddr(8),
    protocolRevenue: pkAddr(9),
    usdcMint: pkAddr(10),
    byeMint: pkAddr(11),
    vrfProgram: pkAddr(12),
    oracleQueue: pkAddr(13),
    swapPool: pkAddr(14),
    maxSwapSlippageBps: 15000,
    poolCounter: 16000,
    bump: 17,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeProtocolConfig(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .pubkey(expected.administrator)
      .u64(expected.t04Ceiling)
      .bytes(expected.reserved0)
      .pubkey(expected.reserved1)
      .pubkey(expected.reserved2)
      .pubkey(expected.reserved3)
      .pubkey(expected.operator)
      .pubkey(expected.t05Authority)
      .pubkey(expected.t02Authority)
      .pubkey(expected.protocolRevenue)
      .pubkey(expected.usdcMint)
      .pubkey(expected.byeMint)
      .pubkey(expected.vrfProgram)
      .pubkey(expected.oracleQueue)
      .pubkey(expected.swapPool)
      .u16(expected.maxSwapSlippageBps)
      .u16(expected.poolCounter)
      .u8(expected.bump)
      .build();
    const data = Buffer.concat([accountDiscriminator("ProtocolConfig"), args]);
    assert.deepEqual(decodeProtocolConfig(data), expected);
  });

  it("rejects a mismatched account discriminator", () => {
    const bad = Buffer.from(golden);
    bad[0] ^= 0xff;
    assert.throws(() => decodeProtocolConfig(bad), /discriminator mismatch/);
  });

  it("rejects a truncated account", () => {
    assert.throws(() => decodeProtocolConfig(golden.subarray(0, golden.length - 1)), /expected 1 byte/);
  });
});

describe("Pool", () => {
  // Non-zero and distinct per byte, so a field moved into or out of `reserved` shows.
  const POOL_RESERVED = Uint8Array.from({ length: 32 }, (_, i) => 0x80 | i);
  const golden = bytes(
    ACCOUNT_DISC.Pool,
    leU16(1), // pool_id
    pkBytes(2), // weight_index
    pkBytes(4), // treasury_03
    leU64(5n), // price
    leU64(6n), // ticket_target
    leU64(7n), // fee
    leU16(8), // alloc_equal_bps
    leU16(9), // alloc_tier_bps
    leU16(10), // alloc_protocol_bps
    leU64(11n), // admission_floor
    leU64(12n), // admission_ceiling
    leU64(13n), // wallet_value_cap
    [14], // max_n
    leU16(15), // buyback_rate_bps
    leU64(16n), // blank_faces[0]
    leU64(17n), // blank_faces[1]
    leU64(18n), // blank_faces[2]
    leU64(160n), // blank_faces[3]
    leU64(161n), // blank_faces[4]
    leU64(162n), // blank_faces[5]
    leU64(163n), // blank_faces[6]
    leU64(164n), // blank_faces[7]
    leU64(19n), // t04_ceiling
    leU16(20), // sweep_cadence_hours
    [21], // tier_size
    [1], // deposits_paused = true
    [0], // rolls_paused = false
    leU16(22), // pause_reason
    leU32(23), // n_real
    leU32(24), // blanks[0]
    leU32(25), // blanks[1]
    leU32(26), // blanks[2]
    leU32(260), // blanks[3]
    leU32(261), // blanks[4]
    leU32(262), // blanks[5]
    leU32(263), // blanks[6]
    leU32(264), // blanks[7]
    leU128(27n), // w_real
    leU32(28), // open_batches
    leU64(29n), // batch_counter
    leU64(30n), // position_counter
    leU64(31n), // pending_roll_liability
    leU64(32n), // owed_fees
    leU128(33n), // acc_equal
    [0], // sweep_pending = false
    leU64(34n), // sweep_epoch
    leI64(-35n), // last_sweep_at
    leU32(36), // sweep_updates
    [37], // bump
    leU16(38), // escrow_bid_bps
    pkBytes(39), // treasury_04
    [...POOL_RESERVED], // reserved
  );
  const expected = {
    poolId: 1,
    weightIndex: pkAddr(2),
    treasury03: pkAddr(4),
    price: 5n,
    ticketTarget: 6n,
    fee: 7n,
    allocEqualBps: 8,
    allocTierBps: 9,
    allocProtocolBps: 10,
    admissionFloor: 11n,
    admissionCeiling: 12n,
    walletValueCap: 13n,
    maxN: 14,
    buybackRateBps: 15,
    blankFaces: [16n, 17n, 18n, 160n, 161n, 162n, 163n, 164n] as BlankFaces,
    t04Ceiling: 19n,
    sweepCadenceHours: 20,
    tierSize: 21,
    depositsPaused: true,
    rollsPaused: false,
    pauseReason: 22,
    nReal: 23,
    blanks: [24, 25, 26, 260, 261, 262, 263, 264] as BlankCounts,
    wReal: 27n,
    openBatches: 28,
    batchCounter: 29n,
    positionCounter: 30n,
    pendingRollLiability: 31n,
    owedFees: 32n,
    accEqual: 33n,
    sweepPending: false,
    sweepEpoch: 34n,
    lastSweepAt: -35n,
    sweepUpdates: 36,
    bump: 37,
    escrowBidBps: 38,
    treasury04: pkAddr(39),
    reserved: POOL_RESERVED,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodePool(golden), expected);
  });

  it("encodePool writes exactly the golden bytes", () => {
    assert.deepEqual(encodePool(expected), golden);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .u16(expected.poolId)
      .pubkey(expected.weightIndex)
      .pubkey(expected.treasury03)
      .u64(expected.price)
      .u64(expected.ticketTarget)
      .u64(expected.fee)
      .u16(expected.allocEqualBps)
      .u16(expected.allocTierBps)
      .u16(expected.allocProtocolBps)
      .u64(expected.admissionFloor)
      .u64(expected.admissionCeiling)
      .u64(expected.walletValueCap)
      .u8(expected.maxN)
      .u16(expected.buybackRateBps)
      .arrayU64(expected.blankFaces, 8)
      .u64(expected.t04Ceiling)
      .u16(expected.sweepCadenceHours)
      .u8(expected.tierSize)
      .bool(expected.depositsPaused)
      .bool(expected.rollsPaused)
      .u16(expected.pauseReason)
      .u32(expected.nReal)
      .arrayU32(expected.blanks, 8)
      .u128(expected.wReal)
      .u32(expected.openBatches)
      .u64(expected.batchCounter)
      .u64(expected.positionCounter)
      .u64(expected.pendingRollLiability)
      .u64(expected.owedFees)
      .u128(expected.accEqual)
      .bool(expected.sweepPending)
      .u64(expected.sweepEpoch)
      .i64(expected.lastSweepAt)
      .u32(expected.sweepUpdates)
      .u8(expected.bump)
      .u16(expected.escrowBidBps)
      .pubkey(expected.treasury04)
      .bytes(expected.reserved)
      .build();
    const data = Buffer.concat([accountDiscriminator("Pool"), args]);
    assert.deepEqual(decodePool(data), expected);
  });

  // The class of defect most worth guarding against: two adjacent same-width fields
  // (`price`/`ticket_target`, both u64) read in the wrong order. Swapping their byte ranges in the
  // fixture must swap the decoded values — proving the decoder reads each from its own fixed slot,
  // not from a slot a reordering bug could share.
  it("swapping price/ticket_target's adjacent byte ranges swaps the decoded values", () => {
    const priceOffset = ACCOUNT_DISC.Pool.length + 2 + 32 * 2; // disc ‖ pool_id ‖ 2 pubkeys
    const swapped = Buffer.from(golden);
    const price = Buffer.from(swapped.subarray(priceOffset, priceOffset + 8));
    const ticket = Buffer.from(swapped.subarray(priceOffset + 8, priceOffset + 16));
    ticket.copy(swapped, priceOffset);
    price.copy(swapped, priceOffset + 8);

    const decoded = decodePool(swapped);
    assert.equal(decoded.price, 6n, "byte-swapped fixture must decode price as the old ticket_target value");
    assert.equal(decoded.ticketTarget, 5n, "byte-swapped fixture must decode ticket_target as the old price value");
  });

  it("rejects a mismatched account discriminator", () => {
    const bad = Buffer.from(golden);
    bad[0] ^= 0xff;
    assert.throws(() => decodePool(bad), /discriminator mismatch/);
  });
});

describe("Position", () => {
  const golden = bytes(
    ACCOUNT_DISC.Position,
    pkBytes(1), // pool
    pkBytes(2), // depositor
    pkBytes(3), // nft_mint
    leU64(4n), // position_id
    [1], // state = Active (tag 1)
    leU64(5n), // recorded_value
    leI64(-6n), // value_observed_at
    leU64(7n), // deposit_value
    leU32(8), // slot_index
    leI64(-9n), // activated_at
    leU128(10n), // equal_checkpoint
    leU64(11n), // accrued
    [1], // in_tier = true
    leU128(12n), // tier_checkpoint
    leI64(-13n), // lock_until
    leU16(14), // reject_reason
    [2], // standard
    [15], // bump
    [16], // vault_bump
  );
  const expected = {
    pool: pkAddr(1),
    depositor: pkAddr(2),
    nftMint: pkAddr(3),
    positionId: 4n,
    state: "Active" as const,
    recordedValue: 5n,
    valueObservedAt: -6n,
    depositValue: 7n,
    slotIndex: 8,
    activatedAt: -9n,
    equalCheckpoint: 10n,
    accrued: 11n,
    inTier: true,
    tierCheckpoint: 12n,
    lockUntil: -13n,
    rejectReason: 14,
    standard: 2,
    bump: 15,
    vaultBump: 16,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodePosition(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .pubkey(expected.pool)
      .pubkey(expected.depositor)
      .pubkey(expected.nftMint)
      .u64(expected.positionId)
      .enumVariant(1) // Active
      .u64(expected.recordedValue)
      .i64(expected.valueObservedAt)
      .u64(expected.depositValue)
      .u32(expected.slotIndex)
      .i64(expected.activatedAt)
      .u128(expected.equalCheckpoint)
      .u64(expected.accrued)
      .bool(expected.inTier)
      .u128(expected.tierCheckpoint)
      .i64(expected.lockUntil)
      .u16(expected.rejectReason)
      .u8(expected.standard)
      .u8(expected.bump)
      .u8(expected.vaultBump)
      .build();
    const data = Buffer.concat([accountDiscriminator("Position"), args]);
    assert.deepEqual(decodePosition(data), expected);
  });

  it("decodes every PositionState tag at its declared index", () => {
    const cases: Array<[number, string]> = [
      [0, "Pending"],
      [1, "Active"],
      [2, "ClosedBelowFloor"],
      [3, "Rejected"],
      [4, "Seized"],
    ];
    for (const [tag, name] of cases) {
      const buf = Buffer.from(golden);
      buf[ACCOUNT_DISC.Position.length + 32 * 3 + 8] = tag; // the `state` byte
      assert.equal(decodePosition(buf).state, name);
    }
  });

  it("throws on an out-of-range PositionState tag", () => {
    const buf = Buffer.from(golden);
    buf[ACCOUNT_DISC.Position.length + 32 * 3 + 8] = 5;
    assert.throws(() => decodePosition(buf), /unknown PositionState tag: 5/);
  });
});

describe("WalletStats", () => {
  const golden = bytes(ACCOUNT_DISC.WalletStats, pkBytes(1), pkBytes(2), leU32(3), leU64(4n), [5]);
  const expected = { pool: pkAddr(1), owner: pkAddr(2), activePositions: 3, activeValue: 4n, bump: 5 };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeWalletStats(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .pubkey(expected.pool)
      .pubkey(expected.owner)
      .u32(expected.activePositions)
      .u64(expected.activeValue)
      .u8(expected.bump)
      .build();
    const data = Buffer.concat([accountDiscriminator("WalletStats"), args]);
    assert.deepEqual(decodeWalletStats(data), expected);
  });
});

describe("TopTier", () => {
  const TIER_ENTRY_SIZE = 56;
  const TOP_TIER_CAPACITY = 20;

  it("decodes fixed golden bytes to the expected struct", () => {
    const entry0 = bytes(pkBytes(10), leU64(100n), leI64(200n), leU64(300n));
    const entry1 = bytes(pkBytes(11), leU64(101n), leI64(201n), leU64(301n));
    const zeroEntry = Buffer.alloc(TIER_ENTRY_SIZE, 0);
    const golden = Buffer.concat([
      Buffer.from(ACCOUNT_DISC.TopTier),
      Buffer.from(pkBytes(1)),
      Buffer.from([2]), // len
      entry0,
      entry1,
      ...Array<Buffer>(TOP_TIER_CAPACITY - 2).fill(zeroEntry),
      Buffer.from(leU128(400n)), // acc_tier
      Buffer.from([5]), // bump
    ]);

    assert.deepEqual(decodeTopTier(golden), {
      pool: pkAddr(1),
      len: 2,
      entries: [
        { position: pkAddr(10), value: 100n, activatedAt: 200n, positionId: 300n },
        { position: pkAddr(11), value: 101n, activatedAt: 201n, positionId: 301n },
      ],
      accTier: 400n,
      bump: 5,
    });
  });

  // Trap: entries past `len` are stale, not live data. Filled with non-zero garbage here so a
  // decoder that returned all 20 slots (rather than `entries.slice(0, len)`) would surface it.
  it("returns only entries[0..len]; stale slots past len never leak into the result", () => {
    const live = bytes(pkBytes(20), leU64(999n), leI64(888n), leU64(777n));
    const stale = Buffer.alloc(TIER_ENTRY_SIZE, 0xee);
    const golden = Buffer.concat([
      Buffer.from(ACCOUNT_DISC.TopTier),
      Buffer.from(pkBytes(9)),
      Buffer.from([1]), // len
      live,
      ...Array<Buffer>(TOP_TIER_CAPACITY - 1).fill(stale),
      Buffer.from(leU128(1n)),
      Buffer.from([1]),
    ]);

    const decoded = decodeTopTier(golden);
    assert.equal(decoded.len, 1);
    assert.equal(decoded.entries.length, 1);
    assert.deepEqual(decoded.entries[0], {
      position: pkAddr(20),
      value: 999n,
      activatedAt: 888n,
      positionId: 777n,
    });
  });

  it("an empty tier (len 0) decodes to no entries regardless of what the fixed array holds", () => {
    const garbage = Buffer.alloc(TIER_ENTRY_SIZE, 0x11);
    const golden = Buffer.concat([
      Buffer.from(ACCOUNT_DISC.TopTier),
      Buffer.from(pkBytes(1)),
      Buffer.from([0]), // len
      ...Array<Buffer>(TOP_TIER_CAPACITY).fill(garbage),
      Buffer.from(leU128(0n)),
      Buffer.from([1]),
    ]);
    assert.deepEqual(decodeTopTier(golden).entries, []);
  });

  it("throws if len exceeds the fixed 20-entry capacity", () => {
    const zeroEntry = Buffer.alloc(TIER_ENTRY_SIZE, 0);
    const golden = Buffer.concat([
      Buffer.from(ACCOUNT_DISC.TopTier),
      Buffer.from(pkBytes(1)),
      Buffer.from([21]), // len > capacity
      ...Array<Buffer>(TOP_TIER_CAPACITY).fill(zeroEntry),
      Buffer.from(leU128(0n)),
      Buffer.from([1]),
    ]);
    assert.throws(() => decodeTopTier(golden), /exceeds the fixed capacity/);
  });
});

describe("WeightIndex — zero_copy/repr(C), pinned byte offsets", () => {
  function buildWeightIndex(fields: {
    pool?: number;
    totalWeight?: bigint;
    highWater?: number;
    freeCount?: number;
    tree0?: bigint;
    freeStack0?: number;
  } = {}): Buffer {
    const buf = Buffer.alloc(196_672); // 8-byte disc + 196,664-byte struct (state/mod.rs's pinned size)
    Buffer.from(ACCOUNT_DISC.WeightIndex).copy(buf, 0);
    Buffer.from(pkBytes(fields.pool ?? 1)).copy(buf, 8 + 0);
    Buffer.from(leU64(fields.totalWeight ?? 111n)).copy(buf, 8 + 32);
    Buffer.from(leU32(fields.highWater ?? 222)).copy(buf, 8 + 40);
    Buffer.from(leU32(fields.freeCount ?? 333)).copy(buf, 8 + 44);
    // _padding at 8+48..8+56 stays zero.
    Buffer.from(leU64(fields.tree0 ?? 444n)).copy(buf, 8 + 56);
    Buffer.from(leU32(fields.freeStack0 ?? 555)).copy(buf, 8 + 56 + 8 * FENWICK_CAPACITY);
    return buf;
  }

  it("decodes the pinned header offsets from fixed golden bytes", () => {
    const decoded = decodeWeightIndex(buildWeightIndex());
    assert.equal(decoded.pool, pkAddr(1));
    assert.equal(decoded.totalWeight, 111n);
    assert.equal(decoded.highWater, 222);
    assert.equal(decoded.freeCount, 333);
    assert.equal(decoded.tree.length, FENWICK_CAPACITY);
    assert.equal(decoded.freeStack.length, FENWICK_CAPACITY);
    assert.equal(decoded.tree[0], 444n);
    assert.equal(decoded.tree[1], 0n);
    assert.equal(decoded.freeStack[0], 555);
    assert.equal(decoded.freeStack[1], 0);
  });

  // The field-order class, driven the same way as the Pool price/ticket_target case above: swapping
  // high_water and free_count's 4-byte ranges must swap the decoded values, the exact defect
  // `state/weight_index.rs`'s own `weight_index_field_order` pin was written to catch in Rust.
  it("swapping high_water/free_count's adjacent byte ranges swaps the decoded values", () => {
    const buf = buildWeightIndex({ highWater: 222, freeCount: 333 });
    const hw = Buffer.from(buf.subarray(8 + 40, 8 + 44));
    const fc = Buffer.from(buf.subarray(8 + 44, 8 + 48));
    fc.copy(buf, 8 + 40);
    hw.copy(buf, 8 + 44);

    const decoded = decodeWeightIndex(buf);
    assert.equal(decoded.highWater, 333, "byte-swapped fixture must decode high_water as the old free_count value");
    assert.equal(decoded.freeCount, 222, "byte-swapped fixture must decode free_count as the old high_water value");
  });

  it("rejects an account of the wrong byte length", () => {
    const short = buildWeightIndex().subarray(0, 196_671);
    assert.throws(() => decodeWeightIndex(short), /expected 196672 bytes, got 196671/);
  });

  it("rejects a mismatched account discriminator", () => {
    const buf = buildWeightIndex();
    buf[0] ^= 0xff;
    assert.throws(() => decodeWeightIndex(buf), /discriminator mismatch/);
  });
});

// ── Enums ────────────────────────────────────────────────────────────────────────────────────

describe("ConfigValue — variant order is load-bearing (Hours at index 4, not Bps)", () => {
  const golden = (tag: number, payload: number[]) =>
    bytes(EVENT_DISC.ConfigChanged, pkBytes(1), leU64(2n), pkBytes(3), strBytes("x"), [tag], payload, [tag], payload);

  it("decodes every ConfigValue tag at its declared index", () => {
    const cases: Array<[number, number[], ReturnType<typeof decodeConfigChanged>["old"]]> = [
      [0, leU64(9n), { kind: "Amount", value: 9n }],
      [1, leU16(9), { kind: "Bps", value: 9 }],
      [2, [9], { kind: "Count", value: 9 }],
      [
        3,
        [1n, 2n, 3n, 4n, 5n, 6n, 7n, 8n].flatMap((face) => leU64(face)),
        { kind: "Faces", value: [1n, 2n, 3n, 4n, 5n, 6n, 7n, 8n] },
      ],
      [4, leU16(24), { kind: "Hours", value: 24 }],
    ];
    for (const [tag, payload, expected] of cases) {
      const decoded = decodeConfigChanged(golden(tag, payload));
      assert.deepEqual(decoded.old, expected);
      assert.deepEqual(decoded.new, expected);
    }
  });

  it("does not read an hour count as basis points: Hours(24) must not decode as Bps(24)", () => {
    const decoded = decodeConfigChanged(golden(4, leU16(24)));
    assert.deepEqual(decoded.old, { kind: "Hours", value: 24 });
    assert.notDeepEqual(decoded.old, { kind: "Bps", value: 24 });
  });

  it("throws on an out-of-range ConfigValue tag", () => {
    assert.throws(() => decodeConfigChanged(golden(5, [0, 0])), /unknown ConfigValue tag: 5/);
  });
});

describe("AuthorityRole declaration order", () => {
  it("decodes every AuthorityRole tag at its declared index", () => {
    const roles: Array<[number, string]> = [
      [0, "Administrator"],
      [1, "Operator"],
      [2, "T05"],
      [3, "T02"],
    ];
    for (const [tag, name] of roles) {
      const golden = bytes(EVENT_DISC.AuthorityRotated, leU64(1n), pkBytes(2), [tag], pkBytes(3), pkBytes(4));
      assert.equal(decodeAuthorityRotated(golden).role, name);
    }
  });

});

// ── Events ───────────────────────────────────────────────────────────────────────────────────

describe("ProtocolInitialized", () => {
  const golden = bytes(
    EVENT_DISC.ProtocolInitialized,
    leU64(1n),
    pkBytes(2),
    pkBytes(3),
    pkBytes(4),
    pkBytes(5),
    pkBytes(6),
    pkBytes(7),
    pkBytes(8),
    pkBytes(9),
    pkBytes(10),
    pkBytes(11),
    leU16(1234),
  );
  const expected = {
    slot: 1n,
    administrator: pkAddr(2),
    operator: pkAddr(3),
    t02Authority: pkAddr(4),
    t05Authority: pkAddr(5),
    protocolRevenue: pkAddr(6),
    usdcMint: pkAddr(7),
    byeMint: pkAddr(8),
    vrfProgram: pkAddr(9),
    oracleQueue: pkAddr(10),
    swapPool: pkAddr(11),
    maxSwapSlippageBps: 1234,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeProtocolInitialized(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .u64(expected.slot)
      .pubkey(expected.administrator)
      .pubkey(expected.operator)
      .pubkey(expected.t02Authority)
      .pubkey(expected.t05Authority)
      .pubkey(expected.protocolRevenue)
      .pubkey(expected.usdcMint)
      .pubkey(expected.byeMint)
      .pubkey(expected.vrfProgram)
      .pubkey(expected.oracleQueue)
      .pubkey(expected.swapPool)
      .u16(expected.maxSwapSlippageBps)
      .build();
    const data = Buffer.concat([eventDiscriminator("ProtocolInitialized"), args]);
    assert.deepEqual(decodeProtocolInitialized(data), expected);
  });
});

describe("PoolInitialized", () => {
  const golden = bytes(
    EVENT_DISC.PoolInitialized,
    pkBytes(1),
    leU64(2n),
    pkBytes(3),
    leU16(4),
    pkBytes(5),
    pkBytes(7),
    leU64(8n),
    leU64(9n),
    leU64(10n),
    leU16(11),
    leU16(12),
    leU16(13),
    leU64(14n),
    leU64(15n),
    leU64(16n),
    [17],
    leU16(18),
    leU64(19n),
    leU64(20n),
    leU64(21n),
    leU64(210n),
    leU64(211n),
    leU64(212n),
    leU64(213n),
    leU64(214n),
    leU64(22n),
    leU16(23),
    [24],
  );
  const expected = {
    pool: pkAddr(1),
    slot: 2n,
    authority: pkAddr(3),
    poolId: 4,
    weightIndex: pkAddr(5),
    treasury03: pkAddr(7),
    price: 8n,
    ticketTarget: 9n,
    fee: 10n,
    allocEqualBps: 11,
    allocTierBps: 12,
    allocProtocolBps: 13,
    admissionFloor: 14n,
    admissionCeiling: 15n,
    walletValueCap: 16n,
    maxN: 17,
    buybackRateBps: 18,
    blankFaces: [19n, 20n, 21n, 210n, 211n, 212n, 213n, 214n] as BlankFaces,
    t04Ceiling: 22n,
    sweepCadenceHours: 23,
    tierSize: 24,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodePoolInitialized(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .pubkey(expected.pool)
      .u64(expected.slot)
      .pubkey(expected.authority)
      .u16(expected.poolId)
      .pubkey(expected.weightIndex)
      .pubkey(expected.treasury03)
      .u64(expected.price)
      .u64(expected.ticketTarget)
      .u64(expected.fee)
      .u16(expected.allocEqualBps)
      .u16(expected.allocTierBps)
      .u16(expected.allocProtocolBps)
      .u64(expected.admissionFloor)
      .u64(expected.admissionCeiling)
      .u64(expected.walletValueCap)
      .u8(expected.maxN)
      .u16(expected.buybackRateBps)
      .arrayU64(expected.blankFaces, 8)
      .u64(expected.t04Ceiling)
      .u16(expected.sweepCadenceHours)
      .u8(expected.tierSize)
      .build();
    const data = Buffer.concat([eventDiscriminator("PoolInitialized"), args]);
    assert.deepEqual(decodePoolInitialized(data), expected);
  });
});

describe("ConfigChanged", () => {
  const golden = bytes(
    EVENT_DISC.ConfigChanged,
    pkBytes(1),
    leU64(2n),
    pkBytes(3),
    strBytes("sweep_cadence_hours"),
    [4],
    leU16(24), // old = Hours(24)
    [4],
    leU16(48), // new = Hours(48)
  );
  const expected = {
    pool: pkAddr(1),
    slot: 2n,
    authority: pkAddr(3),
    param: "sweep_cadence_hours",
    old: { kind: "Hours" as const, value: 24 },
    new: { kind: "Hours" as const, value: 48 },
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeConfigChanged(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .pubkey(expected.pool)
      .u64(expected.slot)
      .pubkey(expected.authority)
      .string(expected.param)
      .enumVariant(4, new Borsh().u16(expected.old.value).build())
      .enumVariant(4, new Borsh().u16(expected.new.value).build())
      .build();
    const data = Buffer.concat([eventDiscriminator("ConfigChanged"), args]);
    assert.deepEqual(decodeConfigChanged(data), expected);
  });
});

describe("PauseSet", () => {
  const golden = bytes(EVENT_DISC.PauseSet, pkBytes(1), leU64(2n), pkBytes(3), [1], [0], leU16(4));
  const expected = {
    pool: pkAddr(1),
    slot: 2n,
    authority: pkAddr(3),
    depositsPaused: true,
    rollsPaused: false,
    reason: 4,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodePauseSet(golden), expected);
  });
});

describe("AuthorityRotated", () => {
  const golden = bytes(EVENT_DISC.AuthorityRotated, leU64(1n), pkBytes(2), [1], pkBytes(3), pkBytes(4));
  const expected = {
    slot: 1n,
    authority: pkAddr(2),
    role: "Operator" as const,
    old: pkAddr(3),
    new: pkAddr(4),
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeAuthorityRotated(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .u64(expected.slot)
      .pubkey(expected.authority)
      .enumVariant(1) // Operator
      .pubkey(expected.old)
      .pubkey(expected.new)
      .build();
    const data = Buffer.concat([eventDiscriminator("AuthorityRotated"), args]);
    assert.deepEqual(decodeAuthorityRotated(data), expected);
  });
});

describe("DepositPending", () => {
  // One golden per admitted `standard`, not one golden at a chosen value. The discriminant is the
  // tail byte and the only field separating a `deposit` intake from a `deposit_core` one, so a
  // single fixture agrees with a decoder that never reads it and returns that fixture's value.
  for (const standard of [0, 1, 2] as const) {
    const golden = bytes(
      EVENT_DISC.DepositPending,
      pkBytes(1),
      leU64(2n),
      pkBytes(3),
      pkBytes(4),
      pkBytes(5),
      [standard],
    );
    const expected = {
      pool: pkAddr(1),
      slot: 2n,
      position: pkAddr(3),
      depositor: pkAddr(4),
      nftMint: pkAddr(5),
      standard,
    };

    it(`decodes fixed golden bytes to the expected struct with standard=${standard}`, () => {
      assert.deepEqual(decodeDepositPending(golden), expected);
    });
  }
});

describe("DepositApproved", () => {
  const golden = bytes(
    EVENT_DISC.DepositApproved,
    pkBytes(1),
    leU64(2n),
    pkBytes(3),
    pkBytes(4),
    leU64(5n),
    leI64(-6n),
    leI64(7n),
  );
  const expected = {
    pool: pkAddr(1),
    slot: 2n,
    authority: pkAddr(3),
    position: pkAddr(4),
    value: 5n,
    observedAt: -6n,
    observedAgeSeconds: 7n,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeDepositApproved(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .pubkey(expected.pool)
      .u64(expected.slot)
      .pubkey(expected.authority)
      .pubkey(expected.position)
      .u64(expected.value)
      .i64(expected.observedAt)
      .i64(expected.observedAgeSeconds)
      .build();
    const data = Buffer.concat([eventDiscriminator("DepositApproved"), args]);
    assert.deepEqual(decodeDepositApproved(data), expected);
  });
});

describe("RebalanceEvaluated", () => {
  const golden = bytes(
    EVENT_DISC.RebalanceEvaluated,
    pkBytes(1),
    leU64(2n),
    leU32(3),
    leU128(4n),
    ...[5, 6, 7, 50, 51, 52, 53, 54].map(leU32),
    ...[8, 9, 10, 80, 81, 82, 83, 84].map(leU32),
  );
  const expected = {
    pool: pkAddr(1),
    slot: 2n,
    nReal: 3,
    wReal: 4n,
    blanksBefore: [5, 6, 7, 50, 51, 52, 53, 54] as BlankCounts,
    blanksAfter: [8, 9, 10, 80, 81, 82, 83, 84] as BlankCounts,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeRebalanceEvaluated(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .pubkey(expected.pool)
      .u64(expected.slot)
      .u32(expected.nReal)
      .u128(expected.wReal)
      .arrayU32(expected.blanksBefore, 8)
      .arrayU32(expected.blanksAfter, 8)
      .build();
    const data = Buffer.concat([eventDiscriminator("RebalanceEvaluated"), args]);
    assert.deepEqual(decodeRebalanceEvaluated(data), expected);
  });
});

describe("TierChanged", () => {
  it("decodes Some(left) / None(entered)", () => {
    const golden = bytes(EVENT_DISC.TierChanged, pkBytes(1), leU64(2n), [1], pkBytes(3), [0]);
    assert.deepEqual(decodeTierChanged(golden), { pool: pkAddr(1), slot: 2n, left: pkAddr(3), entered: null });
  });

  it("decodes None(left) / Some(entered)", () => {
    const golden = bytes(EVENT_DISC.TierChanged, pkBytes(1), leU64(2n), [0], [1], pkBytes(4));
    assert.deepEqual(decodeTierChanged(golden), { pool: pkAddr(1), slot: 2n, left: null, entered: pkAddr(4) });
  });

  it("round-trips through the existing Borsh writer", () => {
    const expected = { pool: pkAddr(1), slot: 2n, left: pkAddr(3), entered: null };
    const args = new Borsh()
      .pubkey(expected.pool)
      .u64(expected.slot)
      .optionPubkey(expected.left)
      .optionPubkey(expected.entered)
      .build();
    const data = Buffer.concat([eventDiscriminator("TierChanged"), args]);
    assert.deepEqual(decodeTierChanged(data), expected);
  });
});

describe("DepositRejected", () => {
  const golden = bytes(EVENT_DISC.DepositRejected, pkBytes(1), leU64(2n), pkBytes(3), pkBytes(4), leU16(5));
  const expected = { pool: pkAddr(1), slot: 2n, authority: pkAddr(3), position: pkAddr(4), reason: 5 };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeDepositRejected(golden), expected);
  });
});

describe("NftReturned", () => {
  const golden = bytes(EVENT_DISC.NftReturned, pkBytes(1), leU64(2n), pkBytes(3), pkBytes(4), pkBytes(5));
  const expected = { pool: pkAddr(1), slot: 2n, position: pkAddr(3), depositor: pkAddr(4), nftMint: pkAddr(5) };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeNftReturned(golden), expected);
  });
});

describe("Withdrawn", () => {
  const golden = bytes(EVENT_DISC.Withdrawn, pkBytes(1), leU64(2n), pkBytes(3), pkBytes(4), leU64(5n));
  const expected = { pool: pkAddr(1), slot: 2n, position: pkAddr(3), depositor: pkAddr(4), feesPaid: 5n };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeWithdrawn(golden), expected);
  });
});

describe("NftClaimed", () => {
  const golden = bytes(EVENT_DISC.NftClaimed, pkBytes(1), leU64(2n), pkBytes(3), pkBytes(4), pkBytes(5), leU64(6n));
  const expected = {
    pool: pkAddr(1), slot: 2n, position: pkAddr(3), depositor: pkAddr(4), nftMint: pkAddr(5), feesPaid: 6n,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeNftClaimed(golden), expected);
  });

  // The decoder reads the current shape only, as every decoder here does: the pre-upgrade
  // payload, which ends at `nft_mint`, is refused rather than read with a defaulted amount.
  it("refuses the pre-upgrade payload that ends at nft_mint", () => {
    const legacy = bytes(EVENT_DISC.NftClaimed, pkBytes(1), leU64(2n), pkBytes(3), pkBytes(4), pkBytes(5));
    assert.throws(() => decodeNftClaimed(legacy));
  });
});

describe("FeeClaimed", () => {
  const golden = bytes(EVENT_DISC.FeeClaimed, pkBytes(1), leU64(2n), pkBytes(3), pkBytes(4), leU64(5n));
  const expected = { pool: pkAddr(1), slot: 2n, position: pkAddr(3), depositor: pkAddr(4), feesPaid: 5n };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeFeeClaimed(golden), expected);
  });
});

describe("ValueRecorded", () => {
  const golden = bytes(
    EVENT_DISC.ValueRecorded,
    pkBytes(1),
    leU64(2n),
    pkBytes(3),
    pkBytes(4),
    leU64(5n),
    leU64(6n),
    leI64(-7n),
  );
  const expected = {
    pool: pkAddr(1),
    slot: 2n,
    authority: pkAddr(3),
    position: pkAddr(4),
    old: 5n,
    new: 6n,
    observedAt: -7n,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeValueRecorded(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .pubkey(expected.pool)
      .u64(expected.slot)
      .pubkey(expected.authority)
      .pubkey(expected.position)
      .u64(expected.old)
      .u64(expected.new)
      .i64(expected.observedAt)
      .build();
    const data = Buffer.concat([eventDiscriminator("ValueRecorded"), args]);
    assert.deepEqual(decodeValueRecorded(data), expected);
  });
});

describe("BelowFloorClosed", () => {
  const golden = bytes(EVENT_DISC.BelowFloorClosed, pkBytes(1), leU64(2n), pkBytes(3), pkBytes(4), pkBytes(5));
  const expected = { pool: pkAddr(1), slot: 2n, authority: pkAddr(3), position: pkAddr(4), depositor: pkAddr(5) };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeBelowFloorClosed(golden), expected);
  });
});

describe("SweepBegun", () => {
  const golden = bytes(EVENT_DISC.SweepBegun, pkBytes(1), leU64(2n), pkBytes(3), leU64(4n));
  const expected = { pool: pkAddr(1), slot: 2n, authority: pkAddr(3), sweepEpoch: 4n };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeSweepBegun(golden), expected);
  });
});

describe("SweepEnded", () => {
  const golden = bytes(
    EVENT_DISC.SweepEnded,
    pkBytes(1),
    leU64(2n),
    pkBytes(3),
    leU64(4n),
    leU32(5),
    leI64(-6n),
  );
  const expected = {
    pool: pkAddr(1),
    slot: 2n,
    authority: pkAddr(3),
    sweepEpoch: 4n,
    positionsUpdated: 5,
    lastSweepAt: -6n,
  };

  it("decodes fixed golden bytes to the expected struct", () => {
    assert.deepEqual(decodeSweepEnded(golden), expected);
  });

  it("round-trips through the existing Borsh writer", () => {
    const args = new Borsh()
      .pubkey(expected.pool)
      .u64(expected.slot)
      .pubkey(expected.authority)
      .u64(expected.sweepEpoch)
      .u32(expected.positionsUpdated)
      .i64(expected.lastSweepAt)
      .build();
    const data = Buffer.concat([eventDiscriminator("SweepEnded"), args]);
    assert.deepEqual(decodeSweepEnded(data), expected);
  });
});

// ── Event dispatch ───────────────────────────────────────────────────────────────────────────

describe("decodeEvent", () => {
  it("dispatches to the right decoder by discriminator alone", () => {
    const withdrawnGolden = bytes(EVENT_DISC.Withdrawn, pkBytes(1), leU64(2n), pkBytes(3), pkBytes(4), leU64(5n));
    const decoded = decodeEvent(withdrawnGolden);
    assert.equal(decoded.name, "Withdrawn");
    assert.deepEqual(decoded.data, {
      pool: pkAddr(1),
      slot: 2n,
      position: pkAddr(3),
      depositor: pkAddr(4),
      feesPaid: 5n,
    });
  });

  it("dispatches FeeClaimed by discriminator alone", () => {
    const feeClaimedGolden = bytes(EVENT_DISC.FeeClaimed, pkBytes(1), leU64(2n), pkBytes(3), pkBytes(4), leU64(5n));
    const decoded = decodeEvent(feeClaimedGolden);
    assert.equal(decoded.name, "FeeClaimed");
    assert.deepEqual(decoded.data, {
      pool: pkAddr(1),
      slot: 2n,
      position: pkAddr(3),
      depositor: pkAddr(4),
      feesPaid: 5n,
    });
  });

  it("throws on an unrecognized discriminator", () => {
    assert.throws(() => decodeEvent(Buffer.alloc(16, 0xff)), /unknown event discriminator/);
  });
});

describe("RollBatch — decoded by length, in either layout", () => {
  const legacyFile = (): Buffer =>
    Buffer.from(
      (JSON.parse(readFileSync("tests/fixtures/legacy-batch.json", "utf8")) as {
        account: { data: [string, string] };
      }).account.data[0],
      "base64",
    );

  it("reads the committed 1,100-byte batch, its three-slot arrays widened with zeros", async () => {
    const data = legacyFile();
    assert.equal(data.length, LEGACY_ROLL_BATCH_LEN);
    const { pool, roller } = await legacyBatchAccount();
    const batch = decodeRollBatch(data);
    assert.equal(batch.pool, pool);
    assert.equal(batch.roller, roller);
    assert.equal(batch.batchId, 7n);
    assert.equal(batch.state, "Complete");
    assert.equal(batch.expectedW, 7_533_333_333_320n);
    assert.deepEqual(batch.expectedBlanks, [3, 2, 1, 0, 0, 0, 0, 0]);
    assert.deepEqual(batch.sAllocBps, [7_500, 500, 2_000]);
    assert.deepEqual(batch.sBlankFaces, [1_000_000n, 2_000_000n, 5_000_000n, 0n, 0n, 0n, 0n, 0n]);
    assert.equal(batch.rolls.length, 10);
    assert.equal(batch.rolls[0]!.status, "Settled");
    assert.equal(batch.rolls[0]!.drawValue, 4_029_087_020_998n);
    assert.equal(batch.rolls[0]!.nRealAtDraw, 40);
    assert.deepEqual(batch.rolls[0]!.blanksAtDraw, [3, 2, 1, 0, 0, 0, 0, 0]);
    assert.equal(batch.rolls[0]!.refundReason, 0);
    assert.equal(batch.rolls[9]!.status, "Pending");
    assert.equal(batch.bump, data[data.length - 1]);
  });

  it("reads a 1,360-byte batch in today's layout with every slot of every array", async () => {
    const legacy = decodeRollBatch(legacyFile());
    const record = (i: number): Buffer =>
      new Borsh()
        .u8(i === 0 ? 1 : 0)
        .u8(i === 0 ? 2 : 0)
        .pubkey(legacy.pool)
        .u8(i === 0 ? 3 : 0)
        .u128(5n)
        .u8(1)
        .u128(6n)
        .u32(7)
        .arrayU32(i === 0 ? [11, 12, 13, 14, 15, 16, 17, 18] : [0, 0, 0, 0, 0, 0, 0, 0], 8)
        .u16(0)
        .build();
    const body = new Borsh()
      .pubkey(legacy.pool)
      .pubkey(legacy.roller)
      .u64(42n)
      .u8(3)
      .u8(1)
      .u8(1)
      .u8(1)
      .u8(0)
      .u64(100n)
      .fixed(Buffer.alloc(32, 1), 32)
      .fixed(Buffer.alloc(32, 2), 32)
      .u128(99n)
      .arrayU32([1, 2, 3, 4, 5, 6, 7, 8], 8)
      .u64(49_500_000n)
      .u64(45_000_000n)
      .u64(4_500_000n)
      .u16(7_500)
      .u16(500)
      .u16(2_000)
      .arrayU64([5_000_000n, 10_000_000n, 25_000_000n, 100_000_000n, 0n, 0n, 0n, 0n], 8);
    for (let i = 0; i < 10; i += 1) body.bytes(record(i));
    body.u8(254);
    const data = Buffer.concat([accountDiscriminator("RollBatch"), body.build()]);
    assert.equal(data.length, ROLL_BATCH_LEN);

    const batch = decodeRollBatch(data);
    assert.equal(batch.batchId, 42n);
    assert.deepEqual(batch.expectedBlanks, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert.deepEqual(batch.sBlankFaces, [5_000_000n, 10_000_000n, 25_000_000n, 100_000_000n, 0n, 0n, 0n, 0n]);
    assert.equal(batch.rolls[0]!.outcome, "Blank");
    assert.equal(batch.rolls[0]!.blankFaceIdx, 3);
    assert.deepEqual(batch.rolls[0]!.blanksAtDraw, [11, 12, 13, 14, 15, 16, 17, 18]);
    assert.equal(batch.bump, 254);
  });

  it("refuses any other length rather than reading it against the wrong layout", () => {
    const data = legacyFile();
    for (const len of [LEGACY_ROLL_BATCH_LEN - 1, LEGACY_ROLL_BATCH_LEN + 1, ROLL_BATCH_LEN - 1, ROLL_BATCH_LEN + 1]) {
      const sized = Buffer.alloc(len);
      data.copy(sized, 0, 0, Math.min(len, data.length));
      assert.throws(() => decodeRollBatch(sized), /expected 1360 or 1100/, `length ${len}`);
    }
  });
});
