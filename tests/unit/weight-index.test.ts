// Codec tests for scripts/lib/weight-index.ts — the WeightIndex account creation helper.
// Golden bytes are built with raw Buffer writes, independent of the Borsh writer weight-index.ts
// itself uses — oracle separation, so a field-order bug in production has to also survive an
// independently-written expected buffer here (mirrors tests/unit/admin.test.ts's pattern).

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { AccountRole, address, getAddressEncoder } from "@solana/kit";
import type { Address, Instruction } from "@solana/kit";
import {
  WEIGHT_INDEX_ACCOUNT_LEN,
  buildCreateWeightIndexAccountIx,
  fetchWeightIndexRentLamports,
  createWeightIndexAccount,
} from "../../scripts/lib/weight-index.js";
import { PROGRAM_ID, SYSTEM_PROGRAM } from "../../scripts/constants.js";
import type { Connection } from "../../scripts/_common.js";

// ── Account size — derived from the Rust field breakdown, not a bare literal ────────────────────
//
// state/weight_index.rs:12-20, its own `weight_index_field_order` test pins this byte-for-byte:
//   pool(32) total_weight(8) high_water(4) free_count(4) _padding(8)
//   tree([u64; FENWICK_CAPACITY]) free_stack([u32; FENWICK_CAPACITY])
// FENWICK_CAPACITY = 16_384 (common/constants.rs). Recomputed here independently of
// weight-index.ts's own constant, so a fault in that file's arithmetic can't hide behind it.
describe("WEIGHT_INDEX_ACCOUNT_LEN", () => {
  const FENWICK_CAPACITY = 16_384;
  const HEADER = 32 + 8 + 4 + 4 + 8;
  const TREE = 8 * FENWICK_CAPACITY;
  const FREE_STACK = 4 * FENWICK_CAPACITY;
  const DISCRIMINATOR = 8;

  it("equals 8 + WeightIndex::INIT_SPACE, derived from the field layout", () => {
    assert.equal(WEIGHT_INDEX_ACCOUNT_LEN, DISCRIMINATOR + HEADER + TREE + FREE_STACK);
  });

  it("matches init_pool.rs's own design-figure pin (196,672 bytes)", () => {
    // init_pool.rs's `weight_index_account_size_is_the_design_figure` test asserts this same
    // number against `WeightIndex::INIT_SPACE` on the Rust side.
    assert.equal(WEIGHT_INDEX_ACCOUNT_LEN, 196_672);
  });
});

// ── buildCreateWeightIndexAccountIx — golden bytes ───────────────────────────────────────────────

const addrEnc = getAddressEncoder();
const pubkeyBuf = (a: Address) => Buffer.from(addrEnc.encode(a));
const u32Buf = (n: number) => {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(n);
  return b;
};
const u64Buf = (n: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(n);
  return b;
};

function dataOf(ix: Instruction): Buffer {
  assert.ok(ix.data, "instruction has no data");
  return Buffer.from(ix.data);
}

function accountsOf(ix: Instruction) {
  assert.ok(ix.accounts, "instruction has no accounts");
  return ix.accounts;
}

