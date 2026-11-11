/**
 * The pools the integration validator loads in states no instruction can produce any more. Two
 * sit at lengths `Pool` no longer has: the 342-byte layout with three blank slots, which
 * `migrate_pool` grows, and the 276-byte layout from before `escrow_bid_bps` and `reserved` were
 * appended, which it refuses. Two more are current-layout pools still holding their own Treasury
 * 03 and Treasury 04, with those token accounts, which `consolidate_treasuries` moves onto the
 * shared ones; one has a batch open. `init_pool` writes neither, and only this program can write
 * `Pool`'s discriminator. Shared by `scripts/build-legacy-pool-fixtures.ts`, which writes them,
 * and the suites that check what was done to them.
 *
 * The pool ids sit far above anything `protocol_config.pool_counter` reaches in a test run, so
 * no pool the suite creates can land on either address.
 */
import { Buffer } from "node:buffer";

import { getAddressDecoder, type Address } from "@solana/kit";

import { TOKEN_PROGRAM } from "../constants";
import { Borsh, accountDiscriminator, derivePda, seedFromPubkey } from "./anchor";
import { LEGACY_POOL_LEN, POOL_LEN, encodePool, type Pool } from "./decoders";
import { USDC_MINT_FIXTURE } from "./swap-fixtures";

/** `accounts/dev/bye_machine-keypair.json`, the program id the integration validator deploys. */
export const LOCALNET_PROGRAM_ID = "szKvUQJx6USMBe6J2yNrd16jEAF98Ys82GdeBP93yqG" as Address;

/** A pool in the three-slot layout: 342 bytes, discriminator included. */
export const LEGACY_POOL_ID = 65_000;
/** A `Pool`-tagged account at the pre-escrow length, 276 bytes, which the migration refuses. */
export const MALFORMED_POOL_ID = 65_001;
/** A current-layout pool still holding its own Treasury 03 and Treasury 04, no batch open. */
export const UNCONSOLIDATED_POOL_ID = 65_002;
/** The same, with a batch open, which consolidation refuses. */
export const BUSY_UNCONSOLIDATED_POOL_ID = 65_003;
/** The same, no batch open, but naming a Treasury 03 other than its own, which consolidation refuses. */
export const MISNAMED_UNCONSOLIDATED_POOL_ID = 65_004;
/** What each unconsolidated pool's own Treasury 03 and Treasury 04 hold, in USDC base units. */
export const UNCONSOLIDATED_T03_AMOUNT = 7_000_000n;
export const UNCONSOLIDATED_T04_AMOUNT = 9_000_000n;
/** An SPL Token account's length. */
export const TOKEN_ACCOUNT_LEN = 165;

export { LEGACY_POOL_LEN, POOL_LEN };
export const MALFORMED_POOL_LEN = 276;

/** Every field the three-slot layout carries, each a value no zero-fill or neighbour could produce.
 * Its 64 reserved bytes are where today's `treasury_04` and 32 reserved bytes sit. */
export type LegacyPoolFields = Omit<Pool, "blankFaces" | "blanks" | "treasury04"> & {
  blankFaces: [bigint, bigint, bigint];
  blanks: [number, number, number];
};

/** What the migration must read back: every field unchanged, face and count slots 3–7 zero, and
 * the old reserved bytes read as `treasury_04` and today's reserved bytes. */
export function grownPoolFields(fields: LegacyPoolFields): Pool {
  const [f0, f1, f2] = fields.blankFaces;
  const [b0, b1, b2] = fields.blanks;
  return {
    ...fields,
    blankFaces: [f0, f1, f2, 0n, 0n, 0n, 0n, 0n],
    blanks: [b0, b1, b2, 0, 0, 0, 0, 0],
    treasury04: getAddressDecoder().decode(fields.reserved.subarray(0, 32)),
    reserved: fields.reserved.slice(32),
  };
}

