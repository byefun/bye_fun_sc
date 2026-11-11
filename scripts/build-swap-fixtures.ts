/**
 * Regenerates the three account fixtures the integration validator loads for the blank
 * settlement path: the two mints, and the DAMM v2 pool pinned as `swap_pool`.
 *
 * Run with:  pnpm exec tsx scripts/build-swap-fixtures.ts
 *
 * **These are accounts, not programs** — loaded with `--account`, exactly as the pNFT rule-set
 * account is. The venue program is never deployed to the test validator, because the eligibility
 * checks read the pool account and derive vault addresses from it; none of them invokes the venue
 * or reads a vault, so its code is not needed to exercise them. Swapping is a separate concern.
 *
 * **Addresses are arbitrary and nothing ever signs for them.** `--account` loads state, not a
 * keypair, so the pool and the two mints need no secret key to exist on the validator. They are
 * written as literals here so the fixture bytes and the test helpers cannot drift apart.
 *
 * **The pool is a real mainnet pool with its identity fields rewritten.** Zero-filling the fields
 * this program does not read would leave the fee schedule, the liquidity and the curve bounds at
 * values no pool ever holds, and the moment anything exercises the swap itself that fixture would
 * have to be rebuilt from scratch. Starting from a pool a launchpad actually created keeps those
 * fields plausible; only the mints, the vaults, the price and the activation point are replaced.
 */
import { Buffer } from "node:buffer";
import { writeFileSync } from "node:fs";

import { derivePda } from "./lib/anchor";
import type { Address } from "@solana/kit";

/** Meteora DAMM v2. Mirrors `common/damm_v2.rs`. */
const CP_AMM = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG" as Address;
const TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" as Address;

/** `accounts/dev/deployer.json`, which every integration test signs as. */
const MINT_AUTHORITY = "DCe2NVuNGg8yqNPZq5PChJtjJV7DRPVxhQr6eSVy11np" as Address;

import {
  BYE_MINT_FIXTURE as BYE_MINT,
  SWAP_POOL_FIXTURE as SWAP_POOL,
  USDC_MINT_FIXTURE as USDC_MINT,
} from "./lib/swap-fixtures";

/** $BYE is nine decimals; USDC is six. */
const BYE_DECIMALS = 9;
const USDC_DECIMALS = 6;

/**
 * $BYE at 0.01 USDC with $BYE as token A — the same value the Rust unit vectors price against,
 * so an integration expectation and a unit expectation cannot disagree about the fixture's price.
 */
const SQRT_PRICE = 58_333_726_687_135_160n;

// Byte offsets into the venue's pool account. Mirrors `common/damm_v2.rs`.
const OFF_TOKEN_A_MINT = 168;
const OFF_TOKEN_B_MINT = 200;
const OFF_TOKEN_A_VAULT = 232;
const OFF_TOKEN_B_VAULT = 264;
const OFF_SQRT_PRICE = 456;
const OFF_ACTIVATION_POINT = 472;
const OFF_POOL_STATUS = 481;
const POOL_LEN = 1112;

/**
 * `HXnzoANseBAVst92PDJU6LsgZbpFpGzVShcXsM2B7PMb` as it stood on mainnet — a pool a launchpad
 * created for its own token against real USDC, so its fee schedule and curve bounds are the shape
 * the pinned pool will have.
 */
const POOL_TEMPLATE_BASE64 =
  "8ZptBBGxbbwAZc0dAAAAAAEAAAAAAB4APAAAAAAAAABSBgAAAAAAAAAAAAAAAAAAFAAUAAAAAAABAAAAAAAAAGCk3ADvAAAAAQAK" +
  "AHgAiBOEaadqAAAAAMsQx7q4jQYAAAAAAAAAAACZp0ehvmI4AAAAAAAAAAAAoPlWAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" +
  "AAACh6px001iAAAAAAAAAAAADQczYe/dMNR0jAE9GxXNVdMCaOSBNCgMk5QwynmIYunG+nrzvtutOj1l82qryXQxsbvkwtL24OR8" +
  "pgIDRS9dYYd8ox+BZ/vwMb5YgrWwAI0h7m0x4ACzVWI8Z1naY9U3+cxVY0xgMV2byK3Pw3a7aWpkJRaj8H3iuBm6e3ep9K4AAAAA" +
  "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAQoRkg+og74UjZ3vrSBsA" +
  "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACn6FAIAAAAAAAAAAAAAAAAAAAAAAAAAAFA7AQABAAAAAAAAAAAAAACbV2lOqRpchLHE" +
  "/v8AAAAAeDcj5VjSNgAAAAAAAAAAAIMfcmkAAAAAAQAAAAEBAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAI69QMxF" +
  "v30AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAPJJpnDQAAAAAAAAAAAAAA" +
  "AAAAAAAAAAAgHQMOAwAAAAAAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAAAKYhHWedwST+OMyap5B3lz/Qpwn7Ucfg0de78" +
  "spOYtC19aZGPYml/ADndzNcFAAAAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" +
  "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" +
  "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" +
  "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" +
  "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" +
  "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

