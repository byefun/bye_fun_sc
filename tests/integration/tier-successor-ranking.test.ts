// The successor-ranking absence-evidence case, on chain rather than against a mock RPC.
//
// `fill_vacancy_from_candidate` (common/tier.rs:147-176) validates only that the supplied
// candidate is writable, belongs to the right pool, and is an eligible non-member — it never
// ranks it against any other eligible candidate. "The highest-ranked non-member promotes" is
// therefore enforced nowhere on chain; `scripts/lib/tier-resolve.ts`'s `resolveSuccessor` is the
// *sole* place that clause is checked, off-chain, before a caller ever builds this instruction.
//
// `tests/unit/tier-resolve.test.ts` already proves `resolveSuccessor` picks correctly, against a
// mock RPC. That is not the same claim as this file's: the case whose passing condition is that
// the program does *not* catch a wrong choice does not exist anywhere else. Passing a
// deliberately wrong (lower-ranked) successor here and observing the call succeed anyway is the
// only way to demonstrate the absence — a call that failed would prove the opposite of what this
// file exists to show.

import { before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import {
  assertProgramIsLive,
  fundedWallet,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createPnftCollection, mintPlainNonFungiblePnft, type PnftCollection } from "../helpers/fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { deposit, approveDeposit } from "../../scripts/lib/deposit.js";
import { beginSweep, endSweep, recordValue } from "../../scripts/lib/value.js";
import { decodePosition } from "../../scripts/lib/decoders.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";

const POSITION_SEED = "position";

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

describe("the successor ranking clause has no on-chain backstop", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;
  let collection: PnftCollection;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    depositor = await fundedWallet(connection, 20);

    const { usdcMint, poolCounter } = await ensureProtocol(connection, admin);
    poolId = poolCounter;

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
      weightIndex,
      usdcMint,
      // tier_size 2: the smallest that lets a vacancy exist with two eligible waiters ranked
      // against each other.
      args: { ...launchProfile(), tierSize: 2 },
    });
    pool = poolAccounts.pool;

    collection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "ranking-absence fixtures",
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
      standards: 0b011,
    });
  });

  async function positionOf(nftMint: Address): Promise<Address> {
    const { address } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    return address;
  }

  async function depositAndApprove(value: bigint): Promise<Address> {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection,
    });
    await sendIxs(connection, depositor, [
      await deposit({
        depositor: depositor.address,
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
        depositor: depositor.address,
        weightIndex,
        displaced: null,
        value,
        observedAt: nowSeconds(),
        lockUntil: 0n,
      }),
    ]);
    return asset.mint;
  }

  it("a lower-ranked eligible waiter is promoted over a higher-ranked one when named as the successor", async () => {
    // Fill the tier (size 2) with two members well above the two waiters below, so the vacancy
    // this test opens is real and neither waiter could have entered on its own merit yet.
    await depositAndApprove(50_000_000n);
    const seatToVacate = await depositAndApprove(40_000_000n);

    // Two eligible, non-member waiters — both rejected from entering while the tier stayed full
    // — ranked against each other by value alone (the same key resolveSuccessor ranks by).
    const higherRanked = await depositAndApprove(20_000_000n);
    const lowerRanked = await depositAndApprove(15_000_000n);
    for (const mint of [higherRanked, lowerRanked]) {
      assert.equal(
        decodePosition((await fetchAccountData(connection, await positionOf(mint)))!).inTier,
        false,
        "both waiters must still be outside the tier for this row to prove anything",
      );
    }

    const vacatedPosition = await positionOf(seatToVacate);
    await sendIxs(connection, operator, [await beginSweep({ operator: operator.address, poolId })]);
    // The wrong choice, deliberately: lowerRanked is named as the successor while higherRanked —
    // strictly better by the same ranking resolveSuccessor would have applied — sits unpromoted
    // in the very same scan. A program that enforced the ranking would refuse this.
    await sendIxs(connection, operator, [
      await recordValue({
        operator: operator.address,
        poolId,
        nftMint: seatToVacate,
        depositor: depositor.address,
        weightIndex,
        value: 1_000_000n,
        observedAt: nowSeconds(),
        effect: { kind: "close", successor: await positionOf(lowerRanked) },
      }),
    ]);
    await sendIxs(connection, operator, [await endSweep({ operator: operator.address, poolId })]);

    assert.equal(
      decodePosition((await fetchAccountData(connection, vacatedPosition))!).inTier,
      false,
      "the closed position must have left the tier",
    );
    assert.equal(
      decodePosition((await fetchAccountData(connection, await positionOf(lowerRanked)))!).inTier,
      true,
      "the deliberately wrong successor was not promoted — the call should have succeeded regardless of rank",
    );
    assert.equal(
      decodePosition((await fetchAccountData(connection, await positionOf(higherRanked)))!).inTier,
      false,
      "the higher-ranked waiter must still be outside the tier — nothing here should have touched it",
    );
  });
});
