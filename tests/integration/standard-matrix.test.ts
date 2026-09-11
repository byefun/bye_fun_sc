// T-110 — AC-82's three-standard matrix and AC-36's rejection reasons, in one pool.
//
// **The three standards have each round-tripped before, and never in the same pool.** T-48 runs
// pNFTs, T-103 runs legacy `V1_NFT`s, T-109 runs MPL Core, each in a file that builds its own
// pool and admits its own collection — so what has been shown is three independent programs'
// worth of behaviour, not one pool that admits three standards at once. AC-82 asks for the
// second thing: `ProgrammableNFT` and `V1_NFT` from the Token Metadata collection, `MplCoreAsset`
// from the Core collection, **each depositing and withdrawing**, with the wrong-collection
// pairings failing beside them. That is a property of a shared `PoolCollection` set and a shared
// `WeightIndex`, and it is invisible to three pools that never met.
//
// **The success paths are here because the criterion is not satisfiable without them.** A
// negative-only matrix passes an operation that never completes: every rejection below would
// still red-free if `deposit` refused all three standards outright. Each of the three therefore
// asserts both sides of the custody move on the way in and on the way out — a `Position` that
// exists proves the handler ran, never that anything moved.
//
// **What AC-36 asks for that this crate does not express, stated rather than quietly skipped.**
// The criterion is written against the DAS `interface` allowlist and a `Type == Card` trait, and
// neither is an on-chain concept: there is no trait check anywhere in `programs/bye_machine`, and
// `MplBubblegumV2` has no account this program could be handed at all — a compressed asset is a
// merkle leaf with no mint. What the *crate* enforces in their place is the presenting account's
// **owner program**, and that is what the last two rejections below drive. The DAS half and the
// `Type == Card` half belong to the backend's admission surface and are outside this repository.
//
// **Four of these rejections had never executed on chain, and one of them is a finding.** T-25's
// row claims each of T-47's five rejection fixtures "rejects with its exact code"; three of them
// — `mintUnverifiedCollectionPnft`, `mintWrongCollectionPnft`, `mintWrongMasterEditionPnft` —
// were minted and inspected by `fixtures.test.ts` and **never deposited**, so the claim was
// pinned in Rust and nowhere else. Depositing them shows what a client actually receives: three
// distinct AC-36 reasons — *not a verified member*, *verified into another collection*, and
// *the record does not belong to the collection the asset declares* — all arrive as the **same**
// `CollectionNotAdmitted` (6100). Only the standards-bit reason is separable, at 6108. AC-36
// requires the rejection to name the check it failed; on this path the code names the family.

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
  createPnftCollection,
  mintAdmissiblePnft,
  mintMplCoreLikeAsset,
  mintPlainNonFungiblePnft,
  mintUnverifiedCollectionPnft,
  mintWrongCollectionPnft,
  mintWrongMasterEditionPnft,
  tokenRecordPda,
  RULE_SET_ADDRESS,
  type MintedAsset,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { createCoreCollection, mintCoreAsset, readCoreAsset } from "../helpers/core-fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { approveDeposit, deposit, depositCore } from "../../scripts/lib/deposit.js";
import { withdraw, withdrawCore } from "../../scripts/lib/exit.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { decodePosition } from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { AUTH_RULES_PROGRAM, SYSVAR_INSTRUCTIONS } from "../../scripts/constants.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";

/** The asset does not belong to the collection whose record was presented — AC-36's first reason
 * family, and the one three separate checks collapse onto. */
const COLLECTION_NOT_ADMITTED = 6100;
/** The presented account is not something this program admits at all — the crate's stand-in for
 * AC-36's `interface` allowlist. */
const STANDARD_NOT_ADMITTED = 6101;
/** The value is under the pool's admission floor. */
const BELOW_ADMISSION_FLOOR = 6104;
/** The collection is admitted, but not for the standard the asset resolved to. */
const STANDARD_NOT_ADMITTED_FOR_COLLECTION = 6108;

const PROFILE = launchProfile("11111111111111111111111111111111" as Address);
const ADMISSION_FLOOR = PROFILE.admissionFloor;
const TICKET_TARGET = PROFILE.ticketTarget;
const ADMITTED_VALUE = 20_000_000n;
const BELOW_FLOOR_VALUE = 1_000_000n;

/** `Position.standard`'s discriminants, verbatim against `common/standards.rs`. */
const STANDARD_PNFT = 0;
const STANDARD_LEGACY = 1;
const STANDARD_CORE = 2;

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

