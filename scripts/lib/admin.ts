// Instruction encoders for the administration instructions: init_protocol, init_pool,
// update_config, update_vrf_config, set_pause, set_authorities. Account order and arg shapes
// are derived mechanically from idl/bye_machine.json, cross-checked against each
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
  optionalAddress,
  readonly,
  seedFromPubkey,
  signer,
  writable,
  writableSigner,
} from "./anchor.js";
import { PROGRAM_ID, SYSTEM_PROGRAM, TOKEN_PROGRAM } from "../constants.js";
import {
  ABOVE_TICKET_SLOT,
  BELOW_TICKET_SLOTS,
  BLANK_SLOTS,
  MIN_ABOVE_TICKET_GAP,
  type BlankFaces,
} from "./decoders.js";

// ── Seeds (bye_machine-specific — anchor.ts stays program-agnostic) ─────────────────────────
// Verbatim against programs/bye_machine/src/common/seeds.rs.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const COLLECTION_SEED = "collection";
const PRINCIPAL_VAULT_SEED = "principal";
const TOP_TIER_SEED = "tier";
const TREASURY_03_SEED = "t03";
const TREASURY_04_SEED = "t04";

/** The Treasury 03 and Treasury 04 every pool shares, `PDA(["t03"])` and `PDA(["t04"])`. */
export async function deriveSharedTreasuries(
  programId: Address = PROGRAM_ID,
): Promise<{ treasury03: Address; treasury04: Address }> {
  const { address: treasury03 } = await derivePda([TREASURY_03_SEED], programId);
  const { address: treasury04 } = await derivePda([TREASURY_04_SEED], programId);
  return { treasury03, treasury04 };
}

/**
 * A pool's own Treasury 03 and Treasury 04, `PDA(["t03", pool])` and `PDA(["t04", pool])`, which a
 * pool created before the treasuries were shared holds until `consolidate_treasuries` closes them.
 */
export async function derivePoolTreasuries(
  pool: Address,
  programId: Address = PROGRAM_ID,
): Promise<{ treasury03: Address; treasury04: Address }> {
  const { address: treasury03 } = await derivePda([TREASURY_03_SEED, seedFromPubkey(pool)], programId);
  const { address: treasury04 } = await derivePda([TREASURY_04_SEED, seedFromPubkey(pool)], programId);
  return { treasury03, treasury04 };
}

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

// ── init_protocol ────────────────────────────────────────────────────────────────────────────
export type ProtocolConfigArgs = {
  operator: Address;
  t02Authority: Address;
  /** Treasury 05 is this key's USDC ATA; required, and refused at the default (6515). */
  t05Authority: Address;
  protocolRevenue: Address;
  usdcMint: Address;
  byeMint: Address | null;
  vrfProgram: Address;
  oracleQueue: Address;
  swapPool: Address | null;
  maxSwapSlippageBps: number;
};

