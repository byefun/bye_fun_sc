// The sale paid from the loader's escrow, against a live validator.
//
// The Operator names the loader; the program binds the three loader accounts to one owner and
// chooses the payer at execution. Every escrow-paid case below is paired with the same sale
// falling through to Treasury 03, so a suite that never reached the escrow path cannot pass by
// accident, and every refusal asserts that neither Treasury 03 nor the vault moved.

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { admitPoolCollection, createPool, launchProfile, smallTierProfile } from "../helpers/pools.js";
import { assertEveryPoolIdle, drainTreasury04 } from "../helpers/treasuries.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createPnftCollection, mintPlainNonFungiblePnft, type MintedAsset } from "../helpers/fixtures.js";
import { settleDrawnRoll } from "../helpers/roll.js";
import { VRF_QUEUE_FIXTURE } from "../helpers/vrf-queue.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { TOKEN_PROGRAM, VRF_PROGRAM } from "../../scripts/constants.js";
import { setPause, updateConfig, updateVrfConfig } from "../../scripts/lib/admin.js";
import { derivePda, seedFromPubkey, signer, writable } from "../../scripts/lib/anchor.js";
import {
  activateAcquired,
  deriveLoaderAccounts,
  instantSell,
  type LoaderAccounts,
} from "../../scripts/lib/buyback.js";
import { decodeEvent, decodePool, decodePosition, decodeWalletStats, type InstantSold } from "../../scripts/lib/decoders.js";
import { createAssociatedTokenAccountIdempotent } from "../../scripts/lib/associated-token.js";
import { approveDeposit, deposit } from "../../scripts/lib/deposit.js";
import { deriveEscrow, escrowDeposit, escrowWithdraw, openEscrow, setEscrowArmed } from "../../scripts/lib/escrow.js";
import { withdraw } from "../../scripts/lib/exit.js";
import { commitRolls, testResolveBatch } from "../../scripts/lib/roll.js";
import { resolveDisplaced, resolveSuccessor } from "../../scripts/lib/tier-resolve.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";

const PROGRAM_DATA_PREFIX = "Program data: ";
const USDC = 1_000_000n;

/** Verbatim against `common/errors.rs`. */
const BELOW_ADMISSION_FLOOR = 6104;
const DEPOSITS_PAUSED = 6107;
const NOT_POSITION_OWNER = 6302;
const ESCROW_ACCOUNTS_MISMATCH = 6802;

/** `launchProfile`'s rate and floor, and the launch bid, restated so a change reds this file. */
const RATE_BPS = 8_500n;
const BID_BPS = 9_250;
const ADMISSION_FLOOR = 12_000_000n;

/** The reference sale: V = 100, seller +85.00, escrow −92.50, Treasury 03 +7.50. */
const VALUE = 100n * USDC;
const PAID = (VALUE * RATE_BPS) / 10_000n;
const ESCROW_PAID = (VALUE * BigInt(BID_BPS)) / 10_000n;
const SPREAD = ESCROW_PAID - PAID;
const OBSERVED_AT = 1_800_000_000n;

/** The value the loader's own loaded card is approved at, so its rollup carries active value. */
const LOADED_VALUE = 1_000n * USDC;

const TREASURY_02_SEED = "t02";
const WALLET_STATS_SEED = "wallet";

/** SPL Token `MintTo`: instruction 7, then the amount as a little-endian u64. */
function mintToData(amount: bigint): Uint8Array {
  const data = new Uint8Array(9);
  data[0] = 7;
  new DataView(data.buffer).setBigUint64(1, amount, true);
  return data;
}

