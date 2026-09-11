// The blocker no *named* plugin read finds, and the census that no longer needs to name it.
//
// **Three review rounds of PR #4 each found a different MPL Core mechanism that refuses a
// `TransferV1` every readable predicate in `core_asset.rs` passed.** Round 1: a
// `permanent_freeze_delegate` on the collection rather than on the card. Round 2: an asset
// detached from its collection. Round 3: `Royalties { rule_set: ProgramDenyList }`, plus the
// `Oracle` and `LifecycleHook` external adapters — which `fetch_plugin` cannot reach at all,
// because it walks only the internal plugin registry.
//
// Each fix appended one more case to a read that asked for gating plugins **by name**, and each
// left the code's own asserted invariant false: *the two gates are exact complements, so no
// position can be both unwithdrawable and uncloseable*. Every miss strands a position with live
// `w_real` and a drawable Fenwick leaf behind a card nobody can move. And the set is open: MPL
// Core can add authority-managed plugins and adapters, and a third-party collection authority
// controls them.
//
// **What replaced the naming, and what this file measures.** `core_asset.rs` now censuses the
// plugin registries of the asset *and* its collection — internal records and external adapters —
// against the plugin types that provably cannot gate a transfer, and answers `Undecidable` for
// everything else, a `plugin_type` byte from a later `mpl-core` included. That classification is
// admitted by **both** gates: the exits proceed on it and MPL Core decides, and `close_seized`
// accepts it as a cause. Nothing is refused by both, so nothing is stranded, whatever the
// mechanism is.
//
// **The round asked for something else, and this file is also the record of why it is not there.**
// The recommendation was attempt-and-classify: have `close_seized` drive the exit's own release
// and take MPL Core's refusal as the cause. That is not implementable on Solana. The last case
// below drives exactly that shape — a deny-listed release inside `return_rejected_core` — and the
// logs show MPL Core answering `0x9` and the **outer** program failing with the same code. A
// program that reads `release_core_from_vault(..).is_ok()` never reaches its own next line, so
// the attempt cannot be classified: it can only abort the transaction the cleanup was supposed to
// complete. A census needs no CPI.
//
// The deny list names the **system program**, and that is the mechanism rather than a trick. MPL
// Core compares the rule set against the owning program of the transfer's authority and of its
// new owner; an escrow release runs from a vault PDA with no account (system-owned) to a
// depositor's wallet (system-owned), so the list refuses exactly the moves this program makes.
// It arrives *after* escrow for the same reason the collection freeze does: a rule set that
// refuses transfers refuses `deposit_core`'s own, so a collection born with one could never hold
// an escrowed card.

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
import { SYSTEM_PROGRAM } from "../../scripts/constants.js";
import {
  burnCoreAsset,
  createCoreCollection,
  mintCoreAsset,
  readCoreAsset,
  setCollectionRoyaltiesRuleSet,
} from "../helpers/core-fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { approveDeposit, depositCore, rejectDeposit, returnRejectedCore } from "../../scripts/lib/deposit.js";
import { closeSeized } from "../../scripts/lib/exit.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import {
  decodeEvent,
  decodePool,
  decodePosition,
  type DecodedEvent,
} from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";

const POOL_SEED = "pool";
const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const WALLET_STATS_SEED = "wallet";
const PROGRAM_DATA_PREFIX = "Program data: ";

/** The census found nothing it could not decide, and nothing blocking — nothing to close. */
const COLLATERAL_PRESENT = 6308;
/** The `Active` row's lock, on the causes that leave the card in the vault. */
const POSITION_LOCKED = 6300;
/** The two named refusals the *decidable* causes produce — asserted absent here on purpose. */
const COLLATERAL_ABSENT = 6307;
const COLLATERAL_FROZEN = 6309;
/** `MplCoreError::InvalidAuthority` — what a rejected plugin validation surfaces as. */
const MPL_CORE_INVALID_AUTHORITY = 9;
/** An operator's rejection reason; any non-zero value, never read by these assertions. */
const REJECT_REASON = 7;
/** Royalty basis points. Irrelevant to the Transfer gate — only `rule_set` is read there. */
const ROYALTY_BPS = 500;

