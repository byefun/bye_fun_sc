// IDL-driven differential oracle for the instruction encoders. Walks
// idl/bye_machine.json at test run time — args field list/types and account list/roles
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
//    file's expectations together is invisible to a differential test — it shows up as a diff of
//    the committed IDL in review, not as a failure here.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { AccountRole, getAddressDecoder, type Address, type Instruction } from "@solana/kit";
import { derivePda, seedFromPubkey, seedFromU64Le } from "../../scripts/lib/anchor.js";
import {
  CP_AMM_EVENT_AUTHORITY_SEED,
  CP_AMM_POOL_AUTHORITY,
  CP_AMM_PROGRAM,
  PROGRAM_ID,
  SYSTEM_PROGRAM,
  SYSVAR_INSTRUCTIONS,
  TOKEN_PROGRAM,
  VRF_PROGRAM,
} from "../../scripts/constants.js";
import {
  admitCollection,
  initProtocol,
  initPool,
  updateConfig,
  updateByeMint,
  updateProtocolRevenue,
  updateSwapPool,
  updateVrfConfig,
  setPause,
  setAuthorities,
  withdrawCollection,
  sweepT04Overflow,
  setT04Ceiling,
  consolidateTreasuries,
  migratePool,
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
import {
  claimFee,
  withdraw,
  withdrawCore,
  claimNft,
  claimNftCore,
  closeSeized,
} from "../../scripts/lib/exit.js";
import { beginSweep, endSweep, recordValue, type ValueEffect } from "../../scripts/lib/value.js";
import { updateTier } from "../../scripts/lib/tier.js";
import {
  claimFeeToEscrow,
  escrowDeposit,
  escrowWithdraw,
  openEscrow,
  setEscrowArmed,
} from "../../scripts/lib/escrow.js";
import {
  activateAcquired,
  deriveLoaderAccounts,
  instantSell,
  instantSellCore,
  type LoaderAccounts,
} from "../../scripts/lib/buyback.js";
import {
  t02ClaimFee,
  t02ClaimNft,
  t02ClaimNftCore,
  t02Deposit,
  t02DepositCore,
  t02Withdraw,
  t02WithdrawCore,
} from "../../scripts/lib/t02.js";
import {
  commitRolls,
  vrfCallback,
  settleRoll,
  settleRollCore,
  recoverBatch,
  closeBatch,
} from "../../scripts/lib/roll.js";
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
const TREASURY_02_SEED = "t02";
const TREASURY_03_SEED = "t03";
const PRINCIPAL_VAULT_SEED = "principal";
const TREASURY_04_SEED = "t04";
const BATCH_SEED = "batch";
const VRF_IDENTITY_SEED = "identity";
const ESCROW_SEED = "escrow";
const ESCROW_VAULT_SEED = "escrow_vault";

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
  const params = { payer, args };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("init_protocol", await initProtocol(params));
  });

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await initProtocol(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "init_protocol", params));
  });

  it("has 4 accounts in IDL order and role", async () => {
    const ix = await initProtocol(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("init_protocol", ix, {
      payer,
      protocol_config: protocolConfig,
      swap_pool: args.swapPool!,
    });
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
    blankFaces: [1_000_000n, 2_000_000n, 5_000_000n, 0n, 0n, 0n, 0n, 0n],
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

  it("has 11 accounts in IDL order and role", async () => {
    const ix = await initPool(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    const { address: treasury03 } = await derivePda([TREASURY_03_SEED]);
    const { address: treasury04 } = await derivePda([TREASURY_04_SEED]);
    assertIdlAccountsExactly("init_pool", ix, {
      administrator,
      protocol_config: protocolConfig,
      pool,
      weight_index: weightIndex,
      top_tier: topTier,
      principal_vault: principalVault,
      treasury_03: treasury03,
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
    { kind: "BlankFaces", value: [1n, 2n, 3n, 4n, 5n, 6n, 7n, 8n] },
    { kind: "EscrowBidBps", value: 9_250 },
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
  const params = {
    administrator,
    vrfProgram: dummy(2),
    oracleQueue: dummy(3),
    swapPool: dummy(4),
    maxSwapSlippageBps: 100,
  };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("update_vrf_config", await updateVrfConfig(params));
  });

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await updateVrfConfig(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "update_vrf_config", params));
  });

  it("has 3 accounts in IDL order and role", async () => {
    const ix = await updateVrfConfig(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("update_vrf_config", ix, {
      administrator,
      protocol_config: protocolConfig,
      swap_pool: params.swapPool,
    });
  });
});

for (const [name, build, field, account] of [
  ["update_bye_mint", updateByeMint, "byeMint", "bye_mint"],
  ["update_swap_pool", updateSwapPool, "swapPool", "swap_pool"],
] as const) {
  describe(`${name} (IDL oracle)`, () => {
    covers(name);
    const administrator = dummy(1);
    const configured = dummy(2);

    for (const value of [configured, null] as const) {
      it(`encodes ${value === null ? "None" : "Some"} from the IDL and preserves its account slot`, async () => {
        const params = { administrator, [field]: value } as never;
        const ix = await build(params);
        assertIdlDiscriminator(name, ix);
        assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, name, params));
        const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
        assertIdlAccountsExactly(name, ix, {
          administrator,
          protocol_config: protocolConfig,
          [account]: value ?? PROGRAM_ID,
        });
      });
    }
  });
}

