// Assembling instructions into a v0 transaction over address lookup tables, and measuring what
// that costs on the wire.
//
// `sendIxs` in scripts/_common.ts sends a legacy transaction, where every account key costs 32
// bytes. The widest instructions in this program do not fit one: a full-width `settle_roll` is 38
// accounts, and key bytes alone overrun the 1,232-byte envelope. This is the path that replaces a
// tabled key with a 1-byte index instead.
//
// A v0 transaction is strictly more expressive than a legacy one and the two are interchangeable
// for narrow instructions, so there is no rule here about which instructions must use which — the
// only reason not to route everything through this file is that a table has to exist first.

import {
  appendTransactionMessageInstructions,
  assertIsTransactionWithBlockhashLifetime,
  compileTransaction,
  compressTransactionMessageUsingAddressLookupTables,
  createTransactionMessage,
  getSignatureFromTransaction,
  getTransactionEncoder,
  setTransactionMessageFeePayer,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
  type Address,
  type Blockhash,
  type Instruction,
  type Signature,
  type TransactionSigner,
} from "@solana/kit";
import {
  buildRequestHeapFrameInstruction,
  buildSetComputeUnitLimitInstruction,
  buildSetComputeUnitPriceInstruction,
  COMPUTE_UNIT_LIMIT,
  type Connection,
} from "../_common.js";

/**
 * A packet's payload limit, and the hard ceiling on a serialized transaction — signatures,
 * message header, static keys, blockhash, instruction data and the table references together.
 */
export const TRANSACTION_SIZE_LIMIT = 1232;

/**
 * Up to this many tables, the smallest subset is found by trying all of them; past it, by greedy
 * removal. Eight tables is 256 measurements, which is milliseconds — and no caller has eight.
 */
const EXHAUSTIVE_TABLE_LIMIT = 8;

/** Table address -> the addresses it holds, in table order. Indices are positional. */
export type LookupTableSet = Record<Address, Address[]>;

type BlockhashLifetime = {
  blockhash: Blockhash;
  lastValidBlockHeight: bigint;
};

/**
 * A blockhash that is 32 zero bytes. Only for measuring: every blockhash is 32 bytes, so the
 * serialized size is the same as a real one's, and a transaction built on it can never be sent.
 */
export const MEASUREMENT_BLOCKHASH: BlockhashLifetime = {
  blockhash: "11111111111111111111111111111111" as Blockhash,
  lastValidBlockHeight: 0n,
};

function computeBudgetPreamble(options: {
  computeUnitLimit?: number;
  computeUnitPriceMicroLamports?: bigint;
  heapFrameBytes?: number;
}): Instruction[] {
  const preamble: Instruction[] = [
    buildSetComputeUnitLimitInstruction(options.computeUnitLimit ?? COMPUTE_UNIT_LIMIT),
  ];
  if (options.computeUnitPriceMicroLamports !== undefined) {
    preamble.push(buildSetComputeUnitPriceInstruction(options.computeUnitPriceMicroLamports));
  }
  if (options.heapFrameBytes !== undefined) {
    preamble.push(buildRequestHeapFrameInstruction(options.heapFrameBytes));
  }
  return preamble;
}

/**
 * The transaction message every other entry point in this file is built from: compute-budget
 * preamble, then `instructions`, then compression against `lookupTables`.
 *
 * Compression is by address: a key is replaced by an index wherever it appears in a table,
 * whatever slot it came from. The fee payer and every signer are left static — a signature is
 * addressed by its position among the static keys, so a tabled signer has nowhere to put one.
 */