describe("escrow buyback — the sale pays from the loader's escrow", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let usdcMint: Address;

  async function balanceOf(tokenAccount: Address): Promise<bigint> {
    const { value } = await connection.rpc.getTokenAccountBalance(tokenAccount).send();
    return BigInt(value.amount);
  }

  async function instantSoldOf(signature: string): Promise<InstantSold> {
    const logs = await connection.getLogs(signature);
    const events = logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
    const sold = events.find((e) => e.name === "InstantSold");
    assert.ok(sold, "the sale emitted no InstantSold");
    return sold.data as InstantSold;
  }

  /** A funded wallet holding `usdc` micro-USDC in its own (legacy Token) account. */
  async function walletWithUsdc(usdc: bigint): Promise<{ wallet: TransactionSigner; usdcAccount: Address }> {
    const wallet = await fundedWallet(connection, 10);
    await connection.mintTokens(usdcMint, admin, usdc, wallet.address, false);
    return { wallet, usdcAccount: await getTokenAccountAddress(wallet.address, usdcMint, false) };
  }

  /** One pool with Treasury 03 funded, a pNFT collection admitted, and the given bid. */
  async function poolWithBid(bid: number, profile = launchProfile()) {
    const poolId = (await ensureProtocol(connection, admin)).poolCounter;
    const { signer: weightIndexSigner, instruction } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], instruction)]);
    const weightIndex = weightIndexSigner.address;
    const { pool, treasury03 } = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: profile,
    });
    await sendIxs(connection, admin, [
      {
        programAddress: TOKEN_PROGRAM,
        accounts: [writable(usdcMint), writable(treasury03), signer(admin.address)],
        data: mintToData(5_000n * USDC),
      },
    ]);
    if (bid !== 0) {
      await sendIxs(connection, admin, [
        await updateConfig({
          administrator: admin.address,
          poolId,
          update: { kind: "EscrowBidBps", value: bid },
          demoted: [],
        }),
      ]);
    }
    const collection = await createPnftCollection({ connection, payer: admin, authority: admin });
    await admitPoolCollection({ connection, administrator: admin, poolId, collection: collection.mint, standards: 0b011 });
    const { address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]);
    return { poolId, pool, weightIndex, treasury03, collection, t02Identity };
  }

  type Pool = Awaited<ReturnType<typeof poolWithBid>>;

  /** A legacy card owned by `owner`, in `pool`'s admitted collection. */
  function mintCard(p: Pool, owner: Address): Promise<MintedAsset> {
    return mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner,
      collection: p.collection,
    });
  }

  /** Loads a card into `p` as `owner`, creating its rollup there, and approves it at `value`. */
  async function load(p: Pool, owner: TransactionSigner, value: bigint | null): Promise<MintedAsset> {
    const asset = await mintCard(p, owner.address);
    await sendIxs(connection, owner, [
      await deposit({
        depositor: owner.address,
        poolId: p.poolId,
        collection: p.collection.mint,
        nftMint: asset.mint,
        depositorToken: asset.tokenAccount,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        depositorTokenRecord: null,
        positionTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
      }),
    ]);
    if (value !== null) {
      await sendIxs(connection, operator, [
        await approveDeposit({
          operator: operator.address,
          poolId: p.poolId,
          nftMint: asset.mint,
          depositor: owner.address,
          weightIndex: p.weightIndex,
          displaced: null,
          value,
          observedAt: BigInt(Math.floor(Date.now() / 1000)),
          lockUntil: 0n,
        }),
      ]);
    }
    return asset;
  }

  async function sell(
    p: Pool,
    seller: TransactionSigner,
    sellerUsdc: Address,
    asset: MintedAsset,
    loader: LoaderAccounts | null,
    value = VALUE,
  ): Promise<string> {
    const ix = await instantSell({
      seller: seller.address,
      operator: operator.address,
      poolId: p.poolId,
      collection: p.collection.mint,
      nftMint: asset.mint,
      sellerToken: asset.tokenAccount,
      sellerUsdc,
      weightIndex: p.weightIndex,
      metadata: asset.metadata,
      masterEdition: asset.masterEdition,
      sellerTokenRecord: null,
      positionTokenRecord: null,
      sysvarInstructions: null,
      authorizationRulesProgram: null,
      authorizationRules: null,
      displaced: null,
      loader,
      value,
      observedAt: OBSERVED_AT,
    });
    return sendIxs(connection, operator, [addSignersToInstruction([seller, operator], ix)]);
  }

  async function statsOf(pool: Address, owner: Address) {
    const { address } = await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(owner)]);
    const data = await fetchAccountData(connection, address);
    return data === null ? null : decodeWalletStats(Buffer.from(data));
  }

  async function positionOf(pool: Address, mint: Address) {
    const { address } = await derivePda(["position", seedFromPubkey(pool), seedFromPubkey(mint)]);
    return { address, ...decodePosition(Buffer.from((await fetchAccountData(connection, address))!)) };
  }

  after(async () => {
    await assertEveryPoolIdle(connection);
  });

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    usdcMint = (await ensureProtocol(connection, admin)).usdcMint;
  });

  describe("in a pool with a bid of 9,250 bps", () => {
    let p: Pool;
    let loader: TransactionSigner;
    let loaderUsdc: Address;
    let loaderAccounts: LoaderAccounts;
    let vault: Address;
    let seller: TransactionSigner;
    let sellerUsdc: Address;

    /** Every balance a sale can move. */
    async function balances() {
      return {
        seller: await balanceOf(sellerUsdc),
        treasury03: await balanceOf(p.treasury03),
        vault: await balanceOf(vault),
      };
    }

    /** Sets the loader's vault to exactly `amount`. */
    async function fundVaultTo(amount: bigint) {
      const current = await balanceOf(vault);
      if (current < amount) {
        await sendIxs(connection, loader, [
          await escrowDeposit({ owner: loader.address, ownerUsdc: loaderUsdc, amount: amount - current }),
        ]);
      } else if (current > amount) {
        await sendIxs(connection, loader, [
          await escrowWithdraw({ owner: loader.address, ownerUsdc: loaderUsdc, amount: current - amount }),
        ]);
      }
      assert.equal(await balanceOf(vault), amount);
    }

    before(async () => {
      p = await poolWithBid(BID_BPS);
      ({ wallet: loader, usdcAccount: loaderUsdc } = await walletWithUsdc(2_000n * USDC));
      ({ wallet: seller, usdcAccount: sellerUsdc } = await walletWithUsdc(0n));
      await load(p, loader, LOADED_VALUE);
      await sendIxs(connection, loader, [
        await openEscrow({ owner: loader.address, usdcMint }),
        await escrowDeposit({ owner: loader.address, ownerUsdc: loaderUsdc, amount: ESCROW_PAID }),
        await setEscrowArmed({ owner: loader.address, armed: true }),
      ]);
      loaderAccounts = await deriveLoaderAccounts(p.poolId, loader.address);
      vault = (await deriveEscrow(loader.address)).escrowVault;
      assert.equal(loaderAccounts.escrowVault, vault);
    });

    it("pays the seller 85 from the escrow at V = 100, Treasury 03 keeps 7.50, and the card is the loader's", async () => {
      await fundVaultTo(ESCROW_PAID);
      const asset = await mintCard(p, seller.address);
      const before = await balances();
      const loaderBefore = (await statsOf(p.pool, loader.address))!;
      const t02Before = await statsOf(p.pool, p.t02Identity);

      const signature = await sell(p, seller, sellerUsdc, asset, loaderAccounts);

      const after = await balances();
      assert.equal(after.seller - before.seller, PAID, "the seller is paid the winner offer");
      assert.equal(before.vault - after.vault, ESCROW_PAID, "the escrow pays the bid");
      assert.equal(after.treasury03 - before.treasury03, SPREAD, "Treasury 03 receives the spread");

      const position = await positionOf(p.pool, asset.mint);
      assert.equal(position.depositor, loader.address);
      assert.equal(position.state, "Active");
      assert.equal(position.recordedValue, VALUE);
      assert.equal(position.lockUntil, 0n);

      const loaderAfter = (await statsOf(p.pool, loader.address))!;
      assert.equal(loaderAfter.activePositions, loaderBefore.activePositions + 1);
      assert.equal(loaderAfter.activeValue, loaderBefore.activeValue + VALUE);
      const t02After = await statsOf(p.pool, p.t02Identity);
      assert.ok(t02After, "the sale did not create the Treasury 02 rollup");
      assert.equal(t02After.activeValue, t02Before?.activeValue ?? 0n, "an escrow sale credited Treasury 02");
      assert.equal(t02After.activePositions, t02Before?.activePositions ?? 0);

      const sold = await instantSoldOf(signature);
      assert.equal(sold.payer, "Escrow");
      assert.equal(sold.escrowOwner, loader.address);
      assert.equal(sold.escrowPaid, ESCROW_PAID);
      assert.equal(sold.bidBps, BID_BPS);
      assert.equal(sold.paid, PAID);
      assert.equal(sold.rateBps, Number(RATE_BPS));
    });

    // Each ineligibility on its own falls to Treasury 03: no error, the escrow untouched, and the
    // card a Treasury 02 position.
    async function assertTreasury03Pays(label: string, loaderForSale: LoaderAccounts | null) {
      const asset = await mintCard(p, seller.address);
      const before = await balances();
      const signature = await sell(p, seller, sellerUsdc, asset, loaderForSale);
      const after = await balances();
      assert.equal(after.seller - before.seller, PAID, `${label}: the seller was not paid the offer`);
      assert.equal(before.treasury03 - after.treasury03, PAID, `${label}: Treasury 03 did not pay`);
      assert.equal(after.vault, before.vault, `${label}: the escrow moved`);
      assert.equal((await positionOf(p.pool, asset.mint)).depositor, p.t02Identity, `${label}: not Treasury 02's`);
      const sold = await instantSoldOf(signature);
      assert.equal(sold.payer, "Treasury03");
      assert.equal(sold.escrowPaid, 0n);
      assert.equal(sold.bidBps, 0);
      assert.equal(sold.escrowOwner, "11111111111111111111111111111111");
    }

    it("falls to Treasury 03 when the escrow is disarmed", async () => {
      await fundVaultTo(ESCROW_PAID);
      await sendIxs(connection, loader, [await setEscrowArmed({ owner: loader.address, armed: false })]);
      await assertTreasury03Pays("disarmed", loaderAccounts);
      await sendIxs(connection, loader, [await setEscrowArmed({ owner: loader.address, armed: true })]);
    });

    it("falls to Treasury 03 when the vault is one micro-USDC short of the bid", async () => {
      await fundVaultTo(ESCROW_PAID - 1n);
      await assertTreasury03Pays("short", loaderAccounts);
    });

    it("falls to Treasury 03 when the value would put the loader over the wallet cap", async () => {
      await fundVaultTo(ESCROW_PAID);
      const loaderActive = (await statsOf(p.pool, loader.address))!.activeValue;
      const setCap = async (value: bigint) =>
        sendIxs(connection, admin, [
          await updateConfig({
            administrator: admin.address,
            poolId: p.poolId,
            update: { kind: "WalletValueCap", value },
            demoted: [],
          }),
        ]);
      await setCap(loaderActive + VALUE - 1n);
      try {
        await assertTreasury03Pays("over the loader's cap", loaderAccounts);
      } finally {
        await setCap(0n);
      }
    });

    it("falls to Treasury 03 for a loader who never opened an escrow", async () => {
      const { wallet: noEscrow } = await walletWithUsdc(0n);
      await load(p, noEscrow, null);
      await assertTreasury03Pays("no escrow", await deriveLoaderAccounts(p.poolId, noEscrow.address));
    });

    it("pays from Treasury 03 when the Operator names no loader", async () => {
      await fundVaultTo(ESCROW_PAID);
      await assertTreasury03Pays("no loader", null);
    });

    it("refuses a vault or a rollup not derived from the escrow's owner, and nothing moves", async () => {
      await fundVaultTo(ESCROW_PAID);
      const { wallet: other, usdcAccount: otherUsdc } = await walletWithUsdc(200n * USDC);
      await load(p, other, null);
      await sendIxs(connection, other, [
        await openEscrow({ owner: other.address, usdcMint }),
        await escrowDeposit({ owner: other.address, ownerUsdc: otherUsdc, amount: ESCROW_PAID }),
        await setEscrowArmed({ owner: other.address, armed: true }),
      ]);
      const others = await deriveLoaderAccounts(p.poolId, other.address);

      for (const [label, substituted] of [
        ["another escrow's vault", { ...loaderAccounts, escrowVault: others.escrowVault }],
        ["another owner's rollup", { ...loaderAccounts, walletStats: others.walletStats }],
      ] as const) {
        const asset = await mintCard(p, seller.address);
        const before = await balances();
        const otherVaultBefore = await balanceOf(others.escrowVault);
        await expectAnchorError(sell(p, seller, sellerUsdc, asset, substituted), ESCROW_ACCOUNTS_MISMATCH);
        assert.deepEqual(await balances(), before, `${label}: a refused sale moved funds`);
        assert.equal(await balanceOf(others.escrowVault), otherVaultBefore, `${label}: the other vault moved`);
      }
    });

    it("moves neither Treasury 03 nor the vault when a sale-side condition fails", async () => {
      await fundVaultTo(ESCROW_PAID);
      const asset = await mintCard(p, seller.address);
      const notHeld = await mintCard(p, loader.address);
      const before = await balances();

      await expectAnchorError(
        sell(p, seller, sellerUsdc, asset, loaderAccounts, ADMISSION_FLOOR - 1n),
        BELOW_ADMISSION_FLOOR,
      );
      await expectAnchorError(sell(p, seller, sellerUsdc, notHeld, loaderAccounts), NOT_POSITION_OWNER);

      await sendIxs(connection, admin, [
        await setPause({ administrator: admin.address, poolId: p.poolId, deposits: true, rolls: false, reason: 1 }),
      ]);
      try {
        await expectAnchorError(sell(p, seller, sellerUsdc, asset, loaderAccounts), DEPOSITS_PAUSED);
      } finally {
        await sendIxs(connection, admin, [
          await setPause({ administrator: admin.address, poolId: p.poolId, deposits: false, rolls: false, reason: 0 }),
        ]);
      }
      assert.deepEqual(await balances(), before, "a refused sale moved Treasury 03 or the vault");

      // The control: the same card sells, from the escrow, once the condition holds.
      await sell(p, seller, sellerUsdc, asset, loaderAccounts);
      assert.equal(await balanceOf(vault), before.vault - ESCROW_PAID);
    });

    it("pays two sales against an escrow covering one: the first from the escrow, the second from Treasury 03", async () => {
      await fundVaultTo(ESCROW_PAID);
      const first = await mintCard(p, seller.address);
      const second = await mintCard(p, seller.address);
      const before = await balances();

      const a = await instantSoldOf(await sell(p, seller, sellerUsdc, first, loaderAccounts));
      const b = await instantSoldOf(await sell(p, seller, sellerUsdc, second, loaderAccounts));

      assert.deepEqual([a.payer, b.payer], ["Escrow", "Treasury03"]);
      const after = await balances();
      assert.equal(after.vault, 0n);
      assert.equal(after.seller - before.seller, 2n * PAID);
      assert.equal(after.treasury03 - before.treasury03, SPREAD - PAID);
      assert.equal((await positionOf(p.pool, first.mint)).depositor, loader.address);
      assert.equal((await positionOf(p.pool, second.mint)).depositor, p.t02Identity);
    });
  });

  it("falls to Treasury 03 in a pool with no bid, and still refuses accounts not the loader's", async () => {
    const p = await poolWithBid(0);
    const { wallet: loader, usdcAccount: loaderUsdc } = await walletWithUsdc(200n * USDC);
    const { wallet: seller, usdcAccount: sellerUsdc } = await walletWithUsdc(0n);
    await load(p, loader, null);
    await sendIxs(connection, loader, [
      await openEscrow({ owner: loader.address, usdcMint }),
      await escrowDeposit({ owner: loader.address, ownerUsdc: loaderUsdc, amount: ESCROW_PAID }),
      await setEscrowArmed({ owner: loader.address, armed: true }),
    ]);
    const accounts = await deriveLoaderAccounts(p.poolId, loader.address);
    const vault = accounts.escrowVault;

    // The loader's accounts are bound whether or not the pool has a bid.
    const { wallet: stranger } = await walletWithUsdc(0n);
    const strangerStats = (await deriveLoaderAccounts(p.poolId, stranger.address)).walletStats;
    await expectAnchorError(
      sell(p, seller, sellerUsdc, await mintCard(p, seller.address), { ...accounts, walletStats: strangerStats }),
      ESCROW_ACCOUNTS_MISMATCH,
    );

    const asset = await mintCard(p, seller.address);
    const t03Before = await balanceOf(p.treasury03);
    const sold = await instantSoldOf(await sell(p, seller, sellerUsdc, asset, accounts));
    assert.equal(sold.payer, "Treasury03");
    assert.equal(await balanceOf(vault), ESCROW_PAID, "a pool with no bid debited the escrow");
    assert.equal(t03Before - (await balanceOf(p.treasury03)), PAID);
  });

  // A sale landing while a batch is open leaves the loader's position `Pending`; it reaches the
  // draw domain through `activate_acquired`, into the loader's own rollup, and the loader can then
  // withdraw it like any card it loaded.
  it("leaves the loader's position Pending during a batch, then activates it, and the loader withdraws it", async () => {
    await sendIxs(connection, admin, [
      await updateVrfConfig({
        administrator: admin.address,
        vrfProgram: VRF_PROGRAM,
        oracleQueue: VRF_QUEUE_FIXTURE,
        swapPool: SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: 100,
      }),
    ]);
    const p = await poolWithBid(BID_BPS, smallTierProfile());
    const protocolConfig = (await ensureProtocol(connection, admin)).protocolConfig;
    const { wallet: loader, usdcAccount: loaderUsdc } = await walletWithUsdc(500n * USDC);
    const { wallet: seller, usdcAccount: sellerUsdc } = await walletWithUsdc(0n);

    // The loader holds every card in the draw domain, so the settlement helper's single-wallet
    // assumption holds.
    const loaded: MintedAsset[] = [];
    for (let i = 0; i < 3; i += 1) loaded.push(await load(p, loader, 20n * USDC));
    await sendIxs(connection, loader, [
      await openEscrow({ owner: loader.address, usdcMint }),
      await escrowDeposit({ owner: loader.address, ownerUsdc: loaderUsdc, amount: ESCROW_PAID }),
      await setEscrowArmed({ owner: loader.address, armed: true }),
    ]);

    const batchId = decodePool(Buffer.from((await fetchAccountData(connection, p.pool))!)).batchCounter;
    await drainTreasury04({ connection, administrator: admin, operator });
    await sendIxs(connection, loader, [
      await commitRolls({
        roller: loader.address,
        poolId: p.poolId,
        batchCounter: batchId,
        rollerUsdc: loaderUsdc,
        usdcMint,
        oracleQueue: VRF_QUEUE_FIXTURE,
        n: 1,
      }),
    ]);
    assert.equal(decodePool(Buffer.from((await fetchAccountData(connection, p.pool))!)).openBatches, 1);

    const asset = await mintCard(p, seller.address);
    const sold = await instantSoldOf(
      await sell(p, seller, sellerUsdc, asset, await deriveLoaderAccounts(p.poolId, loader.address)),
    );
    assert.equal(sold.payer, "Escrow");
    let position = await positionOf(p.pool, asset.mint);
    assert.equal(position.state, "Pending");
    assert.equal(position.depositor, loader.address);
    const loaderBefore = (await statsOf(p.pool, loader.address))!;

    await sendIxs(connection, operator, [
      await testResolveBatch({ operator: operator.address, pool: p.pool, batchId, randomness: new Uint8Array(32).fill(7) }),
    ]);
    await settleDrawnRoll(
      {
        connection,
        operator,
        pool: p.pool,
        protocolConfig,
        weightIndex: p.weightIndex,
        roller: loader.address,
        rollerUsdc: loaderUsdc,
        assets: loaded,
      },
      batchId,
      0,
    );
    assert.equal(decodePool(Buffer.from((await fetchAccountData(connection, p.pool))!)).openBatches, 0);

    await sendIxs(connection, operator, [
      await activateAcquired({
        operator: operator.address,
        poolId: p.poolId,
        nftMint: asset.mint,
        weightIndex: p.weightIndex,
        displaced: await resolveDisplaced(connection, p.pool, { kind: "activating", value: VALUE }),
        depositor: loader.address,
      }),
    ]);
    position = await positionOf(p.pool, asset.mint);
    assert.equal(position.state, "Active");
    assert.equal(position.inTier, true, "a 100 USDC card ranks into a tier of 20 USDC cards");
    const loaderAfter = (await statsOf(p.pool, loader.address))!;
    assert.equal(loaderAfter.activeValue, loaderBefore.activeValue + VALUE);
    assert.equal(loaderAfter.activePositions, loaderBefore.activePositions + 1);

    const { address: loaderToken, instruction: createLoaderToken } = await createAssociatedTokenAccountIdempotent({
      payer: loader.address,
      owner: loader.address,
      mint: asset.mint,
    });
    await sendIxs(connection, loader, [
      createLoaderToken,
      await withdraw({
        depositor: loader.address,
        poolId: p.poolId,
        nftMint: asset.mint,
        weightIndex: p.weightIndex,
        depositorUsdc: loaderUsdc,
        depositorToken: loaderToken,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        positionTokenRecord: null,
        depositorTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
        successor: await resolveSuccessor(connection, p.pool, position.address),
      }),
    ]);
    assert.equal(await balanceOf(loaderToken), 1n, "the card did not reach the loader's wallet");
  });
});
