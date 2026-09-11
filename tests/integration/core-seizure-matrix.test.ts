// T-110 — AC-84's nine cases: **3 causes × 3 source states**, against a live validator.
//
// AC-84 names three ways a third party removes an escrowed `MplCoreAsset` from bye.fun's reach —
// transfer it out, burn it, freeze it in place — and requires each to be exercised from every
// state the card can be escrowed in: `Pending`, `Active`, and `ClosedBelowFloor` awaiting a
// claim. The nine are generated from two literal tables below rather than written out, because
// the criterion is a *product*: a case written by hand is a case that can be omitted by hand, and
// nine `it(...)` blocks with one combination missing look exactly like nine that are complete.
//
// **The three causes are three plugins, and only one of them is applied after escrow.** A
// permanent delegate is addable only at an asset's creation, so `PermanentTransferDelegate` and
// `PermanentBurnDelegate` travel through `mintCoreAsset` and are exercised by acting on the card.
// `PermanentFreezeDelegate` cannot: an asset minted `frozen` cannot be transferred at all, so
// `deposit_core`'s own `TransferV1` would fail and the position the freeze is meant to strand
// would never exist. It is minted `frozen: false` and flipped with `UpdatePluginV1` after the
// card is collateral — which is not a workaround but the cause stated precisely, since what
// AC-84 describes is a third party reaching into a card that is *already* escrowed.
//
// **The three source states are three different effect sets, and that is the whole reason the
// matrix is a product rather than three cases.** Only `Active` holds weight, a Fenwick leaf, both
// `WalletStats` counters and possibly tier membership; `Pending` was never activated and
// `ClosedBelowFloor` had every one of those removed by the sweep that closed it. Applying
// `Active`'s effect set to either of the others double-decrements counters that were incremented
// once — a `67xx` corruption *created by the cleanup instruction* — so each row asserts the set
// that ran **and the two that did not**. T-109 drove `Pending` alone and left both other rows
// unexecuted; this file is where the branch is measured rather than argued.
//
// **The claim assertion is the one AC-84 applies to every cause, and its three answers are not
// the same answer.** D-115: deactivation closes a state and never deallocates the position, its
// vault or the claim — so `claim_nft_core` must remain *callable* on all nine. On the two
// reversible causes it must also **succeed** once the party that caused them gives the card back,
// which is what separates them from the burn: there, the claim stays callable and cannot succeed,
// and `CollateralAbsent` (6307) is the expected result rather than a failure. A claim that
// answered `AccountNotInitialized` on any of the nine would mean the position had been
// deallocated, and that is the shape this file exists to refuse.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import {
  burnCoreAsset,
  createCoreCollection,
  mintCoreAsset,
  readCoreAsset,
  setPermanentFreeze,
  transferCoreAsset,
  type CorePlugin,
} from "../helpers/core-fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { approveDeposit, depositCore } from "../../scripts/lib/deposit.js";
import { claimNftCore, closeSeized } from "../../scripts/lib/exit.js";
import { beginSweep, endSweep, recordValue } from "../../scripts/lib/value.js";
import { resolveSuccessor } from "../../scripts/lib/tier-resolve.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import {
  decodeEvent,
  decodePool,
  decodePosition,
  decodeWalletStats,
  type DecodedEvent,
} from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { SYSTEM_PROGRAM } from "../../scripts/constants.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const WALLET_STATS_SEED = "wallet";
const PROGRAM_DATA_PREFIX = "Program data: ";

/** The card is gone — `claim_nft_core`'s answer on the one cause nothing reverses. */
const COLLATERAL_ABSENT = 6307;

const PROFILE = launchProfile("11111111111111111111111111111111" as Address);
const ADMISSION_FLOOR = PROFILE.admissionFloor;
const TICKET_TARGET = PROFILE.ticketTarget;
const ADMITTED_VALUE = 20_000_000n;
const BELOW_FLOOR_VALUE = 1_000_000n;

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

/**
 * The three causes, each as the plugin that makes it possible and the act that uses it.
 *
 * `kind` is `SeizureKind`'s wire name as `decodePositionSeized` reports it, and `reversible`
 * carries AC-84's own split: the transfer and the freeze are undone by the party that caused
 * them, the burn is not.
 */
const CAUSES = ["transferred", "burned", "frozen"] as const;
type Cause = (typeof CAUSES)[number];

/** The three states a card can be escrowed in — SM-01's three inbound `Seized` edges. */
const SOURCES = ["Pending", "Active", "ClosedBelowFloor"] as const;
type Source = (typeof SOURCES)[number];

/**
 * What `PositionSeized.recorded_value` carries on each source state — see the assertion below
 * for why these are three different quantities rather than one with a zero case.
 */
