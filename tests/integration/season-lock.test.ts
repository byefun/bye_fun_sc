// The Season-0 lock, on chain.
//
// `Position.lock_until` had no writer until `approve_deposit` became one, so every guard that
// reads it was reachable only against a lock that was always `0`: the withdrawal guard and the
// lock clause were **unexercisable, not merely deferred**. This file observes them against a
// validator.
//
// **The lock's scope is departure the depositor chooses, and nothing else.** `withdraw` guards on `now >= lock_until`, and `close_seized` guards its
// `Active` row on the seizure causes a depositor could arrange while still ending up with the
// card. `record_value` does not read the field at all: a position below the admission floor
// closes on the sweep that observes it, lock or no lock.
//
// What each case adds over the unit tests, stated because the overlap is real:
//   - `withdraw`'s `apply_withdraw` is already exhaustively pinned on both sides of
//     `now == lock_until` as a pure function, over synthetic `lock_until` values a client could
//     not produce.
//   - What no unit test can reach is whether the value the *attestor sends* arrives in the field
//     those guards read. That spans the client encoder, the
//     IDL, borsh and the handler, and every one of those layers is a place the value can land in
//     `value_observed_at` instead — see approve_deposit.rs's transposition pin.
//
// Two fixtures deliberately differ only in `lock_until`, and that is the A/B this file rests on:
// case 2's position rejects at 6300 and case 3's identical position withdraws successfully, so
// the guard's polarity is measured rather than inferred from a single rejection that could have
// come from anywhere.
//
// **Case 4 is the safety argument for closing a locked position, and it is the only place on
// chain that carries it.** An earlier design kept a locked below-floor position open because
// closing it would have stranded its NFT until the lock lifted — a closed-but-unclaimable state.
// That is no longer how closure works: every deactivation keeps the account and vault, and
// `claim_nft` gates on `position.state` and never reads `lock_until`. The case asserts the claim
// **succeeds while the lock is still running**, which is the step that makes closing safe.
// Without it the case would show only that the position closed, and the reason the earlier design
// refused to close it would go unmeasured.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import {
  createPnftCollection,
  mintAdmissiblePnft,
  tokenRecordPda,
  RULE_SET_ADDRESS,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { launchProfile, createPool, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { deposit, approveDeposit } from "../../scripts/lib/deposit.js";
import { withdraw, claimNft } from "../../scripts/lib/exit.js";
import { beginSweep, recordValue, endSweep } from "../../scripts/lib/value.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import {
  decodeEvent,
  decodePosition,
  type DecodedEvent,
} from "../../scripts/lib/decoders.js";
import { sendIxs } from "../../scripts/_common.js";
import { AUTH_RULES_PROGRAM, SYSVAR_INSTRUCTIONS } from "../../scripts/constants.js";

// Verbatim against programs/bye_machine/src/common/seeds.rs — the convention
// rule-set-replay.test.ts and return-rejected-custody.test.ts both already follow.
const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";

const PROGRAM_DATA_PREFIX = "Program data: ";

/** `ByeMachineError::PositionLocked` — 6300, pinned by exact numeric value in `errors.rs`'s own
 * `every_variant_is_pinned_to_its_wire_number`. Never `assert.rejects`: a bare rejection would pass
 * on `SweepNotOpen`, on an authority mismatch, or on a fixture that never deposited. */
const POSITION_LOCKED = 6300;

/** `launchProfile`'s `admissionFloor` and `ticketTarget`. Read from the profile rather than
 * restated, so a profile change cannot leave the below-floor value on the wrong side of either
 * bound and every case passing for the wrong reason. */
const PROFILE = launchProfile();
const ADMISSION_FLOOR = PROFILE.admissionFloor;

/** Comfortably above the floor — the value every position is admitted at. */
const ADMITTED_VALUE = 20_000_000n;

/** Verbatim against `programs/bye_machine/src/common/constants.rs` — `WEIGHT_C`, the numerator of
 * `w = WEIGHT_C / value`. Case 4 needs it to say what the closing leaf weighed on its way out. */
const WEIGHT_C = 1_000_000_000_000_000_000n;
/** Comfortably below the admission floor — the value case 4 re-attests at. */
const BELOW_FLOOR_VALUE = 1_000_000n;

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

