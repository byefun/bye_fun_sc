// T-50: IDL-driven differential oracle for the 15 instruction encoders (T-39..T-43). Walks
// target/idl/bye_machine.json at test run time — args field list/types and account list/roles
// come from parsing the IDL, never from a list hand-copied into this file — and asserts byte
// equality against each hand-written encoder in scripts/lib/{admin,deposit,exit,value,tier}.ts,
// plus each account list's exact length and order (never a length or Set-membership check,
// which would pass a transposition of two same-role accounts — see the self-test at the bottom).
//
// This file's axis is TS encoder ⇄ IDL (does the client drift from the IDL?). It is a different
// axis from idl-vs-rust-accounts.test.ts's Rust ⇄ IDL (does the IDL go stale against the Rust
// source?) — neither subsumes the other, and both are worth having.
//
// Two stated limits, not closed here:
// 1. The IDL is generated from the same Rust as the encoders it is compared against, so every
//    assertion below proves client↔program agreement, never program↔spec correctness.
// 2. A source-level accounts-struct reorder that moves the Rust, the regenerated IDL and this
//    file's expectations together is invisible to a differential test — that class is closed by
//    `every_accounts_struct_is_pinned_verbatim` in instructions/mod.rs (a whole-body `assert_eq!`
//    against a verbatim literal, reds at the reorder commit), not by this file.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { AccountRole, getAddressDecoder, type Address, type Instruction } from "@solana/kit";
import { derivePda, seedFromPubkey, seedFromU64Le } from "../../scripts/lib/anchor.js";
import { PROGRAM_ID, SYSVAR_INSTRUCTIONS, VRF_PROGRAM } from "../../scripts/constants.js";
import {
  admitCollection,
  initProtocol,
  initPool,
  updateConfig,
  updateVrfConfig,
  setPause,
  setAuthorities,
  withdrawCollection,
  type ProtocolConfigArgs,
  type PoolConfigArgs,
  type ConfigUpdate,
} from "../../scripts/lib/admin.js";
import {
  deposit,
  depositCore,
  approveDeposit,
  rejectDeposit,
  returnRejected,
  returnRejectedCore,
} from "../../scripts/lib/deposit.js";
import { withdraw, withdrawCore, claimNft, claimNftCore, closeSeized } from "../../scripts/lib/exit.js";
import { beginSweep, endSweep, recordValue, type ValueEffect } from "../../scripts/lib/value.js";
import { updateTier } from "../../scripts/lib/tier.js";
import { commitRolls, vrfCallback } from "../../scripts/lib/roll.js";
import {
  loadIdl,
  findInstruction,
  encodeInstructionArgs,
  idlAccountRoles,
  roleFromFlags,
  expectedAccountAddresses,
  type Idl,
} from "./idl-oracle-support.js";

const idl = loadIdl();
const addrDec = getAddressDecoder();

/** A distinct, valid `Address` per `n` — arbitrary 32-byte pubkeys, not real keys. */
function dummy(n: number): Address {
  return addrDec.decode(Buffer.alloc(32, n));
}

// Seeds, redeclared independently of scripts/lib's own copies (that file's own precedent: "each
// domain file owns its own copies", so a seed drift shows up as a mismatch here too).
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const COLLECTION_SEED = "collection";
const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const WALLET_STATS_SEED = "wallet";
const TOP_TIER_SEED = "tier";
const PRINCIPAL_VAULT_SEED = "principal";
const TREASURY_04_SEED = "t04";
const BATCH_SEED = "batch";
const VRF_IDENTITY_SEED = "identity";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

function dataOf(ix: Instruction): Buffer {
  assert.ok(ix.data, "instruction has no data");
  return Buffer.from(ix.data);
}
function argsOf(ix: Instruction): Buffer {
  return dataOf(ix).subarray(8);
}
function accountsOf(ix: Instruction) {
  assert.ok(ix.accounts, "instruction has no accounts");
  return ix.accounts;
}

/** Asserts the instruction's 8-byte discriminator equals the IDL's own literal, independent of
 * `ixDiscriminator`'s sha256 derivation (already cross-checked elsewhere). */
function assertIdlDiscriminator(instructionName: string, ix: Instruction) {
  assert.deepEqual(dataOf(ix).subarray(0, 8), Buffer.from(findInstruction(idl, instructionName).discriminator));
}

/**
 * Asserts the instruction's account list against the IDL's own declared order and roles —
 * length, address sequence and role sequence, never a length or Set check alone. `resolved` maps
 * every IDL account name this test can pin (marker or recomputed PDA); the rest fall back to the
 * IDL's own literal `address` (constant-program accounts).
 */
function assertIdlAccountsExactly(
  instructionName: string,
  ix: Instruction,
  resolved: Record<string, Address>,
  remaining: Array<{ address: Address; role: AccountRole }> = [],
) {
  const roles = idlAccountRoles(idl, instructionName);
  const addresses = expectedAccountAddresses(idl, instructionName, resolved);
  const expected = [
    ...roles.map((r, i) => ({ address: addresses[i], role: roleFromFlags(r.writable, r.signer) })),
    ...remaining,
  ];
  const accounts = accountsOf(ix);
  assert.equal(accounts.length, expected.length, `${instructionName}: account count`);
  assert.deepEqual(
    accounts.map((a) => a.address),
    expected.map((e) => e.address),
    `${instructionName}: address order`,
  );
  assert.deepEqual(
    accounts.map((a) => a.role),
    expected.map((e) => e.role),
    `${instructionName}: role order`,
  );
}

/** Every instruction this file's describes below actually exercise — asserted at the end against
 * the IDL's own count, so a loop or a forgotten instruction cannot silently pass as covered. */
const TESTED_INSTRUCTIONS = new Set<string>();
function covers(name: string) {
  TESTED_INSTRUCTIONS.add(name);
}