export function legacyPoolFields(params: {
  poolId: number;
  bump: number;
  weightIndex: Pool["weightIndex"];
  treasury03: Pool["treasury03"];
}): LegacyPoolFields {
  return {
    poolId: params.poolId,
    weightIndex: params.weightIndex,
    treasury03: params.treasury03,
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
    blankFaces: [1_000_000n, 2_000_000n, 5_000_000n],
    t04Ceiling: 2_000_000_000n,
    sweepCadenceHours: 24,
    tierSize: 20,
    depositsPaused: true,
    rollsPaused: true,
    pauseReason: 7,
    nReal: 41,
    blanks: [3, 2, 1],
    wReal: 123_456_789_012_345n,
    openBatches: 0,
    batchCounter: 99n,
    positionCounter: 1_234n,
    pendingRollLiability: 0n,
    owedFees: 1_000n,
    accEqual: 987_654_321_098n,
    sweepPending: false,
    sweepEpoch: 8n,
    lastSweepAt: 1_790_000_000n,
    sweepUpdates: 6,
    bump: params.bump,
    escrowBidBps: 9_250,
    reserved: Uint8Array.from({ length: 64 }, (_, i) => 0xc0 ^ i),
  };
}

/** The account bytes of `fields` at the old length, discriminator included. */
export function encodeLegacyPool(fields: LegacyPoolFields): Buffer {
  const body = new Borsh()
    .u16(fields.poolId)
    .pubkey(fields.weightIndex)
    .pubkey(fields.treasury03)
    .u64(fields.price)
    .u64(fields.ticketTarget)
    .u64(fields.fee)
    .u16(fields.allocEqualBps)
    .u16(fields.allocTierBps)
    .u16(fields.allocProtocolBps)
    .u64(fields.admissionFloor)
    .u64(fields.admissionCeiling)
    .u64(fields.walletValueCap)
    .u8(fields.maxN)
    .u16(fields.buybackRateBps)
    .arrayU64(fields.blankFaces, 3)
    .u64(fields.t04Ceiling)
    .u16(fields.sweepCadenceHours)
    .u8(fields.tierSize)
    .bool(fields.depositsPaused)
    .bool(fields.rollsPaused)
    .u16(fields.pauseReason)
    .u32(fields.nReal)
    .arrayU32(fields.blanks, 3)
    .u128(fields.wReal)
    .u32(fields.openBatches)
    .u64(fields.batchCounter)
    .u64(fields.positionCounter)
    .u64(fields.pendingRollLiability)
    .u64(fields.owedFees)
    .u128(fields.accEqual)
    .bool(fields.sweepPending)
    .u64(fields.sweepEpoch)
    .i64(fields.lastSweepAt)
    .u32(fields.sweepUpdates)
    .u8(fields.bump)
    .u16(fields.escrowBidBps)
    .bytes(fields.reserved)
    .build();
  const data = Buffer.concat([accountDiscriminator("Pool"), body]);
  if (data.length !== LEGACY_POOL_LEN) {
    throw new Error(`legacy pool encodes to ${data.length} bytes, expected ${LEGACY_POOL_LEN}`);
  }
  return data;
}

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

/** Rent-exempt minimum under the validator's default rent: `(128 + len) * 3480 * 2` lamports. */
export function rentExempt(len: number): number {
  return (128 + len) * 3_480 * 2;
}

/**
 * The old-length pool at `poolId`'s address under the localnet program id, its fields from
 * `legacyPoolFields`. `weight_index` and `treasury_03` are arbitrary but fixed addresses: the
 * migration reads neither, and the suite compares them byte for byte.
 */
export async function legacyPoolAccount(
  poolId: number,
): Promise<{ address: Address; fields: LegacyPoolFields; data: Buffer }> {
  const { address, bump } = await derivePda(["pool", seedFromU16Le(poolId)], LOCALNET_PROGRAM_ID);
  const { address: weightIndex } = await derivePda(
    ["legacy_weight_index", seedFromU16Le(poolId)],
    LOCALNET_PROGRAM_ID,
  );
  const { address: treasury03 } = await derivePda(
    ["legacy_treasury_03", seedFromU16Le(poolId)],
    LOCALNET_PROGRAM_ID,
  );
  const fields = legacyPoolFields({ poolId, bump, weightIndex, treasury03 });
  return { address, fields, data: encodeLegacyPool(fields) };
}

