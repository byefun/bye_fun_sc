// The fixture addresses are written down three times — in `scripts/lib/swap-fixtures.ts`, in the
// committed account files, and as shell literals in `scripts/run-integration-tests.sh`. Two of
// them agreeing proves nothing about the third, and a mismatch surfaces only as a validator that
// boots without the account a test then fails to find. All three are compared here.
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  BYE_MINT_FIXTURE,
  SWAP_POOL_FIXTURE,
  USDC_MINT_FIXTURE,
} from "../../scripts/lib/swap-fixtures.js";

const CASES = [
  { name: "pool", constant: SWAP_POOL_FIXTURE, file: "tests/fixtures/swap/damm-v2-pool.json" },
  { name: "$BYE mint", constant: BYE_MINT_FIXTURE, file: "tests/fixtures/swap/bye-mint.json" },
  { name: "USDC mint", constant: USDC_MINT_FIXTURE, file: "tests/fixtures/swap/usdc-mint.json" },
] as const;

const RUNNER = readFileSync("scripts/run-integration-tests.sh", "utf8");

describe("swap-path account fixtures", () => {
  for (const { name, constant, file } of CASES) {
    it(`${name}: the committed file is the address the constant names`, () => {
      const parsed = JSON.parse(readFileSync(file, "utf8")) as { pubkey: string };
      assert.equal(parsed.pubkey, constant);
    });

    it(`${name}: the integration runner loads that same address`, () => {
      assert.ok(
        RUNNER.includes(`"${constant}"`),
        `${constant} is not named in run-integration-tests.sh — the validator would boot without it`,
      );
      assert.ok(
        RUNNER.includes(`"${file}"`),
        `${file} is not loaded by run-integration-tests.sh`,
      );
    });
  }

  it("the three addresses are distinct", () => {
    const all = CASES.map((c) => c.constant as string);
    assert.equal(new Set(all).size, all.length, "a reused address would collapse two accounts");
  });

  it("$BYE and USDC are different mints", () => {
    // The bootstrap used one mint for both, which no pool can name as a pair — and which made
    // every mint-confusion bug invisible.
    assert.notEqual(BYE_MINT_FIXTURE as string, USDC_MINT_FIXTURE as string);
  });

  it("the mints carry the decimals the price convention assumes", () => {
    const decimalsOf = (file: string): number => {
      const parsed = JSON.parse(readFileSync(file, "utf8")) as { account: { data: [string, string] } };
      return Buffer.from(parsed.account.data[0], "base64").readUInt8(44);
    };
    assert.equal(decimalsOf("tests/fixtures/swap/bye-mint.json"), 9, "$BYE is nine decimals");
    assert.equal(decimalsOf("tests/fixtures/swap/usdc-mint.json"), 6, "USDC is six");
  });

  it("every committed fixture is listed in the checksum manifest", () => {
    const manifest = readFileSync("tests/fixtures/programs/SHA256SUMS", "utf8");
    for (const { file } of CASES) {
      assert.ok(manifest.includes(file), `${file} is unverified at boot`);
    }
  });
});