describe("the Season-0 lock against a live validator", () => {
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
    depositor = await fundedWallet(connection, 20);

    const { usdcMint, poolCounter, operator: op } = await ensureProtocol(connection, admin);
    operator = op;

    collection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "season-lock fixtures",
    });

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [
      addSignersToInstruction([admin, weightIndexSigner], createWiIx),
    ]);
    weightIndex = weightIndexSigner.address;

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

    // `init_pool` creates no admission record, so the pool takes no deposit until this runs.
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
    });

    // The floor is what makes case 4 mean anything; assert the fixture values straddle it
    // rather than trusting the profile to have kept the shape this file was written against.
    assert.ok(
      ADMITTED_VALUE >= ADMISSION_FLOOR && BELOW_FLOOR_VALUE < ADMISSION_FLOOR,
      `fixture values no longer straddle the admission floor (${ADMISSION_FLOOR}): admitted ` +
        `${ADMITTED_VALUE}, below-floor ${BELOW_FLOOR_VALUE}`,
    );
  });

  type LockedPosition = {
    mint: Address;
    tokenAccount: Address;
    metadata: Address;
    masterEdition: Address;
    position: Address;
    positionTokenRecord: Address;
    depositorTokenRecord: Address;
    observedAt: bigint;
    lockUntil: bigint;
    /** The `approve_deposit` transaction. Case 4 reads its `RebalanceEvaluated` for the pool
     * aggregate *before* the close, so the weight assertion is a delta on this one position rather
     * than an absolute that silently depends on how many earlier cases left a position behind. */
    approvalSignature: string;
  };

  /** Mints, deposits and approves one admissible pNFT at `lockUntil`. Returns everything a later
   * `withdraw` or `record_value` needs, so no case re-derives a PDA. */
  async function admitWithLock(label: string, lockUntil: bigint): Promise<LockedPosition> {
    const fixture = await mintAdmissiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection,
      value: ADMITTED_VALUE,
      label,
    });

    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(fixture.mint),
    ]);
    const { address: positionVault } = await derivePda([
      POSITION_VAULT_SEED,
      seedFromPubkey(position),
    ]);
    const depositorTokenRecord = await tokenRecordPda(fixture.mint, fixture.tokenAccount);
    const positionTokenRecord = await tokenRecordPda(fixture.mint, positionVault);

    await sendIxs(connection, depositor, [
      await deposit({
        depositor: depositor.address,
        poolId,
        collection: collection.mint,
        nftMint: fixture.mint,
        depositorToken: fixture.tokenAccount,
        metadata: fixture.metadata,
        masterEdition: fixture.masterEdition,
        depositorTokenRecord,
        positionTokenRecord,
        sysvarInstructions: SYSVAR_INSTRUCTIONS,
        authorizationRulesProgram: AUTH_RULES_PROGRAM,
        authorizationRules: RULE_SET_ADDRESS,
      }),
    ]);

    // Distinct from `lockUntil` on purpose: these two are adjacent i64 arguments, and equal
    // fixtures would make a transposition unobservable in the read-back below.
    const observedAt = nowSeconds();
    const approvalSignature = await sendIxs(connection, operator, [
      await approveDeposit({
        operator: operator.address,
        poolId,
        nftMint: fixture.mint,
        depositor: depositor.address,
        weightIndex,
        displaced: null,
        value: ADMITTED_VALUE,
        observedAt,
        lockUntil,
      }),
    ]);

    return {
      mint: fixture.mint,
      tokenAccount: fixture.tokenAccount,
      metadata: fixture.metadata,
      masterEdition: fixture.masterEdition,
      position,
      positionTokenRecord,
      depositorTokenRecord,
      observedAt,
      lockUntil,
      approvalSignature,
    };
  }

  async function readPosition(address: Address) {
    const info = await connection.rpc.getAccountInfo(address, { encoding: "base64" }).send();
    assert.notEqual(info.value, null, `position ${address} does not exist`);
    const [b64] = info.value!.data as [string, string];
    return decodePosition(Buffer.from(b64, "base64"));
  }

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  function withdrawIx(p: LockedPosition) {
    return withdraw({
      depositor: depositor.address,
      poolId,
      nftMint: p.mint,
      weightIndex,
      depositorUsdc: null,
      depositorToken: p.tokenAccount,
      metadata: p.metadata,
      masterEdition: p.masterEdition,
      positionTokenRecord: p.positionTokenRecord,
      depositorTokenRecord: p.depositorTokenRecord,
      sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: AUTH_RULES_PROGRAM,
      authorizationRules: RULE_SET_ADDRESS,
      successor: null,
    });
  }

  /** One `begin_sweep` → `record_value` → `end_sweep` cycle, returning the `record_value`
   * transaction's own decoded events. `successor` is `null`: with `tierSize` 20 and a handful of
   * fixtures every position is a tier member, so no eligible non-member candidate exists for a
   * vacancy to be filled from. */
  async function sweepOnce(p: LockedPosition, value: bigint): Promise<DecodedEvent[]> {
    await sendIxs(connection, operator, [
      await beginSweep({ operator: operator.address, poolId }),
    ]);
    let signature: string;
    try {
      signature = await connection.sendTransactionFromInstructions({
        feePayer: operator,
        instructions: [
          await recordValue({
            operator: operator.address,
            poolId,
            nftMint: p.mint,
            depositor: depositor.address,
            weightIndex,
            value,
            observedAt: nowSeconds(),
            effect: { kind: "close", successor: null },
          }),
        ],
      });
    } catch (error) {
      // `end_sweep` on the failure path too, best-effort, and rethrow the original. Without it a
      // failing `record_value` leaks an open sweep into every later case, which then red with an
      // opaque 6404 (`SweepAlreadyOpen`) instead of their own cause: several reds, one cause, and
      // the one that matters unattributable. The `.catch` is deliberate: a failure to close must not replace the error
      // that caused it.
      await sendIxs(connection, operator, [
        await endSweep({ operator: operator.address, poolId }),
      ]).catch(() => undefined);
      throw error;
    }
    await sendIxs(connection, operator, [await endSweep({ operator: operator.address, poolId })]);
    return eventsOf(signature);
  }

  // ── 1. the writer itself ──────────────────────────────────────────────────────────────────
  it("approve_deposit stores lock_until on the Position, uncrossed with value_observed_at", async () => {
    const lockUntil = nowSeconds() + 30n * 24n * 3_600n;
    const p = await admitWithLock("lock-w", lockUntil);
    const stored = await readPosition(p.position);

    assert.equal(stored.state, "Active");
    assert.equal(
      stored.lockUntil,
      lockUntil,
      "the lock_until the attestor sent did not reach Position.lock_until — an unwritten field, or a " +
        "transposition somewhere between the encoder and the handler",
    );
    // The discriminating half. `lockUntil` alone landing correctly is consistent with both fields
    // being written from the same argument; asserting `valueObservedAt` too is what makes a
    // transposition visible on chain, and the two fixture values differ by ~30 days.
    assert.equal(
      stored.valueObservedAt,
      p.observedAt,
      "value_observed_at holds the lock, not the observation time — the two i64 args are crossed",
    );
    assert.notEqual(
      p.observedAt,
      lockUntil,
      "the two fixtures are equal, so the assertion above cannot distinguish a transposition",
    );
  });

  // ── 2. the withdrawal guard ───────────────────────────────────────────────────────────────
  it("withdraw is rejected with 6300 while the lock runs, and leaves the position Active", async () => {
    const p = await admitWithLock("lock-x1", nowSeconds() + 30n * 24n * 3_600n);

    await expectAnchorError(
      sendIxs(connection, depositor, [await withdrawIx(p)]),
      POSITION_LOCKED,
    );

    // A rejection must be a rejection, not a partial exit: the position survives, still Active,
    // still weighted, and the NFT stays in escrow.
    const after = await readPosition(p.position);
    assert.equal(after.state, "Active");
    assert.equal(after.lockUntil, p.lockUntil);
    const balance = await connection.getTokenAccountBalance({ tokenAccount: p.tokenAccount });
    assert.equal(balance.amount, 0n, "the pNFT must still be escrowed after a rejected withdraw");
  });

  // ── 3. the polarity control for case 2 ────────────────────────────────────────────────────
  it("the same withdraw succeeds on a position whose lock has already lapsed", async () => {
    // Identical to case 2 in every respect except `lock_until`. Without this case, case 2's 6300
    // is evidence that *something* rejected, not that the lock did.
    const p = await admitWithLock("lock-x2", nowSeconds() - 1n);

    await sendIxs(connection, depositor, [await withdrawIx(p)]);

    const balance = await connection.getTokenAccountBalance({ tokenAccount: p.tokenAccount });
    assert.equal(balance.amount, 1n, "the depositor's own token account must hold the pNFT again");
    const info = await connection.rpc.getAccountInfo(p.position).send();
    assert.equal(info.value, null, "withdraw closes the position account");
  });

  // ── 4. the lock does not exempt a below-floor position ────────────────────────────────────
  it("a below-floor sweep closes a locked position, and claim_nft succeeds while the lock runs", async () => {
    const lockUntil = nowSeconds() + 30n * 24n * 3_600n;
    const p = await admitWithLock("lock-r", lockUntil);
    const before = await readPosition(p.position);
    assert.equal(before.inTier, true, "the fixture must be a tier member for the demotion to say anything");

    const events = await sweepOnce(p, BELOW_FLOOR_VALUE);
    const names = events.map((e) => e.name);

    assert.ok(
      names.includes("BelowFloorClosed"),
      `expected BelowFloorClosed, got [${names.join(", ")}]`,
    );

    // The close must be the *whole* close, not a variant of it: identical to the unlocked path in
    // state, tier membership and weight. A handler that special-cased the lock anywhere downstream
    // of the classifier would still emit BelowFloorClosed and fail here.
    //
    // The weight half is read off `RebalanceEvaluated` rather than inferred: `inTier` is tier
    // membership, which is a different structure from the Fenwick leaf, and a close that demoted
    // the position but left its leaf weighted would satisfy every other assertion here. The event
    // carries the aggregate the handler itself evaluated against, after the removal.
    const closed = events.find((e) => e.name === "RebalanceEvaluated");
    assert.ok(closed, `no RebalanceEvaluated in [${names.join(", ")}]`);
    const admitted = (await eventsOf(p.approvalSignature)).find(
      (e) => e.name === "RebalanceEvaluated",
    );
    assert.ok(admitted, "approve_deposit emitted no RebalanceEvaluated to take the before-state from");
    assert.equal(
      closed.data.nReal,
      admitted.data.nReal - 1,
      "n_real did not come down by one, so the position is still counted as real",
    );
    assert.equal(
      closed.data.wReal,
      admitted.data.wReal - WEIGHT_C / ADMITTED_VALUE,
      "w_real did not lose exactly this leaf's weight — the leaf is still in the tree, or it left " +
        "at the re-attested value instead of the recorded one",
    );

    const after = await readPosition(p.position);
    assert.equal(after.state, "ClosedBelowFloor");
    assert.equal(after.recordedValue, BELOW_FLOOR_VALUE, "the re-attested value is still recorded");
    assert.equal(after.inTier, false, "a closed position is demoted out of the tier");
    assert.equal(
      after.lockUntil,
      lockUntil,
      "record_value must not clear the lock — withdraw still owes this depositor a 6300",
    );

    // **The assertion closing rests on.** A locked position was once kept open because the NFT
    // would have been stranded until the lock lifted. `claim_nft` reads `position.state` and never
    // `lock_until`, so it is not — and nothing else on this tree measures that. The lock is 30
    // days out and still running at this line.
    await sendIxs(connection, depositor, [
      await claimNft({
        depositor: depositor.address,
        pool,
        nftMint: p.mint,
        depositorUsdc: null,
        depositorToken: p.tokenAccount,
        metadata: p.metadata,
        masterEdition: p.masterEdition,
        positionTokenRecord: p.positionTokenRecord,
        depositorTokenRecord: p.depositorTokenRecord,
        sysvarInstructions: SYSVAR_INSTRUCTIONS,
        authorizationRulesProgram: AUTH_RULES_PROGRAM,
        authorizationRules: RULE_SET_ADDRESS,
      }),
    ]);

    assert.ok(
      nowSeconds() < lockUntil,
      "the lock lapsed during the case, so the claim above proves nothing about a live lock",
    );
    const balance = await connection.getTokenAccountBalance({ tokenAccount: p.tokenAccount });
    assert.equal(
      balance.amount,
      1n,
      "claim_nft did not return the pNFT, so the position closed while locked is unclaimable — " +
        "exactly the closed-but-unclaimable state",
    );
  });

});