// ── update_protocol_revenue ──────────────────────────────────────────────────────────────────
describe("update_protocol_revenue (IDL oracle)", () => {
  covers("update_protocol_revenue");
  const administrator = dummy(1);
  const protocolRevenue = dummy(2);
  const params = { administrator, protocolRevenue };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("update_protocol_revenue", await updateProtocolRevenue(params));
  });

  it("carries no args", async () => {
    const ix = await updateProtocolRevenue(params);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 3 accounts in IDL order and role", async () => {
    const ix = await updateProtocolRevenue(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("update_protocol_revenue", ix, {
      administrator,
      protocol_config: protocolConfig,
      protocol_revenue: protocolRevenue,
    });
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

  for (const role of ["Administrator", "Operator", "T05", "T02"] as const) {
    it(`args byte-equal the IDL-driven encoding for ${role}`, async () => {
      const params = { signer: signerAddress, role, newAuthority: dummy(2) };
      const ix = await setAuthorities(params);
      assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "set_authorities", params));
    });
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

  it("has 18 accounts in IDL order and role", async () => {
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

  it("has 18 accounts in IDL order and role — position_token_record before depositor_token_record, the reverse of deposit's order", async () => {
    const rulesProgram = dummy(90);
    const rules = dummy(91);
    const ix = await returnRejected({
      payer, depositor, pool, nftMint, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: rulesProgram, authorizationRules: rules,
    });
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("return_rejected", ix, {
      payer,
      protocol_config: protocolConfig,
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

  // Nine, against `return_rejected`'s eighteen. `position`'s second seed is the ASSET, where
  // the Token Metadata twin uses the mint — the same slot in the same derivation, so a client
  // that reached for the principal's `nftMint` param name would derive the same address and a
  // client that reached for its account SHAPE fails on length rather than on a mis-parsed tail.
  it("has 9 accounts in IDL order and role", async () => {
    const ix = await build();
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("return_rejected_core", ix, {
      payer,
      protocol_config: protocolConfig,
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

// ── claim_fee ────────────────────────────────────────────────────────────────────────────────
describe("claim_fee (IDL oracle)", () => {
  covers("claim_fee");
  const depositor = dummy(1);
  const nftMint = dummy(2);
  const depositorUsdc = dummy(3);
  const poolId = 4;

  function build() {
    return claimFee({ depositor, poolId, nftMint, depositorUsdc });
  }

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("claim_fee", await build());
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  it("has 7 accounts in IDL order and role, and no remaining account", async () => {
    const ix = await build();
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(nftMint),
    ]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: principalVault } = await derivePda([
      PRINCIPAL_VAULT_SEED,
      seedFromPubkey(pool),
    ]);
    assertIdlAccountsExactly(
      "claim_fee",
      ix,
      {
        depositor,
        pool,
        position,
        top_tier: topTier,
        principal_vault: principalVault,
        depositor_usdc: depositorUsdc,
      },
      [],
    );
  });

  // **`depositor_usdc` is required here, not optional.** `withdraw` carries a slot of the same
  // name that may go unfilled, because a withdraw with nothing to pay out is still a withdraw.
  // A fee claim's whole purpose is the payout, so the account is always present and always
  // writable — pinned rather than left to the encoder, since the two instructions sit in one
  // file and the optional spelling is the one a reader has just seen.
  it("names depositor_usdc writable, with no optional slot", async () => {
    const roles = idlAccountRoles(idl, "claim_fee");
    const slot = roles.find((r) => r.name === "depositor_usdc");
    assert.ok(slot, "claim_fee has no depositor_usdc account");
    assert.equal(slot.writable, true);
    const ix = await build();
    assert.equal(accountsOf(ix)[roles.indexOf(slot)].address, depositorUsdc);
  });

  // **Serves all three token standards with one instruction.** No fee-claim path reads
  // `position.standard` or moves a card, which is why — alone in the exit domain — it has no
  // `_core` twin. The evidence is structural: not one custody account in the list.
  it("carries no NFT-custody account, so it needs no _core twin", () => {
    const names = idlAccountRoles(idl, "claim_fee").map((r) => r.name);
    for (const custody of ["asset", "collection", "position_vault", "nft_mint", "depositor_nft"]) {
      assert.ok(!names.includes(custody), `claim_fee unexpectedly names '${custody}'`);
    }
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
      depositor, pool, nftMint, depositorUsdc: null, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: null, authorizationRules: null,
    });
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 19 accounts in IDL order and role — the pool and its vault, no protocol_config", async () => {
    const rulesProgram = dummy(90);
    const rules = dummy(91);
    const depositorUsdc = dummy(80);
    const ix = await claimNft({
      depositor, pool, nftMint, depositorUsdc, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: rulesProgram, authorizationRules: rules,
    });
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    assertIdlAccountsExactly("claim_nft", ix, {
      depositor,
      pool,
      position,
      position_vault: positionVault,
      principal_vault: principalVault,
      depositor_usdc: depositorUsdc,
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

  // A position owing nothing takes no USDC account. `null` encodes the program's own id, which
  // may not be marked writable, so the slot's role depends on polarity — the IDL declares one
  // role and both polarities pass it.
  it("encodes a null depositor_usdc readonly and a present one writable", async () => {
    const base = {
      depositor, pool, nftMint, depositorToken, metadata, masterEdition,
      positionTokenRecord, depositorTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: null, authorizationRules: null,
    };
    const slot = idlAccountRoles(idl, "claim_nft").findIndex((r) => r.name === "depositor_usdc");
    assert.ok(slot >= 0, "claim_nft has no depositor_usdc account");
    const withNull = accountsOf(await claimNft({ ...base, depositorUsdc: null }));
    const withAccount = accountsOf(await claimNft({ ...base, depositorUsdc: dummy(80) }));
    assert.equal(withNull[slot].role, AccountRole.READONLY);
    assert.equal(withAccount[slot].role, AccountRole.WRITABLE);
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

  function build(depositorUsdc: Address | null = null) {
    return claimNftCore({ depositor, pool, asset, collection, depositorUsdc });
  }

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("claim_nft_core", await build());
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  it("has 11 accounts in IDL order and role — the pool and its vault, the accountless vault readonly", async () => {
    const depositorUsdc = dummy(80);
    const ix = await build(depositorUsdc);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    assertIdlAccountsExactly("claim_nft_core", ix, {
      depositor,
      pool,
      position,
      position_vault: positionVault,
      principal_vault: principalVault,
      depositor_usdc: depositorUsdc,
      asset,
      collection,
    });
    const slot = accountsOf(ix).find((a) => a.address === positionVault);
    assert.ok(slot);
    assert.equal(slot.role, AccountRole.READONLY);
  });

  it("encodes a null depositor_usdc readonly and a present one writable", async () => {
    const slot = idlAccountRoles(idl, "claim_nft_core").findIndex((r) => r.name === "depositor_usdc");
    assert.ok(slot >= 0, "claim_nft_core has no depositor_usdc account");
    assert.equal(accountsOf(await build(null))[slot].role, AccountRole.READONLY);
    assert.equal(accountsOf(await build(dummy(80)))[slot].role, AccountRole.WRITABLE);
  });
});

// ── close_seized ─────────────────────────────────────────────────────────────────────────────
describe("close_seized (IDL oracle)", () => {
  covers("close_seized");
  const operator = dummy(1);
  const asset = dummy(2);
  const depositor = dummy(3);
  const weightIndex = dummy(4);
  const collection = dummy(5);
  const poolId = 6;

  function build() {
    return closeSeized({ operator, poolId, asset, collection, depositor, weightIndex });
  }

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("close_seized", await build());
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  it("has 10 accounts in IDL order and role, and no remaining account", async () => {
    const ix = await build();
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly(
      "close_seized",
      ix,
      {
        operator, protocol_config: protocolConfig, pool, position, weight_index: weightIndex,
        top_tier: topTier, wallet_stats: walletStats, position_vault: positionVault, asset,
        collection,
      },
      [],
    );
  });

  // **Never a remaining account, on any source state — unlike `withdraw`, whose otherwise
  // identical close takes a successor and is asserted above to append it.** `close_seized`
  // vacates a tier seat and does not fill it: the candidate cannot be checked on chain to be the
  // next-ranked non-member, so the choice would be made during someone else's forced closure.
  // There is no `successor` parameter to pass, which `tsc` enforces; what this asserts is the
  // wire consequence.
  it("appends no remaining account, because there is no successor slot to fill", async () => {
    const ix = await build();
    assert.equal(accountsOf(ix).length, idlAccountRoles(idl, "close_seized").length);

  });

  // **Three absences and one presence, and the IDL is the only place a client can read any of
  // them from.** The signer is not writable, because this instruction closes no account on any
  // source state — there is no rent refund for the Operator to receive. Neither `asset`
  // nor `collection` is writable, and there is no `mpl_core_program`, because nothing is
  // transferred: `close_seized` reads both accounts and makes no CPI at all. But it *does* carry
  // `collection`, which is the one slot a reader would expect a CPI-free instruction to omit —
  // a `permanent_freeze_delegate` lives on an asset **or** on a collection, and the live one is
  // on the collection, so the freeze this instruction exists to clean up is invisible without
  // it. Each claim is asserted against a contrast that would otherwise let it pass vacuously.
  it("is the only exit-domain instruction with a readonly signer and a readonly asset", () => {
    const roles = idlAccountRoles(idl, "close_seized");
    const signerSlots = roles.filter((r) => r.signer);
    assert.deepEqual(signerSlots.map((r) => r.name), ["operator"]);
    assert.equal(signerSlots[0].writable, false, "close_seized closes nothing, so its signer is owed no rent");

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

// ── sweep_t04_overflow ───────────────────────────────────────────────────────────────────────
// Five accounts, no args, and every pool as a read-only remaining account. Both treasuries are the
// shared PDAs, derived here independently of the encoder.
describe("sweep_t04_overflow (IDL oracle)", () => {
  covers("sweep_t04_overflow");
  const operator = dummy(1);
  const poolIds = [0, 1, 2];

  it("takes no args", async () => {
    assert.equal(argsOf(await sweepT04Overflow({ operator, poolIds })).length, 0);
  });

  it("has 5 accounts in IDL order and role, plus every pool as a read-only remaining account", async () => {
    const ix = await sweepT04Overflow({ operator, poolIds });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: treasury03 } = await derivePda([TREASURY_03_SEED]);
    const { address: treasury04 } = await derivePda([TREASURY_04_SEED]);
    const pools = await Promise.all(
      poolIds.map(async (id) => (await derivePda([POOL_SEED, seedFromU16Le(id)])).address),
    );
    assertIdlAccountsExactly(
      "sweep_t04_overflow",
      ix,
      {
        operator,
        protocol_config: protocolConfig,
        treasury_04: treasury04,
        treasury_03: treasury03,
        token_program: TOKEN_PROGRAM,
      },
      pools.map((address) => ({ address, role: AccountRole.READONLY })),
    );
  });
});

// ── set_t04_ceiling ──────────────────────────────────────────────────────────────────────────
describe("set_t04_ceiling (IDL oracle)", () => {
  covers("set_t04_ceiling");
  const administrator = dummy(1);
  const ceiling = 2_000_000_000n;

  it("encodes ceiling as the IDL's u64", async () => {
    const ix = await setT04Ceiling({ administrator, ceiling });
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "set_t04_ceiling", { ceiling }));
  });

  it("has 2 accounts in IDL order and role", async () => {
    const ix = await setT04Ceiling({ administrator, ceiling });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("set_t04_ceiling", ix, { administrator, protocol_config: protocolConfig });
  });
});

// ── consolidate_treasuries ───────────────────────────────────────────────────────────────────
// Ten accounts and no args: the pool's own two treasuries, by its seeds, and the shared two.
describe("consolidate_treasuries (IDL oracle)", () => {
  covers("consolidate_treasuries");
  const administrator = dummy(1);
  const usdcMint = dummy(2);
  const poolId = 9;

  it("takes no args", async () => {
    assert.equal(argsOf(await consolidateTreasuries({ administrator, poolId, usdcMint })).length, 0);
  });

  it("has 10 accounts in IDL order and role", async () => {
    const ix = await consolidateTreasuries({ administrator, poolId, usdcMint });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: legacyTreasury03 } = await derivePda([TREASURY_03_SEED, seedFromPubkey(pool)]);
    const { address: legacyTreasury04 } = await derivePda([TREASURY_04_SEED, seedFromPubkey(pool)]);
    const { address: treasury03 } = await derivePda([TREASURY_03_SEED]);
    const { address: treasury04 } = await derivePda([TREASURY_04_SEED]);
    assertIdlAccountsExactly("consolidate_treasuries", ix, {
      administrator,
      protocol_config: protocolConfig,
      pool,
      legacy_treasury_03: legacyTreasury03,
      legacy_treasury_04: legacyTreasury04,
      treasury_03: treasury03,
      treasury_04: treasury04,
      usdc_mint: usdcMint,
    });
  });
});

// ── The escrow instructions ─────────────────────────────────────────────────────────────────
// Every escrow account is derived here from the owner's key independently of the encoder, so an
// encoder that derived the escrow from the wrong key, or the vault from the owner rather than the
// escrow, fails the address comparison rather than agreeing with itself.
async function escrowPdas(owner: Address) {
  const { address: escrow } = await derivePda([ESCROW_SEED, seedFromPubkey(owner)]);
  const { address: escrowVault } = await derivePda([ESCROW_VAULT_SEED, seedFromPubkey(escrow)]);
  return { escrow, escrowVault };
}

describe("open_escrow (IDL oracle)", () => {
  covers("open_escrow");
  const owner = dummy(1);
  const usdcMint = dummy(2);

  it("takes no args", async () => {
    assert.equal(argsOf(await openEscrow({ owner, usdcMint })).length, 0);
  });

  it("has 7 accounts in IDL order and role", async () => {
    const ix = await openEscrow({ owner, usdcMint });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { escrow, escrowVault } = await escrowPdas(owner);
    assertIdlAccountsExactly("open_escrow", ix, {
      owner,
      protocol_config: protocolConfig,
      escrow,
      escrow_vault: escrowVault,
      usdc_mint: usdcMint,
      token_program: TOKEN_PROGRAM,
      system_program: SYSTEM_PROGRAM,
    });
  });
});

for (const [name, encode] of [
  ["escrow_deposit", escrowDeposit],
  ["escrow_withdraw", escrowWithdraw],
] as const) {
  describe(`${name} (IDL oracle)`, () => {
    covers(name);
    const owner = dummy(1);
    const ownerUsdc = dummy(2);
    const params = { owner, ownerUsdc, amount: 10_000_000n };

    it("encodes amount as the IDL's u64", async () => {
      const ix = await encode(params);
      assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, name, { amount: params.amount }));
    });

    it("has 5 accounts in IDL order and role", async () => {
      const ix = await encode(params);
      const { escrow, escrowVault } = await escrowPdas(owner);
      assertIdlAccountsExactly(name, ix, {
        owner,
        escrow,
        escrow_vault: escrowVault,
        owner_usdc: ownerUsdc,
        token_program: TOKEN_PROGRAM,
      });
    });
  });
}

describe("set_escrow_armed (IDL oracle)", () => {
  covers("set_escrow_armed");
  const owner = dummy(1);

  for (const armed of [true, false]) {
    it(`encodes armed=${armed} as the IDL's bool`, async () => {
      const ix = await setEscrowArmed({ owner, armed });
      assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "set_escrow_armed", { armed }));
    });
  }

  it("has 2 accounts in IDL order and role", async () => {
    const ix = await setEscrowArmed({ owner, armed: true });
    const { escrow } = await escrowPdas(owner);
    assertIdlAccountsExactly("set_escrow_armed", ix, { owner, escrow });
  });
});

describe("claim_fee_to_escrow (IDL oracle)", () => {
  covers("claim_fee_to_escrow");
  const depositor = dummy(1);
  const nftMint = dummy(2);
  const poolId = 4;

  it("takes no args", async () => {
    assert.equal(argsOf(await claimFeeToEscrow({ depositor, poolId, nftMint })).length, 0);
  });

  it("has 8 accounts in IDL order and role, the escrow derived from the depositor", async () => {
    const ix = await claimFeeToEscrow({ depositor, poolId, nftMint });
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(nftMint),
    ]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    const { escrow, escrowVault } = await escrowPdas(depositor);
    assertIdlAccountsExactly("claim_fee_to_escrow", ix, {
      depositor,
      pool,
      position,
      top_tier: topTier,
      principal_vault: principalVault,
      escrow,
      escrow_vault: escrowVault,
      token_program: TOKEN_PROGRAM,
    });
  });
});

// ── migrate_pool ─────────────────────────────────────────────────────────────────────────────
describe("migrate_pool (IDL oracle)", () => {
  covers("migrate_pool");
  const administrator = dummy(1);
  const poolId = 9;

  it("takes no args", async () => {
    assert.equal(argsOf(await migratePool({ administrator, poolId })).length, 0);
  });

  it("has 4 accounts in IDL order and role", async () => {
    const ix = await migratePool({ administrator, poolId });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    assertIdlAccountsExactly("migrate_pool", ix, {
      administrator,
      protocol_config: protocolConfig,
      pool,
    });
  });
});

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

// ── settle_roll ──────────────────────────────────────────────────────────────────────────────
describe("settle_roll (IDL oracle)", () => {
  covers("settle_roll");
  const operator = dummy(1);
  const pool = dummy(2);
  const batchId = 6n;
  const rollerUsdc = dummy(3);
  const protocolRevenue = dummy(4);
  const treasury05 = dummy(30);
  const weightIndex = dummy(5);
  const roller = dummy(6);
  const nftMint = dummy(7);
  const depositor = dummy(8);
  const settlementUsdc = dummy(9);
  const rollerToken = dummy(10);
  const metadata = dummy(11);
  const masterEdition = dummy(12);
  const positionTokenRecord = dummy(13);
  const rollerTokenRecord = dummy(14);

  /** The real-card shape: every position-bound slot present, standard 0. */
  const drawn = {
    operator, pool, batchId, rollIndex: 2, rollerUsdc, protocolRevenue, treasury05, weightIndex, roller,
    nftMint, depositor, settlementUsdc, rollerToken, metadata, masterEdition,
    positionTokenRecord, rollerTokenRecord, sysvarInstructions: SYSVAR_INSTRUCTIONS,
    authorizationRulesProgram: null, authorizationRules: null, successor: null,
    blank: null,
  };

  /** The blank shape: no position-bound slot, and the venue group present instead. */
  const blankVenue = {
    swapPool: dummy(20),
    rollerBye: dummy(21),
    byeMint: dummy(22),
    usdcMint: dummy(23),
    tokenAVault: dummy(24),
    tokenBVault: dummy(25),
  };

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await settleRoll(drawn);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "settle_roll", drawn));
  });

  it("a drawn blank fills the venue slots and leaves every position slot absent", async () => {
    const ix = await settleRoll({
      ...drawn,
      nftMint: null,
      depositor: null,
      settlementUsdc: null,
      rollerToken: null,
      metadata: null,
      masterEdition: null,
      positionTokenRecord: null,
      rollerTokenRecord: null,
      sysvarInstructions: null,
      blank: blankVenue,
    });
    const addresses = accountsOf(ix).map((a) => a.address);
    for (const [label, value] of Object.entries(blankVenue)) {
      assert.ok(addresses.includes(value), `${label} is not in the account list`);
    }
    // The three the venue fixes are derived by the builder, never taken from the caller.
    assert.ok(addresses.includes(CP_AMM_PROGRAM), "the venue program slot is unfilled");
    assert.ok(addresses.includes(CP_AMM_POOL_AUTHORITY), "the venue authority slot is unfilled");
  });

  it("a card draw leaves every venue slot absent", async () => {
    const ix = await settleRoll(drawn);
    const addresses = accountsOf(ix).map((a) => a.address);
    assert.ok(
      !addresses.includes(CP_AMM_PROGRAM),
      "the venue program is named on a roll that runs no swap",
    );
  });

  it("has 40 accounts in IDL order and role, plus a writable successor remaining account", async () => {
    const rulesProgram = dummy(90);
    const rules = dummy(91);
    const successor = dummy(95);
    // Every optional slot present, venue group included: the point is the full-width order and
    // each slot's role, and a placeholder in any slot would check the placeholder instead.
    const ix = await settleRoll({
      ...drawn,
      authorizationRulesProgram: rulesProgram,
      authorizationRules: rules,
      successor,
      blank: blankVenue,
    });
    const { address: cpAmmEventAuthority } = await derivePda(
      [CP_AMM_EVENT_AUTHORITY_SEED],
      CP_AMM_PROGRAM,
    );
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    const { address: treasury03 } = await derivePda([TREASURY_03_SEED]);
    const { address: treasury04 } = await derivePda([TREASURY_04_SEED]);
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(nftMint),
    ]);
    const { address: positionVault } = await derivePda([
      POSITION_VAULT_SEED,
      seedFromPubkey(position),
    ]);
    const { address: walletStats } = await derivePda([
      WALLET_STATS_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(depositor),
    ]);
    assertIdlAccountsExactly(
      "settle_roll",
      ix,
      {
        operator,
        protocol_config: protocolConfig,
        pool,
        top_tier: topTier,
        batch,
        principal_vault: principalVault,
        roller_usdc: rollerUsdc,
        treasury_04: treasury04,
        protocol_revenue: protocolRevenue,
        treasury_03: treasury03,
        treasury_05: treasury05,
        weight_index: weightIndex,
        position,
        wallet_stats: walletStats,
        depositor,
        settlement_usdc: settlementUsdc,
        position_vault: positionVault,
        nft_mint: nftMint,
        roller,
        roller_token: rollerToken,
        metadata,
        master_edition: masterEdition,
        position_token_record: positionTokenRecord,
        roller_token_record: rollerTokenRecord,
        authorization_rules_program: rulesProgram,
        authorization_rules: rules,
        swap_pool: blankVenue.swapPool,
        roller_bye: blankVenue.rollerBye,
        bye_mint: blankVenue.byeMint,
        usdc_mint: blankVenue.usdcMint,
        swap_token_a_vault: blankVenue.tokenAVault,
        swap_token_b_vault: blankVenue.tokenBVault,
        cp_amm_pool_authority: CP_AMM_POOL_AUTHORITY,
        cp_amm_event_authority: cpAmmEventAuthority,
        cp_amm_program: CP_AMM_PROGRAM,
      },
      [{ address: successor, role: AccountRole.WRITABLE }],
    );
  });

  // The adjacent-swap control: roller_usdc and treasury_04 are IDL-adjacent and both writable,
  // so only the address sequence separates them — and they are the two destinations this
  // instruction pays, which is the swap that would silently route a refund to the treasury.
  it("self-test: a swapped roller_usdc ⇄ treasury_04 ordering is NOT what the encoder actually produced", async () => {
    const ix = await settleRoll(drawn);
    const { address: treasury03 } = await derivePda([TREASURY_03_SEED]);
    const { address: treasury04 } = await derivePda([TREASURY_04_SEED]);
    const actualAddresses = accountsOf(ix)
      .slice(6, 8)
      .map((a) => a.address);
    assert.notDeepEqual(actualAddresses, [treasury04, rollerUsdc]);
  });

  // The second destination pair: `settlement_usdc` pays the
  // drawn position's depositor while `roller_usdc` refunds the roller. Both are writable USDC
  // accounts supplied by the caller, so nothing but the sequence tells them apart — and swapping
  // them pays a settlement to the roller who just lost the draw.
  it("self-test: a swapped roller_usdc ⇄ settlement_usdc ordering is NOT what the encoder produced", async () => {
    const ix = await settleRoll(drawn);
    const addresses = accountsOf(ix).map((a) => a.address);
    assert.equal(addresses[6], rollerUsdc);
    assert.equal(addresses[15], settlementUsdc);
    assert.notEqual(addresses[6], settlementUsdc);
  });

  // A roll over an emptied pool draws nothing, names no position, and runs no swap. Every
  // optional slot collapses to the program's own id, which may not be marked writable — so the
  // whole group takes the readonly role and the account count is unchanged.
  it("encodes a position-less roll with every optional slot as a readonly placeholder", async () => {
    const ix = await settleRoll({
      operator, pool, batchId, rollIndex: 0, rollerUsdc, protocolRevenue, treasury05, weightIndex, roller,
      nftMint: null, depositor: null, settlementUsdc: null, rollerToken: null, metadata: null,
      masterEdition: null, positionTokenRecord: null, rollerTokenRecord: null,
      sysvarInstructions: null, authorizationRulesProgram: null, authorizationRules: null,
      successor: null, blank: null,
    });
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 40, "the account list is fixed-width either way");

    const optional = accounts.slice(12, 37);
    assert.equal(optional.filter((a) => a.address === PROGRAM_ID).length, 23);
    for (const account of optional) {
      if (account.address !== PROGRAM_ID) continue;
      assert.equal(
        account.role,
        AccountRole.READONLY,
        "a program address may not be marked writable",
      );
    }
  });
});

// ── settle_roll_core ─────────────────────────────────────────────────────────────────────────
describe("settle_roll_core (IDL oracle)", () => {
  covers("settle_roll_core");
  const operator = dummy(1);
  const pool = dummy(2);
  const batchId = 6n;
  const rollerUsdc = dummy(3);
  const protocolRevenue = dummy(4);
  const treasury05 = dummy(30);
  const weightIndex = dummy(5);
  const roller = dummy(6);
  const asset = dummy(7);
  const collection = dummy(8);
  const depositor = dummy(9);
  const settlementUsdc = dummy(10);

  const drawn = {
    operator, pool, batchId, rollIndex: 2, rollerUsdc, protocolRevenue, treasury05, weightIndex, roller,
    asset, collection, depositor, settlementUsdc, successor: null,
  };

  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await settleRollCore(drawn);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "settle_roll_core", drawn));
  });

  it("has 23 accounts in IDL order and role, plus a writable successor remaining account", async () => {
    const successor = dummy(95);
    const ix = await settleRollCore({ ...drawn, successor });
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    const { address: treasury03 } = await derivePda([TREASURY_03_SEED]);
    const { address: treasury04 } = await derivePda([TREASURY_04_SEED]);
    // On this family the asset account is the position's own seed component — there is no mint.
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(asset),
    ]);
    const { address: positionVault } = await derivePda([
      POSITION_VAULT_SEED,
      seedFromPubkey(position),
    ]);
    const { address: walletStats } = await derivePda([
      WALLET_STATS_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(depositor),
    ]);
    assertIdlAccountsExactly(
      "settle_roll_core",
      ix,
      {
        operator,
        protocol_config: protocolConfig,
        pool,
        top_tier: topTier,
        batch,
        principal_vault: principalVault,
        roller_usdc: rollerUsdc,
        treasury_04: treasury04,
        protocol_revenue: protocolRevenue,
        treasury_03: treasury03,
        treasury_05: treasury05,
        weight_index: weightIndex,
        position,
        wallet_stats: walletStats,
        depositor,
        settlement_usdc: settlementUsdc,
        position_vault: positionVault,
        asset,
        collection,
        roller,
      },
      [{ address: successor, role: AccountRole.WRITABLE }],
    );
  });

  // The twin carries none of the Token Metadata account set, and this is where that stays true:
  // a slot added by resemblance to the principal would sit in the list unasserted otherwise.
  it("carries no Token Metadata account set", async () => {
    const names = idlAccountRoles(idl, "settle_roll_core").map((r) => r.name);
    for (const absent of [
      "nft_mint",
      "roller_token",
      "metadata",
      "master_edition",
      "position_token_record",
      "roller_token_record",
      "token_metadata_program",
      "authorization_rules",
      "authorization_rules_program",
      "sysvar_instructions",
      "associated_token_program",
    ]) {
      assert.ok(!names.includes(absent), `settle_roll_core must not carry ${absent}`);
    }
    assert.ok(names.includes("asset"));
    assert.ok(names.includes("collection"));
    assert.ok(names.includes("mpl_core_program"));
  });

  // The same adjacent-swap control the principal carries, over the same two USDC destinations.
  it("self-test: a swapped roller_usdc ⇄ settlement_usdc ordering is NOT what the encoder produced", async () => {
    const ix = await settleRollCore(drawn);
    const addresses = accountsOf(ix).map((a) => a.address);
    assert.equal(addresses[6], rollerUsdc);
    assert.equal(addresses[15], settlementUsdc);
    assert.notEqual(addresses[6], settlementUsdc);
  });

  it("encodes a position-less roll with every optional slot as a readonly placeholder", async () => {
    const ix = await settleRollCore({
      operator, pool, batchId, rollIndex: 0, rollerUsdc, protocolRevenue, treasury05, weightIndex, roller,
      asset: null, collection: null, depositor: null, settlementUsdc: null, successor: null,
    });
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 23, "the account list is fixed-width either way");

    const optional = accounts.slice(12, 19);
    assert.equal(optional.length, 7);
    for (const account of optional) {
      assert.equal(account.address, PROGRAM_ID);
      assert.equal(
        account.role,
        AccountRole.READONLY,
        "a program address may not be marked writable",
      );
    }
  });
});

