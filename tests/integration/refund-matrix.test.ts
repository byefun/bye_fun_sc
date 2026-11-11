// The refund matrix, executed — every unsettleable case driven to its stated terminal, plus the
// one case the matrix asserts cannot happen.
//
// The guarantee behind the matrix is that a paid roll always ends settled or refunded and is never
// silently dropped. A matrix that only *lists* the cases proves nothing about that; each row has to
// reach its terminal against a real validator, and the unreachable row has to be asserted as a
// rejection rather than left out, because an absence with no assertion behind it is not evidence.
//
// Every row, and the one case the matrix asserts cannot happen:
//
//   | case                      | detected at        | terminal                              |
//   |---------------------------|--------------------|---------------------------------------|
//   | VRF timeout               | `recover_batch`    | whole batch `Recovered`               |
//   | Treasury 04 short         | `settle_roll`      | that roll `Refunded`, blank persists  |
//   | collateral unreachable    | `settle_roll_core` | that roll `Refunded`, position seized |
//   | withdrawn before settled  | `withdraw`         | unreachable — refused outright        |
//
// Each row asserts its own precondition held — the treasury was empty, the
// asset was gone — so no refund it observes can be attributed to something else.

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { admitPoolCollection, createPool, smallTierProfile } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import {
  createPnftCollection,
  mintPlainNonFungiblePnft,
  type MintedAsset,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { burnCoreAsset, createCoreCollection, mintCoreAsset } from "../helpers/core-fixtures.js";
import { depositCore } from "../../scripts/lib/deposit.js";
import {
  purgeExpiredRequests,
  VRF_QUEUE_FIXTURE,
  VRF_QUEUE_TTL_SECONDS,
} from "../helpers/vrf-queue.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import {
  ACCOUNT_NOT_ENOUGH_KEYS,
  DRAWN_POSITION_MISMATCH,
  NO_POSITION,
  settleDrawnRoll as settleDrawnRollAgainst,
} from "../helpers/roll.js";
import { VRF_PROGRAM } from "../../scripts/constants.js";
import { deriveSharedTreasuries, updateVrfConfig } from "../../scripts/lib/admin.js";
import { assertEveryPoolIdle, drainTreasury04 } from "../helpers/treasuries.js";
import { approveDeposit, deposit } from "../../scripts/lib/deposit.js";
import { withdraw } from "../../scripts/lib/exit.js";
import { landed } from "../helpers/landed.js";
import {
  closeBatch,
  commitRolls,
  recoverBatch,
  settleRoll,
  settleRollCore,
  testResolveBatch,
} from "../../scripts/lib/roll.js";
import { decodePool, decodePosition, decodeProtocolConfig, decodeRollBatch } from "../../scripts/lib/decoders.js";
import { derivePda, seedFromPubkey, seedFromU64Le } from "../../scripts/lib/anchor.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";
import { deriveAssociatedTokenAddress } from "../../scripts/lib/anchor.js";

const POSITIONS = 3;

/** `RollBatchInFlight` — the weight-freeze guard every exit reaches. */
const ROLL_BATCH_IN_FLIGHT = 6200;

/**
 * The reason persisted on a roll a VRF timeout refunded. `recover_batch` reuses
 * `RecoveryTimeoutNotElapsed`'s own wire number as the branch identifier rather than inventing a
 * second number for the same condition.
 */
const VRF_TIMEOUT_REFUND_REASON = 6208;

/** `recover_batch` refuses until this many slots have passed since the request. */
const VRF_TIMEOUT_SLOTS = 240n;

/** `T04Shortfall` — Treasury 04 could not cover a drawn blank's face. */
const T04_SHORTFALL = 6601;

/** `BatchNotResolved` — a delivery for a batch that is no longer awaiting one. */
const BATCH_NOT_RESOLVED = 6206;

/** `CollateralAbsent` — the escrowed asset is gone, burned or moved out by a third party. */
const COLLATERAL_ABSENT = 6307;

/** `BlankSettlesElsewhere` — a blank drawn on the Core variant. Blanks move no NFT, so they
 *  settle through `settle_roll` whichever variant the pool's cards are. */
const BLANK_SETTLES_ELSEWHERE = 6213;

function queueItemCount(data: Buffer): number {
  return data.readUInt32LE(8);
}

async function batchPda(pool: Address, batchId: bigint): Promise<Address> {
  const { address } = await derivePda(["batch", seedFromPubkey(pool), seedFromU64Le(batchId)]);
  return address;
}

