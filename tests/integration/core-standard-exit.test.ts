// T-109: the three MPL-Core depositor exits, executed against a live validator.
//
// Every assertion these three carried before this file was static — TS ⇄ IDL in
// `tests/unit/{exit,idl-oracle,idl-vs-rust-accounts}.test.ts`, or Rust ⇄ source in each handler's
// own `mod tests`. Both are blind to dispatch: T-108 found `ReturnRejectedCore` declared,
// unit-tested and never delegated to from `lib.rs`, with 551 Rust tests green over it, and the
// same class is why P8's T-86 exists — eight encoders that had never run hid a writable-sentinel
// bug that passed three lead injections.
//
// It is deliberately T-103's three release paths with one standard changed, because the twins are
// *same, mirrored* (design §4.2.1) and a Core exit that needs a differently-shaped test is a Core
// exit that has drifted. T-103's other two cases do not carry over: both are pNFT-account-shape
// rejections, and this family has no such accounts for a caller to wrongly supply. What the mirror image cannot carry over is the custody
// assertion itself: there is no vault token account to read a balance from on this family, so
// release is `AssetV1.owner` returning to the depositor and the vault PDA **still** not existing
// afterwards — the accountless escrow, asserted on the way out as `core-standard-deposit.test.ts`
// asserts it on the way in.
//
// `close_seized` is P9's fourth new exit-domain instruction and is not here: it moves no custody,
// and reaching its terminal needs a card seized out of the vault, which needs a fixture with a
// permanent plugin. See `core-seizure.test.ts`.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import {
  assertProgramIsLive,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createCoreCollection, mintCoreAsset, readCoreAsset } from "../helpers/core-fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { approveDeposit, depositCore, rejectDeposit, returnRejectedCore } from "../../scripts/lib/deposit.js";
import { claimNftCore, withdrawCore } from "../../scripts/lib/exit.js";
import { beginSweep, endSweep, recordValue } from "../../scripts/lib/value.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { decodePosition } from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";

const PROFILE = launchProfile("11111111111111111111111111111111" as Address);
const ADMISSION_FLOOR = PROFILE.admissionFloor;
const TICKET_TARGET = PROFILE.ticketTarget;
const ADMITTED_VALUE = 20_000_000n;
const BELOW_FLOOR_VALUE = 1_000_000n;

const REJECT_REASON = 1;
/** `Position.standard`'s Core discriminant, verbatim against `common/standards.rs`. */
const STANDARD_CORE = 2;

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

