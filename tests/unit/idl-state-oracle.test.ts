// Extends the IDL-driven differential oracle to the 6 accounts and 18
// events decoders.ts hand-mirrors from the Rust state structs and event definitions. The
// IDL fully describes both, and a hand-mirror typo in decoders.ts does not move the IDL — this
// is what makes that mirror more than a same-source round-trip with itself.
//
// Each fixture is built generically from the IDL's own field list (encodeStateBody, in
// idl-oracle-support.ts) and prefixed with the account/event discriminator, then decoded by the
// production decoder and asserted equal, field for field, to the exact object that produced it.
// A field-order, field-width or field-name mismatch between decoders.ts and the IDL fails this
// equality; the discriminator derivation itself is already covered independently
// (tests/unit/codec.test.ts, tests/unit/decoders.test.ts's IDL-literal cross-check).
//
// Same two limits as tests/unit/idl-oracle.test.ts: this proves client↔program agreement, never
// program↔spec correctness, and it cannot see a source-level reorder that moves the Rust struct,
// the regenerated IDL and this file's fixture together — that shows up as a diff of the committed
// IDL in review, not as a failure here.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { getAddressDecoder, type Address } from "@solana/kit";
import { accountDiscriminator, eventDiscriminator } from "../../scripts/lib/anchor.js";
import {
  decodeProtocolConfig,
  decodePool,
  decodePosition,
  decodeCollectionAdmitted,
  decodeCollectionWithdrawn,
  decodePoolCollection,
  decodePositionSeized,
  decodeWalletStats,
  decodeEscrow,
  decodeEscrowOpened,
  decodeEscrowDeposited,
  decodeEscrowWithdrawn,
  decodeEscrowArmed,
  decodeTopTier,
  decodeWeightIndex,
  decodeRollBatch,
  decodeProtocolInitialized,
  decodePoolInitialized,
  decodeConfigChanged,
  decodeVrfConfigChanged,
  decodeByeMintChanged,
  decodeProtocolRevenueChanged,
  decodeSwapPoolChanged,
  decodePauseSet,
  decodeAuthorityRotated,
  decodeDepositPending,
  decodeDepositApproved,
  decodeRebalanceEvaluated,
  decodeTierChanged,
  decodeDepositRejected,
  decodeNftReturned,
  decodeFeeClaimed,
  decodeWithdrawn,
  decodeNftClaimed,
  decodeValueRecorded,
  decodeBelowFloorClosed,
  decodeSweepBegun,
  decodeSweepEnded,
  decodeBatchCommitted,
  decodeBatchResolved,
  decodeRollSettled,
  decodeRollRefunded,
  decodeBatchCompleted,
  decodeBatchRecovered,
  decodeBatchClosed,
  decodeAcquiredActivated,
  decodeT04CeilingOverflow,
  decodeT03Transferred,
  decodeT04Transferred,
  decodeTreasury03Migrated,
  decodeT04Swept,
  decodeT04CeilingSet,
  decodeTreasuriesConsolidated,
  decodeInstantSold,
  type BlankCounts,
  type BlankFaces,
} from "../../scripts/lib/decoders.js";
import { WEIGHT_INDEX_ACCOUNT_LEN } from "../../scripts/lib/weight-index.js";
import {
  loadIdl,
  resolveDefined,
  encodeStateBody,
  idlFixedByteSize,
  type IdlField,
} from "./idl-oracle-support.js";

const idl = loadIdl();
const addrDec = getAddressDecoder();

function dummy(n: number): Address {
  return addrDec.decode(Buffer.alloc(32, n));
}

function fullAccount(name: string, value: unknown): Buffer {
  return Buffer.concat([accountDiscriminator(name), encodeStateBody(idl, name, value)]);
}
function fullEvent(name: string, value: unknown): Buffer {
  return Buffer.concat([eventDiscriminator(name), encodeStateBody(idl, name, value)]);
}

const TESTED_ACCOUNTS = new Set<string>();
const TESTED_EVENTS = new Set<string>();

// ── Accounts ─────────────────────────────────────────────────────────────────────────────────

describe("ProtocolConfig (IDL oracle)", () => {
  TESTED_ACCOUNTS.add("ProtocolConfig");
  it("decodes byte-for-byte what a generic IDL encoding of the same fields produced", () => {
    const value = {
      administrator: dummy(1),
      t04Ceiling: 0x0102_0304_0506_0708n,
      reserved0: Uint8Array.from({ length: 24 }, (_, i) => 0x40 | i),
      reserved1: dummy(3),
      reserved2: dummy(4),
      reserved3: dummy(5),
      operator: dummy(6),
      t05Authority: dummy(7),
      t02Authority: dummy(8),
      protocolRevenue: dummy(9),
      usdcMint: dummy(10),
      byeMint: dummy(11),
      vrfProgram: dummy(12),
      oracleQueue: dummy(13),
      swapPool: dummy(14),
      maxSwapSlippageBps: 250,
      poolCounter: 3,
      bump: 255,
    };
    const decoded = decodeProtocolConfig(fullAccount("ProtocolConfig", value));
    assert.deepEqual(decoded, value);
  });
});

