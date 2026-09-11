// Integration coverage for tests/helpers/pools.ts's three `init_pool` profiles (T-49, decision
// 11) — closes G3 from the P7 QA Lead sign-off: `limitsOnProfile`, `smallTierProfile` and
// `POOL_PROFILES` shipped with zero references anywhere outside pools.ts, so nothing on this tree
// had ever actually exercised them. This file creates all three profiles as three live pools
// (pools.ts's own ≥2-live-pool precondition) at distinct `poolId`s, and asserts — by enumeration,
// keyed on each pool's own decoded `PoolInitialized` event, never a test name or a registration
// call — that every profile actually reached `init_pool` with its own distinguishing values
// intact.
//
// The distinguishing key is `(tierSize, admissionCeiling, walletValueCap)`: Launch and Small-tier
// share the first two (0, 0) and differ only in `tierSize` (20 vs 3); Limits-on shares `tierSize`
// with Launch and differs only in the ceiling/cap. No single field distinguishes all three — the
// triple is the smallest observable that does. A profile imported but never sent to `init_pool`
// would leave its key entirely absent from the decoded set below, which is exactly the failure
// decision 11 exists to catch.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import {
  assertProgramIsLive,
  getConnection,
  loadSuiteAdmin,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import {
  createPool,
  launchProfile,
  limitsOnProfile,
  smallTierProfile,
  POOL_PROFILES,
  type PoolConfigArgs,
  type PoolProfileName,
} from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { sendIxs } from "../../scripts/_common.js";
import { decodeEvent, type DecodedEvent } from "../../scripts/lib/decoders.js";
import { addSignersToInstruction } from "@solana/kit";

function isPoolInitialized(
  event: DecodedEvent,
): event is Extract<DecodedEvent, { name: "PoolInitialized" }> {
  return event.name === "PoolInitialized";
}

const PROGRAM_DATA_PREFIX = "Program data: ";

type ProfileSignature = { tierSize: number; admissionCeiling: bigint; walletValueCap: bigint };

function signatureOf(args: PoolConfigArgs): ProfileSignature {
  return {
    tierSize: args.tierSize,
    admissionCeiling: args.admissionCeiling,
    walletValueCap: args.walletValueCap,
  };
}

function keyOf(sig: ProfileSignature): string {
  return `${sig.tierSize}:${sig.admissionCeiling}:${sig.walletValueCap}`;
}

/** Decodes the *sole* `PoolInitialized` event out of one `init_pool` transaction's own logs — two
 * or zero would mean this signature is not what this test thinks it is. */
async function decodePoolInitializedFrom(
  connection: Connection,
  signature: string,
): Promise<ProfileSignature> {
  const logs = await connection.getLogs(signature);
  const events = logs
    .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
    .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")))
    .filter(isPoolInitialized);
  assert.equal(
    events.length,
    1,
    `expected exactly one PoolInitialized event in ${signature}'s logs, found ${events.length}`,
  );
  const { tierSize, admissionCeiling, walletValueCap } = events[0].data;
  return { tierSize, admissionCeiling, walletValueCap };
}

describe("T-49 / decision 11 — every init_pool profile is actually exercised", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  const decoded = new Map<PoolProfileName, ProfileSignature>();

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();

    const { usdcMint, poolCounter } = await ensureProtocol(connection, admin);

    const names = Object.keys(POOL_PROFILES) as PoolProfileName[];
    for (let i = 0; i < names.length; i++) {
      const name = names[i];
      const args = POOL_PROFILES[name](admin.address);
      const poolId = poolCounter + i;

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
        args,
      });

      decoded.set(name, await decodePoolInitializedFrom(connection, poolAccounts.signature));
    }
  });

  it("Launch profile's PoolInitialized carries tierSize 20 and ceiling/cap 0 — design §1 verbatim", () => {
    assert.deepEqual(decoded.get("launch"), signatureOf(launchProfile(admin.address)));
  });

  it("Limits-on profile's PoolInitialized carries its own nonzero ceiling and wallet cap", () => {
    assert.deepEqual(decoded.get("limitsOn"), signatureOf(limitsOnProfile(admin.address)));
  });

  it("Small-tier profile's PoolInitialized carries tierSize 3, reachable without 20+ fixtures", () => {
    assert.deepEqual(decoded.get("smallTier"), signatureOf(smallTierProfile(admin.address)));
  });

  it("all three profiles are pairwise distinct on-chain — the enumeration itself, keyed on the decoded event", () => {
    const observedKeys = [...decoded.values()].map(keyOf);
    const distinct = new Set(observedKeys);
    assert.equal(
      distinct.size,
      3,
      `expected exactly 3 distinct profile signatures, observed ${distinct.size}: ${observedKeys.join(", ")}`,
    );
  });
});
