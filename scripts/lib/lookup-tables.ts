// Address Lookup Tables: what goes in them, and the Address Lookup Table program's own codec.
//
// A v0 transaction replaces a 32-byte account key with a 1-byte index into a lookup table, so the
// question this file answers is which of the program's account slots hold a key that is the *same*
// across transactions. Three answers — three *tiers*, differing in where the deployer reads the
// address from, not in which table it lands in:
//
//   cluster     — program ids, sysvars and the venue's global authority and event PDA.
//                 Compile-time constants in the program; identical on every cluster.
//   deployment  — the protocol singletons: `protocol_config`, the VRF program identity, the shared
//                 Treasury 03 and Treasury 04, and the addresses `protocol_config` records. One set
//                 per deployed program id.
//   pool        — the per-pool PDAs plus `weight_index`, which the `Pool` account records.
//                 Six keys per pool.
//
// **All three tiers go into ONE table, for every pool.** A transaction pays 32 bytes for the
// table's address plus one index byte per key *it* resolves, so entries it does not touch cost it
// nothing: the same `settle_roll` is the same size against a one-pool table and a many-pool one.
// Splitting the tiers would add 34 bytes and one loaded account per extra table reference and buy
// nothing back. At the designed four pools the single table holds 55 of its 256 slots.
//
// Entries are append-only: `extend` appends, nothing removes. A rotated `swap_pool` therefore
// leaves its predecessor behind — four dead slots against two hundred of headroom, which is why
// that is not a reason to split the table either.
//
// Nothing here weakens a check. A tabled key is resolved by the runtime before the program sees
// it, so every `address =` and `seeds =` constraint runs against exactly the same key it would
// have run against as a static one. A key that is missing from the table is passed static —
// costlier, never wrong.

import {
  AccountRole,
  getAddressEncoder,
  getBase58Decoder,
  getProgramDerivedAddress,
  type Address,
  type AccountMeta,
  type Instruction,
} from "@solana/kit";
import {
  ASSOCIATED_TOKEN_PROGRAM,
  AUTH_RULES_PROGRAM,
  CP_AMM_EVENT_AUTHORITY_SEED,
  CP_AMM_POOL_AUTHORITY,
  CP_AMM_PROGRAM,
  MPL_CORE_PROGRAM,
  SYSTEM_PROGRAM,
  SYSVAR_INSTRUCTIONS,
  SYSVAR_SLOT_HASHES,
  TOKEN_METADATA_PROGRAM,
  TOKEN_PROGRAM,
  VRF_PROGRAM,
} from "../constants.js";
import { fetchAccountData, type Connection } from "../_common.js";
import { deriveSharedTreasuries } from "./admin.js";
import { derivePda, seedFromPubkey } from "./anchor.js";

const addrEnc = getAddressEncoder();

// Seeds, spelled locally as every other builder in this directory spells them.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const TOP_TIER_SEED = "tier";
const PRINCIPAL_VAULT_SEED = "principal";
const TREASURY_02_SEED = "t02";
const WALLET_STATS_SEED = "wallet";
const VRF_IDENTITY_SEED = "identity";

// ── The Address Lookup Table program ─────────────────────────────────────────────────────────

export const ADDRESS_LOOKUP_TABLE_PROGRAM = "AddressLookupTab1e1111111111111111111111111" as Address;

/**
 * A table holds at most this many addresses; the account cannot be extended past it. The single
 * table is 31 keys plus six per pool — 55 at the designed four pools, and the cap is not a
 * constraint any realistic pool count approaches.
 */
export const MAX_ADDRESSES_PER_TABLE = 256;

/**
 * Addresses per `extend` transaction. A 1,232-byte envelope fits ~30 keys alongside the four
 * account metas and the blockhash; 25 leaves room for a priority-fee instruction.
 */
export const EXTEND_CHUNK_SIZE = 25;