describe("Pool (IDL oracle)", () => {
  TESTED_ACCOUNTS.add("Pool");
  it("decodes byte-for-byte what a generic IDL encoding of the same fields produced", () => {
    const value = {
      poolId: 7,
      weightIndex: dummy(1),
      treasury03: dummy(3),
      price: 9_900_000n,
      ticketTarget: 9_000_000n,
      fee: 900_000n,
      allocEqualBps: 7_500,
      allocTierBps: 500,
      allocProtocolBps: 2_000,
      admissionFloor: 12_000_000n,
      admissionCeiling: 100_000_000n,
      walletValueCap: 300_000_000n,
      maxN: 10,
      buybackRateBps: 8_500,
      blankFaces: [1_000_000n, 2_000_000n, 5_000_000n, 100_000_000n, 51n, 52n, 53n, 54n] as BlankFaces,
      t04Ceiling: 2_000_000_000n,
      sweepCadenceHours: 24,
      tierSize: 20,
      depositsPaused: true,
      rollsPaused: false,
      pauseReason: 3,
      nReal: 42,
      blanks: [1, 2, 3, 4, 5, 6, 7, 8] as BlankCounts,
      wReal: 123_456_789_012_345n,
      openBatches: 5,
      batchCounter: 99n,
      positionCounter: 1_234n,
      pendingRollLiability: 55_000n,
      owedFees: 1_000n,
      accEqual: 987_654_321_098n,
      sweepPending: true,
      sweepEpoch: 8n,
      lastSweepAt: -12345n,
      sweepUpdates: 6,
      bump: 254,
      escrowBidBps: 9_250,
      treasury04: dummy(4),
      reserved: Uint8Array.from({ length: 32 }, (_, i) => 0x80 | i),
    };
    const decoded = decodePool(fullAccount("Pool", value));
    assert.deepEqual(decoded, value);
  });
});

describe("Position (IDL oracle)", () => {
  TESTED_ACCOUNTS.add("Position");
  for (const state of ["Pending", "Active", "ClosedBelowFloor", "Rejected", "Seized"] as const) {
    it(`decodes byte-for-byte for state=${state}`, () => {
      const value = {
        pool: dummy(1),
        depositor: dummy(2),
        nftMint: dummy(3),
        positionId: 42n,
        state,
        recordedValue: 12_000_000n,
        valueObservedAt: 1_800_000_000n,
        depositValue: 11_000_000n,
        slotIndex: 5,
        activatedAt: 1_800_000_100n,
        equalCheckpoint: 100n,
        accrued: 200n,
        inTier: true,
        tierCheckpoint: 300n,
        lockUntil: -1n,
        rejectReason: 0,
        standard: 2,
        bump: 253,
        vaultBump: 252,
      };
      const decoded = decodePosition(fullAccount("Position", value));
      assert.deepEqual(decoded, value);
    });
  }
});

describe("PoolCollection (IDL oracle)", () => {
  TESTED_ACCOUNTS.add("PoolCollection");
  it("decodes byte-for-byte what a generic IDL encoding of the same fields produced", () => {
    const value = {
      pool: dummy(1),
      collection: dummy(2),
      standards: 0b101,
      admittedAt: 1_700_000_000n,
      bump: 254,
    };
    const decoded = decodePoolCollection(fullAccount("PoolCollection", value));
    assert.deepEqual(decoded, value);
  });
});

describe("WalletStats (IDL oracle)", () => {
  TESTED_ACCOUNTS.add("WalletStats");
  it("decodes byte-for-byte what a generic IDL encoding of the same fields produced", () => {
    const value = {
      pool: dummy(1),
      owner: dummy(2),
      activePositions: 3,
      activeValue: 36_000_000n,
      bump: 251,
    };
    const decoded = decodeWalletStats(fullAccount("WalletStats", value));
    assert.deepEqual(decoded, value);
  });
});

describe("Escrow (IDL oracle)", () => {
  TESTED_ACCOUNTS.add("Escrow");
  it("decodes byte-for-byte, reserved bytes included", () => {
    const value = {
      owner: dummy(1),
      armed: true,
      bump: 254,
      vaultBump: 253,
      reserved: Uint8Array.from({ length: 64 }, (_, i) => 0x80 | i),
    };
    assert.deepEqual(decodeEscrow(fullAccount("Escrow", value)), value);
  });
});

describe("TopTier (IDL oracle)", () => {
  TESTED_ACCOUNTS.add("TopTier");
  it("decodes only entries[0..len), the fixed [TierEntry; 20] tail is not included", () => {
    const len = 7;
    const entries = Array.from({ length: 20 }, (_, i) => ({
      position: dummy(i + 1),
      value: BigInt(1_000_000 * (i + 1)),
      activatedAt: 1_800_000_000n + BigInt(i),
      positionId: BigInt(i),
    }));
    const value = { pool: dummy(99), len, entries, accTier: 55_000_000n, bump: 250 };
    const decoded = decodeTopTier(fullAccount("TopTier", value));
    assert.equal(decoded.entries.length, len);
    assert.deepEqual(decoded, { ...value, entries: entries.slice(0, len) });
  });

  it("self-test: decoding fewer than the full 20 fixture entries is not what the encoder produced", () => {
    const len = 7;
    const entries = Array.from({ length: 20 }, (_, i) => ({
      position: dummy(i + 1),
      value: BigInt(1_000_000 * (i + 1)),
      activatedAt: 1_800_000_000n + BigInt(i),
      positionId: BigInt(i),
    }));
    const value = { pool: dummy(99), len, entries, accTier: 55_000_000n, bump: 250 };
    const decoded = decodeTopTier(fullAccount("TopTier", value));
    assert.notDeepEqual(decoded.entries, entries.slice(0, len - 1));
  });
});