const EXPECTED_RECORDED_VALUE: Record<Source, bigint> = {
  Pending: 0n,
  Active: ADMITTED_VALUE,
  ClosedBelowFloor: BELOW_FLOOR_VALUE,
};

const SEIZURE_KIND: Record<Cause, string> = {
  transferred: "Transferred",
  burned: "Burned",
  frozen: "Frozen",
};

function isPositionSeized(
  event: DecodedEvent,
): event is Extract<DecodedEvent, { name: "PositionSeized" }> {
  return event.name === "PositionSeized";
}

describe("T-110 — AC-84's nine seizure cases against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  /**
   * Holds every permanent delegate this file mints — and is never the collection authority, the
   * depositor or the `close_seized` caller. AC-84's audit event names who holds the card, so the
   * party that reaches in has to be a wallet an assertion can name.
   */
  let thief: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;
  let collection: Address;
  let collectionAuthority: TransactionSigner;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 30);
    thief = await fundedWallet(connection, 20);

    const { usdcMint, poolCounter, operator: op } = await ensureProtocol(connection, admin);
    operator = op;

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [
      addSignersToInstruction([admin, weightIndexSigner], createWiIx),
    ]);
    weightIndex = weightIndexSigner.address;

    poolId = poolCounter;
    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: { ...launchProfile(admin.address) },
    });
    pool = poolAccounts.pool;

    const core = await createCoreCollection({ connection, payer: admin, name: "T-110 seizures" });
    collection = core.collection;
    collectionAuthority = core.authority;
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection,
      standards: 0b100,
    });

    assert.ok(
      ADMITTED_VALUE >= ADMISSION_FLOOR && BELOW_FLOOR_VALUE < ADMISSION_FLOOR,
      `fixture values no longer straddle the admission floor (${ADMISSION_FLOOR})`,
    );
    assert.ok(
      BELOW_FLOOR_VALUE < TICKET_TARGET,
      `the below-floor value must sit under the ticket target ${TICKET_TARGET}`,
    );
  });

  async function positionPdas(asset: Address) {
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(asset),
    ]);
    const { address: positionVault } = await derivePda([
      POSITION_VAULT_SEED,
      seedFromPubkey(position),
    ]);
    return { position, positionVault };
  }

  async function readPool() {
    return decodePool((await fetchAccountData(connection, pool))!);
  }

  async function readWalletStats() {
    const { address: walletStats } = await derivePda([
      WALLET_STATS_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(depositor.address),
    ]);
    const data = await fetchAccountData(connection, walletStats);
    return data === null ? null : decodeWalletStats(data);
  }

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  /** The plugin the cause needs, always held by `thief` and never by the owner or this program. */
  function pluginFor(cause: Cause): CorePlugin {
    switch (cause) {
      case "transferred":
        return { kind: "permanentTransferDelegate", authority: thief.address };
      case "burned":
        return { kind: "permanentBurnDelegate", authority: thief.address };
      case "frozen":
        // Minted unfrozen deliberately — see the header. The freeze arrives after escrow.
        return { kind: "permanentFreezeDelegate", authority: thief.address, frozen: false };
    }
  }

  /**
   * One sweep that closes `asset`'s position below the floor.
   *
   * The `finally` is not defensive tidying: `begin_sweep` sets `pool.sweep_pending`, and a
   * `record_value` that throws with the flag still set leaves every later case in this file
   * reding on `SweepAlreadyOpen` instead of on its own assertion. `season-lock.test.ts` carries
   * the same shape for the same reason.
   */
  async function sweepClose(asset: Address, position: Address): Promise<void> {
    await sendIxs(connection, operator, [await beginSweep({ operator: operator.address, poolId })]);
    try {
      const successor = await resolveSuccessor(connection, pool, position);
      await sendIxs(connection, operator, [
        await recordValue({
          operator: operator.address,
          poolId,
          nftMint: asset,
          depositor: depositor.address,
          weightIndex,
          value: BELOW_FLOOR_VALUE,
          observedAt: nowSeconds(),
          effect: { kind: "close", successor },
        }),
      ]);
    } finally {
      await sendIxs(connection, operator, [await endSweep({ operator: operator.address, poolId })]);
    }
  }

  /** Escrows one genuine `AssetV1` carrying the cause's plugin, and drives it to `source`. */
  async function escrowInState(cause: Cause, source: Source): Promise<Address> {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection,
      collectionAuthority,
      owner: depositor.address,
      plugins: [pluginFor(cause)],
    });
    await sendIxs(connection, depositor, [
      await depositCore({ depositor: depositor.address, poolId, collection, asset }),
    ]);
    const { position, positionVault } = await positionPdas(asset);
    const escrowed = await readCoreAsset(connection, asset);
    assert.ok(escrowed, "the fixture asset vanished during the deposit");
    assert.equal(escrowed.owner, positionVault, "the fixture did not reach escrow");

    if (source !== "Pending") {
      await sendIxs(connection, operator, [
        await approveDeposit({
          operator: operator.address,
          poolId,
          nftMint: asset,
          depositor: depositor.address,
          weightIndex,
          displaced: null,
          value: ADMITTED_VALUE,
          observedAt: nowSeconds(),
          // Zero, so the below-floor sweep below *closes* rather than retaining: a lock still in
          // force turns `record_value`'s close arm into the retain arm and the source state this
          // case names would never be reached.
          lockUntil: 0n,
        }),
      ]);
    }
    if (source === "ClosedBelowFloor") await sweepClose(asset, position);

    const reached = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(reached.state, source, "the fixture did not reach the source state it names");
    return asset;
  }

  /** The third party acts on a card that is already collateral. */
  async function applyCause(cause: Cause, asset: Address): Promise<void> {
    switch (cause) {
      case "transferred":
        await transferCoreAsset({
          connection,
          payer: thief,
          asset,
          collection,
          authority: thief,
          newOwner: thief.address,
        });
        return;
      case "burned":
        await burnCoreAsset({ connection, payer: thief, asset, collection, authority: thief });
        return;
      case "frozen":
        await setPermanentFreeze({
          connection,
          payer: thief,
          asset,
          collection,
          authority: thief,
          frozen: true,
        });
        return;
    }
  }

  /** Undoes the two reversible causes. Never called on the burn — nothing undoes a burn. */
  async function reverseCause(cause: Cause, asset: Address, vault: Address): Promise<void> {
    switch (cause) {
      case "transferred":
        await transferCoreAsset({
          connection,
          payer: thief,
          asset,
          collection,
          authority: thief,
          newOwner: vault,
        });
        return;
      case "frozen":
        await setPermanentFreeze({
          connection,
          payer: thief,
          asset,
          collection,
          authority: thief,
          frozen: false,
        });
        return;
      case "burned":
        throw new Error("a burn is not reversible — this cause must never reach here");
    }
  }

  /**
   * Who `PositionSeized.owner_observed` must name.
   *
   * Not one answer with two exceptions: each is the fact an auditor needs for that cause. On a
   * transfer it is the new holder; on a freeze the card never moved, so it is still the vault;
   * on a burn there is nobody holding it, and `read_collateral` returns `Pubkey::default()` —
   * which is the all-zero key the system program also carries.
   */
  function expectedOwnerObserved(cause: Cause, vault: Address): Address {
    switch (cause) {
      case "transferred":
        return thief.address;
      case "frozen":
        return vault;
      case "burned":
        return SYSTEM_PROGRAM;
    }
  }

  for (const cause of CAUSES) {
    for (const source of SOURCES) {
      it(`${cause} from ${source}: closes, sheds its weight, and leaves the claim callable`, async () => {
        const asset = await escrowInState(cause, source);
        const { position, positionVault } = await positionPdas(asset);

        // Read *after* the fixture reached its source state, so the comparison isolates what
        // `close_seized` did rather than what the deposit and the approval did.
        const poolBefore = await readPool();
        const statsBefore = await readWalletStats();
        assert.ok(statsBefore, "the deposit must have opened the depositor's WalletStats");
        const holdsWeight = source === "Active";

        await applyCause(cause, asset);

        // Permissionless (SD-10): signed by a wallet with no role in this pool at all. The
        // collateral read is the entire authorisation, and this is the assertion that says so.
        const caller = await fundedWallet(connection, 5);
        const signature = await sendIxs(connection, caller, [
          await closeSeized({
            caller: caller.address,
            poolId,
            asset,
            collection,
            depositor: depositor.address,
            weightIndex,
          }),
        ]);

        // ── Closure (D-115) ──────────────────────────────────────────────────────────────────
        // The state moves and **nothing is deallocated**. AC-84 fails a closure that frees the
        // position outright, because the claim it owes the depositor lives in that account.
        const seizedData = await fetchAccountData(connection, position);
        assert.notEqual(seizedData, null, "the seizure deallocated the position");
        assert.equal(decodePosition(seizedData!).state, "Seized");
        assert.equal(
          await fetchAccountData(connection, positionVault),
          null,
          "the seizure allocated the vault it only ever reads",
        );

        // ── Weight departure, and the two effect sets that must not have run ────────────────
        const poolAfter = await readPool();
        const statsAfter = await readWalletStats();
        assert.ok(statsAfter);
        if (holdsWeight) {
          assert.equal(poolAfter.nReal, poolBefore.nReal - 1, "the Active row kept n_real");
          assert.ok(
            poolAfter.wReal < poolBefore.wReal,
            "the Active row's weight never left w_real",
          );
          assert.equal(
            statsAfter.activePositions,
            statsBefore.activePositions - 1,
            "the depositor still counts a position that no longer exists",
          );
          assert.equal(
            statsAfter.activeValue,
            statsBefore.activeValue - ADMITTED_VALUE,
            "the depositor still counts the seized card's value",
          );
        } else {
          // The pool is the control on these two rows. `Pending` never incremented any of these
          // counters and `ClosedBelowFloor` already had all four decremented at closure, so a
          // decrement here is one too many — the corruption the branch exists to prevent, and
          // invisible to any test that only drives `Active`.
          assert.equal(poolAfter.nReal, poolBefore.nReal, `a ${source} seizure moved n_real`);
          assert.equal(poolAfter.wReal, poolBefore.wReal, `a ${source} seizure moved w_real`);
          assert.deepEqual(
            poolAfter.blanks,
            poolBefore.blanks,
            `a ${source} seizure re-evaluated the blank set`,
          );
          assert.equal(
            statsAfter.activePositions,
            statsBefore.activePositions,
            `a ${source} seizure decremented WalletStats.active_positions`,
          );
          assert.equal(
            statsAfter.activeValue,
            statsBefore.activeValue,
            `a ${source} seizure decremented WalletStats.active_value`,
          );
        }

        // ── The audit event names the observed cause (AC-84) ────────────────────────────────
        const events = await eventsOf(signature);
        const seizures = events.filter(isPositionSeized);
        assert.equal(seizures.length, 1, "expected exactly one PositionSeized event");
        assert.equal(seizures[0].data.kind, SEIZURE_KIND[cause], "the event named the wrong cause");
        assert.equal(seizures[0].data.sourceState, source);
        assert.equal(seizures[0].data.position, position);
        assert.equal(
          seizures[0].data.ownerObserved,
          expectedOwnerObserved(cause, positionVault),
          "owner_observed is the fact an auditor needs and no other event carries",
        );
        // **Three source states, three meanings, and the field name is the same on all three.**
        // `Pending` was never valued, so it is 0. `Active` is the value the position actually
        // held weight at. `ClosedBelowFloor` is neither: `record_value` writes
        // `position.recorded_value = value` unconditionally, *after* removing the weight against
        // the old one, so what survives on a closed position is the below-floor observation that
        // closed it. An auditor reading this field off a seizure event without `source_state`
        // beside it cannot tell those apart — which is design §5's stated reason for publishing
        // the source state at all, measured here rather than assumed.
        assert.equal(
          seizures[0].data.recordedValue,
          EXPECTED_RECORDED_VALUE[source],
          "the event published a value the position does not hold",
        );

        // Conditional on the same `Option` the weight removal is: an unconditional emit would
        // report a blank-set re-evaluation on a source state that moved no weight.
        assert.equal(
          events.filter((event) => event.name === "RebalanceEvaluated").length,
          holdsWeight ? 1 : 0,
          `a ${source} seizure reported the wrong number of rebalances`,
        );

        // ── The claim, on every one of the nine (AC-84's universal clause) ──────────────────
        if (cause === "burned") {
          // Callable and refusing, which is the expected result rather than a failure: the
          // handler's own collateral gate answers, so dispatch and account resolution both ran.
          // `AccountNotInitialized` here would mean the position had been deallocated.
          await expectAnchorError(
            sendIxs(connection, depositor, [
              await claimNftCore({ depositor: depositor.address, pool, asset, collection }),
            ]),
            COLLATERAL_ABSENT,
          );
          assert.notEqual(
            await fetchAccountData(connection, position),
            null,
            "a refused claim must leave the position standing",
          );
          return;
        }

        // Both reversible causes: once the party that caused it gives the card back, the claim
        // does not merely stay callable — it succeeds, from the `Seized` terminal, and pays the
        // position's rent back to the depositor as it closes.
        await reverseCause(cause, asset, positionVault);
        await sendIxs(connection, depositor, [
          await claimNftCore({ depositor: depositor.address, pool, asset, collection }),
        ]);
        const released = await readCoreAsset(connection, asset);
        assert.ok(released, "the asset account must survive its own release");
        assert.equal(
          released.owner,
          depositor.address,
          "the claim did not return the card to its depositor",
        );
        assert.equal(
          await fetchAccountData(connection, position),
          null,
          "the claim is the one instruction that does close the position",
        );
      });
    }
  }
});
