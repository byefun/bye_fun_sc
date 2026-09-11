// Codec tests for tests/helpers/fixtures.ts — the pNFT fixture minting helpers (T-47).
// Golden bytes are built with raw Buffer writes / a second, independent PDA derivation, never by
// calling back into fixtures.ts's own writer — oracle separation, mirroring
// tests/unit/weight-index.test.ts's and tests/unit/admin.test.ts's pattern. Everything here runs
// with no validator: PDA derivation is pure crypto, and the `mint*` functions that actually send
// transactions are exercised only by the integration suite (T-46's validator).

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { address, getAddressEncoder, getProgramDerivedAddress } from "@solana/kit";
import type { Address } from "@solana/kit";
import {
  CREATE_V1_DISCRIMINATOR,
  MINT_V1_DISCRIMINATOR,
  RULE_SET_ADDRESS,
  TOKEN_STANDARD_TAG,
  VERIFY_COLLECTION_V1_DISCRIMINATOR,
  encodeCreateV1Args,
  encodeMintV1Args,
  encodeOptionCollection,
  masterEditionPda,
  metadataPda,
  mplIxData,
  tokenRecordPda,
} from "../helpers/fixtures.js";
import { TOKEN_METADATA_PROGRAM } from "../../scripts/constants.js";

const addrEnc = getAddressEncoder();
const pubkeyBuf = (a: Address) => Buffer.from(addrEnc.encode(a));
const u16Buf = (n: number) => {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return b;
};
const u64Buf = (n: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(n);
  return b;
};
const stringBuf = (s: string) => {
  const utf8 = Buffer.from(s, "utf8");
  const len = Buffer.alloc(4);
  len.writeUInt32LE(utf8.length);
  return Buffer.concat([len, utf8]);
};

const MINT = address("11111111111111111111111111111112");
const COLLECTION_KEY = address("So11111111111111111111111111111111111111112");

// ── mplIxData ────────────────────────────────────────────────────────────────────────────────────

describe("mplIxData", () => {
  it("is the two discriminator bytes with no args", () => {
    assert.deepEqual([...mplIxData(CREATE_V1_DISCRIMINATOR)], [42, 0]);
    assert.deepEqual([...mplIxData(MINT_V1_DISCRIMINATOR)], [43, 0]);
    assert.deepEqual([...mplIxData(VERIFY_COLLECTION_V1_DISCRIMINATOR)], [52, 1]);
  });

  it("appends args after the two discriminator bytes", () => {
    const args = Buffer.from([9, 9, 9]);
    assert.deepEqual([...mplIxData(CREATE_V1_DISCRIMINATOR, args)], [42, 0, 9, 9, 9]);
  });
});

// ── encodeOptionCollection ───────────────────────────────────────────────────────────────────────

describe("encodeOptionCollection", () => {
  it("encodes null as a single None tag byte", () => {
    assert.deepEqual([...encodeOptionCollection(null)], [0]);
  });

  it("encodes Some as tag ‖ verified(bool) ‖ key(pubkey) — verified before key", () => {
    const got = encodeOptionCollection({ key: COLLECTION_KEY, verified: true });
    const want = Buffer.concat([Buffer.from([1, 1]), pubkeyBuf(COLLECTION_KEY)]);
    assert.deepEqual([...got], [...want]);
  });

  it("verified: false serializes the bool as 0, not omitted", () => {
    const got = encodeOptionCollection({ key: COLLECTION_KEY, verified: false });
    const want = Buffer.concat([Buffer.from([1, 0]), pubkeyBuf(COLLECTION_KEY)]);
    assert.deepEqual([...got], [...want]);
  });
});

// ── TOKEN_STANDARD_TAG ───────────────────────────────────────────────────────────────────────────

describe("TOKEN_STANDARD_TAG", () => {
  it("matches TokenStandard's declaration order (generated/types/token_standard.rs)", () => {
    // NonFungible, FungibleAsset, Fungible, NonFungibleEdition, ProgrammableNonFungible, ...
    assert.equal(TOKEN_STANDARD_TAG.NonFungible, 0);
    assert.equal(TOKEN_STANDARD_TAG.ProgrammableNonFungible, 4);
  });
});

// ── encodeCreateV1Args — golden bytes, independently assembled ─────────────────────────────────

