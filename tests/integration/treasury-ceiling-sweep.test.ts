// `sweep_t04_overflow` — the only instruction that moves the shared Treasury 04's excess, and
// `set_t04_ceiling`, the only one that sets the protocol-wide ceiling it sweeps to.
//
// Treasury 04 takes each blank's equal-share credit in full, from every pool, so its balance may
// sit above the ceiling for as long as nobody sweeps. This file drives the instruction that brings
// it back: the excess, and only the excess, to the shared Treasury 03.
//
// Five properties, each of which fails differently:
//
//   - **amount** — exactly `balance - ceiling`. A `>=`/`>` slip moves a zero-value transfer; an
//     off-by-one in either direction leaves the treasury past its ceiling or below it.
//   - **the ceiling** — refused while it is 0, and set by the Administrator alone; the per-pool
//     `update_config` arm that used to set it is refused.
//   - **every pool** — the sweep names every pool once; a set short of one, with a repeat, or with
//     an id past `pool_counter` is refused.
//   - **authorisation** — Operator only. **The success row is the load-bearing one**: a missing
//     authority check shows up as a call that succeeds, and no rejection row anywhere looks for
//     a success. Refusals alone would pass against a handler with no check at all.
//   - **the open-batch gate** — Treasury 04 is what every in-flight batch's blank outcomes are
//     paid from, in every pool, and the ceiling is not frozen behind a batch. The gate keeps a
//     routine sweep from changing that funding after the rolls were paid for.
//
// The success rows need every pool on the validator idle, so they depend on every earlier suite
// having finished its batches; `poolsWithOpenBatches` names the culprit when one has not.

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  loadUnprivileged,
  withSigner,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { admitPoolCollection, createPool, launchProfile } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import {
  createPnftCollection,
  mintPlainNonFungiblePnft,
  type MintedAsset,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { VRF_QUEUE_FIXTURE } from "../helpers/vrf-queue.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { TOKEN_PROGRAM, VRF_PROGRAM } from "../../scripts/constants.js";
import { setT04Ceiling, sweepT04Overflow, updateConfig, updateVrfConfig } from "../../scripts/lib/admin.js";
import { approveDeposit, deposit } from "../../scripts/lib/deposit.js";
import { commitRolls, testResolveBatch } from "../../scripts/lib/roll.js";
import { blankWeight, classifyDraw, expandDraw } from "../../scripts/verify-batch.js";
import { settleDrawnRoll } from "../helpers/roll.js";
import { decodeEvent, decodePool, decodeProtocolConfig, type DecodedEvent } from "../../scripts/lib/decoders.js";
import { derivePda, signer, writable } from "../../scripts/lib/anchor.js";
import { EXECUTED_ROWS, UNAUTHORIZED } from "../helpers/authority-matrix.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import {
  assertEveryPoolIdle,
  drainTreasury04,
  everyPoolId,
  poolsWithOpenBatches,
  sendSweep,
} from "../helpers/treasuries.js";
import { UNCONSOLIDATED_POOL_ID } from "../../scripts/lib/legacy-pool-fixtures.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";

const PROGRAM_DATA_PREFIX = "Program data: ";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

/** SPL Token `MintTo`: instruction 7, then the amount as a little-endian u64. */
function mintToData(amount: bigint): Uint8Array {
  const data = new Uint8Array(9);
  data[0] = 7;
  new DataView(data.buffer).setBigUint64(1, amount, true);
  return data;
}

/** The ceiling this suite sets — `launchProfile`'s, which `init_pool` now ignores. */
const CEILING = launchProfile().t04Ceiling;

/** Above `launchProfile`'s 12 USDC admission floor. */
const VALUE = 20_000_000n;
const OBSERVED_AT = 1_800_000_000n;

/** `RollBatchInFlight` — a batch is open. */
const ROLL_BATCH_IN_FLIGHT = 6200;
/** `T04CeilingIsProtocolWide` — `update_config`'s per-pool ceiling arm. */
const T04_CEILING_IS_PROTOCOL_WIDE = 6530;
/** `T04CeilingUnset` — the sweep at a 0 ceiling. */
const T04_CEILING_UNSET = 6531;
/** `SweepPoolSetIncomplete` — not every pool, once each. */
const SWEEP_POOL_SET_INCOMPLETE = 6532;

