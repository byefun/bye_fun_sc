// Codec tests for scripts/lib/admin.ts — the five administration instruction encoders.
// Mirrors tests/unit/codec.test.ts's pattern: discriminator derivation, fixed golden bytes for
// the arg encoding (independent of the encoder's own Borsh calls, so a field-order bug in
// production has to also survive an independently-written expected buffer here), and the
// account list's exact length and order.

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
import {
  initProtocol,
  initPool,
  updateConfig,
  setPause,
  setAuthorities,
  type ProtocolConfigArgs,
  type PoolConfigArgs,
  type ConfigUpdate,
  type AuthorityRole,
} from "../../scripts/lib/admin.js";
import { SYSTEM_PROGRAM, TOKEN_PROGRAM } from "../../scripts/constants.js";

// ── Fixtures ──────────────────────────────────────────────────────────────────────────────────
const addrEnc = getAddressEncoder();
const addrDec = getAddressDecoder();

/** A distinct, valid `Address` per `n` — arbitrary 32-byte pubkeys, not real keys. */
function dummy(n: number): Address {
  return addrDec.decode(Buffer.alloc(32, n));
}

// Seeds, sourced independently from programs/bye_machine/src/common/seeds.rs — not copied from
// admin.ts — so a seed typo or a swapped seed constant in production shows up as a derived-
// address mismatch below rather than passing by construction.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const PRINCIPAL_VAULT_SEED = "principal";
const TREASURY_04_SEED = "t04";
const TOP_TIER_SEED = "tier";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

// Buffer-level primitives, written independently of scripts/lib/anchor.ts's Borsh class, so the
// "golden bytes" below are a second path over the same spec rather than a round-trip.
const u8Buf = (n: number) => {
  const b = Buffer.alloc(1);
  b.writeUInt8(n);
  return b;
};
const u16Buf = (n: number) => {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return b;
};
const u64Buf = (n: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(n);
  return b;
};
const boolBuf = (v: boolean) => u8Buf(v ? 1 : 0);
const pubkeyBuf = (a: Address) => Buffer.from(addrEnc.encode(a));
const optionPubkeyBuf = (a: Address | null) =>
  a === null ? u8Buf(0) : Buffer.concat([u8Buf(1), pubkeyBuf(a)]);

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

// ── init_protocol ─────────────────────────────────────────────────────────────────────────────
describe("initProtocol", () => {
  const protocolArgs: ProtocolConfigArgs = {
    operator: dummy(1),
    t03Authority: dummy(2),
    t02Authority: dummy(3),
    protocolRevenue: dummy(4),
    usdcMint: dummy(5),
    byeMint: dummy(6),
    vrfProgram: dummy(7),
    oracleQueue: dummy(8),
    swapPool: dummy(9),
    maxSwapSlippageBps: 0x0102,
  };
  const payer = dummy(10);

  it("has the init_protocol discriminator", async () => {
    const ix = await initProtocol({ payer, args: protocolArgs });
    assertDiscriminator(ix, "init_protocol");
  });

  it("encodes ProtocolConfigArgs as nine pubkeys then a u16, in IDL declaration order", async () => {
    const ix = await initProtocol({ payer, args: protocolArgs });
    const expected = Buffer.concat([
      pubkeyBuf(protocolArgs.operator),
      pubkeyBuf(protocolArgs.t03Authority),
      pubkeyBuf(protocolArgs.t02Authority),
      pubkeyBuf(protocolArgs.protocolRevenue),
      pubkeyBuf(protocolArgs.usdcMint),
      pubkeyBuf(protocolArgs.byeMint),
      pubkeyBuf(protocolArgs.vrfProgram),
      pubkeyBuf(protocolArgs.oracleQueue),
      pubkeyBuf(protocolArgs.swapPool),
      u16Buf(protocolArgs.maxSwapSlippageBps),
    ]);
    assert.deepEqual(argsOf(ix), expected);
    assert.equal(argsOf(ix).length, 9 * 32 + 2);
  });

  it("has 3 accounts in order: payer, protocol_config, system_program", async () => {
    const ix = await initProtocol({ payer, args: protocolArgs });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);

    assert.equal(accountsOf(ix).length, 3);
    assert.deepEqual(
      accountsOf(ix).map((a) => a.address),
      [payer, protocolConfig, SYSTEM_PROGRAM],
    );
    assert.deepEqual(
      accountsOf(ix).map((a) => a.role),
      [AccountRole.WRITABLE_SIGNER, AccountRole.WRITABLE, AccountRole.READONLY],
    );
  });
});

