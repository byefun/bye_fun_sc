// Instruction encoders for the two round-2 roll-pipeline instructions committed so far:
// commit_rolls, vrf_callback. Account order and arg shapes are derived mechanically from
// target/idl/bye_machine.json, cross-checked against each instruction's #[derive(Accounts)]
// struct in programs/bye_machine/src/instructions/roll/.
//
// Pure and synchronous with respect to I/O, same discipline as the other domain files: every
// state-dependent account — the pool's `batch_counter` at commit time, the batch's own `pool`
// and `batch_id` at callback time, `protocol_config.oracle_queue`, the roller's own USDC token
// account — is a required, non-defaulted parameter. These encoders derive this program's own
// PDAs but never read on-chain state themselves.

import type { Address, Instruction } from "@solana/kit";
import {
  Borsh,
  buildAnchorIx,
  derivePda,
  readonly,
  seedFromPubkey,
  seedFromU64Le,
  signer,
  writable,
  writableSigner,
} from "./anchor.js";
import {
  PROGRAM_ID,
  SYSTEM_PROGRAM,
  SYSVAR_SLOT_HASHES,
  TOKEN_PROGRAM,
  VRF_PROGRAM,
} from "../constants.js";

// ── Seeds (bye_machine-specific — anchor.ts stays program-agnostic) ─────────────────────────
// Verbatim against programs/bye_machine/src/common/seeds.rs.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const TOP_TIER_SEED = "tier";
const BATCH_SEED = "batch";
const PRINCIPAL_VAULT_SEED = "principal";
const VRF_IDENTITY_SEED = "identity";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

// ── commit_rolls ─────────────────────────────────────────────────────────────────────────────
/**
 * One arg, `n` (u8), and a 14-account list. `batchCounter` is `pool.batch_counter` **before**
 * this call increments it — `commit_rolls.rs`'s own doc comment calls this out, since the batch
 * PDA is seeded on the pre-increment value, not the post-increment one this call writes.
 *
 * `oracleQueue` and `usdcMint` both name accounts the instruction validates against
 * `protocol_config`'s own stored fields rather than deriving — they are runtime configuration,
 * not PDAs, so the caller supplies them same as `rollerUsdc`.
 */
export async function commitRolls(params: {
  roller: Address;
  poolId: number;
  batchCounter: bigint;
  rollerUsdc: Address;
  usdcMint: Address;
  oracleQueue: Address;
  n: number;
}): Promise<Instruction> {
  const { roller, poolId, batchCounter, rollerUsdc, usdcMint, oracleQueue, n } = params;

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

  return buildAnchorIx({
    name: "commit_rolls",
    args: new Borsh().u8(n).build(),
    accounts: [
      writableSigner(roller),
      readonly(protocolConfig),
      writable(pool),
      readonly(topTier),
      writable(batch),
      writable(rollerUsdc),
      writable(principalVault),
      readonly(usdcMint),
      writable(oracleQueue),
      readonly(programIdentity),
      readonly(VRF_PROGRAM),
      readonly(SYSVAR_SLOT_HASHES),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── vrf_callback ─────────────────────────────────────────────────────────────────────────────
/**
 * One arg, `randomness` (`[u8; 32]`), and a 3-account list. `pool` and `batchId` are read off
 * the target `RollBatch` itself (`batch.pool`, `batch.batch_id`) rather than derived from a
 * `poolId` — they are the values the batch was actually committed against.
 *
 * `vrfProgramIdentity` is not a PDA of this program: it is the VRF provider's own
 * per-callback-program identity, `find_program_address([b"identity", PROGRAM_ID], VRF_PROGRAM)`
 * (`ephemeral-vrf-sdk`'s `scoped_vrf_identity`) — seeded with this program's id but derived
 * under the VRF program, which is why `derivePda`'s second argument names `VRF_PROGRAM` rather
 * than the default.
 */
export async function vrfCallback(params: {
  pool: Address;
  batchId: bigint;
  randomness: Uint8Array;
}): Promise<Instruction> {
  const { pool, batchId, randomness } = params;

  const { address: batch } = await derivePda([
    BATCH_SEED,
    seedFromPubkey(pool),
    seedFromU64Le(batchId),
  ]);
  const { address: vrfProgramIdentity } = await derivePda(
    [VRF_IDENTITY_SEED, seedFromPubkey(PROGRAM_ID)],
    VRF_PROGRAM,
  );

  return buildAnchorIx({
    name: "vrf_callback",
    args: new Borsh().fixed(randomness, 32).build(),
    accounts: [signer(vrfProgramIdentity), writable(batch), readonly(pool)],
  });
}
