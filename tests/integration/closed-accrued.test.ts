// A closed position's stored `accrued` against a live validator: the card claims pay it before
// closing the position, and the fee claims pay it while the position stays closed.
//
// `record_value`'s below-floor close and `close_seized` settle the fee delta into
// `Position.accrued` and move no cash, so every case here needs a balance that real fees put there.
// The fixture settles a real roll over seven live cards, then closes two legacy positions below
// the floor, then settles a second roll so `acc_equal` grows past their closure. What each case
// proves:
//
//   - **owed without an account** — `claim_nft` on a position owing a balance, with no USDC
//     account, fails `AccountNotEnoughKeys` and moves nothing: the position and the card stay.
//   - **paid on the way out** — the owner's USDC account is emptied and closed, and one transaction
//     creates it again and claims: the card and exactly the stored `accrued` arrive, and
//     `owed_fees` and the principal vault fall by the same amount.
//   - **paid while closed** — `claim_fee` on a below-floor position pays exactly its stored
//     `accrued`, not the growth after its closure, and leaves it closed with its card claimable;
//     the card claim then pays 0 with no USDC account.
//   - **seized, then reachable again** — a frozen Core card is seized and thawed, and
//     `claim_nft_core` pays its balance with the card.
//   - **seized and burned** — the card claim can never succeed and refuses moving nothing, so the
//     fee claim pays the balance with no asset read; a second fee claim pays 0.
//
// Positions are minted with spares — three legacy, three burnable Core and three freezable Core —
// because each roll draws one card the test cannot choose without re-deriving the draw, and every
// case needs a survivor of its own kind.

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address, type Instruction } from "@solana/kit";
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
import { admitPoolCollection, createPool, smallTierProfile } from "../helpers/pools.js";
import { assertEveryPoolIdle } from "../helpers/treasuries.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createPnftCollection, mintPlainNonFungiblePnft, type MintedAsset } from "../helpers/fixtures.js";
import {
  burnCoreAsset,
  createCoreCollection,
  mintCoreAsset,
  readCoreAsset,
  setPermanentFreeze,
} from "../helpers/core-fixtures.js";
import { ACCOUNT_NOT_ENOUGH_KEYS, DRAWN_POSITION_MISMATCH, NO_POSITION, settleDrawnRoll } from "../helpers/roll.js";
import { VRF_QUEUE_FIXTURE } from "../helpers/vrf-queue.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { TOKEN_PROGRAM, VRF_PROGRAM } from "../../scripts/constants.js";
import { updateVrfConfig } from "../../scripts/lib/admin.js";
import { approveDeposit, deposit, depositCore } from "../../scripts/lib/deposit.js";
import { claimFee, claimNft, claimNftCore, closeSeized } from "../../scripts/lib/exit.js";
import { commitRolls, settleRollCore, testResolveBatch } from "../../scripts/lib/roll.js";
import { beginSweep, endSweep, recordValue } from "../../scripts/lib/value.js";
import { resolveSuccessor } from "../../scripts/lib/tier-resolve.js";
import { createAssociatedTokenAccountIdempotent } from "../../scripts/lib/associated-token.js";
import {
  decodeEvent,
  decodePool,
  decodePosition,
  decodeProtocolConfig,
  type DecodedEvent,
  type Position,
} from "../../scripts/lib/decoders.js";
import { deriveAssociatedTokenAddress, derivePda, seedFromPubkey, writable, writableSigner } from "../../scripts/lib/anchor.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";
import { expandDraw } from "../../scripts/verify-batch.js";

const PROGRAM_DATA_PREFIX = "Program data: ";

const USDC = 1_000_000n;
const ADMITTED_VALUE = 20n * USDC;
/** Under `smallTierProfile`'s admission floor (12 USDC) and its ticket target (9 USDC). */
const BELOW_FLOOR_VALUE = 5n * USDC;
const COLLATERAL_ABSENT = 6307;
/** `common/constants.rs`'s `WEIGHT_C`: a value's draw weight is `WEIGHT_C / value`. */
const WEIGHT_C = 1_000_000_000_000_000_000n;
const SPARES = 3;

