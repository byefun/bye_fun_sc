// Instruction encoders for the administration instructions: init_protocol, init_pool,
// update_config, update_vrf_config, set_pause, set_authorities. Account order and arg shapes
// are derived mechanically from target/idl/bye_machine.json, cross-checked against each
// instruction's #[derive(Accounts)] struct in programs/bye_machine/src/instructions/admin/.
//
// Pure and synchronous with respect to I/O: every state-dependent account (an existing pool's
// id, `update_config`'s demoted positions) is a required parameter — these encoders derive PDAs
// but never read on-chain state themselves.

import type { Address, Instruction } from "@solana/kit";
import {
  Borsh,
  buildAnchorIx,
  derivePda,
  readonly,
  seedFromPubkey,
  signer,
  writable,
  writableSigner,
} from "./anchor.js";
import { SYSTEM_PROGRAM, TOKEN_PROGRAM } from "../constants.js";

// ── Seeds (bye_machine-specific — anchor.ts stays program-agnostic) ─────────────────────────
// Verbatim against programs/bye_machine/src/common/seeds.rs.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const COLLECTION_SEED = "collection";
const PRINCIPAL_VAULT_SEED = "principal";
const TREASURY_04_SEED = "t04";
const TOP_TIER_SEED = "tier";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

// ── init_protocol ────────────────────────────────────────────────────────────────────────────
export type ProtocolConfigArgs = {
  operator: Address;
  t03Authority: Address;
  t02Authority: Address;
  protocolRevenue: Address;
  usdcMint: Address;
  byeMint: Address;
  vrfProgram: Address;
  oracleQueue: Address;
  swapPool: Address;
  maxSwapSlippageBps: number;
};

function encodeProtocolConfigArgs(args: ProtocolConfigArgs): Buffer {
  return new Borsh()
    .pubkey(args.operator)
    .pubkey(args.t03Authority)
    .pubkey(args.t02Authority)
    .pubkey(args.protocolRevenue)
    .pubkey(args.usdcMint)
    .pubkey(args.byeMint)
    .pubkey(args.vrfProgram)
    .pubkey(args.oracleQueue)
    .pubkey(args.swapPool)
    .u16(args.maxSwapSlippageBps)
    .build();
}

export async function initProtocol(params: {
  payer: Address;
  args: ProtocolConfigArgs;
}): Promise<Instruction> {
  const { payer, args } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);

  return buildAnchorIx({
    name: "init_protocol",
    args: encodeProtocolConfigArgs(args),
    accounts: [writableSigner(payer), writable(protocolConfig), readonly(SYSTEM_PROGRAM)],
  });
}

// ── init_pool ─────────────────────────────────────────────────────────────────────────────────
export type PoolConfigArgs = {
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
  blankFaces: [bigint, bigint, bigint];
  t04Ceiling: bigint;
  sweepCadenceHours: number;
  tierSize: number;
};

function encodePoolConfigArgs(args: PoolConfigArgs): Buffer {
  return new Borsh()
    .pubkey(args.treasury03)
    .u64(args.price)
    .u64(args.ticketTarget)
    .u64(args.fee)
    .u16(args.allocEqualBps)
    .u16(args.allocTierBps)
    .u16(args.allocProtocolBps)
    .u64(args.admissionFloor)
    .u64(args.admissionCeiling)
    .u64(args.walletValueCap)
    .u8(args.maxN)
    .u16(args.buybackRateBps)
    .arrayU64(args.blankFaces, 3)
    .u64(args.t04Ceiling)
    .u16(args.sweepCadenceHours)
    .u8(args.tierSize)
    .build();
}

/**
 * `poolId` is `protocol_config.pool_counter` at the moment of the call — the caller's
 * responsibility to read, since this encoder does no I/O. `weightIndex` must already be a
 * zero-initialised account of the exact `WeightIndex` layout size (T-45's job, not this
 * encoder's). No `rent` account, no `set_compute_unit_limit`.
 */
