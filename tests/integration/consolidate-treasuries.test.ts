// `consolidate_treasuries` — moving a pool created before the treasuries were shared onto them.
//
// `init_pool` now creates only the shared Treasury 03 and Treasury 04, so the pools this suite
// consolidates are validator fixtures: current-layout pools still naming their own Treasury 03,
// with both per-pool token accounts held by the pool (scripts/build-legacy-pool-fixtures.ts). One
// has a batch open.
//
// The success row moves both balances and closes both accounts; the refusals are a second call,
// a batch open, and any signer but the Administrator. The removed instructions are here too:
// their discriminators reach no handler.

import { before, describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  assertProgramIsLive,
  expectAnchorError,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  loadUnprivileged,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { PROGRAM_ID } from "../../scripts/constants.js";
import { consolidateTreasuries, deriveSharedTreasuries } from "../../scripts/lib/admin.js";
import { buildAnchorIx, signer } from "../../scripts/lib/anchor.js";
import {
  decodeEvent,
  decodePool,
  type DecodedEvent,
  type TreasuriesConsolidated,
} from "../../scripts/lib/decoders.js";
import {
  BUSY_UNCONSOLIDATED_POOL_ID,
  TOKEN_ACCOUNT_LEN,
  UNCONSOLIDATED_POOL_ID,
  UNCONSOLIDATED_T03_AMOUNT,
  UNCONSOLIDATED_T04_AMOUNT,
  rentExempt,
  unconsolidatedPoolAccounts,
} from "../../scripts/lib/legacy-pool-fixtures.js";
import { EXECUTED_ROWS, UNAUTHORIZED } from "../helpers/authority-matrix.js";

const PROGRAM_DATA_PREFIX = "Program data: ";
/** `RollBatchInFlight`. */
const ROLL_BATCH_IN_FLIGHT = 6200;
/** Anchor's `AccountNotInitialized` — a closed per-pool account. */
const ACCOUNT_NOT_INITIALIZED = 3012;
/** Anchor's `InstructionFallbackNotFound` — a discriminator the program does not declare. */
const INSTRUCTION_FALLBACK_NOT_FOUND = 101;
/** The base fee of a one-signature transaction, which the Administrator pays. */
const SIGNATURE_FEE = 5_000n;

describe("consolidate_treasuries — a pool's own treasuries onto the shared ones, once", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let unprivileged: TransactionSigner;
  let usdcMint: Address;
  let shared: { treasury03: Address; treasury04: Address };

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  async function balanceOf(tokenAccount: Address): Promise<bigint> {
    const { value } = await connection.rpc.getTokenAccountBalance(tokenAccount).send();
    return BigInt(value.amount);
  }

  async function lamportsOf(account: Address): Promise<bigint> {
    return (await connection.rpc.getBalance(account, { commitment: "confirmed" }).send()).value;
  }

  async function consolidate(poolId: number, who: TransactionSigner = admin) {
    return sendIxs(connection, who, [
      await consolidateTreasuries({ administrator: who.address, poolId, usdcMint }),
    ]);
  }

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    unprivileged = await loadUnprivileged();
    usdcMint = (await ensureProtocol(connection, admin)).usdcMint;
    shared = await deriveSharedTreasuries();
    for (const id of [UNCONSOLIDATED_POOL_ID, BUSY_UNCONSOLIDATED_POOL_ID]) {
      const { pool } = await unconsolidatedPoolAccounts(id, 0);
      assert.ok(await fetchAccountData(connection, pool.address), `the validator did not load pool ${id}`);
    }
  });

  for (const [label, who] of [
    ["the Operator", () => operator],
    ["an unprivileged wallet", () => unprivileged],
  ] as const) {
    it(`refuses ${label}`, async () => {
      await expectAnchorError(consolidate(UNCONSOLIDATED_POOL_ID, who()), UNAUTHORIZED);
    });
  }

  it("refuses a pool with a batch open, moving nothing", async () => {
    const busy = await unconsolidatedPoolAccounts(BUSY_UNCONSOLIDATED_POOL_ID, 1);
    await expectAnchorError(consolidate(BUSY_UNCONSOLIDATED_POOL_ID), ROLL_BATCH_IN_FLIGHT);
    assert.equal(await balanceOf(busy.treasury03.address), UNCONSOLIDATED_T03_AMOUNT);
    assert.equal(await balanceOf(busy.treasury04.address), UNCONSOLIDATED_T04_AMOUNT);
    assert.equal(
      decodePool((await fetchAccountData(connection, busy.pool.address))!).treasury03,
      busy.treasury03.address,
    );
  });

  it("moves both balances to the shared accounts, points the pool at them, and closes its own", async () => {
    const own = await unconsolidatedPoolAccounts(UNCONSOLIDATED_POOL_ID, 0);
    assert.ok(await fetchAccountData(connection, shared.treasury03), "precondition: shared Treasury 03 exists");
    assert.ok(await fetchAccountData(connection, shared.treasury04), "precondition: shared Treasury 04 exists");
    const t03Before = await balanceOf(shared.treasury03);
    const t04Before = await balanceOf(shared.treasury04);
    const adminBefore = await lamportsOf(admin.address);

    const signature = await consolidate(UNCONSOLIDATED_POOL_ID);

    assert.equal(await balanceOf(shared.treasury03), t03Before + UNCONSOLIDATED_T03_AMOUNT);
    assert.equal(await balanceOf(shared.treasury04), t04Before + UNCONSOLIDATED_T04_AMOUNT);
    assert.equal(await fetchAccountData(connection, own.treasury03.address), null, "own Treasury 03 left open");
    assert.equal(await fetchAccountData(connection, own.treasury04.address), null, "own Treasury 04 left open");
    assert.equal(
      (await lamportsOf(admin.address)) - adminBefore,
      2n * BigInt(rentExempt(TOKEN_ACCOUNT_LEN)) - SIGNATURE_FEE,
      "both accounts' rent did not reach the Administrator",
    );

    const pool = decodePool((await fetchAccountData(connection, own.pool.address))!);
    assert.equal(pool.treasury03, shared.treasury03);
    assert.equal(pool.treasury04, shared.treasury04);

    const emitted = (await eventsOf(signature)).filter((e) => e.name === "TreasuriesConsolidated");
    assert.equal(emitted.length, 1);
    assert.deepEqual(
      { ...(emitted[0]!.data as TreasuriesConsolidated), slot: 0n },
      {
        pool: own.pool.address,
        slot: 0n,
        authority: admin.address,
        oldTreasury03: own.treasury03.address,
        oldTreasury04: own.treasury04.address,
        treasury03: shared.treasury03,
        treasury04: shared.treasury04,
        amount03: UNCONSOLIDATED_T03_AMOUNT,
        amount04: UNCONSOLIDATED_T04_AMOUNT,
      },
    );
  });

  it("refuses a second call, the pool's own accounts being closed", async () => {
    const t03Before = await balanceOf(shared.treasury03);
    await expectAnchorError(consolidate(UNCONSOLIDATED_POOL_ID), ACCOUNT_NOT_INITIALIZED);
    assert.equal(await balanceOf(shared.treasury03), t03Before);
  });

  it("covers exactly the matrix rows this suite is answerable for", () => {
    assert.deepEqual(
      EXECUTED_ROWS.filter((r) => r.suite === "consolidate-treasuries").map((r) => `${r.instruction}.${r.signer}`),
      ["consolidate_treasuries.administrator"],
    );
  });

  for (const name of ["transfer_t03", "transfer_t04", "migrate_treasury_03"]) {
    it(`${name} no longer exists: its discriminator reaches no handler`, async () => {
      const ix = buildAnchorIx({ name, accounts: [signer(admin.address)] });
      assert.equal(ix.programAddress, PROGRAM_ID);
      await expectAnchorError(sendIxs(connection, admin, [ix]), INSTRUCTION_FALLBACK_NOT_FOUND);
    });
  }
});
