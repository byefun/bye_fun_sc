// `migrate_pool` and the escrow bid it makes room for — measured against a live validator.
//
// What this file proves, each part of which fails differently:
//
//   - **the growth** — a 342-byte three-slot pool comes out at 402 bytes reading back every field
//     it held, with blank-face and blank-count slots 3–7 zero: bytes 155–185 moved 40 later,
//     bytes 186–341 moved 60 later. It is rent-exempt at the new length, and the Administrator
//     paid exactly the difference. The old pool is a genesis-loaded fixture
//     (`tests/fixtures/legacy-pool.json`): no instruction can create one any more.
//   - **idempotence** — a second call, and a call on a pool created at the current length, change
//     nothing and charge nothing but the fee.
//   - **the refusals** — a `Pool`-tagged account at the 276-byte pre-escrow length, an account of
//     another type, and one another program owns, each with its own code and nothing changed.
//   - **authorisation** — Administrator only, refused on the legacy pool before it is grown, so
//     the refusal is not a no-op on an account already at the new length.
//   - **the bid** — one pool's bid moves without touching another's, and the bid and the buyback
//     rate refuse whichever move would put the rate above a set bid, or either above 10,000.

import { before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { addSignersToInstruction } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  withAccountAt,
  withSigner,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, type PoolAccounts } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { EXECUTED_ROWS, UNAUTHORIZED } from "../helpers/authority-matrix.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { migratePool, setPause, updateConfig, type ConfigUpdate } from "../../scripts/lib/admin.js";
import { decodeEvent, decodePool, type DecodedEvent } from "../../scripts/lib/decoders.js";
import {
  LEGACY_POOL_ID,
  LEGACY_POOL_LEN,
  MALFORMED_POOL_ID,
  MALFORMED_POOL_LEN,
  POOL_LEN,
  grownPoolFields,
  legacyPoolAccount,
  rentExempt,
} from "../../scripts/lib/legacy-pool-fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";

const PROGRAM_DATA_PREFIX = "Program data: ";

/** Anchor's `AccountDiscriminatorMismatch`, `AccountDidNotDeserialize`, `AccountOwnedByWrongProgram`. */
const ACCOUNT_DISCRIMINATOR_MISMATCH = 3002;
const ACCOUNT_DID_NOT_DESERIALIZE = 3003;
const ACCOUNT_OWNED_BY_WRONG_PROGRAM = 3007;
const BUYBACK_RATE_OUT_OF_RANGE = 6525;
const ESCROW_BID_OUT_OF_RANGE = 6526;

/** `pool`'s position in `migrate_pool`'s account list. */
const POOL_INDEX = 2;

