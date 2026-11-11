// The final roll's compute budget, measured on each custody path.
//
// `open_batches -= 1` lands in the same transaction as the last roll's settlement, so that one
// transaction carries the NFT transfer with whatever validation its standard requires, the USDC
// leg, the fee split, a tier delta, a rebalance evaluation and the batch-completion writes. If it
// exceeds the per-transaction compute limit the batch never completes and every gated action in
// the pool stays frozen behind it — an overrun here is a liveness failure, not one failed
// transaction, which is why this file asserts headroom rather than only reporting a number.
//
// **Measured from the transaction's own `computeUnitsConsumed`, never estimated**, and taken on a
// batch of one roll so the roll measured *is* the final roll rather than a roll near it.
//
// **Worst case, not average.** Each pool holds four positions with distinct values against a tier
// of three, so the three highest are members and the fourth is not: a draw that takes a member
// demotes it and promotes the outsider in its place, which is the tier work a draw on a
// non-member never does. The removal also moves `n_real` and `w_real`, so the rebalance at the
// end of the settlement re-evaluates the blank set in the same transaction.
//
// The three numbers are per custody path because the transfer leg is one of three, and they are
// not interchangeable: the pNFT path carries a Token Metadata `TransferV1` with its rule-set
// `Validate` sub-CPI and is the binding constraint; MPL Core and legacy carry neither.

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import {
  assertProgramIsLive,
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
  mintAdmissiblePnft,
  mintPlainNonFungiblePnft,
  tokenRecordPda,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { createCoreCollection, mintCoreAsset } from "../helpers/core-fixtures.js";
import { VRF_QUEUE_FIXTURE } from "../helpers/vrf-queue.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import {
  AUTH_RULES_PROGRAM,
  SYSVAR_INSTRUCTIONS,
  TOKEN_PROGRAM,
  VRF_PROGRAM,
} from "../../scripts/constants.js";
import { updateVrfConfig } from "../../scripts/lib/admin.js";
import { approveDeposit, deposit, depositCore } from "../../scripts/lib/deposit.js";
import { commitRolls, settleRoll, settleRollCore, testResolveBatch } from "../../scripts/lib/roll.js";
import { decodePool, decodeProtocolConfig, decodeRollBatch } from "../../scripts/lib/decoders.js";
import {
  derivePda,
  seedFromPubkey,
  seedFromU64Le,
  signer,
  writable,
} from "../../scripts/lib/anchor.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { blankWeight, classifyDraw, expandDraw } from "../../scripts/verify-batch.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";
import { deriveAssociatedTokenAddress } from "../../scripts/lib/anchor.js";

/** The runtime's per-transaction maximum. Not a budget this project chose — the ceiling an
 *  overrun would hit, which is what the headroom below is measured against. */
const COMPUTE_UNIT_CEILING = 1_400_000;

/** The rule-set account every admissible pNFT's `programmable_config` points at — the one whose
 *  `Validate` sub-CPI the pNFT transfer leg pays for. */
const OPERATIVE_RULE_SET = "eBJLFYPxJmMGKuFwpDWkzxZeUrad92kZRC5BJLpzyT9" as Address;

/** Four positions against a tier of three: the top three are members, the fourth is the successor
 *  a displacing demotion promotes. Values are distinct so the ordering is not a tie-break. */
const VALUES = [40_000_000n, 30_000_000n, 25_000_000n, 20_000_000n];

/** Index into `VALUES` of the position outside the tier — the successor a demotion promotes. */
const NON_MEMBER = 3;

/** `w = WEIGHT_C / V`, the same constant the program derives every leaf weight from. */
const WEIGHT_C = 1_000_000_000_000_000_000n;

/**
 * Whether `randomness` would draw a position inside the tier, predicted from pool state.
 *
 * **This steers the fixture; it does not judge the result.** `tests/helpers/roll.ts` refuses to
 * reimplement the draw, and for a good reason: a helper that recomputed which position was
 * selected would be the program's own expression written twice, and a test asserting against it
 * would agree with a wrong program. Nothing here is asserted against this prediction. The settle
 * loop below still discovers the drawn position the honest way — trying live candidates until the
 * on-chain range search accepts one — and the tier-member assertion reads that result, never this
 * one. A wrong prediction costs an attempt and can only ever hide a measurement, never pass one.
 * The functions come from the published verification path rather than a second copy written here.
 *
 * Steering is necessary, not a convenience. The tier holds the highest-valued positions and weight
 * is `C/V`, so the position outside it is always the cheapest and therefore always the heaviest
 * single real leaf: with three members against one outsider, the first real draw is the outsider
 * about a third of the time, and that fraction is a property of the values alone — no blank set
 * changes it. The loop used to take the first real draw it got and then assert it happened to be a
 * member, which held only because the blank set it was written against produced randomness that
 * landed that way. Packing the four values together raises the odds to roughly three in four and
 * no further, so with four positions the choice is to steer or to be lucky.
 */
