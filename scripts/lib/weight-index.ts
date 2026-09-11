// WeightIndex account creation helper — T-45.
//
// WeightIndex is a pre-created keypair account, not a PDA (state/weight_index.rs:8-9) — the
// account is 196,672 bytes and MAX_PERMITTED_DATA_INCREASE (10,240 bytes) caps how much a
// *program* can grow an account via CPI in one call, which is design §2's reason a PDA can't work
// here. `init_pool`'s `#[account(zero)]` guard (init_pool.rs:216) and its blank-payload scan
// (init_pool.rs:88-103) require this account to arrive already allocated at the exact
// `8 + WeightIndex::INIT_SPACE` size, owned by the program, and still all-zero; `load_init()`
// (init_pool.rs:115) writes the discriminator, so a caller that pre-writes it is rejected.
//
// One top-level SystemProgram::CreateAccount allocates the whole account in a single call
// (P7 open, C2): MAX_PERMITTED_DATA_INCREASE binds a program growing an account via CPI, not this
// client-side path — and no top-level System instruction grows an already-existing account, so
// stepped growth is not implementable here. `CreateAccount` zero-fills the new account, so no
// separate zeroing step exists. The System instruction is hand-encoded rather than adding a
// dependency (P7 open, decision 7) — the same reason anchor.ts's own header gives for existing.

import { generateKeyPairSigner } from "@solana/kit";
import type { Address, Instruction, KeyPairSigner } from "@solana/kit";
import type { Connection } from "../_common.js";
import { PROGRAM_ID, SYSTEM_PROGRAM } from "../constants.js";
import { Borsh, writableSigner } from "./anchor.js";

// ── Account size ─────────────────────────────────────────────────────────────────────────────
//
// Mirrors `WeightIndex`'s `#[account(zero_copy)]` field layout (state/weight_index.rs:12-20),
// pinned byte-for-byte by that file's own `weight_index_field_order` test:
//   pool(32) total_weight(8) high_water(4) free_count(4) _padding(8)
//   tree([u64; FENWICK_CAPACITY]) free_stack([u32; FENWICK_CAPACITY])
// FENWICK_CAPACITY is mirrored, not imported, from common/constants.rs — there is no Rust->TS
// bridge in this repo.
const FENWICK_CAPACITY = 16_384;
const WEIGHT_INDEX_INIT_SPACE = 32 + 8 + 4 + 4 + 8 + 8 * FENWICK_CAPACITY + 4 * FENWICK_CAPACITY;

/** `8 + WeightIndex::INIT_SPACE` — cross-check this against init_pool.rs:90's own
 * `data_len() == 8 + WeightIndex::INIT_SPACE` pin, not a number typed in by hand. */
export const WEIGHT_INDEX_ACCOUNT_LEN = 8 + WEIGHT_INDEX_INIT_SPACE;

// ── SystemProgram::CreateAccount ─────────────────────────────────────────────────────────────
//
// system_instruction.rs: SystemInstruction::CreateAccount is variant 0. Layout: u32 LE tag ‖
// lamports u64 LE ‖ space u64 LE ‖ owner 32B, all little-endian.
const CREATE_ACCOUNT_IX_INDEX = 0;

function encodeCreateAccountArgs(lamports: bigint, space: bigint, owner: Address): Buffer {
  return new Borsh().u32(CREATE_ACCOUNT_IX_INDEX).u64(lamports).u64(space).pubkey(owner).build();
}

/**
 * Pure, synchronous `CreateAccount` instruction builder — `weightIndex` must be a fresh keypair's
 * own address (it signs to prove ownership of that key), `lamports` the caller's already-fetched
 * rent-exempt balance for `WEIGHT_INDEX_ACCOUNT_LEN`. Owner is always `PROGRAM_ID`, never the
 * System program — `init_pool`'s `#[account(zero)]` guard requires this account already owned by
 * the program before it validates anything else about it.
 */
export function buildCreateWeightIndexAccountIx(params: {
  payer: Address;
  weightIndex: Address;
  lamports: bigint;
}): Instruction {
  const { payer, weightIndex, lamports } = params;
  return {
    programAddress: SYSTEM_PROGRAM,
    accounts: [writableSigner(payer), writableSigner(weightIndex)],
    data: new Uint8Array(
      encodeCreateAccountArgs(lamports, BigInt(WEIGHT_INDEX_ACCOUNT_LEN), PROGRAM_ID),
    ),
  };
}

// ── Rent ──────────────────────────────────────────────────────────────────────────────────────
//
// Rent for this size differs by cluster (measured: 1.369728 SOL on localnet vs 1.2463344 SOL on
// the CLI's configured cluster, same byte count) — always read at runtime, never a literal.
export async function fetchWeightIndexRentLamports(rpc: Connection): Promise<bigint> {
  return rpc.rpc.getMinimumBalanceForRentExemption(BigInt(WEIGHT_INDEX_ACCOUNT_LEN)).send();
}

/**
 * Generates a fresh `WeightIndex` keypair, fetches its rent-exempt balance, and builds the one
 * `CreateAccount` instruction that allocates it — zero-filled, owned by `PROGRAM_ID`, at the exact
 * layout size. Returns the keypair alongside the instruction: the caller must co-sign the
 * transaction with it (a new account's own key signs its own creation) and pin `signer.address`
 * into `init_pool`.
 */
export async function createWeightIndexAccount(params: {
  rpc: Connection;
  payer: Address;
}): Promise<{ signer: KeyPairSigner; instruction: Instruction }> {
  const { rpc, payer } = params;
  const signer = await generateKeyPairSigner();
  const lamports = await fetchWeightIndexRentLamports(rpc);
  const instruction = buildCreateWeightIndexAccountIx({
    payer,
    weightIndex: signer.address,
    lamports,
  });
  return { signer, instruction };
}