// ── init_pool ─────────────────────────────────────────────────────────────────────────────────
describe("initPool", () => {
  const poolArgs: PoolConfigArgs = {
    treasury03: dummy(11),
    price: 11_000_000n,
    ticketTarget: 10_000_000n,
    fee: 1_000_000n,
    allocEqualBps: 6_000,
    allocTierBps: 1_000,
    allocProtocolBps: 3_000,
    admissionFloor: 20_000_000n,
    admissionCeiling: 50_000_000n,
    walletValueCap: 500_000_000n,
    maxN: 10,
    buybackRateBps: 9_000,
    blankFaces: [1_000_000n, 3_000_000n, 8_999_999n],
    t04Ceiling: 3_000_000_000n,
    sweepCadenceHours: 12,
    tierSize: 20,
  };
  const administrator = dummy(13);
  const weightIndex = dummy(14);
  const usdcMint = dummy(15);
  const poolId = 7;

  it("has the init_pool discriminator", async () => {
    const ix = await initPool({ administrator, poolId, weightIndex, usdcMint, args: poolArgs });
    assertDiscriminator(ix, "init_pool");
  });

  it("encodes PoolConfigArgs in IDL declaration order, blank_faces as a bare [u64; 3]", async () => {
    const ix = await initPool({ administrator, poolId, weightIndex, usdcMint, args: poolArgs });
    const expected = Buffer.concat([
      pubkeyBuf(poolArgs.treasury03),
      u64Buf(poolArgs.price),
      u64Buf(poolArgs.ticketTarget),
      u64Buf(poolArgs.fee),
      u16Buf(poolArgs.allocEqualBps),
      u16Buf(poolArgs.allocTierBps),
      u16Buf(poolArgs.allocProtocolBps),
      u64Buf(poolArgs.admissionFloor),
      u64Buf(poolArgs.admissionCeiling),
      u64Buf(poolArgs.walletValueCap),
      u8Buf(poolArgs.maxN),
      u16Buf(poolArgs.buybackRateBps),
      u64Buf(poolArgs.blankFaces[0]),
      u64Buf(poolArgs.blankFaces[1]),
      u64Buf(poolArgs.blankFaces[2]),
      u64Buf(poolArgs.t04Ceiling),
      u16Buf(poolArgs.sweepCadenceHours),
      u8Buf(poolArgs.tierSize),
    ]);
    assert.deepEqual(argsOf(ix), expected);
  });

  it("has 10 accounts in IDL order, none of them the same address", async () => {
    const ix = await initPool({ administrator, poolId, weightIndex, usdcMint, args: poolArgs });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: principalVault } = await derivePda([
      PRINCIPAL_VAULT_SEED,
      seedFromPubkey(pool),
    ]);
    const { address: treasury04 } = await derivePda([TREASURY_04_SEED, seedFromPubkey(pool)]);

    const expectedAddresses = [
      administrator,
      protocolConfig,
      pool,
      weightIndex,
      topTier,
      principalVault,
      treasury04,
      usdcMint,
      TOKEN_PROGRAM,
      SYSTEM_PROGRAM,
    ];

    assert.equal(accountsOf(ix).length, 10);
    // Order-sensitive: principal_vault and treasury_04 are both writable PDAs of the pool,
    // derived from distinct seed prefixes — swapping their positions (or their seeds) changes
    // this array, which a length or Set-membership check would not catch.
    assert.deepEqual(
      accountsOf(ix).map((a) => a.address),
      expectedAddresses,
    );
    assert.equal(new Set(expectedAddresses).size, expectedAddresses.length);
    assert.deepEqual(
      accountsOf(ix).map((a) => a.role),
      [
        AccountRole.WRITABLE_SIGNER, // administrator
        AccountRole.WRITABLE, // protocol_config
        AccountRole.WRITABLE, // pool
        AccountRole.WRITABLE, // weight_index
        AccountRole.WRITABLE, // top_tier
        AccountRole.WRITABLE, // principal_vault
        AccountRole.WRITABLE, // treasury_04
        AccountRole.READONLY, // usdc_mint
        AccountRole.READONLY, // token_program
        AccountRole.READONLY, // system_program
      ],
    );
  });
});