/**
 * A table's own address is a PDA of the Address Lookup Table program over its authority and the
 * slot it was created against — which is why creation takes a slot argument at all, and why two
 * tables of the same authority need two different slots.
 */
export async function deriveLookupTableAddress(
  authority: Address,
  recentSlot: bigint,
): Promise<{ address: Address; bump: number }> {
  const slotLe = Buffer.alloc(8);
  slotLe.writeBigUInt64LE(recentSlot);
  const [pda, bump] = await getProgramDerivedAddress({
    programAddress: ADDRESS_LOOKUP_TABLE_PROGRAM,
    seeds: [seedFromPubkey(authority), new Uint8Array(slotLe)],
  });
  return { address: pda, bump };
}

// The program's instruction enum is bincode-encoded, not Borsh: the variant tag is a u32 and a
// `Vec` carries a u64 length prefix. Borsh would write a u8 tag and a u32 length, and the program
// would reject every instruction this file builds.
function bincodeTag(tag: number): Buffer {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(tag);
  return b;
}

const writable = (a: Address): AccountMeta => ({ address: a, role: AccountRole.WRITABLE });
const readonly = (a: Address): AccountMeta => ({ address: a, role: AccountRole.READONLY });
const signer = (a: Address): AccountMeta => ({ address: a, role: AccountRole.READONLY_SIGNER });
const writableSigner = (a: Address): AccountMeta => ({
  address: a,
  role: AccountRole.WRITABLE_SIGNER,
});

export function buildCreateLookupTableInstruction(params: {
  lookupTable: Address;
  authority: Address;
  payer: Address;
  recentSlot: bigint;
  bump: number;
}): Instruction {
  const { lookupTable, authority, payer, recentSlot, bump } = params;
  const slot = Buffer.alloc(8);
  slot.writeBigUInt64LE(recentSlot);
  return {
    programAddress: ADDRESS_LOOKUP_TABLE_PROGRAM,
    accounts: [
      writable(lookupTable),
      signer(authority),
      writableSigner(payer),
      readonly(SYSTEM_PROGRAM),
    ],
    data: new Uint8Array(Buffer.concat([bincodeTag(0), slot, Buffer.from([bump])])),
  };
}

export function buildExtendLookupTableInstruction(params: {
  lookupTable: Address;
  authority: Address;
  payer: Address;
  addresses: readonly Address[];
}): Instruction {
  const { lookupTable, authority, payer, addresses } = params;
  if (addresses.length === 0) throw new Error("extend with no addresses");
  const len = Buffer.alloc(8);
  len.writeBigUInt64LE(BigInt(addresses.length));
  const keys = addresses.map((a) => Buffer.from(addrEnc.encode(a)));
  return {
    programAddress: ADDRESS_LOOKUP_TABLE_PROGRAM,
    accounts: [
      writable(lookupTable),
      signer(authority),
      writableSigner(payer),
      readonly(SYSTEM_PROGRAM),
    ],
    data: new Uint8Array(Buffer.concat([bincodeTag(2), len, ...keys])),
  };
}

/**
 * Starts the cooldown. The table keeps resolving for transactions already in flight and stops
 * being usable once the cooldown elapses; only then does `close` succeed.
 */
export function buildDeactivateLookupTableInstruction(params: {
  lookupTable: Address;
  authority: Address;
}): Instruction {
  return {
    programAddress: ADDRESS_LOOKUP_TABLE_PROGRAM,
    accounts: [writable(params.lookupTable), signer(params.authority)],
    data: new Uint8Array(bincodeTag(3)),
  };
}

export function buildCloseLookupTableInstruction(params: {
  lookupTable: Address;
  authority: Address;
  recipient: Address;
}): Instruction {
  return {
    programAddress: ADDRESS_LOOKUP_TABLE_PROGRAM,
    accounts: [
      writable(params.lookupTable),
      signer(params.authority),
      writable(params.recipient),
    ],
    data: new Uint8Array(bincodeTag(4)),
  };
}

