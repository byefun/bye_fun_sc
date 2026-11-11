// The wrong-signer matrix, executed against a live validator for the roll pipeline.
//
// Every row here attacks one signer cell with a party that does not hold it, and every row is
// paired with a control. The control is the half that carries the weight: a rejection on its own
// proves only that the call failed, and a call can fail for a dozen reasons that have nothing to
// do with who signed it. The evidence that an authority check is *present* is that the authorised
// party gets past it — so each rejection below is followed either by the authorised party
// succeeding, or, where the success state is not reachable from here, by the authorised party
// failing at a *different* error. A stranger stopped at `Unauthorized` while the Operator is
// stopped at a state guard is the signature of an authority check that exists and discriminates.
//
// One cell inverts the assertion. `commit_rolls` is permissionless by design — a roller is any
// user — so its row requires a stranger's call to *succeed*. A matrix of pure refusals cannot see
// a missing check, because the evidence for a missing check is a success; this is the row that
// would red if someone gated the instruction that must not be gated.
//
// **Passing the right non-signer accounts matters as much as passing the wrong signer.** Both
// `settle_roll` and `recover_batch` carry a second `Unauthorized`, on `roller_usdc.owner` against
// the batch's roller — a custody check, not an authority check. A row that let the imposter bring
// their own USDC account would red on that constraint and look like a passing authority row while
// proving nothing about the signer. Every attempt below therefore passes the batch's real roller
// accounts and changes exactly one thing: who signs.

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
  withSigner,
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
import { NO_POSITION } from "../helpers/roll.js";
import { VRF_QUEUE_FIXTURE } from "../helpers/vrf-queue.js";
import { UNAUTHORIZED, CONSTRAINT_ADDRESS } from "../helpers/authority-matrix.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { VRF_PROGRAM } from "../../scripts/constants.js";
import { updateVrfConfig } from "../../scripts/lib/admin.js";
import { approveDeposit, deposit } from "../../scripts/lib/deposit.js";
import {
  closeBatch,
  commitRolls,
  recoverBatch,
  settleRoll,
  settleRollCore,
  testResolveBatch,
  vrfCallback,
} from "../../scripts/lib/roll.js";
import { decodePool, decodeProtocolConfig, decodeRollBatch } from "../../scripts/lib/decoders.js";
import { derivePda, seedFromPubkey, seedFromU64Le } from "../../scripts/lib/anchor.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";
import { deriveAssociatedTokenAddress } from "../../scripts/lib/anchor.js";

const POSITIONS = 3;

/** `QueueItemStillPresent` — where the Operator is stopped on a batch whose randomness request
 * is still live. It is what makes the recovery row's control meaningful: the authorised party
 * reaches a state guard, the stranger never gets that far. */
const QUEUE_ITEM_STILL_PRESENT = 6209;