describe("WeightIndex (IDL oracle)", () => {
  TESTED_ACCOUNTS.add("WeightIndex");

  it("its bytemuck/repr(C) body size, summed from the IDL's own field list, matches WEIGHT_INDEX_ACCOUNT_LEN", () => {
    const fields = (resolveDefined(idl, "WeightIndex").type as { kind: "struct"; fields: IdlField[] }).fields;
    const bodySize = fields.reduce((sum, f) => sum + idlFixedByteSize(f.type), 0);
    assert.equal(8 + bodySize, WEIGHT_INDEX_ACCOUNT_LEN);
  });

  it("decodes byte-for-byte what a generic sequential encoding of its IDL field list produced", () => {
    const FENWICK_CAPACITY = 16_384;
    const pool = dummy(1);
    const totalWeight = 123_456_789n;
    const highWater = 42;
    const freeCount = 3;
    const tree = Array.from({ length: FENWICK_CAPACITY }, (_, i) => BigInt(i));
    const freeStack = Array.from({ length: FENWICK_CAPACITY }, (_, i) => FENWICK_CAPACITY - i);

    const value = {
      pool,
      totalWeight,
      highWater,
      freeCount,
      // `_padding` is a real field in the IDL's own layout (state/weight_index.rs), between
      // free_count and tree — 8 zero bytes, not skipped. `snakeToCamel("_padding")` is
      // "Padding" (the leading underscore is consumed by the same "_x" -> "X" rule as every
      // other field), so that is the fixture key, not "_padding" itself.
      Padding: new Array(8).fill(0),
      tree,
      freeStack,
    };
    const decoded = decodeWeightIndex(fullAccount("WeightIndex", value));
    assert.deepEqual(decoded, { pool, totalWeight, highWater, freeCount, tree, freeStack });
  });
});

describe("RollBatch (IDL oracle)", () => {
  TESTED_ACCOUNTS.add("RollBatch");

  function rollRecord(i: number) {
    return {
      status: (["Pending", "Settled", "Refunded"] as const)[i % 3],
      outcome: (["None", "RealCard", "Blank"] as const)[i % 3],
      selectedMint: dummy(i + 1),
      blankFaceIdx: i,
      drawValue: 1_000n + BigInt(i),
      attempts: i + 1,
      wAtDraw: 2_000n + BigInt(i),
      nRealAtDraw: 100 + i,
      blanksAtDraw: [i, i + 1, i + 2, i + 3, i + 4, i + 5, i + 6, i + 7] as BlankCounts,
      refundReason: i,
    };
  }

  it("decodes byte-for-byte what a generic IDL encoding of the same fields produced", () => {
    const rolls = Array.from({ length: 10 }, (_, i) => rollRecord(i));
    const value = {
      pool: dummy(1),
      roller: dummy(2),
      batchId: 9_001n,
      state: "Resolved",
      n: 7,
      nextRoll: 3,
      settled: 2,
      refunded: 1,
      requestSlot: 123_456_789n,
      callerSeed: Uint8Array.from({ length: 32 }, (_, i) => i),
      randomness: Uint8Array.from({ length: 32 }, (_, i) => 255 - i),
      expectedW: 55_555_555n,
      expectedBlanks: [4, 5, 6, 7, 8, 9, 10, 11] as BlankCounts,
      sPrice: 10_000_000n,
      sTicket: 8_500_000n,
      sFee: 1_500_000n,
      sAllocBps: [7_500, 500, 2_000] as [number, number, number],
      sBlankFaces: [1_000_000n, 2_000_000n, 5_000_000n, 100_000_000n, 51n, 52n, 53n, 54n] as BlankFaces,
      rolls,
      bump: 254,
    };
    const decoded = decodeRollBatch(fullAccount("RollBatch", value));
    assert.deepEqual(decoded, value);
  });

  it("self-test: catches a swapped same-width field (pool ⇄ roller, both pubkeys)", () => {
    const rolls = Array.from({ length: 10 }, (_, i) => rollRecord(i));
    const value = {
      pool: dummy(1),
      roller: dummy(2),
      batchId: 9_001n,
      state: "Resolved",
      n: 7,
      nextRoll: 3,
      settled: 2,
      refunded: 1,
      requestSlot: 123_456_789n,
      callerSeed: Uint8Array.from({ length: 32 }, (_, i) => i),
      randomness: Uint8Array.from({ length: 32 }, (_, i) => 255 - i),
      expectedW: 55_555_555n,
      expectedBlanks: [4, 5, 6, 7, 8, 9, 10, 11] as BlankCounts,
      sPrice: 10_000_000n,
      sTicket: 8_500_000n,
      sFee: 1_500_000n,
      sAllocBps: [7_500, 500, 2_000] as [number, number, number],
      sBlankFaces: [1_000_000n, 2_000_000n, 5_000_000n, 100_000_000n, 51n, 52n, 53n, 54n] as BlankFaces,
      rolls,
      bump: 254,
    };
    const decoded = decodeRollBatch(fullAccount("RollBatch", value));
    const swapped = { ...value, pool: value.roller, roller: value.pool };
    assert.notDeepEqual(decoded, swapped);
  });
});