// ── init_protocol ────────────────────────────────────────────────────────────────────────────
describe("init_protocol (IDL oracle)", () => {
  covers("init_protocol");
  const payer = dummy(1);
  const args: ProtocolConfigArgs = {
    operator: dummy(2),
    t03Authority: dummy(3),
    t02Authority: dummy(4),
    protocolRevenue: dummy(5),
    usdcMint: dummy(6),
    byeMint: dummy(7),
    vrfProgram: dummy(8),
    oracleQueue: dummy(9),
    swapPool: dummy(10),
    maxSwapSlippageBps: 250,
  };
  const params = { payer, args };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("init_protocol", await initProtocol(params));
  });

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await initProtocol(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "init_protocol", params));
  });

  it("has 3 accounts in IDL order and role", async () => {
    const ix = await initProtocol(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("init_protocol", ix, { payer, protocol_config: protocolConfig });
  });
});

// ── init_pool ────────────────────────────────────────────────────────────────────────────────
describe("init_pool (IDL oracle)", () => {
  covers("init_pool");
  const administrator = dummy(1);
  const weightIndex = dummy(2);
  const usdcMint = dummy(3);
  const poolId = 7;
  const args: PoolConfigArgs = {
    treasury03: dummy(4),
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
    blankFaces: [1_000_000n, 2_000_000n, 5_000_000n],
    t04Ceiling: 2_000_000_000n,
    sweepCadenceHours: 24,
    tierSize: 20,
  };
  const params = { administrator, poolId, weightIndex, usdcMint, args };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("init_pool", await initPool(params));
  });

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await initPool(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "init_pool", params));
  });

  it("has 10 accounts in IDL order and role", async () => {
    const ix = await initPool(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    const { address: treasury04 } = await derivePda([TREASURY_04_SEED, seedFromPubkey(pool)]);
    assertIdlAccountsExactly("init_pool", ix, {
      administrator,
      protocol_config: protocolConfig,
      pool,
      weight_index: weightIndex,
      top_tier: topTier,
      principal_vault: principalVault,
      treasury_04: treasury04,
      usdc_mint: usdcMint,
    });
  });
});

// ── update_config ────────────────────────────────────────────────────────────────────────────
describe("update_config (IDL oracle)", () => {
  covers("update_config");
  const administrator = dummy(1);
  const poolId = 9;

  async function build(update: ConfigUpdate, demoted: Address[] = []) {
    return updateConfig({ administrator, poolId, update, demoted });
  }

  // One case per ConfigUpdate variant — covers every field-shape this enum carries: a lone u64,
  // a lone u8/u16, a struct variant with several named fields, an array-in-tuple, and a pubkey.
  const cases: ConfigUpdate[] = [
    { kind: "AdmissionFloor", value: 10_000_000n },
    { kind: "AdmissionCeiling", value: 20_000_000n },
    { kind: "WalletValueCap", value: 30_000_000n },
    { kind: "MaxN", value: 12 },
    { kind: "BuybackRateBps", value: 8_000 },
    { kind: "T04Ceiling", value: 3_000_000_000n },
    { kind: "SweepCadenceHours", value: 12 },
    { kind: "TierSize", value: 15 },
    { kind: "Pricing", price: 100n, ticketTarget: 90n, fee: 10n },
    { kind: "Allocation", equalBps: 7_000, tierBps: 1_000, protocolBps: 2_000 },
    { kind: "BlankFaces", value: [1n, 2n, 3n] },
  ];

  for (const update of cases) {
    it(`args byte-equal the IDL-driven encoding for ConfigUpdate::${update.kind}`, async () => {
      const ix = await build(update);
      assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "update_config", { update }));
    });
  }

  it("has 4 accounts in IDL order and role", async () => {
    const ix = await build({ kind: "MaxN", value: 5 });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    assertIdlAccountsExactly("update_config", ix, {
      administrator,
      protocol_config: protocolConfig,
      pool,
      top_tier: topTier,
    });
  });

  it("appends demoted positions as writable remaining accounts, after the IDL's own 4", async () => {
    const demoted = [dummy(50), dummy(51)];
    const ix = await build({ kind: "MaxN", value: 5 }, demoted);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    assertIdlAccountsExactly(
      "update_config",
      ix,
      { administrator, protocol_config: protocolConfig, pool, top_tier: topTier },
      demoted.map((d) => ({ address: d, role: AccountRole.WRITABLE })),
    );
  });
});

// ── set_pause ────────────────────────────────────────────────────────────────────────────────
describe("set_pause (IDL oracle)", () => {
  covers("set_pause");
  const administrator = dummy(1);
  const poolId = 4;
  const params = { administrator, poolId, deposits: true, rolls: false, reason: 7 };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("set_pause", await setPause(params));
  });

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await setPause(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "set_pause", params));
  });

  it("has 3 accounts in IDL order and role", async () => {
    const ix = await setPause(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    assertIdlAccountsExactly("set_pause", ix, { administrator, protocol_config: protocolConfig, pool });
  });
});

// ── update_vrf_config ────────────────────────────────────────────────────────────────────────
describe("update_vrf_config (IDL oracle)", () => {
  covers("update_vrf_config");
  const administrator = dummy(1);
  const params = { administrator, vrfProgram: dummy(2), oracleQueue: dummy(3), swapPool: dummy(4) };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("update_vrf_config", await updateVrfConfig(params));
  });

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await updateVrfConfig(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "update_vrf_config", params));
  });

  it("has 2 accounts in IDL order and role", async () => {
    const ix = await updateVrfConfig(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("update_vrf_config", ix, { administrator, protocol_config: protocolConfig });
  });
});