describe("wrong-signer matrix — the roll pipeline", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let roller: TransactionSigner;
  let stranger: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let usdcMint: Address;
  let rollerUsdc: Address;
  let strangerUsdc: Address;
  let protocolConfig: Address;
  let protocolRevenue: Address;
  let treasury05: Address;
  let weightIndex: Address;
  const assets: MintedAsset[] = [];

  async function batchAddress(batchId: bigint): Promise<Address> {
    const { address } = await derivePda([
      Buffer.from("batch"),
      seedFromPubkey(pool),
      seedFromU64Le(batchId),
    ]);
    return address;
  }

  /** Opens a batch of `n` rolls paid for by `payer`, and returns its id. */
  async function openBatch(payer: TransactionSigner, payerUsdc: Address, n: number): Promise<bigint> {
    const state = decodePool((await fetchAccountData(connection, pool))!);
    const batchId = state.batchCounter;
    await drainTreasury04({ connection, administrator: admin, operator });
    await sendIxs(connection, payer, [
      await commitRolls({
        roller: payer.address,
        poolId,
        batchCounter: batchId,
        rollerUsdc: payerUsdc,
        usdcMint,
        oracleQueue: VRF_QUEUE_FIXTURE,
        n,
      }),
    ]);
    return batchId;
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
    stranger = await fundedWallet(connection, 20);

    const protocolState = await ensureProtocol(connection, admin);
    usdcMint = protocolState.usdcMint;
    poolId = protocolState.poolCounter;
    protocolConfig = protocolState.protocolConfig;

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
    await connection.mintTokens(usdcMint, admin, 1_000_000_000n, stranger.address, false);
    rollerUsdc = await getTokenAccountAddress(roller.address, usdcMint, false);
    strangerUsdc = await getTokenAccountAddress(stranger.address, usdcMint, false);

    const cfg = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    protocolRevenue = cfg.protocolRevenue;
    treasury05 = await deriveAssociatedTokenAddress(cfg.t05Authority, cfg.usdcMint);
  });

  it("update_vrf_config: a stranger is refused; the administrator is not", async () => {
    // Same authority mechanism as every other admin call — a `constraint =` on `protocol_config`
    // ahead of anything the call would otherwise write — so the refused attempt below leaves the
    // VRF wiring untouched for the real call right after it.
    await expectAnchorError(
      sendIxs(connection, stranger, [
        await updateVrfConfig({
          administrator: stranger.address,
          vrfProgram: VRF_PROGRAM,
          oracleQueue: VRF_QUEUE_FIXTURE,
          swapPool: SWAP_POOL_FIXTURE,
          maxSwapSlippageBps: 100,
        }),
      ]),
      UNAUTHORIZED,
    );

    await sendIxs(connection, admin, [
      await updateVrfConfig({
        administrator: admin.address,
        vrfProgram: VRF_PROGRAM,
        oracleQueue: VRF_QUEUE_FIXTURE,
        swapPool: SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: 100,
      }),
    ]);
    const cfgAfter = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    assert.equal(cfgAfter.oracleQueue, VRF_QUEUE_FIXTURE, "the administrator's call did not land");
  });

  // One batch carries every row below, in the order its own state machine allows. The pool
  // admits a single batch at a time, so a file that opened one per row would be refused at its
  // own second commit — and the rows are cheaper this way regardless: each settled draw can
  // consume a real card, and this pool holds three.
  //
  // The batch is opened by the stranger on purpose. That is the permissionless row, and it also
  // sharpens the closing row: the wallet refused there is the pool's own depositor, which is a
  // stronger statement than refusing a wallet with no relationship to the pool at all.
  let batchId: bigint;
  let batch: Address;

  /**
   * Settles one roll for a batch whose roller is not the wallet that deposited the cards.
   *
   * The shared settlement helper assumes those are the same wallet — every other roll suite opens
   * its batch with the depositor — and passing the roller as the depositor here derives a
   * `WalletStats` the stranger never opened, which Anchor refuses as uninitialised before any
   * guard under test runs. The two roles are kept apart below: the card is released to the
   * batch's roller, while the payout and the rent go to the account that actually deposited it.
   *
   * Like the shared helper, this is deliberately not a reimplementation of the draw: it tries
   * each live candidate and lets the program's own range search decide, so nothing here can agree
   * with a wrong program by recomputing what the program computed.
   */
  async function settleForOutsideRoller(
    rollIndex: number,
    rollerSigner: TransactionSigner,
    rollerUsdcAddr: Address,
  ): Promise<number> {
    const candidates: (number | null)[] = [null, ...assets.map((_, i) => i)];
    let mismatches = 0;

    for (const candidate of candidates) {
      const asset = candidate === null ? null : assets[candidate]!;
      const rollerToken =
        asset === null
          ? null
          : await getTokenAccountAddress(rollerSigner.address, asset.mint, false);
      try {
        await sendIxs(connection, operator, [
          await settleRoll({
            operator: operator.address,
            pool,
            batchId,
            rollIndex,
            rollerUsdc: rollerUsdcAddr,
            protocolRevenue,
            treasury05,
            weightIndex,
            roller: rollerSigner.address,
            nftMint: asset?.mint ?? null,
            depositor: asset === null ? null : roller.address,
            settlementUsdc: asset === null ? null : rollerUsdc,
            rollerToken,
            metadata: asset?.metadata ?? null,
            masterEdition: asset?.masterEdition ?? null,
            positionTokenRecord: null,
            rollerTokenRecord: null,
            sysvarInstructions: null,
            authorizationRulesProgram: null,
            authorizationRules: null,
            successor: null,
            blank: null,
          }),
        ]);
        return candidate ?? NO_POSITION;
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        if (!message.includes("6214") && !message.includes("3005")) throw e;
        mismatches += 1;
      }
    }
    throw new Error(`no candidate settled roll ${rollIndex} (${mismatches} mismatches)`);
  }

  it("commit_rolls: a wallet with no standing in the pool opens a batch of its own", async () => {
    // The inverted row. If someone gates this instruction, this reds — and nothing else in the
    // matrix would, because every other row is looking for a refusal.
    batchId = await openBatch(stranger, strangerUsdc, 1);
    batch = await batchAddress(batchId);

    const opened = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(
      opened.roller,
      stranger.address,
      "the batch did not record the stranger as its roller",
    );
    assert.equal(opened.state, "Committed");
  });

  it("commit_rolls: but the USDC it is charged from must be its own — custody, not authority", async () => {
    // The constraint that is easy to mistake for an authority check: it binds the funding
    // account to the signer. It is also an account constraint, so it decides before the
    // handler's in-flight guard — which is why this row reads the same whether or not a batch is
    // already open.
    const state = decodePool((await fetchAccountData(connection, pool))!);
    await expectAnchorError(
      sendIxs(connection, stranger, [
        await commitRolls({
          roller: stranger.address,
          poolId,
          batchCounter: state.batchCounter,
          rollerUsdc,
          usdcMint,
          oracleQueue: VRF_QUEUE_FIXTURE,
          n: 1,
        }),
      ]),
      UNAUTHORIZED,
    );
  });

  it("recover_batch: a stranger is stopped at the authority, the Operator at the timeout", async () => {
    // Driven while the batch's randomness request is still live, the only state this file can
    // offer a recovery attempt in. There is no success to reach — but the two signers stop at two
    // different errors, and that difference is what says the authority check exists and
    // discriminates. Pinning the Operator's exact code is what makes that argument hold: "the
    // Operator also failed" would be satisfied by an authority rejection too.
    await expectAnchorError(
      sendIxs(connection, roller, [
        await recoverBatch({
          operator: roller.address,
          pool,
          batchId,
          rollerUsdc: strangerUsdc,
          oracleQueue: VRF_QUEUE_FIXTURE,
        }),
      ]),
      UNAUTHORIZED,
    );

    await expectAnchorError(
      sendIxs(connection, operator, [
        await recoverBatch({
          operator: operator.address,
          pool,
          batchId,
          rollerUsdc: strangerUsdc,
          oracleQueue: VRF_QUEUE_FIXTURE,
        }),
      ]),
      QUEUE_ITEM_STILL_PRESENT,
    );
  });

  it("vrf_callback: a wallet in the callback's signer slot is refused by the address pin", async () => {
    await expectAnchorError(
      sendIxs(connection, roller, [
        withSigner(
          await vrfCallback({ pool, batchId, randomness: new Uint8Array(32).fill(7) }),
          roller.address,
        ),
      ]),
      CONSTRAINT_ADDRESS,
    );

    // The control. The real callback's signer is a PDA owned by the VRF program, so this suite
    // cannot produce it and there is no "right signer succeeds" half to run. What stands in its
    // place is the test-only resolution path: the same batch, in the same state, does accept a
    // delivery — which is what makes the refusal above a statement about the signer rather than
    // about the batch being unresolvable.
    await sendIxs(connection, operator, [
      await testResolveBatch({
        operator: operator.address,
        pool,
        batchId,
        randomness: new Uint8Array(32).fill(7),
      }),
    ]);
    const resolved = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(resolved.state, "Resolved", "the batch never accepted a delivery");
  });

  it("settle_roll: a stranger cannot settle, and the Operator can", async () => {
    // Every account here is the batch's real one; the only thing wrong is who signs.
    await expectAnchorError(
      sendIxs(connection, roller, [
        await settleRoll({
          operator: roller.address,
          pool,
          batchId,
          rollIndex: 0,
          rollerUsdc: strangerUsdc,
          protocolRevenue,
          treasury05,
          weightIndex,
          roller: stranger.address,
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
      ]),
      UNAUTHORIZED,
    );

    const drawn = await settleForOutsideRoller(0, stranger, strangerUsdc);

    assert.ok(
      drawn === NO_POSITION || drawn >= 0,
      "the Operator's settlement did not report a draw, so the refusal above proved nothing",
    );

    const completed = decodeRollBatch((await fetchAccountData(connection, batch))!);
    assert.equal(completed.state, "Complete", "the batch did not complete on its only roll");
  });

  it("settle_roll_core: a stranger is stopped at the authority, the Operator is not", async () => {
    // The Core settlement's authority cell, reachable without a Core pool: its `operator`
    // constraint hangs off `protocol_config`, the second account in the list, so it is decided
    // before the pool, the batch, or the asset is looked at.
    //
    // Driven while the batch still exists, which is load-bearing. Against a closed batch both
    // signers red on the missing account and the row proves nothing — the stranger has to reach
    // the authority for its refusal to be about the authority.
    //
    // The control is differential and deliberately states only what it can: the Operator's
    // refusal must not be the authority's. Everything else about this call is wrong for a Core
    // settlement — a Token Metadata mint stands in for the asset — so which later guard catches
    // the Operator is not the point, and pinning one would pin an accident. What matters is that
    // the two signers stop in different places, and only one of them at the gate under test.
    const coreArgs = {
      pool,
      batchId,
      rollIndex: 0,
      rollerUsdc: strangerUsdc,
      protocolRevenue,
      treasury05,
      weightIndex,
      roller: stranger.address,
      asset: assets[0]!.mint,
      collection: assets[0]!.mint,
      depositor: roller.address,
      settlementUsdc: rollerUsdc,
      successor: null,
    };

    await expectAnchorError(
      sendIxs(connection, roller, [
        await settleRollCore({ ...coreArgs, operator: roller.address }),
      ]),
      UNAUTHORIZED,
    );

    let operatorCode: number | null = null;
    try {
      await sendIxs(connection, operator, [
        await settleRollCore({ ...coreArgs, operator: operator.address }),
      ]);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      operatorCode = Number(/#(\d+)|error (\d+)/.exec(message)?.slice(1).find(Boolean) ?? NaN);
    }

    assert.notEqual(
      operatorCode,
      null,
      "the Operator's call succeeded, so this row's accounts were not as wrong as it assumes",
    );
    assert.notEqual(
      operatorCode,
      UNAUTHORIZED,
      `the Operator was refused by the authority too (${operatorCode}), so the stranger's ` +
        `refusal says nothing about who signed`,
    );
  });

  it("close_batch: the pool's own depositor cannot close another wallet's batch", async () => {
    // The batch's roller here is `stranger`. Neither naming itself nor naming the true roller in
    // the roller slot lets another wallet close it.
    for (const recipient of [roller.address, stranger.address]) {
      await expectAnchorError(
        sendIxs(connection, roller, [
          await closeBatch({ signer: roller.address, roller: recipient, pool, batchId }),
        ]),
        UNAUTHORIZED,
      );
    }
    assert.notEqual(
      await fetchAccountData(connection, batch),
      null,
      "the refused close removed the account anyway",
    );

    await sendIxs(connection, stranger, [
      await closeBatch({ signer: stranger.address, roller: stranger.address, pool, batchId }),
    ]);
    assert.equal(
      await fetchAccountData(connection, batch),
      null,
      "the batch's own roller could not close it",
    );
  });
});
