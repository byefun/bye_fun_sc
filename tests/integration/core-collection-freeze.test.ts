// The freeze that lives on the **collection** rather than on the card, against a live validator.
//
// **This is the cause D-115 was opened for, and until the review that produced this file nothing
// in the program could see it.** `permanent_freeze_delegate` is authority-managed, so MPL Core
// accepts it on an asset *or* on a collection, and a collection's copy is run over every member
// asset's transfer. design §3.4 records the live instance: on the Core collection this pool
// admits, that plugin sits with `EZnkm3wLDss1…AMxg` — a third party, on the collection, and
// nowhere in any card's own registry. A freeze read that walked only the asset therefore found
// nothing in exactly the case the `Seized` terminal exists to serve: the card is present,
// vault-owned and clean in its own registry, so the classification said *settleable*, every exit
// drove a `TransferV1` MPL Core refuses, and `close_seized` answered `CollateralPresent` (6308) —
// a position that could neither exit nor be cleaned up, holding weight behind a card nobody can
// move.
//
// The whole fix is one more account and one more registry walk, and the two are inseparable: the
// collection has to be *supplied* for its plugins to be read, and it has to be *pinned* to the
// asset's own `update_authority` or a permissionless `close_seized` caller would name any frozen
// collection on chain and seize a healthy position with it. Both halves are asserted here,
// because a unit test over fabricated account bytes can prove the walk and cannot prove that MPL
// Core refuses the transfer for the same reason.
//
// **The source state is `Rejected` on purpose**, and it is the review's second finding in the
// same file. `return_rejected_core` refuses a frozen card with `CollateralFrozen` (6309) *naming
// `close_seized`* — and `close_seized` used to refuse `Rejected` outright, so the only remedy a
// depositor was offered was a call that could not be made. Both directions run below, in the
// order a depositor would meet them.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  expectProgramError,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import {
  MPL_CORE_PROGRAM,
  UPDATE_AUTHORITY_TAG,
  createCoreCollection,
  mintCoreAsset,
  readCoreAsset,
  setCollectionPermanentFreeze,
  updateCoreAssetAuthority,
} from "../helpers/core-fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { depositCore, rejectDeposit, returnRejectedCore } from "../../scripts/lib/deposit.js";
import { claimNftCore, closeSeized } from "../../scripts/lib/exit.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { decodeEvent, decodePosition, type DecodedEvent } from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const PROGRAM_DATA_PREFIX = "Program data: ";

/** The record's derivation, and the pin that refuses a collection the asset does not declare. */
const COLLECTION_NOT_ADMITTED = 6100;
/** The card is frozen and cannot be transferred — `close_seized` is the remedy. */
const COLLATERAL_FROZEN = 6309;
/** The card is present, vault-owned and unfrozen — there is nothing to close. */
const COLLATERAL_PRESENT = 6308;
/** An operator's rejection reason; any non-zero value, never read by these assertions. */
const REJECT_REASON = 7;
/** `MplCoreError::NotAvailable` — the answer to every attempt to move an asset out of its
 * collection, which is what makes the program's no-collection arm unreachable. */
const MPL_CORE_NOT_AVAILABLE = 23;

function isPositionSeized(
  event: DecodedEvent,
): event is Extract<DecodedEvent, { name: "PositionSeized" }> {
  return event.name === "PositionSeized";
}