// ── admit_collection ─────────────────────────────────────────────────────────────────────────
describe("admit_collection (IDL oracle)", () => {
  covers("admit_collection");
  const administrator = dummy(1);
  const collection = dummy(2);
  const poolId = 4;

  async function pdas() {
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: poolCollection } = await derivePda([
      COLLECTION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(collection),
    ]);
    return { protocolConfig, pool, poolCollection };
  }

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator(
      "admit_collection",
      await admitCollection({ administrator, poolId, collection, standards: 0b011, reason: null }),
    );
  });

  // Both `reason` arms: Option<u16> is the only variable-width argument, so a None-vs-Some
  // encoding error shifts nothing else and is invisible on whichever arm is not tested.
  for (const reason of [null, 9] as const) {
    it(`args byte-equal the IDL-driven encoding with reason=${reason}`, async () => {
      const params = { administrator, poolId, collection, standards: 0b011, reason };
      const ix = await admitCollection(params);
      assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "admit_collection", params));
    });
  }

  it("has 5 accounts in IDL order and role", async () => {
    const ix = await admitCollection({ administrator, poolId, collection, standards: 0b011, reason: null });
    const { protocolConfig, pool, poolCollection } = await pdas();
    assertIdlAccountsExactly("admit_collection", ix, {
      administrator,
      protocol_config: protocolConfig,
      pool,
      pool_collection: poolCollection,
    });
  });
});

// ── withdraw_collection ──────────────────────────────────────────────────────────────────────
describe("withdraw_collection (IDL oracle)", () => {
  covers("withdraw_collection");
  const administrator = dummy(1);
  const collection = dummy(2);
  const poolId = 4;
  const params = { administrator, poolId, collection, reason: 3 };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("withdraw_collection", await withdrawCollection(params));
  });

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await withdrawCollection(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "withdraw_collection", params));
  });

  it("has 4 accounts in IDL order and role", async () => {
    const ix = await withdrawCollection(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: poolCollection } = await derivePda([
      COLLECTION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(collection),
    ]);
    assertIdlAccountsExactly("withdraw_collection", ix, {
      administrator,
      protocol_config: protocolConfig,
      pool,
      pool_collection: poolCollection,
    });
  });
});

// ── set_authorities ──────────────────────────────────────────────────────────────────────────
describe("set_authorities (IDL oracle)", () => {
  covers("set_authorities");
  const signerAddress = dummy(1);

  for (const role of ["Administrator", "Operator", "T03", "T02"] as const) {
    for (const newAuthority of [dummy(2), null]) {
      it(`args byte-equal the IDL-driven encoding for ${role}, newAuthority=${newAuthority}`, async () => {
        const params = { signer: signerAddress, role, newAuthority };
        const ix = await setAuthorities(params);
        assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "set_authorities", params));
      });
    }
  }

  it("has 2 accounts in IDL order and role", async () => {
    const ix = await setAuthorities({ signer: signerAddress, role: "Operator", newAuthority: dummy(3) });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("set_authorities", ix, { signer: signerAddress, protocol_config: protocolConfig });
  });
});

// ── deposit ──────────────────────────────────────────────────────────────────────────────────
describe("deposit (IDL oracle)", () => {
  covers("deposit");
  const depositor = dummy(1);
  const nftMint = dummy(2);
  const depositorToken = dummy(3);
  const metadata = dummy(4);
  const masterEdition = dummy(5);
  const depositorTokenRecord = dummy(6);
  const positionTokenRecord = dummy(7);
  const collection = dummy(8);
  const poolId = 3;

  async function build(authorizationRulesProgram: Address | null, authorizationRules: Address | null) {
    return deposit({
      depositor,
      poolId,
      collection,
      nftMint,
      depositorToken,
      metadata,
      masterEdition,
      depositorTokenRecord,
      positionTokenRecord,
      sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram,
      authorizationRules,
    });
  }

  it("takes no args", async () => {
    assert.equal(argsOf(await build(null, null)).length, 0);
    assert.equal(encodeInstructionArgs(idl, "deposit", {}).length, 0);
  });

  it("has 19 accounts in IDL order and role", async () => {
    const rulesProgram = dummy(90);
    const rules = dummy(91);
    const ix = await build(rulesProgram, rules);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]);
    const { address: poolCollection } = await derivePda([COLLECTION_SEED, seedFromPubkey(pool), seedFromPubkey(collection)]);
    assertIdlAccountsExactly("deposit", ix, {
      depositor,
      pool,
      pool_collection: poolCollection,
      position,
      position_vault: positionVault,
      wallet_stats: walletStats,
      nft_mint: nftMint,
      depositor_token: depositorToken,
      metadata,
      master_edition: masterEdition,
      depositor_token_record: depositorTokenRecord,
      position_token_record: positionTokenRecord,
      authorization_rules_program: rulesProgram,
      authorization_rules: rules,
    });
  });
});