/**
 * The first candidate randomness whose roll 0 lands on a card rather than a blank: a blank against
 * this fixture's empty Treasury 04 is refunded, and a refund books no fee. Which card it draws is
 * left to the program; the settlement below finds out by trying candidates.
 */
function randomnessDrawingACard(state: {
  wReal: bigint;
  blanks: readonly number[];
  blankFaces: readonly bigint[];
}): Uint8Array {
  // A slot whose face is 0 carries no weight, as on chain.
  const blankWeight = state.blanks.reduce(
    (total, count, i) =>
      state.blankFaces[i] === 0n ? total : total + BigInt(count) * (WEIGHT_C / state.blankFaces[i]!),
    0n,
  );
  const w = state.wReal + blankWeight;
  for (let fill = 0; fill < 256; fill += 1) {
    const randomness = new Uint8Array(32).fill(fill);
    if (expandDraw(randomness, 0, w).r >= blankWeight) return randomness;
  }
  throw new Error(`no candidate randomness draws a card — w_real=${state.wReal}, blank weight=${blankWeight}`);
}

/** SPL Token `Burn` (tag 8) then `CloseAccount` (tag 9): empties and removes `account`. */
function burnAndClose(account: Address, mint: Address, owner: Address, amount: bigint): Instruction[] {
  const burnData = new Uint8Array(9);
  burnData[0] = 8;
  new DataView(burnData.buffer).setBigUint64(1, amount, true);
  return [
    {
      programAddress: TOKEN_PROGRAM,
      accounts: [writable(account), writable(mint), writableSigner(owner)],
      data: burnData,
    },
    {
      programAddress: TOKEN_PROGRAM,
      accounts: [writable(account), writable(owner), writableSigner(owner)],
      data: new Uint8Array([9]),
    },
  ];
}

