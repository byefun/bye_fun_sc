// The T02 authority's claim, withdraw and deposit against a live validator.
//
// `ProtocolConfig.t02_authority` was stored and rotatable but read by nothing, so a position the
// Treasury 02 identity owns could never claim its fee or leave, a closed one kept its card for good,
// and the only way a card entered treasury custody was `instant_sell`, which pays out of Treasury
// 03. These seven instructions close those gaps, and what only a live validator proves is the
// surface this file exercises: the signer binding, the custody legs on both token families, the
// position's depositor staying the Treasury 02 identity, a deposit that moves no USDC and then
// follows a depositor's approval path, and every card leaving escrow for the T02 authority's wallet
// and nowhere else.
//
// The fee payout itself is not exercised here: no roll settles in this suite, so a position's
// accrued balance never leaves zero. The arithmetic is `apply_claim_fee`'s and `apply_withdraw`'s,
// shared with the depositor instructions and covered by their own Rust unit tests; what this file
// adds for the fee is that the destination is Treasury 03 by construction, which the IDL oracle
// pins as an account list with no caller-chosen payee.
//
// Every refusal is paired with a control that runs the same call with the right signer and
// requires it to succeed. A row that only refuses cannot see a missing authority check.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  loadT02Authority,
  withSigner,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import {
  createPnftCollection,
  mintPlainNonFungiblePnft,
  type MintedAsset,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { createCoreCollection, mintCoreAsset, readCoreAsset } from "../helpers/core-fixtures.js";
import {
  EXECUTED_ROWS,
  NOT_A_TREASURY_POSITION,
  NOT_POSITION_OWNER,
  UNAUTHORIZED,
} from "../helpers/authority-matrix.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import {
  approveDeposit,
  deposit,
  rejectDeposit,
  returnRejected,
  returnRejectedCore,
} from "../../scripts/lib/deposit.js";
import { beginSweep, endSweep, recordValue } from "../../scripts/lib/value.js";
import {
  t02ClaimFee,
  t02ClaimNft,
  t02ClaimNftCore,
  t02Deposit,
  t02DepositCore,
  t02Withdraw,
  t02WithdrawCore,
} from "../../scripts/lib/t02.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { decodePosition, decodeWalletStats } from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const TREASURY_02_SEED = "t02";
const WALLET_STATS_SEED = "wallet";

const VALUE = 20_000_000n;
const BELOW_FLOOR_VALUE = 1_000_000n;
const STANDARD_LEGACY = 1;
const STANDARD_CORE = 2;

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

