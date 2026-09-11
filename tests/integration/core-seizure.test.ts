// T-109: `close_seized` executed against a live validator, on the one source state that needs no
// weight to move.
//
// **The fixture is the hard part, and it is what made this instruction unexecutable until now.**
// Once a Core card is escrowed, `AssetV1.owner` is `PDA(["vault", position])`, and that PDA signs
// only through `bye_machine` — so no key this suite holds can move the card out from under it, and
// without a card that has left the vault there is nothing for a seizure to observe. A
// `PermanentTransferDelegate`, held by a third party and addable **only at creation**, is what
// makes the state constructible: design §3.4 records exactly that plugin sitting with an external
// authority on the Core collection this pool admits, which is why `close_seized` exists at all.
// The fixture is therefore not a contrivance standing in for the hazard — it *is* the hazard.
//
// It is also the first time anything in this repository presents this program with an asset that
// carries a plugin registry: every Core fixture before it minted with `plugins: None`, so
// `BaseAssetV1::from_bytes` has never read this program's fields out of an account whose tail is
// plugin data. What that does *not* exercise is the freeze read, and the ordering is the reason
// rather than an oversight — `read_collateral` tests ownership before frozenness deliberately, so
// a card both moved out and frozen reports `Transferred` with the new owner, and returns before
// the registry is walked. The `fetch_plugin` walks are reached by the refusal case below and by
// the three exits, where the asset is present and vault-owned and carries no registry at all:
// that is `core_asset.rs`'s `Err → false` fail-closed arm, which nothing had executed either.
//
// **Two cases, and the refusal is not the lesser of them.** `close_seized` is permissionless: the
// collateral read is the whole of its authorisation (SD-10), so a caller naming a present,
// vault-owned, unfrozen asset getting `CollateralPresent` (6308) is the assertion that a stranger
// cannot deactivate a healthy position, remove its weight and strand a perfectly transferable
// card. The nine-case matrix — 3 causes × 3 source states, with the weight departure and the
// claim-after-seizure — is T-110's, and this file is deliberately not a down payment on it: what
// T-109 owes is that the encoder, the dispatch, the account resolution and `decodePositionSeized`
// have each run once against a real program.

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
  createCoreCollection,
  mintCoreAsset,
  readCoreAsset,
  transferCoreAsset,
} from "../helpers/core-fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { depositCore } from "../../scripts/lib/deposit.js";
import { closeSeized } from "../../scripts/lib/exit.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { decodeEvent, decodePool, decodePosition, type DecodedEvent } from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const PROGRAM_DATA_PREFIX = "Program data: ";

/** The card is present, vault-owned and unfrozen — there is nothing to clean up. */
const COLLATERAL_PRESENT = 6308;

function isPositionSeized(
  event: DecodedEvent,
): event is Extract<DecodedEvent, { name: "PositionSeized" }> {
  return event.name === "PositionSeized";
}

