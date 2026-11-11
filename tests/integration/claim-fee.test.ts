// claim_fee against a live validator: a depositor pays out their position's accrued
// fee share while it stays `Active`, in custody, and a tier member, instead of having to call
// `withdraw` just to be paid what they have already earned.
//
// The instruction carries no NFT-custody account and reads no `position.standard`, so one case
// against a plain standard-1 deposit exercises the whole account graph — nothing here is
// standard-specific the way the exit twins are.
//
// No live roll settles in this suite, so `Position.accrued` never leaves zero: the payout
// arithmetic itself (both fee legs, folding `accrued` in exactly once, the zero/repeated-claim
// idempotency, the owed_fees under-run) is exhaustively covered by `apply_claim_fee`'s own Rust
// unit tests in claim_fee.rs, which drive it directly against synthetic non-zero accumulators.
// What only a live validator proves is the surface this file exercises instead: the account
// graph resolves, the CPI framework accepts a zero-amount transfer being skipped, the position
// survives untouched, and the ownership constraint is enforced end to end.

import { before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  withSigner,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createPnftCollection, mintPlainNonFungiblePnft, type PnftCollection } from "../helpers/fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { deposit, approveDeposit } from "../../scripts/lib/deposit.js";
import { claimFee } from "../../scripts/lib/exit.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { decodePosition } from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { NOT_POSITION_OWNER } from "../helpers/authority-matrix.js";

const POSITION_SEED = "position";

const ADMITTED_VALUE = 20_000_000n;

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

describe("claim_fee against a live validator", () => {
  let connection: Connection;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let nftMint: Address;
  let position: Address;
  let depositorUsdc: Address;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    const admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 20);

    const { usdcMint, poolCounter, operator: op } = await ensureProtocol(connection, admin);
    operator = op;

    const collection: PnftCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "claim_fee fixtures",
    });

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [
      addSignersToInstruction([admin, weightIndexSigner], createWiIx),
    ]);
    const weightIndex = weightIndexSigner.address;

    poolId = poolCounter;
    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: { ...launchProfile() },
    });
    pool = poolAccounts.pool;

    // Legacy (standard 1) — the simplest custody path, since claim_fee reads none of it.
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
      standards: 0b010,
    });

    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection,
    });
    nftMint = asset.mint;

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
        value: ADMITTED_VALUE,
        observedAt: nowSeconds(),
        lockUntil: 0n,
      }),
    ]);

    const { address: derivedPosition } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(nftMint),
    ]);
    position = derivedPosition;

    // A zero-balance ATA is enough: the pool's fee accumulator never moves in this suite (no roll
    // settles), so `apply_claim_fee` returns exactly zero and the CPI that would move it is
    // skipped — see claim_fee.rs's own `if fees_paid > 0` gate.
    await connection.mintTokens(usdcMint, admin, 0n, depositor.address, false);
    depositorUsdc = await getTokenAccountAddress(depositor.address, usdcMint, false);
  });

  it("pays exactly the position's zero accrued balance, leaves it Active and its tier seat untouched, and is repeatable", async () => {
    const before1 = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(before1.state, "Active");
    assert.equal(
      before1.inTier,
      true,
      "the pool's launch profile carries a 20-seat tier and this is the only real position — it " +
        "must have crossed in at approval",
    );

    const usdcBefore = await connection.getTokenAccountBalance({ tokenAccount: depositorUsdc });

    await sendIxs(connection, depositor, [
      await claimFee({ depositor: depositor.address, poolId, nftMint, depositorUsdc }),
    ]);

    const after1 = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(after1.state, "Active", "claim_fee must not transition the position");
    assert.equal(after1.inTier, true, "claim_fee must not vacate the tier seat");
    assert.equal(after1.accrued, 0n, "nothing was owed and nothing is left owed");
    assert.equal(after1.recordedValue, before1.recordedValue, "unmoved");
    assert.equal(after1.slotIndex, before1.slotIndex, "the Fenwick leaf is untouched");

    const usdcAfter1 = await connection.getTokenAccountBalance({ tokenAccount: depositorUsdc });
    assert.equal(usdcAfter1.amount, usdcBefore.amount, "a zero payout moves no USDC");

    // Repeated: still zero, still a no-op on every field the first call left alone.
    await sendIxs(connection, depositor, [
      await claimFee({ depositor: depositor.address, poolId, nftMint, depositorUsdc }),
    ]);
    const after2 = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(after2.state, "Active");
    assert.equal(after2.inTier, true);
    assert.equal(after2.accrued, 0n);
    const usdcAfter2 = await connection.getTokenAccountBalance({ tokenAccount: depositorUsdc });
    assert.equal(usdcAfter2.amount, usdcBefore.amount);
  });

  it("refuses a signer who is not the position's depositor", async () => {
    const stranger = await fundedWallet(connection, 5);
    const ix = await claimFee({ depositor: depositor.address, poolId, nftMint, depositorUsdc });
    await expectAnchorError(
      sendIxs(connection, stranger, [withSigner(ix, stranger.address)]),
      NOT_POSITION_OWNER,
    );

    const untouched = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(untouched.state, "Active", "the refused call must not have moved the position");
  });
});