// ── update_config ─────────────────────────────────────────────────────────────────────────────
describe("updateConfig", () => {
  const administrator = dummy(20);
  const poolId = 3;

  async function build(update: ConfigUpdate, demoted: Address[] = []) {
    return updateConfig({ administrator, poolId, update, demoted });
  }

  it("has the update_config discriminator", async () => {
    const ix = await build({ kind: "MaxN", value: 5 });
    assertDiscriminator(ix, "update_config");
  });

  it("has 4 accounts in order: administrator, protocol_config, pool, top_tier", async () => {
    const ix = await build({ kind: "MaxN", value: 5 });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);

    assert.equal(accountsOf(ix).length, 4);
    // pool and top_tier are both writable PDAs derived from distinct seeds — an order swap or a
    // seed swap between them changes this array.
    assert.deepEqual(
      accountsOf(ix).map((a) => a.address),
      [administrator, protocolConfig, pool, topTier],
    );
    assert.deepEqual(
      accountsOf(ix).map((a) => a.role),
      [AccountRole.READONLY_SIGNER, AccountRole.READONLY, AccountRole.WRITABLE, AccountRole.WRITABLE],
    );
  });

  it("appends demoted positions as writable remaining_accounts, by key, preserving caller order", async () => {
    const demoted = [dummy(30), dummy(31), dummy(32)];
    const ix = await build({ kind: "TierSize", value: 5 }, demoted);
    assert.equal(accountsOf(ix).length, 4 + demoted.length);
    assert.deepEqual(
      accountsOf(ix).slice(4).map((a) => a.address),
      demoted,
    );
    for (const a of accountsOf(ix).slice(4)) assert.equal(a.role, AccountRole.WRITABLE);
  });

  it("accepts exactly 19 demoted positions and throws above it", async () => {
    const at19 = Array.from({ length: 19 }, (_, i) => dummy(40 + i));
    await assert.doesNotReject(build({ kind: "TierSize", value: 1 }, at19));

    const at20 = Array.from({ length: 20 }, (_, i) => dummy(60 + i));
    await assert.rejects(build({ kind: "TierSize", value: 1 }, at20), /at most 19/);
  });

  // Each ConfigUpdate variant's declaration-order index (update_config.rs) and payload, checked
  // independently of scripts/lib/anchor.ts's enumVariant call sites.
  const cases: Array<{ update: ConfigUpdate; index: number; payload: Buffer }> = [
    { update: { kind: "AdmissionFloor", value: 111n }, index: 0, payload: u64Buf(111n) },
    { update: { kind: "AdmissionCeiling", value: 222n }, index: 1, payload: u64Buf(222n) },
    { update: { kind: "WalletValueCap", value: 333n }, index: 2, payload: u64Buf(333n) },
    { update: { kind: "MaxN", value: 9 }, index: 3, payload: u8Buf(9) },
    { update: { kind: "BuybackRateBps", value: 444 }, index: 4, payload: u16Buf(444) },
    { update: { kind: "T04Ceiling", value: 555n }, index: 5, payload: u64Buf(555n) },
    { update: { kind: "SweepCadenceHours", value: 6 }, index: 6, payload: u16Buf(6) },
    { update: { kind: "TierSize", value: 15 }, index: 7, payload: u8Buf(15) },
    {
      update: { kind: "Pricing", price: 1n, ticketTarget: 2n, fee: 3n },
      index: 8,
      payload: Buffer.concat([u64Buf(1n), u64Buf(2n), u64Buf(3n)]),
    },
    {
      update: { kind: "Allocation", equalBps: 4, tierBps: 5, protocolBps: 6 },
      index: 9,
      payload: Buffer.concat([u16Buf(4), u16Buf(5), u16Buf(6)]),
    },
    {
      update: { kind: "BlankFaces", value: [7n, 8n, 9n] },
      index: 10,
      payload: Buffer.concat([u64Buf(7n), u64Buf(8n), u64Buf(9n)]),
    },
  ];

  for (const { update, index, payload } of cases) {
    it(`encodes ConfigUpdate::${update.kind} at variant index ${index}`, async () => {
      const ix = await build(update);
      const args = argsOf(ix);
      assert.equal(args[0], index);
      assert.deepEqual(args.subarray(1), payload);
    });
  }
});