export async function initPool(params: {
  administrator: Address;
  poolId: number;
  weightIndex: Address;
  usdcMint: Address;
  args: PoolConfigArgs;
}): Promise<Instruction> {
  const { administrator, poolId, weightIndex, usdcMint, args } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: principalVault } = await derivePda([
    PRINCIPAL_VAULT_SEED,
    seedFromPubkey(pool),
  ]);
  const { address: treasury04 } = await derivePda([TREASURY_04_SEED, seedFromPubkey(pool)]);

  return buildAnchorIx({
    name: "init_pool",
    args: encodePoolConfigArgs(args),
    accounts: [
      writableSigner(administrator),
      writable(protocolConfig),
      writable(pool),
      writable(weightIndex),
      writable(topTier),
      writable(principalVault),
      writable(treasury04),
      readonly(usdcMint),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── update_config ────────────────────────────────────────────────────────────────────────────
export type ConfigUpdate =
  | { kind: "AdmissionFloor"; value: bigint }
  | { kind: "AdmissionCeiling"; value: bigint }
  | { kind: "WalletValueCap"; value: bigint }
  | { kind: "MaxN"; value: number }
  | { kind: "BuybackRateBps"; value: number }
  | { kind: "T04Ceiling"; value: bigint }
  | { kind: "SweepCadenceHours"; value: number }
  | { kind: "TierSize"; value: number }
  | { kind: "Pricing"; price: bigint; ticketTarget: bigint; fee: bigint }
  | { kind: "Allocation"; equalBps: number; tierBps: number; protocolBps: number }
  | { kind: "BlankFaces"; value: [bigint, bigint, bigint] };

// Variant index is declaration order in ConfigUpdate (update_config.rs), not this union's order.
function encodeConfigUpdate(update: ConfigUpdate): Buffer {
  switch (update.kind) {
    case "AdmissionFloor":
      return new Borsh().enumVariant(0, new Borsh().u64(update.value).build()).build();
    case "AdmissionCeiling":
      return new Borsh().enumVariant(1, new Borsh().u64(update.value).build()).build();
    case "WalletValueCap":
      return new Borsh().enumVariant(2, new Borsh().u64(update.value).build()).build();
    case "MaxN":
      return new Borsh().enumVariant(3, new Borsh().u8(update.value).build()).build();
    case "BuybackRateBps":
      return new Borsh().enumVariant(4, new Borsh().u16(update.value).build()).build();
    case "T04Ceiling":
      return new Borsh().enumVariant(5, new Borsh().u64(update.value).build()).build();
    case "SweepCadenceHours":
      return new Borsh().enumVariant(6, new Borsh().u16(update.value).build()).build();
    case "TierSize":
      return new Borsh().enumVariant(7, new Borsh().u8(update.value).build()).build();
    case "Pricing":
      return new Borsh()
        .enumVariant(
          8,
          new Borsh().u64(update.price).u64(update.ticketTarget).u64(update.fee).build(),
        )
        .build();
    case "Allocation":
      return new Borsh()
        .enumVariant(
          9,
          new Borsh().u16(update.equalBps).u16(update.tierBps).u16(update.protocolBps).build(),
        )
        .build();
    case "BlankFaces":
      return new Borsh().enumVariant(10, new Borsh().arrayU64(update.value, 3).build()).build();
  }
}

/** `TopTier`'s `entries` capacity — a `tier_size` lowering can demote at most this many. */
const MAX_TIER_SIZE = 20;

/**
 * `demoted` carries `demote_shrink_excess`'s remaining_accounts on a `TierSize` lowering — by
 * key, any order, required and non-defaulted so a caller can't silently omit them (the program
 * fails loudly with 3005, but only after a wasted transaction). Empty for every other variant.
 * Throws above 19 (`TopTier.entries`'s capacity) rather than letting the program reject it.
 */
export async function updateConfig(params: {
  administrator: Address;
  poolId: number;
  update: ConfigUpdate;
  demoted: Address[];
}): Promise<Instruction> {
  const { administrator, poolId, update, demoted } = params;
  if (demoted.length > MAX_TIER_SIZE - 1) {
    throw new Error(
      `update_config: at most ${MAX_TIER_SIZE - 1} demoted positions, got ${demoted.length}`,
    );
  }

  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);

  return buildAnchorIx({
    name: "update_config",
    args: encodeConfigUpdate(update),
    accounts: [signer(administrator), readonly(protocolConfig), writable(pool), writable(topTier)],
    remaining: demoted.map(writable),
  });
}

// ── set_pause ─────────────────────────────────────────────────────────────────────────────────
export async function setPause(params: {
  administrator: Address;
  poolId: number;
  deposits: boolean;
  rolls: boolean;
  reason: number;
}): Promise<Instruction> {
  const { administrator, poolId, deposits, rolls, reason } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);

  return buildAnchorIx({
    name: "set_pause",
    args: new Borsh().bool(deposits).bool(rolls).u16(reason).build(),
    accounts: [signer(administrator), readonly(protocolConfig), writable(pool)],
  });
}

// ── update_vrf_config ────────────────────────────────────────────────────────────────────────
/**
 * All three fields move together — `vrf_program`, `oracle_queue` and `swap_pool` are write-once
 * at `init_protocol` otherwise, with no partial-update form.
 */
export async function updateVrfConfig(params: {
  administrator: Address;
  vrfProgram: Address;
  oracleQueue: Address;
  swapPool: Address;
}): Promise<Instruction> {
  const { administrator, vrfProgram, oracleQueue, swapPool } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);

  return buildAnchorIx({
    name: "update_vrf_config",
    args: new Borsh().pubkey(vrfProgram).pubkey(oracleQueue).pubkey(swapPool).build(),
    accounts: [signer(administrator), writable(protocolConfig)],
  });
}