describe("migrate_pool — widening a three-slot pool to eight blank slots", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let usdcMint: Address;
  let current: PoolAccounts & { poolId: number };
  let legacy: Awaited<ReturnType<typeof legacyPoolAccount>>;

  async function account(address: Address): Promise<{ data: Buffer; lamports: bigint }> {
    const { value } = await connection.rpc
      .getAccountInfo(address, { encoding: "base64", commitment: "confirmed" })
      .send();
    assert.ok(value, `${address} does not exist`);
    return { data: Buffer.from(value.data[0], "base64"), lamports: BigInt(value.lamports) };
  }

  async function lamportsOf(address: Address): Promise<bigint> {
    const { value } = await connection.rpc.getBalance(address, { commitment: "confirmed" }).send();
    return BigInt(value);
  }

  async function feeOf(signature: string): Promise<bigint> {
    const tx = await connection.rpc
      .getTransaction(signature as never, {
        maxSupportedTransactionVersion: 0,
        encoding: "json",
        commitment: "confirmed",
      })
      .send();
    assert.ok(tx?.meta, `no transaction record for ${signature}`);
    return BigInt(tx.meta.fee);
  }

  async function migrate(poolId: number): Promise<string> {
    return sendIxs(connection, admin, [await migratePool({ administrator: admin.address, poolId })]);
  }

  async function migrateAt(address: Address): Promise<string> {
    const ix = await migratePool({ administrator: admin.address, poolId: LEGACY_POOL_ID });
    return sendIxs(connection, admin, [withAccountAt(ix, POOL_INDEX, address)]);
  }

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();

    const protocolState = await ensureProtocol(connection, admin);
    usdcMint = protocolState.usdcMint;
    const { signer: weightIndexSigner, instruction } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], instruction)]);
    const poolId = protocolState.poolCounter;
    current = {
      ...(await createPool({
        connection,
        administrator: admin,
        poolId,
        weightIndex: weightIndexSigner.address,
        usdcMint,
        args: launchProfile(),
      })),
      poolId,
    };
    legacy = await legacyPoolAccount(LEGACY_POOL_ID);

    const loaded = await account(legacy.address);
    assert.equal(loaded.data.length, LEGACY_POOL_LEN, "the validator did not load the legacy pool fixture");
    assert.deepEqual(loaded.data, legacy.data);
  });

  it("refuses the Operator in the Administrator's slot and leaves the old pool as it was", async () => {
    const ix = await migratePool({ administrator: admin.address, poolId: LEGACY_POOL_ID });
    await expectAnchorError(
      sendIxs(connection, operator, [withSigner(ix, operator.address)]),
      UNAUTHORIZED,
    );
    assert.deepEqual((await account(legacy.address)).data, legacy.data);
  });

  it("no instruction reads a 342-byte pool before it is grown", async () => {
    await expectAnchorError(
      sendIxs(connection, admin, [
        await setPause({ administrator: admin.address, poolId: LEGACY_POOL_ID, deposits: true, rolls: false, reason: 1 }),
      ]),
      ACCOUNT_DID_NOT_DESERIALIZE,
    );
    assert.deepEqual((await account(legacy.address)).data, legacy.data);
  });

  it("grows a 342-byte pool to 402 bytes: every field read back, slots 3–7 zero, rent paid by the Administrator", async () => {
    const adminBefore = await lamportsOf(admin.address);
    const signature = await migrate(LEGACY_POOL_ID);
    const fee = await feeOf(signature);

    const grown = await account(legacy.address);
    assert.equal(grown.data.length, POOL_LEN);
    assert.deepEqual(grown.data.subarray(0, 155), legacy.data.subarray(0, 155));
    assert.deepEqual(grown.data.subarray(155, 195), Buffer.alloc(40));
    assert.deepEqual(grown.data.subarray(195, 226), legacy.data.subarray(155, 186));
    assert.deepEqual(grown.data.subarray(226, 246), Buffer.alloc(20));
    assert.deepEqual(grown.data.subarray(246), legacy.data.subarray(186, LEGACY_POOL_LEN));
    assert.deepEqual(decodePool(grown.data), grownPoolFields(legacy.fields));

    assert.equal(grown.lamports, BigInt(rentExempt(POOL_LEN)));
    assert.equal(
      adminBefore - (await lamportsOf(admin.address)),
      BigInt(rentExempt(POOL_LEN) - rentExempt(LEGACY_POOL_LEN)) + fee,
    );
  });

  it("changes nothing on a second call", async () => {
    const before = await account(legacy.address);
    const adminBefore = await lamportsOf(admin.address);
    const fee = await feeOf(await migrate(LEGACY_POOL_ID));
    assert.deepEqual(await account(legacy.address), before);
    assert.equal(adminBefore - (await lamportsOf(admin.address)), fee);
  });

  it("changes nothing on a pool created at the current length", async () => {
    const before = await account(current.pool);
    assert.equal(before.data.length, POOL_LEN);
    await migrate(current.poolId);
    assert.deepEqual(await account(current.pool), before);
  });

  it("refuses a Pool-tagged account at the pre-escrow length", async () => {
    const { address } = await legacyPoolAccount(MALFORMED_POOL_ID);
    const before = await account(address);
    assert.equal(before.data.length, MALFORMED_POOL_LEN, "the validator did not load the malformed pool fixture");
    await expectAnchorError(migrate(MALFORMED_POOL_ID), ACCOUNT_DID_NOT_DESERIALIZE);
    assert.deepEqual(await account(address), before);
  });

  it("refuses a program account of another type", async () => {
    const before = await account(current.topTier);
    await expectAnchorError(migrateAt(current.topTier), ACCOUNT_DISCRIMINATOR_MISMATCH);
    assert.deepEqual(await account(current.topTier), before);
  });

  it("refuses an account another program owns", async () => {
    const before = await account(current.treasury03);
    await expectAnchorError(migrateAt(current.treasury03), ACCOUNT_OWNED_BY_WRONG_PROGRAM);
    assert.deepEqual(await account(current.treasury03), before);
  });

  it("the grown pool is read by an instruction that takes it, and its write lands at the new offset", async () => {
    const original = grownPoolFields(legacy.fields);
    const pause = (deposits: boolean, rolls: boolean, reason: number) =>
      setPause({ administrator: admin.address, poolId: LEGACY_POOL_ID, deposits, rolls, reason });
    await sendIxs(connection, admin, [await pause(false, false, 9)]);
    assert.deepEqual(decodePool((await account(legacy.address)).data), {
      ...original,
      depositsPaused: false,
      rollsPaused: false,
      pauseReason: 9,
    });

    await sendIxs(connection, admin, [
      await pause(original.depositsPaused, original.rollsPaused, original.pauseReason),
    ]);
    assert.deepEqual(decodePool((await account(legacy.address)).data), original);
  });

  it("covers exactly the matrix rows this suite is answerable for", () => {
    assert.deepEqual(
      EXECUTED_ROWS.filter((r) => r.suite === "migrate-pool").map((r) => `${r.instruction}.${r.signer}`),
      ["migrate_pool.administrator"],
    );
  });
});