describe("T-109 — close_seized against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let depositor: TransactionSigner;
  /** Holds the permanent transfer delegate. Not the collection authority, and not the depositor:
   * the party that reaches in has to be nameable in an assertion. */
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
    depositor = await fundedWallet(connection, 20);
    thief = await fundedWallet(connection, 10);

    const { usdcMint, poolCounter } = await ensureProtocol(connection, admin);

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

    const core = await createCoreCollection({ connection, payer: admin, name: "T-109 seizure" });
    collection = core.collection;
    collectionAuthority = core.authority;
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection,
      standards: 0b100,
    });
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

  /**
   * Escrows one genuine `AssetV1`, optionally carrying a permanent transfer delegate held by
   * `thief`. The position is left `Pending`: no approval, so no weight, no Fenwick leaf and no
   * `WalletStats` increment — the source state whose effect set is empty, which is what keeps
   * this file clear of T-110's accounting matrix.
   */
  async function depositCoreAsset(options: { seizable: boolean }): Promise<Address> {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection,
      collectionAuthority,
      owner: depositor.address,
      plugins: options.seizable
        ? [{ kind: "permanentTransferDelegate", authority: thief.address }]
        : [],
    });
    await sendIxs(connection, depositor, [
      await depositCore({ depositor: depositor.address, poolId, collection, asset }),
    ]);
    const { positionVault } = await positionPdas(asset);
    const escrowed = await readCoreAsset(connection, asset);
    assert.ok(escrowed, "the fixture asset vanished during the deposit");
    assert.equal(escrowed.owner, positionVault, "the fixture did not reach escrow");
    return asset;
  }

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  it("refuses a position whose card is still in its own vault, and changes nothing", async () => {
    const asset = await depositCoreAsset({ seizable: false });
    const stranger = await fundedWallet(connection, 5);

    await expectAnchorError(
      sendIxs(connection, stranger, [
        await closeSeized({
          caller: stranger.address,
          poolId,
          asset,
          collection,
          depositor: depositor.address,
          weightIndex,
        }),
      ]),
      COLLATERAL_PRESENT,
    );

    const { position } = await positionPdas(asset);
    const untouched = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(untouched.state, "Pending", "a refused seizure must not move the state");
  });

  it("seizes a Pending position whose card a permanent delegate took, and keeps the account", async () => {
    const asset = await depositCoreAsset({ seizable: true });
    const { position, positionVault } = await positionPdas(asset);
    const before = decodePool((await fetchAccountData(connection, pool))!);

    // The seizure itself: the delegate moves the card out of the vault without the vault — or
    // this program — having any say in it.
    await transferCoreAsset({
      connection,
      payer: thief,
      asset,
      collection,
      authority: thief,
      newOwner: thief.address,
    });
    const taken = await readCoreAsset(connection, asset);
    assert.ok(taken);
    assert.equal(taken.owner, thief.address, "the delegate did not take the card");

    // Permissionless, and signed by a wallet with no role of any kind: the read is the
    // authorisation.
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

    // D-115: the state moves and the account stays, because `claim_nft_core` is still owed —
    // even here, where the card is gone and the claim would refuse. What survives the seizure is
    // the depositor's standing to be told so.
    const seized = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(seized.state, "Seized");
    assert.equal(
      await fetchAccountData(connection, positionVault),
      null,
      "the seizure allocated the vault it only ever reads",
    );

    // The `Pending` row's effect set is empty, and the pool is the control: applying the
    // `Active` row's set here would decrement counters that were never incremented for this
    // position, which is the `67xx` corruption the branch exists to prevent.
    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.nReal, before.nReal, "a Pending seizure decremented n_real");
    assert.equal(after.wReal, before.wReal, "a Pending seizure decremented w_real");
    assert.deepEqual(after.blanks, before.blanks, "a Pending seizure re-evaluated the blank set");

    // `decodePositionSeized` and its `SeizureKind` table, against bytes the program emitted
    // rather than bytes a test wrote. `owner_observed` is the fact an auditor needs and the one
    // no other event carries: it names who holds the card now.
    const events = await eventsOf(signature);
    const seizures = events.filter(isPositionSeized);
    assert.equal(seizures.length, 1, "expected exactly one PositionSeized event");
    assert.equal(seizures[0].data.kind, "Transferred");
    assert.equal(seizures[0].data.ownerObserved, thief.address);
    assert.equal(seizures[0].data.sourceState, "Pending");
    assert.equal(seizures[0].data.position, position);
    assert.equal(seizures[0].data.recordedValue, 0n, "a Pending position was never valued");

    // Conditional on the same `Option` the weight removal is — an unconditional emit would claim
    // a blank-set re-evaluation on a source state that moved no weight.
    assert.equal(
      events.filter((event) => event.name === "RebalanceEvaluated").length,
      0,
      "a Pending seizure reported a rebalance it did not run",
    );
    assert.equal(
      events.filter((event) => event.name === "TierChanged").length,
      0,
      "a Pending position holds no tier membership to vacate",
    );
  });
});