function wouldDrawATierMember(
  randomness: Uint8Array,
  state: ReturnType<typeof decodePool>,
  live: readonly number[],
): boolean {
  const totalWeight = state.wReal + blankWeight(state.blanks, state.blankFaces);
  const { r } = expandDraw(randomness, 0, totalWeight);
  const target = classifyDraw(r, state.blanks, state.blankFaces);
  if (target.kind === "blank") return false;

  // Live positions in activation order, which is the order they entered the tree. Nothing has been
  // removed at this point — the loop returns on its first settled roll — so no slot has been
  // recycled underneath them.
  let prefix = 0n;
  for (const i of live) {
    prefix += WEIGHT_C / VALUES[i]!;
    if (target.residual < prefix) return i !== NON_MEMBER;
  }
  return false;
}

const DRAWN_POSITION_MISMATCH = 6214;
const ACCOUNT_NOT_ENOUGH_KEYS = 3005;
const BLANK_SETTLES_ELSEWHERE = 6213;

type Standard = "pnft" | "legacy" | "core";

interface Measurement {
  standard: Standard;
  units: number;
  tierMember: boolean;
  attempts: number;
}

const measurements: Measurement[] = [];

function isMiss(e: unknown): boolean {
  const message = e instanceof Error ? e.message : String(e);
  return (
    message.includes(String(DRAWN_POSITION_MISMATCH)) ||
    message.includes(String(ACCOUNT_NOT_ENOUGH_KEYS)) ||
    message.includes(String(BLANK_SETTLES_ELSEWHERE))
  );
}

