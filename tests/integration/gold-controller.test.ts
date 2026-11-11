// Gold's low-value controller state on chain: once the harmonic mean of Gold's cards falls to or
// below its 45 USDC ticket, every writer of the blank set leaves slots 0–2 at 0 and puts the
// score-minimising count of 100 USDC blanks in slot 3; above the ticket, slot 3 stays 0. A roll
// that draws a slot-3 blank then reaches a terminal like any blank: with Treasury 04 unable to
// cover 100 USDC it is refunded in full, its record naming slot 3.
//
// Expected counts are hand-computed by brute force over whole counts of the score
// |T·(w_real + x·w₃) − C·(n_real + x)|, ties to fewer blanks, with w = floor(C / value):
// seven cards at 30 USDC → 6; adding one, two, three cards at 500 USDC → 5, 3, 1; the ten-card
// pool with a 50 USDC slot-3 face → 8.

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
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
import { admitPoolCollection, createPool, goldProfile } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import {
  createPnftCollection,
  mintPlainNonFungiblePnft,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { VRF_PROGRAM } from "../../scripts/constants.js";
import { updateConfig, updateVrfConfig } from "../../scripts/lib/admin.js";
import { assertEveryPoolIdle, drainTreasury04 } from "../helpers/treasuries.js";
import { approveDeposit, deposit } from "../../scripts/lib/deposit.js";
import { commitRolls, settleRoll, testResolveBatch } from "../../scripts/lib/roll.js";
import {
  decodeEvent,
  decodePool,
  decodeProtocolConfig,
  decodeRollBatch,
  type BlankFaces,
  type DecodedEvent,
} from "../../scripts/lib/decoders.js";
import { deriveAssociatedTokenAddress, derivePda, seedFromPubkey, seedFromU64Le } from "../../scripts/lib/anchor.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";
import { VRF_QUEUE_FIXTURE } from "../helpers/vrf-queue.js";
import { blankWeight, classifyDraw, expandDraw } from "../../scripts/verify-batch.js";

const T04_SHORTFALL = 6601;
const FLOOR_NEEDS_ABOVE_TICKET_BLANK = 6527;
const ABOVE_TICKET_BLANK_TOO_CLOSE = 6528;
const RESERVED_BLANK_SLOT_SET = 6529;

describe("Gold's low-value controller state — slot 3 on chain", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let roller: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let usdcMint: Address;
  let rollerUsdc: Address;
  let protocolConfig: Address;
  let weightIndex: Address;
  let collection: PnftCollection;

  async function poolState() {
    return decodePool((await fetchAccountData(connection, pool))!);
  }

  async function usdcOf(owner: Address): Promise<bigint> {
    const ata = await getTokenAccountAddress(owner, usdcMint, false);
    const { value } = await connection.rpc.getTokenAccountBalance(ata, { commitment: "confirmed" }).send();
    return BigInt(value.amount);
  }

  async function depositAt(value: bigint): Promise<void> {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: roller.address,
      collection,
    });
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
        value,
        observedAt: BigInt(Math.floor(Date.now() / 1000)),
        lockUntil: 0n,
      }),
    ]);
  }

  async function newWeightIndex(): Promise<Address> {
    const { signer: weightIndexSigner, instruction } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], instruction)]);
    return weightIndexSigner.address;
  }

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

    weightIndex = await newWeightIndex();
    ({ pool } = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: goldProfile(),
    }));

    collection = await createPnftCollection({ connection, payer: admin, authority: admin });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
      standards: 0b011,
    });

    await connection.mintTokens(usdcMint, admin, 1_000_000_000n, roller.address, false);
    rollerUsdc = await getTokenAccountAddress(roller.address, usdcMint, false);
  });

  it("init_pool refuses a floor under the ticket with slot 3 empty, a slot-3 face under ticket + 1 USDC, and a reserved-slot face; accepts slot 3 at exactly ticket + 1 USDC", async () => {
    const spareIndex = await newWeightIndex();
    const id = (await ensureProtocol(connection, admin)).poolCounter;
    const gold = goldProfile();
    const refused: [bigint[], number][] = [
      [[5_000_000n, 10_000_000n, 25_000_000n, 0n, 0n, 0n, 0n, 0n], FLOOR_NEEDS_ABOVE_TICKET_BLANK],
      [[5_000_000n, 10_000_000n, 25_000_000n, 45_000_000n, 0n, 0n, 0n, 0n], ABOVE_TICKET_BLANK_TOO_CLOSE],
      [[5_000_000n, 10_000_000n, 25_000_000n, 45_999_999n, 0n, 0n, 0n, 0n], ABOVE_TICKET_BLANK_TOO_CLOSE],
      [[5_000_000n, 10_000_000n, 25_000_000n, 100_000_000n, 0n, 0n, 0n, 1n], RESERVED_BLANK_SLOT_SET],
    ];
    for (const [blankFaces, code] of refused) {
      await expectAnchorError(
        createPool({
          connection,
          administrator: admin,
          poolId: id,
          weightIndex: spareIndex,
          usdcMint,
          args: { ...gold, blankFaces: blankFaces as typeof gold.blankFaces },
        }),
        code,
      );
    }
    assert.equal((await ensureProtocol(connection, admin)).poolCounter, id, "a refused init_pool consumed a pool id");

    const accepted = await createPool({
      connection,
      administrator: admin,
      poolId: id,
      weightIndex: spareIndex,
      usdcMint,
      args: { ...gold, blankFaces: [5_000_000n, 10_000_000n, 25_000_000n, 46_000_000n, 0n, 0n, 0n, 0n] },
    });
    const created = decodePool((await fetchAccountData(connection, accepted.pool))!);
    assert.equal(created.blankFaces[3], 46_000_000n);
    assert.equal(created.admissionFloor, 30_000_000n);
  });

  it("approve_deposit sets slot 3 alone, at the score-minimising count, while Gold's cards average below the ticket", async () => {
    const observed: number[][] = [];
    for (let i = 0; i < 7; i += 1) await depositAt(30_000_000n);
    observed.push((await poolState()).blanks);
    for (let i = 0; i < 3; i += 1) {
      await depositAt(500_000_000n);
      observed.push((await poolState()).blanks);
    }
    assert.deepEqual(observed, [
      [0, 0, 0, 6, 0, 0, 0, 0],
      [0, 0, 0, 5, 0, 0, 0, 0],
      [0, 0, 0, 3, 0, 0, 0, 0],
      [0, 0, 0, 1, 0, 0, 0, 0],
    ]);
    const state = await poolState();
    assert.equal(state.nReal, 10);
  });

  it("a roll that draws the slot-3 blank is refunded in full while Treasury 04 cannot cover it, its record naming slot 3", async () => {
    const state = await poolState();
    assert.deepEqual(state.blanks, [0, 0, 0, 1, 0, 0, 0, 0]);
    const total = state.wReal + blankWeight(state.blanks, state.blankFaces);

    let randomness: Uint8Array | null = null;
    for (let fill = 0; fill < 256 && randomness === null; fill += 1) {
      const candidate = new Uint8Array(32).fill(fill);
      const target = classifyDraw(expandDraw(candidate, 0, total).r, state.blanks, state.blankFaces);
      if (target.kind === "blank" && target.faceIndex === 3) randomness = candidate;
    }
    assert.ok(randomness, "no fill byte lands on the slot-3 band");

    await drainTreasury04({ connection, administrator: admin, operator });

    const batchId = state.batchCounter;
    const before = await usdcOf(roller.address);
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
    assert.equal(before - (await usdcOf(roller.address)), 49_500_000n, "a Gold roll is not 49.50 USDC");

    await sendIxs(connection, operator, [
      await testResolveBatch({ operator: operator.address, pool, batchId, randomness }),
    ]);
    const cfg = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    await sendIxs(connection, operator, [
      await settleRoll({
        operator: operator.address,
        pool,
        batchId,
        rollIndex: 0,
        rollerUsdc,
        protocolRevenue: cfg.protocolRevenue,
        treasury05: await deriveAssociatedTokenAddress(cfg.t05Authority, cfg.usdcMint),
        weightIndex,
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

    const { address } = await derivePda(["batch", seedFromPubkey(pool), seedFromU64Le(batchId)]);
    const batch = decodeRollBatch((await fetchAccountData(connection, address))!);
    const record = batch.rolls[0]!;
    assert.equal(record.status, "Refunded");
    assert.equal(record.blankFaceIdx, 3);
    assert.equal(record.refundReason, T04_SHORTFALL);
    assert.deepEqual(record.blanksAtDraw, [0, 0, 0, 1, 0, 0, 0, 0]);
    assert.deepEqual(batch.sBlankFaces, goldProfile().blankFaces);
    assert.equal(await usdcOf(roller.address), before, "the refund is not the whole roll price");
    assert.deepEqual((await poolState()).blanks, [0, 0, 0, 1, 0, 0, 0, 0]);
  });

  it("update_config moving the slot-3 face re-evaluates the blank set on chain and announces it", async () => {
    async function setSlot3(face: bigint): Promise<DecodedEvent[]> {
      const faces = [...goldProfile().blankFaces] as BlankFaces;
      faces[3] = face;
      const signature = await sendIxs(connection, admin, [
        await updateConfig({ administrator: admin.address, poolId, update: { kind: "BlankFaces", value: faces }, demoted: [] }),
      ]);
      return (await connection.getLogs(signature))
        .filter((line) => line.startsWith("Program data: "))
        .map((line) => decodeEvent(Buffer.from(line.slice("Program data: ".length), "base64")));
    }
    function rebalanceOf(events: DecodedEvent[]) {
      const found = events.filter((e) => e.name === "RebalanceEvaluated");
      assert.equal(found.length, 1, `expected one RebalanceEvaluated, got ${events.map((e) => e.name)}`);
      return found[0]!.data as { pool: Address; nReal: number; blanksBefore: number[]; blanksAfter: number[] };
    }

    assert.deepEqual((await poolState()).blanks, [0, 0, 0, 1, 0, 0, 0, 0]);
    const lowered = rebalanceOf(await setSlot3(50_000_000n));
    assert.equal(lowered.pool, pool);
    assert.equal(lowered.nReal, 10);
    assert.deepEqual(lowered.blanksBefore, [0, 0, 0, 1, 0, 0, 0, 0]);
    assert.deepEqual(lowered.blanksAfter, [0, 0, 0, 8, 0, 0, 0, 0]);
    assert.deepEqual((await poolState()).blanks, [0, 0, 0, 8, 0, 0, 0, 0]);

    const restored = rebalanceOf(await setSlot3(100_000_000n));
    assert.deepEqual(restored.blanksBefore, [0, 0, 0, 8, 0, 0, 0, 0]);
    assert.deepEqual(restored.blanksAfter, [0, 0, 0, 1, 0, 0, 0, 0]);
    const state = await poolState();
    assert.deepEqual(state.blanks, [0, 0, 0, 1, 0, 0, 0, 0]);
    assert.deepEqual(state.blankFaces, goldProfile().blankFaces);
  });

  it("once the cards average above the ticket, the blank set moves to slots 0–2 and slot 3 returns to 0", async () => {
    for (let i = 0; i < 3; i += 1) await depositAt(2_500_000_000n);
    const blanks = (await poolState()).blanks;
    assert.equal(blanks[3], 0, `slot 3 still set: ${blanks}`);
    assert.deepEqual(blanks.slice(4), [0, 0, 0, 0]);
    assert.ok(blanks.slice(0, 3).some((count) => count > 0), `no below-ticket blank: ${blanks}`);
  });
});