// ── recover_batch ────────────────────────────────────────────────────────────────────────────
describe("recover_batch (IDL oracle)", () => {
  covers("recover_batch");
  const operator = dummy(1);
  const pool = dummy(2);
  const batchId = 4n;
  const rollerUsdc = dummy(3);
  const oracleQueue = dummy(4);
  const params = { operator, pool, batchId, rollerUsdc, oracleQueue };

  it("takes no args", async () => {
    const ix = await recoverBatch(params);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 8 accounts in IDL order and role", async () => {
    const ix = await recoverBatch(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
    assertIdlAccountsExactly("recover_batch", ix, {
      operator,
      protocol_config: protocolConfig,
      pool,
      batch,
      roller_usdc: rollerUsdc,
      principal_vault: principalVault,
      oracle_queue: oracleQueue,
    });
  });

  // The adjacent-swap control: pool and batch are IDL-adjacent, both writable — so only the
  // address (not the role) sequence would catch a swap here.
  it("self-test: a swapped pool ⇄ batch ordering is NOT what the encoder actually produced", async () => {
    const ix = await recoverBatch(params);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    const actualAddresses = accountsOf(ix)
      .slice(2, 4)
      .map((a) => a.address);
    assert.notDeepEqual(actualAddresses, [batch, pool]);
  });
});

// ── close_batch ──────────────────────────────────────────────────────────────────────────────
describe("close_batch (IDL oracle)", () => {
  covers("close_batch");
  const closer = dummy(3);
  const roller = dummy(1);
  const pool = dummy(2);
  const batchId = 9n;
  const params = { signer: closer, roller, pool, batchId };

  it("takes no args", async () => {
    const ix = await closeBatch(params);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 4 accounts in IDL order and role", async () => {
    const ix = await closeBatch(params);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assertIdlAccountsExactly("close_batch", ix, {
      signer: closer,
      roller,
      batch,
      protocol_config: protocolConfig,
    });
  });

  it("self-test: a swapped roller ⇄ batch ordering is NOT what the encoder actually produced", async () => {
    const ix = await closeBatch(params);
    const { address: batch } = await derivePda([
      BATCH_SEED,
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const actualAddresses = accountsOf(ix).map((a) => a.address);
    assert.notDeepEqual(actualAddresses, [closer, batch, roller, protocolConfig]);
  });
});

// ── instant_sell ─────────────────────────────────────────────────────────────────────────────
// The program's only two-signer instructions. The oracle's account-order comparison is what
// catches a signer landing in the wrong slot: both are signers, so a transposition of `seller`
// and `operator` keeps both the length and the role sequence intact and is invisible to anything
// weaker than a positional address comparison.
describe("instant_sell (IDL oracle)", () => {
  covers("instant_sell");
  const seller = dummy(1);
  const operator = dummy(2);
  const nftMint = dummy(4);
  const sellerToken = dummy(5);
  const sellerUsdc = dummy(6);
  const weightIndex = dummy(8);
  const metadata = dummy(9);
  const masterEdition = dummy(10);
  const sellerTokenRecord = dummy(11);
  const positionTokenRecord = dummy(12);
  const collection = dummy(13);
  const poolId = 3;
  const value = 12_000_000n;
  const observedAt = 1_800_000_000n;

  const params = {
    seller, operator, poolId, collection, nftMint, sellerToken, sellerUsdc,
    weightIndex, metadata, masterEdition, sellerTokenRecord, positionTokenRecord,
    sysvarInstructions: SYSVAR_INSTRUCTIONS,
    authorizationRulesProgram: dummy(90) as Address | null,
    authorizationRules: dummy(91) as Address | null,
    displaced: null as Address | null,
    loader: { escrow: dummy(20), escrowVault: dummy(21), walletStats: dummy(22) } as LoaderAccounts | null,
    value, observedAt,
  };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("instant_sell", await instantSell(params));
  });

  // `(value, observedAt)` and no `lockUntil`. The pair is u64 then i64, so a
  // transposition changes the bytes; the self-test below drives that case.
  it("args byte-equal the IDL-driven encoding", async () => {
    const ix = await instantSell(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "instant_sell", params));
  });

  it("has 29 accounts in IDL order and role", async () => {
    const ix = await instantSell(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: poolCollection } = await derivePda([COLLECTION_SEED, seedFromPubkey(pool), seedFromPubkey(collection)]);
    const { address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(t02Identity)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: treasury03 } = await derivePda([TREASURY_03_SEED]);
    assertIdlAccountsExactly("instant_sell", ix, {
      seller,
      operator,
      protocol_config: protocolConfig,
      pool,
      pool_collection: poolCollection,
      t02_identity: t02Identity,
      position,
      position_vault: positionVault,
      wallet_stats: walletStats,
      weight_index: weightIndex,
      top_tier: topTier,
      treasury_03: treasury03,
      seller_usdc: sellerUsdc,
      nft_mint: nftMint,
      seller_token: sellerToken,
      metadata,
      master_edition: masterEdition,
      seller_token_record: sellerTokenRecord,
      position_token_record: positionTokenRecord,
      authorization_rules_program: params.authorizationRulesProgram!,
      authorization_rules: params.authorizationRules!,
      loader_escrow: dummy(20),
      loader_escrow_vault: dummy(21),
      loader_wallet_stats: dummy(22),
    });
  });

  // A sale with no loader passes all three slots as the program id, which Anchor reads as `None`,
  // and readonly, since a program address may not be writable. The list keeps its length, so a
  // displaced tier member still lands after the named accounts.
  it("passes no loader as three readonly None placeholders", async () => {
    const ix = await instantSell({ ...params, loader: null, displaced: dummy(77) });
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 30);
    for (const slot of accounts.slice(26, 29)) {
      assert.equal(slot.address, PROGRAM_ID);
      assert.equal(slot.role, AccountRole.READONLY);
    }
    assert.equal(accounts[29].address, dummy(77));
  });

  it("derives the loader accounts from the loader's wallet", async () => {
    const loader = dummy(30);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: escrow } = await derivePda(["escrow", seedFromPubkey(loader)]);
    const { address: escrowVault } = await derivePda(["escrow_vault", seedFromPubkey(escrow)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(loader)]);
    assert.deepEqual(await deriveLoaderAccounts(poolId, loader), { escrow, escrowVault, walletStats });
  });

  // The wallet rollup is the T02 identity's, never the seller's: the position this creates is the
  // treasury's, and no seller rollup is read or written. Derived here from the
  // seller's key to prove the encoder did NOT do that.
  it("keys wallet_stats on the T02 identity and not on the seller", async () => {
    const ix = await instantSell(params);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: sellerStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(seller)]);
    assert.ok(
      !accountsOf(ix).some((a) => a.address === sellerStats),
      "the seller's own WalletStats reached the account list",
    );
  });

  it("self-test: a swapped seller ⇄ operator ordering is NOT what the encoder produced", async () => {
    const ix = await instantSell(params);
    const actual = accountsOf(ix).map((a) => a.address);
    const swapped = [...actual];
    [swapped[0], swapped[1]] = [swapped[1], swapped[0]];
    assert.notDeepEqual(actual, swapped);
  });
});

