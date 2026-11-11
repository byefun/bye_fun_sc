// The shared Treasury 03 and Treasury 04 as two pools meet them: one pool's sales can empty the
// Treasury 03 another pool's sale is paid from, and the refusals `consolidate_treasuries` and `sweep_t04_overflow`
// owe beyond the ones their own suites drive.
//
// Nothing here opens a batch: `sweep_t04_overflow` needs every pool idle, and a later suite sweeps.

import { before, describe, it } from "node:test";
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
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { admitPoolCollection, createPool, goldProfile, launchProfile, type PoolConfigArgs } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createPnftCollection, mintPlainNonFungiblePnft, type PnftCollection } from "../helpers/fixtures.js";
import { everyPoolId, sendSweep, t04Ceiling } from "../helpers/treasuries.js";
import { UNAUTHORIZED } from "../helpers/authority-matrix.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { TOKEN_PROGRAM } from "../../scripts/constants.js";
import {
  consolidateTreasuries,
  deriveSharedTreasuries,
  setT04Ceiling,
  sweepT04Overflow,
} from "../../scripts/lib/admin.js";
import { instantSell } from "../../scripts/lib/buyback.js";
import { derivePda, readonly, signer, writable } from "../../scripts/lib/anchor.js";
import { decodePool } from "../../scripts/lib/decoders.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import {
  MISNAMED_UNCONSOLIDATED_POOL_ID,
  UNCONSOLIDATED_T03_AMOUNT,
  UNCONSOLIDATED_T04_AMOUNT,
  unconsolidatedPoolAccounts,
} from "../../scripts/lib/legacy-pool-fixtures.js";

const T03_SHORTFALL = 6602;
const SWEEP_POOL_SET_INCOMPLETE = 6532;
const TREASURY_NOT_PER_POOL = 6533;
/** Anchor's `AccountDiscriminatorMismatch`. */
const ACCOUNT_DISCRIMINATOR_MISMATCH = 3002;

/** Both profiles buy back at 85%: a payout that is a multiple of 17 comes from a value of 20/17 of it, exactly. */
const BUYBACK_RATE_BPS = 8_500n;
const OBSERVED_AT = 1_800_000_000n;
/** Gold's admission ceiling bounds one sale's value, so a large drain takes several. */
const GOLD_MAX_PAYOUT = (goldProfile().admissionCeiling * BUYBACK_RATE_BPS) / 10_000n;
const GOLD_MIN_PAYOUT = (goldProfile().admissionFloor * BUYBACK_RATE_BPS) / 10_000n;
/** Bronze's sale: 24 USDC, paying 20.4 USDC. */
const BRONZE_VALUE = 24_000_000n;
const BRONZE_PAYOUT = (BRONZE_VALUE * BUYBACK_RATE_BPS) / 10_000n;

function mintToData(amount: bigint): Uint8Array {
  const data = new Uint8Array(9);
  data[0] = 7;
  new DataView(data.buffer).setBigUint64(1, amount, true);
  return data;
}

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

type SellingPool = {
  poolId: number;
  weightIndex: Address;
  treasury03: Address;
  treasury04: Address;
  collection: PnftCollection;
};