function encodeAddress(a: Address): Uint8Array {
  // Base58 decode, kept local so the generator needs no codec import beyond the PDA helper.
  const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let n = 0n;
  for (const ch of a) {
    const i = ALPHABET.indexOf(ch);
    if (i < 0) throw new Error(`not base58: ${a}`);
    n = n * 58n + BigInt(i);
  }
  const bytes: number[] = [];
  while (n > 0n) {
    bytes.unshift(Number(n & 0xffn));
    n >>= 8n;
  }
  for (const ch of a) {
    if (ch !== "1") break;
    bytes.unshift(0);
  }
  if (bytes.length !== 32) throw new Error(`${a} decodes to ${bytes.length} bytes, expected 32`);
  return new Uint8Array(bytes);
}

/** An SPL token mint, initialized, with no freeze authority. */
function mintAccount(decimals: number): Buffer {
  const data = Buffer.alloc(82);
  data.writeUInt32LE(1, 0); // mint_authority: Some
  Buffer.from(encodeAddress(MINT_AUTHORITY)).copy(data, 4);
  data.writeBigUInt64LE(0n, 36); // supply
  data.writeUInt8(decimals, 44);
  data.writeUInt8(1, 45); // is_initialized
  data.writeUInt32LE(0, 46); // freeze_authority: None
  return data;
}

function accountJson(params: {
  pubkey: Address;
  owner: Address;
  data: Buffer;
  lamports: number;
}): string {
  return `${JSON.stringify(
    {
      pubkey: params.pubkey,
      account: {
        lamports: params.lamports,
        data: [params.data.toString("base64"), "base64"],
        owner: params.owner,
        executable: false,
        rentEpoch: 0,
      },
    },
    null,
    2,
  )}\n`;
}

async function main(): Promise<void> {
  const template = Buffer.from(POOL_TEMPLATE_BASE64, "base64");
  if (template.length !== POOL_LEN) {
    throw new Error(`template is ${template.length} bytes, expected ${POOL_LEN}`);
  }

  const pool = Buffer.from(template);
  Buffer.from(encodeAddress(BYE_MINT)).copy(pool, OFF_TOKEN_A_MINT);
  Buffer.from(encodeAddress(USDC_MINT)).copy(pool, OFF_TOKEN_B_MINT);

  for (const [mint, offset] of [
    [BYE_MINT, OFF_TOKEN_A_VAULT],
    [USDC_MINT, OFF_TOKEN_B_VAULT],
  ] as const) {
    const { address } = await derivePda(["token_vault", encodeAddress(mint), encodeAddress(SWAP_POOL)], CP_AMM);
    Buffer.from(encodeAddress(address)).copy(pool, offset);
  }

  pool.writeBigUInt64LE(SQRT_PRICE & 0xffffffffffffffffn, OFF_SQRT_PRICE);
  pool.writeBigUInt64LE(SQRT_PRICE >> 64n, OFF_SQRT_PRICE + 8);
  // Zero so the pool is past its activation point under any clock the validator reports.
  pool.writeBigUInt64LE(0n, OFF_ACTIVATION_POINT);
  if (pool.readUInt8(OFF_POOL_STATUS) !== 0) {
    throw new Error("template pool is not Enable — pick a template that is");
  }

  writeFileSync(
    "tests/fixtures/swap/damm-v2-pool.json",
    accountJson({ pubkey: SWAP_POOL, owner: CP_AMM, data: pool, lamports: 8_630_400 }),
  );
  writeFileSync(
    "tests/fixtures/swap/bye-mint.json",
    accountJson({ pubkey: BYE_MINT, owner: TOKEN_PROGRAM, data: mintAccount(BYE_DECIMALS), lamports: 1_461_600 }),
  );
  writeFileSync(
    "tests/fixtures/swap/usdc-mint.json",
    accountJson({ pubkey: USDC_MINT, owner: TOKEN_PROGRAM, data: mintAccount(USDC_DECIMALS), lamports: 1_461_600 }),
  );

  console.log("wrote tests/fixtures/swap/{damm-v2-pool,bye-mint,usdc-mint}.json");
}

await main();