// ── activate_acquired ────────────────────────────────────────────────────────────────────────
// **The only instruction in the program with no args at all**, and the absence is the point: the
// attestation lives on the position and is read back, never re-supplied. An encoder that grew an
// arg would encode against an IDL that declares none, so the byte comparison below is the whole
// instrument — `encodeInstructionArgs` walks the IDL's own (empty) field list.
describe("activate_acquired (IDL oracle)", () => {
  covers("activate_acquired");
  const operator = dummy(2);
  const nftMint = dummy(4);
  const weightIndex = dummy(8);
  const poolId = 3;

  const params = {
    operator, poolId, nftMint, weightIndex,
    displaced: null as Address | null, depositor: null as Address | null,
  };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("activate_acquired", await activateAcquired(params));
  });

  it("encodes an empty arg buffer, as the IDL declares", async () => {
    const ix = await activateAcquired(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "activate_acquired", params));
    assert.equal(argsOf(ix).length, 0);
    // The control on the claim above: the IDL is what says there are no args, so read it
    // directly rather than trusting an encoder that could be empty for its own reasons.
    assert.deepEqual(findInstruction(idl, "activate_acquired").args, []);
  });

  it("has 7 accounts in IDL order and role", async () => {
    const ix = await activateAcquired(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(t02Identity)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    assertIdlAccountsExactly("activate_acquired", ix, {
      operator,
      protocol_config: protocolConfig,
      pool,
      position,
      weight_index: weightIndex,
      top_tier: topTier,
      wallet_stats: walletStats,
    });
  });

  // The rollup is the T02 identity's. Deriving one on the operator's key proves the encoder did
  // not reach for the caller instead — the program seeds this account on `position.depositor`,
  // so an operator-keyed address would simply fail to derive on chain and the mistake would look
  // like a missing account rather than a wrong one.
  it("keys wallet_stats on the T02 identity and not on the operator", async () => {
    const ix = await activateAcquired(params);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: operatorStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(operator)]);
    assert.ok(
      !accountsOf(ix).some((a) => a.address === operatorStats),
      "the operator's own WalletStats reached the account list",
    );
  });

  // The displaced member rides in remaining_accounts, exactly as on the two activating paths that
  // already carry one. Writable, because it is re-stamped and un-flagged by the settlement.
  // An escrow sale's position belongs to the loader, and the program seeds the rollup on the
  // position's depositor, so the encoder keys it on the loader when one is named.
  it("keys wallet_stats on the loader for an escrow sale's position", async () => {
    const loader = dummy(30);
    const ix = await activateAcquired({ ...params, depositor: loader });
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: loaderStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(loader)]);
    assert.equal(accountsOf(ix)[6]!.address, loaderStats);
  });

  it("appends a displaced tier member as one writable remaining account", async () => {
    const displaced = dummy(77);
    const without = accountsOf(await activateAcquired(params));
    const withOne = accountsOf(await activateAcquired({ ...params, displaced }));
    assert.equal(withOne.length, without.length + 1);
    assert.equal(withOne[withOne.length - 1].address, displaced);
    assert.equal(withOne[withOne.length - 1].role, AccountRole.WRITABLE);
  });
});

