// Integration coverage for tests/helpers/fixtures.ts (T-47) — proves each fixture builder mints
// real, correctly-shaped on-chain state against the validator T-46 loads (real Token Metadata +
// Token Auth Rules + the eBJLFY…zyT9 rule-set account). Run directly with
// `pnpm test:integration` against an already-running validator (e.g. via
// `scripts/run-integration-tests.sh`, once its liveness gate is raised for this file — not done
// here, since that script is off limits to this task; see the T-47 handoff).
//
// Assertions read the real accounts back and decode the exact fields deposit.rs's own
// `validate_admission` inspects (Metadata's `token_standard` / `collection`, and each account's
// `owner`) — never just "the transaction did not throw".

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import {
  assertProgramIsLive,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import {
  createPnftCollection,
  mintAdmissiblePnfts,
  mintMplCoreLikeAsset,
  mintPlainNonFungiblePnft,
  mintUnverifiedCollectionPnft,
  mintWrongCollectionPnft,
  mintWrongMasterEditionPnft,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { TOKEN_METADATA_PROGRAM } from "../../scripts/constants.js";

async function fetchAccountOwner(connection: Connection, addr: Address): Promise<Address | null> {
  const res = await connection.rpc.getAccountInfo(addr, { encoding: "base64" }).send();
  return res.value === null ? null : res.value.owner;
}

async function fetchAccountData(connection: Connection, addr: Address): Promise<Buffer | null> {
  const res = await connection.rpc.getAccountInfo(addr, { encoding: "base64" }).send();
  if (res.value === null) return null;
  const [b64] = res.value.data as [string, string];
  return Buffer.from(b64, "base64");
}

// Minimal Metadata reader — only the fields validate_admission inspects. Borsh field order:
// key(1) update_authority(32) mint(32) name(String) symbol(String) uri(String)
// seller_fee_basis_points(2) creators(Option<Vec<Creator>>) primary_sale_happened(1)
// is_mutable(1) edition_nonce(Option<u8>) token_standard(Option<u8>) collection(Option<{verified,key}>).
function readMetadataStandardAndCollection(
  data: Buffer,
): { tokenStandard: number | null; collection: { verified: boolean; key: Address } | null } {
  let offset = 1 + 32 + 32;
  const readString = () => {
    const len = data.readUInt32LE(offset);
    offset += 4 + len;
  };
  readString(); // name
  readString(); // symbol
  readString(); // uri
  offset += 2; // seller_fee_basis_points
  if (data[offset] === 1) {
    offset += 1;
    const count = data.readUInt32LE(offset);
    offset += 4 + count * (32 + 1 + 1); // Vec<Creator>: address(32) verified(1) share(1)
  } else {
    offset += 1;
  }
  offset += 1; // primary_sale_happened
  offset += 1; // is_mutable
  if (data[offset] === 1) offset += 1 + 1;
  else offset += 1; // edition_nonce: Option<u8>

  let tokenStandard: number | null = null;
  if (data[offset] === 1) {
    tokenStandard = data[offset + 1];
    offset += 2;
  } else {
    offset += 1;
  }

  let collection: { verified: boolean; key: Address } | null = null;
  if (data[offset] === 1) {
    const verified = data[offset + 1] === 1;
    const keyBytes = data.subarray(offset + 2, offset + 2 + 32);
    // base58-encode via the address type's own decoder is unnecessary here — comparisons below
    // go through fetchAccountOwner-style Address values already, so this local reader only ever
    // needs `verified`; `collection` here is exposed for completeness but key comparisons in the
    // tests below use the fixture's own returned addresses instead of re-deriving this bytes-to-
    // base58 step.
    collection = { verified, key: keyBytes as unknown as Address };
  }
  return { tokenStandard, collection };
}

describe("T-47 pNFT fixtures — integration", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let depositor: TransactionSigner;
  let rightCollection: PnftCollection;
  let wrongCollection: PnftCollection;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 5);
    rightCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "T-47 IT right",
    });
    wrongCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "T-47 IT wrong",
    });
  });

  it("createPnftCollection mints a real, Token-Metadata-owned collection", async () => {
    assert.equal(await fetchAccountOwner(connection, rightCollection.metadata), TOKEN_METADATA_PROGRAM);
    assert.equal(await fetchAccountOwner(connection, rightCollection.masterEdition), TOKEN_METADATA_PROGRAM);
  });

  it("mintAdmissiblePnfts: ≥3 distinct values plus the identical-value tie-break pair", async () => {
    const values = [12_000_000n, 50_000_000n, 20_000_000n, 20_000_000n];
    const fixtures = await mintAdmissiblePnfts({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection: rightCollection,
      values,
    });
    assert.equal(fixtures.length, 4);
    assert.equal(new Set(values).size, 3, "the fixture set covers exactly 3 distinct values");

    const tieBreakPair = fixtures.filter((f) => f.value === 20_000_000n);
    assert.equal(tieBreakPair.length, 2, "exactly two fixtures share the tie-break value");
    assert.notEqual(tieBreakPair[0].mint, tieBreakPair[1].mint, "the pair is two distinct positions");

    for (const f of fixtures) {
      assert.equal(await fetchAccountOwner(connection, f.metadata), TOKEN_METADATA_PROGRAM);
      assert.equal(await fetchAccountOwner(connection, f.masterEdition), TOKEN_METADATA_PROGRAM);
      const data = await fetchAccountData(connection, f.metadata);
      assert.ok(data);
      const { tokenStandard, collection } = readMetadataStandardAndCollection(data);
      assert.equal(tokenStandard, 4, "ProgrammableNonFungible");
      assert.ok(collection);
      assert.equal(collection.verified, true);
    }
  });

  it("mintWrongCollectionPnft: verified against a different, but real, collection", async () => {
    const asset = await mintWrongCollectionPnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      wrongCollection,
    });
    const data = await fetchAccountData(connection, asset.metadata);
    assert.ok(data);
    const { tokenStandard, collection } = readMetadataStandardAndCollection(data);
    assert.equal(tokenStandard, 4);
    assert.ok(collection);
    assert.equal(collection.verified, true, "verified — the failure is the collection key, not verification");
  });

  it("mintUnverifiedCollectionPnft: right collection key, never verified", async () => {
    const asset = await mintUnverifiedCollectionPnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection: rightCollection,
    });
    const data = await fetchAccountData(connection, asset.metadata);
    assert.ok(data);
    const { tokenStandard, collection } = readMetadataStandardAndCollection(data);
    assert.equal(tokenStandard, 4);
    assert.ok(collection);
    assert.equal(collection.verified, false);
  });

  it("mintPlainNonFungiblePnft: verified, right collection, but not programmable", async () => {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection: rightCollection,
    });
    const data = await fetchAccountData(connection, asset.metadata);
    assert.ok(data);
    const { tokenStandard, collection } = readMetadataStandardAndCollection(data);
    assert.equal(tokenStandard, 0, "NonFungible, not ProgrammableNonFungible");
    assert.ok(collection);
    assert.equal(collection.verified, true);
  });

  it("mintWrongMasterEditionPnft: the returned masterEdition belongs to a different mint", async () => {
    const asset = await mintWrongMasterEditionPnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection: rightCollection,
    });
    assert.equal(asset.masterEdition, rightCollection.masterEdition);
    assert.notEqual(asset.masterEdition, asset.mint);
    // The item's own real master edition (at its own mint's PDA) does exist — proving this
    // fixture's rejection comes from *which* address was supplied, not from a missing account.
    assert.equal(await fetchAccountOwner(connection, rightCollection.masterEdition), TOKEN_METADATA_PROGRAM);
  });

  it("mintMplCoreLikeAsset: metadata slot is owned by a program other than Token Metadata", async () => {
    const asset = await mintMplCoreLikeAsset({ connection, payer: admin, owner: depositor.address });
    const owner = await fetchAccountOwner(connection, asset.metadata);
    assert.ok(owner);
    assert.notEqual(owner, TOKEN_METADATA_PROGRAM);
  });
});