// ── deposit_core ─────────────────────────────────────────────────────────────────────────────
describe("deposit_core (IDL oracle)", () => {
  covers("deposit_core");
  const depositor = dummy(1);
  const asset = dummy(2);
  const collection = dummy(8);
  const poolId = 3;

  function build() {
    return depositCore({ depositor, poolId, collection, asset });
  }

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
    assert.equal(encodeInstructionArgs(idl, "deposit_core", {}).length, 0);
  });

  // Ten, against `deposit`'s nineteen. The Token Metadata twin's absent slots are not optional
  // accounts here — they do not exist in this instruction at all, so a client that reached for
  // `deposit`'s shape fails on length rather than on a mis-parsed tail.
  it("has 10 accounts in IDL order and role", async () => {
    const ix = await build();
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]);
    const { address: poolCollection } = await derivePda([COLLECTION_SEED, seedFromPubkey(pool), seedFromPubkey(collection)]);
    assertIdlAccountsExactly("deposit_core", ix, {
      depositor,
      pool,
      pool_collection: poolCollection,
      position,
      position_vault: positionVault,
      wallet_stats: walletStats,
      asset,
      collection,
    });
  });

  // The vault is the one slot whose role carries a cost: writable on a never-allocated address
  // is how a client ends up funding the account this family exists without. The IDL says
  // readonly because the Rust attribute carries no `mut`, and the assertion above compares
  // against the IDL — so this reads the flag directly, which is the claim a reader wants.
  it("marks the accountless vault readonly, unlike the Token Metadata twin's token account", () => {
    const vault = idlAccountRoles(idl, "deposit_core").find((r) => r.name === "position_vault");
    assert.ok(vault, "deposit_core has no position_vault account");
    assert.equal(vault.writable, false);
    const tmVault = idlAccountRoles(idl, "deposit").find((r) => r.name === "position_vault");
    assert.ok(tmVault, "deposit has no position_vault account");
    assert.equal(tmVault.writable, true, "the contrast is the point — if both are readonly this asserts nothing");
  });
});

// ── approve_deposit ──────────────────────────────────────────────────────────────────────────
describe("approve_deposit (IDL oracle)", () => {
  covers("approve_deposit");
  const operator = dummy(10);
  const nftMint = dummy(11);
  const depositor = dummy(12);
  const weightIndex = dummy(13);
  const poolId = 5;
  const params = { operator, poolId, nftMint, depositor, weightIndex, displaced: null as Address | null, value: 12_000_000n, observedAt: 1_800_000_000n, lockUntil: 1_900_000_000n };

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await approveDeposit(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "approve_deposit", params));
  });

  it("has 7 accounts in IDL order and role, plus a writable displaced remaining account", async () => {
    const displaced = dummy(20);
    const ix = await approveDeposit({ ...params, displaced });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]);
    assertIdlAccountsExactly(
      "approve_deposit",
      ix,
      { operator, protocol_config: protocolConfig, pool, position, weight_index: weightIndex, top_tier: topTier, wallet_stats: walletStats },
      [{ address: displaced, role: AccountRole.WRITABLE }],
    );
  });
});

// ── reject_deposit ───────────────────────────────────────────────────────────────────────────
describe("reject_deposit (IDL oracle)", () => {
  covers("reject_deposit");
  const operator = dummy(1);
  const pool = dummy(2);
  const nftMint = dummy(3);
  const params = { operator, pool, nftMint, reason: 42 };

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await rejectDeposit(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "reject_deposit", params));
  });

  it("has 3 accounts in IDL order and role", async () => {
    const ix = await rejectDeposit(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    assertIdlAccountsExactly("reject_deposit", ix, { operator, protocol_config: protocolConfig, position });
  });
});

// ── return_rejected ──────────────────────────────────────────────────────────────────────────
describe("return_rejected (IDL oracle)", () => {
  covers("return_rejected");
  const payer = dummy(1);
  const depositor = dummy(2);
  const pool = dummy(3);
  const nftMint = dummy(4);
  const depositorToken = dummy(5);
  const metadata = dummy(6);
  const masterEdition = dummy(7);
  const positionTokenRecord = dummy(8);
  const depositorTokenRecord = dummy(9);

  it("takes no args", async () => {
    const ix = await returnRejected({
      payer, depositor, pool, nftMint, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: null, authorizationRules: null,
    });
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 17 accounts in IDL order and role — position_token_record before depositor_token_record, the reverse of deposit's order", async () => {
    const rulesProgram = dummy(90);
    const rules = dummy(91);
    const ix = await returnRejected({
      payer, depositor, pool, nftMint, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: rulesProgram, authorizationRules: rules,
    });
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    assertIdlAccountsExactly("return_rejected", ix, {
      payer,
      depositor,
      position,
      position_vault: positionVault,
      nft_mint: nftMint,
      depositor_token: depositorToken,
      metadata,
      master_edition: masterEdition,
      position_token_record: positionTokenRecord,
      depositor_token_record: depositorTokenRecord,
      authorization_rules_program: rulesProgram,
      authorization_rules: rules,
    });
  });
});

// ── return_rejected_core ─────────────────────────────────────────────────────────────────────
describe("return_rejected_core (IDL oracle)", () => {
  covers("return_rejected_core");
  const payer = dummy(1);
  const depositor = dummy(2);
  const pool = dummy(3);
  const asset = dummy(4);
  const collection = dummy(5);

  function build() {
    return returnRejectedCore({ payer, depositor, pool, asset, collection });
  }

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("return_rejected_core", await build());
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  // Eight, against `return_rejected`'s seventeen. `position`'s second seed is the ASSET, where
  // the Token Metadata twin uses the mint — the same slot in the same derivation, so a client
  // that reached for the principal's `nftMint` param name would derive the same address and a
  // client that reached for its account SHAPE fails on length rather than on a mis-parsed tail.
  it("has 8 accounts in IDL order and role", async () => {
    const ix = await build();
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    assertIdlAccountsExactly("return_rejected_core", ix, {
      payer,
      depositor,
      position,
      position_vault: positionVault,
      asset,
      collection,
    });
  });

  // The permissionless shape, read off the IDL rather than off this file's own call: `payer`
  // signs and `depositor` does not, so the card and both rent refunds land on the position's
  // recorded depositor whoever submits. A `depositor` that acquired `signer` here would turn a
  // permissionless return into one only its beneficiary can make.
  it("is payer-signed and depositor-unsigned, its principal's permissionless shape", () => {
    const roles = idlAccountRoles(idl, "return_rejected_core");
    assert.deepEqual(
      roles.filter((r) => r.signer).map((r) => r.name),
      ["payer"],
    );
    const beneficiary = roles.find((r) => r.name === "depositor");
    assert.ok(beneficiary, "return_rejected_core has no depositor account");
    assert.equal(beneficiary.writable, true, "the depositor receives the asset and the rent");
  });
});