// ── Events ───────────────────────────────────────────────────────────────────────────────────

describe("ProtocolInitialized (IDL oracle)", () => {
  TESTED_EVENTS.add("ProtocolInitialized");
  it("decodes byte-for-byte", () => {
    const value = {
      slot: 111n,
      administrator: dummy(1),
      operator: dummy(2),
      t02Authority: dummy(3),
      t05Authority: dummy(4),
      protocolRevenue: dummy(5),
      usdcMint: dummy(6),
      byeMint: dummy(7),
      vrfProgram: dummy(8),
      oracleQueue: dummy(9),
      swapPool: dummy(10),
      maxSwapSlippageBps: 250,
    };
    assert.deepEqual(decodeProtocolInitialized(fullEvent("ProtocolInitialized", value)), value);
  });
});

describe("PoolInitialized (IDL oracle)", () => {
  TESTED_EVENTS.add("PoolInitialized");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1),
      slot: 222n,
      authority: dummy(2),
      poolId: 7,
      weightIndex: dummy(3),
      treasury03: dummy(5),
      price: 9_900_000n,
      ticketTarget: 9_000_000n,
      fee: 900_000n,
      allocEqualBps: 7_500,
      allocTierBps: 500,
      allocProtocolBps: 2_000,
      admissionFloor: 12_000_000n,
      admissionCeiling: 0n,
      walletValueCap: 0n,
      maxN: 10,
      buybackRateBps: 8_500,
      blankFaces: [1_000_000n, 2_000_000n, 5_000_000n, 100_000_000n, 51n, 52n, 53n, 54n] as BlankFaces,
      t04Ceiling: 2_000_000_000n,
      sweepCadenceHours: 24,
      tierSize: 20,
    };
    assert.deepEqual(decodePoolInitialized(fullEvent("PoolInitialized", value)), value);
  });
});

describe("ConfigChanged (IDL oracle)", () => {
  TESTED_EVENTS.add("ConfigChanged");
  const cases: Array<{ old: unknown; new: unknown }> = [
    { old: { kind: "Amount", value: 100n }, new: { kind: "Amount", value: 200n } },
    { old: { kind: "Bps", value: 500 }, new: { kind: "Bps", value: 600 } },
    { old: { kind: "Count", value: 1 }, new: { kind: "Count", value: 2 } },
    {
      old: { kind: "Faces", value: [1n, 2n, 3n, 4n, 5n, 6n, 7n, 8n] },
      new: { kind: "Faces", value: [9n, 10n, 11n, 12n, 13n, 14n, 15n, 16n] },
    },
    { old: { kind: "Hours", value: 24 }, new: { kind: "Hours", value: 12 } },
  ];
  for (const { old, new: neu } of cases) {
    it(`decodes byte-for-byte for ConfigValue::${(old as { kind: string }).kind}`, () => {
      const value = { pool: dummy(1), slot: 333n, authority: dummy(2), param: "buyback_rate_bps", old, new: neu };
      assert.deepEqual(decodeConfigChanged(fullEvent("ConfigChanged", value)), value);
    });
  }
});

describe("VrfConfigChanged (IDL oracle)", () => {
  TESTED_EVENTS.add("VrfConfigChanged");
  it("decodes byte-for-byte", () => {
    const value = {
      slot: 555n,
      authority: dummy(1),
      oldVrfProgram: dummy(2),
      oldOracleQueue: dummy(3),
      oldSwapPool: dummy(4),
      vrfProgram: dummy(5),
      oracleQueue: dummy(6),
      swapPool: dummy(7),
      oldMaxSwapSlippageBps: 250,
      maxSwapSlippageBps: 100,
    };
    assert.deepEqual(decodeVrfConfigChanged(fullEvent("VrfConfigChanged", value)), value);
  });
});

describe("ByeMintChanged (IDL oracle)", () => {
  TESTED_EVENTS.add("ByeMintChanged");
  it("decodes byte-for-byte", () => {
    const value = { slot: 555n, authority: dummy(1), oldByeMint: dummy(2), byeMint: dummy(3) };
    assert.deepEqual(decodeByeMintChanged(fullEvent("ByeMintChanged", value)), value);
  });
});

describe("SwapPoolChanged (IDL oracle)", () => {
  TESTED_EVENTS.add("SwapPoolChanged");
  it("decodes byte-for-byte", () => {
    const value = { slot: 555n, authority: dummy(1), oldSwapPool: dummy(2), swapPool: dummy(3) };
    assert.deepEqual(decodeSwapPoolChanged(fullEvent("SwapPoolChanged", value)), value);
  });
});

describe("ProtocolRevenueChanged (IDL oracle)", () => {
  TESTED_EVENTS.add("ProtocolRevenueChanged");
  it("decodes byte-for-byte", () => {
    const value = { slot: 555n, authority: dummy(1), oldProtocolRevenue: dummy(2), protocolRevenue: dummy(3) };
    assert.deepEqual(decodeProtocolRevenueChanged(fullEvent("ProtocolRevenueChanged", value)), value);
  });
});