describe("T-110 — the three admitted standards in one pool", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;

  /** Admitted `0b011` — both Token Metadata standards, as D-112 admits `CCryptWBY…N2Yf`. */
  let tmCollection: PnftCollection;
  /** Admitted `0b100` — MPL Core only, as D-112 admits `CCryptUfe…9CRac`. */
  let coreCollection: Address;
  let coreAuthority: TransactionSigner;
  /**
   * A Token Metadata collection admitted for **MPL Core alone**. It exists for one case: a pNFT
   * whose own declared collection has a record that does not carry bit 0. Every wrong-pairing
   * case in the suite until now ran on standards 1 and 2, so 6108 had never been reached from
   * standard 0 — and the pairing AC-82 names is per-standard, not per-collection.
   */
  let coreOnlyTmCollection: PnftCollection;
  /** Never admitted anywhere. The collection `mintWrongCollectionPnft` verifies against. */
  let strangerCollection: PnftCollection;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 30);

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

    tmCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "T-110 TM",
    });
    coreOnlyTmCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "T-110 TM core-only",
    });
    strangerCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "T-110 stranger",
    });
    const core = await createCoreCollection({ connection, payer: admin, name: "T-110 Core" });
    coreCollection = core.collection;
    coreAuthority = core.authority;

    // One pool, three records, three different masks — which is the whole of what this file adds
    // over the three single-standard files it does not repeat.
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: tmCollection.mint,
      standards: 0b011,
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: coreCollection,
      standards: 0b100,
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: coreOnlyTmCollection.mint,
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

  async function positionPdas(mint: Address) {
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(mint),
    ]);
    const { address: positionVault } = await derivePda([
      POSITION_VAULT_SEED,
      seedFromPubkey(position),
    ]);
    return { position, positionVault };
  }

  async function approve(mint: Address, value: bigint): Promise<void> {
    await sendIxs(connection, operator, [
      await approveDeposit({
        operator: operator.address,
        poolId,
        nftMint: mint,
        depositor: depositor.address,
        weightIndex,
        displaced: null,
        value,
        observedAt: nowSeconds(),
        lockUntil: 0n,
      }),
    ]);
  }

  // ── standard 0 — ProgrammableNFT ─────────────────────────────────────────────────────────

  async function pnftFrom(collection: PnftCollection, label: string) {
    return mintAdmissiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection,
      value: ADMITTED_VALUE,
      label,
    });
  }

  /** The five pNFT accounts, in the one shape standard 0 requires them present. */
  async function pnftAccounts(asset: MintedAsset, positionVault: Address) {
    return {
      depositorTokenRecord: await tokenRecordPda(asset.mint, asset.tokenAccount),
      positionTokenRecord: await tokenRecordPda(asset.mint, positionVault),
      sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: AUTH_RULES_PROGRAM,
      // Every pNFT this file mints takes `mintAdmissiblePnft`'s default rule set, and the fixture
      // type does not carry it back — so it is named once here rather than threaded through five
      // call sites that would each be free to name a different one.
      authorizationRules: RULE_SET_ADDRESS,
    };
  }

  async function depositPnft(asset: MintedAsset, collection: Address): Promise<void> {
    const { positionVault } = await positionPdas(asset.mint);
    const accounts = await pnftAccounts(asset, positionVault);
    await sendIxs(connection, depositor, [
      await deposit({
        depositor: depositor.address,
        poolId,
        collection,
        nftMint: asset.mint,
        depositorToken: asset.tokenAccount,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        ...accounts,
      }),
    ]);
  }

  // ── standard 1 — legacy V1_NFT ───────────────────────────────────────────────────────────

  async function depositLegacy(asset: MintedAsset, collection: Address): Promise<void> {
    await sendIxs(connection, depositor, [
      await deposit({
        depositor: depositor.address,
        poolId,
        collection,
        nftMint: asset.mint,
        depositorToken: asset.tokenAccount,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        // Required-absent on this standard (design §4.2) — a supplied account here is 2003.
        depositorTokenRecord: null,
        positionTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
      }),
    ]);
  }

  /** Both sides of an SPL custody move: the vault holds it and the depositor does not. */
  async function assertEscrowedSpl(asset: MintedAsset): Promise<void> {
    const { positionVault } = await positionPdas(asset.mint);
    assert.equal(
      (await connection.getTokenAccountBalance({ tokenAccount: positionVault })).amount,
      1n,
      "the card did not reach escrow",
    );
    assert.equal(
      (await connection.getTokenAccountBalance({ tokenAccount: asset.tokenAccount })).amount,
      0n,
      "the depositor still holds a card that is supposedly escrowed",
    );
  }

  async function assertReleasedSpl(asset: MintedAsset): Promise<void> {
    assert.equal(
      (await connection.getTokenAccountBalance({ tokenAccount: asset.tokenAccount })).amount,
      1n,
      "the card did not return to its depositor",
    );
    const { position, positionVault } = await positionPdas(asset.mint);
    assert.equal(await fetchAccountData(connection, position), null, "the position is still open");
    assert.equal(
      await fetchAccountData(connection, positionVault),
      null,
      "the vault survived the release",
    );
  }

  it("a ProgrammableNFT deposits from the Token Metadata collection and withdraws", async () => {
    const asset = await pnftFrom(tmCollection, "mx0");
    await depositPnft(asset, tmCollection.mint);
    await assertEscrowedSpl(asset);

    const { position, positionVault } = await positionPdas(asset.mint);
    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).standard,
      STANDARD_PNFT,
      "the intake resolved the wrong standard",
    );

    await approve(asset.mint, ADMITTED_VALUE);
    const accounts = await pnftAccounts(asset, positionVault);
    await sendIxs(connection, depositor, [
      await withdraw({
        depositor: depositor.address,
        poolId,
        nftMint: asset.mint,
        weightIndex,
        depositorUsdc: null,
        depositorToken: asset.tokenAccount,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        positionTokenRecord: accounts.positionTokenRecord,
        depositorTokenRecord: accounts.depositorTokenRecord,
        sysvarInstructions: accounts.sysvarInstructions,
        authorizationRulesProgram: accounts.authorizationRulesProgram,
        authorizationRules: accounts.authorizationRules,
        successor: null,
      }),
    ]);
    await assertReleasedSpl(asset);
  });

  it("a legacy V1_NFT deposits from the same collection and withdraws", async () => {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection: tmCollection,
    });
    await depositLegacy(asset, tmCollection.mint);
    await assertEscrowedSpl(asset);

    const { position } = await positionPdas(asset.mint);
    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).standard,
      STANDARD_LEGACY,
      "the intake resolved the wrong standard",
    );

    await approve(asset.mint, ADMITTED_VALUE);
    await sendIxs(connection, depositor, [
      await withdraw({
        depositor: depositor.address,
        poolId,
        nftMint: asset.mint,
        weightIndex,
        depositorUsdc: null,
        depositorToken: asset.tokenAccount,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        positionTokenRecord: null,
        depositorTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
        successor: null,
      }),
    ]);
    await assertReleasedSpl(asset);
  });

  it("an MplCoreAsset deposits from the Core collection and withdraws", async () => {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection: coreCollection,
      collectionAuthority: coreAuthority,
      owner: depositor.address,
    });
    await sendIxs(connection, depositor, [
      await depositCore({
        depositor: depositor.address,
        poolId,
        collection: coreCollection,
        asset,
      }),
    ]);

    const { position, positionVault } = await positionPdas(asset);
    const escrowed = await readCoreAsset(connection, asset);
    assert.ok(escrowed, "the fixture asset vanished during the deposit");
    assert.equal(escrowed.owner, positionVault, "the card did not reach escrow");
    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).standard,
      STANDARD_CORE,
      "the intake resolved the wrong standard",
    );

    await approve(asset, ADMITTED_VALUE);
    await sendIxs(connection, depositor, [
      await withdrawCore({
        depositor: depositor.address,
        poolId,
        asset,
        collection: coreCollection,
        weightIndex,
        depositorUsdc: null,
        successor: null,
      }),
    ]);

    const released = await readCoreAsset(connection, asset);
    assert.ok(released, "the asset account must survive its own release");
    assert.equal(released.owner, depositor.address, "the card did not return to its depositor");
    assert.equal(await fetchAccountData(connection, position), null, "the position is still open");
    // The accountless escrow, asserted on the way out: a lazily allocated vault would leave every
    // Core position paying rent the design says it does not.
    assert.equal(
      await fetchAccountData(connection, positionVault),
      null,
      "the vault was allocated somewhere in the exit",
    );
  });

  // ── AC-82's wrong-collection pairings ────────────────────────────────────────────────────

  it("a pNFT whose own collection is admitted for MPL Core alone is refused on the standards bit", async () => {
    const asset = await pnftFrom(coreOnlyTmCollection, "mxbit");
    await expectAnchorError(
      depositPnft(asset, coreOnlyTmCollection.mint),
      STANDARD_NOT_ADMITTED_FOR_COLLECTION,
    );
  });

  it("a pNFT presented against the Core collection's record is refused on the derivation", async () => {
    const asset = await pnftFrom(tmCollection, "mxx");
    // The record is real, self-consistent, and admitted — for a collection this asset does not
    // declare. `require_admits` re-derives the record address from what the *asset* says, never
    // from the record's own field, which is the substitution T-101 proved a suite can pass while
    // getting wrong.
    await expectAnchorError(depositPnft(asset, coreCollection), COLLECTION_NOT_ADMITTED);
  });

  // ── AC-36's rejection reasons, on the Token Metadata path ────────────────────────────────

  it("a pNFT whose collection membership was never verified is refused", async () => {
    const asset = await mintUnverifiedCollectionPnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection: tmCollection,
    });
    // The metadata names the right collection; `verified` is false. AC-36 makes the verified flag
    // the signal and rules `creators[].verified` out as one, and this is that clause on chain.
    await expectAnchorError(depositPnft(asset, tmCollection.mint), COLLECTION_NOT_ADMITTED);
  });

  it("a pNFT verified into a different collection is refused", async () => {
    const asset = await mintWrongCollectionPnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      wrongCollection: strangerCollection,
    });
    await expectAnchorError(depositPnft(asset, tmCollection.mint), COLLECTION_NOT_ADMITTED);
  });

  it("a pNFT supplying another mint's master edition is refused", async () => {
    const asset = await mintWrongMasterEditionPnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection: tmCollection,
    });
    // The supplied account is a real, Token-Metadata-owned `MasterEditionV2` — it is simply not
    // this mint's. The rejection therefore comes from the PDA derivation and not from a missing
    // or malformed account, which is the distinction `fixtures.test.ts` asserts and never drove.
    await expectAnchorError(depositPnft(asset, tmCollection.mint), STANDARD_NOT_ADMITTED);
  });

  it("an asset whose metadata is owned by a program outside Token Metadata is refused", async () => {
    const asset = await mintMplCoreLikeAsset({
      connection,
      payer: admin,
      owner: depositor.address,
    });
    // The crate's expression of AC-36's `interface` allowlist: admission keys on the owner
    // program of the accounts presented, so anything outside the Token Metadata world is refused
    // before a token standard is ever resolved. `MplBubblegumV2` cannot be driven here at all —
    // a compressed asset is a merkle leaf with no mint account to present.
    await expectAnchorError(depositLegacy(asset, tmCollection.mint), STANDARD_NOT_ADMITTED);
  });

  // ── AC-82's admission floor, on all three standards ──────────────────────────────────────
  //
  // "exercised on all three standards, not on pNFTs alone" — and until this file it was
  // exercised on none of them: `BelowAdmissionFloor` (6104) had never been returned to a client
  // in any integration test. The floor lives in `approve_deposit`, not in admission, so each of
  // these escrows successfully first and is refused at the value gate. The position is left
  // `Pending`, which is exactly what a refused approval must leave behind.

  it("a pNFT below the admission floor is refused at approval", async () => {
    const asset = await pnftFrom(tmCollection, "mxfl");
    await depositPnft(asset, tmCollection.mint);
    await expectAnchorError(approve(asset.mint, BELOW_FLOOR_VALUE), BELOW_ADMISSION_FLOOR);
    const { position } = await positionPdas(asset.mint);
    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).state,
      "Pending",
      "a refused approval must leave the position pending",
    );
  });

  it("a legacy V1_NFT below the admission floor is refused at approval", async () => {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection: tmCollection,
    });
    await depositLegacy(asset, tmCollection.mint);
    await expectAnchorError(approve(asset.mint, BELOW_FLOOR_VALUE), BELOW_ADMISSION_FLOOR);
  });

  it("an MplCoreAsset below the admission floor is refused at approval", async () => {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection: coreCollection,
      collectionAuthority: coreAuthority,
      owner: depositor.address,
    });
    await sendIxs(connection, depositor, [
      await depositCore({
        depositor: depositor.address,
        poolId,
        collection: coreCollection,
        asset,
      }),
    ]);
    await expectAnchorError(approve(asset, BELOW_FLOOR_VALUE), BELOW_ADMISSION_FLOOR);
  });
});