// ── instant_sell_core ────────────────────────────────────────────────────────────────────────
describe("instant_sell_core (IDL oracle)", () => {
  covers("instant_sell_core");
  const seller = dummy(1);
  const operator = dummy(2);
  const asset = dummy(4);
  const sellerUsdc = dummy(6);
  const weightIndex = dummy(8);
  const collection = dummy(13);
  const poolId = 3;

  const params = {
    seller, operator, poolId, collection, asset, sellerUsdc,
    weightIndex, displaced: null as Address | null,
    loader: { escrow: dummy(20), escrowVault: dummy(21), walletStats: dummy(22) } as LoaderAccounts | null,
    value: 12_000_000n, observedAt: 1_800_000_000n,
  };

  it("has the IDL's own discriminator", async () => {
    assertIdlDiscriminator("instant_sell_core", await instantSellCore(params));
  });

  it("takes the same two args as its principal", async () => {
    const ix = await instantSellCore(params);
    assert.deepEqual(argsOf(ix), encodeInstructionArgs(idl, "instant_sell_core", params));
    assert.deepEqual(argsOf(ix), argsOf(await instantSell({
      ...params, nftMint: asset, sellerToken: dummy(5), metadata: dummy(9), masterEdition: dummy(10),
      sellerTokenRecord: null, positionTokenRecord: null, sysvarInstructions: null,
      authorizationRulesProgram: null, authorizationRules: null,
    })));
  });

  it("has 21 accounts in IDL order and role", async () => {
    const ix = await instantSellCore(params);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: poolCollection } = await derivePda([COLLECTION_SEED, seedFromPubkey(pool), seedFromPubkey(collection)]);
    const { address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(t02Identity)]);
    const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
    const { address: treasury03 } = await derivePda([TREASURY_03_SEED]);
    assertIdlAccountsExactly("instant_sell_core", ix, {
      seller,
      operator,
      protocol_config: protocolConfig,
      pool,
      pool_collection: poolCollection,
      t02_identity: t02Identity,
      position,
      position_vault: positionVault,
      wallet_stats: walletStats,
      weight_index: weightIndex,
      top_tier: topTier,
      treasury_03: treasury03,
      seller_usdc: sellerUsdc,
      asset,
      collection,
      loader_escrow: dummy(20),
      loader_escrow_vault: dummy(21),
      loader_wallet_stats: dummy(22),
    });
  });

  it("passes no loader as three readonly None placeholders", async () => {
    const accounts = accountsOf(await instantSellCore({ ...params, loader: null }));
    assert.equal(accounts.length, 21);
    for (const slot of accounts.slice(18)) {
      assert.equal(slot.address, PROGRAM_ID);
      assert.equal(slot.role, AccountRole.READONLY);
    }
  });

  // Same claim deposit_core makes, and for the same cost: writable on a never-allocated address
  // is how a client ends up funding the account this family exists without.
  it("marks the accountless vault readonly, unlike the Token Metadata twin", async () => {
    const ix = await instantSellCore(params);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const slot = accountsOf(ix).find((a) => a.address === positionVault);
    assert.ok(slot, "the vault slot is missing");
    assert.equal(slot.role, AccountRole.READONLY);
  });
});