function buildMessage(params: {
  feePayer: Address | TransactionSigner;
  instructions: readonly Instruction[];
  lifetime: BlockhashLifetime;
  lookupTables?: LookupTableSet;
  computeUnitLimit?: number;
  computeUnitPriceMicroLamports?: bigint;
  heapFrameBytes?: number;
}) {
  const { feePayer, instructions, lifetime, lookupTables } = params;
  const base = createTransactionMessage({ version: 0 });
  const withPayer =
    typeof feePayer === "string"
      ? setTransactionMessageFeePayer(feePayer, base)
      : setTransactionMessageFeePayerSigner(feePayer, base);
  const withLifetime = setTransactionMessageLifetimeUsingBlockhash(lifetime, withPayer);
  const withIxs = appendTransactionMessageInstructions(
    [...computeBudgetPreamble(params), ...instructions],
    withLifetime,
  );
  if (lookupTables === undefined || Object.keys(lookupTables).length === 0) return withIxs;
  return compressTransactionMessageUsingAddressLookupTables(withIxs, lookupTables);
}

function sizeOf(params: Parameters<typeof buildMessage>[0]): number {
  return getTransactionEncoder().encode(compileTransaction(buildMessage(params))).length;
}

/**
 * The subset of `lookupTables` that makes this transaction smallest.
 *
 * Referencing a table is not free: it adds the table's own 32-byte address, two length bytes, and
 * one index byte per key it resolves, against 32 bytes saved per key. A table that resolves a
 * single key of this transaction therefore costs three bytes more than passing that key static,
 * and one that resolves none costs thirty-four. Naming every table unconditionally would grow the
 * narrow instructions — nine of this program's thirty-four — which is the opposite of the point.
 *
 * The subset is chosen by measuring, not by that arithmetic: an exhaustive search while the table
 * count is small enough for one, and a greedy drop of the worst offender past that. Both return a
 * set no larger than the input, so the result is never worse than sending uncompressed.
 */
export function selectLookupTables(params: {
  feePayer: Address | TransactionSigner;
  instructions: readonly Instruction[];
  lookupTables: LookupTableSet;
  computeUnitLimit?: number;
  computeUnitPriceMicroLamports?: bigint;
  heapFrameBytes?: number;
}): LookupTableSet {
  const names = Object.keys(params.lookupTables) as Address[];
  const measure = (subset: Address[]) =>
    sizeOf({
      ...params,
      lifetime: MEASUREMENT_BLOCKHASH,
      lookupTables: Object.fromEntries(
        subset.map((n) => [n, params.lookupTables[n]]),
      ) as LookupTableSet,
    });

  const pick = (subset: Address[]) =>
    Object.fromEntries(subset.map((n) => [n, params.lookupTables[n]])) as LookupTableSet;

  if (names.length <= EXHAUSTIVE_TABLE_LIMIT) {
    let best = names;
    let bestSize = measure(names);
    for (let mask = 0; mask < 1 << names.length; mask++) {
      const subset = names.filter((_, i) => (mask >> i) & 1);
      const size = measure(subset);
      // Ties go to the smaller set: a table reference the size does not pay for still costs a
      // loaded account on-chain.
      if (size < bestSize || (size === bestSize && subset.length < best.length)) {
        best = subset;
        bestSize = size;
      }
    }
    return pick(best);
  }

  let subset = [...names];
  let size = measure(subset);
  for (;;) {
    let dropped: Address[] | null = null;
    let droppedSize = size;
    for (const name of subset) {
      const candidate = subset.filter((n) => n !== name);
      const candidateSize = measure(candidate);
      if (candidateSize < droppedSize) {
        dropped = candidate;
        droppedSize = candidateSize;
      }
    }
    if (dropped === null) return pick(subset);
    subset = dropped;
    size = droppedSize;
  }
}

/**
 * Serialized byte length of the transaction these instructions compile to — what gets compared
 * against `TRANSACTION_SIZE_LIMIT`.
 *
 * Signatures are counted at full width: `compileTransaction` reserves 64 bytes per required
 * signature and the encoder writes them as zeros, so an unsigned measurement is the same length
 * as the signed transaction it stands for.
 */
