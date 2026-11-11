// `activate_acquired`, and with it the branch of the instant sell that had never run.
//
// Named for the treasury rather than the instruction so it sorts after `bootstrap-init-authority`,
// whose whole subject is `init_protocol` against a ledger on which no protocol exists. This file's
// setup creates one, and the runner walks the directory in name order, so a name ahead of that one
// would take the bootstrap row's precondition away — which it detects and reports, but only after
// the fact.
//
// A sale that lands while a batch is open cannot enter the draw domain: the weights are frozen for
// the duration, so the position is created `Pending` and waits. Nothing could drive that here until
// the pipeline could open a batch at all, so both halves — the `Pending` sale and the activation
// that later completes it — arrive together in this file.
//
// What makes the activation its own instruction rather than a second door into `approve_deposit` is
// that it takes no attestation. Treasury 03 has already paid a rate against the value recorded at
// acceptance, so re-observing the card here would let it enter the pool at a weight the treasury
// never paid for — a mispricing with no failing guard and no wrong-looking event behind it. The
// value and the observation time are therefore read back off the position, and the only thing
// re-checked is the admission limits, because those are configuration and the wallet's own active
// value both move while the position waits.

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
import { admitPoolCollection, createPool, launchProfile } from "../helpers/pools.js";
import { assertEveryPoolIdle, drainTreasury04 } from "../helpers/treasuries.js";
import { ensureProtocol } from "../helpers/protocol.js";
import {
  createPnftCollection,
  mintPlainNonFungiblePnft,
  type MintedAsset,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { NO_POSITION, settleDrawnRoll } from "../helpers/roll.js";
import { VRF_QUEUE_FIXTURE } from "../helpers/vrf-queue.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { EXECUTED_ROWS, UNAUTHORIZED } from "../helpers/authority-matrix.js";
import { TOKEN_PROGRAM, VRF_PROGRAM } from "../../scripts/constants.js";
import { updateVrfConfig } from "../../scripts/lib/admin.js";
import { approveDeposit, deposit } from "../../scripts/lib/deposit.js";
import { activateAcquired, instantSell } from "../../scripts/lib/buyback.js";
import { commitRolls, testResolveBatch } from "../../scripts/lib/roll.js";
import { decodeEvent, decodePool, decodePosition, type DecodedEvent } from "../../scripts/lib/decoders.js";
import { derivePda, seedFromPubkey, seedFromU64Le, signer, writable } from "../../scripts/lib/anchor.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";

const PROGRAM_DATA_PREFIX = "Program data: ";
/** SPL Token `MintTo`: instruction 7, then the amount as a little-endian u64. */
function mintToData(amount: bigint): Uint8Array {
  const data = new Uint8Array(9);
  data[0] = 7;
  new DataView(data.buffer).setBigUint64(1, amount, true);
  return data;
}

const POSITION_SEED = "position";
const TREASURY_02_SEED = "t02";

/** Above `launchProfile`'s 12 USDC admission floor, and the figure Treasury 03 pays its rate on. */
const VALUE = 20_000_000n;
const OBSERVED_AT = 1_800_000_000n;

/** `RollBatchInFlight` — a batch is open, so no weight may move. */
const ROLL_BATCH_IN_FLIGHT = 6200;

/**
 * Anchor's `ConstraintSeeds`. The T02 rollup is seeded on `position.depositor`, so a position the
 * treasury does not own resolves a different rollup than any client derives, and the account set is
 * refused before the handler runs. The handler's own treasury check is the layer behind this one
 * and is unreachable from a client that derives its accounts; it is pinned in Rust instead.
 */
const CONSTRAINT_SEEDS = 2006;

/** `PositionNotPending` — the position has already left `Pending`. */
const POSITION_NOT_PENDING = 6400;

describe("activate_acquired — the deferred half of a sale made while the weights were frozen", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let roller: TransactionSigner;
  let seller: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let usdcMint: Address;
  let rollerUsdc: Address;
  let sellerUsdc: Address;
  let treasury03: Address;
  let protocolConfig: Address;
  let weightIndex: Address;
  let t02Identity: Address;
  let collection: PnftCollection;
  let batchId: bigint;
  /** The ordinary deposit that makes the pool rollable — never the treasury's. */
  let rollable: MintedAsset;
  /** The card sold while the batch was open. */
  let acquired: MintedAsset;
  /** A depositor's own untouched `Pending` position, for the treasury-only check. */
  let strangerPending: MintedAsset;

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  async function positionFor(mint: Address) {
    const { address } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(mint)]);
    return decodePosition((await fetchAccountData(connection, address))!);
  }

  after(async () => {
    await assertEveryPoolIdle(connection);
  });

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    roller = await fundedWallet(connection, 20);
    seller = await fundedWallet(connection, 20);

    const protocolState = await ensureProtocol(connection, admin);
    usdcMint = protocolState.usdcMint;
    poolId = protocolState.poolCounter;
    protocolConfig = protocolState.protocolConfig;

    await sendIxs(connection, admin, [
      await updateVrfConfig({
        administrator: admin.address,
        vrfProgram: VRF_PROGRAM,
        oracleQueue: VRF_QUEUE_FIXTURE,
        swapPool: SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: 100,
      }),
    ]);

    await connection.mintTokens(usdcMint, admin, 0n, seller.address, false);
    sellerUsdc = await getTokenAccountAddress(seller.address, usdcMint, false);
    await connection.mintTokens(usdcMint, admin, 1_000_000_000n, roller.address, false);
    rollerUsdc = await getTokenAccountAddress(roller.address, usdcMint, false);

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [
      addSignersToInstruction([admin, weightIndexSigner], createWiIx),
    ]);
    weightIndex = weightIndexSigner.address;

    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: { ...launchProfile() },
    });
    pool = poolAccounts.pool;
    ({ address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]));

    // Treasury 03 is the shared `PDA(["t03"])`, which `init_pool` creates when absent, authorised
    // by the ProtocolConfig PDA, so funding it is a bare `MintTo` against an account that exists.
    treasury03 = poolAccounts.treasury03;
    await sendIxs(connection, admin, [
      { programAddress: TOKEN_PROGRAM, accounts: [writable(usdcMint), writable(treasury03), signer(admin.address)], data: mintToData(1_000_000_000n) },
    ]);

    collection = await createPnftCollection({ connection, payer: admin, authority: admin });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
      standards: 0b011,
    });

    // One activated position, so `commit_rolls` has a non-empty draw domain and a full tier.
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

    // A second deposit left unapproved: a `Pending` position whose depositor is an ordinary
    // wallet, which is what the treasury-only check has to refuse.
    strangerPending = await mintPlainNonFungiblePnft({
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
        nftMint: strangerPending.mint,
        depositorToken: strangerPending.tokenAccount,
        metadata: strangerPending.metadata,
        masterEdition: strangerPending.masterEdition,
        depositorTokenRecord: null,
        positionTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
      }),
    ]);

    const state = decodePool((await fetchAccountData(connection, pool))!);
    batchId = state.batchCounter;
    await drainTreasury04({ connection, administrator: admin, operator });
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

  it("a sale that lands while a batch is open creates a Pending position, not an Active one", async () => {
    const before = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(before.openBatches, 1, "no batch is open, so this is the already-covered branch");

    acquired = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: seller.address,
      collection,
    });

    await sendIxs(connection, operator, [
      addSignersToInstruction(
        [seller, operator],
        await instantSell({
          seller: seller.address,
          operator: operator.address,
          poolId,
          collection: collection.mint,
          nftMint: acquired.mint,
          sellerToken: acquired.tokenAccount,
          sellerUsdc,
          weightIndex,
          metadata: acquired.metadata,
          masterEdition: acquired.masterEdition,
          sellerTokenRecord: null,
          positionTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
          // A Pending position ranks nothing, so there is no tier member for it to displace.
          displaced: null,
          loader: null,
          value: VALUE,
          observedAt: OBSERVED_AT,
        }),
      ),
    ]);

    const position = await positionFor(acquired.mint);
    assert.equal(position.state, "Pending", "the sale entered the draw domain while weights were frozen");
    assert.equal(position.depositor, t02Identity, "the position is not the treasury's");
    // Paid for at acceptance and stored there: the activation has no argument to take these from.
    assert.equal(position.recordedValue, VALUE);
    assert.equal(position.valueObservedAt, OBSERVED_AT);

    // The seller was paid regardless — the freeze defers ranking, not the purchase.
    const paid = (await connection.getTokenAccountBalance({ tokenAccount: sellerUsdc }))
      .amount as bigint;
    assert.ok(paid > 0n, "Treasury 03 did not pay for the card it took");

    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.nReal, before.nReal, "a Pending position entered the draw domain");
    assert.equal(after.wReal, before.wReal, "a Pending position moved weight");
  });

  it("the activation is refused while that same batch is still open", async () => {
    await expectAnchorError(
      sendIxs(connection, operator, [
        await activateAcquired({
          operator: operator.address,
          poolId,
          nftMint: acquired.mint,
          weightIndex,
          displaced: null,
          depositor: null,
        }),
      ]),
      ROLL_BATCH_IN_FLIGHT,
    );

    const position = await positionFor(acquired.mint);
    assert.equal(position.state, "Pending", "the refused activation moved the position anyway");
  });

  it("the batch resolves and settles, which is what lifts the freeze", async () => {
    await sendIxs(connection, operator, [
      await testResolveBatch({
        operator: operator.address,
        pool,
        batchId,
        randomness: new Uint8Array(32).fill(5),
      }),
    ]);
    const drawn = await settleDrawnRoll(
      {
        connection,
        operator,
        pool,
        protocolConfig,
        weightIndex,
        roller: roller.address,
        rollerUsdc,
        assets: [rollable],
      },
      batchId,
      0,
    );
    assert.ok(drawn === NO_POSITION || drawn === 0, "the settlement reported an asset it was not given");

    const reopened = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(reopened.openBatches, 0, "the freeze did not lift");
  });

  // Placed after the freeze lifts so the code returned is the only thing that could have refused
  // it: with a batch still open, a weight-frozen refusal would be indistinguishable from this one.
  //
  // Also the account-substitution sweep's `activate_acquired.position` row (a `Partial`-anchored
  // slot — registered in `tests/helpers/substitution-sweep.ts`): a stranger's own Pending
  // position substituted for the treasury's, same pool. The rejection is `wallet_stats`' own
  // seeds re-derivation off the substituted position's real depositor, not a check on `position`
  // itself.
  it("a depositor's own Pending position is refused — this door is the treasury's alone", async () => {
    const stranger = await positionFor(strangerPending.mint);
    assert.equal(stranger.state, "Pending", "the control position is not Pending");
    assert.notEqual(stranger.depositor, t02Identity);

    await expectAnchorError(
      sendIxs(connection, operator, [
        await activateAcquired({
          operator: operator.address,
          poolId,
          nftMint: strangerPending.mint,
          weightIndex,
          displaced: null,
          depositor: null,
        }),
      ]),
      CONSTRAINT_SEEDS,
    );
  });

  // Placed immediately before the activation it controls, so both run against the same position
  // in the same state and the only difference between them is who signed. A refusal on its own
  // would not distinguish an authority check from a position that had simply stopped being
  // activatable.
  it("a wallet that is not the Operator cannot activate the treasury's position", async () => {
    await expectAnchorError(
      sendIxs(connection, seller, [
        await activateAcquired({
          operator: seller.address,
          poolId,
          nftMint: acquired.mint,
          weightIndex,
          displaced: null,
          depositor: null,
        }),
      ]),
      UNAUTHORIZED,
    );

    const untouched = await positionFor(acquired.mint);
    assert.equal(untouched.state, "Pending", "the refused activation moved the position anyway");
  });

  it("the treasury's position activates on the value stored at acceptance", async () => {
    const before = decodePool((await fetchAccountData(connection, pool))!);

    const signature = await sendIxs(connection, operator, [
      await activateAcquired({
        operator: operator.address,
        poolId,
        nftMint: acquired.mint,
        weightIndex,
        displaced: null,
        depositor: null,
      }),
    ]);

    // The event carries the figure the handler actually ranked the card at. The position's own
    // fields cannot show that: they were written at acceptance and would still read correctly if
    // the activation had used some other number, so asserting them alone proves only that nothing
    // overwrote them.
    const activated = (await eventsOf(signature)).find((e) => e.name === "AcquiredActivated");
    assert.ok(activated, "the activation emitted no AcquiredActivated");
    assert.equal(activated.data.value, VALUE, "the activation ranked the card at a value nobody attested");
    assert.equal(activated.data.observedAt, OBSERVED_AT);

    const position = await positionFor(acquired.mint);
    assert.equal(position.state, "Active");
    // Unchanged across the activation, which is the whole point: no fresh observation happened,
    // so the pool ranks the card at exactly what Treasury 03 paid its rate against.
    assert.equal(position.recordedValue, VALUE);
    assert.equal(position.valueObservedAt, OBSERVED_AT);
    // A treasury-acquired position earns no season score, so it carries no lock.
    assert.equal(position.lockUntil, 0n);

    const after = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(after.nReal, before.nReal + 1, "the activation did not enter the draw domain");
    assert.ok(after.wReal > before.wReal, "the activation added no weight");
  });

  it("covers exactly the matrix rows this suite is answerable for", () => {
    // The structural link between the table and what ran here: this file owns one cell, and
    // deleting the row above must red rather than silently shrink the matrix.
    assert.deepEqual(
      EXECUTED_ROWS.filter((r) => r.suite === "treasury-activation").map(
        (r) => `${r.instruction}.${r.signer}`,
      ),
      ["activate_acquired.operator"],
    );
  });

  it("activating it a second time is refused — the position has left Pending", async () => {
    await expectAnchorError(
      sendIxs(connection, operator, [
        await activateAcquired({
          operator: operator.address,
          poolId,
          nftMint: acquired.mint,
          weightIndex,
          displaced: null,
          depositor: null,
        }),
      ]),
      POSITION_NOT_PENDING,
    );
  });
});
