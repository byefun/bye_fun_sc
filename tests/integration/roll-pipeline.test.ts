// The roll pipeline, driven against a validator for the first time.
//
// Until the VRF program joined the fixture list, nothing in this suite could open a batch at all:
// `commit_rolls` CPIs into that program to place its randomness request, so without it the very
// first step of the pipeline was unreachable — not merely unresolvable. Every roll-side claim was
// carried by Rust unit tests and source pins instead.
//
// What this file establishes is the floor the refund matrix and the compute-budget measurements
// stand on: a request that genuinely lands in a VRF-owned queue, a batch driven to `Resolved`, and
// the state machine still refusing a second resolution — then the settlement half, which is the
// pipeline's own contract rather than any particular payout: a second commit queues behind an
// already-open batch rather than being refused, only the queue's head may settle, rolls are admitted only at `next_roll`, and a batch completes on its last roll.
// The refund matrix covers which terminal each roll reaches; nothing here depends on that.
//
// **`test_resolve_batch` stands in for the oracle, and only for the oracle.** The real callback's
// signer is the provider's scoped identity PDA, which no party to this system can produce — that
// is what makes the callback unspoofable and also what makes a delivery impossible to stage here.
// The stand-in reuses the callback's own transition, so the batch it leaves behind is the batch a
// delivery leaves behind; what it skips is the signature. It exists only in a build carrying the
// `testing` feature.

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
import { updateVrfConfig } from "../../scripts/lib/admin.js";
import { approveDeposit, deposit } from "../../scripts/lib/deposit.js";
import { commitRolls, settleRoll, testResolveBatch } from "../../scripts/lib/roll.js";
import { decodePool, decodeProtocolConfig, decodeRollBatch } from "../../scripts/lib/decoders.js";
import { derivePda, seedFromPubkey, seedFromU64Le } from "../../scripts/lib/anchor.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";

/** `smallTierProfile`'s tier holds 3, and `required_tier_len` is `min(tier_size, n_real)`. */
const POSITIONS = 3;
const RANDOMNESS = new Uint8Array(32).fill(7);

/** `RollOutOfOrder` — a settlement whose index is not the batch's `next_roll`. */
const ROLL_OUT_OF_ORDER = 6205;

/** `BatchNotHeadOfQueue` — a settlement or recovery attempted on a batch that is queued but not
 * yet the pool's settlement queue head. */
const BATCH_NOT_HEAD_OF_QUEUE = 6215;

/** The queue's live-item count, at the u32 that follows its 8-byte discriminator. */
function queueItemCount(data: Buffer): number {
  return data.readUInt32LE(8);
}
import { deriveAssociatedTokenAddress } from "../../scripts/lib/anchor.js";

