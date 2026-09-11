// T-105 — the phase's largest single risk, measured before a handler was written around it.
//
// The Core escrow shape has no vault *token account*: `PDA(["vault", position])` holds the asset
// directly as `AssetV1.owner`, and that PDA is never allocated. architecture §3.4 marks the
// premise `Unverified` — whether MPL Core `TransferV1` accepts a `getAccountInfo → null` PDA as
// `[4] newOwner` is asserted by no primary source — and T-105's row rules that every Core exit is
// a mirror of this move, so all four fail together if it does not hold.
//
// The question is about the **dependency**, so these cases call MPL Core directly rather than
// through `bye_machine`: routing it through our own program would leave a failure ambiguous
// between the two, and `deposit_core` had no on-chain caller at all when these were written (its
// wiring is T-113's, and `core-standard-deposit.test.ts` is the same move made through it).
// Against genuine `CollectionV1`/`AssetV1` accounts created by the real program, per the row —
// `fixtures.ts`'s `mintMplCoreLikeAsset` is a system-created account assigned to MPL Core, which
// proves an owner check and could never be transferred at all.
//
// MEASURED RESULT: it holds. `TransferV1` accepts the accountless PDA, writes it into
// `AssetV1.owner`, and **does not allocate it** — the PDA still reads `null` afterwards, so the
// escrow really is accountless rather than lazily created. Custody moves for real: the previous
// owner's signature is refused after the move.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { generateKeyPairSigner } from "@solana/kit";
import {
  assertProgramIsLive,
  fundedWallet,
  getConnection,
  PROGRAM_ID,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import {
  createCoreCollection,
  mintCoreAsset,
  readCoreAsset,
  transferCoreAsset,
  CORE_KEY,
  UPDATE_AUTHORITY_TAG,
} from "../helpers/core-fixtures.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { fetchAccountData } from "../../scripts/_common.js";

describe("T-105 — MPL Core TransferV1 against an accountless PDA as newOwner", () => {
  let connection: Connection;
  let payer: TransactionSigner;
  let holder: TransactionSigner;
  let collection: Address;
  let collectionAuthority: TransactionSigner;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    payer = await fundedWallet(connection);
    holder = await fundedWallet(connection);
    const created = await createCoreCollection({ connection, payer });
    collection = created.collection;
    collectionAuthority = created.authority;
  });

  /// The read `deposit_core` is built on. `UpdateAuthority::Collection` is the *only* arm that
  /// names a collection — spec defect 4's `grouping.collection` is a DAS field with no on-chain
  /// existence — and MPL Core requires the collection's own authority to sign the mint, which is
  /// what makes the arm a claim the program vouches for rather than one a depositor asserts.
  it("a genuine AssetV1 reports its collection through UpdateAuthority::Collection", async () => {
    const asset = await mintCoreAsset({
      connection,
      payer,
      collection,
      collectionAuthority,
      owner: holder.address,
    });

    const read = await readCoreAsset(connection, asset);
    assert.ok(read, "the asset account should exist");
    assert.equal(read.key, CORE_KEY.AssetV1, "leading discriminator byte");
    assert.equal(read.owner, holder.address);
    assert.equal(read.updateAuthorityTag, UPDATE_AUTHORITY_TAG.Collection);
    assert.equal(read.updateAuthority, collection);
  });

  it("TransferV1 accepts an accountless PDA as newOwner and does not allocate it", async () => {
    const asset = await mintCoreAsset({
      connection,
      payer,
      collection,
      collectionAuthority,
      owner: holder.address,
    });

    // Derived exactly as the program will derive it. The position need not exist for the probe —
    // what is under test is the vault PDA's own absence, so a stand-in seed is the honest choice.
    const position = await generateKeyPairSigner();
    const { address: vault } = await derivePda(
      ["vault", seedFromPubkey(position.address)],
      PROGRAM_ID,
    );

    assert.equal(
      await fetchAccountData(connection, vault),
      null,
      "the premise: the vault PDA must have no account before the transfer, or this case is " +
        "measuring an ordinary transfer to an existing account and proves nothing",
    );

    await transferCoreAsset({
      connection,
      payer,
      asset,
      collection,
      authority: holder,
      newOwner: vault,
    });

    const read = await readCoreAsset(connection, asset);
    assert.ok(read, "the asset account should still exist after a transfer");
    assert.equal(read.owner, vault, "AssetV1.owner is the accountless PDA");

    assert.equal(
      await fetchAccountData(connection, vault),
      null,
      "MPL Core must not have allocated the new owner — a lazily created account would mean " +
        "every Core deposit pays rent the design says it does not, and would make the vault " +
        "closable by whoever funded it",
    );
  });

  /// The owner field is not cosmetic. Without this, a `TransferV1` that silently no-opped on an
  /// unallocated destination would satisfy the case above on the read alone.
  it("the previous owner cannot move the asset once the vault PDA holds it", async () => {
    const asset = await mintCoreAsset({
      connection,
      payer,
      collection,
      collectionAuthority,
      owner: holder.address,
    });
    const position = await generateKeyPairSigner();
    const { address: vault } = await derivePda(
      ["vault", seedFromPubkey(position.address)],
      PROGRAM_ID,
    );
    await transferCoreAsset({
      connection,
      payer,
      asset,
      collection,
      authority: holder,
      newOwner: vault,
    });

    const stranger = await fundedWallet(connection);
    await assert.rejects(
      transferCoreAsset({
        connection,
        payer,
        asset,
        collection,
        authority: holder,
        newOwner: stranger.address,
      }),
      "the depositor signed as authority over an asset the vault PDA now owns; MPL Core must " +
        "refuse, or escrow is not escrow",
    );

    const read = await readCoreAsset(connection, asset);
    assert.ok(read);
    assert.equal(read.owner, vault, "the card is still in the vault");
  });
});
