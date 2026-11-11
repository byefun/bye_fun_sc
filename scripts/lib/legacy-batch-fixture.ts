/**
 * A finished roll batch in the 1,100-byte layout `RollBatch` had with three blank slots, loaded by
 * the integration validator so the `close_batch` suite can close a batch written before the
 * layout grew. No instruction can create one any more: `commit_rolls` writes the current length,
 * and only this program can write `RollBatch`'s discriminator. Written by
 * `scripts/build-legacy-pool-fixtures.ts`.
 *
 * `close_batch` reads only the header and the last byte, and loads neither the pool nor the
 * roller, so both are fixed addresses no test creates; the roller's account is only compared, never credited.
 */
import { Buffer } from "node:buffer";

import { createKeyPairSignerFromPrivateKeyBytes, type Address, type KeyPairSigner } from "@solana/kit";

import { Borsh, accountDiscriminator, derivePda, seedFromPubkey, seedFromU64Le } from "./anchor";
import { LEGACY_ROLL_BATCH_LEN, LEGACY_ROLL_RECORD_LEN } from "./decoders";
import { LOCALNET_PROGRAM_ID } from "./legacy-pool-fixtures";

export const LEGACY_BATCH_ID = 7n;
/** A finished 1,100-byte batch whose roller holds a key, so the roller itself can close it. */
export const LEGACY_ROLLER_BATCH_ID = 8n;
/** A 1,100-byte batch still `Committed`, which no signer may close. */
export const LEGACY_COMMITTED_BATCH_ID = 9n;
/** Where the misplaced batch sits; its header names `LEGACY_ROLLER_BATCH_ID`, another PDA. */
export const MISPLACED_BATCH_ID = 10n;

/** `BatchState::Committed`'s and `BatchState::Complete`'s wire bytes. */
const COMMITTED = 0;
const COMPLETE = 3;
const RECORDS = 10;

/** `RollRecord` in the three-slot layout, 86 bytes. Roll 0 settled on a real card, drawn against
 * three-slot blank counts; rolls 1–9 never drew. Real records rather than filler, so the batch
 * also decodes through `decodeRollBatch`'s legacy branch. */
function legacyRecords(pool: Address): Buffer {
  const b = new Borsh();
  for (let i = 0; i < RECORDS; i += 1) {
    const drawn = i === 0;
    b.u8(drawn ? 1 : 0) // status: Settled / Pending
      .u8(drawn ? 1 : 0) // outcome: RealCard / None
      .pubkey(pool) // selected_mint — any fixed address
      .u8(0) // blank_face_idx
      .u128(drawn ? 4_029_087_020_998n : 0n) // draw_value
      .u8(drawn ? 1 : 0) // attempts
      .u128(drawn ? 7_533_333_333_320n : 0n) // w_at_draw
      .u32(drawn ? 40 : 0) // n_real_at_draw
      .arrayU32(drawn ? [3, 2, 1] : [0, 0, 0], 3) // blanks_at_draw
      .u16(0); // refund_reason
  }
  const records = b.build();
  if (records.length !== LEGACY_ROLL_RECORD_LEN * RECORDS) {
    throw new Error(`legacy records encode to ${records.length} bytes, expected ${LEGACY_ROLL_RECORD_LEN * RECORDS}`);
  }
  return records;
}

/** The three-slot layout, field for field. */
export function encodeLegacyBatch(params: {
  pool: Address;
  roller: Address;
  batchId: bigint;
  bump: number;
  state?: number;
}): Buffer {
  const body = new Borsh()
    .pubkey(params.pool)
    .pubkey(params.roller)
    .u64(params.batchId)
    .u8(params.state ?? COMPLETE)
    .u8(1) // n
    .u8(1) // next_roll
    .u8(1) // settled
    .u8(0) // refunded
    .u64(100n) // request_slot
    .fixed(Buffer.alloc(32, 0x11), 32) // caller_seed
    .fixed(Buffer.alloc(32, 0x22), 32) // randomness
    .u128(7_533_333_333_320n) // expected_w
    .arrayU32([3, 2, 1], 3) // expected_blanks
    .u64(9_900_000n) // s_price
    .u64(9_000_000n) // s_ticket
    .u64(900_000n) // s_fee
    .u16(7_500)
    .u16(500)
    .u16(2_000) // s_alloc_bps
    .arrayU64([1_000_000n, 2_000_000n, 5_000_000n], 3) // s_blank_faces
    .bytes(legacyRecords(params.pool)) // rolls
    .u8(params.bump)
    .build();
  const data = Buffer.concat([accountDiscriminator("RollBatch"), body]);
  if (data.length !== LEGACY_ROLL_BATCH_LEN) {
    throw new Error(`legacy batch encodes to ${data.length} bytes, expected ${LEGACY_ROLL_BATCH_LEN}`);
  }
  return data;
}

/** The legacy batch at its own PDA under the localnet program id, with its pool and roller. */
export async function legacyBatchAccount(): Promise<{
  address: Address;
  pool: Address;
  roller: Address;
  data: Buffer;
}> {
  const { address: pool } = await derivePda(["legacy_batch_pool"], LOCALNET_PROGRAM_ID);
  const { address: roller } = await derivePda(["legacy_batch_roller"], LOCALNET_PROGRAM_ID);
  const { address, bump } = await derivePda(
    ["batch", seedFromPubkey(pool), seedFromU64Le(LEGACY_BATCH_ID)],
    LOCALNET_PROGRAM_ID,
  );
  const data = encodeLegacyBatch({ pool, roller, batchId: LEGACY_BATCH_ID, bump });
  return { address, pool, roller, data };
}

/** The localnet-only key of the roller the keyed legacy batches name. */
export function legacyBatchRoller(): Promise<KeyPairSigner> {
  return createKeyPairSignerFromPrivateKeyBytes(new Uint8Array(32).fill(0x4c));
}

async function keyedLegacyBatch(params: {
  addressId: bigint;
  headerId: bigint;
  state: number;
}): Promise<{ address: Address; pool: Address; roller: Address; data: Buffer }> {
  const { address: pool } = await derivePda(["legacy_batch_pool"], LOCALNET_PROGRAM_ID);
  const { address: roller } = await legacyBatchRoller();
  const { address } = await derivePda(
    ["batch", seedFromPubkey(pool), seedFromU64Le(params.addressId)],
    LOCALNET_PROGRAM_ID,
  );
  const { bump } = await derivePda(
    ["batch", seedFromPubkey(pool), seedFromU64Le(params.headerId)],
    LOCALNET_PROGRAM_ID,
  );
  const data = encodeLegacyBatch({ pool, roller, batchId: params.headerId, bump, state: params.state });
  return { address, pool, roller, data };
}

/** A finished legacy batch at its own PDA, naming the keyed roller. */
export function legacyRollerBatchAccount() {
  return keyedLegacyBatch({
    addressId: LEGACY_ROLLER_BATCH_ID,
    headerId: LEGACY_ROLLER_BATCH_ID,
    state: COMPLETE,
  });
}

/** A legacy batch at its own PDA, still `Committed`. */
export function legacyCommittedBatchAccount() {
  return keyedLegacyBatch({
    addressId: LEGACY_COMMITTED_BATCH_ID,
    headerId: LEGACY_COMMITTED_BATCH_ID,
    state: COMMITTED,
  });
}

/** A finished legacy batch whose header names another batch's id and bump than the PDA it sits at. */
export function misplacedLegacyBatchAccount() {
  return keyedLegacyBatch({
    addressId: MISPLACED_BATCH_ID,
    headerId: LEGACY_ROLLER_BATCH_ID,
    state: COMPLETE,
  });
}