describe("refund matrix — every unsettleable case reaches its stated terminal", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let roller: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;
  let usdcMint: Address;
  let rollerUsdc: Address;
  let assets: MintedAsset[] = [];
  /** Indices into `assets` whose position is still open; settling cases remove what they draw. */
  const live: number[] = [];
  let collection: PnftCollection;
  let protocolConfig: Address;
  /** The shared queue's depth before this file's first commit. Every other test file that
   *  opens a batch adds to the same account, so only the delta is this file's to assert. */
  let queueBeforeCommit = 0;
  /** The batch the VRF-timeout row recovered — the late-callback row below re-uses it. */
  let recoveredBatchId: bigint;

  after(async () => {
    await assertEveryPoolIdle(connection);
  });

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    roller = await fundedWallet(connection, 20);

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

    collection = await createPnftCollection({ connection, payer: admin, authority: admin });
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
      live.push(assets.length - 1);

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
  });

  /**
   * Settles roll `rollIndex` against this file's fixtures.
   *
   * The position-less candidate carries no venue accounts, and that is what keeps the blank rows
   * reachable here: a drawn blank decides its terminal from Treasury 04's balance *before* the swap
   * leg dereferences any of them, so a treasury that cannot cover the face refunds without the
   * venue being named at all — and this validator never deploys the venue program.
   */
  function settleDrawnRoll(
    batchId: bigint,
    rollIndex: number,
    live: number[] = assets.map((_, i) => i),
  ): Promise<number> {
    return settleDrawnRollAgainst(
      {
        connection,
        operator,
        pool,
        protocolConfig,
        weightIndex,
        roller: roller.address,
        rollerUsdc,
        assets,
      },
      batchId,
      rollIndex,
      live,
    );
  }

  // ── the two rows ───────────────────────────────────────────────────────────────────────────

  it("withdrawn before settled — unreachable: withdraw is refused while a batch is open", async () => {
    const before = decodePool((await fetchAccountData(connection, pool))!);
    const batchId = before.batchCounter;
    queueBeforeCommit = queueItemCount((await fetchAccountData(connection, VRF_QUEUE_FIXTURE))!);

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

    const opened = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(opened.openBatches, 1, "the batch did not open — the refusal below would be vacuous");

    // The position is otherwise withdrawable: it is Active, unlocked, and its depositor is
    // signing. The only thing standing between it and an exit is the open batch — which is what
    // makes this a test of the guard rather than of some unrelated precondition.
    const asset = assets[0]!;
    await expectAnchorError(
      sendIxs(connection, roller, [
        await withdraw({
          depositor: roller.address,
          poolId,
          nftMint: asset.mint,
          weightIndex,
          depositorUsdc: rollerUsdc,
          depositorToken: asset.tokenAccount,
          metadata: asset.metadata,
          masterEdition: asset.masterEdition,
          positionTokenRecord: null,
          depositorTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
          successor: null,
        }),
      ]),
      ROLL_BATCH_IN_FLIGHT,
    );
  });

  it("VRF timeout — the whole batch reaches Recovered and every roll refunds", async () => {
    const opened = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(opened.openBatches, 1, "the batch from the previous case is not still open");
    const batchId = opened.batchCounter - 1n;
    const batch = await batchPda(pool, batchId);

    const committed = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(committed.state, "Committed");
    const refundDue = committed.sPrice * BigInt(committed.n);

    // The queue is shared with every other batch this suite commits, so the assertion is that
    // *this* commit added an item — never that the queue holds exactly one.
    assert.equal(
      queueItemCount((await fetchAccountData(connection, VRF_QUEUE_FIXTURE))!),
      queueBeforeCommit + 1,
      "this batch's request never reached the queue, so there is nothing for the purge to remove",
    );

    // Recovery is refused while the request is live, no matter how much time passes: a refund
    // issued against a live item races a callback with unbounded lateness. Two conditions have to
    // come true — the provider's wall-clock TTL, so the purge removes the item, and the program's
    // own 240-slot timeout — so both are polled rather than assumed from one sleep, and
    // `recover_batch`'s own queue scan is what decides the first of them. A success here is
    // therefore evidence the request is genuinely gone, which no count of a shared queue could be.
    const balanceBefore = (await connection.getTokenAccountBalance({ tokenAccount: rollerUsdc }))
      .amount as bigint;

    const deadline = Date.now() + (VRF_QUEUE_TTL_SECONDS + 120) * 1000;
    let didRecover = false;
    let lastError = "never attempted";
    while (Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 5_000));
      try {
        await sendIxs(connection, admin, [purgeExpiredRequests()]);
      } catch (e) {
        // Before the TTL nothing is expired and the call removes nothing. Carried into the
        // assertion below rather than dropped: a swallowed error is indistinguishable from
        // "not expired yet", and the two have entirely different fixes.
        lastError = `purge: ${e instanceof Error ? e.message : String(e)}`;
      }
      try {
        await sendIxs(connection, operator, [
          await recoverBatch({
            operator: operator.address,
            pool,
            batchId,
            rollerUsdc,
            oracleQueue: VRF_QUEUE_FIXTURE,
          }),
        ]);
        didRecover = true;
        break;
      } catch (e) {
        lastError = `recover: ${e instanceof Error ? e.message : String(e)}`;
      }
    }
    assert.ok(didRecover, `the batch never became recoverable; last attempt: ${lastError}`);

    const slot = await connection.rpc.getSlot().send();
    assert.ok(
      BigInt(slot) >= committed.requestSlot + VRF_TIMEOUT_SLOTS,
      `recovery succeeded after only ${BigInt(slot) - committed.requestSlot} slots; it needs ${VRF_TIMEOUT_SLOTS}`,
    );

    const recovered = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(recovered.state, "Recovered");
    recoveredBatchId = batchId;
    for (let i = 0; i < recovered.n; i += 1) {
      const roll = recovered.rolls[i]!;
      assert.equal(roll.status, "Refunded", `roll ${i} did not refund`);
      assert.equal(roll.outcome, "None", `roll ${i} carries an outcome`);
      assert.equal(roll.refundReason, VRF_TIMEOUT_REFUND_REASON, `roll ${i} reason`);
    }

    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.openBatches, 0, "the freeze did not lift");
    assert.equal(after.pendingRollLiability, 0n, "the liability was not released");

    const balanceAfter = (await connection.getTokenAccountBalance({ tokenAccount: rollerUsdc }))
      .amount as bigint;
    assert.equal(
      balanceAfter - balanceBefore,
      refundDue,
      "the roller was not refunded the full price of every roll",
    );
  });

  // A recovery is only sound if the delivery it raced can no longer land. The purge and the
  // absence check make the request unservable; this is the other half — the batch itself refuses a
  // delivery that arrives anyway. The stand-in shares `apply_resolve` with the real callback, so
  // the guard exercised here is the one an actual late delivery would meet.
  it("a callback arriving after recovery is refused and settles nothing", async () => {
    const batch = await batchPda(pool, recoveredBatchId);
    const before = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(before.state, "Recovered", "the batch under test was never recovered");

    await expectAnchorError(
      sendIxs(connection, operator, [
        await testResolveBatch({
          operator: operator.address,
          pool,
          batchId: recoveredBatchId,
          randomness: new Uint8Array(32).fill(9),
        }),
      ]),
      BATCH_NOT_RESOLVED,
    );

    const after = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(after.state, "Recovered", "the refused delivery moved the batch anyway");
    assert.deepEqual(
      Array.from(after.randomness),
      Array.from(before.randomness),
      "the refused delivery wrote its randomness",
    );
    for (let i = 0; i < after.n; i += 1) {
      assert.equal(after.rolls[i]!.status, "Refunded", `roll ${i} left its terminal`);
    }
  });

  it("the Operator closes the Recovered batch and receives its rent less the fee", async () => {
    const batch = await batchPda(pool, recoveredBatchId);
    const rent = (await connection.rpc.getAccountInfo(batch, { encoding: "base64" }).send()).value!.lamports;

    const signature = await sendIxs(connection, operator, [
      await closeBatch({ signer: operator.address, roller: roller.address, pool, batchId: recoveredBatchId }),
    ]);

    assert.equal(await fetchAccountData(connection, batch), null, "the Recovered batch still exists");
    const tx = await landed(connection, signature);
    assert.equal(tx.change(operator.address), rent - tx.fee);
    assert.equal(tx.change(roller.address), 0n);
  });

  it("Treasury 04 short — a drawn blank refunds and the blank set is untouched", async () => {
    const before = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(before.openBatches, 0, "the previous case left a batch open");
    assert.ok(
      before.blanks.some((count) => count > 0),
      "the pool holds no blanks, so no roll in this batch can draw one",
    );
    const batchId = before.batchCounter;

    // Every earlier settlement credited the one shared Treasury 04, so it is swept down to a
    // single base unit first: far below any face this pool draws.
    await drainTreasury04({ connection, administrator: admin, operator });
    const { treasury04 } = await deriveSharedTreasuries();
    const t04Balance = (await connection.getTokenAccountBalance({ tokenAccount: treasury04 }))
      .amount as bigint;
    assert.ok(t04Balance <= 1n, "Treasury 04 holds a balance — the refund below proves nothing");

    // One randomness value expands to n independent draws by roll index, so a batch this wide
    // reaches a blank without re-resolving. Nothing here is chance: the bytes are fixed and the
    // pool state is fixed, so the draw sequence is the same on every run.
    const rolls = 6;
    await sendIxs(connection, roller, [
      await commitRolls({
        roller: roller.address,
        poolId,
        batchCounter: batchId,
        rollerUsdc,
        usdcMint,
        oracleQueue: VRF_QUEUE_FIXTURE,
        n: rolls,
      }),
    ]);
    await sendIxs(connection, operator, [
      await testResolveBatch({
        operator: operator.address,
        pool,
        batchId,
        randomness: new Uint8Array(32).fill(7),
      }),
    ]);

    // A roll that draws a real position closes it, so later rolls must not offer it again.
    let blankRoll = -1;
    let beforeBlankRoll: ReturnType<typeof decodePool> | null = null;
    let afterBlankRoll: ReturnType<typeof decodePool> | null = null;
    for (let i = 0; i < rolls; i += 1) {
      const poolBefore = decodePool((await fetchAccountData(connection, pool))!);
      const drawn = await settleDrawnRoll(batchId, i, live);
      if (drawn === NO_POSITION) {
        if (blankRoll === -1) {
          blankRoll = i;
          beforeBlankRoll = poolBefore;
          afterBlankRoll = decodePool((await fetchAccountData(connection, pool))!);
        }
      } else {
        live.splice(live.indexOf(drawn), 1);
      }
    }
    assert.notEqual(blankRoll, -1, `no roll of ${rolls} drew a blank`);

    const batch = decodeRollBatch((await fetchAccountData(connection, await batchPda(pool, batchId)))!);
    const roll = batch.rolls[blankRoll]!;
    assert.equal(roll.status, "Refunded");
    assert.equal(roll.refundReason, T04_SHORTFALL);

    // "the virtual position stays active" — the blank denomination is not consumed by a roll that
    // could not pay it out, so the blank set is exactly what it was across that one roll. Rolls
    // after it may settle blanks normally once earlier settlements have funded Treasury 04.
    assert.deepEqual(
      afterBlankRoll!.blanks,
      beforeBlankRoll!.blanks,
      "a refunded blank changed the blank set",
    );
    assert.equal(afterBlankRoll!.nReal, beforeBlankRoll!.nReal, "a refunded blank removed a real position");
    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.openBatches, 0, "the batch did not complete");
  });

  it("escrowed collateral unreachable — the roll refunds and the phantom position is force-closed", async () => {
    // A pool of its own: this row's card has to be an MPL Core asset, which settles through
    // `settle_roll_core` rather than `settle_roll`.
    const corePoolId = poolId + 1;
    const thief = await fundedWallet(connection, 20);

    const { signer: coreWiSigner, instruction: coreWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [
      addSignersToInstruction([admin, coreWiSigner], coreWiIx),
    ]);

    const { pool: corePool } = await createPool({
      connection,
      administrator: admin,
      poolId: corePoolId,
      weightIndex: coreWiSigner.address,
      usdcMint,
      args: { ...smallTierProfile() },
    });

    const core = await createCoreCollection({ connection, payer: admin, name: "refund collateral" });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId: corePoolId,
      collection: core.collection,
      standards: 0b100,
    });

    // One position, so `required_tier_len` is min(tier_size, 1) = 1 and every real draw in this
    // pool selects it. The burn delegate is held by a third party and never by the depositor or
    // this program — that is what makes the removal below something the pool cannot prevent.
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection: core.collection,
      collectionAuthority: admin,
      owner: roller.address,
      plugins: [{ kind: "permanentBurnDelegate", authority: thief.address }],
    });

    await sendIxs(connection, roller, [
      await depositCore({
        depositor: roller.address,
        poolId: corePoolId,
        collection: core.collection,
        asset,
      }),
    ]);
    await sendIxs(connection, operator, [
      await approveDeposit({
        operator: operator.address,
        poolId: corePoolId,
        nftMint: asset,
        depositor: roller.address,
        weightIndex: coreWiSigner.address,
        displaced: null,
        value: 20_000_000n,
        observedAt: BigInt(Math.floor(Date.now() / 1000)),
        lockUntil: 0n,
      }),
    ]);

    const before = decodePool((await fetchAccountData(connection, corePool))!);
    assert.equal(before.nReal, 1, "the Core position did not activate");
    const batchId = before.batchCounter;
    const rolls = 6;

    await drainTreasury04({ connection, administrator: admin, operator });
    await sendIxs(connection, roller, [
      await commitRolls({
        roller: roller.address,
        poolId: corePoolId,
        batchCounter: batchId,
        rollerUsdc,
        usdcMint,
        oracleQueue: VRF_QUEUE_FIXTURE,
        n: rolls,
      }),
    ]);
    await sendIxs(connection, operator, [
      await testResolveBatch({
        operator: operator.address,
        pool: corePool,
        batchId,
        randomness: new Uint8Array(32).fill(3),
      }),
    ]);

    // The removal lands after the draw is fixed and before the settlement reads it, which is the
    // only window in which this row's cause can occur at all: a card removed earlier would never
    // have been drawn, and one removed later would already have been transferred out.
    await burnCoreAsset({
      connection,
      payer: thief,
      asset,
      collection: core.collection,
      authority: thief,
    });

    const cfg = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);

    const missed = (e: unknown): boolean => {
      const message = e instanceof Error ? e.message : String(e);
      return (
        message.includes(String(DRAWN_POSITION_MISMATCH)) ||
        message.includes(String(ACCOUNT_NOT_ENOUGH_KEYS)) ||
        message.includes(String(BLANK_SETTLES_ELSEWHERE))
      );
    };

    let seizedRoll = -1;
    for (let i = 0; i < rolls; i += 1) {
      let settled = false;

      // The Core variant first, while the position is still there to be drawn. A blank is refused
      // by it outright rather than settled, so the fallback below is the same position-less
      // `settle_roll` candidate every other row uses — a blank moves no NFT and has no variant.
      if (seizedRoll === -1) {
        try {
          await sendIxs(connection, operator, [
            await settleRollCore({
              operator: operator.address,
              pool: corePool,
              batchId,
              rollIndex: i,
              rollerUsdc,
              protocolRevenue: cfg.protocolRevenue,
              treasury05: await deriveAssociatedTokenAddress(cfg.t05Authority, cfg.usdcMint),
              weightIndex: coreWiSigner.address,
              roller: roller.address,
              asset,
              collection: core.collection,
              depositor: roller.address,
              settlementUsdc: rollerUsdc,
              successor: null,
            }),
          ]);
          seizedRoll = i;
          settled = true;
        } catch (e) {
          if (!missed(e)) throw e;
        }
      }

      if (!settled) {
        await sendIxs(connection, operator, [
          await settleRoll({
            operator: operator.address,
            pool: corePool,
            batchId,
            rollIndex: i,
            rollerUsdc,
            protocolRevenue: cfg.protocolRevenue,
            treasury05: await deriveAssociatedTokenAddress(cfg.t05Authority, cfg.usdcMint),
            weightIndex: coreWiSigner.address,
            roller: roller.address,
            nftMint: null,
            depositor: null,
            settlementUsdc: null,
            rollerToken: null,
            metadata: null,
            masterEdition: null,
            positionTokenRecord: null,
            rollerTokenRecord: null,
            sysvarInstructions: null,
            authorizationRulesProgram: null,
            authorizationRules: null,
            successor: null,
            blank: null,
          }),
        ]);
      }
    }

    assert.notEqual(seizedRoll, -1, `no roll of ${rolls} drew the Core position`);

    const batch = decodeRollBatch(
      (await fetchAccountData(connection, await batchPda(corePool, batchId)))!,
    );
    const roll = batch.rolls[seizedRoll]!;
    assert.equal(roll.status, "Refunded");
    assert.equal(roll.refundReason, COLLATERAL_ABSENT);

    // The force-close is what keeps the pool consistent once the card is gone: the weight leaves
    // and the position is closed in the same transaction that refunds the roll, so no later roll
    // can draw weight backing nothing.
    const { address: position } = await derivePda([
      "position",
      seedFromPubkey(corePool),
      seedFromPubkey(asset),
    ]);
    const positionState = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(positionState.state, "Seized", "the phantom position was not force-closed");

    const after = decodePool((await fetchAccountData(connection, corePool))!);
    assert.equal(after.nReal, 0, "the seized position still counts as real");
    assert.equal(after.openBatches, 0, "the batch did not complete");
  });
});