describe("update_config — the escrow bid and the buyback rate bound each other", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let a: PoolAccounts & { poolId: number };
  let b: PoolAccounts & { poolId: number };
  let c: PoolAccounts & { poolId: number };

  async function poolOf(accounts: PoolAccounts) {
    return decodePool((await fetchAccountData(connection, accounts.pool))!);
  }

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  async function configure(poolId: number, update: ConfigUpdate): Promise<string> {
    return sendIxs(connection, admin, [
      await updateConfig({ administrator: admin.address, poolId, update, demoted: [] }),
    ]);
  }

  async function newPool(poolId: number, usdcMint: Address) {
    const { signer: weightIndexSigner, instruction } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], instruction)]);
    const accounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex: weightIndexSigner.address,
      usdcMint,
      args: launchProfile(),
    });
    return { ...accounts, poolId };
  }

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    const protocolState = await ensureProtocol(connection, admin);
    a = await newPool(protocolState.poolCounter, protocolState.usdcMint);
    b = await newPool(protocolState.poolCounter + 1, protocolState.usdcMint);
    c = await newPool(protocolState.poolCounter + 2, protocolState.usdcMint);
  });

  it("moves one pool's bid without touching another's, and announces it", async () => {
    assert.equal((await poolOf(a)).escrowBidBps, 0, "a new pool starts with no bid");
    await configure(a.poolId, { kind: "EscrowBidBps", value: 9_250 });
    await configure(b.poolId, { kind: "EscrowBidBps", value: 9_250 });

    const signature = await configure(a.poolId, { kind: "EscrowBidBps", value: 9_500 });

    assert.equal((await poolOf(a)).escrowBidBps, 9_500);
    assert.equal((await poolOf(b)).escrowBidBps, 9_250);
    const changed = (await eventsOf(signature)).filter((e) => e.name === "ConfigChanged");
    assert.equal(changed.length, 1);
    assert.deepEqual(
      changed.map((e) => e.data).map(({ pool, param, old, new: next }) => ({ pool, param, old, next })),
      [
        {
          pool: a.pool,
          param: "escrow_bid_bps",
          old: { kind: "Bps", value: 9_250 },
          next: { kind: "Bps", value: 9_500 },
        },
      ],
    );
  });

  it("refuses a bid below the pool's rate, above 10,000, or 0 once set, and moves nothing", async () => {
    for (const bid of [8_499, 10_001, 0]) {
      await expectAnchorError(configure(b.poolId, { kind: "EscrowBidBps", value: bid }), ESCROW_BID_OUT_OF_RANGE);
    }
    const after = await poolOf(b);
    assert.equal(after.escrowBidBps, 9_250);
    assert.equal(after.buybackRateBps, 8_500);
  });

  it("refuses a rate above a set bid, and above 10,000 with or without a bid", async () => {
    await expectAnchorError(configure(b.poolId, { kind: "BuybackRateBps", value: 9_251 }), BUYBACK_RATE_OUT_OF_RANGE);
    await expectAnchorError(configure(b.poolId, { kind: "BuybackRateBps", value: 10_001 }), BUYBACK_RATE_OUT_OF_RANGE);
    await expectAnchorError(configure(c.poolId, { kind: "BuybackRateBps", value: 10_001 }), BUYBACK_RATE_OUT_OF_RANGE);
    assert.equal((await poolOf(b)).buybackRateBps, 8_500);
    assert.equal((await poolOf(c)).buybackRateBps, 8_500);

    // The successes the refusals cannot stand in for: up to the bid, and up to 10,000 with none.
    await configure(b.poolId, { kind: "BuybackRateBps", value: 9_250 });
    await configure(c.poolId, { kind: "BuybackRateBps", value: 10_000 });
    assert.equal((await poolOf(b)).buybackRateBps, 9_250);
    assert.equal((await poolOf(c)).buybackRateBps, 10_000);
  });
});
