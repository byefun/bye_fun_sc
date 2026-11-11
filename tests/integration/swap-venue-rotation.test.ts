// `update_vrf_config` — the venue gate on the rotation path, measured against a live validator.
//
// WHY THIS FILE EXISTS SEPARATELY FROM THE INIT CASES. `init_protocol` validates the pool it
// pins, but it runs once. Rotation runs whenever the Administrator says so, and for a while it
// wrote `swap_pool` with nothing read — so "pin a valid pool, then rotate to anything" made the
// gate at launch worth nothing. A suite that checked only the init path would have stayed green
// through exactly that.
//
// THE POSITIVE CONTROL IS NOT OPTIONAL. Both cases below look for a rejection, and a rejection is
// also what an instruction nobody can satisfy produces — a wrong account order, a missing signer,
// or a handler that refuses every input would make both pass for the wrong reason. The third case
// drives a real rotation through to success on the same ledger, reads the account back, and puts
// the value it changed back where it found it so later files see the configuration they expect.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";

import {
  assertProgramIsLive,
  expectAnchorError,
  getConnection,
  loadSuiteAdmin,
  loadUnprivileged,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { updateVrfConfig } from "../../scripts/lib/admin.js";
import { decodeProtocolConfig } from "../../scripts/lib/decoders.js";
import { sendIxs } from "../../scripts/_common.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";

// programs/bye_machine/src/common/errors.rs — the group based at 6500.
const SWAP_POOL_INVALID = 6512;
const SWAP_SLIPPAGE_OUT_OF_BOUNDS = 6514;

/** The value `ensureProtocol` installs, and the value this file must leave behind. */
const DEPLOY_SLIPPAGE_BPS = 100;

describe("update_vrf_config — the venue gate on rotation", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let protocolConfig: string;
  let vrfProgram: string;
  let oracleQueue: string;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();

    const bootstrap = await ensureProtocol(connection, admin);
    protocolConfig = bootstrap.protocolConfig;

    // Rotation rewrites all four fields together, so the two this file does not exercise are
    // read back and passed through unchanged rather than guessed at.
    const decoded = await readConfig();
    vrfProgram = decoded.vrfProgram;
    oracleQueue = decoded.oracleQueue;
    assert.equal(
      decoded.maxSwapSlippageBps,
      DEPLOY_SLIPPAGE_BPS,
      "the bootstrap no longer installs the bound this file restores",
    );
  });

  async function readConfig() {
    const info = await connection.rpc
      .getAccountInfo(protocolConfig as never, { encoding: "base64" })
      .send();
    assert.notEqual(info.value, null, "protocol_config is missing");
    const [b64] = info.value!.data as [string, string];
    return decodeProtocolConfig(Buffer.from(b64, "base64"));
  }

  function rotation(overrides: { swapPool?: string; maxSwapSlippageBps?: number }) {
    return updateVrfConfig({
      administrator: admin.address,
      vrfProgram: vrfProgram as never,
      oracleQueue: oracleQueue as never,
      swapPool: (overrides.swapPool ?? SWAP_POOL_FIXTURE) as never,
      maxSwapSlippageBps: overrides.maxSwapSlippageBps ?? DEPLOY_SLIPPAGE_BPS,
    });
  }

  it("refuses to rotate onto a pool the venue program does not own", async () => {
    const unprivileged = await loadUnprivileged();
    await expectAnchorError(
      sendIxs(connection, admin, [await rotation({ swapPool: unprivileged.address })]),
      SWAP_POOL_INVALID,
    );
    assert.equal(
      (await readConfig()).swapPool,
      SWAP_POOL_FIXTURE,
      "a refused rotation still moved the pinned venue",
    );
  });

  it("refuses a slippage bound of 10,000 basis points", async () => {
    await expectAnchorError(
      sendIxs(connection, admin, [await rotation({ maxSwapSlippageBps: 10_000 })]),
      SWAP_SLIPPAGE_OUT_OF_BOUNDS,
    );
    assert.equal(
      (await readConfig()).maxSwapSlippageBps,
      DEPLOY_SLIPPAGE_BPS,
      "a refused rotation still moved the bound",
    );
  });

  it("rotates the bound onto a valid pool, and the account carries the new value", async () => {
    await sendIxs(connection, admin, [await rotation({ maxSwapSlippageBps: 250 })]);
    assert.equal(
      (await readConfig()).maxSwapSlippageBps,
      250,
      "rotation reported success but the account did not move",
    );

    // Restored, so files that run after this one see the configuration the bootstrap installed.
    await sendIxs(connection, admin, [await rotation({ maxSwapSlippageBps: DEPLOY_SLIPPAGE_BPS })]);
    assert.equal((await readConfig()).maxSwapSlippageBps, DEPLOY_SLIPPAGE_BPS);
  });
});