describe("a closed position's stored accrued is paid by the card and fee claims", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let thief: TransactionSigner;
  let owner: TransactionSigner;
  let ownerUsdc: Address;
  let usdcMint: Address;
  let protocolConfig: Address;
  let poolId: number;
  let pool: Address;
  let principalVault: Address;
  let weightIndex: Address;
  let legacyCollection: Address;
  let coreCollection: Address;
  const legacy: MintedAsset[] = [];
  const burnable: Address[] = [];
  const freezable: Address[] = [];
  /** The two legacy positions closed below the floor between the two rolls. */
  let closedFirst: MintedAsset;
  let closedSecond: MintedAsset;

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  async function balanceOf(tokenAccount: Address): Promise<bigint> {
    const { value } = await connection.rpc.getTokenAccountBalance(tokenAccount).send();
    return BigInt(value.amount);
  }

  async function readPool() {
    return decodePool((await fetchAccountData(connection, pool))!);
  }

  async function positionOf(mint: Address): Promise<{ address: Address; state: Position | null }> {
    const { address } = await derivePda(["position", seedFromPubkey(pool), seedFromPubkey(mint)]);
    const data = await fetchAccountData(connection, address);
    return { address, state: data === null ? null : decodePosition(data) };
  }

  async function isActive(mint: Address): Promise<boolean> {
    return (await positionOf(mint)).state?.state === "Active";
  }

  async function approve(mint: Address): Promise<void> {
    await sendIxs(connection, operator, [
      await approveDeposit({
        operator: operator.address,
        poolId,
        nftMint: mint,
        depositor: owner.address,
        weightIndex,
        displaced: null,
        value: ADMITTED_VALUE,
        observedAt: BigInt(Math.floor(Date.now() / 1000)),
        lockUntil: 0n,
      }),
    ]);
  }

  /**
   * Settles roll 0 of `batchId`: the Token Metadata candidates through `settleDrawnRoll`, and, when
   * none of them is the drawn card, each live Core card through `settle_roll_core`. Only `Active`
   * positions are offered — a closed one is refused by a guard, not as a mismatch.
   */
  async function settleOneRoll(batchId: bigint): Promise<Address> {
    const live: number[] = [];
    for (const [i, asset] of legacy.entries()) if (await isActive(asset.mint)) live.push(i);
    try {
      const drawn = await settleDrawnRoll(
        { connection, operator, pool, protocolConfig, weightIndex, roller: owner.address, rollerUsdc: ownerUsdc, assets: legacy },
        batchId,
        0,
        live,
      );
      assert.notEqual(drawn, NO_POSITION, "the roll predicted to draw a card drew nothing");
      return legacy[drawn]!.mint;
    } catch (e) {
      if (!(e instanceof Error) || !e.message.startsWith("no candidate settled")) throw e;
    }
    const cfg = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    for (const asset of [...burnable, ...freezable]) {
      if (!(await isActive(asset))) continue;
      try {
        await sendIxs(connection, operator, [
          await settleRollCore({
            operator: operator.address,
            pool,
            batchId,
            rollIndex: 0,
            rollerUsdc: ownerUsdc,
            protocolRevenue: cfg.protocolRevenue,
            treasury05: await deriveAssociatedTokenAddress(cfg.t05Authority, cfg.usdcMint),
            weightIndex,
            roller: owner.address,
            asset,
            collection: coreCollection,
            depositor: owner.address,
            settlementUsdc: ownerUsdc,
            successor: null,
          }),
        ]);
        return asset;
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        if (!message.includes(String(DRAWN_POSITION_MISMATCH)) && !message.includes(String(ACCOUNT_NOT_ENOUGH_KEYS))) {
          throw e;
        }
      }
    }
    throw new Error(`no candidate, Token Metadata or Core, settled batch ${batchId}`);
  }

  async function rollOnce(): Promise<Address> {
    const batchId = (await readPool()).batchCounter;
    await sendIxs(connection, owner, [
      await commitRolls({
        roller: owner.address,
        poolId,
        batchCounter: batchId,
        rollerUsdc: ownerUsdc,
        usdcMint,
        oracleQueue: VRF_QUEUE_FIXTURE,
        n: 1,
      }),
    ]);
    const randomness = randomnessDrawingACard(await readPool());
    await sendIxs(connection, operator, [await testResolveBatch({ operator: operator.address, pool, batchId, randomness })]);
    return settleOneRoll(batchId);
  }

  /** One sweep closing `mint`'s position below the floor; the `finally` keeps a failure local. */
  async function sweepClose(mint: Address): Promise<void> {
    const { address: position } = await positionOf(mint);
    await sendIxs(connection, operator, [await beginSweep({ operator: operator.address, poolId })]);
    try {
      await sendIxs(connection, operator, [
        await recordValue({
          operator: operator.address,
          poolId,
          nftMint: mint,
          depositor: owner.address,
          weightIndex,
          value: BELOW_FLOOR_VALUE,
          observedAt: BigInt(Math.floor(Date.now() / 1000)),
          effect: { kind: "close", successor: await resolveSuccessor(connection, pool, position) },
        }),
      ]);
    } finally {
      await sendIxs(connection, operator, [await endSweep({ operator: operator.address, poolId })]);
    }
  }

  async function seize(asset: Address): Promise<void> {
    await sendIxs(connection, operator, [
      await closeSeized({
        operator: operator.address,
        poolId,
        asset,
        collection: coreCollection,
        depositor: owner.address,
        weightIndex,
      }),
    ]);
  }

  async function firstActive(assets: Address[]): Promise<Address> {
    for (const asset of assets) if (await isActive(asset)) return asset;
    throw new Error("every spare was drawn — the fixture needs more of this kind");
  }

  function legacyClaim(asset: MintedAsset, depositorUsdc: Address | null) {
    return claimNft({
      depositor: owner.address,
      pool,
      nftMint: asset.mint,
      depositorUsdc,
      depositorToken: asset.tokenAccount,
      metadata: asset.metadata,
      masterEdition: asset.masterEdition,
      positionTokenRecord: null,
      depositorTokenRecord: null,
      sysvarInstructions: null,
      authorizationRulesProgram: null,
      authorizationRules: null,
    });
  }

  function feesPaidOf(events: DecodedEvent[], name: "NftClaimed" | "FeeClaimed"): bigint {
    const matching = events.filter((e) => e.name === name);
    assert.equal(matching.length, 1, `expected one ${name}, got ${events.map((e) => e.name).join(", ")}`);
    return (matching[0]!.data as { feesPaid: bigint }).feesPaid;
  }

  after(async () => {
    await assertEveryPoolIdle(connection);
  });

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    thief = await fundedWallet(connection, 10);
    owner = await fundedWallet(connection, 40);

    const protocolState = await ensureProtocol(connection, admin);
    usdcMint = protocolState.usdcMint;
    protocolConfig = protocolState.protocolConfig;
    poolId = protocolState.poolCounter;
    await connection.mintTokens(usdcMint, admin, 1_000n * USDC, owner.address, false);
    ownerUsdc = await getTokenAccountAddress(owner.address, usdcMint, false);

    // `commit_rolls` pins `oracle_queue` to the config, which the bootstrap leaves at a placeholder.
    await sendIxs(connection, admin, [
      await updateVrfConfig({
        administrator: admin.address,
        vrfProgram: VRF_PROGRAM,
        oracleQueue: VRF_QUEUE_FIXTURE,
        swapPool: SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: 100,
      }),
    ]);

    const { signer: weightIndexSigner, instruction } = await createWeightIndexAccount({ rpc: connection, payer: admin.address });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], instruction)]);
    weightIndex = weightIndexSigner.address;
    pool = (await createPool({ connection, administrator: admin, poolId, weightIndex, usdcMint, args: smallTierProfile() })).pool;
    ({ address: principalVault } = await derivePda(["principal", seedFromPubkey(pool)]));

    const pnft = await createPnftCollection({ connection, payer: admin, authority: admin });
    legacyCollection = pnft.mint;
    await admitPoolCollection({ connection, administrator: admin, poolId, collection: legacyCollection, standards: 0b011 });
    const core = await createCoreCollection({ connection, payer: admin, name: "closed accrued" });
    coreCollection = core.collection;
    await admitPoolCollection({ connection, administrator: admin, poolId, collection: coreCollection, standards: 0b100 });

    for (let i = 0; i < SPARES; i += 1) {
      const asset = await mintPlainNonFungiblePnft({ connection, payer: admin, authority: admin, owner: owner.address, collection: pnft });
      legacy.push(asset);
      await sendIxs(connection, owner, [
        await deposit({
          depositor: owner.address,
          poolId,
          collection: legacyCollection,
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
      await approve(asset.mint);
    }
    for (const [list, plugin] of [
      [burnable, { kind: "permanentBurnDelegate" as const, authority: thief.address }],
      [freezable, { kind: "permanentFreezeDelegate" as const, authority: thief.address, frozen: false }],
    ] as const) {
      for (let i = 0; i < SPARES; i += 1) {
        const asset = await mintCoreAsset({
          connection,
          payer: admin,
          collection: coreCollection,
          collectionAuthority: core.authority,
          owner: owner.address,
          plugins: [plugin],
        });
        list.push(asset);
        await sendIxs(connection, owner, [
          await depositCore({ depositor: owner.address, poolId, collection: coreCollection, asset }),
        ]);
        await approve(asset);
      }
    }

    await rollOnce();

    const survivors: MintedAsset[] = [];
    for (const asset of legacy) if (await isActive(asset.mint)) survivors.push(asset);
    assert.ok(survivors.length >= 2, `only ${survivors.length} legacy positions survived the first roll`);
    [closedFirst, closedSecond] = survivors as [MintedAsset, MintedAsset];
    await sweepClose(closedFirst.mint);
    await sweepClose(closedSecond.mint);

    // Growth after the closures, which a closed position must never be paid.
    await rollOnce();
  });

  it("refuses a card claim owing a balance with no USDC account, and nothing moves", async () => {
    const { address: position, state } = await positionOf(closedFirst.mint);
    assert.equal(state?.state, "ClosedBelowFloor");
    assert.ok(state.accrued > 0n, "the close stored no balance — the case proves nothing");
    const poolBefore = await readPool();

    await expectAnchorError(sendIxs(connection, owner, [await legacyClaim(closedFirst, null)]), ACCOUNT_NOT_ENOUGH_KEYS);

    const after = (await positionOf(closedFirst.mint)).state;
    assert.equal(after?.accrued, state.accrued, "the refused claim touched the balance");
    assert.equal((await readPool()).owedFees, poolBefore.owedFees);
    assert.notEqual(await fetchAccountData(connection, position), null);
    assert.equal(await balanceOf(closedFirst.tokenAccount), 0n, "the refused claim released the card");
  });

  it("pays a below-floor position's stored accrued alone through the fee claim, and leaves it closed", async () => {
    const { state } = await positionOf(closedSecond.mint);
    assert.equal(state?.state, "ClosedBelowFloor");
    const owed = state.accrued;
    assert.ok(owed > 0n, "the close stored no balance — the case proves nothing");
    const poolBefore = await readPool();
    assert.ok(
      poolBefore.accEqual > state.equalCheckpoint,
      "the second roll grew no accumulator past the closure — the no-growth claim is untested",
    );
    const walletBefore = await balanceOf(ownerUsdc);
    const vaultBefore = await balanceOf(principalVault);

    const signature = await sendIxs(connection, owner, [
      await claimFee({ depositor: owner.address, poolId, nftMint: closedSecond.mint, depositorUsdc: ownerUsdc }),
    ]);

    assert.equal(feesPaidOf(await eventsOf(signature), "FeeClaimed"), owed, "paid other than the stored accrued");
    assert.equal(await balanceOf(ownerUsdc), walletBefore + owed);
    assert.equal(await balanceOf(principalVault), vaultBefore - owed);
    assert.equal((await readPool()).owedFees, poolBefore.owedFees - owed);
    const after = (await positionOf(closedSecond.mint)).state;
    assert.equal(after?.state, "ClosedBelowFloor", "the fee claim moved the position out of its terminal");
    assert.equal(after.accrued, 0n);
    assert.equal(after.equalCheckpoint, state.equalCheckpoint, "a closed position's checkpoint was stamped");
  });

  it("then claims the card back paying 0, with no USDC account", async () => {
    const walletBefore = await balanceOf(ownerUsdc);
    const owedBefore = (await readPool()).owedFees;

    const signature = await sendIxs(connection, owner, [await legacyClaim(closedSecond, null)]);

    assert.equal(feesPaidOf(await eventsOf(signature), "NftClaimed"), 0n);
    assert.equal(await balanceOf(closedSecond.tokenAccount), 1n, "the card did not come back");
    assert.equal((await positionOf(closedSecond.mint)).state, null, "the position was not closed");
    assert.equal(await balanceOf(ownerUsdc), walletBefore);
    assert.equal((await readPool()).owedFees, owedBefore);
  });

  it("pays a seized Core card's balance with the card once the card is reachable again", async () => {
    const asset = await firstActive(freezable);
    await setPermanentFreeze({ connection, payer: thief, asset, collection: coreCollection, authority: thief, frozen: true });
    await seize(asset);
    const { state } = await positionOf(asset);
    assert.equal(state?.state, "Seized");
    const owed = state.accrued;
    assert.ok(owed > 0n, "the seizure stored no balance — the case proves nothing");
    await setPermanentFreeze({ connection, payer: thief, asset, collection: coreCollection, authority: thief, frozen: false });
    const walletBefore = await balanceOf(ownerUsdc);
    const owedBefore = (await readPool()).owedFees;

    const signature = await sendIxs(connection, owner, [
      await claimNftCore({ depositor: owner.address, pool, asset, collection: coreCollection, depositorUsdc: ownerUsdc }),
    ]);

    assert.equal(feesPaidOf(await eventsOf(signature), "NftClaimed"), owed);
    assert.equal(await balanceOf(ownerUsdc), walletBefore + owed);
    assert.equal((await readPool()).owedFees, owedBefore - owed);
    assert.equal((await readCoreAsset(connection, asset))?.owner, owner.address, "the card did not come back");
    assert.equal((await positionOf(asset)).state, null, "the position was not closed");
  });

  it("pays a burned Core card's balance through the fee claim, which reads no asset", async () => {
    const asset = await firstActive(burnable);
    await burnCoreAsset({ connection, payer: thief, asset, collection: coreCollection, authority: thief });
    await seize(asset);
    const { state } = await positionOf(asset);
    assert.equal(state?.state, "Seized");
    const owed = state.accrued;
    assert.ok(owed > 0n, "the seizure stored no balance — the case proves nothing");
    const owedBefore = (await readPool()).owedFees;

    // The card claim can never succeed on a burned card, and refusing it moves nothing.
    await expectAnchorError(
      sendIxs(connection, owner, [
        await claimNftCore({ depositor: owner.address, pool, asset, collection: coreCollection, depositorUsdc: ownerUsdc }),
      ]),
      COLLATERAL_ABSENT,
    );
    assert.equal((await readPool()).owedFees, owedBefore);

    const walletBefore = await balanceOf(ownerUsdc);
    const signature = await sendIxs(connection, owner, [
      await claimFee({ depositor: owner.address, poolId, nftMint: asset, depositorUsdc: ownerUsdc }),
    ]);
    assert.equal(feesPaidOf(await eventsOf(signature), "FeeClaimed"), owed);
    assert.equal(await balanceOf(ownerUsdc), walletBefore + owed);
    assert.equal((await readPool()).owedFees, owedBefore - owed);
    const after = (await positionOf(asset)).state;
    assert.equal(after?.state, "Seized");
    assert.equal(after.accrued, 0n);

    const again = await sendIxs(connection, owner, [
      await claimFee({ depositor: owner.address, poolId, nftMint: asset, depositorUsdc: ownerUsdc }),
    ]);
    assert.equal(feesPaidOf(await eventsOf(again), "FeeClaimed"), 0n, "a second fee claim paid again");
    assert.equal(await balanceOf(ownerUsdc), walletBefore + owed);
  });

  // Last, because it removes the owner's USDC account.
  it("pays the card claim's balance to a USDC account the same transaction creates", async () => {
    const { state } = await positionOf(closedFirst.mint);
    const owed = state!.accrued;
    assert.ok(owed > 0n);
    await sendIxs(connection, owner, burnAndClose(ownerUsdc, usdcMint, owner.address, await balanceOf(ownerUsdc)));
    assert.equal(await fetchAccountData(connection, ownerUsdc), null, "the USDC account still exists — the case proves nothing");
    const owedBefore = (await readPool()).owedFees;
    const vaultBefore = await balanceOf(principalVault);

    const { address: created, instruction: createAta } = await createAssociatedTokenAccountIdempotent({
      payer: owner.address,
      owner: owner.address,
      mint: usdcMint,
    });
    assert.equal(created, ownerUsdc);
    const signature = await sendIxs(connection, owner, [createAta, await legacyClaim(closedFirst, created)]);

    assert.equal(feesPaidOf(await eventsOf(signature), "NftClaimed"), owed);
    assert.equal(await balanceOf(created), owed);
    assert.equal(await balanceOf(principalVault), vaultBefore - owed);
    assert.equal((await readPool()).owedFees, owedBefore - owed);
    assert.equal(await balanceOf(closedFirst.tokenAccount), 1n, "the card did not come back");
    assert.equal((await positionOf(closedFirst.mint)).state, null, "the position was not closed");
  });
});
