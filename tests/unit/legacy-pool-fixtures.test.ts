// The pools the integration validator loads in states no instruction can produce: two old-length
// pools for the `migrate_pool` suite, and two unconsolidated ones, with their token accounts, for
// the `consolidate_treasuries` suite.
//
// Each is checked three ways: the committed bytes are what the generator produces today, the
// account sits where `init_pool` would put its pool id and is owned by the program, and the
// integration runner loads exactly that file at exactly that address. Any one of these drifting
// leaves the suite migrating an account that is not the old layout, or none at all.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Buffer } from "node:buffer";
import { getAddressDecoder } from "@solana/kit";

import { TOKEN_PROGRAM } from "../../scripts/constants.js";
import { accountDiscriminator, derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { LEGACY_ROLL_BATCH_LEN, decodePool } from "../../scripts/lib/decoders.js";
import {
  BUSY_UNCONSOLIDATED_POOL_ID,
  MISNAMED_UNCONSOLIDATED_POOL_ID,
  LEGACY_POOL_ID,
  LEGACY_POOL_LEN,
  LOCALNET_PROGRAM_ID,
  MALFORMED_POOL_LEN,
  POOL_LEN,
  TOKEN_ACCOUNT_LEN,
  UNCONSOLIDATED_POOL_ID,
  UNCONSOLIDATED_T03_AMOUNT,
  UNCONSOLIDATED_T04_AMOUNT,
  grownPoolFields,
  legacyPoolAccount,
  malformedPoolAccount,
  rentExempt,
  unconsolidatedPoolAccounts,
} from "../../scripts/lib/legacy-pool-fixtures.js";
import { USDC_MINT_FIXTURE } from "../../scripts/lib/swap-fixtures.js";
import {
  LEGACY_ROLLER_BATCH_ID,
  legacyBatchAccount,
  legacyBatchRoller,
  legacyCommittedBatchAccount,
  legacyRollerBatchAccount,
  misplacedLegacyBatchAccount,
} from "../../scripts/lib/legacy-batch-fixture.js";

const RUNNER = readFileSync("scripts/run-integration-tests.sh", "utf8");

type AccountFile = {
  pubkey: string;
  account: { lamports: number; data: [string, string]; owner: string; executable: boolean };
};

function load(file: string): AccountFile & { bytes: Buffer } {
  const parsed = JSON.parse(readFileSync(file, "utf8")) as AccountFile;
  return { ...parsed, bytes: Buffer.from(parsed.account.data[0], "base64") };
}

const CASES = [
  {
    name: "legacy pool",
    file: "tests/fixtures/legacy-pool.json",
    constant: "LEGACY_POOL_ADDRESS",
    len: LEGACY_POOL_LEN,
    expected: async () => legacyPoolAccount(LEGACY_POOL_ID),
  },
  {
    name: "malformed pool",
    file: "tests/fixtures/malformed-pool.json",
    constant: "MALFORMED_POOL_ADDRESS",
    len: MALFORMED_POOL_LEN,
    expected: malformedPoolAccount,
  },
] as const;

describe("legacy pool fixtures", () => {
  for (const { name, file, constant, len, expected } of CASES) {
    it(`${name}: the committed file is what the generator produces`, async () => {
      const committed = load(file);
      const { address, data } = await expected();
      assert.equal(committed.pubkey, address);
      assert.deepEqual(committed.bytes, data);
      assert.equal(committed.bytes.length, len);
    });

    it(`${name}: owned by the localnet program, rent-exempt, and Pool-tagged`, () => {
      const committed = load(file);
      assert.equal(committed.account.owner, LOCALNET_PROGRAM_ID);
      assert.equal(committed.account.executable, false);
      assert.equal(committed.account.lamports, rentExempt(len));
      assert.deepEqual(committed.bytes.subarray(0, 8), accountDiscriminator("Pool"));
    });

    it(`${name}: the integration runner loads that file at that address`, () => {
      const committed = load(file);
      assert.ok(RUNNER.includes(`${constant}="${committed.pubkey}"`), `${constant} names another address`);
      assert.ok(
        RUNNER.includes(`--account "$${constant}" "$${constant.replace("ADDRESS", "ACCOUNT")}"`),
        `${constant} is not passed to the validator`,
      );
      assert.ok(RUNNER.includes(`"${file}"`), `${file} is not named by the runner`);
    });
  }

  it("the old length is the current one less five face and five count slots", () => {
    assert.equal(POOL_LEN - LEGACY_POOL_LEN, 5 * 8 + 5 * 4);
  });

  it("the refused account is the pre-escrow length, which is neither layout", () => {
    assert.equal(MALFORMED_POOL_LEN, 276);
    assert.notEqual(MALFORMED_POOL_LEN, LEGACY_POOL_LEN);
    assert.notEqual(MALFORMED_POOL_LEN, POOL_LEN);
  });

  it("decodePool reads a legacy pool by its length, its blank arrays widened with zeros", async () => {
    const { fields } = await legacyPoolAccount(LEGACY_POOL_ID);
    assert.deepEqual(decodePool(load("tests/fixtures/legacy-pool.json").bytes), grownPoolFields(fields));
  });

  it("decodePool refuses any length that is neither layout", () => {
    const old = load("tests/fixtures/legacy-pool.json").bytes;
    for (const len of [MALFORMED_POOL_LEN, LEGACY_POOL_LEN - 1, LEGACY_POOL_LEN + 1, POOL_LEN - 1, POOL_LEN + 1]) {
      const data = Buffer.alloc(len);
      old.copy(data, 0, 0, Math.min(len, old.length));
      assert.throws(() => decodePool(data), /expected 402 or 342/, `length ${len}`);
    }
  });

  it("the legacy pool, with the five face and five count slots inserted as zeros, decodes to its own fields", async () => {
    const { fields } = await legacyPoolAccount(LEGACY_POOL_ID);
    const old = load("tests/fixtures/legacy-pool.json").bytes;
    const grown = Buffer.concat([
      old.subarray(0, 155),
      Buffer.alloc(40),
      old.subarray(155, 186),
      Buffer.alloc(20),
      old.subarray(186),
    ]);
    assert.equal(grown.length, POOL_LEN);
    assert.deepEqual(decodePool(grown), grownPoolFields(fields));
  });
});

describe("legacy batch fixture", () => {
  const file = "tests/fixtures/legacy-batch.json";

  it("the committed file is what the generator produces, at the three-slot length", async () => {
    const committed = load(file);
    const { address, data } = await legacyBatchAccount();
    assert.equal(committed.pubkey, address);
    assert.deepEqual(committed.bytes, data);
    assert.equal(committed.bytes.length, LEGACY_ROLL_BATCH_LEN);
  });

  it("is a RollBatch owned by the localnet program, rent-exempt, Complete, its bump last", async () => {
    const committed = load(file);
    const { pool, roller } = await legacyBatchAccount();
    assert.equal(committed.account.owner, LOCALNET_PROGRAM_ID);
    assert.equal(committed.account.lamports, rentExempt(LEGACY_ROLL_BATCH_LEN));
    assert.deepEqual(committed.bytes.subarray(0, 8), accountDiscriminator("RollBatch"));
    assert.equal(committed.bytes[80], 3, "state byte is not Complete");
    const addr = getAddressDecoder();
    assert.equal(addr.decode(committed.bytes.subarray(8, 40)), pool);
    assert.equal(addr.decode(committed.bytes.subarray(40, 72)), roller);
  });

  it("the integration runner loads that file at that address", () => {
    const committed = load(file);
    assert.ok(RUNNER.includes(`LEGACY_BATCH_ADDRESS="${committed.pubkey}"`));
    assert.ok(RUNNER.includes(`--account "$LEGACY_BATCH_ADDRESS" "$LEGACY_BATCH_ACCOUNT"`));
    assert.ok(RUNNER.includes(`"${file}"`));
  });
});

describe("keyed legacy batch fixtures", () => {
  const cases = [
    { file: "tests/fixtures/legacy-batch-roller.json", variable: "LEGACY_ROLLER_BATCH", build: legacyRollerBatchAccount, state: 3 },
    { file: "tests/fixtures/legacy-batch-committed.json", variable: "LEGACY_COMMITTED_BATCH", build: legacyCommittedBatchAccount, state: 0 },
    { file: "tests/fixtures/legacy-batch-misplaced.json", variable: "MISPLACED_BATCH", build: misplacedLegacyBatchAccount, state: 3 },
  ];

  for (const { file, variable, build, state } of cases) {
    it(`${file} is what the generator produces, names the keyed roller, and the runner loads it`, async () => {
      const committed = load(file);
      const { address, data } = await build();
      assert.equal(committed.pubkey, address);
      assert.deepEqual(committed.bytes, data);
      assert.equal(committed.bytes.length, LEGACY_ROLL_BATCH_LEN);
      assert.equal(committed.account.owner, LOCALNET_PROGRAM_ID);
      assert.equal(committed.account.lamports, rentExempt(LEGACY_ROLL_BATCH_LEN));
      assert.deepEqual(committed.bytes.subarray(0, 8), accountDiscriminator("RollBatch"));
      assert.equal(committed.bytes[80], state);
      assert.equal(getAddressDecoder().decode(committed.bytes.subarray(40, 72)), (await legacyBatchRoller()).address);
      assert.ok(RUNNER.includes(`${variable}_ADDRESS="${committed.pubkey}"`));
      assert.ok(RUNNER.includes(`--account "$${variable}_ADDRESS" "$${variable}_ACCOUNT"`));
      assert.ok(RUNNER.includes(`"${file}"`));
    });
  }

  it("the misplaced batch's header names the keyed batch's id and bump, not the PDA it sits at", async () => {
    const misplaced = await misplacedLegacyBatchAccount();
    const keyed = await legacyRollerBatchAccount();
    assert.notEqual(misplaced.address, keyed.address);
    assert.equal(misplaced.data.readBigUInt64LE(72), LEGACY_ROLLER_BATCH_ID);
    assert.equal(misplaced.data[LEGACY_ROLL_BATCH_LEN - 1], keyed.data[LEGACY_ROLL_BATCH_LEN - 1]);
  });
});

describe("unconsolidated pool fixtures", () => {
  const cases = [
    { name: "unconsolidated-pool", variable: "UNCONSOLIDATED", poolId: UNCONSOLIDATED_POOL_ID, openBatches: 0 },
    { name: "busy-unconsolidated-pool", variable: "BUSY_UNCONSOLIDATED", poolId: BUSY_UNCONSOLIDATED_POOL_ID, openBatches: 1 },
    { name: "misnamed-unconsolidated-pool", variable: "MISNAMED_UNCONSOLIDATED", poolId: MISNAMED_UNCONSOLIDATED_POOL_ID, openBatches: 0, namesOwn: false },
  ] as const;

  for (const { name, variable, poolId, openBatches, ...rest } of cases) {
    const namesOwn = "namesOwn" in rest ? rest.namesOwn : true;
    it(`${name}: the three committed files are what the generator produces, and the runner loads them`, async () => {
      const built = await unconsolidatedPoolAccounts(poolId, openBatches, namesOwn);
      for (const [suffix, slot, account, owner] of [
        ["", "POOL", built.pool, LOCALNET_PROGRAM_ID],
        ["-t03", "T03", built.treasury03, TOKEN_PROGRAM],
        ["-t04", "T04", built.treasury04, TOKEN_PROGRAM],
      ] as const) {
        const file = `tests/fixtures/${name}${suffix}.json`;
        const committed = load(file);
        assert.equal(committed.pubkey, account.address, file);
        assert.deepEqual(committed.bytes, account.data, file);
        assert.equal(committed.account.owner, owner, file);
        assert.equal(committed.account.lamports, rentExempt(account.data.length), file);
        assert.ok(RUNNER.includes(`${variable}_${slot}_ADDRESS="${committed.pubkey}"`), file);
        assert.ok(RUNNER.includes(`--account "$${variable}_${slot}_ADDRESS" "$${variable}_${slot}_ACCOUNT"`), file);
        assert.ok(RUNNER.includes(`"${file}"`), file);
      }
    });

    it(`${name}: a current-layout pool naming ${namesOwn ? "its own" : "another"} Treasury 03 and no Treasury 04, its token accounts held by the pool`, async () => {
      const built = await unconsolidatedPoolAccounts(poolId, openBatches, namesOwn);
      const pool = decodePool(built.pool.data);
      assert.equal(built.pool.data.length, POOL_LEN);
      assert.equal(pool.poolId, poolId);
      assert.equal(pool.openBatches, openBatches);
      if (namesOwn) assert.equal(pool.treasury03, built.treasury03.address);
      else assert.notEqual(pool.treasury03, built.treasury03.address);
      assert.equal(pool.treasury04, "11111111111111111111111111111111");
      const { address: t03 } = await derivePda(["t03", seedFromPubkey(built.pool.address)], LOCALNET_PROGRAM_ID);
      const { address: t04 } = await derivePda(["t04", seedFromPubkey(built.pool.address)], LOCALNET_PROGRAM_ID);
      assert.equal(built.treasury03.address, t03);
      assert.equal(built.treasury04.address, t04);
      const addr = getAddressDecoder();
      for (const [account, amount] of [
        [built.treasury03, UNCONSOLIDATED_T03_AMOUNT],
        [built.treasury04, UNCONSOLIDATED_T04_AMOUNT],
      ] as const) {
        assert.equal(account.data.length, TOKEN_ACCOUNT_LEN);
        assert.equal(addr.decode(account.data.subarray(0, 32)), USDC_MINT_FIXTURE, "mint");
        assert.equal(addr.decode(account.data.subarray(32, 64)), built.pool.address, "authority");
        assert.equal(account.data.readBigUInt64LE(64), amount, "amount");
        assert.equal(account.data[108], 1, "state is Initialized");
      }
    });
  }
});