describe("buildCreateWeightIndexAccountIx", () => {
  const payer = address("11111111111111111111111111111112");
  const weightIndex = address("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
  const lamports = 1_369_728_000_000_000n;

  it("targets the System program", () => {
    const ix = buildCreateWeightIndexAccountIx({ payer, weightIndex, lamports });
    assert.equal(ix.programAddress, SYSTEM_PROGRAM);
  });

  it("has 2 accounts, both writable signers, in order: payer, weightIndex", () => {
    const ix = buildCreateWeightIndexAccountIx({ payer, weightIndex, lamports });
    assert.equal(accountsOf(ix).length, 2);
    assert.deepEqual(
      accountsOf(ix).map((a) => a.address),
      [payer, weightIndex],
    );
    assert.deepEqual(
      accountsOf(ix).map((a) => a.role),
      [AccountRole.WRITABLE_SIGNER, AccountRole.WRITABLE_SIGNER],
    );
  });

  it("encodes ix 0 ‖ lamports u64 ‖ space u64 ‖ owner 32B, little-endian, built independently", () => {
    const ix = buildCreateWeightIndexAccountIx({ payer, weightIndex, lamports });
    const expected = Buffer.concat([
      u32Buf(0),
      u64Buf(lamports),
      u64Buf(BigInt(WEIGHT_INDEX_ACCOUNT_LEN)),
      pubkeyBuf(PROGRAM_ID),
    ]);
    assert.deepEqual(dataOf(ix), expected);
    assert.equal(dataOf(ix).length, 4 + 8 + 8 + 32);
  });

  it("owner is PROGRAM_ID, not the System program — a substitution this test would catch", () => {
    const ix = buildCreateWeightIndexAccountIx({ payer, weightIndex, lamports });
    const ownerBytes = dataOf(ix).subarray(20, 52);
    assert.deepEqual(ownerBytes, pubkeyBuf(PROGRAM_ID));
    assert.notDeepEqual(ownerBytes, pubkeyBuf(SYSTEM_PROGRAM));
  });

  it("space is WEIGHT_INDEX_ACCOUNT_LEN regardless of the account's future contents", () => {
    const ix = buildCreateWeightIndexAccountIx({ payer, weightIndex, lamports });
    const space = dataOf(ix).subarray(12, 20).readBigUInt64LE();
    assert.equal(space, BigInt(WEIGHT_INDEX_ACCOUNT_LEN));
  });

  it("threads the caller's lamports value through unchanged", () => {
    const other = 1_246_334_400_000n;
    const ix = buildCreateWeightIndexAccountIx({ payer, weightIndex, lamports: other });
    const encodedLamports = dataOf(ix).subarray(4, 12).readBigUInt64LE();
    assert.equal(encodedLamports, other);
  });
});

// ── fetchWeightIndexRentLamports / createWeightIndexAccount — fake RPC, no validator ───────────
//
// A discriminating positive control: the fake returns a distinctive, non-round value, so if
// production ever substituted a hardcoded literal for the RPC result, this would fail rather
// than pass by coincidence.
const FAKE_RENT_LAMPORTS = 1_234_567_890n;

function fakeRentConnection(onRequestedSize: (size: bigint) => void): Connection {
  return {
    rpc: {
      getMinimumBalanceForRentExemption: (size: bigint) => ({
        send: async () => {
          onRequestedSize(size);
          return FAKE_RENT_LAMPORTS;
        },
      }),
    },
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  } as any as Connection;
}

describe("fetchWeightIndexRentLamports", () => {
  it("requests rent for exactly WEIGHT_INDEX_ACCOUNT_LEN, not some other size", async () => {
    let requested: bigint | null = null;
    const rpc = fakeRentConnection((size) => {
      requested = size;
    });
    const lamports = await fetchWeightIndexRentLamports(rpc);
    assert.equal(requested, BigInt(WEIGHT_INDEX_ACCOUNT_LEN));
    assert.equal(lamports, FAKE_RENT_LAMPORTS);
  });
});

describe("createWeightIndexAccount", () => {
  const payer: Address = address("11111111111111111111111111111112");

  it("builds an instruction whose lamports came from the RPC fetch, not a literal", async () => {
    const rpc = fakeRentConnection(() => {});
    const { signer, instruction } = await createWeightIndexAccount({ rpc, payer });

    const encodedLamports = dataOf(instruction).subarray(4, 12).readBigUInt64LE();
    assert.equal(encodedLamports, FAKE_RENT_LAMPORTS);
    assert.deepEqual(
      accountsOf(instruction).map((a) => a.address),
      [payer, signer.address],
    );
  });

  it("generates a fresh keypair each call — the two signers are never the same address", async () => {
    const rpc = fakeRentConnection(() => {});
    const first = await createWeightIndexAccount({ rpc, payer });
    const second = await createWeightIndexAccount({ rpc, payer });
    assert.notEqual(first.signer.address, second.signer.address);
  });
});