describe("sweep_t04_overflow — the excess above the protocol-wide ceiling, and nothing else", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let unprivileged: TransactionSigner;
  let roller: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let treasury04: Address;
  let treasury03: Address;
  let usdcMint: Address;
  let rollerUsdc: Address;
  let weightIndex: Address;
  let collection: PnftCollection;

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  async function t04CeilingOf(): Promise<bigint> {
    const { address } = await derivePda(["protocol"]);
    return decodeProtocolConfig((await fetchAccountData(connection, address))!).t04Ceiling;
  }

  async function balanceOf(tokenAccount: Address): Promise<bigint> {
    const { value } = await connection.rpc.getTokenAccountBalance(tokenAccount).send();
    return BigInt(value.amount);
  }

  /** Sets the ceiling to `CEILING` and the shared Treasury 04 to exactly `target`, draining it first
   * when it holds more, so each test sets its own starting state whatever ran before it. The top-up
   * is a bare `MintTo` into the PDA `init_pool` already created — never a create-and-fund, which the
   * runtime refuses. */
  async function fundTreasury04To(target: bigint): Promise<void> {
    await setCeiling(CEILING);
    if ((await balanceOf(treasury04)) > target) {
      await drainTreasury04({ connection, administrator: admin, operator });
    }
    const current = await balanceOf(treasury04);
    assert.ok(current <= target, `Treasury 04 holds ${current} after draining, above the ${target} target`);
    if (current === target) return;
    await sendIxs(connection, admin, [
      {
        programAddress: TOKEN_PROGRAM,
        accounts: [writable(usdcMint), writable(treasury04), signer(admin.address)],
        data: mintToData(target - current),
      },
    ]);
  }

  async function sweep(signerKey: TransactionSigner = operator, poolIds?: number[]) {
    return sendSweep({
      connection,
      feePayer: signerKey,
      tableAuthority: operator,
      instruction: await sweepT04Overflow({
        operator: signerKey.address,
        poolIds: poolIds ?? (await everyPoolId(connection)),
      }),
    });
  }

  async function setCeiling(ceiling: bigint, signerKey: TransactionSigner = admin) {
    return sendIxs(connection, signerKey, [
      await setT04Ceiling({ administrator: signerKey.address, ceiling }),
    ]);
  }

  after(async () => {
    await assertEveryPoolIdle(connection);
  });

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    unprivileged = await loadUnprivileged();
    roller = await fundedWallet(connection, 20);

    const protocolState = await ensureProtocol(connection, admin);
    usdcMint = protocolState.usdcMint;
    poolId = protocolState.poolCounter;

    await sendIxs(connection, admin, [
      await updateVrfConfig({
        administrator: admin.address,
        vrfProgram: VRF_PROGRAM,
        oracleQueue: VRF_QUEUE_FIXTURE,
        swapPool: SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: 100,
      }),
    ]);

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], createWiIx)]);
    weightIndex = weightIndexSigner.address;

    const pooled = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: launchProfile(),
    });
    pool = pooled.pool;
    treasury03 = pooled.treasury03;
    treasury04 = pooled.treasury04;

    collection = await createPnftCollection({ connection, payer: admin, authority: admin });
    // 0b011 — both Token Metadata standards. The rollable deposit below is a plain
    // NonFungible, which the pNFT-only default would refuse at intake.
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
      standards: 0b011,
    });

    await connection.mintTokens(usdcMint, admin, 1_000_000_000n, roller.address, false);
    rollerUsdc = await getTokenAccountAddress(roller.address, usdcMint, false);

    await setCeiling(CEILING);
    const busy = await poolsWithOpenBatches(connection, await everyPoolId(connection));
    assert.deepEqual(busy, [], `an earlier suite left a batch open in pools ${busy.join(", ")}`);
  });

  // ── The shared accounts and the ceiling ───────────────────────────────────────────────────

  it("every pool names the same Treasury 03 and Treasury 04, both held by the ProtocolConfig PDA", async () => {
    const { address: protocolConfig } = await derivePda(["protocol"]);
    for (const id of await everyPoolId(connection)) {
      const { address } = await derivePda(["pool", seedFromU16Le(id)]);
      const state = decodePool((await fetchAccountData(connection, address))!);
      assert.equal(state.treasury03, treasury03, `pool ${id}'s Treasury 03`);
      assert.equal(state.treasury04, treasury04, `pool ${id}'s Treasury 04`);
    }
    for (const account of [treasury03, treasury04]) {
      const { value } = await connection.rpc
        .getAccountInfo(account, { encoding: "jsonParsed" })
        .send();
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const owner = (value!.data as any).parsed.info.owner as Address;
      assert.equal(owner, protocolConfig, `${account}'s token authority`);
    }
  });

  it("refuses to sweep while the ceiling is 0", async () => {
    await fundTreasury04To(CEILING + 1_000_000n);
    await setCeiling(0n);
    const before = await balanceOf(treasury04);
    await expectAnchorError(sweep(), T04_CEILING_UNSET);
    assert.equal(await balanceOf(treasury04), before, "Treasury 04 moved at a 0 ceiling");
    await setCeiling(CEILING);
  });

  it("only the Administrator sets the ceiling, and the change is announced", async () => {
    await setCeiling(CEILING);
    await expectAnchorError(setCeiling(CEILING + 1n, operator), UNAUTHORIZED);
    assert.equal(await t04CeilingOf(), CEILING);

    const signature = await setCeiling(CEILING + 1n);
    assert.equal(await t04CeilingOf(), CEILING + 1n);
    const emitted = (await eventsOf(signature)).filter((e) => e.name === "T04CeilingSet");
    assert.equal(emitted.length, 1);
    assert.deepEqual(
      { ...(emitted[0]!.data as { old: bigint; new: bigint; authority: Address }), slot: 0n },
      { slot: 0n, authority: admin.address, old: CEILING, new: CEILING + 1n },
    );
    await setCeiling(CEILING);
  });

  it("refuses update_config's per-pool ceiling arm, leaving the pool's own field alone", async () => {
    const before = decodePool((await fetchAccountData(connection, pool))!).t04Ceiling;
    await expectAnchorError(
      sendIxs(connection, admin, [
        await updateConfig({
          administrator: admin.address,
          poolId,
          demoted: [],
          update: { kind: "T04Ceiling", value: 1_000_000n },
        }),
      ]),
      T04_CEILING_IS_PROTOCOL_WIDE,
    );
    assert.equal(decodePool((await fetchAccountData(connection, pool))!).t04Ceiling, before);
  });

  // ── The no-op path ────────────────────────────────────────────────────────────────────────

  it("is a no-op below the ceiling: no transfer, and no event to reconcile away", async () => {
    await fundTreasury04To(CEILING - 1_000_000n);
    const t04Before = await balanceOf(treasury04);
    const t03Before = await balanceOf(treasury03);
    assert.ok(t04Before < CEILING, "precondition: Treasury 04 starts below its ceiling");

    const signature = await sweep();

    assert.equal(await balanceOf(treasury04), t04Before, "Treasury 04 moved on a no-op sweep");
    assert.equal(await balanceOf(treasury03), t03Before, "Treasury 03 moved on a no-op sweep");
    assert.deepEqual(
      (await eventsOf(signature)).filter((e) => e.name === "T04Swept"),
      [],
      "a no-op sweep emitted a sweep event",
    );
  });

  it("is a no-op *at* the ceiling — the boundary a `>` written for a `>=` would move", async () => {
    await fundTreasury04To(CEILING);
    const t03Before = await balanceOf(treasury03);

    const signature = await sweep();

    assert.equal(await balanceOf(treasury04), CEILING, "a balance exactly at the ceiling was swept");
    assert.equal(await balanceOf(treasury03), t03Before);
    assert.deepEqual(
      (await eventsOf(signature)).filter((e) => e.name === "T04Swept"),
      [],
    );
  });

  // ── The transfer path ─────────────────────────────────────────────────────────────────────

  it("moves exactly the excess to Treasury 03, and leaves Treasury 04 at its ceiling", async () => {
    const excess = 137_000_000n;
    await fundTreasury04To(CEILING + excess);
    const t03Before = await balanceOf(treasury03);

    const signature = await sweep();

    assert.equal(await balanceOf(treasury04), CEILING, "Treasury 04 is not left at its ceiling");
    assert.equal(
      await balanceOf(treasury03),
      t03Before + excess,
      "Treasury 03 received something other than exactly the excess",
    );

    const emitted = (await eventsOf(signature)).filter((e) => e.name === "T04Swept");
    assert.equal(emitted.length, 1, "the transfer path emits exactly one sweep event");
    const data = emitted[0]!.data as { amount: bigint; ceiling: bigint };
    assert.equal(data.amount, excess, "the event's amount disagrees with what actually moved");
    assert.equal(data.ceiling, CEILING);
  });

  it("refuses a pool set that is not every pool once, leaving both treasuries alone", async () => {
    await fundTreasury04To(CEILING + 5_000_000n);
    const all = await everyPoolId(connection);
    assert.ok(all.length >= 2, "precondition: at least two pools, so one can be left out");
    const t04Before = await balanceOf(treasury04);
    const t03Before = await balanceOf(treasury03);
    for (const [label, ids] of [
      ["one pool short", all.slice(1)],
      ["a repeat in place of a pool", [all[0]!, ...all.slice(0, -1)]],
      // A pool-tagged account at its own id's address, loaded as a fixture: an id no `init_pool` reached.
      ["a pool past pool_counter", [...all.slice(0, -1), UNCONSOLIDATED_POOL_ID]],
    ] as const) {
      await expectAnchorError(sweep(operator, [...ids]), SWEEP_POOL_SET_INCOMPLETE).catch((e) => {
        throw new Error(`${label}: ${e instanceof Error ? e.message : String(e)}`);
      });
    }
    assert.equal(await balanceOf(treasury04), t04Before);
    assert.equal(await balanceOf(treasury03), t03Before);
  });

  // ── Authorisation ─────────────────────────────────────────────────────────────────────────

  describe("only the Operator may trigger it", () => {
    before(async () => {
      await fundTreasury04To(CEILING + 50_000_000n);
    });

    for (const [label, who] of [
      ["the administrator", () => admin],
      ["an unprivileged wallet", () => unprivileged],
    ] as const) {
      it(`refuses ${label}`, async () => {
        await expectAnchorError(sweep(who()), UNAUTHORIZED);
      });
    }

    it("refuses a call whose operator slot is someone else's key", async () => {
      const ix = await sweepT04Overflow({ operator: operator.address, poolIds: await everyPoolId(connection) });
      await expectAnchorError(
        sendSweep({
          connection,
          feePayer: unprivileged,
          tableAuthority: operator,
          instruction: withSigner(ix, unprivileged.address),
        }),
        UNAUTHORIZED,
      );
    });

    it("covers exactly the matrix rows this suite is answerable for", () => {
      assert.deepEqual(
        EXECUTED_ROWS.filter((r) => r.suite === "treasury-ceiling-sweep").map((r) => `${r.instruction}.${r.signer}`),
        ["set_t04_ceiling.administrator", "sweep_t04_overflow.operator"],
      );
    });

    // The control the refusals above cannot supply. Every one of them passes just as well
    // against a handler that refuses everyone, including the party meant to be allowed.
    it("accepts the Operator — the success the refusals cannot stand in for", async () => {
      const t04Before = await balanceOf(treasury04);
      assert.ok(t04Before > CEILING, "precondition: there is an excess to move");

      await sweep(operator);

      assert.equal(await balanceOf(treasury04), CEILING, "the Operator's own sweep did not run");
    });
  });

  // ── The open-batch gate, and the property it exists for ───────────────────────────────────

  describe("it cannot run while a batch is open", () => {
    let batchId: bigint;
    let fundedAtCommit: bigint;
    let rollable: MintedAsset;

    // The batch is left open on purpose above, and every later sweep needs every pool idle, so it
    // is settled here. Treasury 04 is funded past any face, and the localnet has no venue to pay a
    // blank, so the randomness is chosen to draw the card.
    after(async () => {
      const state = decodePool((await fetchAccountData(connection, pool))!);
      const total = state.wReal + blankWeight(state.blanks, state.blankFaces);
      let randomness: Uint8Array | null = null;
      for (let fill = 0; fill < 256 && randomness === null; fill += 1) {
        const candidate = new Uint8Array(32).fill(fill);
        const target = classifyDraw(expandDraw(candidate, 0, total).r, state.blanks, state.blankFaces);
        if (target.kind !== "blank") randomness = candidate;
      }
      assert.ok(randomness, "no fill byte draws the card");
      await sendIxs(connection, operator, [
        await testResolveBatch({ operator: operator.address, pool, batchId, randomness }),
      ]);
      const { address: protocolConfig } = await derivePda(["protocol"]);
      const drawn = await settleDrawnRoll(
        { connection, operator, pool, protocolConfig, weightIndex, roller: roller.address, rollerUsdc, assets: [rollable] },
        batchId,
        0,
      );
      assert.equal(drawn, 0, "the cleanup roll did not draw the card");
      assert.equal(decodePool((await fetchAccountData(connection, pool))!).openBatches, 0);
    });

    before(async () => {
      // One approved position, so `commit_rolls` has a non-empty draw domain.
      rollable = await mintPlainNonFungiblePnft({
        connection,
        payer: admin,
        authority: admin,
        owner: roller.address,
        collection,
      });
      await sendIxs(connection, roller, [
        await deposit({
          depositor: roller.address,
          poolId,
          collection: collection.mint,
          nftMint: rollable.mint,
          depositorToken: rollable.tokenAccount,
          metadata: rollable.metadata,
          masterEdition: rollable.masterEdition,
          depositorTokenRecord: null,
          positionTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
        }),
      ]);
      await sendIxs(connection, operator, [
        await approveDeposit({
          operator: operator.address,
          poolId,
          nftMint: rollable.mint,
          depositor: roller.address,
          weightIndex,
          displaced: null,
          value: VALUE,
          observedAt: OBSERVED_AT,
          lockUntil: 0n,
        }),
      ]);

      // Treasury 04 holds a real balance when the batch opens — this is the figure the batch's
      // blank outcomes would be paid from, and the figure the gate has to protect.
      await fundTreasury04To(CEILING + 400_000_000n);
      fundedAtCommit = await balanceOf(treasury04);

      batchId = decodePool((await fetchAccountData(connection, pool))!).batchCounter;
      await sendIxs(connection, roller, [
        await commitRolls({
          roller: roller.address,
          poolId,
          batchCounter: batchId,
          rollerUsdc,
          usdcMint,
          oracleQueue: VRF_QUEUE_FIXTURE,
          n: 1,
        }),
      ]);
    });

    it("refuses the sweep outright", async () => {
      assert.equal(decodePool((await fetchAccountData(connection, pool))!).openBatches, 1);
      await expectAnchorError(sweep(), ROLL_BATCH_IN_FLIGHT);
    });

    // The ceiling is not frozen behind an open batch, so this ordering is reachable without
    // anyone intending it: the ceiling drops mid-batch, and the next routine sweep would take the
    // "excess" the new ceiling invents out of money the batch is relying on. The gate is what
    // makes the ordering safe rather than lucky.
    it("still refuses after the ceiling is lowered mid-batch, leaving the balance intact", async () => {
      await setCeiling(1_000_000n);
      assert.equal(
        await t04CeilingOf(),
        1_000_000n,
        "precondition: the Administrator lowered the ceiling while the batch was open",
      );

      await expectAnchorError(sweep(), ROLL_BATCH_IN_FLIGHT);

      assert.equal(
        await balanceOf(treasury04),
        fundedAtCommit,
        "Treasury 04's balance moved while a batch was open — the blank outcomes' funding is not intact",
      );
    });
  });
});
