// The shared Treasury 03 and Treasury 04, as integration suites meet them.
//
// One Treasury 04 serves every pool the validator holds, so a suite that needs a known balance
// cannot assume the one it starts with: every earlier suite's settlements credited it. And
// `sweep_t04_overflow` runs only over every pool at once, none with a batch open, so a suite that
// sweeps depends on every earlier suite having closed out its batches.

import { AccountRole, type Instruction } from "@solana/kit";
import type { Address, Connection, TransactionSigner } from "./env.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { derivePda } from "../../scripts/lib/anchor.js";
import {
  buildCreateLookupTableInstruction,
  buildExtendLookupTableInstruction,
  chunkAddresses,
  decodeLookupTable,
  deriveLookupTableAddress,
  missingAddresses,
} from "../../scripts/lib/lookup-tables.js";
import { sendV0Ixs } from "../../scripts/lib/v0-tx.js";
import { deriveSharedTreasuries, setT04Ceiling, sweepT04Overflow } from "../../scripts/lib/admin.js";
import { decodePool, decodeProtocolConfig } from "../../scripts/lib/decoders.js";

// Verbatim against programs/bye_machine/src/common/seeds.rs.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

/**
 * The lookup table this process sends sweeps over, and the key that controls it. A sweep names
 * every pool, and with the dozens a full integration run creates a legacy transaction outgrows the
 * 1,232-byte limit; over a table each pool costs one byte.
 */
let sweepTable: { address: Address; authority: TransactionSigner } | null = null;

