// T-89 — the Season-0 lock, on chain for the first time.
//
// `Position.lock_until` has had two readers since P4/P5 and no writer: `withdraw` guards on
// `now >= lock_until` (6300) and `record_value`'s below-floor branch retains a locked position
// instead of closing it. Both were reachable only against a `lock_until` that was always `0`, so
// AC-23, AC-61, AC-62 and INV-52 / INV-54's lock clause were **unexercisable, not merely
// deferred** — that is D-061, and it is why the crate's coverage tables list them as blocked with
// classifier-only cases. D-091 makes `approve_deposit` the writer, and this file is the first
// thing on this tree to observe any of it against a validator.
//
// What each case adds over the unit tests, stated because the overlap is real:
//   - `record_value`'s `classify` and `withdraw`'s `apply_withdraw` are already exhaustively
//     pinned on both sides of `now == lock_until` as pure functions, over synthetic `lock_until`
//     values a client could not produce.
//   - What no unit test can reach is whether the value the *attestor sends* arrives in the field
//     those classifiers read. That is the whole content of D-091, it spans the client encoder,
//     the IDL, borsh and the handler, and every one of those layers is a place the value can land
//     in `value_observed_at` instead — see approve_deposit.rs's transposition pin.
//
// Two fixtures deliberately differ only in `lock_until`, and that is the A/B this file rests on:
// case 2's position rejects at 6300 and case 3's identical position withdraws successfully, so
// the guard's polarity is measured rather than inferred from a single rejection that could have
// come from anywhere.
//
// **Case 6 is T-112's, and it is here because this file is what produces its precondition.** A
// position retained below the floor is also the only way a leaf below `ticket_target` stays in the
// Fenwick tree, and that state made `rebalance::evaluate` return 6703 where the answer is zero
// blanks — freezing `approve_deposit`, `withdraw` and `record_value` pool-wide. The case asserts
// the regime from the program's own `RebalanceEvaluated` on both the entry and the exit leg, so it
// reds rather than silently stops meaning anything if the fixtures drift out of it.
//
// **Case 5 depends on wall-clock time and does so deliberately.** AC-62's sentence is "the first
// sweep after its lock lifts deactivates it if it is still below the floor" — a *transition* on
// one position, which a second fixture with a pre-lapsed lock does not reproduce. The validator's
// `Clock::unix_timestamp` tracks real time, so the case takes a short lock, sweeps inside it, waits
// past it, and sweeps again. It fails closed: if the first sweep landed late it would emit
// `BelowFloorClosed` and the assertion reds. A red here is a timing slip on `LOCK_WINDOW_SECONDS`,
// not a program defect — check the emitted event before reading it as one.

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
import { withdraw } from "../../scripts/lib/exit.js";
import { beginSweep, recordValue, endSweep } from "../../scripts/lib/value.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import {
  decodeEvent,
  decodePosition,
  type DecodedEvent,
  type RebalanceEvaluated,
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
const PROFILE = launchProfile("11111111111111111111111111111111" as Address);
const ADMISSION_FLOOR = PROFILE.admissionFloor;
const TICKET_TARGET = PROFILE.ticketTarget;

/** Verbatim against `programs/bye_machine/src/common/constants.rs` — `WEIGHT_C`, the numerator of
 * `w = WEIGHT_C / value`. Case 6 needs it to say which rebalance regime the pool is in. */
const WEIGHT_C = 1_000_000_000_000_000_000n;

/** Comfortably above the floor — the value every position is admitted at. */
const ADMITTED_VALUE = 20_000_000n;
/** Comfortably below the floor **and below `ticket_target`**, and the second half is what matters.
 * This was `ADMISSION_FLOOR - 1_000_000n` = 11,000,000 — below the 12,000,000 floor and *above*
 * the 9,000,000 target, so every rebalance in this file ran with `n_real·C − T·w_real` positive
 * and T-112's regime was excluded by a fixture that read as thorough. `weight_of` is `C/V`, so a $1
 * leaf weighs 12x a floor leaf: one retained at this value puts `T·w_real` past `n_real·C` for any
 * pool of up to fifteen admitted positions, which is the state case 6 exercises entry and exit
 * in. */
const BELOW_FLOOR_VALUE = 1_000_000n;

/** Case 5's lock window. Long enough that the first sweep is safely inside it on a loaded
 * validator, short enough not to dominate the suite's wall clock. */
const LOCK_WINDOW_SECONDS = 25n;

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