describe("encodeCreateV1Args", () => {
  it("encodes a programmable fixture: name, empty symbol/uri, collection Some, rule_set Some", () => {
    const got = encodeCreateV1Args({
      name: "x",
      tokenStandard: "ProgrammableNonFungible",
      collection: { key: COLLECTION_KEY, verified: true },
      ruleSet: RULE_SET_ADDRESS,
    });
    const want = Buffer.concat([
      stringBuf("x"), // name
      stringBuf(""), // symbol
      stringBuf(""), // uri
      u16Buf(0), // seller_fee_basis_points
      Buffer.from([0]), // creators: None
      Buffer.from([0]), // primary_sale_happened: false
      Buffer.from([1]), // is_mutable: true
      Buffer.from([4]), // token_standard: ProgrammableNonFungible
      Buffer.from([1, 1]), // collection: Some{ verified: true, ...
      pubkeyBuf(COLLECTION_KEY), //   ...key }
      Buffer.from([0]), // uses: None
      Buffer.from([0]), // collection_details: None
      Buffer.from([1]), // rule_set: Some(...
      pubkeyBuf(RULE_SET_ADDRESS), //   ...)
      Buffer.from([1, 0]), // decimals: Some(0)
      Buffer.from([1, 0]), // print_supply: Some(PrintSupply::Zero)
    ]);
    assert.deepEqual([...got], [...want]);
  });

  it("encodes collection: None and rule_set: None as single tag bytes", () => {
    const got = encodeCreateV1Args({
      name: "",
      tokenStandard: "NonFungible",
      collection: null,
      ruleSet: null,
    });
    const want = Buffer.concat([
      stringBuf(""),
      stringBuf(""),
      stringBuf(""),
      u16Buf(0),
      Buffer.from([0, 0, 1, 0]), // creators:None, primary_sale_happened:false, is_mutable:true, standard:0
      Buffer.from([0]), // collection: None
      Buffer.from([0]), // uses: None
      Buffer.from([0]), // collection_details: None
      Buffer.from([0]), // rule_set: None
      Buffer.from([1, 0]), // decimals: Some(0)
      Buffer.from([1, 0]), // print_supply: Some(Zero)
    ]);
    assert.deepEqual([...got], [...want]);
  });
});

// ── encodeMintV1Args ─────────────────────────────────────────────────────────────────────────────

describe("encodeMintV1Args", () => {
  it("encodes amount ‖ authorization_data: None", () => {
    const got = encodeMintV1Args(1n);
    const want = Buffer.concat([u64Buf(1n), Buffer.from([0])]);
    assert.deepEqual([...got], [...want]);
  });

  it("amount is not hardcoded to 1 — a different value round-trips too", () => {
    const got = encodeMintV1Args(7n);
    const want = Buffer.concat([u64Buf(7n), Buffer.from([0])]);
    assert.deepEqual([...got], [...want]);
  });
});

// ── PDAs — cross-checked against a second, independent derivation ──────────────────────────────

async function derivePdaIndependently(seeds: (Uint8Array | string)[]): Promise<Address> {
  const [pda] = await getProgramDerivedAddress({
    programAddress: TOKEN_METADATA_PROGRAM,
    seeds: seeds.map((s) => (typeof s === "string" ? new Uint8Array(Buffer.from(s, "utf8")) : s)),
  });
  return pda;
}

describe("metadataPda / masterEditionPda / tokenRecordPda", () => {
  it("metadataPda matches an independently-derived ['metadata', program, mint] PDA", async () => {
    const want = await derivePdaIndependently(["metadata", pubkeyBuf(TOKEN_METADATA_PROGRAM), pubkeyBuf(MINT)]);
    assert.equal(await metadataPda(MINT), want);
  });

  it("masterEditionPda appends the 'edition' seed", async () => {
    const want = await derivePdaIndependently([
      "metadata",
      pubkeyBuf(TOKEN_METADATA_PROGRAM),
      pubkeyBuf(MINT),
      "edition",
    ]);
    assert.equal(await masterEditionPda(MINT), want);
  });

  it("tokenRecordPda appends 'token_record' ‖ token, distinct from masterEditionPda", async () => {
    const token = COLLECTION_KEY;
    const want = await derivePdaIndependently([
      "metadata",
      pubkeyBuf(TOKEN_METADATA_PROGRAM),
      pubkeyBuf(MINT),
      "token_record",
      pubkeyBuf(token),
    ]);
    const got = await tokenRecordPda(MINT, token);
    assert.equal(got, want);
    assert.notEqual(got, await masterEditionPda(MINT));
  });

  it("different mints derive different metadata PDAs", async () => {
    const other = COLLECTION_KEY;
    assert.notEqual(await metadataPda(MINT), await metadataPda(other));
  });
});
