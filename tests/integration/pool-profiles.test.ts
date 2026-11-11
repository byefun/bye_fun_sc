// Integration coverage for tests/helpers/pools.ts's three `init_pool` profiles. `limitsOnProfile`,
// `smallTierProfile` and `POOL_PROFILES` shipped with zero references anywhere outside pools.ts, so nothing on this tree
// had ever actually exercised them. This file creates all three profiles as three live pools
// (pools.ts's own ≥2-live-pool precondition) at distinct `poolId`s, and asserts — by enumeration,
// keyed on each pool's own decoded `PoolInitialized` event, never a test name or a registration
// call — that every profile actually reached `init_pool` with its own distinguishing values
// intact.
//
// The distinguishing key is `(tierSize, admissionCeiling, walletValueCap)`: Launch and Small-tier
// share the first two (0, 0) and differ only in `tierSize` (20 vs 3); Limits-on shares `tierSize`
// with Launch and differs only in the ceiling/cap. No single field distinguishes all three — the
// triple is the smallest observable that does. A profile imported but never sent to `init_pool`
// would leave its key entirely absent from the decoded set below, which is exactly the failure
// this file exists to catch.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import {
  assertProgramIsLive,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import {
  admitPoolCollection,
  createPool,
  launchProfile,
  limitsOnProfile,
  smallTierProfile,
  POOL_PROFILES,
  type PoolConfigArgs,
  type PoolProfileName,
} from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createPnftCollection, mintPlainNonFungiblePnft, type PnftCollection } from "../helpers/fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { deposit, approveDeposit } from "../../scripts/lib/deposit.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { decodeEvent, decodePosition, type DecodedEvent } from "../../scripts/lib/decoders.js";
import { addSignersToInstruction } from "@solana/kit";

const POSITION_SEED = "position";

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

function isPoolInitialized(
  event: DecodedEvent,
): event is Extract<DecodedEvent, { name: "PoolInitialized" }> {
  return event.name === "PoolInitialized";
}

const PROGRAM_DATA_PREFIX = "Program data: ";

type ProfileSignature = { tierSize: number; admissionCeiling: bigint; walletValueCap: bigint };

function signatureOf(args: PoolConfigArgs): ProfileSignature {
  return {
    tierSize: args.tierSize,
    admissionCeiling: args.admissionCeiling,
    walletValueCap: args.walletValueCap,
  };
}

function keyOf(sig: ProfileSignature): string {
  return `${sig.tierSize}:${sig.admissionCeiling}:${sig.walletValueCap}`;
}

/** Decodes the *sole* `PoolInitialized` event out of one `init_pool` transaction's own logs — two
 * or zero would mean this signature is not what this test thinks it is. */
async function decodePoolInitializedFrom(
  connection: Connection,
  signature: string,
): Promise<ProfileSignature> {
  const logs = await connection.getLogs(signature);
  const events = logs
    .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
    .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")))
    .filter(isPoolInitialized);
  assert.equal(
    events.length,
    1,
    `expected exactly one PoolInitialized event in ${signature}'s logs, found ${events.length}`,
  );
  const { tierSize, admissionCeiling, walletValueCap } = events[0].data;
  return { tierSize, admissionCeiling, walletValueCap };
}