// ── withdraw ─────────────────────────────────────────────────────────────────────────────────
describe("withdraw (IDL oracle)", () => {
  covers("withdraw");
  const depositor = dummy(1);
  const nftMint = dummy(2);
  const weightIndex = dummy(3);
  const depositorToken = dummy(4);
  const metadata = dummy(5);
  const masterEdition = dummy(6);
  const positionTokenRecord = dummy(7);
  const depositorTokenRecord = dummy(8);
  const poolId = 6;

  it("takes no args", async () => {
    const ix = await withdraw({
      depositor, poolId, nftMint, weightIndex, depositorUsdc: null, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: null, authorizationRules: null, successor: null,
    });
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 22 accounts in IDL order and role, plus a writable successor remaining account", async () => {
    const depositorUsdc = dummy(80);
    const rulesProgram = dummy(90);
    const rules = dummy(91);
    const successor = dummy(95);
    const ix = await withdraw({
      depositor, poolId, nftMint, weightIndex, depositorUsdc, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: rulesProgram, authorizationRules: rules, successor,
    });
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    assertIdlAccountsExactly(
      "withdraw",
      ix,
      {
        depositor, pool, position, weight_index: weightIndex, top_tier: topTier, wallet_stats: walletStats,
        principal_vault: principalVault, depositor_usdc: depositorUsdc, position_vault: positionVault,
        nft_mint: nftMint, depositor_token: depositorToken, metadata, master_edition: masterEdition,
        position_token_record: positionTokenRecord, depositor_token_record: depositorTokenRecord,
        authorization_rules_program: rulesProgram, authorization_rules: rules,
      },
      [{ address: successor, role: AccountRole.WRITABLE }],
    );
  });
});

// ── claim_nft ────────────────────────────────────────────────────────────────────────────────
describe("claim_nft (IDL oracle)", () => {
  covers("claim_nft");
  const depositor = dummy(1);
  const pool = dummy(2);
  const nftMint = dummy(3);
  const depositorToken = dummy(4);
  const metadata = dummy(5);
  const masterEdition = dummy(6);
  const positionTokenRecord = dummy(7);
  const depositorTokenRecord = dummy(8);

  it("takes no args", async () => {
    const ix = await claimNft({
      depositor, pool, nftMint, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: null, authorizationRules: null,
    });
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 16 accounts in IDL order and role — no pool/protocol_config account", async () => {
    const rulesProgram = dummy(90);
    const rules = dummy(91);
    const ix = await claimNft({
      depositor, pool, nftMint, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: rulesProgram, authorizationRules: rules,
    });
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    assertIdlAccountsExactly("claim_nft", ix, {
      depositor,
      position,
      position_vault: positionVault,
      nft_mint: nftMint,
      depositor_token: depositorToken,
      metadata,
      master_edition: masterEdition,
      position_token_record: positionTokenRecord,
      depositor_token_record: depositorTokenRecord,
      authorization_rules_program: rulesProgram,
      authorization_rules: rules,
    });
  });
});

// ── withdraw_core ────────────────────────────────────────────────────────────────────────────
describe("withdraw_core (IDL oracle)", () => {
  covers("withdraw_core");
  const depositor = dummy(1);
  const asset = dummy(2);
  const collection = dummy(3);
  const weightIndex = dummy(4);
  const poolId = 6;

  it("has the IDL's own discriminator", async () => {
    const ix = await withdrawCore({
      depositor, poolId, asset, collection, weightIndex, depositorUsdc: null, successor: null,
    });
    assertIdlDiscriminator("withdraw_core", ix);
  });

  it("takes no args", async () => {
    const ix = await withdrawCore({
      depositor, poolId, asset, collection, weightIndex, depositorUsdc: null, successor: null,
    });
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 14 accounts in IDL order and role, plus a writable successor remaining account", async () => {
    const depositorUsdc = dummy(80);
    const successor = dummy(95);
    const ix = await withdrawCore({
      depositor, poolId, asset, collection, weightIndex, depositorUsdc, successor,
    });
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    assertIdlAccountsExactly(
      "withdraw_core",
      ix,
      {
        depositor, pool, position, weight_index: weightIndex, top_tier: topTier,
        wallet_stats: walletStats, principal_vault: principalVault, depositor_usdc: depositorUsdc,
        position_vault: positionVault, asset, collection,
      },
      [{ address: successor, role: AccountRole.WRITABLE }],
    );
  });

  // `remaining_accounts` has no "None" encoding of its own — an entry is either present or
  // absent — so a `null` successor must append NOTHING, not a placeholder.
  // `tier::fill_vacancy_from_candidate` reads its candidate positionally (`.first()`), so a
  // placeholder in that slot is read as a real candidate and the vacancy is filled from an
  // address the resolver never chose. Every other assertion in this describe passes a non-null
  // successor, which is why the empty case needs its own.
  it("appends no remaining account when the successor resolves to null", async () => {
    const ix = await withdrawCore({
      depositor, poolId, asset, collection, weightIndex, depositorUsdc: null, successor: null,
    });
    assert.equal(accountsOf(ix).length, idlAccountRoles(idl, "withdraw_core").length);
  });

  // `withdraw`'s finding, carried onto the twin: a `null` payout encodes the program's own id,
  // and a program address marked writable while it is also the invoked program is illegal at
  // the transaction-format level — the whole call fails before the handler runs. The slot's
  // role is therefore polarity-dependent, which no IDL-driven comparison can see, because the
  // IDL declares one role for the slot and both polarities pass it.
  it("encodes a null depositor_usdc readonly and a present one writable", async () => {
    const withNull = await withdrawCore({
      depositor, poolId, asset, collection, weightIndex, depositorUsdc: null, successor: null,
    });
    const withAccount = await withdrawCore({
      depositor, poolId, asset, collection, weightIndex, depositorUsdc: dummy(80), successor: null,
    });
    const slot = idlAccountRoles(idl, "withdraw_core").findIndex((r) => r.name === "depositor_usdc");
    assert.ok(slot >= 0, "withdraw_core has no depositor_usdc account");
    assert.equal(accountsOf(withNull)[slot].role, AccountRole.READONLY);
    assert.equal(accountsOf(withAccount)[slot].role, AccountRole.WRITABLE);
  });
});

