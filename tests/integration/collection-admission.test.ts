// Integration coverage for `admit_collection` and `withdraw_collection` (D-114) against a live
// validator.
//
// **T-109 added the event half.** The two records' *accounts* were read here from the first
// version of this file; the two events the same instructions emit were decoded only in
// `tests/unit/decoders.test.ts`, against bytes a test wrote. `CollectionAdmitted.reason` is an
// `Option<u16>` — the only variable-width field on either payload, and the place a decoder drifts
// — so both arms are now read off the program's own log lines, on the two cases that produce
// them.
//
// It exists for the defect class T-86 named: an instruction encoder that has never executed hides
// bugs every unit instrument passes. `admitCollection` reaches a validator through every
// depositing suite's setup, but `withdrawCollection` had no caller anywhere — so its account
// order, its `close = administrator`, and its mandatory `reason` were pinned only against a static
// IDL. The two cases below execute both, and assert on the record's own decoded bytes rather than
// on a transaction succeeding.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { address, type Address } from "@solana/kit";
import {
  assertProgramIsLive,
  getConnection,
  loadSuiteAdmin,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { admitCollection, withdrawCollection } from "../../scripts/lib/admin.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import {
  decodeEvent,
  decodePoolCollection,
  type DecodedEvent,
} from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { addSignersToInstruction } from "@solana/kit";

const COLLECTION_SEED = "collection";
const PROGRAM_DATA_PREFIX = "Program data: ";

function isCollectionAdmitted(
  event: DecodedEvent,
): event is Extract<DecodedEvent, { name: "CollectionAdmitted" }> {
  return event.name === "CollectionAdmitted";
}

function isCollectionWithdrawn(
  event: DecodedEvent,
): event is Extract<DecodedEvent, { name: "CollectionWithdrawn" }> {
  return event.name === "CollectionWithdrawn";
}

/** Two arbitrary but distinct addresses standing in for collections. Nothing reads their contents
 * — `admit_collection` takes the collection as an argument and never touches the account, which is
 * itself part of what these cases prove: a record can be admitted for an address that holds no
 * collection at all, and the *intake* is what binds a record to a real asset's metadata. */
const COLLECTION_A = address("CCryptWBYktukHDQ2vHGtVcmtjXxYzvw8XNVY64YN2Yf");
const COLLECTION_B = address("So11111111111111111111111111111111111111112");