/** Split `addresses` into `extend`-sized chunks, preserving order. */
export function chunkAddresses(
  addresses: readonly Address[],
  size: number = EXTEND_CHUNK_SIZE,
): Address[][] {
  if (size < 1) throw new Error(`chunk size must be >= 1, got ${size}`);
  const out: Address[][] = [];
  for (let i = 0; i < addresses.length; i += size) out.push([...addresses.slice(i, i + size)]);
  return out;
}

// ── Which tier an account slot belongs to ────────────────────────────────────────────────────

export type AltTier = "cluster" | "deployment" | "pool" | "dynamic";

/**
 * Every account name the IDL declares, and the tier the deployer resolves its key from. `dynamic`
 * means the key varies per caller, position, batch or collection and the table cannot hold it —
 * signers are always `dynamic`, because a signature index addresses the static keys and a tabled
 * signer is unrepresentable rather than merely wasteful.
 *
 * This map is exhaustive by test: an account name the IDL declares and this map does not classify
 * is a failure, not a default. A new instruction therefore cannot quietly inherit `dynamic`.
 */
export const ACCOUNT_TIER: Readonly<Record<string, AltTier>> = {
  // Signers — the fee payer and every authority.
  operator: "dynamic",
  administrator: "dynamic",
  payer: "dynamic",
  seller: "dynamic",
  t02_authority: "dynamic",
  vrf_program_identity: "dynamic",
  signer: "dynamic",
  // An escrow's owner, signing every escrow instruction but the fee claim.
  owner: "dynamic",

  // `depositor` and `roller` sign some instructions and are a plain destination in others; both
  // readings are per-user, so the classification does not depend on which instruction is meant.
  depositor: "dynamic",
  roller: "dynamic",

  // Cluster constants — compile-time `address =` pins in the program, plus the venue's event
  // authority, which is a PDA of a fixed seed under a fixed program id.
  system_program: "cluster",
  token_program: "cluster",
  associated_token_program: "cluster",
  token_metadata_program: "cluster",
  mpl_core_program: "cluster",
  authorization_rules_program: "cluster",
  sysvar_instructions: "cluster",
  slot_hashes: "cluster",
  vrf_program: "cluster",
  cp_amm_program: "cluster",
  cp_amm_pool_authority: "cluster",
  cp_amm_event_authority: "cluster",

  // Deployment constants — the config PDA, the VRF identity PDA, and the addresses
  // `protocol_config` records.
  protocol_config: "deployment",
  program_identity: "deployment",
  protocol_revenue: "deployment",
  // Treasuries 03, 04 and 05 are protocol-wide, one account each for every pool — deployment
  // rather than pool, the same side `protocol_revenue` sits on.
  treasury_03: "deployment",
  treasury_04: "deployment",
  treasury_05: "deployment",
  usdc_mint: "deployment",
  bye_mint: "deployment",
  oracle_queue: "deployment",
  swap_pool: "deployment",
  swap_token_a_vault: "deployment",
  swap_token_b_vault: "deployment",

  // Per-pool — PDAs seeded by the pool, plus the `weight_index` address the `Pool` records.
  pool: "pool",
  top_tier: "pool",
  principal_vault: "pool",
  weight_index: "pool",
  t02_identity: "pool",

  // Per-position, per-batch, per-collection or per-user. A pool's own Treasury 03 and Treasury 04
  // sit here too: `consolidate_treasuries` names each once, then closes it.
  legacy_treasury_03: "dynamic",
  legacy_treasury_04: "dynamic",
  position: "dynamic",
  position_vault: "dynamic",
  wallet_stats: "dynamic",
  batch: "dynamic",
  pool_collection: "dynamic",
  collection: "dynamic",
  asset: "dynamic",
  nft_mint: "dynamic",
  metadata: "dynamic",
  master_edition: "dynamic",
  authorization_rules: "dynamic",
  position_token_record: "dynamic",
  depositor_token: "dynamic",
  depositor_token_record: "dynamic",
  depositor_usdc: "dynamic",
  roller_usdc: "dynamic",
  roller_token: "dynamic",
  roller_token_record: "dynamic",
  roller_bye: "dynamic",
  authority_token: "dynamic",
  authority_token_record: "dynamic",
  seller_token: "dynamic",
  seller_token_record: "dynamic",
  seller_usdc: "dynamic",
  settlement_usdc: "dynamic",
  // A wallet's escrow, its vault and the owner's USDC account: per-user.
  escrow: "dynamic",
  escrow_vault: "dynamic",
  owner_usdc: "dynamic",
  // The loader a sale may pay from: per-card, named from the card's latest draw.
  loader_escrow: "dynamic",
  loader_escrow_vault: "dynamic",
  loader_wallet_stats: "dynamic",
};

