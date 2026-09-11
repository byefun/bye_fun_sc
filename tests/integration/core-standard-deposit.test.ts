// T-113's behavioural half: the MPL-Core deposit path, executed through `bye_machine`.
//
// T-105 measured the same custody move against MPL Core **directly**, because at that point
// `deposit_core` had no on-chain caller — a handler is not reachable from the entrypoint until
// `lib.rs` names it. That left two things unmeasured, and both are closed here.
//
// 1. **The handler's own half of the accountless premise.** MPL Core accepting a `null` PDA as
//    `newOwner` says nothing about our program producing the same call: the CPI's eight
//    positional `&AccountInfo` arguments, the record derivation and the `Position` write all sit
//    between the client and that transfer.
// 2. **The two handler-side identity checks.** T-105 injected both away and the suite stayed
//    green — there is no `Context` to drive from a unit test, and no caller existed. They are
//    pinned by source there. Here they are exercised, and the assertion is on **which code**
//    comes back: MPL Core refuses both cases itself with `NoApprovals` (26) and
//    `InvalidCollection` (19), so what these checks buy is a reason a client can act on, and a
//    test that only asserted "rejected" would pass with them deleted.
//
// The success case reads both sides of the move — `AssetV1.owner` and the vault's continued
// non-existence. A `Position` that exists proves the handler ran, never that the card moved.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createCoreCollection, mintCoreAsset, readCoreAsset } from "../helpers/core-fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { depositCore } from "../../scripts/lib/deposit.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { decodePosition } from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";

/** `Position.standard`'s Core discriminant, verbatim against `common/standards.rs`. */
const STANDARD_CORE = 2;
const POSITION_STATE_PENDING = "Pending";

/** No record exists at the address the *asset's* declared collection derives. */
const COLLECTION_NOT_ADMITTED = 6100;
/** The record is this collection's and does not carry the Core bit. */
const STANDARD_NOT_ADMITTED_FOR_COLLECTION = 6108;
/** The signer does not hold the card — our code, not MPL Core's `NoApprovals` (26). */
const NOT_POSITION_OWNER = 6302;