describe("T-89 — the Season-0 lock (D-091) against a live validator", () => {
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
      name: "T-89 season-lock fixtures",
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
      args: { ...launchProfile(admin.address) },
    });
    pool = poolAccounts.pool;

    // `init_pool` creates no admission record, so the pool takes no deposit until this runs.
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
    });

    // The floor is what makes cases 4 and 5 mean anything; assert the fixture values straddle it
    // rather than trusting the profile to have kept the shape this file was written against.
    assert.ok(
      ADMITTED_VALUE >= ADMISSION_FLOOR && BELOW_FLOOR_VALUE < ADMISSION_FLOOR,
      `fixture values no longer straddle the admission floor (${ADMISSION_FLOOR}): admitted ` +
        `${ADMITTED_VALUE}, below-floor ${BELOW_FLOOR_VALUE}`,
    );
    // And the ticket target is what makes case 6 mean anything — a separate bound, not a
    // restatement. `admission_floor > ticket_target` always holds, so a value below the floor can
    // still be above the target, and every value this file used before T-112 was. Straddling the
    // floor is *not* sufficient, and asserting only it is what hid the defect.
    assert.ok(
      BELOW_FLOOR_VALUE < TICKET_TARGET,
      `the below-floor value ${BELOW_FLOOR_VALUE} is not below the ticket target ` +
        `${TICKET_TARGET}, so case 6 cannot reach the regime where T·w_real exceeds n_real·C`,
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
    /** The `approve_deposit` transaction. Case 6 reads its `RebalanceEvaluated` — activation is a
     * rebalance trigger, so the freeze T-112 closes is observable on the way *in* as well as out. */
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

  /** The `RebalanceEvaluated` a transaction emitted. `n_real` and `w_real` come off the program's
   * own event rather than being recomputed here, so case 6's regime assertion reads the state the
   * handler actually evaluated against. */
  function pickRebalance(events: DecodedEvent[]): RebalanceEvaluated {
    for (const event of events) {
      if (event.name === "RebalanceEvaluated") return event.data;
    }
    assert.fail(`no RebalanceEvaluated in [${events.map((e) => e.name).join(", ")}]`);
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
      // failing `record_value` leaks an open sweep into every later case: measured with T-112's
      // fix reverted, case 4 reverted 6703 and cases 5 and 6 then red with an opaque 6404
      // (`SweepAlreadyOpen`) — three reds, one cause, and the two that matter unattributable.
      // The `.catch` is deliberate: a failure to close must not replace the error that caused it.
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
    const p = await admitWithLock("t89-w", lockUntil);
    const stored = await readPosition(p.position);

    assert.equal(stored.state, "Active");
    assert.equal(
      stored.lockUntil,
      lockUntil,
      "the lock_until the attestor sent did not reach Position.lock_until — D-061's hole, or a " +
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

  // ── 2. AC-23, the withdrawal guard ────────────────────────────────────────────────────────
  it("withdraw is rejected with 6300 while the lock runs, and leaves the position Active", async () => {
    const p = await admitWithLock("t89-x1", nowSeconds() + 30n * 24n * 3_600n);

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
    const p = await admitWithLock("t89-x2", nowSeconds() - 1n);

    await sendIxs(connection, depositor, [await withdrawIx(p)]);

    const balance = await connection.getTokenAccountBalance({ tokenAccount: p.tokenAccount });
    assert.equal(balance.amount, 1n, "the depositor's own token account must hold the pNFT again");
    const info = await connection.rpc.getAccountInfo(p.position).send();
    assert.equal(info.value, null, "withdraw closes the position account");
  });

  // ── 4. AC-61 / INV-52 / INV-54's lock clause ──────────────────────────────────────────────
  it("a below-floor sweep retains a locked position: BelowFloorRetained, still Active, still in tier", async () => {
    const p = await admitWithLock("t89-r", nowSeconds() + 30n * 24n * 3_600n);
    const before = await readPosition(p.position);
    assert.equal(before.inTier, true, "the fixture must be a tier member for AC-61 to say anything");

    const events = await sweepOnce(p, BELOW_FLOOR_VALUE);
    const names = events.map((e) => e.name);

    assert.ok(
      names.includes("BelowFloorRetained"),
      `expected BelowFloorRetained, got [${names.join(", ")}]`,
    );
    assert.ok(
      !names.includes("BelowFloorClosed"),
      "the locked branch must succeed and retain — it never closes (P4 ruling 3)",
    );
    const retained = events.find((e) => e.name === "BelowFloorRetained")!;
    assert.equal(
      retained.data.lockUntil,
      p.lockUntil,
      "BelowFloorRetained published a different lock than the one on the account",
    );

    // AC-61: "retains its weight, selectability, and accrual". The position stays Active with its
    // re-attested value and its tier membership, and its Fenwick slot never moves — a released
    // slot is what a close does.
    const after = await readPosition(p.position);
    assert.equal(after.state, "Active");
    assert.equal(after.recordedValue, BELOW_FLOOR_VALUE, "the value is still recorded");
    assert.equal(after.inTier, true, "a retained position keeps its tier membership");
    assert.equal(after.slotIndex, before.slotIndex, "a retained position keeps its weight slot");
    assert.equal(after.lockUntil, p.lockUntil, "record_value must not touch the lock");
  });

  // ── 5. AC-62 — the first sweep after the lock lifts ───────────────────────────────────────
  it("the first sweep after the lock lifts closes the still-below-floor position", async () => {
    const lockUntil = nowSeconds() + LOCK_WINDOW_SECONDS;
    const p = await admitWithLock("t89-l", lockUntil);

    // Separate the two things that can red the next assertion, so its message is never wrong.
    // Without this, a missing or misplaced `lock_until` writer reds the sweep below and reports it
    // as a timing slip — measured: deleting the D-091 assignment produced exactly that misreport.
    assert.equal(
      (await readPosition(p.position)).lockUntil,
      lockUntil,
      "the lock never reached the account, so the sweep below would fail for that reason and " +
        "report it as a timing slip",
    );

    const insideLock = (await sweepOnce(p, BELOW_FLOOR_VALUE)).map((e) => e.name);
    assert.ok(
      insideLock.includes("BelowFloorRetained") && !insideLock.includes("BelowFloorClosed"),
      `the first sweep did not retain. The lock is on the account (asserted above), so this is ` +
        `either a slip past the ${LOCK_WINDOW_SECONDS}s window — timing, not a defect — or an ` +
        `inverted lock comparison in record_value's classify, which reds this same assertion. ` +
        `Case 4 tells the two apart: it uses a 30-day lock, so only the classifier reds it. ` +
        `Events: [${insideLock.join(", ")}]`,
    );

    // The validator's Clock::unix_timestamp is real time, so this is the lock actually lifting on
    // one position rather than a second fixture standing in for it — which is what AC-62 says.
    while (nowSeconds() <= lockUntil) {
      await new Promise((resolve) => setTimeout(resolve, 1_000));
    }

    const afterLock = await sweepOnce(p, BELOW_FLOOR_VALUE);
    const names = afterLock.map((e) => e.name);
    assert.ok(
      names.includes("BelowFloorClosed"),
      `expected BelowFloorClosed once the lock lifted, got [${names.join(", ")}]`,
    );
    assert.ok(
      !names.includes("BelowFloorRetained"),
      "the lock has lifted, so the retain branch must no longer be taken",
    );

    const after = await readPosition(p.position);
    assert.equal(after.state, "ClosedBelowFloor");
    assert.equal(after.inTier, false, "a closed position is demoted out of the tier");
  });

  // ── 6. T-112 — entry and exit while a sub-target leaf is held ──────────────────────────────
  it("approve_deposit and withdraw both succeed while a retained sub-target position is held", async () => {
    // A locked position retained below the floor *and* below the ticket target puts a leaf in
    // `w_real` heavier than any the admission floor would admit — `weight_of` is `C/V` — and
    // `T·w_real` past `n_real·C`. There the answer is zero blanks; `rebalance::evaluate` returned
    // 6703 instead, and `approve_deposit`, `withdraw` and `record_value` all evaluate a rebalance
    // unconditionally, so nothing could enter or leave the pool until the retained position was
    // re-attested above the floor — which is exactly what its lock prevents.
    //
    // **This case builds its own retained position rather than inheriting case 4's**, measured
    // rather than reasoned: with the fix reverted, case 4's sweep reverts, its position is never
    // re-attested, and a version of this case that depended on it red on the regime guard below
    // instead of on either leg — honest, since the guard fails closed, but it proved nothing about
    // entry or exit. Self-contained, the precondition is *observed* (`BelowFloorRetained`) inside
    // the case that needs it.
    const retained = await admitWithLock("t112-r", nowSeconds() + 30n * 24n * 3_600n);
    const retainEvents = (await sweepOnce(retained, BELOW_FLOOR_VALUE)).map((e) => e.name);
    assert.ok(
      retainEvents.includes("BelowFloorRetained"),
      `the sub-target leaf was not retained, so the regime below is not set up: ` +
        `[${retainEvents.join(", ")}]`,
    );

    const p = await admitWithLock("t112-x", 0n);

    const activation = pickRebalance(await eventsOf(p.approvalSignature));
    assert.ok(
      TICKET_TARGET * activation.wReal > BigInt(activation.nReal) * WEIGHT_C,
      `the pool is not in the sub-target regime at activation (n_real=${activation.nReal}, ` +
        `w_real=${activation.wReal}), so this case proves nothing — the retained position ` +
        `asserted above should have put T·w_real past n_real·C at BELOW_FLOOR_VALUE ` +
        `${BELOW_FLOOR_VALUE} against a ticket target of ${TICKET_TARGET}`,
    );
    assert.deepEqual(
      activation.blanksAfter,
      [0, 0, 0],
      "zero blanks is the answer in this regime, and the program publishes it",
    );

    const signature = await sendIxs(connection, depositor, [await withdrawIx(p)]);

    const exit = pickRebalance(await eventsOf(signature));
    assert.ok(
      TICKET_TARGET * exit.wReal > BigInt(exit.nReal) * WEIGHT_C,
      `the withdrawal left the sub-target regime (n_real=${exit.nReal}, w_real=${exit.wReal}), ` +
        `so the exit leg was not exercised in it`,
    );
    assert.deepEqual(exit.blanksAfter, [0, 0, 0]);

    const balance = await connection.getTokenAccountBalance({ tokenAccount: p.tokenAccount });
    assert.equal(balance.amount, 1n, "the depositor's own token account must hold the pNFT again");
    const info = await connection.rpc.getAccountInfo(p.position).send();
    assert.equal(info.value, null, "withdraw closes the position account");
  });
});