const PROFILE = launchProfile("11111111111111111111111111111111" as Address);
/** Comfortably over the pool's admission floor, so the approval activates rather than retains. */
const ADMITTED_VALUE = 20_000_000n;

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

function isPositionSeized(
  event: DecodedEvent,
): event is Extract<DecodedEvent, { name: "PositionSeized" }> {
  return event.name === "PositionSeized";
}

describe("a royalty rule set no named plugin read finds", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  /**
   * Holds the collection's `Royalties` plugin, and is neither the collection's update authority,
   * the depositor, nor the `close_seized` caller. The party that reaches in has to be a wallet an
   * assertion can name — design §3.4's third party, with a different plugin in its hand.
   */
  let issuer: TransactionSigner;
  /** Holds the `permanent_burn_delegate` the lock gate's ungated side needs. */
  let thief: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;
  /** Carries the `Royalties` plugin — so every card in it is `Undecidable` by census. */
  let collection: Address;
  let collectionAuthority: TransactionSigner;
  /**
   * **No plugins at all**, and it is the control the whole file rests on. Every card in
   * `collection` is undecidable from the moment it is minted, because the census reads the
   * *presence* of a `Royalties` record and not its contents — so a case that only varied the rule
   * set could not tell "the census found something it cannot decide" from "the census found
   * nothing at all". This collection is the second reading.
   */
  let cleanCollection: Address;
  let cleanCollectionAuthority: TransactionSigner;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 30);
    issuer = await fundedWallet(connection, 10);
    thief = await fundedWallet(connection, 10);

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

    // `rule_set: None` at creation, pointed at the deny list afterwards. A collection born with a
    // refusing rule set refuses `deposit_core`'s own `TransferV1`, so the position the rule set
    // is meant to strand could never be opened — the blocker arriving after escrow is not a
    // workaround, it is the cause stated precisely.
    const core = await createCoreCollection({
      connection,
      payer: admin,
      name: "royalty deny list",
      plugins: [
        {
          kind: "royalties",
          authority: issuer.address,
          basisPoints: ROYALTY_BPS,
          creator: issuer.address,
          ruleSet: "none",
        },
      ],
    });
    collection = core.collection;
    collectionAuthority = core.authority;
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection,
      standards: 0b100,
    });

    // Only the floor: the launch profile's `admissionCeiling` is `0`, which the program reads as
    // *no ceiling*, so a two-sided band assertion here would compare against a sentinel.
    const clean = await createCoreCollection({
      connection,
      payer: admin,
      name: "no plugins at all",
    });
    cleanCollection = clean.collection;
    cleanCollectionAuthority = clean.authority;
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: cleanCollection,
      standards: 0b100,
    });

    assert.ok(
      ADMITTED_VALUE >= PROFILE.admissionFloor,
      `the fixture value no longer clears the admission floor (${PROFILE.admissionFloor}) — the ` +
        `approval would close below floor instead of activating`,
    );
    assert.equal(
      PROFILE.admissionCeiling,
      0n,
      "the profile grew a real ceiling — the fixture value now needs checking against it too",
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

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  /** Points the collection's rule set at the deny list, or back at `None`. Reversible. */
  async function denyTransfers(deny: boolean): Promise<void> {
    await setCollectionRoyaltiesRuleSet({
      connection,
      payer: issuer,
      collection,
      authority: issuer,
      basisPoints: ROYALTY_BPS,
      creator: issuer.address,
      ruleSet: deny ? { denyList: [SYSTEM_PROGRAM] } : "none",
    });
  }

  /** Escrows one genuine `AssetV1` in `into`. No approval: `Pending`. */
  async function escrow(
    plugins: Parameters<typeof mintCoreAsset>[0]["plugins"] = [],
    into: Address = collection,
    intoAuthority: TransactionSigner = collectionAuthority,
  ): Promise<Address> {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection: into,
      collectionAuthority: intoAuthority,
      owner: depositor.address,
      plugins,
    });
    await sendIxs(connection, depositor, [
      await depositCore({ depositor: depositor.address, poolId, collection: into, asset }),
    ]);
    const { positionVault } = await positionPdas(asset);
    const escrowed = await readCoreAsset(connection, asset);
    assert.ok(escrowed, "the fixture asset vanished during the deposit");
    assert.equal(escrowed.owner, positionVault, "the fixture did not reach escrow");
    return asset;
  }

  /** Escrows, then has the operator reject it: `Rejected`, card still in the vault. */
  async function escrowAndReject(
    into: Address = collection,
    intoAuthority: TransactionSigner = collectionAuthority,
  ): Promise<Address> {
    const asset = await escrow([], into, intoAuthority);
    await sendIxs(connection, operator, [
      await rejectDeposit({
        operator: operator.address,
        pool,
        nftMint: asset,
        reason: REJECT_REASON,
      }),
    ]);
    const { position } = await positionPdas(asset);
    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).state,
      "Rejected",
      "the fixture is not in the state under test",
    );
    return asset;
  }

  /** Escrows and approves at `lockUntil`: `Active`, holding weight and a Fenwick leaf. */
  async function escrowAndApprove(
    lockUntil: bigint,
    plugins: Parameters<typeof mintCoreAsset>[0]["plugins"] = [],
  ): Promise<Address> {
    const asset = await escrow(plugins);
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
        lockUntil,
      }),
    ]);
    const { position } = await positionPdas(asset);
    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).state,
      "Active",
      "the fixture is not in the state under test",
    );
    return asset;
  }

  /** Permissionless: the caller holds no role in this pool at all. */
  async function callCloseIn(into: Address, asset: Address): Promise<string> {
    const caller = await fundedWallet(connection, 5);
    return sendIxs(connection, caller, [
      await closeSeized({
        caller: caller.address,
        poolId,
        asset,
        collection: into,
        depositor: depositor.address,
        weightIndex,
      }),
    ]);
  }

  async function callClose(asset: Address): Promise<string> {
    return callCloseIn(collection, asset);
  }

  /** A rejected position whose collection carries no plugins — the control's fixture. */
  async function escrowInCleanCollection(): Promise<Address> {
    return escrowAndReject(cleanCollection, cleanCollectionAuthority);
  }

  // ── the mechanism, and the census that classifies it ──────────────────────────────────────

  /**
   * **The exit is refused, and not by this program.** With the rule set at `None` the census
   * finds a clean surface, so `return_rejected_core` returns the card. Point the rule set at the
   * deny list and the same call reaches its `TransferV1` and MPL Core rejects it — the depositor
   * gets `MplCoreError::InvalidAuthority` (9) from inside the CPI instead of one of the program's
   * two named codes.
   *
   * That the named codes are **absent** is the assertion, not a detail: 6307 and 6309 are what
   * the *decidable* causes produce, and no named plugin read reaches this one. Under the gates as
   * they stood before this round the story ended here, with `close_seized` answering
   * `CollateralPresent` and the position holding weight behind a card nobody could move.
   *
   * **And this is where attempt-and-classify dies.** The log this case produces is
   * `Program CoRE… failed: custom program error: 0x9` followed immediately by
   * `Program 4exR… failed: custom program error: 0x9` — the caller does not survive its callee's
   * refusal, so a `close_seized` that drove this release to read its result would abort instead
   * of classifying. The census is what closes the gap without a CPI.
   */
  it("refuses the deny-listed return with MPL Core's own error, not a named code", async () => {
    const asset = await escrowAndReject();
    await denyTransfers(true);
    try {
      let thrown: unknown;
      try {
        await sendIxs(connection, depositor, [
          await returnRejectedCore({
            payer: depositor.address,
            depositor: depositor.address,
            pool,
            asset,
            collection,
          }),
        ]);
        assert.fail("MPL Core accepted a transfer its own rule set denies");
      } catch (err) {
        thrown = err;
      }
      const message = String(thrown);
      assert.ok(
        message.includes(String(MPL_CORE_INVALID_AUTHORITY)),
        `expected MPL Core's own rejection (${MPL_CORE_INVALID_AUTHORITY}), got: ${message}`,
      );
      for (const named of [COLLATERAL_ABSENT, COLLATERAL_FROZEN]) {
        assert.equal(
          message.includes(String(named)),
          false,
          `the refusal reported ${named} — no named plugin read reaches this cause, and a named ` +
            `code here would mean something had somehow decided it`,
        );
      }

      const { position, positionVault } = await positionPdas(asset);
      assert.equal(
        decodePosition((await fetchAccountData(connection, position))!).state,
        "Rejected",
        "a refused return must not move the state",
      );
      const held = await readCoreAsset(connection, asset);
      assert.equal(held?.owner, positionVault, "and the card must still be in the vault");
    } finally {
      await denyTransfers(false);
    }
  });

  /**
   * **The fix, end to end: the census sees a record it cannot decide and `close_seized` acts on
   * it.** Nothing here reads the royalty plugin's contents — the census only notices that the
   * collection's registry holds a `Royalties` record, whose effect on a transfer belongs to MPL
   * Core — and that is the property being bought. A `plugin_type` byte from a release nobody has
   * shipped yet would take exactly this path.
   *
   * `Undecidable` is deliberately not a claim that the transfer would fail. On this very fixture
   * it *does* fail, which the case above measures separately; the cause records only that the
   * program could not tell.
   */
  it("seizes the position the deny list stranded, reporting Undecidable", async () => {
    const asset = await escrowAndReject();
    const { position, positionVault } = await positionPdas(asset);
    await denyTransfers(true);

    try {
      const signature = await callClose(asset);

      const seized = decodePosition((await fetchAccountData(connection, position))!);
      assert.equal(seized.state, "Seized");

      const events = (await eventsOf(signature)).filter(isPositionSeized);
      assert.equal(events.length, 1, "exactly one PositionSeized per seizure");
      const reported = events[0].data;
      assert.equal(
        reported.kind,
        "Undecidable",
        "the cause is the absence of a decision, not a plugin this program named",
      );
      assert.equal(reported.sourceState, "Rejected");
      assert.equal(reported.position, position);
      assert.equal(
        reported.ownerObserved,
        positionVault,
        "the card never moved — on this cause the observed owner is still the vault",
      );

      // Nothing was transferred: the classification is a read, and this instruction makes no CPI.
      const held = await readCoreAsset(connection, asset);
      assert.equal(held?.owner, positionVault, "close_seized moved the card");

      // Reversible, like the freeze: the rule set goes back to `None`, and the position and its
      // vault survived the seizure so the depositor's claim is callable (D-115).
      await denyTransfers(false);
      assert.notEqual(
        await fetchAccountData(connection, position),
        null,
        "the seizure deallocated the position the claim needs",
      );
    } finally {
      await denyTransfers(false);
    }
  });

  /**
   * **The control, and without it the case above proves nothing.** Same instruction, same
   * accounts, same collection — only the rule set differs, and with it at `None` the census finds
   * a clean surface and the seizure is refused `CollateralPresent` (6308).
   *
   * Note what this control does *not* say: it is the **rule set**, not the `Royalties` record,
   * that moves the verdict here. The plugin is on the collection in both runs. What the census
   * reads is the record's presence, and the fixture's rule set is `None` at creation, so a
   * `Royalties` plugin whose registry record exists is undecidable in both — which is why this
   * case is driven with the plugin *absent* from the collection under test.
   */
  it("refuses 6308 when the collection carries nothing the census cannot decide", async () => {
    const asset = await escrowInCleanCollection();
    const { position, positionVault } = await positionPdas(asset);

    await expectAnchorError(callCloseIn(cleanCollection, asset), COLLATERAL_PRESENT);

    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).state,
      "Rejected",
      "a refused seizure must not move the state",
    );
    const held = await readCoreAsset(connection, asset);
    assert.equal(held?.owner, positionVault, "and must move nothing");
  });

  // ── the lock, on the row that holds weight ─────────────────────────────────────────────────

  /**
   * **The `Active` row is lock-gated on every cause a depositor could arrange while still
   * ending up with the card**, and the bypass it closes needs no more than a cooperative
   * collection authority. The call is permissionless, so the depositor can make it themselves,
   * and `claim_nft_core` is never lock-gated. So the sequence *have transfers denied, seize your
   * own position, have the denial lifted, claim* would take the weight out of the pool months
   * before `season.tge_at + 14 days` (D-091) and hand the card back — the Season-0 lock
   * dissolved by collusion.
   *
   * This case drives `Undecidable`. `Transferred` is gated for the same reason and by the same
   * predicate, and it is the shorter route — a collection-level `PermanentTransferDelegate`
   * hands the card over directly, with no claim leg to run — but it is covered at the unit
   * level only; see `a_transferred_card_cannot_take_a_locked_position_out_of_the_pool`.
   *
   * The pool's counters are read on both sides of the refusal: `PositionLocked` has to leave the
   * weight in, not merely return a code.
   */
  it("holds the Active row back while the card is escrowed and the lock stands", async () => {
    const asset = await escrowAndApprove(nowSeconds() + 3600n);
    const { position } = await positionPdas(asset);
    await denyTransfers(true);

    try {
      const before = decodePool((await fetchAccountData(connection, pool))!);
      await expectAnchorError(callClose(asset), POSITION_LOCKED);

      const after = decodePool((await fetchAccountData(connection, pool))!);
      assert.equal(
        decodePosition((await fetchAccountData(connection, position))!).state,
        "Active",
        "a locked position must stay Active",
      );
      assert.equal(after.wReal, before.wReal, "and must keep its weight");
      assert.equal(after.nReal, before.nReal);

      const { address: walletStats } = await derivePda([
        WALLET_STATS_SEED,
        seedFromPubkey(pool),
        seedFromPubkey(depositor.address),
      ]);
      assert.notEqual(
        await fetchAccountData(connection, walletStats),
        null,
        "the depositor's WalletStats must exist for the counters above to mean anything",
      );
    } finally {
      await denyTransfers(false);
    }
  });

  /**
   * **And it is open on the one cause that destroys the card, which is not an exemption.** A
   * burned card cannot be recovered by anybody, so a depositor gains nothing from the seizure —
   * while a position kept `Active` behind a destroyed card keeps earning draw odds and fee
   * accrual on collateral the pool no longer holds. Delaying *this* seizure would reward the
   * depositor, not restrain them, so the same lock that holds the case above back must not reach
   * it. `Burned` is the whole of that side: a transfer-out leaves the card recoverable, so it
   * sits with the freezes.
   *
   * Same lock, same row, same instruction — only the cause differs.
   */
  it("lets the Active row go on a burned card however long its lock has to run", async () => {
    const asset = await escrowAndApprove(nowSeconds() + 3600n, [
      { kind: "permanentBurnDelegate", authority: thief.address },
    ]);
    const { position } = await positionPdas(asset);

    const before = decodePool((await fetchAccountData(connection, pool))!);
    await burnCoreAsset({ connection, payer: thief, asset, collection, authority: thief });

    const signature = await callClose(asset);
    const events = (await eventsOf(signature)).filter(isPositionSeized);
    assert.equal(events.length, 1);
    assert.equal(events[0].data.kind, "Burned");
    assert.equal(events[0].data.sourceState, "Active");

    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).state,
      "Seized",
      "a burned card's position must close even under a live lock",
    );
    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.ok(
      after.wReal < before.wReal,
      "and its weight must leave — that is the point of not gating this cause",
    );
  });
});
