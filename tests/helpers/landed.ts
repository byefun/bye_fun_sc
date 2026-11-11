// What a landed transaction did, read from its own metadata: the fee, each account's lamport change
// and the program's own events. Reading balances before and after the send instead would also count
// anything else that touched the wallet in between.

import type { Address } from "@solana/kit";
import type { Connection } from "./env.js";
import { PROGRAM_ID } from "../../scripts/constants.js";
import { decodeEvent, type DecodedEvent } from "../../scripts/lib/decoders.js";

/** A landed transaction as `getTransaction` returns it in JSON, the fields read here. */
type LandedTransaction = {
  transaction: { message: { accountKeys: string[] } };
  meta: {
    fee?: number | bigint;
    logMessages?: string[];
    preBalances: (number | bigint)[];
    postBalances: (number | bigint)[];
    loadedAddresses?: { writable: string[]; readonly: string[] };
  } | null;
};

/** The landed transaction, waiting up to `waitS` seconds for it to be visible; null if it never is. */
async function fetchLandedTransaction(connection: Connection, signature: string, waitS = 30): Promise<LandedTransaction | null> {
  for (let attempt = 0; ; attempt += 1) {
    const tx = (await connection.rpc
      .getTransaction(signature as never, { maxSupportedTransactionVersion: 0, commitment: "confirmed", encoding: "json" })
      .send()) as unknown as LandedTransaction | null;
    if (tx !== null || attempt >= waitS) return tx;
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
}

/** Every account a transaction touched, in index order: its message keys, then its lookup-table loads. */
function transactionKeys(tx: LandedTransaction): string[] {
  return [...tx.transaction.message.accountKeys, ...(tx.meta?.loadedAddresses?.writable ?? []), ...(tx.meta?.loadedAddresses?.readonly ?? [])];
}

/** Event payloads (discriminator first) the program emitted in its own invoke frame, from a transaction's logs. */
function ownProgramData(logMessages: readonly string[], programId: string): Buffer[] {
  const out: Buffer[] = [];
  const stack: string[] = [];
  for (const line of logMessages) {
    const invoke = /^Program (\S+) invoke \[\d+\]$/.exec(line);
    if (invoke) { stack.push(invoke[1]!); continue; }
    if (/^Program \S+ (success|failed: .*)$/.test(line)) { stack.pop(); continue; }
    const data = /^Program data: (\S+)$/.exec(line);
    if (data && stack[stack.length - 1] === programId) out.push(Buffer.from(data[1]!, "base64"));
  }
  return out;
}

export type Landed = {
  fee: bigint;
  /** The lamport change of `address` in this transaction; throws when the transaction does not hold `address`, so a wrong key cannot read as no change. */
  change(address: Address): bigint;
  /** Every event the program emitted in its own frame. */
  events: DecodedEvent[];
};

export async function landed(connection: Connection, signature: string): Promise<Landed> {
  const tx = await fetchLandedTransaction(connection, signature);
  if (tx?.meta == null) throw new Error(`transaction ${signature} not found`);
  const keys = transactionKeys(tx);
  const { preBalances, postBalances } = tx.meta;
  return {
    fee: BigInt(tx.meta.fee ?? 0),
    change: (address) => {
      const at = keys.indexOf(address);
      if (at < 0) throw new Error(`${address} is not in transaction ${signature}`);
      return BigInt(postBalances[at]!) - BigInt(preBalances[at]!);
    },
    events: ownProgramData(tx.meta.logMessages ?? [], String(PROGRAM_ID)).map((data) => decodeEvent(data)),
  };
}