describe("roll pipeline — a batch opens and resolves against a validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let roller: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let usdcMint: Address;
  let rollerUsdc: Address;
  let batchId: bigint;
  let queueBefore: number;
  // Retained for the settlement rows below: predicting a draw needs every deposited asset, and
  // `settle_roll` needs the two accounts `init_protocol`/`init_pool` bound the pool to.
  let protocolConfig: Address;
  let weightIndex: Address;
  const assets: MintedAsset[] = [];

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

    // The bootstrap seeds `oracle_queue` with a placeholder, which was sufficient only while no
    // test reached the pipeline. `commit_rolls` pins the account by `address =
    // protocol_config.oracle_queue`, so the config has to name the loaded queue before the
    // request CPI can succeed. The other three fields move with it and are passed unchanged.
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
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], createWiIx)]);
    weightIndex = weightIndexSigner.address;

    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex: weightIndexSigner.address,
      usdcMint,
      args: { ...smallTierProfile() },
    });
    pool = poolAccounts.pool;

    const collection: PnftCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
    });
    // 0b011 — pNFT and legacy. The fixtures below are plain NonFungibles (standard 1), and the
    // helper's default admits standard 0 only.
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
      standards: 0b011,
    });

    // Three activations fill the tier: `promote` inserts without eviction while the tier has room,
    // so `top_tier.len` and `min(tier_size, n_real)` meet exactly at the third.
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
          weightIndex: weightIndexSigner.address,
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

  it("commit_rolls opens a batch and its request reaches the real VRF queue", async () => {
    const before = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(before.openBatches, 0, "a batch is already open — the pool was not clean");
    batchId = before.batchCounter;

    queueBefore = queueItemCount((await fetchAccountData(connection, VRF_QUEUE_FIXTURE))!);

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

    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.openBatches, 1, "the batch did not open");
    assert.equal(after.batchCounter, batchId + 1n);

    // The discriminating assertion: the item count moved, so the CPI reached the provider's own
    // request handler and it wrote. A batch account alone would not distinguish that from a
    // commit whose CPI silently did nothing.
    const queueAfter = queueItemCount((await fetchAccountData(connection, VRF_QUEUE_FIXTURE))!);
    assert.equal(queueAfter, queueBefore + 1, "no request was appended to the VRF queue");
  });

  it("test_resolve_batch drives the batch to Resolved with the randomness it was given", async () => {
    await sendIxs(connection, operator, [
      await testResolveBatch({
        operator: operator.address,
        pool,
        batchId,
        randomness: RANDOMNESS,
      }),
    ]);

    const { address: batchPda } = await derivePda([
      "batch",
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    const batch = decodeRollBatch((await fetchAccountData(connection, batchPda))!);

    assert.equal(batch.state, "Resolved");
    assert.deepEqual(Array.from(batch.randomness), Array.from(RANDOMNESS));
    // The expectation pair is seeded by settle_roll at this batch's own roll_index ==
    // 0, not by resolution — a live delivery and this stand-in both leave it zero here, and a
    // nonzero value would mean the stand-in wrote a field the real callback no longer touches.
    assert.equal(batch.expectedW, 0n, "expected_w must stay unseeded until this batch's roll 0");
  });

  it("a second resolution is refused, exactly as a re-delivery is", async () => {
    await expectAnchorError(
      sendIxs(connection, operator, [
        await testResolveBatch({
          operator: operator.address,
          pool,
          batchId,
          randomness: RANDOMNESS,
        }),
      ]),
      6206,
    );
  });

  // ── Settlement: the queue, head-of-queue gating, roll order, and completion ─────────────────
  //
  // The rows above leave a batch `Resolved` and unsettled, so `open_batches` is 1. A second
  // commitment does not refuse on that — it queues behind the first — so the rows below
  // spend that queue before returning the pool to the `open_batches == 0` state the later rows
  // assume.

  /** Assets still holding an open position: a settled real card closes one, and the candidate
   * search must stop offering it. */
  let live: number[];
  let queuedBatchId: bigint;

  async function batchAddress(id: bigint): Promise<Address> {
    const { address } = await derivePda(["batch", seedFromPubkey(pool), seedFromU64Le(id)]);
    return address;
  }

  function suite() {
    return { connection, operator, pool, protocolConfig, weightIndex, roller: roller.address, rollerUsdc, assets };
  }

  it("a second batch queues behind the first instead of being refused", async () => {
    const before = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(before.openBatches, 1, "the first batch is not open — the queue below proves nothing");
    queuedBatchId = before.batchCounter;

    await sendIxs(connection, roller, [
      await commitRolls({
        roller: roller.address,
        poolId,
        batchCounter: queuedBatchId,
        rollerUsdc,
        usdcMint,
        oracleQueue: VRF_QUEUE_FIXTURE,
        n: 1,
      }),
    ]);

    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.openBatches, 2, "the second commit did not queue");
    assert.equal(after.batchCounter, queuedBatchId + 1n);
  });

  // **The stale-expectation regression.** The queued batch's VRF is fulfilled here, while the head
  // batch (`batchId`) is still unsettled — this is precisely the window in which a callback that
  // seeded the queued batch's expectation from live pool state would see the head batch's own
  // settlement (below) move that state before the queued batch ever reaches the head. The callback seeds nothing, so there is nothing to go stale.
  it("the queued batch's VRF resolves while the head batch is still unsettled", async () => {
    await sendIxs(connection, operator, [
      await testResolveBatch({
        operator: operator.address,
        pool,
        batchId: queuedBatchId,
        randomness: new Uint8Array(32).fill(11),
      }),
    ]);

    const batch = decodeRollBatch(
      (await fetchAccountData(connection, await batchAddress(queuedBatchId)))!,
    );
    assert.equal(batch.state, "Resolved");
    assert.equal(
      batch.expectedW,
      0n,
      "a resolved-but-not-head batch must not have had its expectation seeded yet",
    );
  });

  it("the resolved-but-queued batch is still refused ahead of the head", async () => {
    const cfg = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    // Which position this names is immaterial — the head check refuses before any drawn-position
    // comparison runs, so an arbitrary still-live asset is enough to build a well-formed account
    // list.
    const candidate = assets[0]!;
    await expectAnchorError(
      sendIxs(connection, operator, [
        await settleRoll({
          operator: operator.address,
          pool,
          batchId: queuedBatchId,
          rollIndex: 0,
          rollerUsdc,
          protocolRevenue: cfg.protocolRevenue,
          treasury05: await deriveAssociatedTokenAddress(cfg.t05Authority, cfg.usdcMint),
          weightIndex,
          roller: roller.address,
          nftMint: candidate.mint,
          depositor: roller.address,
          settlementUsdc: rollerUsdc,
          rollerToken: await getTokenAccountAddress(roller.address, candidate.mint, false),
          metadata: candidate.metadata,
          masterEdition: candidate.masterEdition,
          positionTokenRecord: null,
          rollerTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
          successor: null,
          blank: null,
        }),
      ]),
      // The head check runs first — ahead of `BatchNotResolved`/`BatchExpectationMismatch` — so
      // a batch that is queued but already `Resolved` is refused on its queue position, exactly
      // as an unresolved one would be, rather than on any expectation state it hasn't seeded.
      BATCH_NOT_HEAD_OF_QUEUE,
    );
  });

  it("the head batch settles, completes, and leaves the queued batch still open", async () => {
    live = assets.map((_, i) => i);
    const drawn = await settleDrawnRoll(suite(), batchId, 0, live);
    if (drawn !== NO_POSITION) live.splice(live.indexOf(drawn), 1);

    const batch = decodeRollBatch((await fetchAccountData(connection, await batchAddress(batchId)))!);
    assert.equal(batch.nextRoll, 1, "next_roll did not advance past the settled roll");
    assert.equal(batch.state, "Complete", "a one-roll batch did not complete on its only roll");
    assert.notEqual(batch.rolls[0]!.status, "Pending", "the roll is still pending after settling");

    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.openBatches, 1, "completing the head must not touch the still-queued batch");
  });

  it("the queued batch becomes head and settles cleanly, seeding its own expectation fresh", async () => {
    const drawn = await settleDrawnRoll(suite(), queuedBatchId, 0, live);
    if (drawn !== NO_POSITION) live.splice(live.indexOf(drawn), 1);

    const batch = decodeRollBatch(
      (await fetchAccountData(connection, await batchAddress(queuedBatchId)))!,
    );
    assert.equal(batch.state, "Complete", "the former queue tail did not complete on its only roll");

    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.openBatches, 0, "the freeze did not lift once the queue drained");
  });

  // The minimum batch that can have an order at all. Whether each roll settles or refunds is the
  // refund matrix's subject and depends on what the draw selects; what this row pins is that the
  // program admits them only in index order and completes on the last one.
  it("settle × N: rolls are admitted in index order, and the batch completes on the last", async () => {
    const before = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(before.openBatches, 0, "the previous row left a batch open");
    const id = before.batchCounter;

    await drainTreasury04({ connection, administrator: admin, operator });
    await sendIxs(connection, roller, [
      await commitRolls({
        roller: roller.address,
        poolId,
        batchCounter: id,
        rollerUsdc,
        usdcMint,
        oracleQueue: VRF_QUEUE_FIXTURE,
        n: 2,
      }),
    ]);
    await sendIxs(connection, operator, [
      await testResolveBatch({
        operator: operator.address,
        pool,
        batchId: id,
        randomness: new Uint8Array(32).fill(3),
      }),
    ]);

    const opened = decodeRollBatch((await fetchAccountData(connection, await batchAddress(id)))!);
    assert.equal(opened.n, 2, "the batch did not open at the committed width");
    assert.equal(opened.nextRoll, 0, "a fresh batch does not start at roll 0");

    // Roll 1 before roll 0. The prediction is a live position rather than `null` so the account
    // list is the full one: a short list is refused by Anchor before the handler runs, which would
    // pass this row on an error raised by the wrong check. The order guard runs ahead of the
    // drawn-position check, so the prediction's correctness never comes into it.
    const cfg = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    const candidate = assets[live[0]!]!;
    await expectAnchorError(
      sendIxs(connection, operator, [
        await settleRoll({
          operator: operator.address,
          pool,
          batchId: id,
          rollIndex: 1,
          rollerUsdc,
          protocolRevenue: cfg.protocolRevenue,
          treasury05: await deriveAssociatedTokenAddress(cfg.t05Authority, cfg.usdcMint),
          weightIndex,
          roller: roller.address,
          nftMint: candidate.mint,
          depositor: roller.address,
          settlementUsdc: rollerUsdc,
          rollerToken: await getTokenAccountAddress(roller.address, candidate.mint, false),
          metadata: candidate.metadata,
          masterEdition: candidate.masterEdition,
          positionTokenRecord: null,
          rollerTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
          successor: null,
          blank: null,
        }),
      ]),
      ROLL_OUT_OF_ORDER,
    );

    const stalled = decodeRollBatch((await fetchAccountData(connection, await batchAddress(id)))!);
    assert.equal(stalled.nextRoll, 0, "the refused settlement advanced the cursor anyway");

    for (let i = 0; i < 2; i += 1) {
      const drawn = await settleDrawnRoll(suite(), id, i, live);
      if (drawn !== NO_POSITION) live.splice(live.indexOf(drawn), 1);

      const mid = decodeRollBatch((await fetchAccountData(connection, await batchAddress(id)))!);
      assert.equal(mid.nextRoll, i + 1, `next_roll did not advance after roll ${i}`);
    }

    const done = decodeRollBatch((await fetchAccountData(connection, await batchAddress(id)))!);
    assert.equal(done.state, "Complete", "the batch did not complete on its last roll");
    for (let i = 0; i < 2; i += 1) {
      assert.notEqual(done.rolls[i]!.status, "Pending", `roll ${i} is still pending`);
    }

    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.openBatches, 0, "the freeze did not lift on completion");
  });
});
