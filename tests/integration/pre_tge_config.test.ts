import { before, describe, it } from "node:test";
import assert from "node:assert/strict";

import { assertProgramIsLive, expectAnchorError, fundedWallet, getConnection, loadSuiteAdmin, loadUnprivileged, type Address, type Connection, type TransactionSigner } from "../helpers/env.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { updateByeMint, updateProtocolRevenue, updateSwapPool } from "../../scripts/lib/admin.js";
import { decodeProtocolConfig } from "../../scripts/lib/decoders.js";
import { sendIxs } from "../../scripts/_common.js";
import { createAtaInstruction } from "../helpers/ata.js";
import { BYE_MINT_FIXTURE, SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";

const UNAUTHORIZED = 6503;
const SWAP_POOL_INVALID = 6512;
const STANDARD_NOT_ADMITTED = 6101;
const UNSET = "11111111111111111111111111111111";

describe("pre-TGE $BYE configuration", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let protocolConfig: string;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    protocolConfig = (await ensureProtocol(connection, admin)).protocolConfig;
  });

  async function config() {
    const info = await connection.rpc.getAccountInfo(protocolConfig as never, { encoding: "base64" }).send();
    assert.notEqual(info.value, null, "protocol config is missing");
    return decodeProtocolConfig(Buffer.from((info.value!.data as [string, string])[0], "base64"));
  }

  it("requires administrator authorization and enforces clear → mint → pool", async () => {
    const stranger = await loadUnprivileged();
    await expectAnchorError(sendIxs(connection, stranger, [await updateSwapPool({ administrator: stranger.address, swapPool: null })]), UNAUTHORIZED);
    await expectAnchorError(sendIxs(connection, stranger, [await updateByeMint({ administrator: stranger.address, byeMint: null })]), UNAUTHORIZED);

    await sendIxs(connection, admin, [await updateSwapPool({ administrator: admin.address, swapPool: null })]);
    assert.equal((await config()).swapPool, UNSET);
    await sendIxs(connection, admin, [await updateByeMint({ administrator: admin.address, byeMint: null })]);
    assert.equal((await config()).byeMint, UNSET);

    await expectAnchorError(sendIxs(connection, admin, [await updateSwapPool({ administrator: admin.address, swapPool: SWAP_POOL_FIXTURE })]), SWAP_POOL_INVALID);
    await sendIxs(connection, admin, [await updateByeMint({ administrator: admin.address, byeMint: BYE_MINT_FIXTURE })]);
    await sendIxs(connection, admin, [await updateSwapPool({ administrator: admin.address, swapPool: SWAP_POOL_FIXTURE })]);
    assert.equal((await config()).swapPool, SWAP_POOL_FIXTURE);
    await expectAnchorError(sendIxs(connection, admin, [await updateByeMint({ administrator: admin.address, byeMint: null })]), SWAP_POOL_INVALID);
  });
});

describe("update_protocol_revenue", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let protocolConfig: string;
  let usdcMint: Address;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    const bootstrap = await ensureProtocol(connection, admin);
    protocolConfig = bootstrap.protocolConfig;
    usdcMint = bootstrap.usdcMint;
  });

  async function config() {
    const info = await connection.rpc.getAccountInfo(protocolConfig as never, { encoding: "base64" }).send();
    assert.notEqual(info.value, null, "protocol config is missing");
    return decodeProtocolConfig(Buffer.from((info.value!.data as [string, string])[0], "base64"));
  }

  it("requires administrator authorization, rejects a mint mismatch, and repoints the destination", async () => {
    const stranger = await loadUnprivileged();
    const destination = await fundedWallet(connection);

    const { address: correctMintAccount, instruction: createCorrect } = await createAtaInstruction({
      payer: admin.address,
      owner: destination.address,
      mint: usdcMint,
    });
    await sendIxs(connection, admin, [createCorrect]);

    const { address: wrongMintAccount, instruction: createWrong } = await createAtaInstruction({
      payer: admin.address,
      owner: destination.address,
      mint: BYE_MINT_FIXTURE,
    });
    await sendIxs(connection, admin, [createWrong]);

    await expectAnchorError(
      sendIxs(connection, stranger, [
        await updateProtocolRevenue({ administrator: stranger.address, protocolRevenue: correctMintAccount }),
      ]),
      UNAUTHORIZED,
    );

    await expectAnchorError(
      sendIxs(connection, admin, [
        await updateProtocolRevenue({ administrator: admin.address, protocolRevenue: wrongMintAccount }),
      ]),
      STANDARD_NOT_ADMITTED,
    );

    const before = await config();
    await sendIxs(connection, admin, [
      await updateProtocolRevenue({ administrator: admin.address, protocolRevenue: correctMintAccount }),
    ]);
    assert.equal((await config()).protocolRevenue, correctMintAccount);
    assert.notEqual(before.protocolRevenue, correctMintAccount);
  });
});