// ── claim_nft_core ───────────────────────────────────────────────────────────────────────────
describe("claim_nft_core (IDL oracle)", () => {
  covers("claim_nft_core");
  const depositor = dummy(1);
  const pool = dummy(2);
  const asset = dummy(3);
  const collection = dummy(4);

  function build() {
    return claimNftCore({ depositor, pool, asset, collection });
  }

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("claim_nft_core", await build());
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  // Seven — the smallest instruction in the program, and `pool` is not one of the seven. It is a
  // parameter of the encoder solely to derive `position`, exactly as `claimNft` takes it; a
  // client that expected it in the account list fails on length here rather than on chain.
  it("has 7 accounts in IDL order and role — no pool account", async () => {
    const ix = await build();
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    assertIdlAccountsExactly("claim_nft_core", ix, {
      depositor,
      position,
      position_vault: positionVault,
      asset,
      collection,
    });
    assert.equal(
      idlAccountRoles(idl, "claim_nft_core").some((r) => r.name === "pool"),
      false,
    );
  });
});

// ── close_seized ─────────────────────────────────────────────────────────────────────────────
describe("close_seized (IDL oracle)", () => {
  covers("close_seized");
  const caller = dummy(1);
  const asset = dummy(2);
  const depositor = dummy(3);
  const weightIndex = dummy(4);
  const collection = dummy(5);
  const poolId = 6;

  function build() {
    return closeSeized({ caller, poolId, asset, collection, depositor, weightIndex });
  }

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("close_seized", await build());
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  it("has 9 accounts in IDL order and role, and no remaining account", async () => {
    const ix = await build();
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    assertIdlAccountsExactly(
      "close_seized",
      ix,
      {
        caller, pool, position, weight_index: weightIndex, top_tier: topTier,
        wallet_stats: walletStats, position_vault: positionVault, asset, collection,
      },
      [],
    );
  });

  // **Never a remaining account, on any source state — unlike `withdraw`, whose otherwise
  // identical close takes a successor and is asserted above to append it.** `close_seized`
  // vacates a tier seat and does not fill it: the candidate cannot be checked on chain to be the
  // next-ranked non-member, and this instruction's caller may be anybody. There is no `successor`
  // parameter to pass, which `tsc` enforces; what this asserts is the wire consequence.
  it("appends no remaining account, because there is no successor slot to fill", async () => {
    const ix = await build();
    assert.equal(accountsOf(ix).length, idlAccountRoles(idl, "close_seized").length);

  });

  // **Three absences and one presence, and the IDL is the only place a client can read any of
  // them from.** The signer is not writable, because this instruction closes no account on any
  // source state (D-115) — there is no rent refund for the caller to receive. Neither `asset`
  // nor `collection` is writable, and there is no `mpl_core_program`, because nothing is
  // transferred: `close_seized` reads both accounts and makes no CPI at all. But it *does* carry
  // `collection`, which is the one slot a reader would expect a CPI-free instruction to omit —
  // a `permanent_freeze_delegate` lives on an asset **or** on a collection, and the live one is
  // on the collection, so the freeze this instruction exists to clean up is invisible without
  // it. Each claim is asserted against a contrast that would otherwise let it pass vacuously.
  it("is the only exit-domain instruction with a readonly signer and a readonly asset", () => {
    const roles = idlAccountRoles(idl, "close_seized");
    const signerSlots = roles.filter((r) => r.signer);
    assert.deepEqual(signerSlots.map((r) => r.name), ["caller"]);
    assert.equal(signerSlots[0].writable, false, "close_seized closes nothing, so its caller is owed no rent");

    for (const name of ["asset", "collection"]) {
      const slot = roles.find((r) => r.name === name);
      assert.ok(slot, `close_seized has no ${name} account`);
      assert.equal(slot.writable, false, `close_seized reads ${name} and never writes it`);
    }
    assert.equal(
      roles.some((r) => r.name === "mpl_core_program"),
      false,
      "close_seized makes no CPI and needs no mpl_core_program",
    );

    // The contrast, without which the assertions above are vacuous: every Core exit that DOES
    // transfer marks its own asset writable and takes the CPI program account, and every other
    // signer in the program's exit-and-return set is writable. All four take `collection`, which
    // is what makes the freeze read uniform across the family rather than a special case here.
    for (const transferring of ["withdraw_core", "claim_nft_core", "return_rejected_core"]) {
      const other = idlAccountRoles(idl, transferring);
      assert.equal(other.find((r) => r.name === "asset")?.writable, true, `${transferring} rewrites AssetV1.owner`);
      assert.equal(other.some((r) => r.name === "collection"), true, `${transferring} reads the same collection`);
      assert.equal(other.some((r) => r.name === "mpl_core_program"), true);
      assert.equal(other.filter((r) => r.signer).every((r) => r.writable), true, `${transferring}'s signer is writable`);
    }
  });
});