describe("PauseSet (IDL oracle)", () => {
  TESTED_EVENTS.add("PauseSet");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 444n, authority: dummy(2), depositsPaused: true, rollsPaused: false, reason: 9 };
    assert.deepEqual(decodePauseSet(fullEvent("PauseSet", value)), value);
  });
});

describe("AuthorityRotated (IDL oracle)", () => {
  TESTED_EVENTS.add("AuthorityRotated");
  for (const role of ["Administrator", "Operator", "T05", "T02"] as const) {
    it(`decodes byte-for-byte for role=${role}`, () => {
      const value = { slot: 555n, authority: dummy(1), role, old: dummy(2), new: dummy(3) };
      assert.deepEqual(decodeAuthorityRotated(fullEvent("AuthorityRotated", value)), value);
    });
  }
});

describe("DepositPending (IDL oracle)", () => {
  TESTED_EVENTS.add("DepositPending");
  // Every `standard` the program admits, on `InstantSold`'s reasoning — the other event carrying
  // the discriminant. Sweeping the domain rather than pinning one value is what separates a
  // decoder that reads the tail byte from one that returns a constant: a lone `standard=2` case
  // passes against a hardcoded 2, and 2 is the Core value the custody monitor sweeps on.
  for (const standard of [0, 1, 2] as const) {
    it(`decodes byte-for-byte with standard=${standard}`, () => {
      const value = {
        pool: dummy(1), slot: 666n, position: dummy(2), depositor: dummy(3),
        nftMint: dummy(4), standard,
      };
      assert.deepEqual(decodeDepositPending(fullEvent("DepositPending", value)), value);
    });
  }
});

describe("DepositApproved (IDL oracle)", () => {
  TESTED_EVENTS.add("DepositApproved");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 777n, authority: dummy(2), position: dummy(3),
      value: 12_000_000n, observedAt: 1_800_000_000n, observedAgeSeconds: 30n,
    };
    assert.deepEqual(decodeDepositApproved(fullEvent("DepositApproved", value)), value);
  });
});

describe("RebalanceEvaluated (IDL oracle)", () => {
  TESTED_EVENTS.add("RebalanceEvaluated");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 888n, nReal: 41, wReal: 999_999_999_999n,
      blanksBefore: [1, 2, 3, 4, 5, 6, 7, 8] as BlankCounts,
      blanksAfter: [0, 1, 2, 3, 4, 5, 6, 7] as BlankCounts,
    };
    assert.deepEqual(decodeRebalanceEvaluated(fullEvent("RebalanceEvaluated", value)), value);
  });
});

describe("TierChanged (IDL oracle)", () => {
  TESTED_EVENTS.add("TierChanged");
  const cases: Array<{ left: Address | null; entered: Address | null }> = [
    { left: dummy(1), entered: dummy(2) },
    { left: null, entered: dummy(3) },
    { left: dummy(4), entered: null },
    { left: null, entered: null },
  ];
  for (const { left, entered } of cases) {
    it(`decodes byte-for-byte for left=${left}, entered=${entered}`, () => {
      const value = { pool: dummy(9), slot: 999n, left, entered };
      assert.deepEqual(decodeTierChanged(fullEvent("TierChanged", value)), value);
    });
  }
});

describe("DepositRejected (IDL oracle)", () => {
  TESTED_EVENTS.add("DepositRejected");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 1010n, authority: dummy(2), position: dummy(3), reason: 5 };
    assert.deepEqual(decodeDepositRejected(fullEvent("DepositRejected", value)), value);
  });
});

describe("NftReturned (IDL oracle)", () => {
  TESTED_EVENTS.add("NftReturned");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 1111n, position: dummy(2), depositor: dummy(3), nftMint: dummy(4) };
    assert.deepEqual(decodeNftReturned(fullEvent("NftReturned", value)), value);
  });
});

describe("Withdrawn (IDL oracle)", () => {
  TESTED_EVENTS.add("Withdrawn");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 1212n, position: dummy(2), depositor: dummy(3), feesPaid: 5_000n };
    assert.deepEqual(decodeWithdrawn(fullEvent("Withdrawn", value)), value);
  });
});

// **Field-identical to `Withdrawn`, and that is the hazard.** Same five fields in the same
// order and the same widths, so the two decode each other's bytes cleanly and only the
// discriminator separates them — `decodeFeeClaimed` is pinned against a `FeeClaimed` event
// here, and the cross-decode is refused below.
describe("FeeClaimed (IDL oracle)", () => {
  TESTED_EVENTS.add("FeeClaimed");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 1414n, position: dummy(2), depositor: dummy(3), feesPaid: 0n };
    assert.deepEqual(decodeFeeClaimed(fullEvent("FeeClaimed", value)), value);
  });

  // `fees_paid` may be zero, exactly as `Withdrawn.fees_paid` may — the IDL doc says so, and a
  // claim that pays nothing is still a claim. Pinned so nobody "fixes" it into a nonzero check.
  it("accepts a zero fees_paid", () => {
    const value = { pool: dummy(4), slot: 1n, position: dummy(5), depositor: dummy(6), feesPaid: 0n };
    assert.equal(decodeFeeClaimed(fullEvent("FeeClaimed", value)).feesPaid, 0n);
  });

  it("refuses Withdrawn's bytes, which differ only in the discriminator", () => {
    const value = { pool: dummy(1), slot: 1414n, position: dummy(2), depositor: dummy(3), feesPaid: 7n };
    assert.throws(() => decodeFeeClaimed(fullEvent("Withdrawn", value)));
    assert.throws(() => decodeWithdrawn(fullEvent("FeeClaimed", value)));
  });
});