describe("D-114 — the admitted-collection set against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let poolId: number;
  let pool: Address;

  async function recordAddress(collection: Address): Promise<Address> {
    const { address: record } = await derivePda([
      COLLECTION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(collection),
    ]);
    return record;
  }

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();

    const { usdcMint, poolCounter } = await ensureProtocol(connection, admin);
    poolId = poolCounter;

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], createWiIx)]);

    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex: weightIndexSigner.address,
      usdcMint,
      args: { ...launchProfile(admin.address) },
    });
    pool = poolAccounts.pool;
  });

  it("admit_collection writes a record whose pool, collection and standards are what was asked for", async () => {
    const signature = await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: COLLECTION_A,
      standards: 0b011,
    });

    const data = await fetchAccountData(connection, await recordAddress(COLLECTION_A));
    assert.ok(data, "admit_collection created no account");
    const record = decodePoolCollection(data);
    assert.equal(record.pool, pool);
    assert.equal(record.collection, COLLECTION_A);
    assert.equal(record.standards, 0b011);
    assert.ok(record.admittedAt > 0n, "admitted_at was never stamped");

    // The event, off the program's own log line. `old_standards = 0` is the creation path's own
    // marker — an indexer has no other way to tell a first admission from a re-standarding that
    // happened to widen from nothing — and a `None` reason is what the creation path must carry,
    // because no bit was cleared.
    const admitted = (await eventsOf(signature)).filter(isCollectionAdmitted);
    assert.equal(admitted.length, 1, "expected exactly one CollectionAdmitted event");
    assert.equal(admitted[0].data.pool, pool);
    assert.equal(admitted[0].data.collection, COLLECTION_A);
    assert.equal(admitted[0].data.standards, 0b011);
    assert.equal(admitted[0].data.oldStandards, 0);
    assert.equal(admitted[0].data.reason, null);
    assert.equal(admitted[0].data.authority, admin.address);
  });

  // The widening path and the narrowing path in one case, because the `reason` requirement is a
  // property of the *effect* and the two directions reject each other's argument: a widening with
  // a reason is 6510 and a narrowing without one is 6509, so a handler that pinned the condition
  // to the entry path rather than the masks passes one and fails the other.
  it("admit_collection re-standards an existing record, keeping admitted_at and honouring the reason rule", async () => {
    const recordAddr = await recordAddress(COLLECTION_A);
    const before = decodePoolCollection((await fetchAccountData(connection, recordAddr))!);

    // Widening: no reason.
    const wideningSignature = await sendIxs(connection, admin, [
      await admitCollection({
        administrator: admin.address,
        poolId,
        collection: COLLECTION_A,
        standards: 0b111,
        reason: null,
      }),
    ]);
    const widened = decodePoolCollection((await fetchAccountData(connection, recordAddr))!);
    assert.equal(widened.standards, 0b111);
    assert.equal(widened.admittedAt, before.admittedAt, "admitted_at must date the FIRST admission");
    const widenedEvents = (await eventsOf(wideningSignature)).filter(isCollectionAdmitted);
    assert.equal(widenedEvents.length, 1);
    assert.equal(widenedEvents[0].data.oldStandards, 0b011, "the event must carry the mask it replaced");
    assert.equal(widenedEvents[0].data.standards, 0b111);
    assert.equal(widenedEvents[0].data.reason, null);

    // Narrowing: reason required, and carried.
    const narrowingSignature = await sendIxs(connection, admin, [
      await admitCollection({
        administrator: admin.address,
        poolId,
        collection: COLLECTION_A,
        standards: 0b001,
        reason: 42,
      }),
    ]);
    const narrowed = decodePoolCollection((await fetchAccountData(connection, recordAddr))!);
    assert.equal(narrowed.standards, 0b001);
    assert.equal(narrowed.admittedAt, before.admittedAt);
    // The `Some` arm, and the pair above is why both are read here: `reason` is the payload's
    // only variable-width field, so a decoder that assumed one arm reads `authority` out of the
    // tail on the other and is wrong about the last 32 bytes rather than about two.
    const narrowedEvents = (await eventsOf(narrowingSignature)).filter(isCollectionAdmitted);
    assert.equal(narrowedEvents.length, 1);
    assert.equal(narrowedEvents[0].data.oldStandards, 0b111);
    assert.equal(narrowedEvents[0].data.standards, 0b001);
    assert.equal(narrowedEvents[0].data.reason, 42);
    assert.equal(narrowedEvents[0].data.authority, admin.address);
  });

  it("withdraw_collection closes the record and leaves the pool's other records untouched", async () => {
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: COLLECTION_B,
      standards: 0b100,
    });
    const addrA = await recordAddress(COLLECTION_A);
    const addrB = await recordAddress(COLLECTION_B);
    assert.ok(await fetchAccountData(connection, addrB), "COLLECTION_B was never admitted");

    const signature = await sendIxs(connection, admin, [
      await withdrawCollection({
        administrator: admin.address,
        poolId,
        collection: COLLECTION_B,
        reason: 7,
      }),
    ]);

    assert.equal(
      await fetchAccountData(connection, addrB),
      null,
      "withdraw_collection left the record allocated",
    );
    // `standards` on this payload is the mask the record held when it was withdrawn, read from an
    // account the same transaction closed — so the emit has to happen before the close, and this
    // is the only assertion that can see that ordering.
    const withdrawn = (await eventsOf(signature)).filter(isCollectionWithdrawn);
    assert.equal(withdrawn.length, 1, "expected exactly one CollectionWithdrawn event");
    assert.equal(withdrawn[0].data.collection, COLLECTION_B);
    assert.equal(withdrawn[0].data.standards, 0b100);
    assert.equal(withdrawn[0].data.reason, 7);
    assert.equal(withdrawn[0].data.authority, admin.address);
    // The other record is the control: a `close` that took the wrong account, or one that closed
    // by pool rather than by collection, would take this one with it and a single-record test
    // could not tell.
    const survivor = decodePoolCollection((await fetchAccountData(connection, addrA))!);
    assert.equal(survivor.collection, COLLECTION_A);
    assert.equal(survivor.standards, 0b001);
  });
});