/**
 * The slots where the name-keyed tier above is wrong for one instruction, keyed
 * `<instruction>.<account>`.
 *
 * Two causes, and both are about *when* rather than *what*. The bootstrap instructions create the
 * very accounts a table would hold, so at the moment they run the table does not exist yet — their
 * slots are static however constant the key becomes afterwards. And `wallet_stats` is the
 * depositor's in every instruction but the two instant sells, where it is the pool's own
 * Treasury-02 identity and therefore a per-pool constant.
 */
export const ACCOUNT_TIER_OVERRIDES: Readonly<Record<string, AltTier>> = {
  // `init_protocol` writes `protocol_config` and the venue it pins; the table is built from that
  // account's contents, so it cannot precede it.
  "init_protocol.protocol_config": "dynamic",
  "init_protocol.swap_pool": "dynamic",

  // `init_pool` creates the pool and everything seeded by it; its keys are appended after, not
  // before.
  "init_pool.pool": "dynamic",
  "init_pool.weight_index": "dynamic",
  "init_pool.top_tier": "dynamic",
  "init_pool.principal_vault": "dynamic",
  "init_pool.treasury_04": "dynamic",

  // The seller is paid by the pool's Treasury-02 identity, whose stats account is per-pool.
  "instant_sell.wallet_stats": "pool",
  "instant_sell_core.wallet_stats": "pool",
};

/**
 * The tier this slot's key is resolved from, or `undefined` if the account name is unclassified.
 * Pass `instructionName` wherever one is known — omitting it silently skips the overrides above.
 */
export function tierOf(accountName: string, instructionName?: string): AltTier | undefined {
  if (instructionName !== undefined) {
    const override = ACCOUNT_TIER_OVERRIDES[`${instructionName}.${accountName}`];
    if (override !== undefined) return override;
  }
  return ACCOUNT_TIER[accountName];
}

// ── Resolving each tier's addresses ──────────────────────────────────────────────────────────

/**
 * The `deposit`/`deposit_core` instructions' fixed program and sysvar accounts — the part of the
 * cluster tier that shrinks every deposit transaction regardless of which pool or NFT is deposited.
 * Unlike the rest of `resolveClusterTier`, none of these are derived and none depend on
 * `protocol_config` or a pool existing, so this can go into the table standalone, before either is
 * deployed. `protocol_config` and pool addresses join later via
 * `resolveDeploymentTier`/`resolvePoolTier` once they exist on the cluster.
 */
export function labelDepositCommonTier(): LabelledAddress[] {
  return [
    ["system_program", SYSTEM_PROGRAM],
    ["token_program", TOKEN_PROGRAM],
    ["associated_token_program", ASSOCIATED_TOKEN_PROGRAM],
    ["token_metadata_program", TOKEN_METADATA_PROGRAM],
    ["mpl_core_program", MPL_CORE_PROGRAM],
    ["auth_rules_program", AUTH_RULES_PROGRAM],
    ["sysvar_instructions", SYSVAR_INSTRUCTIONS],
  ];
}

export function resolveDepositCommonTier(): Address[] {
  return addressesOf(labelDepositCommonTier());
}

