// Integration coverage for `admit_collection` and `withdraw_collection` against a live
// validator.
//
// **The event half.** The two records' *accounts* were read here from the first
// version of this file; the two events the same instructions emit were decoded only in
// `tests/unit/decoders.test.ts`, against bytes a test wrote. `CollectionAdmitted.reason` is an
// `Option<u16>` — the only variable-width field on either payload, and the place a decoder drifts
// — so both arms are now read off the program's own log lines, on the two cases that produce
// them.
//
// It exists for a known defect class: an instruction encoder that has never executed hides
// bugs every unit instrument passes. `admitCollection` reaches a validator through every
// depositing suite's setup, but `withdrawCollection` had no caller anywhere — so its account
// order, its `close = administrator`, and its mandatory `reason` were pinned only against a static
// IDL. The two cases below execute both, and assert on the record's own decoded bytes rather than
// on a transaction succeeding.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { address, type Address } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  loadUnprivileged,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { admitCollection, initPool, withdrawCollection } from "../../scripts/lib/admin.js";
import { UNAUTHORIZED } from "../helpers/authority-matrix.js";
import { createPnftCollection, mintPlainNonFungiblePnft, type PnftCollection } from "../helpers/fixtures.js";
import { createCoreCollection, mintCoreAsset, readCoreAsset } from "../helpers/core-fixtures.js";
import { deposit, depositCore } from "../../scripts/lib/deposit.js";
import { instantSell, instantSellCore } from "../../scripts/lib/buyback.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { derivePda, seedFromPubkey, signer, writable } from "../../scripts/lib/anchor.js";
import { TOKEN_PROGRAM } from "../../scripts/constants.js";
import {
  decodeEvent,
  decodePoolCollection,
  type DecodedEvent,
} from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { addSignersToInstruction } from "@solana/kit";

const COLLECTION_SEED = "collection";
const PROGRAM_DATA_PREFIX = "Program data: ";
/** No record matches the collection the presented asset actually declares. */
const COLLECTION_NOT_ADMITTED = 6100;
/** The record is the asset's own declared collection, and does not carry the resolved
 * standard's bit. */
const STANDARD_NOT_ADMITTED_FOR_COLLECTION = 6108;

/** SPL Token `MintTo`: instruction 7, then the amount as a little-endian u64. */
function mintToData(amount: bigint): Uint8Array {
  const data = new Uint8Array(9);
  data[0] = 7;
  new DataView(data.buffer).setBigUint64(1, amount, true);
  return data;
}

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
const COLLECTION_C = address("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

describe("the admitted-collection set against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let stranger: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let usdcMint: Address;
  let weightIndex: Address;

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
    stranger = await loadUnprivileged();

    const protocolState = await ensureProtocol(connection, admin);
    usdcMint = protocolState.usdcMint;
    poolId = protocolState.poolCounter;

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], createWiIx)]);
    weightIndex = weightIndexSigner.address;
  });

  it("init_pool: a stranger is refused; the administrator is not", async () => {
    // The authority check sits on `protocol_config`, ahead of `pool`'s own `init` in account
    // order — Anchor evaluates constraints before the handler body, so a refused attempt here
    // creates nothing and the same `poolId`/`weight_index` are safe to reuse for the real call.
    await expectAnchorError(
      sendIxs(connection, stranger, [
        await initPool({
          administrator: stranger.address,
          poolId,
          weightIndex,
          usdcMint,
          args: { ...launchProfile() },
        }),
      ]),
      UNAUTHORIZED,
    );

    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: { ...launchProfile() },
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

  it("admit_collection: a stranger is refused; the administrator is not", async () => {
    await expectAnchorError(
      sendIxs(connection, stranger, [
        await admitCollection({
          administrator: stranger.address,
          poolId,
          collection: COLLECTION_C,
          standards: 0b001,
          reason: null,
        }),
      ]),
      UNAUTHORIZED,
    );
    assert.equal(
      await fetchAccountData(connection, await recordAddress(COLLECTION_C)),
      null,
      "the refused call admitted the collection anyway",
    );

    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: COLLECTION_C,
      standards: 0b001,
    });
    const record = decodePoolCollection((await fetchAccountData(connection, await recordAddress(COLLECTION_C)))!);
    assert.equal(record.collection, COLLECTION_C);
  });

  it("withdraw_collection: a stranger is refused; the administrator is not", async () => {
    const recordAddr = await recordAddress(COLLECTION_C);
    assert.ok(await fetchAccountData(connection, recordAddr), "COLLECTION_C was never admitted");

    await expectAnchorError(
      sendIxs(connection, stranger, [
        await withdrawCollection({
          administrator: stranger.address,
          poolId,
          collection: COLLECTION_C,
          reason: 1,
        }),
      ]),
      UNAUTHORIZED,
    );
    assert.ok(
      await fetchAccountData(connection, recordAddr),
      "the refused call closed the record anyway",
    );

    await sendIxs(connection, admin, [
      await withdrawCollection({
        administrator: admin.address,
        poolId,
        collection: COLLECTION_C,
        reason: 1,
      }),
    ]);
    assert.equal(
      await fetchAccountData(connection, recordAddr),
      null,
      "the administrator's withdraw_collection left the record allocated",
    );
  });
});