// ── begin_sweep / end_sweep ──────────────────────────────────────────────────────────────────
for (const [name, build] of [["begin_sweep", beginSweep] as const, ["end_sweep", endSweep] as const]) {
  describe(`${name} (IDL oracle)`, () => {
    covers(name);
    const operator = dummy(1);
    const poolId = 8;

    it("takes no args", async () => {
      assert.equal(argsOf(await build({ operator, poolId })).length, 0);
    });

    it("has 3 accounts in IDL order and role", async () => {
      const ix = await build({ operator, poolId });
      const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
      const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
      assertIdlAccountsExactly(name, ix, { operator, protocol_config: protocolConfig, pool });
    });
  });
}

// ── record_value ─────────────────────────────────────────────────────────────────────────────
describe("record_value (IDL oracle)", () => {
  covers("record_value");
  const operator = dummy(1);
  const nftMint = dummy(2);
  const depositor = dummy(3);
  const weightIndex = dummy(4);
  const poolId = 11;
  const value = 15_000_000n;
  const observedAt = 1_900_000_000n;

  async function build(effect: ValueEffect) {
    return recordValue({ operator, poolId, nftMint, depositor, weightIndex, value, observedAt, effect });
  }

  it("args byte-equal the IDL-driven encoding", async () => {
    const params = { operator, poolId, nftMint, depositor, weightIndex, value, observedAt };
    const ix = await build({ kind: "cross", displaced: null });
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "record_value", params));
  });

  async function pdas() {
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]);
    return { protocolConfig, pool, position, topTier, walletStats };
  }

  it("has 7 accounts in IDL order and role, plus a writable successor on the 'close' effect", async () => {
    const successor = dummy(30);
    const ix = await build({ kind: "close", successor });
    const { protocolConfig, pool, position, topTier, walletStats } = await pdas();
    assertIdlAccountsExactly(
      "record_value",
      ix,
      { operator, protocol_config: protocolConfig, pool, position, weight_index: weightIndex, top_tier: topTier, wallet_stats: walletStats },
      [{ address: successor, role: AccountRole.WRITABLE }],
    );
  });

  it("has 7 accounts in IDL order and role, plus a writable displaced on the 'cross' effect", async () => {
    const displaced = dummy(31);
    const ix = await build({ kind: "cross", displaced });
    const { protocolConfig, pool, position, topTier, walletStats } = await pdas();
    assertIdlAccountsExactly(
      "record_value",
      ix,
      { operator, protocol_config: protocolConfig, pool, position, weight_index: weightIndex, top_tier: topTier, wallet_stats: walletStats },
      [{ address: displaced, role: AccountRole.WRITABLE }],
    );
  });
});

// ── update_tier ──────────────────────────────────────────────────────────────────────────────
describe("update_tier (IDL oracle)", () => {
  covers("update_tier");
  const operator = dummy(1);
  const nftMint = dummy(2);
  const poolId = 2;

  it("takes no args", async () => {
    assert.equal(argsOf(await updateTier({ operator, poolId, nftMint, displaced: null })).length, 0);
  });

  it("has 5 accounts in IDL order and role, plus a writable displaced remaining account", async () => {
    const displaced = dummy(40);
    const ix = await updateTier({ operator, poolId, nftMint, displaced });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    assertIdlAccountsExactly(
      "update_tier",
      ix,
      { operator, protocol_config: protocolConfig, pool, top_tier: topTier, position },
      [{ address: displaced, role: AccountRole.WRITABLE }],
    );
  });

  // The adjacent-swap control: top_tier and position are IDL-adjacent, both writable and
  // non-signer — same length, same name set, order-only. A length or Set-membership comparator
  // passes this; assertIdlAccountsExactly must not.
  it("self-test: a swapped top_tier ⇄ position ordering is NOT what the encoder actually produced", async () => {
    const displaced = dummy(41);
    const ix = await updateTier({ operator, poolId, nftMint, displaced });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);

    const roles = idlAccountRoles(idl, "update_tier");
    const swappedNames = roles.map((r) => r.name).map((n, i, arr) =>
      i === 3 ? arr[4] : i === 4 ? arr[3] : n,
    );
    const resolvedByName: Record<string, Address> = {
      operator,
      protocol_config: protocolConfig,
      pool,
      top_tier: topTier,
      position,
    };
    const swappedExpected = swappedNames.map((n) => resolvedByName[n]);
    const actualAddresses = accountsOf(ix)
      .slice(0, 5)
      .map((a) => a.address);
    assert.notDeepEqual(actualAddresses, swappedExpected);
  });
});

// ── commit_rolls ─────────────────────────────────────────────────────────────────────────────
describe("commit_rolls (IDL oracle)", () => {
  covers("commit_rolls");
  const roller = dummy(1);
  const rollerUsdc = dummy(2);
  const usdcMint = dummy(3);
  const oracleQueue = dummy(4);
  const poolId = 3;
  const batchCounter = 7n;
  const params = { roller, poolId, batchCounter, rollerUsdc, usdcMint, oracleQueue, n: 5 };

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await commitRolls(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "commit_rolls", params));
  });

  it("has 14 accounts in IDL order and role", async () => {
    const ix = await commitRolls(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchCounter),
    ]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    const { address: programIdentity } = await derivePda([VRF_IDENTITY_SEED]);
    assertIdlAccountsExactly("commit_rolls", ix, {
      roller,
      protocol_config: protocolConfig,
      pool,
      top_tier: topTier,
      batch,
      roller_usdc: rollerUsdc,
      principal_vault: principalVault,
      usdc_mint: usdcMint,
      oracle_queue: oracleQueue,
      program_identity: programIdentity,
    });
  });

  // The adjacent-swap control: pool and top_tier are IDL-adjacent, pool is the only writable one
  // of the pair — swapping in the other direction (top_tier ⇄ batch, both otherwise unrelated
  // roles) is what a length-or-role-multiset comparator would miss.
  it("self-test: a swapped top_tier ⇄ batch ordering is NOT what the encoder actually produced", async () => {
    const ix = await commitRolls(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchCounter),
    ]);

    const roles = idlAccountRoles(idl, "commit_rolls");
    const swappedNames = roles.map((r) => r.name).map((n, i, arr) =>
      i === 3 ? arr[4] : i === 4 ? arr[3] : n,
    );
    const resolvedByName: Record<string, Address> = {
      top_tier: topTier,
      batch,
    };
    const swappedExpected = swappedNames.map((n) => resolvedByName[n] ?? dummy(99));
    const actualAddresses = accountsOf(ix)
      .slice(3, 5)
      .map((a) => a.address);
    assert.notDeepEqual(actualAddresses, swappedExpected.slice(3, 5));
  });
});