describe("T-113 — deposit_core through bye_machine, against a genuine AssetV1", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let depositor: TransactionSigner;
  let poolId: number;
  let pool: Address;
  /** Admitted for Core only (`0b100`) — the shape `CCryptUfe…9CRac` will launch with. */
  let coreCollection: Address;
  let coreAuthority: TransactionSigner;
  /** A second real Core collection, admitted for pNFT and legacy but **not** Core (`0b011`).
   * Same pool, so the wrong-pairing case differs from the success case in exactly one bit. */
  let tmOnlyCollection: Address;
  let tmOnlyAuthority: TransactionSigner;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 20);

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

    const core = await createCoreCollection({ connection, payer: admin, name: "T-113 core" });
    coreCollection = core.collection;
    coreAuthority = core.authority;
    const tmOnly = await createCoreCollection({ connection, payer: admin, name: "T-113 tm-only" });
    tmOnlyCollection = tmOnly.collection;
    tmOnlyAuthority = tmOnly.authority;

    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: coreCollection,
      standards: 0b100,
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: tmOnlyCollection,
      standards: 0b011,
    });
  });

  async function mint(collection: Address, authority: TransactionSigner, owner?: Address) {
    return mintCoreAsset({
      connection,
      payer: admin,
      collection,
      collectionAuthority: authority,
      owner: owner ?? depositor.address,
    });
  }

  /**
   * `collection` is deliberately a parameter rather than derived from the asset: the encoder
   * builds the `pool_collection` account from it, so a caller can present a real, self-consistent
   * record for a collection the asset does not declare — the T-101 fault, on chain.
   */
  async function send(asset: Address, collection: Address, signer: TransactionSigner = depositor) {
    return sendIxs(connection, signer, [
      await depositCore({ depositor: signer.address, poolId, collection, asset }),
    ]);
  }

  async function vaultOf(asset: Address) {
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(asset),
    ]);
    const { address: vault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    return { position, vault };
  }

  it("escrows a Core asset into the accountless vault and records standard 2", async () => {
    const asset = await mint(coreCollection, coreAuthority);
    const { position, vault } = await vaultOf(asset);

    assert.equal(
      await fetchAccountData(connection, vault),
      null,
      "the vault PDA must not exist before the deposit, or the case below proves nothing about " +
        "an accountless escrow",
    );

    await send(asset, coreCollection);

    const data = await fetchAccountData(connection, position);
    assert.ok(data, "the Core deposit created no position account");
    const decoded = decodePosition(data);
    assert.equal(decoded.standard, STANDARD_CORE);
    assert.equal(decoded.state, POSITION_STATE_PENDING);
    // On this family the `AssetV1` account *is* the instrument; `nft_mint` names it because the
    // field is a shipped layout three TS decoders read, not because a mint exists.
    assert.equal(decoded.nftMint, asset);
    assert.equal(decoded.depositor, depositor.address);

    // The custody half. The card must be owned by the vault, and the depositor must have lost it
    // — an owner field rewritten without a real transfer satisfies neither on its own.
    const read = await readCoreAsset(connection, asset);
    assert.ok(read, "the asset account must survive the transfer");
    assert.equal(read.owner, vault, "AssetV1.owner is the position's vault PDA");
    assert.notEqual(read.owner, depositor.address);

    assert.equal(
      await fetchAccountData(connection, vault),
      null,
      "the vault must still not exist — a lazily allocated account would mean every Core " +
        "deposit pays rent the design says it does not, and would leave the escrow closable by " +
        "whoever funded it",
    );
  });

  it("refuses a Core asset from a collection admitted for the Token Metadata standards only", async () => {
    const asset = await mint(tmOnlyCollection, tmOnlyAuthority);
    await expectAnchorError(send(asset, tmOnlyCollection), STANDARD_NOT_ADMITTED_FOR_COLLECTION);
  });

  /**
   * **The discriminating case, on chain.** Both collections are admitted and both records are
   * real; the asset declares `coreCollection` while the caller presents `tmOnlyCollection`'s
   * record — an account that is entirely self-consistent, its own `collection` field and its own
   * address agreeing. Only a derivation seeded on what the *asset* declares separates them.
   * Deriving from the record instead accepts, and the pool takes a card under a record it was
   * never admitted by.
   */
  it("refuses a self-consistent record for a collection the asset does not declare", async () => {
    const asset = await mint(coreCollection, coreAuthority);
    await expectAnchorError(send(asset, tmOnlyCollection), COLLECTION_NOT_ADMITTED);
    const read = await readCoreAsset(connection, asset);
    assert.ok(read);
    assert.equal(read.owner, depositor.address, "the card must not have moved");
  });

  /**
   * The collection-account pin, which the encoder cannot express: it derives both the record and
   * the `collection` slot from one parameter, so the mismatch has to be built by hand. The record
   * presented here is the **right** one — derived from what the asset declares, carrying the Core
   * bit — and only the CPI's collection account is wrong, which is why this case reaches a check
   * the one above cannot. MPL Core would refuse it too, with `InvalidCollection` (19); 6100 is
   * what says our own pin ran first.
   */
  it("refuses a collection account that is not the one the asset declares", async () => {
    const asset = await mint(coreCollection, coreAuthority);
    const ix = await depositCore({
      depositor: depositor.address,
      poolId,
      collection: coreCollection,
      asset,
    });
    const accounts = [...(ix.accounts ?? [])];
    assert.equal(accounts[7].address, coreCollection, "slot 7 is the collection — the shape moved");
    accounts[7] = { ...accounts[7], address: tmOnlyCollection };

    await expectAnchorError(
      sendIxs(connection, depositor, [{ ...ix, accounts }]),
      COLLECTION_NOT_ADMITTED,
    );
    const read = await readCoreAsset(connection, asset);
    assert.ok(read);
    assert.equal(read.owner, depositor.address, "the card must not have moved");
  });

  /**
   * The depositor-owns-the-card check, at its real weight. MPL Core refuses this itself with
   * `NoApprovals` (26) — so the assertion that matters is the **code**: 6302 means our check ran
   * first and told the client something actionable, and a bare 26 means it was deleted.
   */
  it("refuses a depositor who does not hold the card, with our code rather than MPL Core's", async () => {
    const stranger = await fundedWallet(connection, 5);
    const asset = await mint(coreCollection, coreAuthority, stranger.address);
    await expectAnchorError(send(asset, coreCollection), NOT_POSITION_OWNER);
  });
});
