// The admission floor and the blank-face slots on chain: a pool may hold a floor at or below its
// ticket only while slot 3 carries a face at least 1 USDC above the ticket, slots 4–7 stay 0, and
// both `init_pool` and every later `update_config` judge the merged config.
//
// The refusals are paired with the successes they would otherwise hide behind: Gold is created
// with its floor under its ticket, and moves its floor to the ticket and back, before any refusal
// is read as a guard rather than a broken encoder.

import { before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  getConnection,
  loadSuiteAdmin,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, goldProfile, launchProfile, type PoolAccounts } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { updateConfig, type ConfigUpdate } from "../../scripts/lib/admin.js";
import { decodePool, type BlankFaces } from "../../scripts/lib/decoders.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";

const FLOOR_NEEDS_ABOVE_TICKET_BLANK = 6527;
const ABOVE_TICKET_BLANK_TOO_CLOSE = 6528;
const RESERVED_BLANK_SLOT_SET = 6529;

describe("the admission floor and the eight blank-face slots", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let gold: PoolAccounts & { poolId: number };
  let bronze: PoolAccounts & { poolId: number };

  async function newPool(poolId: number, usdcMint: Address, args: ReturnType<typeof goldProfile>) {
    const { signer: weightIndexSigner, instruction } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], instruction)]);
    const accounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex: weightIndexSigner.address,
      usdcMint,
      args,
    });
    return { ...accounts, poolId };
  }

  async function poolOf(accounts: PoolAccounts) {
    return decodePool((await fetchAccountData(connection, accounts.pool))!);
  }

  async function configure(poolId: number, update: ConfigUpdate): Promise<string> {
    return sendIxs(connection, admin, [
      await updateConfig({ administrator: admin.address, poolId, update, demoted: [] }),
    ]);
  }

  function goldFacesWith(slot: number, face: bigint): BlankFaces {
    const faces = [...goldProfile().blankFaces] as BlankFaces;
    faces[slot] = face;
    return faces;
  }

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    const protocolState = await ensureProtocol(connection, admin);
    gold = await newPool(protocolState.poolCounter, protocolState.usdcMint, goldProfile());
    bronze = await newPool(protocolState.poolCounter + 1, protocolState.usdcMint, launchProfile());
  });

  it("creates Gold with its 30 USDC floor under its 45 USDC ticket and its 100 USDC face in slot 3", async () => {
    const pool = await poolOf(gold);
    assert.equal(pool.admissionFloor, 30_000_000n);
    assert.equal(pool.ticketTarget, 45_000_000n);
    assert.equal(pool.price, 49_500_000n);
    assert.deepEqual(pool.blankFaces, goldProfile().blankFaces);
    assert.deepEqual(pool.blanks, [0, 0, 0, 0, 0, 0, 0, 0]);
  });

  it("accepts a later floor change on Gold, including one to the ticket itself", async () => {
    await configure(gold.poolId, { kind: "AdmissionFloor", value: 45_000_000n });
    assert.equal((await poolOf(gold)).admissionFloor, 45_000_000n);
    await configure(gold.poolId, { kind: "AdmissionFloor", value: 30_000_000n });
    assert.equal((await poolOf(gold)).admissionFloor, 30_000_000n);
  });

  it("refuses removing Gold's above-ticket face while its floor is under the ticket", async () => {
    await expectAnchorError(
      configure(gold.poolId, { kind: "BlankFaces", value: goldFacesWith(3, 0n) }),
      FLOOR_NEEDS_ABOVE_TICKET_BLANK,
    );
    assert.deepEqual((await poolOf(gold)).blankFaces, goldProfile().blankFaces);
  });

  it("refuses an above-ticket face less than 1 USDC over the ticket, set directly or by a ticket raise", async () => {
    await expectAnchorError(
      configure(gold.poolId, { kind: "BlankFaces", value: goldFacesWith(3, 45_999_999n) }),
      ABOVE_TICKET_BLANK_TOO_CLOSE,
    );
    await expectAnchorError(
      configure(gold.poolId, {
        kind: "Pricing",
        price: 109_000_001n,
        ticketTarget: 99_000_001n,
        fee: 10_000_000n,
      }),
      ABOVE_TICKET_BLANK_TOO_CLOSE,
    );
    const pool = await poolOf(gold);
    assert.deepEqual(pool.blankFaces, goldProfile().blankFaces);
    assert.equal(pool.ticketTarget, 45_000_000n);
  });

  it("refuses a face in any reserved slot", async () => {
    for (const slot of [4, 5, 6, 7]) {
      await expectAnchorError(
        configure(gold.poolId, { kind: "BlankFaces", value: goldFacesWith(slot, 200_000_000n) }),
        RESERVED_BLANK_SLOT_SET,
      );
    }
    assert.deepEqual((await poolOf(gold)).blankFaces, goldProfile().blankFaces);
  });

  it("still refuses a Bronze floor at or below its ticket, which has no above-ticket face", async () => {
    await expectAnchorError(
      configure(bronze.poolId, { kind: "AdmissionFloor", value: 9_000_000n }),
      FLOOR_NEEDS_ABOVE_TICKET_BLANK,
    );
    assert.equal((await poolOf(bronze)).admissionFloor, launchProfile().admissionFloor);
  });
});