// ── set_pause ─────────────────────────────────────────────────────────────────────────────────
describe("setPause", () => {
  const administrator = dummy(70);
  const poolId = 4;

  it("has the set_pause discriminator", async () => {
    const ix = await setPause({ administrator, poolId, deposits: true, rolls: false, reason: 9 });
    assertDiscriminator(ix, "set_pause");
  });

  it("encodes (deposits: bool, rolls: bool, reason: u16) in that order", async () => {
    const ix = await setPause({
      administrator,
      poolId,
      deposits: true,
      rolls: false,
      reason: 0x0201,
    });
    assert.deepEqual(
      argsOf(ix),
      Buffer.concat([boolBuf(true), boolBuf(false), u16Buf(0x0201)]),
    );
  });

  it("has 3 accounts in order: administrator, protocol_config, pool", async () => {
    const ix = await setPause({ administrator, poolId, deposits: false, rolls: false, reason: 0 });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);

    assert.equal(accountsOf(ix).length, 3);
    assert.deepEqual(
      accountsOf(ix).map((a) => a.address),
      [administrator, protocolConfig, pool],
    );
    assert.deepEqual(
      accountsOf(ix).map((a) => a.role),
      [AccountRole.READONLY_SIGNER, AccountRole.READONLY, AccountRole.WRITABLE],
    );
  });
});

// ── set_authorities ───────────────────────────────────────────────────────────────────────────
describe("setAuthorities", () => {
  const signerAddress = dummy(80);
  const newAuthority = dummy(81);

  it("has the set_authorities discriminator", async () => {
    const ix = await setAuthorities({ signer: signerAddress, role: "Operator", newAuthority });
    assertDiscriminator(ix, "set_authorities");
  });

  it("has 2 accounts in order: signer, protocol_config", async () => {
    const ix = await setAuthorities({ signer: signerAddress, role: "Operator", newAuthority });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);

    assert.equal(accountsOf(ix).length, 2);
    assert.deepEqual(
      accountsOf(ix).map((a) => a.address),
      [signerAddress, protocolConfig],
    );
    assert.deepEqual(
      accountsOf(ix).map((a) => a.role),
      [AccountRole.READONLY_SIGNER, AccountRole.WRITABLE],
    );
  });

  const roles: Array<{ role: AuthorityRole; index: number }> = [
    { role: "Administrator", index: 0 },
    { role: "Operator", index: 1 },
    { role: "T03", index: 2 },
    { role: "T02", index: 3 },
  ];

  for (const { role, index } of roles) {
    it(`encodes AuthorityRole::${role} at variant index ${index}, propose phase (Some)`, async () => {
      const ix = await setAuthorities({ signer: signerAddress, role, newAuthority });
      assert.deepEqual(argsOf(ix), Buffer.concat([u8Buf(index), optionPubkeyBuf(newAuthority)]));
    });

    it(`encodes AuthorityRole::${role} at variant index ${index}, accept phase (None)`, async () => {
      const ix = await setAuthorities({ signer: signerAddress, role, newAuthority: null });
      assert.deepEqual(argsOf(ix), Buffer.concat([u8Buf(index), optionPubkeyBuf(null)]));
    });
  }
});