/**
 * The refused account: the three-slot pool at `MALFORMED_POOL_ID` cut to its first 276 bytes,
 * which is exactly the pre-escrow layout.
 */
export async function malformedPoolAccount(): Promise<{ address: Address; data: Buffer }> {
  const { address, data } = await legacyPoolAccount(MALFORMED_POOL_ID);
  return { address, data: Buffer.from(data.subarray(0, MALFORMED_POOL_LEN)) };
}

/**
 * An initialized SPL Token account: `mint`, `owner` (its authority) and `amount`, with no
 * delegate, not native, and no close authority.
 */
export function encodeTokenAccount(params: { mint: Address; owner: Address; amount: bigint }): Buffer {
  const data = new Borsh()
    .pubkey(params.mint)
    .pubkey(params.owner)
    .u64(params.amount)
    .fixed(Buffer.alloc(36), 36) // delegate: COption<Pubkey>, None
    .u8(1) // state: Initialized
    .fixed(Buffer.alloc(12), 12) // is_native: COption<u64>, None
    .u64(0n) // delegated_amount
    .fixed(Buffer.alloc(36), 36) // close_authority: COption<Pubkey>, None
    .build();
  if (data.length !== TOKEN_ACCOUNT_LEN) {
    throw new Error(`token account encodes to ${data.length} bytes, expected ${TOKEN_ACCOUNT_LEN}`);
  }
  return data;
}

export type FixtureAccount = { address: Address; data: Buffer; owner: Address };

/**
 * A current-layout pool at `poolId` that still names its own Treasury 03 and has no Treasury 04
 * recorded, as every pool created before the treasuries were shared reads once `migrate_pool` has
 * grown it, plus its two per-pool token accounts, each authorised by the pool and holding
 * `UNCONSOLIDATED_T03_AMOUNT` / `UNCONSOLIDATED_T04_AMOUNT` of the fixture USDC.
 */
export async function unconsolidatedPoolAccounts(
  poolId: number,
  openBatches: number,
  namesOwnTreasury03 = true,
): Promise<{ pool: FixtureAccount; treasury03: FixtureAccount; treasury04: FixtureAccount; fields: Pool }> {
  const { address, bump } = await derivePda(["pool", seedFromU16Le(poolId)], LOCALNET_PROGRAM_ID);
  const { address: weightIndex } = await derivePda(
    ["legacy_weight_index", seedFromU16Le(poolId)],
    LOCALNET_PROGRAM_ID,
  );
  const { address: treasury03 } = await derivePda(["t03", seedFromPubkey(address)], LOCALNET_PROGRAM_ID);
  const { address: treasury04 } = await derivePda(["t04", seedFromPubkey(address)], LOCALNET_PROGRAM_ID);
  const named = namesOwnTreasury03
    ? treasury03
    : (await derivePda(["legacy_treasury_03", seedFromU16Le(poolId)], LOCALNET_PROGRAM_ID)).address;
  const fields: Pool = {
    ...grownPoolFields(legacyPoolFields({ poolId, bump, weightIndex, treasury03: named })),
    depositsPaused: false,
    rollsPaused: false,
    pauseReason: 0,
    openBatches,
    batchCounter: BigInt(openBatches),
    treasury04: "11111111111111111111111111111111" as Address,
    reserved: new Uint8Array(32),
  };
  const token = (account: Address, amount: bigint): FixtureAccount => ({
    address: account,
    data: encodeTokenAccount({ mint: USDC_MINT_FIXTURE, owner: address, amount }),
    owner: TOKEN_PROGRAM,
  });
  return {
    pool: { address, data: encodePool(fields), owner: LOCALNET_PROGRAM_ID },
    treasury03: token(treasury03, UNCONSOLIDATED_T03_AMOUNT),
    treasury04: token(treasury04, UNCONSOLIDATED_T04_AMOUNT),
    fields,
  };
}