describe("a permanent freeze on the collection, not on the card", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  /**
   * Holds the collection's `permanent_freeze_delegate` — and is neither the collection's update
   * authority, the depositor, nor the `close_seized` caller. The party that reaches in has to be
   * a wallet an assertion can name, exactly as design §3.4 names the live one.
   */
  let issuer: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;
  /** Carries the plugin, `frozen: false` at creation. */
  let collection: Address;
  let collectionAuthority: TransactionSigner;
  /** A second collection, frozen, that no asset in this file belongs to. */
  let foreignCollection: Address;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 20);
    issuer = await fundedWallet(connection, 10);

    const { usdcMint, poolCounter, operator: op } = await ensureProtocol(connection, admin);
    operator = op;

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

    // The plugin travels at creation, `frozen: false`: MPL Core adds no permanent plugin
    // afterwards, and a collection born frozen would refuse `deposit_core`'s own `TransferV1`,
    // so the position the freeze is meant to strand could never be opened. The freeze arriving
    // after escrow is not a workaround — it is the cause stated precisely.
    const core = await createCoreCollection({
      connection,
      payer: admin,
      name: "collection-level freeze",
      plugins: [{ kind: "permanentFreezeDelegate", authority: issuer.address, frozen: false }],
    });
    collection = core.collection;
    collectionAuthority = core.authority;
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection,
      standards: 0b100,
    });

    const foreign = await createCoreCollection({
      connection,
      payer: admin,
      name: "a collection no card here is in",
      plugins: [{ kind: "permanentFreezeDelegate", authority: issuer.address, frozen: true }],
    });
    foreignCollection = foreign.collection;
  });

  async function positionPdas(asset: Address) {
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(asset),
    ]);
    const { address: positionVault } = await derivePda([
      POSITION_VAULT_SEED,
      seedFromPubkey(position),
    ]);
    return { position, positionVault };
  }

  /** Escrows one genuine `AssetV1` in the plugin-carrying collection. No approval: `Pending`. */
  async function escrow(): Promise<Address> {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection,
      collectionAuthority,
      owner: depositor.address,
      plugins: [],
    });
    await sendIxs(connection, depositor, [
      await depositCore({ depositor: depositor.address, poolId, collection, asset }),
    ]);
    const { positionVault } = await positionPdas(asset);
    const escrowed = await readCoreAsset(connection, asset);
    assert.ok(escrowed, "the fixture asset vanished during the deposit");
    assert.equal(escrowed.owner, positionVault, "the fixture did not reach escrow");
    return asset;
  }

  /** Escrows, then has the operator reject it: `Rejected`, card still in the vault. */
  async function escrowAndReject(): Promise<Address> {
    const asset = await escrow();
    await sendIxs(connection, operator, [
      await rejectDeposit({
        operator: operator.address,
        pool,
        nftMint: asset,
        reason: REJECT_REASON,
      }),
    ]);
    const { position } = await positionPdas(asset);
    const rejected = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(rejected.state, "Rejected", "the fixture is not in the state under test");
    return asset;
  }

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  async function freezeCollection(frozen: boolean): Promise<void> {
    await setCollectionPermanentFreeze({
      connection,
      payer: issuer,
      collection,
      authority: issuer,
      frozen,
    });
  }

  // ── the depositor's own path: refused, then cleaned up, then claimed ────────────────────────

  it("refuses the rejected card's return with 6309 while the collection is frozen", async () => {
    const asset = await escrowAndReject();
    await freezeCollection(true);
    try {
      // The named code, and it is named *before* the CPI: without the collection read this
      // returns nothing at all from this program — MPL Core refuses the `TransferV1` from
      // inside the CPI and the caller reads an opaque revert instead of a remedy.
      await expectAnchorError(
        sendIxs(connection, depositor, [
          await returnRejectedCore({
            payer: depositor.address,
            depositor: depositor.address,
            pool,
            asset,
            collection,
          }),
        ]),
        COLLATERAL_FROZEN,
      );

      const { position } = await positionPdas(asset);
      const untouched = decodePosition((await fetchAccountData(connection, position))!);
      assert.equal(untouched.state, "Rejected", "a refused return must not move the state");
    } finally {
      await freezeCollection(false);
    }
  });

  it("seizes the rejected position the frozen collection stranded, and keeps the account", async () => {
    const asset = await escrowAndReject();
    const { position, positionVault } = await positionPdas(asset);
    await freezeCollection(true);

    try {
      // Permissionless: the caller holds no role in this pool at all, and the collateral read is
      // the whole of the authorisation.
      const caller = await fundedWallet(connection, 5);
      const signature = await sendIxs(connection, caller, [
        await closeSeized({
          caller: caller.address,
          poolId,
          asset,
          collection,
          depositor: depositor.address,
          weightIndex,
        }),
      ]);

      const seized = decodePosition((await fetchAccountData(connection, position))!);
      assert.equal(seized.state, "Seized");
      assert.equal(
        await fetchAccountData(connection, positionVault),
        null,
        "the seizure allocated the vault it only ever reads",
      );

      // The cause and the source state, from the event that is the only record of either. `Frozen`
      // rather than `Burned` is the whole point: the card is still there, in the vault, and the
      // registry that says so belongs to the collection.
      const seizedEvents = (await eventsOf(signature)).filter(isPositionSeized);
      assert.equal(seizedEvents.length, 1, "exactly one PositionSeized per seizure");
      const reported = seizedEvents[0].data;
      assert.equal(reported.kind, "Frozen");
      assert.equal(reported.sourceState, "Rejected");
      assert.equal(reported.position, position);
      assert.equal(
        reported.ownerObserved,
        positionVault,
        "a frozen card is still vault-owned — that is why no absence test can see it",
      );

      // Reversible, and the account is what makes the reversal reachable: thaw, and the claim
      // the seizure preserved succeeds.
      await freezeCollection(false);
      await sendIxs(connection, depositor, [
        await claimNftCore({ depositor: depositor.address, pool, asset, collection }),
      ]);
      const returned = await readCoreAsset(connection, asset);
      assert.ok(returned, "the claim burned the card it was supposed to return");
      assert.equal(returned.owner, depositor.address, "the thawed card did not reach its owner");
      assert.equal(
        await fetchAccountData(connection, position),
        null,
        "the claim is what closes the position, and only after the card is out",
      );
    } finally {
      await freezeCollection(false);
    }
  });

  // ── the other thing the collection authority can do to an escrowed card ─────────────────────

  /**
   * **The other thing a collection authority might do to an escrowed card — and cannot.**
   *
   * If an asset could be taken out of its collection it would declare `UpdateAuthority::Address`,
   * and MPL Core would then refuse any `TransferV1` that supplied a collection account for it. A
   * program that always supplied one would strand the position exactly as a missed freeze does,
   * and `close_seized` could not clean it up either: the card is present, vault-owned and
   * unfrozen. That is why the collateral read resolves the CPI's collection argument from the
   * asset's own `update_authority` and the exits pass through what it returns.
   *
   * **`mpl-core 0.11.1` refuses the transition outright, and this case is where that is
   * measured rather than assumed.** Every shape of `UpdateV1` answers `NotAvailable` (23): a new
   * authority of `Address` or of `None`, with the collection account readonly — the form MPL
   * Core's own client builds — or writable. Combined with `deposit_core`, which refuses an asset
   * declaring no collection at intake, that makes the no-collection arm of the read unreachable
   * for any escrowed card. The arm stays because it costs one match arm and this test is the
   * whole of what stands behind "unreachable"; if a later `mpl-core` makes the transition
   * available, this reds and points at the code that already handles it.
   */
  it("cannot be taken out of its collection — every UpdateV1 shape is refused", async () => {
    const asset = await escrow();
    const before = await readCoreAsset(connection, asset);
    assert.equal(before?.updateAuthorityTag, UPDATE_AUTHORITY_TAG.Collection);

    const shapes: ReadonlyArray<{ label: string; newUpdateAuthority: Address | null; collectionWritable: boolean }> = [
      { label: "Address, collection readonly", newUpdateAuthority: issuer.address, collectionWritable: false },
      { label: "Address, collection writable", newUpdateAuthority: issuer.address, collectionWritable: true },
      { label: "None, collection writable", newUpdateAuthority: null, collectionWritable: true },
    ];
    for (const shape of shapes) {
      await expectProgramError(
        updateCoreAssetAuthority({
          connection,
          payer: admin,
          asset,
          collection,
          collectionAuthority,
          newUpdateAuthority: shape.newUpdateAuthority,
          collectionWritable: shape.collectionWritable,
        }),
        MPL_CORE_PROGRAM,
        MPL_CORE_NOT_AVAILABLE,
      );
    }

    const after = await readCoreAsset(connection, asset);
    assert.equal(
      after?.updateAuthorityTag,
      UPDATE_AUTHORITY_TAG.Collection,
      "the card is still in its collection, so no refusal above was a partial success",
    );
    assert.equal(after?.updateAuthority, collection);
  });

  // ── the attack the collection slot would open if it were not pinned ─────────────────────────

  it("refuses a collection the asset does not declare, even though that collection is frozen", async () => {
    const asset = await escrow();
    const stranger = await fundedWallet(connection, 5);

    // `foreignCollection` really is frozen, and its registry really would read `frozen: true`.
    // The refusal is the address pin firing before the walk — without it, a permissionless
    // caller picks any frozen collection on chain and seizes a healthy position with it.
    await expectAnchorError(
      sendIxs(connection, stranger, [
        await closeSeized({
          caller: stranger.address,
          poolId,
          asset,
          collection: foreignCollection,
          depositor: depositor.address,
          weightIndex,
        }),
      ]),
      COLLECTION_NOT_ADMITTED,
    );

    // And with the card's own collection — unfrozen — the same call gets the ordinary refusal,
    // which is what proves the case above turned on the collection and not on the position.
    await expectAnchorError(
      sendIxs(connection, stranger, [
        await closeSeized({
          caller: stranger.address,
          poolId,
          asset,
          collection,
          depositor: depositor.address,
          weightIndex,
        }),
      ]),
      COLLATERAL_PRESENT,
    );

    const { position } = await positionPdas(asset);
    const untouched = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(untouched.state, "Pending", "neither refusal may move the state");
  });
});