/** Blocks until the table's last extension is behind the current slot, when it starts resolving. */
async function waitForWarmUp(connection: Connection, table: Address): Promise<void> {
  for (let attempt = 0; attempt < 60; attempt++) {
    const data = await fetchAccountData(connection, table);
    if (data !== null) {
      const { lastExtendedSlot } = decodeLookupTable(data);
      const current = BigInt(await connection.rpc.getSlot({ commitment: "confirmed" }).send());
      if (current > lastExtendedSlot) return;
    }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error(`${table}: still not past its last-extended slot after 24s`);
}

/**
 * The table, created on first use with `authority` controlling and paying for it, and extended
 * with whichever of `addresses` it lacks. Returns what it holds, in table order.
 */
async function sweepTableHolding(
  connection: Connection,
  authority: TransactionSigner,
  addresses: readonly Address[],
): Promise<{ address: Address; held: Address[] }> {
  if (sweepTable === null) {
    const recentSlot = BigInt(await connection.rpc.getSlot({ commitment: "finalized" }).send());
    const { address, bump } = await deriveLookupTableAddress(authority.address, recentSlot);
    await sendIxs(connection, authority, [
      buildCreateLookupTableInstruction({
        lookupTable: address,
        authority: authority.address,
        payer: authority.address,
        recentSlot,
        bump,
      }),
    ]);
    sweepTable = { address, authority };
  }
  const { address, authority: owner } = sweepTable;
  const held = decodeLookupTable((await fetchAccountData(connection, address))!).addresses;
  const missing = missingAddresses(addresses, held);
  if (missing.length === 0) return { address, held };
  for (const chunk of chunkAddresses(missing)) {
    await sendIxs(connection, owner, [
      buildExtendLookupTableInstruction({
        lookupTable: address,
        authority: owner.address,
        payer: owner.address,
        addresses: chunk,
      }),
    ]);
  }
  await waitForWarmUp(connection, address);
  return { address, held: decodeLookupTable((await fetchAccountData(connection, address))!).addresses };
}

/**
 * Sends a `sweep_t04_overflow` instruction, signed by `feePayer`, as a v0 transaction over this
 * process's lookup table, which `tableAuthority` creates and extends with every non-signer account
 * the instruction names. Every test sweep goes through here, so none outgrows a legacy envelope.
 */
export async function sendSweep(params: {
  connection: Connection;
  feePayer: TransactionSigner;
  tableAuthority: TransactionSigner;
  instruction: Instruction;
}): Promise<string> {
  const { connection, feePayer, tableAuthority, instruction } = params;
  const keys = (instruction.accounts ?? [])
    .filter((a) => a.role === AccountRole.READONLY || a.role === AccountRole.WRITABLE)
    .map((a) => a.address);
  const table = await sweepTableHolding(connection, tableAuthority, keys);
  return sendV0Ixs(connection, feePayer, [instruction], { [table.address]: table.held });
}

/** `0..protocol_config.pool_counter` — the pool set `sweep_t04_overflow` requires. */
export async function everyPoolId(connection: Connection): Promise<number[]> {
  const { address } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const config = decodeProtocolConfig((await fetchAccountData(connection, address))!);
  return Array.from({ length: config.poolCounter }, (_, id) => id);
}

/** The pools among `poolIds` with a batch open — what refuses a sweep, named for its message. */
export async function poolsWithOpenBatches(connection: Connection, poolIds: number[]): Promise<number[]> {
  const busy: number[] = [];
  for (const id of poolIds) {
    const { address } = await derivePda([POOL_SEED, seedFromU16Le(id)]);
    if (decodePool((await fetchAccountData(connection, address))!).openBatches > 0) busy.push(id);
  }
  return busy;
}

/** `protocol_config.t04_ceiling`. */
export async function t04Ceiling(connection: Connection): Promise<bigint> {
  const { address } = await derivePda([PROTOCOL_CONFIG_SEED]);
  return decodeProtocolConfig((await fetchAccountData(connection, address))!).t04Ceiling;
}

/**
 * Sweeps the shared Treasury 04 down to `ceiling` (no lower: the sweep moves only the excess),
 * then puts the protocol's ceiling back as it was. Throws naming the pools with a batch open
 * before trying, since any of them refuses the sweep.
 */
export async function sweepTreasury04To(params: {
  connection: Connection;
  administrator: TransactionSigner;
  operator: TransactionSigner;
  ceiling: bigint;
}): Promise<void> {
  const { connection, administrator, operator, ceiling } = params;
  const poolIds = await everyPoolId(connection);
  const busy = await poolsWithOpenBatches(connection, poolIds);
  if (busy.length > 0) {
    throw new Error(`cannot sweep Treasury 04: pools ${busy.join(", ")} have a batch open`);
  }
  const restore = await t04Ceiling(connection);
  await sendIxs(connection, administrator, [await setT04Ceiling({ administrator: administrator.address, ceiling })]);
  await sendSweep({
    connection,
    feePayer: operator,
    tableAuthority: operator,
    instruction: await sweepT04Overflow({ operator: operator.address, poolIds }),
  });
  if (restore !== ceiling) {
    await sendIxs(connection, administrator, [
      await setT04Ceiling({ administrator: administrator.address, ceiling: restore }),
    ]);
  }
}

/**
 * Brings the shared Treasury 04 down to one base unit before a suite opens a batch it will settle.
 *
 * A blank refunds only while Treasury 04 plus the roll's own credit is short of its face, and the
 * localnet has no swap venue to pay one, so every suite that settles must start from a Treasury 04
 * as empty as the per-pool one a fresh pool used to have — whatever earlier suites credited. A
 * no-op when it already holds one unit or less. Throws naming any pool with a batch open, since
 * the sweep needs every pool idle.
 */
export async function drainTreasury04(params: {
  connection: Connection;
  administrator: TransactionSigner;
  operator: TransactionSigner;
}): Promise<void> {
  const { connection } = params;
  const { treasury04 } = await deriveSharedTreasuries();
  const { value } = await connection.rpc.getTokenAccountBalance(treasury04).send();
  if (BigInt(value.amount) <= 1n) return;
  await sweepTreasury04To({ ...params, ceiling: 1n });
}

/**
 * Fails naming every pool `pool_counter` counts that still has a batch open. Run after each suite
 * that opens batches: a batch left open refuses every later sweep, which `drainTreasury04` and
 * the sweep suites depend on.
 */
export async function assertEveryPoolIdle(connection: Connection): Promise<void> {
  const busy = await poolsWithOpenBatches(connection, await everyPoolId(connection));
  if (busy.length > 0) {
    throw new Error(`pools ${busy.join(", ")} were left with a batch open`);
  }
}

export type { Address };