describe("T-109 — the standard-2 depositor exits against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
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

    const core = await createCoreCollection({ connection, payer: admin, name: "T-109 core exits" });
    collection = core.collection;
    collectionAuthority = core.authority;

    // Core only. The wrong-pairing rejections belong to `core-standard-deposit.test.ts`; what
    // this file needs from the record is an admitted intake, once, for every case.
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

  /** Mints one genuine `AssetV1` in the admitted collection and escrows it — every case starts
   * here, and the escrow half is asserted so a case that later fails to release cannot be a case
   * whose fixture never arrived. */
  async function depositCoreAsset(): Promise<Address> {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection,
      collectionAuthority,
      owner: depositor.address,
    });
    await sendIxs(connection, depositor, [
      await depositCore({ depositor: depositor.address, poolId, collection, asset }),
    ]);
    const { position, positionVault } = await positionPdas(asset);
    const escrowed = await readCoreAsset(connection, asset);
    assert.ok(escrowed, "the fixture asset vanished during the deposit");
    assert.equal(escrowed.owner, positionVault, "the fixture did not reach escrow");
    const recorded = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(recorded.standard, STANDARD_CORE, "the intake stored the wrong standard");
    return asset;
  }

  /**
   * The card is back with its depositor, the position is closed, and the vault still does not
   * exist.
   *
   * The third assertion is the one the legacy twin cannot make and the reason this is not a
   * copy: `assertReleased` there reads a *token account* balance and then requires the vault to
   * have been closed. Here the vault was never allocated at all, so its continued absence after
   * a release signed under its own seeds is what says the escrow stayed accountless through both
   * legs — a lazily created vault would leave every Core position paying rent the design says it
   * does not, and closable by whoever funded it.
   */
  async function assertReleased(asset: Address) {
    const released = await readCoreAsset(connection, asset);
    assert.ok(released, "the asset account must survive its own release");
    assert.equal(released.owner, depositor.address, "the Core asset did not return to its depositor");
    const { position, positionVault } = await positionPdas(asset);
    assert.equal(await fetchAccountData(connection, position), null, "the position is still open");
    assert.equal(
      await fetchAccountData(connection, positionVault),
      null,
      "the vault was allocated somewhere in the exit — the escrow is no longer accountless",
    );
  }

  it("return_rejected_core releases a standard-2 card, and it is permissionless in doing so", async () => {
    const asset = await depositCoreAsset();
    await sendIxs(connection, operator, [
      await rejectDeposit({
        operator: operator.address,
        pool,
        nftMint: asset,
        reason: REJECT_REASON,
      }),
    ]);

    // Signed by a wallet that is neither the depositor nor any protocol authority. The
    // permissionless shape is the whole of `payer` ≠ `depositor` on this handler, and it is the
    // one instruction of the four that T-108's crate-wide enumerator found unwired — declared,
    // unit-tested and undispatchable for two rows.
    const stranger = await fundedWallet(connection, 5);
    const { position } = await positionPdas(asset);
    // **`"confirmed"` on all three reads is load-bearing, not decoration.** solana-kite's
    // `getLamportBalance` defaults to `finalized` while the send path returns on confirmation, so
    // the default reads both sides of the transaction from before it landed: measured here, the
    // pair came back identical *and* the fee-paying stranger's balance was still its exact
    // airdrop, which is impossible after a transaction it signed. A lamport assertion written
    // without this argument therefore compares two stale reads and reports "nothing moved" — it
    // passes for any handler that pays nobody, which is precisely the defect it is here to catch.
    const rent = await connection.getLamportBalance(position, "confirmed");
    const depositorBefore = await connection.getLamportBalance(depositor.address, "confirmed");

    await sendIxs(connection, stranger, [
      await returnRejectedCore({
        payer: stranger.address,
        depositor: depositor.address,
        pool,
        asset,
        collection,
      }),
    ]);
    await assertReleased(asset);

    // **The rent is the second thing a permissionless caller could take, and `close = depositor`
    // is all that stops them.** The card is protected by the handler pinning `new_owner` to the
    // position's recorded depositor; the position's own lamports are protected only by which
    // account the `close` names, and a `close = payer` would pass every custody assertion above.
    // Exact equality is affordable here precisely because the depositor did not sign: the
    // stranger paid the fee, so the only lamports that moved into this wallet are the rent.
    assert.equal(
      await connection.getLamportBalance(depositor.address, "confirmed"),
      depositorBefore + rent,
      "the position's rent did not land with its depositor",
    );
  });

  it("withdraw_core releases a standard-2 card from Active", async () => {
    const asset = await depositCoreAsset();
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
        lockUntil: 0n,
      }),
    ]);
    await sendIxs(connection, depositor, [
      await withdrawCore({
        depositor: depositor.address,
        poolId,
        asset,
        collection,
        weightIndex,
        // Structurally zero for the whole of slice 1, and `null` here is the polarity case: the
        // slot encodes as this program's own id and must take the readonly role, because marking
        // the invoked program writable is refused at the transaction-format level before any
        // handler runs (T-48).
        depositorUsdc: null,
        successor: null,
      }),
    ]);
    await assertReleased(asset);
  });

  it("claim_nft_core releases a standard-2 card from ClosedBelowFloor", async () => {
    const asset = await depositCoreAsset();
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
        lockUntil: 0n,
      }),
    ]);

    // One sweep at a below-floor value closes the position and leaves the card escrowed. Unlike
    // the legacy twin, `Seized` is *also* live on this standard — it is `close_seized`'s terminal
    // and `claim_nft_core` is the only instruction that can serve it — and it is reached in
    // `core-seizure.test.ts` rather than here, because it needs a seizure to exist first.
    await sendIxs(connection, operator, [await beginSweep({ operator: operator.address, poolId })]);
    await sendIxs(connection, operator, [
      await recordValue({
        operator: operator.address,
        poolId,
        nftMint: asset,
        depositor: depositor.address,
        weightIndex,
        value: BELOW_FLOOR_VALUE,
        observedAt: nowSeconds(),
        effect: { kind: "close", successor: null },
      }),
    ]);
    await sendIxs(connection, operator, [await endSweep({ operator: operator.address, poolId })]);

    const { position, positionVault } = await positionPdas(asset);
    const closed = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(closed.state, "ClosedBelowFloor", "the sweep did not close the position");
    const stillEscrowed = await readCoreAsset(connection, asset);
    assert.ok(stillEscrowed);
    assert.equal(
      stillEscrowed.owner,
      positionVault,
      "design §3.4: a below-floor close keeps the card until the claim, so the claim below has " +
        "something to release",
    );

    await sendIxs(connection, depositor, [
      await claimNftCore({ depositor: depositor.address, pool, asset, collection }),
    ]);
    await assertReleased(asset);
  });
});