describe("NftClaimed (IDL oracle)", () => {
  TESTED_EVENTS.add("NftClaimed");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 1313n, position: dummy(2), depositor: dummy(3), nftMint: dummy(4), feesPaid: 7_250_000n,
    };
    assert.deepEqual(decodeNftClaimed(fullEvent("NftClaimed", value)), value);
  });
});

describe("ValueRecorded (IDL oracle)", () => {
  TESTED_EVENTS.add("ValueRecorded");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 1414n, authority: dummy(2), position: dummy(3),
      old: 10_000_000n, new: 11_000_000n, observedAt: 1_800_000_500n,
    };
    assert.deepEqual(decodeValueRecorded(fullEvent("ValueRecorded", value)), value);
  });
});

describe("BelowFloorClosed (IDL oracle)", () => {
  TESTED_EVENTS.add("BelowFloorClosed");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 1515n, authority: dummy(2), position: dummy(3), depositor: dummy(4) };
    assert.deepEqual(decodeBelowFloorClosed(fullEvent("BelowFloorClosed", value)), value);
  });
});

describe("SweepBegun (IDL oracle)", () => {
  TESTED_EVENTS.add("SweepBegun");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 1717n, authority: dummy(2), sweepEpoch: 4n };
    assert.deepEqual(decodeSweepBegun(fullEvent("SweepBegun", value)), value);
  });
});

describe("SweepEnded (IDL oracle)", () => {
  TESTED_EVENTS.add("SweepEnded");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 1818n, authority: dummy(2), sweepEpoch: 4n,
      positionsUpdated: 12, lastSweepAt: 1_800_000_600n,
    };
    assert.deepEqual(decodeSweepEnded(fullEvent("SweepEnded", value)), value);
  });
});

describe("CollectionAdmitted (IDL oracle)", () => {
  TESTED_EVENTS.add("CollectionAdmitted");
  // Both `reason` arms, because `Option<u16>` is the only variable-width field on this event and
  // a decoder that assumed `Some` would read the authority out of the payload's tail.
  for (const reason of [null, 7] as const) {
    it(`decodes byte-for-byte with reason=${reason}`, () => {
      const value = {
        pool: dummy(1), slot: 900n, collection: dummy(2),
        standards: 0b011, oldStandards: reason === null ? 0 : 0b111,
        reason, authority: dummy(3),
      };
      assert.deepEqual(decodeCollectionAdmitted(fullEvent("CollectionAdmitted", value)), value);
    });
  }
});

describe("CollectionWithdrawn (IDL oracle)", () => {
  TESTED_EVENTS.add("CollectionWithdrawn");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 901n, collection: dummy(2),
      standards: 0b101, reason: 42, authority: dummy(3),
    };
    assert.deepEqual(decodeCollectionWithdrawn(fullEvent("CollectionWithdrawn", value)), value);
  });
});

describe("PositionSeized (IDL oracle)", () => {
  TESTED_EVENTS.add("PositionSeized");
  // Every cause against every source state: the two tags are adjacent single bytes, so a decoder
  // reading them in the wrong order reports a burn from Pending as a transfer from Active.
  for (const kind of ["Burned", "Transferred", "Frozen"] as const) {
    for (const sourceState of ["Pending", "Active", "ClosedBelowFloor"] as const) {
      it(`decodes byte-for-byte for kind=${kind} sourceState=${sourceState}`, () => {
        const value = {
          pool: dummy(1), slot: 902n, position: dummy(2), kind,
          ownerObserved: dummy(3), sourceState, recordedValue: 12_000_000n,
        };
        assert.deepEqual(decodePositionSeized(fullEvent("PositionSeized", value)), value);
      });
    }
  }
});

// ── Roll-pipeline events ────────────────────────────────────────────────────────────────────

describe("BatchCommitted (IDL oracle)", () => {
  TESTED_EVENTS.add("BatchCommitted");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 4001n, roller: dummy(2), batch: dummy(3), n: 9,
      sPrice: 9_900_000n, sTicket: 9_000_000n, sFee: 900_000n,
      sAllocBps: [7_500, 500, 2_000] as [number, number, number],
      sBlankFaces: [1_000_000n, 2_000_000n, 5_000_000n, 100_000_000n, 51n, 52n, 53n, 54n] as BlankFaces,
    };
    assert.deepEqual(decodeBatchCommitted(fullEvent("BatchCommitted", value)), value);
  });
});

describe("BatchResolved (IDL oracle)", () => {
  TESTED_EVENTS.add("BatchResolved");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 4002n, batch: dummy(2) };
    assert.deepEqual(decodeBatchResolved(fullEvent("BatchResolved", value)), value);
  });
});