describe("every init_pool profile is actually exercised", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  let collection: PnftCollection;
  const decoded = new Map<PoolProfileName, ProfileSignature>();
  const pools = new Map<PoolProfileName, { pool: Address; poolId: number; weightIndex: Address }>();

  async function positionOf(pool: Address, nftMint: Address): Promise<Address> {
    const { address } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)]);
    return address;
  }

  /** Mints, deposits and approves one legacy asset into `profile`'s pool at `value`. */
  async function depositAndApprove(profile: PoolProfileName, value: bigint): Promise<Address> {
    const { poolId, weightIndex } = pools.get(profile)!;
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection,
    });
    await sendIxs(connection, depositor, [
      await deposit({
        depositor: depositor.address,
        poolId,
        collection: collection.mint,
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
    await sendIxs(connection, operator, [
      await approveDeposit({
        operator: operator.address,
        poolId,
        nftMint: asset.mint,
        depositor: depositor.address,
        weightIndex,
        displaced: null,
        value,
        observedAt: nowSeconds(),
        lockUntil: 0n,
      }),
    ]);
    return asset.mint;
  }

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();

    const { usdcMint, poolCounter, operator: op } = await ensureProtocol(connection, admin);
    operator = op;
    depositor = await fundedWallet(connection, 20);

    const names = Object.keys(POOL_PROFILES) as PoolProfileName[];
    for (let i = 0; i < names.length; i++) {
      const name = names[i];
      const args = POOL_PROFILES[name]();
      const poolId = poolCounter + i;

      const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
        rpc: connection,
        payer: admin.address,
      });
      await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], createWiIx)]);

      const poolAccounts = await createPool({
        connection,
        administrator: admin,
        poolId,
        weightIndex: weightIndexSigner.address,
        usdcMint,
        args,
      });

      decoded.set(name, await decodePoolInitializedFrom(connection, poolAccounts.signature));
      pools.set(name, { pool: poolAccounts.pool, poolId, weightIndex: weightIndexSigner.address });
    }

    // The residual coverage needs live deposits into the small-tier and limits-on profiles, which needs
    // an admitted collection on each — `init_pool` admits nothing itself.
    collection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "pool-profiles fixtures",
    });
    for (const name of ["smallTier", "limitsOn"] as const) {
      const { poolId } = pools.get(name)!;
      await admitPoolCollection({ connection, administrator: admin, poolId, collection: collection.mint, standards: 0b011 });
    }
  });

  it("Launch profile's PoolInitialized carries tierSize 20 and ceiling/cap 0", () => {
    assert.deepEqual(decoded.get("launch"), signatureOf(launchProfile()));
  });

  it("Limits-on profile's PoolInitialized carries its own nonzero ceiling and wallet cap", () => {
    assert.deepEqual(decoded.get("limitsOn"), signatureOf(limitsOnProfile()));
  });

  it("Small-tier profile's PoolInitialized carries tierSize 3, reachable without 20+ fixtures", () => {
    assert.deepEqual(decoded.get("smallTier"), signatureOf(smallTierProfile()));
  });

  it("all three profiles are pairwise distinct on-chain — the enumeration itself, keyed on the decoded event", () => {
    const observedKeys = [...decoded.values()].map(keyOf);
    const distinct = new Set(observedKeys);
    assert.equal(
      distinct.size,
      3,
      `expected exactly 3 distinct profile signatures, observed ${distinct.size}: ${observedKeys.join(", ")}`,
    );
  });

  // The residual coverage: the four cases above prove every profile reached init_pool with the right
  // shape, but nothing deposited into any of them — so nothing this file did could reach tier
  // fill or demotion, and nothing probed the admission limits' inclusive boundary, which limitsOnProfile exists
  // for (tests/helpers/pools.ts's own comment on it).
  it("small-tier: reaches tier fill and a real demotion, not just three distinct pools", async () => {
    const { pool } = pools.get("smallTier")!;
    // tierSize is 3. Three ascending values fill it one at a time (each insertion has room); the
    // fourth outranks the current weakest (10M) and swaps it out — the natural eviction path,
    // distinct from unexecuted-encoders.test.ts's update_tier coverage, which promotes a
    // previously-rejected waiting candidate rather than displacing a seated member.
    const weakest = await depositAndApprove("smallTier", 15_000_000n);
    await depositAndApprove("smallTier", 20_000_000n);
    await depositAndApprove("smallTier", 30_000_000n);

    const weakestPosition = await positionOf(pool, weakest);
    assert.equal(
      decodePosition((await fetchAccountData(connection, weakestPosition))!).inTier,
      true,
      "the fixture assumption failed: the weakest of the three must be seated before the swap",
    );

    const challenger = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection,
    });
    const { poolId, weightIndex } = pools.get("smallTier")!;
    await sendIxs(connection, depositor, [
      await deposit({
        depositor: depositor.address,
        poolId,
        collection: collection.mint,
        nftMint: challenger.mint,
        depositorToken: challenger.tokenAccount,
        metadata: challenger.metadata,
        masterEdition: challenger.masterEdition,
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
        nftMint: challenger.mint,
        depositor: depositor.address,
        weightIndex,
        displaced: weakestPosition,
        value: 25_000_000n,
        observedAt: nowSeconds(),
        lockUntil: 0n,
      }),
    ]);

    const challengerPosition = await positionOf(pool, challenger.mint);
    assert.equal(
      decodePosition((await fetchAccountData(connection, challengerPosition))!).inTier,
      true,
      "the challenger did not enter the tier",
    );
    assert.equal(
      decodePosition((await fetchAccountData(connection, weakestPosition))!).inTier,
      false,
      "the weakest member was not demoted",
    );
  });

  it("limits-on: the admission floor is inclusive — a deposit valued exactly at it is admitted", async () => {
    const floor = limitsOnProfile().admissionFloor;
    const asset = await depositAndApprove("limitsOn", floor);
    const { pool } = pools.get("limitsOn")!;
    const decodedPosition = decodePosition((await fetchAccountData(connection, await positionOf(pool, asset)))!);
    assert.equal(decodedPosition.state, "Active", "a deposit valued exactly at the floor was refused");
  });

  it("limits-on: the admission ceiling is inclusive — a deposit valued exactly at it is admitted", async () => {
    const ceiling = limitsOnProfile().admissionCeiling;
    const asset = await depositAndApprove("limitsOn", ceiling);
    const { pool } = pools.get("limitsOn")!;
    const decodedPosition = decodePosition((await fetchAccountData(connection, await positionOf(pool, asset)))!);
    assert.equal(decodedPosition.state, "Active", "a deposit valued exactly at the ceiling was refused");
  });
});