describe("the T02 authority's instructions against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let t02Authority: TransactionSigner;
  let stranger: TransactionSigner;
  let depositor: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;
  let treasury03: Address;
  let t02Identity: Address;
  let pnftCollection: PnftCollection;
  let coreCollection: Address;
  let coreCollectionAuthority: TransactionSigner;

  /** Which matrix cells this file is answerable for, asserted against what actually ran. */
  const covered = new Set<string>();

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    t02Authority = await loadT02Authority();
    stranger = await fundedWallet(connection, 20);
    depositor = await fundedWallet(connection, 20);

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
      args: { ...launchProfile() },
    });
    pool = poolAccounts.pool;
    treasury03 = poolAccounts.treasury03;
    ({ address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]));

    pnftCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "T02 authority",
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: pnftCollection.mint,
      standards: 0b010,
    });

    const core = await createCoreCollection({ connection, payer: admin, name: "T02 core" });
    coreCollection = core.collection;
    coreCollectionAuthority = core.authority;
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: coreCollection,
      standards: 0b100,
    });
  });

  async function positionOf(asset: Address) {
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

  /** SPL token account layout: mint(32) ‖ owner(32) ‖ amount(u64 le). */
  async function tokenAmount(account: Address): Promise<bigint> {
    const raw = await fetchAccountData(connection, account);
    assert.ok(raw, `token account ${account} does not exist`);
    return raw.readBigUInt64LE(64);
  }

  async function treasury03Balance(): Promise<bigint> {
    return tokenAmount(treasury03);
  }

  async function decodePositionOf(asset: Address) {
    const { position } = await positionOf(asset);
    return decodePosition((await fetchAccountData(connection, position))!);
  }

  /** The Operator's attestation, exactly as for a depositor's deposit but with Treasury 02 as the
   * rollup owner. */
  async function approve(asset: Address) {
    await sendIxs(connection, operator, [
      await approveDeposit({
        operator: operator.address,
        poolId,
        nftMint: asset,
        depositor: t02Identity,
        weightIndex,
        displaced: null,
        value: VALUE,
        observedAt: nowSeconds(),
        lockUntil: 0n,
      }),
    ]);
  }

  /** One sweep at a below-floor value closes the position and leaves the card escrowed. */
  async function closeBelowFloor(asset: Address) {
    await sendIxs(connection, operator, [await beginSweep({ operator: operator.address, poolId })]);
    await sendIxs(connection, operator, [
      await recordValue({
        operator: operator.address,
        poolId,
        nftMint: asset,
        depositor: t02Identity,
        weightIndex,
        value: BELOW_FLOOR_VALUE,
        observedAt: nowSeconds(),
        effect: { kind: "close", successor: null },
      }),
    ]);
    await sendIxs(connection, operator, [await endSweep({ operator: operator.address, poolId })]);
  }

  describe("Token Metadata family", () => {
    async function mintForAuthority(): Promise<MintedAsset> {
      return mintPlainNonFungiblePnft({
        connection,
        payer: admin,
        authority: admin,
        owner: t02Authority.address,
        collection: pnftCollection,
      });
    }

    async function depositAs(asset: MintedAsset, signerWallet: TransactionSigner) {
      return sendIxs(connection, signerWallet, [
        await t02Deposit({
          t02Authority: signerWallet.address,
          poolId,
          collection: pnftCollection.mint,
          nftMint: asset.mint,
          authorityToken: asset.tokenAccount,
          metadata: asset.metadata,
          masterEdition: asset.masterEdition,
          authorityTokenRecord: null,
          positionTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
        }),
      ]);
    }

    async function withdrawAs(asset: MintedAsset, signerWallet: TransactionSigner) {
      return sendIxs(connection, signerWallet, [
        await t02Withdraw({
          t02Authority: signerWallet.address,
          poolId,
          nftMint: asset.mint,
          weightIndex,
          authorityToken: asset.tokenAccount,
          metadata: asset.metadata,
          masterEdition: asset.masterEdition,
          positionTokenRecord: null,
          authorityTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
          successor: null,
        }),
      ]);
    }

    async function claimNftAs(asset: MintedAsset, signerWallet: TransactionSigner) {
      return sendIxs(connection, signerWallet, [
        await t02ClaimNft({
          t02Authority: signerWallet.address,
          poolId,
          nftMint: asset.mint,
          authorityToken: asset.tokenAccount,
          metadata: asset.metadata,
          masterEdition: asset.masterEdition,
          positionTokenRecord: null,
          authorityTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
        }),
      ]);
    }

    let asset: MintedAsset;
    let t03Before: bigint;

    before(async () => {
      asset = await mintForAuthority();
      t03Before = await treasury03Balance();
    });

    it("t02_deposit: a wallet that is not the T02 authority is refused", async () => {
      await expectAnchorError(depositAs(asset, stranger), UNAUTHORIZED);
      covered.add("t02_deposit.t02_authority");
    });

    it("t02_deposit: the authority deposits as a depositor would — Pending, no value, no USDC moved", async () => {
      await depositAs(asset, t02Authority);

      const recorded = await decodePositionOf(asset.mint);
      assert.equal(recorded.depositor, t02Identity, "the depositor must be the Treasury 02 identity");
      assert.equal(recorded.state, "Pending");
      assert.equal(recorded.recordedValue, 0n, "no value is attested at intake");
      assert.equal(recorded.standard, STANDARD_LEGACY);
      assert.equal(await treasury03Balance(), t03Before, "a deposit moves no USDC");

      const { positionVault } = await positionOf(asset.mint);
      assert.equal(await tokenAmount(positionVault), 1n, "the card is in the position's escrow");

      const { address: stats } = await derivePda([
        WALLET_STATS_SEED,
        seedFromPubkey(pool),
        seedFromPubkey(t02Identity),
      ]);
      const rollup = decodeWalletStats((await fetchAccountData(connection, stats))!);
      assert.equal(rollup.owner, t02Identity, "the rollup is the Treasury 02 identity's");
    });

    it("approve_deposit: the Operator attests and activates a T02 deposit like any depositor's", async () => {
      await approve(asset.mint);

      const recorded = await decodePositionOf(asset.mint);
      assert.equal(recorded.state, "Active");
      assert.equal(recorded.depositor, t02Identity);
      assert.equal(recorded.recordedValue, VALUE);
      assert.equal(recorded.lockUntil, 0n);
    });

    it("t02_claim_fee: a wallet that is not the T02 authority is refused", async () => {
      const ix = await t02ClaimFee({ t02Authority: t02Authority.address, poolId, nftMint: asset.mint });
      await expectAnchorError(
        sendIxs(connection, stranger, [withSigner(ix, stranger.address)]),
        UNAUTHORIZED,
      );
      covered.add("t02_claim_fee.t02_authority");
    });

    it("t02_claim_fee: the T02 authority claims, the position is untouched, and it is repeatable", async () => {
      const before1 = await decodePositionOf(asset.mint);

      for (let call = 0; call < 2; call++) {
        await sendIxs(connection, t02Authority, [
          await t02ClaimFee({ t02Authority: t02Authority.address, poolId, nftMint: asset.mint }),
        ]);
      }

      const after = await decodePositionOf(asset.mint);
      assert.equal(after.state, "Active");
      assert.equal(after.inTier, before1.inTier, "a claim never changes tier membership");
      assert.equal(after.slotIndex, before1.slotIndex, "the Fenwick leaf is untouched");
      assert.equal(after.accrued, 0n);
      assert.equal(await treasury03Balance(), t03Before, "nothing was owed, so nothing moved");
    });

    it("t02_claim_fee: the T02 authority is refused on a position a depositor owns", async () => {
      const owned = await mintPlainNonFungiblePnft({
        connection,
        payer: admin,
        authority: admin,
        owner: depositor.address,
        collection: pnftCollection,
      });
      await sendIxs(connection, depositor, [
        await deposit({
          depositor: depositor.address,
          poolId,
          collection: pnftCollection.mint,
          nftMint: owned.mint,
          depositorToken: owned.tokenAccount,
          metadata: owned.metadata,
          masterEdition: owned.masterEdition,
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
          nftMint: owned.mint,
          depositor: depositor.address,
          weightIndex,
          displaced: null,
          value: VALUE,
          observedAt: nowSeconds(),
          lockUntil: 0n,
        }),
      ]);

      await expectAnchorError(
        sendIxs(connection, t02Authority, [
          await t02ClaimFee({ t02Authority: t02Authority.address, poolId, nftMint: owned.mint }),
        ]),
        NOT_A_TREASURY_POSITION,
      );
    });

    it("t02_withdraw: a wallet that is not the T02 authority is refused", async () => {
      await expectAnchorError(withdrawAs(asset, stranger), UNAUTHORIZED);
      covered.add("t02_withdraw.t02_authority");
    });

    it("t02_withdraw: the T02 authority takes the card back and the position closes", async () => {
      const { position, positionVault } = await positionOf(asset.mint);
      await withdrawAs(asset, t02Authority);

      assert.equal(await fetchAccountData(connection, position), null, "the position is still open");
      assert.equal(await fetchAccountData(connection, positionVault), null, "the vault is not closed");
      assert.equal(
        await tokenAmount(asset.tokenAccount),
        1n,
        "the card did not return to the T02 authority",
      );
      assert.equal(await treasury03Balance(), t03Before);
    });

    it("a rejected T02 deposit's card goes to the T02 authority and to no one else", async () => {
      const rejected = await mintForAuthority();
      await depositAs(rejected, t02Authority);
      await sendIxs(connection, operator, [
        await rejectDeposit({ operator: operator.address, pool, nftMint: rejected.mint, reason: 1 }),
      ]);
      assert.equal((await decodePositionOf(rejected.mint)).state, "Rejected");

      const returnTo = async (recipient: Address, payer: TransactionSigner) =>
        sendIxs(connection, payer, [
          await returnRejected({
            payer: payer.address,
            depositor: recipient,
            pool,
            nftMint: rejected.mint,
            depositorToken: rejected.tokenAccount,
            metadata: rejected.metadata,
            masterEdition: rejected.masterEdition,
            positionTokenRecord: null,
            depositorTokenRecord: null,
            sysvarInstructions: null,
            authorizationRulesProgram: null,
            authorizationRules: null,
          }),
        ]);

      // Permissionless, but the destination is fixed by the configuration, never by the caller.
      await expectAnchorError(returnTo(stranger.address, stranger), NOT_POSITION_OWNER);
      await expectAnchorError(returnTo(t02Identity, stranger), NOT_POSITION_OWNER);

      // The control: the same call, naming the configured T02 authority, succeeds for any payer.
      await returnTo(t02Authority.address, stranger);
      const { position } = await positionOf(rejected.mint);
      assert.equal(await fetchAccountData(connection, position), null);
      assert.equal(await tokenAmount(rejected.tokenAccount), 1n, "the card did not reach the T02 authority");
    });

    it("t02_claim_nft: a wallet that is not the T02 authority is refused", async () => {
      const closed = await mintForAuthority();
      await depositAs(closed, t02Authority);
      await approve(closed.mint);
      await closeBelowFloor(closed.mint);
      assert.equal((await decodePositionOf(closed.mint)).state, "ClosedBelowFloor");

      await expectAnchorError(claimNftAs(closed, stranger), UNAUTHORIZED);
      covered.add("t02_claim_nft.t02_authority");

      // The control: the same call with the right signer releases the card to the authority.
      const { position } = await positionOf(closed.mint);
      await claimNftAs(closed, t02Authority);
      assert.equal(await fetchAccountData(connection, position), null, "the position is still open");
      assert.equal(
        await tokenAmount(closed.tokenAccount),
        1n,
        "the card did not return to the T02 authority",
      );
    });
  });

  describe("MPL Core family", () => {
    async function mintForAuthority(): Promise<Address> {
      return mintCoreAsset({
        connection,
        payer: admin,
        collection: coreCollection,
        collectionAuthority: coreCollectionAuthority,
        owner: t02Authority.address,
      });
    }

    async function depositCoreAs(asset: Address, signerWallet: TransactionSigner) {
      return sendIxs(connection, signerWallet, [
        await t02DepositCore({
          t02Authority: signerWallet.address,
          poolId,
          collection: coreCollection,
          asset,
        }),
      ]);
    }

    async function withdrawCoreAs(asset: Address, signerWallet: TransactionSigner) {
      return sendIxs(connection, signerWallet, [
        await t02WithdrawCore({
          t02Authority: signerWallet.address,
          poolId,
          asset,
          collection: coreCollection,
          weightIndex,
          successor: null,
        }),
      ]);
    }

    async function claimNftCoreAs(asset: Address, signerWallet: TransactionSigner) {
      return sendIxs(connection, signerWallet, [
        await t02ClaimNftCore({
          t02Authority: signerWallet.address,
          poolId,
          asset,
          collection: coreCollection,
        }),
      ]);
    }

    async function assertAccountless(asset: Address) {
      const { positionVault } = await positionOf(asset);
      assert.equal(
        await fetchAccountData(connection, positionVault),
        null,
        "the escrow stopped being accountless",
      );
    }

    let asset: Address;
    let t03Before: bigint;

    before(async () => {
      asset = await mintForAuthority();
      t03Before = await treasury03Balance();
    });

    it("t02_deposit_core: a wallet that is not the T02 authority is refused", async () => {
      await expectAnchorError(depositCoreAs(asset, stranger), UNAUTHORIZED);
      covered.add("t02_deposit_core.t02_authority");
    });

    it("t02_deposit_core: the authority deposits — Pending, no value, no USDC moved", async () => {
      await depositCoreAs(asset, t02Authority);

      const recorded = await decodePositionOf(asset);
      assert.equal(recorded.depositor, t02Identity);
      assert.equal(recorded.state, "Pending");
      assert.equal(recorded.recordedValue, 0n);
      assert.equal(recorded.standard, STANDARD_CORE);
      assert.equal(await treasury03Balance(), t03Before, "a deposit moves no USDC");

      const { positionVault } = await positionOf(asset);
      const escrowed = await readCoreAsset(connection, asset);
      assert.ok(escrowed, "the asset account must survive the deposit");
      assert.equal(escrowed.owner, positionVault, "the asset did not reach escrow");
      await assertAccountless(asset);
    });

    it("approve_deposit: the Operator activates a Core T02 deposit", async () => {
      await approve(asset);
      assert.equal((await decodePositionOf(asset)).state, "Active");
    });

    it("t02_withdraw_core: a wallet that is not the T02 authority is refused", async () => {
      await expectAnchorError(withdrawCoreAs(asset, stranger), UNAUTHORIZED);
      covered.add("t02_withdraw_core.t02_authority");
    });

    it("t02_withdraw_core: the T02 authority takes the asset back and the position closes", async () => {
      const { position } = await positionOf(asset);
      await withdrawCoreAs(asset, t02Authority);

      const released = await readCoreAsset(connection, asset);
      assert.ok(released, "the asset account must survive its own release");
      assert.equal(released.owner, t02Authority.address, "the asset did not return to the T02 authority");
      assert.equal(await fetchAccountData(connection, position), null, "the position is still open");
      await assertAccountless(asset);
      assert.equal(await treasury03Balance(), t03Before);
    });

    it("a rejected Core T02 deposit's asset goes to the T02 authority and to no one else", async () => {
      const rejected = await mintForAuthority();
      await depositCoreAs(rejected, t02Authority);
      await sendIxs(connection, operator, [
        await rejectDeposit({ operator: operator.address, pool, nftMint: rejected, reason: 1 }),
      ]);

      const returnTo = async (recipient: Address) =>
        sendIxs(connection, stranger, [
          await returnRejectedCore({
            payer: stranger.address,
            depositor: recipient,
            pool,
            asset: rejected,
            collection: coreCollection,
          }),
        ]);

      await expectAnchorError(returnTo(stranger.address), NOT_POSITION_OWNER);
      await expectAnchorError(returnTo(t02Identity), NOT_POSITION_OWNER);

      await returnTo(t02Authority.address);
      const returned = await readCoreAsset(connection, rejected);
      assert.ok(returned);
      assert.equal(returned.owner, t02Authority.address, "the asset did not reach the T02 authority");
    });

    it("t02_claim_nft_core: a wallet that is not the T02 authority is refused", async () => {
      const closed = await mintForAuthority();
      await depositCoreAs(closed, t02Authority);
      await approve(closed);
      await closeBelowFloor(closed);
      assert.equal((await decodePositionOf(closed)).state, "ClosedBelowFloor");

      await expectAnchorError(claimNftCoreAs(closed, stranger), UNAUTHORIZED);
      covered.add("t02_claim_nft_core.t02_authority");

      // The control: the same call with the right signer releases the asset to the authority.
      const { position } = await positionOf(closed);
      await claimNftCoreAs(closed, t02Authority);
      assert.equal(await fetchAccountData(connection, position), null, "the position is still open");
      const released = await readCoreAsset(connection, closed);
      assert.ok(released);
      assert.equal(released.owner, t02Authority.address, "the asset did not return to the T02 authority");
      await assertAccountless(closed);
    });
  });

  it("covers exactly the matrix rows this suite is answerable for", () => {
    // `t02_claim_fee`'s refusal is recorded in the Token Metadata block; its control is the claim
    // that follows it. Deleting a row from the table, or a refusal from this file, must red
    // rather than silently shrink the matrix.
    const owned = EXECUTED_ROWS.filter((row) => row.suite === "t02-authority").map(
      (row) => `${row.instruction}.${row.signer}`,
    );
    assert.deepEqual([...covered].sort(), [...owned].sort());
  });
});