// Both rejection codes apply on both entry actions. The record's own
// predicate (`PoolCollection::require_admits`) is unit-pinned exhaustively in
// `state/pool_collection.rs`, and `core-standard-deposit.test.ts` /
// `legacy-standard-deposit.test.ts` each already drive one code through `deposit`/`deposit_core`
// as a side effect of what they were written to prove. Nothing yet drives either code through
// `instant_sell`/`instant_sell_core` — which reach the same predicate through
// `common/escrow.rs`'s `validate_admission` (Token Metadata) and
// `common/core_asset.rs`/`instant_sell_core.rs` (Core) — so this section is the deliberate
// census: one case per code per entry action, split across the two asset families so both guard
// implementations are each measured at least once.
describe("deposit and instant_sell refuse an inadmissible collection", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  let seller: TransactionSigner;
  let poolId: number;
  let sellerUsdc: Address;
  let treasury03: Address;
  let weightIndex: Address;

  /** A real pNFT collection with no `PoolCollection` record at all — the "no admitted collection"
   * story a caller actually presents on chain as a real, self-consistent record for a different
   * collection, since an `Account<PoolCollection>` at a nonexistent address never reaches the
   * handler at all. */
  let unadmittedPnft: PnftCollection;
  /** Admitted for pNFT only (`0b001`). Doubles as the real-but-wrong record substituted into the
   * `deposit` 6100 case below, and as its own collection for the `instant_sell` 6108 case — a
   * legacy asset from it resolves to standard 1, whose bit this record does not carry. */
  let pnftOnly: PnftCollection;
  /** A real Core collection with no record — the Core twin of `unadmittedPnft`. */
  let unadmittedCore: Address;
  let unadmittedCoreAuthority: TransactionSigner;
  /** Admitted for the Token Metadata standards only (`0b011`, no Core bit). Doubles as the
   * real-but-wrong record substituted into the `instant_sell_core` 6100 case, and as its own
   * collection for the `deposit_core` 6108 case. */
  let tmOnlyCore: Address;
  let tmOnlyCoreAuthority: TransactionSigner;

  const VALUE = 20_000_000n;
  const OBSERVED_AT = 1_800_000_000n;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    depositor = await fundedWallet(connection, 20);
    seller = await fundedWallet(connection, 20);

    const { usdcMint, poolCounter } = await ensureProtocol(connection, admin);
    poolId = poolCounter;

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], createWiIx)]);
    weightIndex = weightIndexSigner.address;

    // `false` on both calls: kite's `mintTokens` defaults `useTokenExtensions` to true while its
    // own `getTokenAccountAddress` defaults it to false, and this fixture's USDC mint is a legacy
    // Token mint — taking both defaults derives an ATA the mint call never funded.
    await connection.mintTokens(usdcMint, admin, 0n, seller.address, false);
    sellerUsdc = await getTokenAccountAddress(seller.address, usdcMint, false);

    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: { ...launchProfile() },
    });

    // Treasury 03 is the shared `PDA(["t03"])`, which `init_pool` creates when absent, authorised
    // by the ProtocolConfig PDA, so funding it is a bare `MintTo` against an account that exists.
    treasury03 = poolAccounts.treasury03;
    await sendIxs(connection, admin, [
      {
        programAddress: TOKEN_PROGRAM,
        accounts: [writable(usdcMint), writable(treasury03), signer(admin.address)],
        data: mintToData(1_000_000_000n),
      },
    ]);

    unadmittedPnft = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "unadmitted pnft",
    });
    pnftOnly = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "pnft-only pnft",
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: pnftOnly.mint,
      standards: 0b001,
    });

    const unadmitted = await createCoreCollection({
      connection,
      payer: admin,
      name: "unadmitted core",
    });
    unadmittedCore = unadmitted.collection;
    unadmittedCoreAuthority = unadmitted.authority;

    const tmOnly = await createCoreCollection({ connection, payer: admin, name: "tm-only core" });
    tmOnlyCore = tmOnly.collection;
    tmOnlyCoreAuthority = tmOnly.authority;
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: tmOnlyCore,
      standards: 0b011,
    });
  });

  it("deposit refuses a legacy asset against a real record for a collection it does not declare", async () => {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection: unadmittedPnft,
    });

    await expectAnchorError(
      sendIxs(connection, depositor, [
        await deposit({
          depositor: depositor.address,
          poolId,
          // Real and admitted — but not the collection this asset's own metadata names.
          collection: pnftOnly.mint,
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
      ]),
      COLLECTION_NOT_ADMITTED,
    );

    const balance = await connection.getTokenAccountBalance({ tokenAccount: asset.tokenAccount });
    assert.equal(balance.amount, 1n, "the card must not have moved");
  });

  it("instant_sell_core refuses a Core asset against a real record for a collection it does not declare", async () => {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection: unadmittedCore,
      collectionAuthority: unadmittedCoreAuthority,
      owner: seller.address,
    });

    const ix = await instantSellCore({
      seller: seller.address,
      operator: operator.address,
      poolId,
      // Real and admitted — but not the collection this asset declares.
      collection: tmOnlyCore,
      asset,
      sellerUsdc,
      weightIndex,
      displaced: null,
      loader: null,
      value: VALUE,
      observedAt: OBSERVED_AT,
    });
    await expectAnchorError(
      sendIxs(connection, operator, [addSignersToInstruction([seller, operator], ix)]),
      COLLECTION_NOT_ADMITTED,
    );

    const read = await readCoreAsset(connection, asset);
    assert.ok(read, "the Core asset account vanished");
    assert.equal(read.owner, seller.address, "the card must not have moved");
  });

  it("deposit_core refuses a Core asset from a collection admitted for the Token Metadata standards only", async () => {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection: tmOnlyCore,
      collectionAuthority: tmOnlyCoreAuthority,
      owner: depositor.address,
    });

    await expectAnchorError(
      sendIxs(connection, depositor, [
        await depositCore({ depositor: depositor.address, poolId, collection: tmOnlyCore, asset }),
      ]),
      STANDARD_NOT_ADMITTED_FOR_COLLECTION,
    );

    const read = await readCoreAsset(connection, asset);
    assert.ok(read, "the Core asset account vanished");
    assert.equal(read.owner, depositor.address, "the card must not have moved");
  });

  it("instant_sell refuses a legacy asset from a collection admitted for pNFT only", async () => {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: seller.address,
      collection: pnftOnly,
    });

    const ix = await instantSell({
      seller: seller.address,
      operator: operator.address,
      poolId,
      collection: pnftOnly.mint,
      nftMint: asset.mint,
      sellerToken: asset.tokenAccount,
      sellerUsdc,
      weightIndex,
      metadata: asset.metadata,
      masterEdition: asset.masterEdition,
      sellerTokenRecord: null,
      positionTokenRecord: null,
      sysvarInstructions: null,
      authorizationRulesProgram: null,
      authorizationRules: null,
      displaced: null,
      loader: null,
      value: VALUE,
      observedAt: OBSERVED_AT,
    });
    await expectAnchorError(
      sendIxs(connection, operator, [addSignersToInstruction([seller, operator], ix)]),
      STANDARD_NOT_ADMITTED_FOR_COLLECTION,
    );

    const balance = await connection.getTokenAccountBalance({ tokenAccount: asset.tokenAccount });
    assert.equal(balance.amount, 1n, "the card must not have moved");
  });
});