// ── admit_collection / withdraw_collection ───────────────────────────────────────────────────

/**
 * `reason` is required exactly when the update clears a `standards` bit and rejected otherwise —
 * the program enforces both directions (6509 / 6510), so this encoder passes it through rather
 * than defaulting it. `null` is `None`.
 */
export async function admitCollection(params: {
  administrator: Address;
  poolId: number;
  collection: Address;
  standards: number;
  reason: number | null;
}): Promise<Instruction> {
  const { administrator, poolId, collection, standards, reason } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: poolCollection } = await derivePda([
    COLLECTION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(collection),
  ]);

  return buildAnchorIx({
    name: "admit_collection",
    args: new Borsh().pubkey(collection).u8(standards).optionU16(reason).build(),
    accounts: [
      writableSigner(administrator),
      readonly(protocolConfig),
      readonly(pool),
      writable(poolCollection),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

/** `reason` is mandatory here — closing the record leaves the event log as the only record of
 * what was admitted and why it stopped being. */
export async function withdrawCollection(params: {
  administrator: Address;
  poolId: number;
  collection: Address;
  reason: number;
}): Promise<Instruction> {
  const { administrator, poolId, collection, reason } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: poolCollection } = await derivePda([
    COLLECTION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(collection),
  ]);

  return buildAnchorIx({
    name: "withdraw_collection",
    args: new Borsh().pubkey(collection).u16(reason).build(),
    accounts: [
      writableSigner(administrator),
      readonly(protocolConfig),
      readonly(pool),
      writable(poolCollection),
    ],
  });
}

// ── set_authorities ──────────────────────────────────────────────────────────────────────────
export type AuthorityRole = "Administrator" | "Operator" | "T03" | "T02";

function authorityRoleIndex(role: AuthorityRole): number {
  switch (role) {
    case "Administrator":
      return 0;
    case "Operator":
      return 1;
    case "T03":
      return 2;
    case "T02":
      return 3;
  }
}

/**
 * `newAuthority: null` is the accept phase (the pending authority claims the role); non-null is
 * the propose phase (the administrator nominates a pending authority for `role`).
 */
export async function setAuthorities(params: {
  signer: Address;
  role: AuthorityRole;
  newAuthority: Address | null;
}): Promise<Instruction> {
  const { signer: signerAddress, role, newAuthority } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);

  return buildAnchorIx({
    name: "set_authorities",
    args: new Borsh().enumVariant(authorityRoleIndex(role)).optionPubkey(newAuthority).build(),
    accounts: [signer(signerAddress), writable(protocolConfig)],
  });
}