describe("compute budget — the final roll of a batch, per custody path", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let roller: TransactionSigner;
  let usdcMint: Address;
  let rollerUsdc: Address;
  let protocolConfig: Address;
  let nextPoolId: number;
  let tmCollection: PnftCollection;
  let coreCollection: Address;

  after(async () => {
    await assertEveryPoolIdle(connection);
  });

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    roller = await fundedWallet(connection, 40);

    const protocolState = await ensureProtocol(connection, admin);
    usdcMint = protocolState.usdcMint;
    protocolConfig = protocolState.protocolConfig;
    nextPoolId = protocolState.poolCounter;

    await sendIxs(connection, admin, [
      await updateVrfConfig({
        administrator: admin.address,
        vrfProgram: VRF_PROGRAM,
        oracleQueue: VRF_QUEUE_FIXTURE,
        swapPool: SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: 100,
      }),
    ]);

    await connection.mintTokens(usdcMint, admin, 5_000_000_000n, roller.address, false);
    rollerUsdc = await getTokenAccountAddress(roller.address, usdcMint, false);

    tmCollection = await createPnftCollection({ connection, payer: admin, authority: admin });
    const core = await createCoreCollection({ connection, payer: admin, name: "core CU" });
    coreCollection = core.collection;
  });

  /** Mints USDC straight into a token account. `mintTokens` takes an owner and mints to that
   *  owner's account; a vault PDA is itself the account, so it is named as the destination. */
  async function fund(account: Address, amount: bigint): Promise<void> {
    const data = new Uint8Array(9);
    data[0] = 7;
    new DataView(data.buffer).setBigUint64(1, amount, true);
    await sendIxs(connection, admin, [
      {
        programAddress: TOKEN_PROGRAM,
        accounts: [writable(usdcMint), writable(account), signer(admin.address)],
        data,
      },
    ]);
  }

  async function measure(standard: Standard): Promise<Measurement> {
    const poolId = nextPoolId;
    nextPoolId += 1;

    const { signer: wiSigner, instruction: wiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, wiSigner], wiIx)]);
    const weightIndex = wiSigner.address;

    const { pool } = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: { ...smallTierProfile() },
    });

    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: standard === "core" ? coreCollection : tmCollection.mint,
      standards: standard === "core" ? 0b100 : standard === "pnft" ? 0b001 : 0b010,
    });

    // Highest value first, so the tier fills with the three that stay in it and the fourth
    // activates outside it — no displacement at admission, one available at settlement.
    const cards: { key: Address; asset: any }[] = [];
    for (const [rank, value] of VALUES.entries()) {
      let key: Address;
      let asset: any = null;

      if (standard === "core") {
        key = await mintCoreAsset({
          connection,
          payer: admin,
          collection: coreCollection,
          collectionAuthority: admin,
          owner: roller.address,
        });
        await sendIxs(connection, roller, [
          await depositCore({
            depositor: roller.address,
            poolId,
            collection: coreCollection,
            asset: key,
          }),
        ]);
      } else if (standard === "pnft") {
        asset = await mintAdmissiblePnft({
          connection,
          payer: admin,
          authority: admin,
          owner: roller.address,
          collection: tmCollection,
          value,
          label: `cu${rank}`,
        });
        key = asset.mint;
        const { address: position } = await derivePda([
          "position",
          seedFromPubkey(pool),
          seedFromPubkey(key),
        ]);
        const { address: positionVault } = await derivePda(["vault", seedFromPubkey(position)]);
        await sendIxs(connection, roller, [
          await deposit({
            depositor: roller.address,
            poolId,
            collection: tmCollection.mint,
            nftMint: key,
            depositorToken: asset.tokenAccount,
            metadata: asset.metadata,
            masterEdition: asset.masterEdition,
            depositorTokenRecord: await tokenRecordPda(key, asset.tokenAccount),
            positionTokenRecord: await tokenRecordPda(key, positionVault),
            sysvarInstructions: SYSVAR_INSTRUCTIONS,
            authorizationRulesProgram: AUTH_RULES_PROGRAM,
            authorizationRules: OPERATIVE_RULE_SET,
          }),
        ]);
      } else {
        asset = await mintPlainNonFungiblePnft({
          connection,
          payer: admin,
          authority: admin,
          owner: roller.address,
          collection: tmCollection,
        });
        key = asset.mint;
        await sendIxs(connection, roller, [
          await deposit({
            depositor: roller.address,
            poolId,
            collection: tmCollection.mint,
            nftMint: key,
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
      }

      await sendIxs(connection, operator, [
        await approveDeposit({
          operator: operator.address,
          poolId,
          nftMint: key,
          depositor: roller.address,
          weightIndex,
          displaced: null,
          value,
          observedAt: BigInt(Math.floor(Date.now() / 1000)),
          lockUntil: 0n,
        }),
      ]);

      cards.push({ key, asset });
    }

    const { address: principalVault } = await derivePda(["principal", seedFromPubkey(pool)]);
    await fund(principalVault, 5_000_000_000n);

    const cfg = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    const live = cards.map((_, i) => i);

    // The fourth card is the one outside the tier, so it is the successor a demotion promotes.
    const { address: successor } = await derivePda([
      "position",
      seedFromPubkey(pool),
      seedFromPubkey(cards[3]!.key),
    ]);

    // One roll per batch, so the roll measured is the batch's last. Randomness that would not put
    // the draw on a tier member is skipped before a batch is opened for it, so the measurement is
    // of the demotion-and-promotion case rather than of whichever case the bytes happened to
    // select — see `wouldDrawATierMember` for why this is steered rather than retried, and for why
    // nothing is asserted against the prediction.
    for (let attempt = 1; attempt <= 255; attempt += 1) {
      const state = decodePool((await fetchAccountData(connection, pool))!);
      if (state.nReal === 0) break;

      const randomness = new Uint8Array(32).fill(attempt);
      if (!wouldDrawATierMember(randomness, state, live)) continue;

      const batchId = state.batchCounter;

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
      await sendIxs(connection, operator, [
        await testResolveBatch({
          operator: operator.address,
          pool,
          batchId,
          randomness,
        }),
      ]);

      for (const i of [...live]) {
        const card = cards[i]!;
        let signature: string | null = null;
        try {
          if (standard === "core") {
            signature = await sendIxs(connection, operator, [
              await settleRollCore({
                operator: operator.address,
                pool,
                batchId,
                rollIndex: 0,
                rollerUsdc,
                protocolRevenue: cfg.protocolRevenue,
                treasury05: await deriveAssociatedTokenAddress(cfg.t05Authority, cfg.usdcMint),
                weightIndex,
                roller: roller.address,
                asset: card.key,
                collection: coreCollection,
                depositor: roller.address,
                settlementUsdc: rollerUsdc,
                successor: i === 3 ? null : successor,
              }),
            ]);
          } else {
            const rollerToken = await getTokenAccountAddress(roller.address, card.key, false);
            const { address: position } = await derivePda([
              "position",
              seedFromPubkey(pool),
              seedFromPubkey(card.key),
            ]);
            const { address: positionVault } = await derivePda(["vault", seedFromPubkey(position)]);
            const pnft = standard === "pnft";
            signature = await sendIxs(connection, operator, [
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
                nftMint: card.key,
                depositor: roller.address,
                settlementUsdc: rollerUsdc,
                rollerToken,
                metadata: card.asset.metadata,
                masterEdition: card.asset.masterEdition,
                positionTokenRecord: pnft ? await tokenRecordPda(card.key, positionVault) : null,
                rollerTokenRecord: pnft ? await tokenRecordPda(card.key, rollerToken) : null,
                sysvarInstructions: pnft ? SYSVAR_INSTRUCTIONS : null,
                authorizationRulesProgram: pnft ? AUTH_RULES_PROGRAM : null,
                authorizationRules: pnft ? OPERATIVE_RULE_SET : null,
                successor: i === 3 ? null : successor,
                blank: null,
              }),
            ]);
          }
        } catch (e) {
          if (!isMiss(e)) throw e;
          continue;
        }


        const batch = decodeRollBatch(
          (await fetchAccountData(
            connection,
            (await derivePda(["batch", seedFromPubkey(pool), seedFromU64Le(batchId)])).address,
          ))!,
        );
        // Only a settled roll removes its position; a refunded one leaves it in the pool, so the
        // candidate stays live and the next attempt can draw it again.
        if (batch.rolls[0]!.status !== "Settled") {
          break;
        }
        live.splice(live.indexOf(i), 1);

        const tx = await connection.rpc
          .getTransaction(signature as never, {
            maxSupportedTransactionVersion: 0,
            encoding: "json",
          })
          .send();
        const units = Number(tx?.meta?.computeUnitsConsumed ?? 0);
        assert.ok(units > 0, "the transaction reported no compute units consumed");
        assert.equal(batch.state, "Complete", "the roll measured was not the batch's last");

        return { standard, units, tierMember: i !== 3, attempts: attempt };
      }

      // No position matched, so the roll drew a blank. It still has to reach a terminal: the batch
      // stays open until it does, and `commit_rolls` refuses to open a second one over it.
      const pending = decodePool((await fetchAccountData(connection, pool))!);
      if (pending.openBatches > 0) {
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
      }
    }

    throw new Error(`no real card settled on the ${standard} path`);
  }

  for (const standard of ["legacy", "core", "pnft"] as const) {
    it(`${standard} — the final roll fits the compute ceiling`, async () => {
      const result = await measure(standard);
      measurements.push(result);

      const headroom = (((COMPUTE_UNIT_CEILING - result.units) / COMPUTE_UNIT_CEILING) * 100).toFixed(1);
      console.log(
        `  CU ${standard}: ${result.units} of ${COMPUTE_UNIT_CEILING} ` +
          `(${headroom}% headroom, tier member: ${result.tierMember}, batches: ${result.attempts})`,
      );

      // Without this the measurement could quietly be of the cheap case: a draw on the position
      // outside the tier demotes nothing and promotes nothing, and would under-report the very
      // work this file exists to bound.
      assert.ok(
        result.tierMember,
        `${standard} measured a draw on the non-member position — no demotion, so not the worst case`,
      );

      assert.ok(
        result.units < COMPUTE_UNIT_CEILING,
        `${standard} final roll consumed ${result.units}, over the ${COMPUTE_UNIT_CEILING} ceiling — ` +
          `a batch that cannot complete freezes every gated action in its pool`,
      );
    });
  }

  it("the pNFT path is the binding constraint, and every path is measured", () => {
    assert.equal(measurements.length, 3, "a custody path went unmeasured");
    const byStandard = new Map(measurements.map((m) => [m.standard, m.units]));
    const pnft = byStandard.get("pnft")!;

    // Asserted rather than assumed: the ordering is arguable from the account sets, and the whole
    // point of measuring is that the magnitudes are not.
    for (const other of ["legacy", "core"] as const) {
      assert.ok(
        pnft >= byStandard.get(other)!,
        `pNFT consumed ${pnft} but ${other} consumed ${byStandard.get(other)} — ` +
          `the path assumed to bind does not`,
      );
    }
  });
});
