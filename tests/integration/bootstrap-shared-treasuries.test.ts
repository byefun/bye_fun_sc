// `init_pool` creates the shared Treasury 03 and Treasury 04 when they are absent, and every later
// pool reuses them.
//
// Absence is observable only before the first `init_pool` on the validator, so this file sorts
// directly after the protocol bootstrap and creates the first two pools itself. It reads account
// state before and after each call rather than transaction history, which the test validator prunes
// on a long run.

import { before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, getAddressDecoder } from "@solana/kit";
import {
  assertProgramIsLive,
  getConnection,
  loadSuiteAdmin,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, goldProfile, launchProfile, type PoolConfigArgs } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { TOKEN_PROGRAM } from "../../scripts/constants.js";
import { deriveSharedTreasuries } from "../../scripts/lib/admin.js";
import { decodePool } from "../../scripts/lib/decoders.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";

type Snapshot = { lamports: bigint; owner: string; data: Buffer } | null;

describe("init_pool and the shared treasuries", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let usdcMint: Address;
  let protocolConfig: Address;
  let shared: { treasury03: Address; treasury04: Address };

  async function snapshot(account: Address): Promise<Snapshot> {
    const { value } = await connection.rpc
      .getAccountInfo(account, { encoding: "base64", commitment: "confirmed" })
      .send();
    if (!value) return null;
    return { lamports: BigInt(value.lamports), owner: value.owner, data: Buffer.from(value.data[0], "base64") };
  }

  async function newPool(args: PoolConfigArgs): Promise<{ poolId: number; pool: Address }> {
    const poolId = (await ensureProtocol(connection, admin)).poolCounter;
    const { signer: wi, instruction } = await createWeightIndexAccount({ rpc: connection, payer: admin.address });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, wi], instruction)]);
    const { pool } = await createPool({ connection, administrator: admin, poolId, weightIndex: wi.address, usdcMint, args });
    return { poolId, pool };
  }

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    const protocol = await ensureProtocol(connection, admin);
    usdcMint = protocol.usdcMint;
    protocolConfig = protocol.protocolConfig;
    shared = await deriveSharedTreasuries();
  });

  it("the first init_pool creates both shared treasuries, and the next init_pool reuses them", async () => {
    assert.equal((await ensureProtocol(connection, admin)).poolCounter, 0, "precondition: no pool exists yet");
    assert.equal(await snapshot(shared.treasury03), null, "precondition: Treasury 03 is absent");
    assert.equal(await snapshot(shared.treasury04), null, "precondition: Treasury 04 is absent");

    const first = await newPool(launchProfile());
    const created: Snapshot[] = [];
    for (const account of [shared.treasury03, shared.treasury04]) {
      const state = await snapshot(account);
      assert.ok(state, `the first init_pool did not create ${account}`);
      assert.equal(state.owner, TOKEN_PROGRAM);
      const address = getAddressDecoder();
      assert.deepEqual(
        {
          mint: address.decode(state.data.subarray(0, 32)),
          authority: address.decode(state.data.subarray(32, 64)),
          amount: state.data.readBigUInt64LE(64),
        },
        { mint: usdcMint, authority: protocolConfig, amount: 0n },
        `${account}: mint, authority, amount`,
      );
      created.push(state);
    }

    const second = await newPool(goldProfile());
    assert.notEqual(first.poolId, second.poolId);
    for (const [i, account] of [shared.treasury03, shared.treasury04].entries()) {
      assert.deepEqual(await snapshot(account), created[i], `the second init_pool changed ${account}`);
    }
    for (const { poolId, pool } of [first, second]) {
      const state = decodePool((await fetchAccountData(connection, pool))!);
      assert.equal(state.treasury03, shared.treasury03, `pool ${poolId}'s Treasury 03`);
      assert.equal(state.treasury04, shared.treasury04, `pool ${poolId}'s Treasury 04`);
    }
  });
});
