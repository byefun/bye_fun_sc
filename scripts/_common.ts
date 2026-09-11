// Runtime helpers shared by every operational script and integration test:
// connect, load a signer, read accounts, and send instructions with the
// compute-budget preamble.

import { connect, loadWalletFromFile } from "solana-kite";
import type { Address, Instruction, TransactionSigner } from "@solana/kit";
import { COMPUTE_BUDGET_PROGRAM, env, rpcUrl, wsUrl } from "./constants.js";

export { env };
export const PROGRAM_ID = env.programId;
export const DEPLOYER_KEYPAIR_PATH = env.deployerKeypairPath;

export type Connection = ReturnType<typeof connect>;

export function connectCluster(): Connection {
  return connect(rpcUrl, wsUrl);
}

/** Load a TransactionSigner from a Solana keypair JSON file (64-byte array). */
export async function loadKeypair(path: string): Promise<TransactionSigner> {
  return loadWalletFromFile(path);
}

/** Read a required env var, or fail with a message naming it. */
export function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`Missing required env var: ${name}`);
  return value;
}

// ── Compute budget ───────────────────────────────────────────────────────────
// The default per-tx ceiling is ~200k CU, but a single Metaplex pNFT `TransferV1`
// CPI burns ~130–180k — so any transaction moving more than one card blows the
// default and aborts with "Program failed to complete". Raise to the per-tx max;
// billing is on CU *consumed*, not on the limit, so a high ceiling is harmless.
export const COMPUTE_UNIT_LIMIT = 1_400_000;

// A program that declares its own #[global_allocator] with a larger heap (the usual
// answer to pNFT-transfer batches exhausting the default 32KB bump heap) requires
// EVERY transaction to carry a RequestHeapFrame instruction, or the allocator writes
// past the heap → "Access violation in heap section", aborting before any handler
// runs. Whether bye.fun's program needs this is a later-phase decision, so `sendIxs`
// takes it as an opt-in rather than always prepending it.
export const MAX_HEAP_FRAME_BYTES = 256 * 1024;

/** ComputeBudget::SetComputeUnitLimit */
export function buildSetComputeUnitLimitInstruction(units: number): Instruction {
  const data = new Uint8Array(5);
  data[0] = 0x02;
  new DataView(data.buffer).setUint32(1, units, true);
  return { programAddress: COMPUTE_BUDGET_PROGRAM, accounts: [], data };
}

/** ComputeBudget::SetComputeUnitPrice (priority fee, micro-lamports per CU) */
export function buildSetComputeUnitPriceInstruction(microLamports: bigint): Instruction {
  const data = new Uint8Array(9);
  data[0] = 0x03;
  new DataView(data.buffer).setBigUint64(1, microLamports, true);
  return { programAddress: COMPUTE_BUDGET_PROGRAM, accounts: [], data };
}

/** ComputeBudget::RequestHeapFrame (multiple of 1024, max 256 * 1024) */
export function buildRequestHeapFrameInstruction(bytes: number): Instruction {
  const data = new Uint8Array(5);
  data[0] = 0x01;
  new DataView(data.buffer).setUint32(1, bytes, true);
  return { programAddress: COMPUTE_BUDGET_PROGRAM, accounts: [], data };
}

/**
 * Sign and send `instructions` as one transaction, prepending the compute-budget
 * preamble. This is the default send path for every script — going around it means
 * re-deriving the CU limit by hand.
 */
export async function sendIxs(
  connection: Connection,
  feePayer: TransactionSigner,
  instructions: Instruction[],
  options: {
    computeUnitLimit?: number;
    computeUnitPriceMicroLamports?: bigint;
    heapFrameBytes?: number;
  } = {},
): Promise<string> {
  const {
    computeUnitLimit = COMPUTE_UNIT_LIMIT,
    computeUnitPriceMicroLamports,
    heapFrameBytes,
  } = options;

  const preamble: Instruction[] = [buildSetComputeUnitLimitInstruction(computeUnitLimit)];
  if (computeUnitPriceMicroLamports !== undefined) {
    preamble.push(buildSetComputeUnitPriceInstruction(computeUnitPriceMicroLamports));
  }
  if (heapFrameBytes !== undefined) {
    preamble.push(buildRequestHeapFrameInstruction(heapFrameBytes));
  }

  return connection.sendTransactionFromInstructions({
    feePayer,
    instructions: [...preamble, ...instructions],
  });
}

// ── Account reads ────────────────────────────────────────────────────────────
/** Raw account data (base64-decoded), or null if the account does not exist. */
export async function fetchAccountData(
  connection: Connection,
  addr: Address,
): Promise<Buffer | null> {
  const res = await connection.rpc.getAccountInfo(addr, { encoding: "base64" }).send();
  if (!res.value) return null;
  const [b64] = res.value.data as [string, string];
  return Buffer.from(b64, "base64");
}

export async function accountExists(connection: Connection, addr: Address): Promise<boolean> {
  return (await fetchAccountData(connection, addr)) !== null;
}