// ── The T02 authority's five instructions ───────────────────────────────────────────────────
// Each acts on a position the Treasury 02 identity owns. The signer is `t02_authority`, read from
// `ProtocolConfig`, so every account list carries `protocol_config` and `t02_identity` — and the
// fee always lands in Treasury 03, never in a wallet the caller names.
async function t02Pdas(poolId: number, nftMint: Address, collection: Address) {
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: poolCollection } = await derivePda([COLLECTION_SEED, seedFromPubkey(pool), seedFromPubkey(collection)]);
  const { address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]);
  const { address: position } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
  const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
  const { address: walletStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(t02Identity)]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: treasury03 } = await derivePda([TREASURY_03_SEED]);
  const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
  return { protocolConfig, pool, poolCollection, t02Identity, position, positionVault, walletStats, topTier, treasury03, principalVault };
}

describe("t02_claim_fee (IDL oracle)", () => {
  covers("t02_claim_fee");
  const t02Authority = dummy(1);
  const nftMint = dummy(2);
  const poolId = 4;
  const params = { t02Authority, poolId, nftMint };

  it("has the IDL's own discriminator and takes no args", async () => {
    const ix = await t02ClaimFee(params);
    assertIdlDiscriminator("t02_claim_fee", ix);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 8 accounts in IDL order and role", async () => {
    const p = await t02Pdas(poolId, nftMint, dummy(3));
    assertIdlAccountsExactly("t02_claim_fee", await t02ClaimFee(params), {
      t02_authority: t02Authority,
      protocol_config: p.protocolConfig,
      pool: p.pool,
      t02_identity: p.t02Identity,
      position: p.position,
      top_tier: p.topTier,
      principal_vault: p.principalVault,
      treasury_03: p.treasury03,
    }, []);
  });

  it("names no destination the caller chooses and no custody account", () => {
    const names = idlAccountRoles(idl, "t02_claim_fee").map((r) => r.name);
    for (const absent of ["depositor_usdc", "authority_usdc", "asset", "collection", "position_vault", "nft_mint"]) {
      assert.ok(!names.includes(absent), `t02_claim_fee unexpectedly names '${absent}'`);
    }
  });
});