describe("RollSettled (IDL oracle)", () => {
  TESTED_EVENTS.add("RollSettled");
  // Every `RollOutcome` arm, because the tag sits mid-payload: a decoder that mis-sized it would
  // read `selected_mint` off by a byte on whichever arm it was not written against.
  for (const outcome of ["None", "RealCard", "Blank"] as const) {
    it(`decodes byte-for-byte with outcome=${outcome}`, () => {
      const value = {
        pool: dummy(1), slot: 4003n, batch: dummy(2), rollIndex: 6, outcome,
        selectedMint: dummy(3), blankFaceIdx: 7,
        drawValue: 555_555n, attempts: 3, wAtDraw: 666_666n, nRealAtDraw: 12,
        blanksAtDraw: [21, 22, 23, 24, 25, 26, 27, 28] as BlankCounts,
        // Four distinct values: a swap inside the split is invisible to a same-width check.
        feeEqual: 111_111n, feeTier: 222_222n, feeProtocol: 333_333n, feeT04: 444_444n,
      };
      assert.deepEqual(decodeRollSettled(fullEvent("RollSettled", value)), value);
    });
  }
});

describe("RollRefunded (IDL oracle)", () => {
  TESTED_EVENTS.add("RollRefunded");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 4004n, batch: dummy(2), rollIndex: 4,
      reason: 6_204, wallet: dummy(3),
    };
    assert.deepEqual(decodeRollRefunded(fullEvent("RollRefunded", value)), value);
  });
});

describe("BatchCompleted (IDL oracle)", () => {
  TESTED_EVENTS.add("BatchCompleted");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 4005n, batch: dummy(2), openBatchesRemaining: 0 };
    assert.deepEqual(decodeBatchCompleted(fullEvent("BatchCompleted", value)), value);
  });
});

describe("BatchRecovered (IDL oracle)", () => {
  TESTED_EVENTS.add("BatchRecovered");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 4006n, batch: dummy(2), wallet: dummy(3), openBatchesRemaining: 0,
    };
    assert.deepEqual(decodeBatchRecovered(fullEvent("BatchRecovered", value)), value);
  });
});

describe("BatchClosed (IDL oracle)", () => {
  TESTED_EVENTS.add("BatchClosed");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 4007n, batch: dummy(2), rentRecipient: dummy(3) };
    assert.deepEqual(decodeBatchClosed(fullEvent("BatchClosed", value)), value);
  });
});

describe("AcquiredActivated (IDL oracle)", () => {
  TESTED_EVENTS.add("AcquiredActivated");
  it("decodes byte-for-byte", () => {
    // The two i64s are adjacent and same-width; the age is not derivable from the slot, so a
    // swapped pair fails this equality rather than round-tripping.
    const value = {
      pool: dummy(1), slot: 4008n, authority: dummy(2), position: dummy(3),
      value: 36_000_000n, observedAt: 1_760_000_000n, observedAgeSeconds: 3_600n,
    };
    assert.deepEqual(decodeAcquiredActivated(fullEvent("AcquiredActivated", value)), value);
  });
});

describe("T04CeilingOverflow (IDL oracle)", () => {
  TESTED_EVENTS.add("T04CeilingOverflow");
  it("decodes byte-for-byte", () => {
    const value = { pool: dummy(1), slot: 4009n, amount: 1_250_000n };
    assert.deepEqual(decodeT04CeilingOverflow(fullEvent("T04CeilingOverflow", value)), value);
  });
});

describe("T04Swept (IDL oracle)", () => {
  TESTED_EVENTS.add("T04Swept");
  it("decodes byte-for-byte", () => {
    const value = { slot: 4012n, amount: 1_250_000n, ceiling: 2_000_000_000n };
    assert.deepEqual(decodeT04Swept(fullEvent("T04Swept", value)), value);
  });
});

describe("T04CeilingSet (IDL oracle)", () => {
  TESTED_EVENTS.add("T04CeilingSet");
  it("decodes byte-for-byte", () => {
    const value = { slot: 4013n, authority: dummy(1), old: 1_000_000_000n, new: 3_000_000_000n };
    assert.deepEqual(decodeT04CeilingSet(fullEvent("T04CeilingSet", value)), value);
  });
});

describe("TreasuriesConsolidated (IDL oracle)", () => {
  TESTED_EVENTS.add("TreasuriesConsolidated");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 4014n, authority: dummy(2), oldTreasury03: dummy(3), oldTreasury04: dummy(4),
      treasury03: dummy(5), treasury04: dummy(6), amount03: 7_000_000n, amount04: 9_000_000n,
    };
    assert.deepEqual(decodeTreasuriesConsolidated(fullEvent("TreasuriesConsolidated", value)), value);
  });
});

describe("T03Transferred (IDL oracle)", () => {
  TESTED_EVENTS.add("T03Transferred");
  it("decodes byte-for-byte", () => {
    const value = {
      sourcePool: dummy(1), destinationPool: dummy(2), slot: 4010n, authority: dummy(3), amount: 500_000_000n,
    };
    assert.deepEqual(decodeT03Transferred(fullEvent("T03Transferred", value)), value);
  });
});

describe("T04Transferred (IDL oracle)", () => {
  TESTED_EVENTS.add("T04Transferred");
  it("decodes byte-for-byte", () => {
    const value = {
      sourcePool: dummy(1), destinationPool: dummy(2), slot: 4010n, authority: dummy(3), amount: 500_000_000n,
    };
    assert.deepEqual(decodeT04Transferred(fullEvent("T04Transferred", value)), value);
  });
});

