// init_pool config profiles + the pool-instantiation helper that turns one into a live pool.
//
// Three profiles are not three pools. Four account types (`pool`, `top_tier`, `weight_index`,
// `principal_vault`) only have a sibling once a second pool exists on-chain — the
// account-substitution half of the signer matrix (pool B's `top_tier` rejected by a pool-A instruction) needs
// ≥ 2 live pools, not ≥ 2 argument sets; `treasury_03` and `treasury_04` are shared by every pool
// and have none. `createPool` below is what turns a
// profile into one, so a caller driving all three through it — at distinct `poolId`s — is what
// gives that sweep something real to test against.

import { address, type Address, type Connection, type TransactionSigner } from "./env.js";
import { admitCollection, initPool, type PoolConfigArgs } from "../../scripts/lib/admin.js";

export type { PoolConfigArgs };

// `initPool`'s own account order (scripts/lib/admin.ts) — read back rather than re-derived, so
// this file carries no PDA seed of its own to drift out of sync with it.
const INIT_POOL_ACCOUNT_INDEX = {
  pool: 2,
  topTier: 4,
  principalVault: 5,
  treasury03: 6,
  treasury04: 7,
} as const;

/**
 * Launch profile. `admissionCeiling`/`walletValueCap` both `0`, the
 * "no limit" encoding.
 */
export function launchProfile(): PoolConfigArgs {
  return {
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
}

/**
 * Limits-on profile — Launch with `admissionCeiling`/`walletValueCap` set, so the inclusive
 * `admission_floor ≤ value ≤ ceiling`/cap boundary is reachable *at* the ceiling/cap value, not
 * only below it.
 */
export function limitsOnProfile(): PoolConfigArgs {
  return {
    ...launchProfile(),
    admissionCeiling: 100_000_000n,
    walletValueCap: 300_000_000n,
  };
}

/**
 * Small-tier profile — `tierSize` shrunk to 3 so `TopTier` fill and demotion are reachable with
 * a handful of fixtures, not the Launch profile's 20+ deposits.
 */
export function smallTierProfile(): PoolConfigArgs {
  return { ...launchProfile(), tierSize: 3 };
}

/**
 * Gold — a 30 USDC floor under a 45 USDC ticket, accepted only because slot 3 carries a 100 USDC
 * above-ticket face. Kept out of `POOL_PROFILES`, whose enumeration is the three launch-era shapes.
 */
export function goldProfile(): PoolConfigArgs {
  return {
    ...launchProfile(),
    price: 49_500_000n,
    ticketTarget: 45_000_000n,
    fee: 4_500_000n,
    admissionFloor: 30_000_000n,
    admissionCeiling: 2_500_000_000n,
    blankFaces: [5_000_000n, 10_000_000n, 25_000_000n, 100_000_000n, 0n, 0n, 0n, 0n],
  };
}

/** Every profile, enumerable — so a coverage test can assert each one gets exercised. */
export const POOL_PROFILES = {
  launch: launchProfile,
  limitsOn: limitsOnProfile,
  smallTier: smallTierProfile,
} as const;

export type PoolProfileName = keyof typeof POOL_PROFILES;

/** The six accounts `init_pool` creates, addressed by name instead of ix position. */
export type PoolAccounts = {
  pool: Address;
  topTier: Address;
  principalVault: Address;
  treasury03: Address;
  treasury04: Address;
  /** The `init_pool` transaction's own signature — for a caller that needs to decode its
   * `PoolInitialized` event, read back from here rather
   * than re-deriving or re-sending. */
  signature: string;
};

/**
 * Sends `init_pool` for one profile and returns its derived accounts. `poolId` must be
 * `protocol_config.pool_counter` at call time — the caller's responsibility to track, since
 * neither this helper nor `initPool` reads on-chain state. `weightIndex` must already be a
 * zero-initialised account of the exact `WeightIndex` layout size (a separate helper's job, not
 * this one's).
 *
 * Calling this once per profile, at increasing `poolId`s, is what instantiates three *actual*
 * pools rather than three arg-builders exercised one at a time against a single pool.
 */
export async function createPool(params: {
  connection: Connection;
  administrator: TransactionSigner;
  poolId: number;
  weightIndex: Address;
  usdcMint: Address;
  args: PoolConfigArgs;
}): Promise<PoolAccounts> {
  const { connection, administrator, poolId, weightIndex, usdcMint, args } = params;
  const ix = await initPool({
    administrator: administrator.address,
    poolId,
    weightIndex,
    usdcMint,
    args,
  });
  const signature = await connection.sendTransactionFromInstructions({
    feePayer: administrator,
    instructions: [ix],
  });
  // `buildAnchorIx` (scripts/lib/anchor.ts) always populates `accounts`; the `?` on
  // `Instruction.accounts` is the generic @solana/kit type, not a real possibility here.
  const accounts = ix.accounts ?? [];
  return {
    pool: accounts[INIT_POOL_ACCOUNT_INDEX.pool].address,
    topTier: accounts[INIT_POOL_ACCOUNT_INDEX.topTier].address,
    principalVault: accounts[INIT_POOL_ACCOUNT_INDEX.principalVault].address,
    treasury03: accounts[INIT_POOL_ACCOUNT_INDEX.treasury03].address,
    treasury04: accounts[INIT_POOL_ACCOUNT_INDEX.treasury04].address,
    signature,
  };
}

/**
 * Admits one collection to a pool — the explicit step `init_pool` deliberately does not imply.
 *
 * A freshly initialised pool admits nothing and takes no deposit until this runs, so every
 * integration file that deposits must call it. That mirrors production, not a test convenience: the
 * admitted set is also the production gate on the Core deposit path, which ships by having no
 * record with the Core bit set rather than by a flag.
 *
 * `standards` defaults to pNFT only, which is what the intake resolves today.
 */
export async function admitPoolCollection(params: {
  connection: Connection;
  administrator: TransactionSigner;
  poolId: number;
  collection: Address;
  standards?: number;
}): Promise<string> {
  const { connection, administrator, poolId, collection, standards = 0b001 } = params;
  const ix = await admitCollection({
    administrator: administrator.address,
    poolId,
    collection,
    standards,
    reason: null,
  });
  return connection.sendTransactionFromInstructions({
    feePayer: administrator,
    instructions: [ix],
  });
}