describe("shared treasuries across pools", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let unprivileged: TransactionSigner;
  let seller: TransactionSigner;
  let sellerUsdc: Address;
  let usdcMint: Address;
  let shared: { treasury03: Address; treasury04: Address };

  async function balanceOf(tokenAccount: Address): Promise<bigint> {
    const { value } = await connection.rpc.getTokenAccountBalance(tokenAccount).send();
    return BigInt(value.amount);
  }

  async function mintTo(tokenAccount: Address, amount: bigint): Promise<void> {
    await sendIxs(connection, admin, [
      {
        programAddress: TOKEN_PROGRAM,
        accounts: [writable(usdcMint), writable(tokenAccount), signer(admin.address)],
        data: mintToData(amount),
      },
    ]);
  }

  async function sellingPool(args: PoolConfigArgs, name: string): Promise<SellingPool> {
    const poolId = (await ensureProtocol(connection, admin)).poolCounter;
    const { signer: wi, instruction } = await createWeightIndexAccount({ rpc: connection, payer: admin.address });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, wi], instruction)]);
    const accounts = await createPool({ connection, administrator: admin, poolId, weightIndex: wi.address, usdcMint, args });
    const collection = await createPnftCollection({ connection, payer: admin, authority: admin, name });
    await admitPoolCollection({ connection, administrator: admin, poolId, collection: collection.mint, standards: 0b011 });
    return { poolId, weightIndex: wi.address, treasury03: accounts.treasury03, treasury04: accounts.treasury04, collection };
  }

  async function sell(pool: SellingPool, value: bigint, card?: Awaited<ReturnType<typeof mintPlainNonFungiblePnft>>) {
    const asset =
      card ??
      (await mintPlainNonFungiblePnft({ connection, payer: admin, authority: admin, owner: seller.address, collection: pool.collection }));
    const ix = await instantSell({
      seller: seller.address,
      operator: operator.address,
      poolId: pool.poolId,
      collection: pool.collection.mint,
      nftMint: asset.mint,
      sellerToken: asset.tokenAccount,
      sellerUsdc,
      weightIndex: pool.weightIndex,
      metadata: asset.metadata,
      masterEdition: asset.masterEdition,
      sellerTokenRecord: null,
      positionTokenRecord: null,
      sysvarInstructions: null,
      authorizationRulesProgram: null,
      authorizationRules: null,
      displaced: null,
      loader: null,
      value,
      observedAt: OBSERVED_AT,
    });
    return { asset, send: () => sendIxs(connection, operator, [addSignersToInstruction([seller, operator], ix)]) };
  }

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    unprivileged = await loadUnprivileged();
    seller = await fundedWallet(connection, 20);
    usdcMint = (await ensureProtocol(connection, admin)).usdcMint;
    shared = await deriveSharedTreasuries();
    await connection.mintTokens(usdcMint, admin, 0n, seller.address, false);
    sellerUsdc = await getTokenAccountAddress(seller.address, usdcMint, false);
  });

  // ── One pool's sales empty the Treasury 03 another pool's sale is paid from ─────────

  it("Gold's sales empty Treasury 03, Bronze's sale then fails until Treasury 03 is refilled", async () => {
    const gold = await sellingPool(goldProfile(), "shared Gold");
    const bronze = await sellingPool(launchProfile(), "shared Bronze");
    assert.equal(gold.treasury03, shared.treasury03);
    assert.equal(bronze.treasury03, shared.treasury03);
    assert.notEqual(gold.poolId, bronze.poolId);

    if ((await balanceOf(shared.treasury03)) < BRONZE_PAYOUT + GOLD_MIN_PAYOUT) {
      await mintTo(shared.treasury03, BRONZE_PAYOUT + GOLD_MIN_PAYOUT);
    }
    const start = await balanceOf(shared.treasury03);
    assert.ok(start >= BRONZE_PAYOUT, "precondition: before Gold sells, Treasury 03 covers Bronze's sale");

    const residue = BRONZE_PAYOUT - 1_000_000n;
    let toDrain = start - residue;
    while (toDrain >= GOLD_MIN_PAYOUT) {
      let payout = toDrain > GOLD_MAX_PAYOUT ? GOLD_MAX_PAYOUT : toDrain;
      if (toDrain - payout > 0n && toDrain - payout < GOLD_MIN_PAYOUT) payout = toDrain - GOLD_MIN_PAYOUT;
      payout -= payout % 17n;
      const before = await balanceOf(shared.treasury03);
      await (await sell(gold, (payout * 20n) / 17n)).send();
      assert.equal(await balanceOf(shared.treasury03), before - payout, "a Gold sale was not paid from the shared Treasury 03");
      toDrain -= payout;
    }
    const drained = await balanceOf(shared.treasury03);
    assert.ok(drained < BRONZE_PAYOUT, `Gold's sales left ${drained}, enough for Bronze's ${BRONZE_PAYOUT}`);

    const bronzeSale = await sell(bronze, BRONZE_VALUE);
    await expectAnchorError(bronzeSale.send(), T03_SHORTFALL);
    assert.equal(await balanceOf(shared.treasury03), drained);

    await mintTo(shared.treasury03, BRONZE_PAYOUT - drained);
    const sellerBefore = await balanceOf(sellerUsdc);
    await (await sell(bronze, BRONZE_VALUE, bronzeSale.asset)).send();
    assert.equal(await balanceOf(shared.treasury03), 0n, "Bronze's sale was not paid from the refilled shared Treasury 03");
    assert.equal(await balanceOf(sellerUsdc), sellerBefore + BRONZE_PAYOUT);
    await mintTo(shared.treasury03, start);
  });

  // ── consolidate_treasuries: a pool naming a Treasury 03 other than its own ────────────────

  it("consolidate_treasuries refuses a pool whose Treasury 03 is not its per-pool account, moving nothing", async () => {
    const misnamed = await unconsolidatedPoolAccounts(MISNAMED_UNCONSOLIDATED_POOL_ID, 0, false);
    assert.ok(await fetchAccountData(connection, misnamed.pool.address), "the validator did not load the misnamed pool");
    assert.notEqual(misnamed.fields.treasury03, misnamed.treasury03.address, "precondition: the pool names another account");
    const t03Before = await balanceOf(shared.treasury03);
    await expectAnchorError(
      sendIxs(connection, admin, [
        await consolidateTreasuries({ administrator: admin.address, poolId: MISNAMED_UNCONSOLIDATED_POOL_ID, usdcMint }),
      ]),
      TREASURY_NOT_PER_POOL,
    );
    assert.equal(await balanceOf(misnamed.treasury03.address), UNCONSOLIDATED_T03_AMOUNT);
    assert.equal(await balanceOf(misnamed.treasury04.address), UNCONSOLIDATED_T04_AMOUNT);
    assert.equal(await balanceOf(shared.treasury03), t03Before);
  });

  // ── sweep_t04_overflow and set_t04_ceiling: the refusals the sweep suite leaves out ────────

  describe("the sweep's pool set and the ceiling's signer", () => {
    let restore: bigint;

    before(async () => {
      restore = await t04Ceiling(connection);
      await sendIxs(connection, admin, [await setT04Ceiling({ administrator: admin.address, ceiling: 1n })]);
      await mintTo(shared.treasury04, 5_000_000n);
    });

    async function sweepWith(remaining: Address[]) {
      const ix = await sweepT04Overflow({ operator: operator.address, poolIds: [] });
      return sendSweep({
        connection,
        feePayer: operator,
        tableAuthority: operator,
        instruction: { ...ix, accounts: [...ix.accounts!, ...remaining.map(readonly)] },
      });
    }

    async function poolAddresses(): Promise<Address[]> {
      return Promise.all((await everyPoolId(connection)).map(async (id) => (await derivePda(["pool", seedFromU16Le(id)])).address));
    }

    it("refuses every pool plus one more", async () => {
      const all = await poolAddresses();
      const { address: extra } = await derivePda(["pool", seedFromU16Le(MISNAMED_UNCONSOLIDATED_POOL_ID)]);
      const t04Before = await balanceOf(shared.treasury04);
      await expectAnchorError(sweepWith([...all, extra]), SWEEP_POOL_SET_INCOMPLETE);
      assert.equal(await balanceOf(shared.treasury04), t04Before);
    });

    it("refuses an account that is not a pool in a pool's place", async () => {
      const all = await poolAddresses();
      const { address: protocolConfig } = await derivePda(["protocol"]);
      await expectAnchorError(sweepWith([protocolConfig, ...all.slice(1)]), ACCOUNT_DISCRIMINATOR_MISMATCH);
    });

    it("refuses no pools at all", async () => {
      await expectAnchorError(sweepWith([]), SWEEP_POOL_SET_INCOMPLETE);
    });

    it("refuses an unprivileged wallet setting the ceiling", async () => {
      await expectAnchorError(
        sendIxs(connection, unprivileged, [await setT04Ceiling({ administrator: unprivileged.address, ceiling: 7n })]),
        UNAUTHORIZED,
      );
      assert.equal(await t04Ceiling(connection), 1n);
    });

    it("control: the same instruction with every pool once sweeps to the ceiling", async () => {
      await sweepWith(await poolAddresses());
      assert.equal(await balanceOf(shared.treasury04), 1n);
      await sendIxs(connection, admin, [await setT04Ceiling({ administrator: admin.address, ceiling: restore })]);
    });
  });
});