export function measureTransactionSize(params: {
  feePayer: Address;
  instructions: readonly Instruction[];
  lookupTables?: LookupTableSet;
  computeUnitLimit?: number;
  computeUnitPriceMicroLamports?: bigint;
  heapFrameBytes?: number;
}): number {
  const lookupTables =
    params.lookupTables === undefined
      ? undefined
      : selectLookupTables({ ...params, lookupTables: params.lookupTables });
  return sizeOf({ ...params, lookupTables, lifetime: MEASUREMENT_BLOCKHASH });
}

/**
 * Sign and send `instructions` as one v0 transaction over `lookupTables`, with the same
 * compute-budget preamble `sendIxs` prepends.
 *
 * Every table named here must already be on-chain and past its warm-up — a table is not usable in
 * the same slot it was created or last extended in, and the runtime rejects the transaction rather
 * than resolving a stale view of it.
 */
export async function sendV0Ixs(
  connection: Connection,
  feePayer: TransactionSigner,
  instructions: readonly Instruction[],
  lookupTables: LookupTableSet,
  options: {
    computeUnitLimit?: number;
    computeUnitPriceMicroLamports?: bigint;
    heapFrameBytes?: number;
    skipPreflight?: boolean;
  } = {},
): Promise<Signature> {
  const { value: latest } = await connection.rpc.getLatestBlockhash().send();
  const selected = selectLookupTables({ ...options, feePayer, instructions, lookupTables });
  const message = buildMessage({
    ...options,
    feePayer,
    instructions,
    lifetime: { blockhash: latest.blockhash, lastValidBlockHeight: latest.lastValidBlockHeight },
    lookupTables: selected,
  });

  const size = getTransactionEncoder().encode(compileTransaction(message)).length;
  if (size > TRANSACTION_SIZE_LIMIT) {
    throw new Error(
      `transaction is ${size} bytes, over the ${TRANSACTION_SIZE_LIMIT}-byte limit — ` +
        `${Object.keys(selected).length} of ${Object.keys(lookupTables).length} ` +
        `lookup table(s) resolved enough keys to be worth naming`,
    );
  }

  const signed = await signTransactionMessageWithSigners(message);
  assertIsTransactionWithBlockhashLifetime(signed);
  await connection.sendAndConfirmTransaction(signed, {
    commitment: "confirmed",
    skipPreflight: options.skipPreflight ?? false,
  });
  return getSignatureFromTransaction(signed);
}

/**
 * Sign `instructions` as one v0 transaction without lookup tables, with the compute-budget
 * preamble `sendIxs` prepends, and hand back the signature before anything is sent. `send` then
 * submits it at `confirmed` and resolves once it confirms; preflight is skipped, as `sendIxs`
 * does, unless `options.skipPreflight` is false. A caller that must record the signature before the network can lose it (a send that
 * throws says nothing about whether the transaction landed) records it, then calls `send`.
 * `lastValidBlockHeight` is the height past which the cluster will never include the transaction.
 */
export async function sendSignedFirst(
  connection: Connection,
  feePayer: TransactionSigner,
  instructions: readonly Instruction[],
  options: { computeUnitLimit?: number; computeUnitPriceMicroLamports?: bigint; heapFrameBytes?: number; skipPreflight?: boolean } = {},
): Promise<{ signature: Signature; lastValidBlockHeight: bigint; send: () => Promise<void> }> {
  const { value: latest } = await connection.rpc.getLatestBlockhash().send();
  const message = buildMessage({
    ...options,
    feePayer,
    instructions,
    lifetime: { blockhash: latest.blockhash, lastValidBlockHeight: latest.lastValidBlockHeight },
  });
  const signed = await signTransactionMessageWithSigners(message);
  assertIsTransactionWithBlockhashLifetime(signed);
  return {
    signature: getSignatureFromTransaction(signed),
    lastValidBlockHeight: latest.lastValidBlockHeight,
    send: async () => {
      await connection.sendAndConfirmTransaction(signed, { commitment: "confirmed", skipPreflight: options.skipPreflight ?? true });
    },
  };
}