/**
 * The cluster tier's addresses, in a fixed order: `resolveDepositCommonTier`'s addresses plus the
 * venue and VRF constants. `cp_amm_event_authority` is derived rather than listed because the
 * venue's event PDA is a seed under its program id, not a published constant.
 */
export async function labelClusterTier(): Promise<LabelledAddress[]> {
  const { address: cpAmmEventAuthority } = await derivePda(
    [CP_AMM_EVENT_AUTHORITY_SEED],
    CP_AMM_PROGRAM,
  );
  return [
    ...labelDepositCommonTier(),
    ["sysvar_slot_hashes", SYSVAR_SLOT_HASHES],
    ["vrf_program", VRF_PROGRAM],
    ["cp_amm_program", CP_AMM_PROGRAM],
    ["cp_amm_pool_authority", CP_AMM_POOL_AUTHORITY],
    ["cp_amm_event_authority", cpAmmEventAuthority],
  ];
}

export async function resolveClusterTier(): Promise<Address[]> {
  return addressesOf(await labelClusterTier());
}

/**
 * The deployment tier's addresses. `swapTokenAVault`/`swapTokenBVault` are the venue pool's own
 * vaults, which the caller reads off the venue account — they are passed in rather than derived
 * here, because their layout belongs to the venue, not to this program.
 */
export async function labelDeploymentTier(params: DeploymentTierParams): Promise<LabelledAddress[]> {
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: programIdentity } = await derivePda([VRF_IDENTITY_SEED]);
  const { treasury03, treasury04 } = await deriveSharedTreasuries();
  const optional: [string, Address | null][] = [
    ["bye_mint", params.byeMint],
    ["swap_pool", params.swapPool],
    ["swap_token_a_vault", params.swapTokenAVault],
    ["swap_token_b_vault", params.swapTokenBVault],
  ];
  return [
    ["protocol_config", protocolConfig],
    ["program_identity", programIdentity],
    ["treasury_03", treasury03],
    ["treasury_04", treasury04],
    ["protocol_revenue", params.protocolRevenue],
    ["treasury_05", params.treasury05],
    ["usdc_mint", params.usdcMint],
    ["oracle_queue", params.oracleQueue],
    ...optional.filter((entry): entry is LabelledAddress => entry[1] !== null),
  ];
}

export async function resolveDeploymentTier(params: DeploymentTierParams): Promise<Address[]> {
  return addressesOf(await labelDeploymentTier(params));
}

export interface DeploymentTierParams {
  protocolRevenue: Address;
  /** Treasury 05 — the USDC ATA of `protocol_config.t05_authority`, credited by every
   * settlement in every pool, so it is one deployment-tier key rather than a per-pool
   * one. Passed rather than derived: the authority it hangs off is config this module does not
   * read, exactly as `protocolRevenue` is. */
  treasury05: Address;
  usdcMint: Address;
  byeMint: Address | null;
  oracleQueue: Address;
  swapPool: Address | null;
  swapTokenAVault: Address | null;
  swapTokenBVault: Address | null;
}

/**
 * One pool's six addresses. `weightIndex` alone is read off the `Pool` account rather than
 * derived: it is a keypair-created account, so it has no seeds to derive from.
 */
export async function resolvePoolTier(params: PoolTierParams): Promise<Address[]> {
  return addressesOf(await labelPoolTier(params));
}

export interface PoolTierParams {
  poolId: number;
  weightIndex: Address;
}