describe("t02_withdraw (IDL oracle)", () => {
  covers("t02_withdraw");
  const t02Authority = dummy(1);
  const nftMint = dummy(2);
  const weightIndex = dummy(3);
  const authorityToken = dummy(4);
  const metadata = dummy(5);
  const masterEdition = dummy(6);
  const positionTokenRecord = dummy(7);
  const authorityTokenRecord = dummy(8);
  const poolId = 4;
  const params = {
    t02Authority, poolId, nftMint, weightIndex, authorityToken, metadata, masterEdition,
    positionTokenRecord, authorityTokenRecord,
    sysvarInstructions: SYSVAR_INSTRUCTIONS as Address | null,
    authorizationRulesProgram: dummy(90) as Address | null,
    authorizationRules: dummy(91) as Address | null,
    successor: null as Address | null,
  };

  it("has the IDL's own discriminator and takes no args", async () => {
    const ix = await t02Withdraw(params);
    assertIdlDiscriminator("t02_withdraw", ix);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 24 accounts in IDL order and role", async () => {
    const p = await t02Pdas(poolId, nftMint, dummy(9));
    assertIdlAccountsExactly("t02_withdraw", await t02Withdraw(params), {
      t02_authority: t02Authority,
      protocol_config: p.protocolConfig,
      pool: p.pool,
      t02_identity: p.t02Identity,
      position: p.position,
      weight_index: weightIndex,
      top_tier: p.topTier,
      wallet_stats: p.walletStats,
      principal_vault: p.principalVault,
      treasury_03: p.treasury03,
      position_vault: p.positionVault,
      nft_mint: nftMint,
      authority_token: authorityToken,
      metadata,
      master_edition: masterEdition,
      position_token_record: positionTokenRecord,
      authority_token_record: authorityTokenRecord,
      authorization_rules_program: params.authorizationRulesProgram!,
      authorization_rules: params.authorizationRules!,
    });
  });

  it("keys wallet_stats on the T02 identity and not on the signer", async () => {
    const ix = await t02Withdraw(params);
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
    const { address: signerStats } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(t02Authority)]);
    assert.ok(!accountsOf(ix).some((a) => a.address === signerStats));
  });

  it("appends a tier successor to remaining accounts only when one is owed", async () => {
    const without = accountsOf(await t02Withdraw(params)).length;
    const successor = dummy(50);
    const withIt = accountsOf(await t02Withdraw({ ...params, successor }));
    assert.equal(withIt.length, without + 1);
    assert.equal(withIt[withIt.length - 1].address, successor);
  });
});

describe("t02_withdraw_core (IDL oracle)", () => {
  covers("t02_withdraw_core");
  const t02Authority = dummy(1);
  const asset = dummy(2);
  const collection = dummy(3);
  const weightIndex = dummy(4);
  const poolId = 4;
  const params = { t02Authority, poolId, asset, collection, weightIndex, successor: null as Address | null };

  it("has the IDL's own discriminator and takes no args", async () => {
    const ix = await t02WithdrawCore(params);
    assertIdlDiscriminator("t02_withdraw_core", ix);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 14 accounts in IDL order and role", async () => {
    const p = await t02Pdas(poolId, asset, collection);
    assertIdlAccountsExactly("t02_withdraw_core", await t02WithdrawCore(params), {
      t02_authority: t02Authority,
      protocol_config: p.protocolConfig,
      pool: p.pool,
      t02_identity: p.t02Identity,
      position: p.position,
      weight_index: weightIndex,
      top_tier: p.topTier,
      wallet_stats: p.walletStats,
      principal_vault: p.principalVault,
      treasury_03: p.treasury03,
      position_vault: p.positionVault,
      asset,
      collection,
    });
  });

  it("marks the accountless vault readonly", async () => {
    const p = await t02Pdas(poolId, asset, collection);
    const slot = accountsOf(await t02WithdrawCore(params)).find((a) => a.address === p.positionVault);
    assert.ok(slot, "the vault slot is missing");
    assert.equal(slot.role, AccountRole.READONLY);
  });
});

