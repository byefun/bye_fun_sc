// `close_batch` — the batch account's own disposal, and the only instruction in the pipeline whose
// whole effect is reclaiming rent.
//
// It is guarded on three things: the batch's liability must already be resolved one way or the
// other, the signer must be the roller who paid for it or the Operator, and the rent goes to the
// signer. The batch is read by offset, so it closes in either layout — the 1,100-byte one with
// three blank slots and today's 1,360-byte one — and the close is done by hand: lamports to the
// signer, the account handed to the system program and emptied.
//
// The first batch carries the in-flight refusals, the wrong-party refusals, then the Operator's
// close; a second is closed by its own roller; and a genesis-loaded 1,100-byte batch is closed by
// the Operator. Rent and fee are read from each close's own transaction metadata.

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { AccountRole, addSignersToInstruction, type Address } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  lamports,
  loadOperator,
  loadSuiteAdmin,
  withAccountAt,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { admitPoolCollection, createPool, smallTierProfile } from "../helpers/pools.js";
import { assertEveryPoolIdle, drainTreasury04 } from "../helpers/treasuries.js";
import { ensureProtocol } from "../helpers/protocol.js";
import {
  createPnftCollection,
  mintPlainNonFungiblePnft,
  type MintedAsset,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { NO_POSITION, settleDrawnRoll } from "../helpers/roll.js";
import { VRF_QUEUE_FIXTURE } from "../helpers/vrf-queue.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { VRF_PROGRAM } from "../../scripts/constants.js";
import { landed } from "../helpers/landed.js";
import { updateVrfConfig } from "../../scripts/lib/admin.js";
import { approveDeposit, deposit } from "../../scripts/lib/deposit.js";
import { closeBatch, commitRolls, testResolveBatch } from "../../scripts/lib/roll.js";
import { LEGACY_ROLL_BATCH_LEN, decodePool, decodeRollBatch } from "../../scripts/lib/decoders.js";
import { derivePda, seedFromPubkey, seedFromU64Le } from "../../scripts/lib/anchor.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { blankWeight, classifyDraw, expandDraw } from "../../scripts/verify-batch.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";
import {
  LEGACY_BATCH_ID,
  LEGACY_COMMITTED_BATCH_ID,
  LEGACY_ROLLER_BATCH_ID,
  MISPLACED_BATCH_ID,
  legacyBatchAccount,
  legacyBatchRoller,
  legacyCommittedBatchAccount,
  legacyRollerBatchAccount,
  misplacedLegacyBatchAccount,
} from "../../scripts/lib/legacy-batch-fixture.js";

const POSITIONS = 3;

/** `BatchNotClosable` — the batch still owes a terminal to at least one of its rolls. */
const BATCH_NOT_CLOSABLE = 6211;

/** `Unauthorized` — the signer is neither the batch's roller nor the Operator, or the roller account
 * is not the roller the batch recorded. */
const UNAUTHORIZED = 6503;

/** Anchor's `ConstraintSeeds`, `AccountDidNotDeserialize` and `AccountOwnedByWrongProgram`. */
const CONSTRAINT_SEEDS = 2006;
const ACCOUNT_DID_NOT_DESERIALIZE = 3003;
const ACCOUNT_OWNED_BY_WRONG_PROGRAM = 3007;

/** Anchor's `ConstraintMut`: a writable account arrived read-only. */
const CONSTRAINT_MUT = 2000;

/** `batch`'s position in `close_batch`'s account list. */
const BATCH_INDEX = 2;

describe("close_batch — rent goes to the signer, and only once the batch owes nothing", () => {
  async function lamportsOf(address: Address): Promise<bigint> {
    return (await connection.rpc.getBalance(address, { commitment: "confirmed" }).send()).value;
  }

  async function rentOf(address: Address): Promise<bigint> {
    return (await connection.rpc.getAccountInfo(address, { encoding: "base64" }).send()).value!.lamports;
  }

  /**
   * The first fill byte from `start` whose roll 0 lands on a real card. A blank Treasury 04 can
   * cover settles through the swap venue, which the localnet does not deploy.
   */
  function realCardRandomness(state: ReturnType<typeof decodePool>, start: number): Uint8Array {
    const total = state.wReal + blankWeight(state.blanks, state.blankFaces);
    for (let fill = start; fill < start + 256; fill += 1) {
      const candidate = new Uint8Array(32).fill(fill % 256);
      if (classifyDraw(expandDraw(candidate, 0, total).r, state.blanks, state.blankFaces).kind === "real") {
        return candidate;
      }
    }
    throw new Error("no fill byte draws a real card");
  }

  /** Commits, resolves and settles a one-roll batch on a real card, returning its id and address. */
  async function completedBatch(fill: number): Promise<{ id: bigint; address: Address }> {
    const state = decodePool((await fetchAccountData(connection, pool))!);
    const id = state.batchCounter;
    await drainTreasury04({ connection, administrator: admin, operator });
    await sendIxs(connection, roller, [
      await commitRolls({
        roller: roller.address,
        poolId,
        batchCounter: id,
        rollerUsdc,
        usdcMint,
        oracleQueue: VRF_QUEUE_FIXTURE,
        n: 1,
      }),
    ]);
    await sendIxs(connection, operator, [
      await testResolveBatch({
        operator: operator.address,
        pool,
        batchId: id,
        randomness: realCardRandomness(state, fill),
      }),
    ]);
    const drawn = await settleDrawnRoll(
      { connection, operator, pool, protocolConfig, weightIndex, roller: roller.address, rollerUsdc, assets },
      id,
      0,
      assets.map((_, i) => i).filter((i) => !withdrawn.includes(i)),
    );
    if (drawn >= 0) withdrawn.push(drawn);
    const { address } = await derivePda(["batch", seedFromPubkey(pool), seedFromU64Le(id)]);
    assert.equal(decodeRollBatch((await fetchAccountData(connection, address))!).state, "Complete");
    return { id, address };
  }

  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let roller: TransactionSigner;
  let stranger: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let usdcMint: Address;
  let rollerUsdc: Address;
  let protocolConfig: Address;
  let weightIndex: Address;
  let batchId: bigint;
  let batch: Address;
  const assets: MintedAsset[] = [];
  /** Indices into `assets` a settled roll has already paid out, so no later draw can name them. */
  const withdrawn: number[] = [];

  after(async () => {
    await assertEveryPoolIdle(connection);
  });

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    roller = await fundedWallet(connection, 20);
    stranger = await fundedWallet(connection, 20);

    const protocolState = await ensureProtocol(connection, admin);
    usdcMint = protocolState.usdcMint;
    poolId = protocolState.poolCounter;
    protocolConfig = protocolState.protocolConfig;

    await sendIxs(connection, admin, [
      await updateVrfConfig({
        administrator: admin.address,
        vrfProgram: VRF_PROGRAM,
        oracleQueue: VRF_QUEUE_FIXTURE,
        swapPool: SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: 100,
      }),
    ]);

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [
      addSignersToInstruction([admin, weightIndexSigner], createWiIx),
    ]);
    weightIndex = weightIndexSigner.address;

    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: { ...smallTierProfile() },
    });
    pool = poolAccounts.pool;

    const collection: PnftCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
      standards: 0b011,
    });

    for (let i = 0; i < POSITIONS; i += 1) {
      const asset = await mintPlainNonFungiblePnft({
        connection,
        payer: admin,
        authority: admin,
        owner: roller.address,
        collection,
      });
      assets.push(asset);

      await sendIxs(connection, roller, [
        await deposit({
          depositor: roller.address,
          poolId,
          collection: collection.mint,
          nftMint: asset.mint,
          depositorToken: asset.tokenAccount,
          metadata: asset.metadata,
          masterEdition: asset.masterEdition,
          depositorTokenRecord: null,
          positionTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
        }),
      ]);

      await sendIxs(connection, operator, [
        await approveDeposit({
          operator: operator.address,
          poolId,
          nftMint: asset.mint,
          depositor: roller.address,
          weightIndex,
          displaced: null,
          value: 20_000_000n,
          observedAt: BigInt(Math.floor(Date.now() / 1000)),
          lockUntil: 0n,
        }),
      ]);
    }

    await connection.mintTokens(usdcMint, admin, 1_000_000_000n, roller.address, false);
    rollerUsdc = await getTokenAccountAddress(roller.address, usdcMint, false);

    const state = decodePool((await fetchAccountData(connection, pool))!);
    batchId = state.batchCounter;
    await drainTreasury04({ connection, administrator: admin, operator });
    await sendIxs(connection, roller, [
      await commitRolls({
        roller: roller.address,
        poolId,
        batchCounter: batchId,
        rollerUsdc,
        usdcMint,
        oracleQueue: VRF_QUEUE_FIXTURE,
        n: 1,
      }),
    ]);
    ({ address: batch } = await derivePda([
      "batch",
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]));
  });

  it("a batch still awaiting its randomness cannot be closed", async () => {
    const committed = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(committed.state, "Committed", "the batch is not in flight, so this row is vacuous");

    await expectAnchorError(
      sendIxs(connection, roller, [
        await closeBatch({ signer: roller.address, roller: roller.address, pool, batchId }),
      ]),
      BATCH_NOT_CLOSABLE,
    );
  });

  // Resolved is not a terminal either: the randomness has arrived but no roll has been paid or
  // refunded, so the liability the batch account represents is still outstanding. A state test
  // that accepted anything past `Committed` would pass the row above and fail here.
  it("a resolved batch cannot be closed either — settlement is what discharges it", async () => {
    await sendIxs(connection, operator, [
      await testResolveBatch({
        operator: operator.address,
        pool,
        batchId,
        randomness: new Uint8Array(32).fill(11),
      }),
    ]);
    const resolved = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(resolved.state, "Resolved");

    await expectAnchorError(
      sendIxs(connection, roller, [
        await closeBatch({ signer: roller.address, roller: roller.address, pool, batchId }),
      ]),
      BATCH_NOT_CLOSABLE,
    );
  });

  it("a completed batch is still refused to anyone but its roller or the Operator, and its rent cannot be redirected", async () => {
    const drawn = await settleDrawnRoll(
      { connection, operator, pool, protocolConfig, weightIndex, roller: roller.address, rollerUsdc, assets },
      batchId,
      0,
    );
    assert.ok(drawn === NO_POSITION || drawn >= 0, "the settlement did not report a draw");
    if (drawn >= 0) withdrawn.push(drawn);

    const completed = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(completed.state, "Complete", "the batch did not complete on its only roll");

    // A stranger may not close it, whether naming itself or the true roller in the roller slot; and
    // neither the roller nor the Operator may name any wallet but the batch's roller there.
    const wallets = [roller.address, operator.address, stranger.address];
    const heldBefore = await lamportsOf(batch);
    const walletsBefore = await Promise.all(wallets.map(lamportsOf));
    for (const [signer, recipient] of [
      [stranger, stranger.address],
      [stranger, roller.address],
      [roller, stranger.address],
      [operator, operator.address],
      [operator, stranger.address],
    ] as const) {
      await expectAnchorError(
        sendIxs(connection, signer, [
          await closeBatch({ signer: signer.address, roller: recipient, pool, batchId }),
        ]),
        UNAUTHORIZED,
      );
    }
    assert.notEqual(
      await fetchAccountData(connection, batch),
      null,
      "the refused close removed the account anyway",
    );
    assert.equal(await lamportsOf(batch), heldBefore, "a refused close moved the batch's lamports");
    const walletsAfter = await Promise.all(wallets.map(lamportsOf));
    wallets.forEach((wallet, i) => {
      // A refused transaction still lands, so its fee payer pays; nobody may gain a lamport.
      assert.ok(walletsAfter[i]! <= walletsBefore[i]!, `${wallet} gained lamports from a refused close`);
      assert.ok(walletsBefore[i]! - walletsAfter[i]! < 1_000_000n, `${wallet} lost more than fees`);
    });
  });

  // The runtime lets a program credit only a writable account, so the signer meta must be
  // writable; another wallet pays the fee here so the message does not make it writable anyway.
  it("a read-only signer meta is refused at ConstraintMut and nothing moves", async () => {
    const built = await closeBatch({ signer: operator.address, roller: roller.address, pool, batchId });
    const accounts = [...built.accounts!];
    accounts[0] = { ...accounts[0]!, role: AccountRole.READONLY_SIGNER };
    const readOnly = addSignersToInstruction([operator], { ...built, accounts });
    const held = await lamportsOf(batch);
    const operatorBefore = await lamportsOf(operator.address);
    const rollerBefore = await lamportsOf(roller.address);

    await expectAnchorError(sendIxs(connection, stranger, [readOnly]), CONSTRAINT_MUT);

    assert.notEqual(await fetchAccountData(connection, batch), null, "the refused close removed the account");
    assert.equal(await lamportsOf(batch), held);
    assert.equal(await lamportsOf(operator.address), operatorBefore);
    assert.equal(await lamportsOf(roller.address), rollerBefore);
  });

  it("the Operator closes the completed batch and receives exactly its rent less the fee", async () => {
    const data = await fetchAccountData(connection, batch);
    assert.equal(data!.length, 1_360, "the batch is not in the current layout");
    const rent = await rentOf(batch);

    const signature = await sendIxs(connection, operator, [
      await closeBatch({ signer: operator.address, roller: roller.address, pool, batchId }),
    ]);

    assert.equal(await fetchAccountData(connection, batch), null, "the batch account still exists");
    const tx = await landed(connection, signature);
    assert.equal(tx.change(operator.address), rent - tx.fee, "the Operator did not gain exactly the rent less the fee");
    assert.equal(tx.change(roller.address), 0n, "the roller gained from a close the Operator signed");
    assert.equal(tx.change(batch), -rent);
    const closed = tx.events.find((e) => e.name === "BatchClosed");
    assert.equal(closed?.name === "BatchClosed" && closed.data.rentRecipient, operator.address);
  });

  it("the roller closes its own completed batch and receives its rent less the fee, once", async () => {
    const second = await completedBatch(12);
    const rent = await rentOf(second.address);
    const operatorBefore = await lamportsOf(operator.address);

    const signature = await sendIxs(connection, roller, [
      await closeBatch({ signer: roller.address, roller: roller.address, pool, batchId: second.id }),
    ]);

    assert.equal(await fetchAccountData(connection, second.address), null, "the batch account still exists");

    // The roller fills both the signer and the roller slot; the batch is credited to it once, not
    // twice, and the Operator is not touched.
    const tx = await landed(connection, signature);
    assert.equal(tx.change(roller.address), rent - tx.fee);
    assert.equal(await lamportsOf(operator.address), operatorBefore);
    const closed = tx.events.find((e) => e.name === "BatchClosed");
    assert.equal(closed?.name === "BatchClosed" && closed.data.rentRecipient, roller.address);
  });

  it("the Operator closes a finished batch written in the 1,100-byte layout, rent to the Operator", async () => {
    const legacy = await legacyBatchAccount();
    const loaded = await fetchAccountData(connection, legacy.address);
    assert.equal(loaded?.length, LEGACY_ROLL_BATCH_LEN, "the validator did not load the legacy batch fixture");
    assert.deepEqual(Buffer.from(loaded!), legacy.data);

    await expectAnchorError(
      sendIxs(connection, stranger, [
        await closeBatch({ signer: stranger.address, roller: legacy.roller, pool: legacy.pool, batchId: LEGACY_BATCH_ID }),
      ]),
      UNAUTHORIZED,
    );

    assert.notEqual(await fetchAccountData(connection, legacy.address), null, "the refused close removed the batch");
    const rent = await rentOf(legacy.address);
    const signature = await sendIxs(connection, operator, [
      await closeBatch({ signer: operator.address, roller: legacy.roller, pool: legacy.pool, batchId: LEGACY_BATCH_ID }),
    ]);
    assert.equal(await fetchAccountData(connection, legacy.address), null, "the legacy batch still exists");
    const tx = await landed(connection, signature);
    assert.equal(tx.change(operator.address), rent - tx.fee);
    assert.equal(tx.change(legacy.roller), 0n, "the batch's roller gained from a close the Operator signed");
  });

  it("the roller closes its own finished 1,100-byte batch and receives its rent less the fee", async () => {
    const keyedRoller = await legacyBatchRoller();
    await connection.airdropIfRequired(keyedRoller.address, lamports(2_000_000_000n), lamports(1_000_000_000n));
    const legacy = await legacyRollerBatchAccount();
    const loaded = await fetchAccountData(connection, legacy.address);
    assert.equal(loaded?.length, LEGACY_ROLL_BATCH_LEN, "the validator did not load the keyed legacy batch fixture");
    assert.deepEqual(Buffer.from(loaded!), legacy.data);

    const rent = await rentOf(legacy.address);
    const signature = await sendIxs(connection, keyedRoller, [
      await closeBatch({
        signer: keyedRoller.address,
        roller: keyedRoller.address,
        pool: legacy.pool,
        batchId: LEGACY_ROLLER_BATCH_ID,
      }),
    ]);
    assert.equal(await fetchAccountData(connection, legacy.address), null, "the legacy batch still exists");
    const tx = await landed(connection, signature);
    assert.equal(tx.change(keyedRoller.address), rent - tx.fee);
  });

  it("a 1,100-byte batch still in progress is refused to its roller and to the Operator", async () => {
    const keyedRoller = await legacyBatchRoller();
    await connection.airdropIfRequired(keyedRoller.address, lamports(2_000_000_000n), lamports(1_000_000_000n));
    const committed = await legacyCommittedBatchAccount();
    const before = await fetchAccountData(connection, committed.address);
    assert.equal(before?.length, LEGACY_ROLL_BATCH_LEN, "the validator did not load the committed legacy batch fixture");

    for (const signer of [keyedRoller, operator]) {
      await expectAnchorError(
        sendIxs(connection, signer, [
          await closeBatch({
            signer: signer.address,
            roller: keyedRoller.address,
            pool: committed.pool,
            batchId: LEGACY_COMMITTED_BATCH_ID,
          }),
        ]),
        BATCH_NOT_CLOSABLE,
      );
    }
    assert.deepEqual(Buffer.from((await fetchAccountData(connection, committed.address))!), committed.data);
  });

  it("a finished batch at another batch's address is refused, and nothing moves", async () => {
    const keyedRoller = await legacyBatchRoller();
    await connection.airdropIfRequired(keyedRoller.address, lamports(2_000_000_000n), lamports(1_000_000_000n));
    const misplaced = await misplacedLegacyBatchAccount();
    const before = await fetchAccountData(connection, misplaced.address);
    assert.equal(before?.length, LEGACY_ROLL_BATCH_LEN, "the validator did not load the misplaced batch fixture");

    await expectAnchorError(
      sendIxs(connection, keyedRoller, [
        await closeBatch({
          signer: keyedRoller.address,
          roller: keyedRoller.address,
          pool: misplaced.pool,
          batchId: MISPLACED_BATCH_ID,
        }),
      ]),
      CONSTRAINT_SEEDS,
    );
    assert.deepEqual(Buffer.from((await fetchAccountData(connection, misplaced.address))!), misplaced.data);
  });

  it("a program account of another type, and an account another program owns, are refused in the batch slot", async () => {
    const ix = await closeBatch({ signer: operator.address, roller: roller.address, pool, batchId: 0n });
    await expectAnchorError(
      sendIxs(connection, operator, [withAccountAt(ix, BATCH_INDEX, pool)]),
      ACCOUNT_DID_NOT_DESERIALIZE,
    );
    await expectAnchorError(
      sendIxs(connection, operator, [withAccountAt(ix, BATCH_INDEX, rollerUsdc)]),
      ACCOUNT_OWNED_BY_WRONG_PROGRAM,
    );
    assert.notEqual(await fetchAccountData(connection, pool), null);
  });
});