export async function labelPoolTier(params: PoolTierParams): Promise<LabelledAddress[]> {
  const poolIdLe = Buffer.alloc(2);
  poolIdLe.writeUInt16LE(params.poolId);
  const { address: pool } = await derivePda([POOL_SEED, new Uint8Array(poolIdLe)]);
  const poolSeed = seedFromPubkey(pool);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, poolSeed]);
  const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, poolSeed]);
  const { address: t02Identity } = await derivePda([TREASURY_02_SEED, poolSeed]);
  const { address: t02WalletStats } = await derivePda([
    WALLET_STATS_SEED,
    poolSeed,
    seedFromPubkey(t02Identity),
  ]);
  const at = `pool ${params.poolId}`;
  return [
    [`${at} pool`, pool],
    [`${at} top_tier`, topTier],
    [`${at} principal_vault`, principalVault],
    [`${at} t02_identity`, t02Identity],
    [`${at} t02_wallet_stats`, t02WalletStats],
    [`${at} weight_index`, params.weightIndex],
  ];
}

/** An address with the account name it is prepared under. */
export type LabelledAddress = [label: string, address: Address];

export function addressesOf(labelled: readonly LabelledAddress[]): Address[] {
  return labelled.map(([, a]) => a);
}

/**
 * The addresses still missing from `onChain`, in `wanted` order. Re-running a sync against a table
 * that already holds everything returns an empty list, which is what makes the deployer idempotent.
 */
export function missingAddresses(
  wanted: readonly Address[],
  onChain: readonly Address[],
): Address[] {
  const have = new Set(onChain);
  const out: Address[] = [];
  for (const a of wanted) {
    if (!have.has(a) && !out.includes(a)) out.push(a);
  }
  return out;
}

// ── Reading a table back ─────────────────────────────────────────────────────────────────────

/**
 * Fixed prefix ahead of the address list: a u32 account discriminator, the deactivation slot and
 * last-extended slot as u64s, the last-extended start index as a u8, an optional authority
 * (1 presence byte + 32), and two padding bytes.
 */
export const LOOKUP_TABLE_META_SIZE = 56;

/** A never-deactivated table records this slot, which is `u64::MAX`. */
export const LOOKUP_TABLE_NOT_DEACTIVATED = 0xffff_ffff_ffff_ffffn;

export interface LookupTableAccount {
  deactivationSlot: bigint;
  lastExtendedSlot: bigint;
  /**
   * `null` once frozen. Nothing here builds a freeze: a new pool has to be able to extend this
   * table, so freezing it is never right — but a table frozen out of band still has to be read
   * back and refused rather than silently treated as extendable.
   */
  authority: Address | null;
  addresses: Address[];
}

export function decodeLookupTable(data: Buffer | Uint8Array): LookupTableAccount {
  const buf = Buffer.from(data);
  if (buf.length < LOOKUP_TABLE_META_SIZE) {
    throw new Error(`lookup table account is ${buf.length} bytes, under the ${LOOKUP_TABLE_META_SIZE}-byte header`);
  }
  const body = buf.subarray(LOOKUP_TABLE_META_SIZE);
  if (body.length % 32 !== 0) {
    throw new Error(`lookup table address list is ${body.length} bytes, not a multiple of 32`);
  }
  const hasAuthority = buf.readUInt8(21) === 1;
  const addresses: Address[] = [];
  for (let i = 0; i < body.length; i += 32) {
    addresses.push(addressFromBytes(body.subarray(i, i + 32)));
  }
  return {
    deactivationSlot: buf.readBigUInt64LE(4),
    lastExtendedSlot: buf.readBigUInt64LE(12),
    authority: hasAuthority ? addressFromBytes(buf.subarray(22, 54)) : null,
    addresses,
  };
}

function addressFromBytes(bytes: Uint8Array): Address {
  return getBase58Decoder().decode(bytes) as Address;
}

/**
 * Read the published table off chain, keyed the way `sendV0Ixs` wants it. A published address
 * whose account is missing yields an empty set rather than failing: no table yet means an
 * uncompressed transaction, not a broken caller.
 */
export async function fetchLookupTable(
  connection: Connection,
  table: Address | null,
): Promise<Record<Address, Address[]>> {
  if (table === null) return {};
  const data = await fetchAccountData(connection, table);
  if (data === null) return {};
  return { [table]: decodeLookupTable(data).addresses };
}