function encodeProtocolConfigArgs(args: ProtocolConfigArgs): Buffer {
  return new Borsh()
    .pubkey(args.operator)
    .pubkey(args.t02Authority)
    .pubkey(args.t05Authority)
    .pubkey(args.protocolRevenue)
    .pubkey(args.usdcMint)
    .optionPubkey(args.byeMint)
    .pubkey(args.vrfProgram)
    .pubkey(args.oracleQueue)
    .optionPubkey(args.swapPool)
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
    // `swap_pool` is read-only and never signs: the handler checks its owner, discriminator,
    // mint pair and vaults, then records its address. It must be the same address `args.swapPool`
    // names — the program rejects the pair if they differ.
    accounts: [
      writableSigner(payer),
      writable(protocolConfig),
      readonly(optionalAddress(args.swapPool)),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── init_pool ─────────────────────────────────────────────────────────────────────────────────
export type PoolConfigArgs = {
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
};

/**
 * The program's blank-face and floor guard, which `init_pool` and `update_config` both apply to
 * the effective config: slots 0–2 each above 0 and below the ticket; slot 3 either 0 or at least
 * `MIN_ABOVE_TICKET_GAP` above the ticket; slots 4–7 zero; and a floor at or below the ticket
 * only with slot 3 set. Throws with the rule broken, so a script refuses before a transaction does.
 */
export function validateBlankFaces(
  ticketTarget: bigint,
  admissionFloor: bigint,
  blankFaces: readonly bigint[],
): void {
  if (blankFaces.length !== BLANK_SLOTS) {
    throw new Error(`expected ${BLANK_SLOTS} blank faces, got ${blankFaces.length}`);
  }
  for (const face of blankFaces.slice(0, BELOW_TICKET_SLOTS)) {
    if (face <= 0n) throw new Error("blank faces in slots 0-2 must be positive");
    if (face >= ticketTarget) throw new Error("blank faces in slots 0-2 must be below ticketTarget");
  }
  const aboveFace = blankFaces[ABOVE_TICKET_SLOT]!;
  if (aboveFace !== 0n && aboveFace < ticketTarget + MIN_ABOVE_TICKET_GAP) {
    throw new Error("the above-ticket blank face must be at least 1 USDC above ticketTarget");
  }
  if (blankFaces.slice(ABOVE_TICKET_SLOT + 1).some((face) => face !== 0n)) {
    throw new Error("blank-face slots 4-7 are reserved and must be 0");
  }
  if (admissionFloor <= ticketTarget && aboveFace === 0n) {
    throw new Error("an admissionFloor at or below ticketTarget needs an above-ticket blank face");
  }
}

function encodePoolConfigArgs(args: PoolConfigArgs): Buffer {
  return new Borsh()
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
    .arrayU64(args.blankFaces, BLANK_SLOTS)
    .u64(args.t04Ceiling)
    .u16(args.sweepCadenceHours)
    .u8(args.tierSize)
    .build();
}

/**
 * `poolId` is `protocol_config.pool_counter` at the moment of the call — the caller's
 * responsibility to read, since this encoder does no I/O. `weightIndex` must already be a
 * zero-initialised account of the exact `WeightIndex` layout size (weight-index.ts's job, not
 * this encoder's). No `rent` account, no `set_compute_unit_limit`.
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
  const { treasury03, treasury04 } = await deriveSharedTreasuries();

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
      writable(treasury03),
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
  | { kind: "BlankFaces"; value: BlankFaces }
  | { kind: "EscrowBidBps"; value: number };

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
      return new Borsh().enumVariant(10, new Borsh().arrayU64(update.value, BLANK_SLOTS).build()).build();
    case "EscrowBidBps":
      return new Borsh().enumVariant(11, new Borsh().u16(update.value).build()).build();
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

// ── sweep_t04_overflow ───────────────────────────────────────────────────────────────────────
/**
 * Restores the shared Treasury 04 to the protocol-wide ceiling by moving the excess to the shared
 * Treasury 03.
 *
 * **No amount or destination parameter, and that is the point.** The program reads Treasury 04's
 * live balance, subtracts the ceiling, and sends exactly the remainder to Treasury 03 — the
 * Operator signature triggers the transfer but chooses nothing about it.
 *
 * `poolIds` become the remaining accounts, read-only. The program refuses unless they are every
 * pool `protocol_config.pool_counter` counts, once each, none with a batch open — so the caller
 * passes `0..pool_counter`; any other set is accepted here only so a test can send it.
 */
export async function sweepT04Overflow(params: {
  operator: Address;
  poolIds: readonly number[];
}): Promise<Instruction> {
  const { operator, poolIds } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { treasury03, treasury04 } = await deriveSharedTreasuries();
  const pools = await Promise.all(
    poolIds.map(async (id) => (await derivePda([POOL_SEED, seedFromU16Le(id)])).address),
  );

  return buildAnchorIx({
    name: "sweep_t04_overflow",
    accounts: [
      signer(operator),
      readonly(protocolConfig),
      writable(treasury04),
      writable(treasury03),
      readonly(TOKEN_PROGRAM),
    ],
    remaining: pools.map(readonly),
  });
}

// ── set_t04_ceiling ──────────────────────────────────────────────────────────────────────────
/** Sets Treasury 04's protocol-wide ceiling, in USDC base units. Administrator-signed. */
export async function setT04Ceiling(params: {
  administrator: Address;
  ceiling: bigint;
}): Promise<Instruction> {
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  return buildAnchorIx({
    name: "set_t04_ceiling",
    args: new Borsh().u64(params.ceiling).build(),
    accounts: [signer(params.administrator), writable(protocolConfig)],
  });
}

// ── consolidate_treasuries ───────────────────────────────────────────────────────────────────
/**
 * Moves a pool created before the treasuries were shared onto the shared Treasury 03 and
 * Treasury 04: both balances move, both pool fields are rewritten, and the pool's own accounts
 * close with their rent to the Administrator, who also pays for a shared account still absent.
 * Once per pool; a second call fails on the closed accounts.
 */
export async function consolidateTreasuries(params: {
  administrator: Address;
  poolId: number;
  usdcMint: Address;
}): Promise<Instruction> {
  const { administrator, poolId, usdcMint } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const legacy = await derivePoolTreasuries(pool);
  const shared = await deriveSharedTreasuries();

  return buildAnchorIx({
    name: "consolidate_treasuries",
    accounts: [
      writableSigner(administrator),
      readonly(protocolConfig),
      writable(pool),
      writable(legacy.treasury03),
      writable(legacy.treasury04),
      writable(shared.treasury03),
      writable(shared.treasury04),
      readonly(usdcMint),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── migrate_pool ─────────────────────────────────────────────────────────────────────────────
/**
 * Grows a three-slot pool from 342 to 402 bytes, widening `blank_faces` and `blanks` to eight
 * slots in place with slots 3–7 zero, the Administrator paying the added rent. A pool already at
 * 402 bytes is left unchanged; a 276-byte pool is refused.
 */
export async function migratePool(params: {
  administrator: Address;
  poolId: number;
}): Promise<Instruction> {
  const { administrator, poolId } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);

  return buildAnchorIx({
    name: "migrate_pool",
    accounts: [
      writableSigner(administrator),
      readonly(protocolConfig),
      writable(pool),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── update_vrf_config ────────────────────────────────────────────────────────────────────────
/**
 * All four fields move together — `vrf_program`, `oracle_queue`, `swap_pool` and the slippage
 * bound spent on that venue are write-once at `init_protocol` otherwise, with no partial-update
 * form. The bound travels with the venue because it is spent on that venue's fee and price
 * impact; carried over to a differently-priced pool it is the wrong number.
 *
 * `swapPool` is passed twice on purpose: once as the account the program validates (owner, pool
 * discriminator, mint pair, vaults) and once as the argument it records. The program rejects the
 * pair if they differ.
 */
export async function updateVrfConfig(params: {
  administrator: Address;
  vrfProgram: Address;
  oracleQueue: Address;
  swapPool: Address;
  maxSwapSlippageBps: number;
}): Promise<Instruction> {
  const { administrator, vrfProgram, oracleQueue, swapPool, maxSwapSlippageBps } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);

  return buildAnchorIx({
    name: "update_vrf_config",
    args: new Borsh()
      .pubkey(vrfProgram)
      .pubkey(oracleQueue)
      .pubkey(swapPool)
      .u16(maxSwapSlippageBps)
      .build(),
    accounts: [signer(administrator), writable(protocolConfig), readonly(swapPool)],
  });
}

// ── update_bye_mint / update_protocol_revenue / update_swap_pool ────────────────────────────
export async function updateByeMint(params: {
  administrator: Address;
  byeMint: Address | null;
}): Promise<Instruction> {
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  return buildAnchorIx({
    name: "update_bye_mint",
    args: new Borsh().optionPubkey(params.byeMint).build(),
    accounts: [
      signer(params.administrator),
      writable(protocolConfig),
      readonly(optionalAddress(params.byeMint)),
    ],
  });
}

export async function updateProtocolRevenue(params: {
  administrator: Address;
  protocolRevenue: Address;
}): Promise<Instruction> {
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  return buildAnchorIx({
    name: "update_protocol_revenue",
    args: Buffer.alloc(0),
    accounts: [
      signer(params.administrator),
      writable(protocolConfig),
      readonly(params.protocolRevenue),
    ],
  });
}

export async function updateSwapPool(params: {
  administrator: Address;
  swapPool: Address | null;
}): Promise<Instruction> {
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  return buildAnchorIx({
    name: "update_swap_pool",
    args: new Borsh().optionPubkey(params.swapPool).build(),
    accounts: [
      signer(params.administrator),
      writable(protocolConfig),
      readonly(optionalAddress(params.swapPool)),
    ],
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
// `T05` is Treasury 05's delegated $BYE-purchase key, and it is rotatable like the other three.
// It sits at index 2 because that is the index Treasury 03's authority held before that role was
// retired: reusing the variant keeps `T02` at index 3 — the bytes a deployed cluster already emitted —
// where appending `T05` at index 4 would have left a dead variant at 2 for no gain.
export type AuthorityRole = "Administrator" | "Operator" | "T05" | "T02";

function authorityRoleIndex(role: AuthorityRole): number {
  switch (role) {
    case "Administrator":
      return 0;
    case "Operator":
      return 1;
    case "T05":
      return 2;
    case "T02":
      return 3;
  }
}

/** The Administrator replaces `role`'s live authority in one step. */
export async function setAuthorities(params: {
  signer: Address;
  role: AuthorityRole;
  newAuthority: Address;
}): Promise<Instruction> {
  const { signer: signerAddress, role, newAuthority } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);

  return buildAnchorIx({
    name: "set_authorities",
    args: new Borsh().enumVariant(authorityRoleIndex(role)).pubkey(newAuthority).build(),
    accounts: [signer(signerAddress), writable(protocolConfig)],
  });
}