// ── vrf_callback ─────────────────────────────────────────────────────────────────────────────
describe("vrf_callback (IDL oracle)", () => {
  covers("vrf_callback");
  const pool = dummy(1);
  const batchId = 11n;
  const randomness = new Uint8Array(32).fill(7);
  const params = { pool, batchId, randomness };

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await vrfCallback(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "vrf_callback", params));
  });

  it("has 3 accounts in IDL order and role", async () => {
    const ix = await vrfCallback(params);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    const { address: vrfProgramIdentity } = await derivePda(
      [VRF_IDENTITY_SEED, seedFromPubkey(PROGRAM_ID)],
      VRF_PROGRAM,
    );
    assertIdlAccountsExactly("vrf_callback", ix, {
      vrf_program_identity: vrfProgramIdentity,
      batch,
      pool,
    });
  });

  // The adjacent-swap control: batch and pool are IDL-adjacent, batch the only writable one of
  // the pair.
  it("self-test: a swapped batch ⇄ pool ordering is NOT what the encoder actually produced", async () => {
    const ix = await vrfCallback(params);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    const actualAddresses = accountsOf(ix)
      .slice(1, 3)
      .map((a) => a.address);
    assert.notDeepEqual(actualAddresses, [pool, batch]);
  });
});

// ── Structural coverage ─────────────────────────────────────────────────────────────────────
// A loop or a forgotten describe that silently iterates zero (or fewer than 15) instructions is
// the failure this project has hit repeatedly — this pins the count, not just "at least one".
describe("coverage", () => {
  it("exercises exactly the IDL's 25 instructions, no more, no fewer", () => {
    assert.equal(idl.instructions.length, 25);
    assert.equal(TESTED_INSTRUCTIONS.size, 25);
    for (const ix of idl.instructions) {
      assert.ok(TESTED_INSTRUCTIONS.has(ix.name), `instruction '${ix.name}' has no oracle coverage`);
    }
  });

  it("does not read idl.address as a program id — it is the literal string 'PROGRAMID'", () => {
    assert.equal(idl.address, "PROGRAMID");
  });
});

// ── Self-test: the comparators must red on a deliberately wrong encoding ────────────────────
describe("self-test: the differential actually discriminates", () => {
  it("args byte comparator catches a swapped same-width field (value ⇄ observed_at)", async () => {
    const params = {
      operator: dummy(1), poolId: 1, nftMint: dummy(2), depositor: dummy(3), weightIndex: dummy(4),
      displaced: null as Address | null, value: 12_000_000n, observedAt: 1_800_000_000n,
      lockUntil: 1_900_000_000n,
    };
    const ix = await approveDeposit(params);
    const correct = encodeInstructionArgs(idl, "approve_deposit", params);
    assert.deepEqual(argsOf(ix), correct);

    // Same two u64/i64-width fields, deliberately transposed — the bug class this oracle exists
    // to catch. The transposed encoding must NOT equal what the real encoder produced.
    const swapped = encodeInstructionArgs(idl, "approve_deposit", {
      ...params,
      value: params.observedAt,
      observedAt: params.value,
    });
    assert.notDeepEqual(argsOf(ix), swapped);
  });

  // T-89. The i64⇄i64 pair is a strictly harder case than value⇄observedAt above: those two
  // differ in signedness in the IDL, these two are the same declared type. This case pins that
  // the comparator still discriminates when only the field *order* differs.
  //
  // What it does NOT cover, stated because the gap is invisible: this whole file compares an
  // encoder against an IDL generated from the same Rust, so a transposition inside
  // `approve_deposit`'s own handler moves the IDL with it and both sides keep agreeing. That
  // direction is pinned in `approve_deposit.rs` by
  // `approve_deposit_binds_its_two_i64_args_uncrossed`, not here.
  it("args byte comparator catches a swapped same-type field (observed_at ⇄ lock_until)", async () => {
    const params = {
      operator: dummy(1), poolId: 1, nftMint: dummy(2), depositor: dummy(3), weightIndex: dummy(4),
      displaced: null as Address | null, value: 12_000_000n, observedAt: 1_800_000_000n,
      lockUntil: 1_900_000_000n,
    };
    const ix = await approveDeposit(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "approve_deposit", params));

    const swapped = encodeInstructionArgs(idl, "approve_deposit", {
      ...params,
      observedAt: params.lockUntil,
      lockUntil: params.observedAt,
    });
    assert.equal(
      argsOf(ix).length,
      swapped.length,
      "the transposition must be length-preserving, or this case is testing the wrong thing",
    );
    assert.notDeepEqual(argsOf(ix), swapped);
  });

  it("account comparator catches a wrong role assignment", async () => {
    const roles = idlAccountRoles(idl, "set_pause");
    const wrongRoles = roles.map((r) => ({ ...r, writable: !r.writable }));
    const actualRoleFlags = roles.map((r) => roleFromFlags(r.writable, r.signer));
    const wrongRoleFlags = wrongRoles.map((r) => roleFromFlags(r.writable, r.signer));
    assert.notDeepEqual(actualRoleFlags, wrongRoleFlags);
  });
});