describe("t02_claim_nft (IDL oracle)", () => {
  covers("t02_claim_nft");
  const t02Authority = dummy(1);
  const nftMint = dummy(2);
  const authorityToken = dummy(4);
  const metadata = dummy(5);
  const masterEdition = dummy(6);
  const positionTokenRecord = dummy(7);
  const authorityTokenRecord = dummy(8);
  const poolId = 4;
  const params = {
    t02Authority, poolId, nftMint, authorityToken, metadata, masterEdition,
    positionTokenRecord, authorityTokenRecord,
    sysvarInstructions: SYSVAR_INSTRUCTIONS as Address | null,
    authorizationRulesProgram: dummy(90) as Address | null,
    authorizationRules: dummy(91) as Address | null,
  };

  it("has the IDL's own discriminator and takes no args", async () => {
    const ix = await t02ClaimNft(params);
    assertIdlDiscriminator("t02_claim_nft", ix);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 21 accounts in IDL order and role", async () => {
    const p = await t02Pdas(poolId, nftMint, dummy(9));
    assertIdlAccountsExactly("t02_claim_nft", await t02ClaimNft(params), {
      t02_authority: t02Authority,
      protocol_config: p.protocolConfig,
      pool: p.pool,
      t02_identity: p.t02Identity,
      position: p.position,
      position_vault: p.positionVault,
      principal_vault: p.principalVault,
      treasury_03: p.treasury03,
      nft_mint: nftMint,
      authority_token: authorityToken,
      metadata,
      master_edition: masterEdition,
      position_token_record: positionTokenRecord,
      authority_token_record: authorityTokenRecord,
      authorization_rules_program: params.authorizationRulesProgram!,
      authorization_rules: params.authorizationRules!,
    });
  });
});

describe("t02_claim_nft_core (IDL oracle)", () => {
  covers("t02_claim_nft_core");
  const t02Authority = dummy(1);
  const asset = dummy(2);
  const collection = dummy(3);
  const poolId = 4;
  const params = { t02Authority, poolId, asset, collection };

  it("has the IDL's own discriminator and takes no args", async () => {
    const ix = await t02ClaimNftCore(params);
    assertIdlDiscriminator("t02_claim_nft_core", ix);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 13 accounts in IDL order and role, with the accountless vault readonly", async () => {
    const p = await t02Pdas(poolId, asset, collection);
    const ix = await t02ClaimNftCore(params);
    assertIdlAccountsExactly("t02_claim_nft_core", ix, {
      t02_authority: t02Authority,
      protocol_config: p.protocolConfig,
      pool: p.pool,
      t02_identity: p.t02Identity,
      position: p.position,
      position_vault: p.positionVault,
      principal_vault: p.principalVault,
      treasury_03: p.treasury03,
      asset,
      collection,
    });
    const slot = accountsOf(ix).find((a) => a.address === p.positionVault);
    assert.ok(slot);
    assert.equal(slot.role, AccountRole.READONLY);
  });
});

describe("t02_deposit (IDL oracle)", () => {
  covers("t02_deposit");
  const t02Authority = dummy(1);
  const nftMint = dummy(4);
  const authorityToken = dummy(5);
  const metadata = dummy(9);
  const masterEdition = dummy(10);
  const authorityTokenRecord = dummy(11);
  const positionTokenRecord = dummy(12);
  const collection = dummy(13);
  const poolId = 3;
  const params = {
    t02Authority, poolId, collection, nftMint, authorityToken, metadata, masterEdition,
    authorityTokenRecord, positionTokenRecord,
    sysvarInstructions: SYSVAR_INSTRUCTIONS as Address | null,
    authorizationRulesProgram: dummy(90) as Address | null,
    authorizationRules: dummy(91) as Address | null,
  };

  // `deposit` takes no args and neither does this: the position is created `Pending` with no value
  // on it, and the Operator's approval is what attests one.
  it("has the IDL's own discriminator and takes no args", async () => {
    const ix = await t02Deposit(params);
    assertIdlDiscriminator("t02_deposit", ix);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 21 accounts in IDL order and role", async () => {
    const p = await t02Pdas(poolId, nftMint, collection);
    assertIdlAccountsExactly("t02_deposit", await t02Deposit(params), {
      t02_authority: t02Authority,
      protocol_config: p.protocolConfig,
      pool: p.pool,
      t02_identity: p.t02Identity,
      pool_collection: p.poolCollection,
      position: p.position,
      position_vault: p.positionVault,
      wallet_stats: p.walletStats,
      nft_mint: nftMint,
      authority_token: authorityToken,
      metadata,
      master_edition: masterEdition,
      authority_token_record: authorityTokenRecord,
      position_token_record: positionTokenRecord,
      authorization_rules_program: params.authorizationRulesProgram!,
      authorization_rules: params.authorizationRules!,
    });
  });

  it("moves no USDC and has no Operator signer: no Treasury 03 or USDC account in the list", () => {
    const roles = idlAccountRoles(idl, "t02_deposit");
    const names = roles.map((r) => r.name);
    for (const absent of ["treasury_03", "seller_usdc", "authority_usdc", "principal_vault", "operator"]) {
      assert.ok(!names.includes(absent), `t02_deposit unexpectedly names '${absent}'`);
    }
    assert.deepEqual(roles.filter((r) => r.signer).map((r) => r.name), ["t02_authority"]);
  });
});

describe("t02_deposit_core (IDL oracle)", () => {
  covers("t02_deposit_core");
  const t02Authority = dummy(1);
  const asset = dummy(4);
  const collection = dummy(13);
  const poolId = 3;
  const params = { t02Authority, poolId, collection, asset };

  it("has the IDL's own discriminator and takes no args", async () => {
    const ix = await t02DepositCore(params);
    assertIdlDiscriminator("t02_deposit_core", ix);
    assert.equal(argsOf(ix).length, 0);
  });

  it("has 12 accounts in IDL order and role, with the accountless vault readonly", async () => {
    const p = await t02Pdas(poolId, asset, collection);
    const ix = await t02DepositCore(params);
    assertIdlAccountsExactly("t02_deposit_core", ix, {
      t02_authority: t02Authority,
      protocol_config: p.protocolConfig,
      pool: p.pool,
      t02_identity: p.t02Identity,
      pool_collection: p.poolCollection,
      position: p.position,
      position_vault: p.positionVault,
      wallet_stats: p.walletStats,
      asset,
      collection,
    });
    const slot = accountsOf(ix).find((a) => a.address === p.positionVault);
    assert.ok(slot);
    assert.equal(slot.role, AccountRole.READONLY);
  });
});

// ── Structural coverage ─────────────────────────────────────────────────────────────────────
// A loop or a forgotten describe that silently iterates zero (or fewer than 15) instructions is
// the failure this project has hit repeatedly — this pins the count, not just "at least one".
describe("coverage", () => {
  it("exercises exactly the IDL's 52 instructions, no more, no fewer", () => {
    assert.equal(idl.instructions.length, 52);
    assert.equal(TESTED_INSTRUCTIONS.size, 52);
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

  // The i64⇄i64 pair is a strictly harder case than value⇄observedAt above: those two
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
