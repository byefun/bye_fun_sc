/**
 * Regenerates the two old-length pool accounts the integration validator loads for the
 * `migrate_pool` suite: a pool in the 342-byte three-slot layout, and a `Pool`-tagged account at
 * the 276-byte pre-escrow length that the migration must refuse. Also regenerates the finished
 * 1,100-byte three-slot batch the `close_batch` suite closes, and the two pools still holding
 * their own Treasury 03 and Treasury 04, with those token accounts, that the
 * `consolidate_treasuries` suite consolidates.
 *
 * Run with:  pnpm exec tsx scripts/build-legacy-pool-fixtures.ts
 *
 * Both sit at the address `init_pool` would give their pool id under the localnet program id,
 * owned by that program, so the validator holds what a pool created before the layout change
 * looks like. Nothing signs for them: `--account` loads state, not a keypair.
 */
import { Buffer } from "node:buffer";
import { writeFileSync } from "node:fs";

import type { Address } from "@solana/kit";

import {
  BUSY_UNCONSOLIDATED_POOL_ID,
  LEGACY_POOL_ID,
  LOCALNET_PROGRAM_ID,
  MALFORMED_POOL_ID,
  MISNAMED_UNCONSOLIDATED_POOL_ID,
  UNCONSOLIDATED_POOL_ID,
  legacyPoolAccount,
  malformedPoolAccount,
  rentExempt,
  unconsolidatedPoolAccounts,
} from "./lib/legacy-pool-fixtures";
import {
  legacyBatchAccount,
  legacyCommittedBatchAccount,
  legacyRollerBatchAccount,
  misplacedLegacyBatchAccount,
} from "./lib/legacy-batch-fixture";

function accountJson(params: { pubkey: Address; data: Buffer; owner?: Address }): string {
  return `${JSON.stringify(
    {
      pubkey: params.pubkey,
      account: {
        lamports: rentExempt(params.data.length),
        data: [params.data.toString("base64"), "base64"],
        owner: params.owner ?? LOCALNET_PROGRAM_ID,
        executable: false,
        rentEpoch: 0,
      },
    },
    null,
    2,
  )}\n`;
}

async function main(): Promise<void> {
  const legacy = await legacyPoolAccount(LEGACY_POOL_ID);
  const malformed = await malformedPoolAccount();

  writeFileSync(
    "tests/fixtures/legacy-pool.json",
    accountJson({ pubkey: legacy.address, data: legacy.data }),
  );
  writeFileSync(
    "tests/fixtures/malformed-pool.json",
    accountJson({ pubkey: malformed.address, data: malformed.data }),
  );

  const batch = await legacyBatchAccount();
  writeFileSync(
    "tests/fixtures/legacy-batch.json",
    accountJson({ pubkey: batch.address, data: batch.data }),
  );

  for (const [file, account] of [
    ["tests/fixtures/legacy-batch-roller.json", await legacyRollerBatchAccount()],
    ["tests/fixtures/legacy-batch-committed.json", await legacyCommittedBatchAccount()],
    ["tests/fixtures/legacy-batch-misplaced.json", await misplacedLegacyBatchAccount()],
  ] as const) {
    writeFileSync(file, accountJson({ pubkey: account.address, data: account.data }));
    console.log(`${file}: ${account.address}`);
  }

  for (const [name, poolId, openBatches, namesOwn = true] of [
    ["unconsolidated-pool", UNCONSOLIDATED_POOL_ID, 0],
    ["busy-unconsolidated-pool", BUSY_UNCONSOLIDATED_POOL_ID, 1],
    ["misnamed-unconsolidated-pool", MISNAMED_UNCONSOLIDATED_POOL_ID, 0, false],
  ] as const) {
    const accounts = await unconsolidatedPoolAccounts(poolId, openBatches, namesOwn);
    for (const [suffix, account] of [
      ["", accounts.pool],
      ["-t03", accounts.treasury03],
      ["-t04", accounts.treasury04],
    ] as const) {
      const file = `tests/fixtures/${name}${suffix}.json`;
      writeFileSync(file, accountJson({ pubkey: account.address, data: account.data, owner: account.owner }));
      console.log(`${file}: ${account.address}`);
    }
  }

  console.log(`legacy pool ${LEGACY_POOL_ID}: ${legacy.address}`);
  console.log(`malformed pool ${MALFORMED_POOL_ID}: ${malformed.address}`);
  console.log(`legacy batch: ${batch.address}`);
}

void main();