describe("Treasury03Migrated (IDL oracle)", () => {
  TESTED_EVENTS.add("Treasury03Migrated");
  it("decodes byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 4011n, oldTreasury03: dummy(2), newTreasury03: dummy(3),
    };
    assert.deepEqual(decodeTreasury03Migrated(fullEvent("Treasury03Migrated", value)), value);
  });
});

describe("InstantSold (IDL oracle)", () => {
  TESTED_EVENTS.add("InstantSold");
  // Every `standard` the program admits, since this is one of only two events carrying the
  // discriminant and it is the tail byte — a decoder that stopped one field short would drop it
  // silently, which `assertConsumed` is what catches.
  for (const standard of [0, 1, 2] as const) {
    it(`decodes byte-for-byte with standard=${standard}`, () => {
      const value = {
        pool: dummy(1), slot: 4010n, position: dummy(2), seller: dummy(3),
        value: 36_000_000n, observedAt: 1_760_000_000n, observedAgeSeconds: 7_200n,
        rateBps: 8_500, paid: 30_600_000n, standard,
        payer: "Treasury03", escrowOwner: dummy(0), escrowPaid: 0n, bidBps: 0,
      };
      assert.deepEqual(decodeInstantSold(fullEvent("InstantSold", value)), value);
    });
  }
  it("decodes an escrow sale byte-for-byte", () => {
    const value = {
      pool: dummy(1), slot: 4010n, position: dummy(2), seller: dummy(3),
      value: 100_000_000n, observedAt: 1_760_000_000n, observedAgeSeconds: 7_200n,
      rateBps: 8_500, paid: 85_000_000n, standard: 2,
      payer: "Escrow", escrowOwner: dummy(4), escrowPaid: 92_500_000n, bidBps: 9_250,
    };
    assert.deepEqual(decodeInstantSold(fullEvent("InstantSold", value)), value);
  });
});

describe("EscrowOpened (IDL oracle)", () => {
  TESTED_EVENTS.add("EscrowOpened");
  it("decodes byte-for-byte", () => {
    const value = { escrow: dummy(1), slot: 4010n, owner: dummy(2) };
    assert.deepEqual(decodeEscrowOpened(fullEvent("EscrowOpened", value)), value);
  });
});

describe("EscrowDeposited (IDL oracle)", () => {
  TESTED_EVENTS.add("EscrowDeposited");
  it("decodes a wallet deposit byte-for-byte", () => {
    const value = {
      escrow: dummy(1), slot: 4010n, owner: dummy(2), amount: 10_000_000n, source: "Wallet",
      position: dummy(0), positionPool: dummy(0), balanceAfter: 10_000_000n,
    };
    assert.deepEqual(decodeEscrowDeposited(fullEvent("EscrowDeposited", value)), value);
  });
  it("decodes a fee claim byte-for-byte", () => {
    const value = {
      escrow: dummy(1), slot: 4010n, owner: dummy(2), amount: 3_000_000n, source: "Fees",
      position: dummy(3), positionPool: dummy(4), balanceAfter: 13_000_000n,
    };
    assert.deepEqual(decodeEscrowDeposited(fullEvent("EscrowDeposited", value)), value);
  });
});

describe("EscrowWithdrawn (IDL oracle)", () => {
  TESTED_EVENTS.add("EscrowWithdrawn");
  it("decodes byte-for-byte", () => {
    const value = {
      escrow: dummy(1), slot: 4010n, owner: dummy(2), amount: 7_000_000n, balanceAfter: 6_000_000n,
    };
    assert.deepEqual(decodeEscrowWithdrawn(fullEvent("EscrowWithdrawn", value)), value);
  });
});

describe("EscrowArmed (IDL oracle)", () => {
  TESTED_EVENTS.add("EscrowArmed");
  for (const armed of [true, false]) {
    it(`decodes byte-for-byte with armed=${armed}`, () => {
      const value = { escrow: dummy(1), slot: 4010n, owner: dummy(2), armed };
      assert.deepEqual(decodeEscrowArmed(fullEvent("EscrowArmed", value)), value);
    });
  }
});

// ── Structural coverage ─────────────────────────────────────────────────────────────────────
describe("coverage", () => {
  it("exercises exactly the IDL's 9 accounts and 45 events, no more, no fewer", () => {
    assert.equal(idl.accounts.length, 9);
    assert.equal(idl.events.length, 45);
    assert.equal(TESTED_ACCOUNTS.size, 9);
    assert.equal(TESTED_EVENTS.size, 45);
    for (const a of idl.accounts) {
      assert.ok(TESTED_ACCOUNTS.has(a.name), `account '${a.name}' has no oracle coverage`);
    }
    for (const e of idl.events) {
      assert.ok(TESTED_EVENTS.has(e.name), `event '${e.name}' has no oracle coverage`);
    }
  });
});

// ── Self-test: the comparator must red on a deliberately wrong decode ───────────────────────
describe("self-test: the state differential actually discriminates", () => {
  it("catches a swapped same-width field (pool ⇄ owner, both pubkeys) in WalletStats", () => {
    const value = { pool: dummy(1), owner: dummy(2), activePositions: 3, activeValue: 36_000_000n, bump: 251 };
    const decoded = decodeWalletStats(fullAccount("WalletStats", value));
    const swapped = { ...value, pool: value.owner, owner: value.pool };
    assert.notDeepEqual(decoded, swapped);
  });
});
