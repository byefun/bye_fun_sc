// The depositor escrow — `open_escrow`, `escrow_deposit`, `escrow_withdraw`, `set_escrow_armed`
// and `claim_fee_to_escrow` against a live validator.
//
// What this file proves, each part of which fails differently:
//
//   - **creation** — the owner pays for the escrow and its vault, the escrow starts disarmed, the
//     vault's authority is the escrow PDA, and a second open is refused. Open, deposit and arm fit
//     in one transaction under one signature.
//   - **the balance is the vault** — every `balance_after` an escrow event reports equals the
//     vault's token amount read back afterwards, across a seeded random run of deposits and
//     withdrawals, including withdrawals above the balance that must move nothing.
//   - **the owner can always leave** — with both pause flags set and a roll batch open on a pool,
//     the full balance withdraws; one micro-USDC more is refused and nothing moves.
//   - **only the owner arms** — a stranger is refused by the seeds on every escrow instruction,
//     each refusal paired with the owner's own call succeeding; an empty escrow can be armed, and
//     withdrawing to zero leaves it armed.
//   - **fees into the escrow** — after a real roll settles, a fee claim lands in the vault and
//     the wallet's USDC does not move, with `FeeClaimed` then `EscrowDeposited(Fees)`.

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import {
  assertProgramIsLive,
  expectAnchorError,
  expectProgramError,
  fundedWallet,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  withSigner,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { admitPoolCollection, createPool, smallTierProfile } from "../helpers/pools.js";
import { assertEveryPoolIdle } from "../helpers/treasuries.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createPnftCollection, mintPlainNonFungiblePnft, type MintedAsset } from "../helpers/fixtures.js";
import { NO_POSITION, settleDrawnRoll } from "../helpers/roll.js";
import { VRF_QUEUE_FIXTURE } from "../helpers/vrf-queue.js";
import { CONSTRAINT_SEEDS, EXECUTED_ROWS, NOT_POSITION_OWNER } from "../helpers/authority-matrix.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { SYSTEM_PROGRAM, VRF_PROGRAM } from "../../scripts/constants.js";
import { setPause, updateVrfConfig } from "../../scripts/lib/admin.js";
import { approveDeposit, deposit } from "../../scripts/lib/deposit.js";
import { commitRolls, testResolveBatch } from "../../scripts/lib/roll.js";
import {
  claimFeeToEscrow,
  deriveEscrow,
  escrowDeposit,
  escrowWithdraw,
  openEscrow,
  setEscrowArmed,
} from "../../scripts/lib/escrow.js";
import { decodeEscrow, decodeEvent, decodePool, decodePosition, type DecodedEvent } from "../../scripts/lib/decoders.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { SWAP_POOL_FIXTURE } from "../../scripts/lib/swap-fixtures.js";
import { expandDraw } from "../../scripts/verify-batch.js";

const PROGRAM_DATA_PREFIX = "Program data: ";

const ESCROW_AMOUNT_ZERO = 6800;
const ESCROW_BALANCE_SHORT = 6801;
/** System program `AccountAlreadyInUse` — what `init` raises for a second `open_escrow`. */
const ACCOUNT_ALREADY_IN_USE = 0;

const USDC = 1_000_000n;
/** `smallTierProfile`'s tier holds 3. */
const POSITIONS = 3;
/** `common/constants.rs`'s `WEIGHT_C`: a value's draw weight is `WEIGHT_C / value`. */
const WEIGHT_C = 1_000_000_000_000_000_000n;

/**
 * The first candidate randomness whose roll 0 lands on a card, computed the way `settle_roll`
 * draws: `r = expand(randomness, 0, W)` over `W = w_real + Σ blanks[i] · WEIGHT_C / face[i]` (slots
 * whose face is 0 skipped),
 * blanks occupying `[0, blank weight)` and cards everything above. A blank against this fixture's
 * empty Treasury 04 is refunded, and a refund books no fee, so the fee claim needs a card draw.
 * The caller asserts the settlement really drew one, which checks this prediction on chain.
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

/** SplitMix64, so a failing seed reproduces exactly. */
function rng(seed: bigint) {
  let state = seed;
  const MASK = (1n << 64n) - 1n;
  return (bound: bigint): bigint => {
    state = (state + 0x9e3779b97f4a7c15n) & MASK;
    let z = state;
    z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & MASK;
    z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & MASK;
    z ^= z >> 31n;
    return bound === 0n ? 0n : z % bound;
  };
}

describe("escrow — the depositor's USDC escrow", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let owner: TransactionSigner;
  let stranger: TransactionSigner;
  let usdcMint: Address;
  let ownerUsdc: Address;
  let escrow: Address;
  let escrowVault: Address;

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

  async function lamportsOf(account: Address): Promise<bigint> {
    const { value } = await connection.rpc.getBalance(account).send();
    return BigInt(value);
  }

  async function escrowOf(address: Address) {
    return decodeEscrow((await fetchAccountData(connection, address))!);
  }

  /** A funded wallet holding `usdc` micro-USDC in its own token account. */
  async function walletWithUsdc(usdc: bigint): Promise<{ wallet: TransactionSigner; usdcAccount: Address }> {
    const wallet = await fundedWallet(connection, 5);
    await connection.mintTokens(usdcMint, admin, usdc, wallet.address, false);
    return { wallet, usdcAccount: await getTokenAccountAddress(wallet.address, usdcMint, false) };
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

    ({ wallet: owner, usdcAccount: ownerUsdc } = await walletWithUsdc(1_000n * USDC));
    stranger = await fundedWallet(connection, 5);
    ({ escrow, escrowVault } = await deriveEscrow(owner.address));
  });

  describe("open_escrow", () => {
    it("creates a disarmed escrow and an empty vault the escrow owns, paid for by the owner", async () => {
      const lamportsBefore = await lamportsOf(owner.address);
      const signature = await sendIxs(connection, owner, [
        await openEscrow({ owner: owner.address, usdcMint }),
      ]);

      const account = await escrowOf(escrow);
      assert.equal(account.owner, owner.address);
      assert.equal(account.armed, false, "a new escrow must start disarmed");
      assert.deepEqual(Array.from(account.reserved), new Array(64).fill(0));
      assert.equal(await balanceOf(escrowVault), 0n);

      const vault = await connection.rpc.getTokenAccountBalance(escrowVault).send();
      assert.equal(vault.value.amount, "0");
      const { value: vaultInfo } = await connection.rpc
        .getAccountInfo(escrowVault, { encoding: "jsonParsed" })
        .send();
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const parsed = (vaultInfo!.data as any).parsed.info;
      assert.equal(parsed.owner, escrow, "the vault's authority must be the escrow PDA");
      assert.equal(parsed.mint, usdcMint);

      const escrowRent = await lamportsOf(escrow);
      const vaultRent = await lamportsOf(escrowVault);
      assert.ok(
        lamportsBefore - (await lamportsOf(owner.address)) >= escrowRent + vaultRent,
        "the owner did not pay the rent for both accounts",
      );

      const events = await eventsOf(signature);
      assert.deepEqual(
        events.map((e) => e.name),
        ["EscrowOpened"],
      );
      const opened = events[0]!.data as { escrow: Address; owner: Address };
      assert.equal(opened.escrow, escrow);
      assert.equal(opened.owner, owner.address);
    });

    it("refuses a second escrow for the same wallet", async () => {
      await expectProgramError(
        sendIxs(connection, owner, [await openEscrow({ owner: owner.address, usdcMint })]),
        SYSTEM_PROGRAM,
        ACCOUNT_ALREADY_IN_USE,
      );
    });

    it("opens, deposits 10 USDC and arms in one transaction under one signature", async () => {
      const { wallet, usdcAccount } = await walletWithUsdc(10n * USDC);
      const pdas = await deriveEscrow(wallet.address);
      const signature = await sendIxs(connection, wallet, [
        await openEscrow({ owner: wallet.address, usdcMint }),
        await escrowDeposit({ owner: wallet.address, ownerUsdc: usdcAccount, amount: 10n * USDC }),
        await setEscrowArmed({ owner: wallet.address, armed: true }),
      ]);

      assert.equal((await escrowOf(pdas.escrow)).armed, true);
      assert.equal(await balanceOf(pdas.escrowVault), 10n * USDC);
      assert.equal(await balanceOf(usdcAccount), 0n);
      assert.deepEqual(
        (await eventsOf(signature)).map((e) => e.name),
        ["EscrowOpened", "EscrowDeposited", "EscrowArmed"],
      );
    });
  });

  describe("escrow_deposit", () => {
    it("accepts any amount — the 10 USDC minimum is the web's to enforce, not the program's", async () => {
      const before = await balanceOf(escrowVault);
      const signature = await sendIxs(connection, owner, [
        await escrowDeposit({ owner: owner.address, ownerUsdc, amount: 9_990_000n }),
      ]);
      assert.equal(await balanceOf(escrowVault), before + 9_990_000n);

      const [deposited] = (await eventsOf(signature)).filter((e) => e.name === "EscrowDeposited");
      const data = deposited!.data as {
        amount: bigint;
        source: string;
        position: Address;
        positionPool: Address;
        balanceAfter: bigint;
      };
      assert.equal(data.amount, 9_990_000n);
      assert.equal(data.source, "Wallet");
      assert.equal(data.position, SYSTEM_PROGRAM, "a wallet deposit names no position");
      assert.equal(data.positionPool, SYSTEM_PROGRAM);
      assert.equal(data.balanceAfter, await balanceOf(escrowVault));
    });

    it("leaves arming exactly as it was", async () => {
      const armedBefore = (await escrowOf(escrow)).armed;
      await sendIxs(connection, owner, [await escrowDeposit({ owner: owner.address, ownerUsdc, amount: USDC })]);
      assert.equal((await escrowOf(escrow)).armed, armedBefore);
    });
  });

  describe("escrow_withdraw", () => {
    it("refuses a zero amount at 6800", async () => {
      await expectAnchorError(
        sendIxs(connection, owner, [await escrowWithdraw({ owner: owner.address, ownerUsdc, amount: 0n })]),
        ESCROW_AMOUNT_ZERO,
      );
    });

    it("refuses one micro-USDC above the balance at 6801, and nothing moves", async () => {
      const vaultBefore = await balanceOf(escrowVault);
      const walletBefore = await balanceOf(ownerUsdc);
      await expectAnchorError(
        sendIxs(connection, owner, [
          await escrowWithdraw({ owner: owner.address, ownerUsdc, amount: vaultBefore + 1n }),
        ]),
        ESCROW_BALANCE_SHORT,
      );
      assert.equal(await balanceOf(escrowVault), vaultBefore);
      assert.equal(await balanceOf(ownerUsdc), walletBefore);
    });
  });

  describe("set_escrow_armed", () => {
    it("arms an empty escrow, and withdrawing to zero leaves it armed", async () => {
      const { wallet, usdcAccount } = await walletWithUsdc(5n * USDC);
      const pdas = await deriveEscrow(wallet.address);
      await sendIxs(connection, wallet, [await openEscrow({ owner: wallet.address, usdcMint })]);

      await sendIxs(connection, wallet, [await setEscrowArmed({ owner: wallet.address, armed: true })]);
      assert.equal((await escrowOf(pdas.escrow)).armed, true, "an empty escrow must be armable");

      await sendIxs(connection, wallet, [
        await escrowDeposit({ owner: wallet.address, ownerUsdc: usdcAccount, amount: 5n * USDC }),
      ]);
      await sendIxs(connection, wallet, [
        await escrowWithdraw({ owner: wallet.address, ownerUsdc: usdcAccount, amount: 5n * USDC }),
      ]);
      assert.equal(await balanceOf(pdas.escrowVault), 0n);
      assert.equal((await escrowOf(pdas.escrow)).armed, true, "withdrawing to zero must not disarm");
    });

    it("writes the current value again without an event", async () => {
      const current = (await escrowOf(escrow)).armed;
      const signature = await sendIxs(connection, owner, [
        await setEscrowArmed({ owner: owner.address, armed: current }),
      ]);
      assert.equal((await escrowOf(escrow)).armed, current);
      assert.deepEqual(await eventsOf(signature), []);
    });

    it("emits EscrowArmed carrying the new value when it changes", async () => {
      const next = !(await escrowOf(escrow)).armed;
      const signature = await sendIxs(connection, owner, [
        await setEscrowArmed({ owner: owner.address, armed: next }),
      ]);
      const events = await eventsOf(signature);
      assert.deepEqual(
        events.map((e) => e.name),
        ["EscrowArmed"],
      );
      assert.equal((events[0]!.data as { armed: boolean }).armed, next);
    });
  });

  // ── Seeded random runs ─────────────────────────────────────────────────────────────────────
  describe("properties", () => {
    // Deposits, withdrawals inside and above the balance, and zero withdrawals, in a seeded
    // order. After every step the vault is read back: it never goes below zero, a refused step
    // moves nothing, and every escrow event's `balance_after` is the vault's amount.
    it("the vault never goes negative, refusals move nothing, and balance_after is the vault", async () => {
      const next = rng(62n);
      let model = await balanceOf(escrowVault);
      let refused = 0;
      let accepted = 0;
      for (let step = 0; step < 24; step += 1) {
        const kind = next(3n);
        if (kind === 0n) {
          const amount = next(20n * USDC);
          const signature = await sendIxs(connection, owner, [
            await escrowDeposit({ owner: owner.address, ownerUsdc, amount }),
          ]);
          model += amount;
          const [event] = await eventsOf(signature);
          assert.equal((event!.data as { balanceAfter: bigint }).balanceAfter, model, `step ${step}`);
          accepted += 1;
        } else {
          const amount = kind === 1n ? next(model + 1n) : model + 1n + next(5n * USDC);
          const call = sendIxs(connection, owner, [await escrowWithdraw({ owner: owner.address, ownerUsdc, amount })]);
          if (amount === 0n || amount > model) {
            await expectAnchorError(call, amount === 0n ? ESCROW_AMOUNT_ZERO : ESCROW_BALANCE_SHORT);
            refused += 1;
          } else {
            const [event] = await eventsOf(await call);
            model -= amount;
            assert.equal((event!.data as { balanceAfter: bigint }).balanceAfter, model, `step ${step}`);
            accepted += 1;
          }
        }
        assert.equal(await balanceOf(escrowVault), model, `vault drifted from the model at step ${step}`);
      }
      assert.ok(refused > 0 && accepted > 0, `${accepted} accepted, ${refused} refused`);
    });

    // A stranger and the owner alternate arm requests in a seeded order. Every stranger request
    // fails at the seeds and leaves `armed` where it was; every owner request lands.
    it("only the owner's requests ever change armed", async () => {
      const next = rng(66n);
      let expected = (await escrowOf(escrow)).armed;
      for (let step = 0; step < 12; step += 1) {
        const armed = next(2n) === 1n;
        const ix = await setEscrowArmed({ owner: owner.address, armed });
        if (next(2n) === 0n) {
          await expectAnchorError(
            sendIxs(connection, stranger, [withSigner(ix, stranger.address)]),
            CONSTRAINT_SEEDS,
          );
        } else {
          await sendIxs(connection, owner, [ix]);
          expected = armed;
        }
        assert.equal((await escrowOf(escrow)).armed, expected, `step ${step}`);
      }
    });
  });

  // ── A live pool: an open batch, both pauses, then a real fee claim ───────────────────────────
  describe("against a pool with a batch open and both pauses set", () => {
    let poolId: number;
    let pool: Address;
    let weightIndex: Address;
    let protocolConfig: Address;
    let batchId: bigint;
    const assets: MintedAsset[] = [];
    /** A card the settled roll did not draw: still the owner's `Active` position. */
    let survivorMint: Address;

    before(async () => {
      const protocolState = await ensureProtocol(connection, admin);
      poolId = protocolState.poolCounter;
      protocolConfig = protocolState.protocolConfig;

      // `commit_rolls` pins `oracle_queue` to the config, which the bootstrap leaves at a
      // placeholder; the other three fields are passed unchanged.
      await sendIxs(connection, admin, [
        await updateVrfConfig({
          administrator: admin.address,
          vrfProgram: VRF_PROGRAM,
          oracleQueue: VRF_QUEUE_FIXTURE,
          swapPool: SWAP_POOL_FIXTURE,
          maxSwapSlippageBps: 100,
        }),
      ]);

      const { signer: weightIndexSigner, instruction } = await createWeightIndexAccount({
        rpc: connection,
        payer: admin.address,
      });
      await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], instruction)]);
      weightIndex = weightIndexSigner.address;
      pool = (
        await createPool({ connection, administrator: admin, poolId, weightIndex, usdcMint, args: smallTierProfile() })
      ).pool;

      const collection = await createPnftCollection({ connection, payer: admin, authority: admin });
      await admitPoolCollection({ connection, administrator: admin, poolId, collection: collection.mint, standards: 0b011 });

      // The owner deposits every card and rolls, so the settlement helper's single-wallet
      // assumption holds and every surviving position is the owner's.
      for (let i = 0; i < POSITIONS; i += 1) {
        const asset = await mintPlainNonFungiblePnft({
          connection,
          payer: admin,
          authority: admin,
          owner: owner.address,
          collection,
        });
        assets.push(asset);
        await sendIxs(connection, owner, [
          await deposit({
            depositor: owner.address,
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
            depositor: owner.address,
            weightIndex,
            displaced: null,
            value: 20n * USDC,
            observedAt: BigInt(Math.floor(Date.now() / 1000)),
            lockUntil: 0n,
          }),
        ]);
      }

      batchId = decodePool((await fetchAccountData(connection, pool))!).batchCounter;
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
      await sendIxs(connection, admin, [
        await setPause({ administrator: admin.address, poolId, deposits: true, rolls: true, reason: 1 }),
      ]);
    });

    it("withdraws the owner's full balance while the pool is paused both ways with a batch open", async () => {
      const state = decodePool((await fetchAccountData(connection, pool))!);
      assert.equal(state.depositsPaused, true);
      assert.equal(state.rollsPaused, true);
      assert.equal(state.openBatches, 1, "the batch is not open — the precondition is void");

      await sendIxs(connection, owner, [
        await escrowDeposit({ owner: owner.address, ownerUsdc, amount: 25n * USDC }),
      ]);
      const full = await balanceOf(escrowVault);
      const walletBefore = await balanceOf(ownerUsdc);

      await expectAnchorError(
        sendIxs(connection, owner, [await escrowWithdraw({ owner: owner.address, ownerUsdc, amount: full + 1n })]),
        ESCROW_BALANCE_SHORT,
      );
      assert.equal(await balanceOf(escrowVault), full, "a refused withdrawal moved funds");

      await sendIxs(connection, owner, [await escrowWithdraw({ owner: owner.address, ownerUsdc, amount: full })]);
      assert.equal(await balanceOf(escrowVault), 0n);
      assert.equal(await balanceOf(ownerUsdc), walletBefore + full);
    });

    it("deposits and arms while the pool is paused both ways with a batch open", async () => {
      await sendIxs(connection, owner, [
        await escrowDeposit({ owner: owner.address, ownerUsdc, amount: 10n * USDC }),
        await setEscrowArmed({ owner: owner.address, armed: true }),
      ]);
      assert.equal(await balanceOf(escrowVault), 10n * USDC);
      assert.equal((await escrowOf(escrow)).armed, true);
    });

    it("claims a settled roll's fees into the escrow, and the wallet's USDC does not move", async () => {
      await sendIxs(connection, admin, [
        await setPause({ administrator: admin.address, poolId, deposits: false, rolls: false, reason: 0 }),
      ]);
      const randomness = randomnessDrawingACard(decodePool((await fetchAccountData(connection, pool))!));
      await sendIxs(connection, operator, [
        await testResolveBatch({ operator: operator.address, pool, batchId, randomness }),
      ]);
      const drawn = await settleDrawnRoll(
        { connection, operator, pool, protocolConfig, weightIndex, roller: owner.address, rollerUsdc: ownerUsdc, assets },
        batchId,
        0,
      );
      assert.notEqual(drawn, NO_POSITION, "the roll predicted to draw a card did not — the prediction is wrong");

      // Any position the draw did not close is still the owner's and carries fees.
      const asset = assets.find((_, i) => i !== drawn)!;
      survivorMint = asset.mint;
      const { address: position } = await derivePda(["position", seedFromPubkey(pool), seedFromPubkey(asset.mint)]);
      const survivorState = decodePosition((await fetchAccountData(connection, position))!);
      assert.equal(survivorState.state, "Active");
      const poolState = decodePool((await fetchAccountData(connection, pool))!);
      assert.ok(
        poolState.accEqual > survivorState.equalCheckpoint || survivorState.accrued > 0n,
        `the settled roll left the survivor nothing to claim — drawn=${drawn}, ` +
          `accEqual=${poolState.accEqual}, checkpoint=${survivorState.equalCheckpoint}, ` +
          `accrued=${survivorState.accrued}, inTier=${survivorState.inTier}`,
      );

      const vaultBefore = await balanceOf(escrowVault);
      const walletBefore = await balanceOf(ownerUsdc);
      const signature = await sendIxs(connection, owner, [
        await claimFeeToEscrow({ depositor: owner.address, poolId, nftMint: asset.mint }),
      ]);
      const events = await eventsOf(signature);
      assert.deepEqual(
        events.map((e) => e.name),
        ["FeeClaimed", "EscrowDeposited"],
      );
      const feesPaid = (events[0]!.data as { feesPaid: bigint }).feesPaid;
      assert.ok(feesPaid > 0n, "the settled roll accrued no fee — the claim proves nothing");

      const deposited = events[1]!.data as {
        amount: bigint;
        source: string;
        position: Address;
        positionPool: Address;
        balanceAfter: bigint;
      };
      assert.equal(deposited.amount, feesPaid);
      assert.equal(deposited.source, "Fees");
      assert.equal(deposited.position, position);
      assert.equal(deposited.positionPool, pool);
      assert.equal(await balanceOf(escrowVault), vaultBefore + feesPaid);
      assert.equal(deposited.balanceAfter, vaultBefore + feesPaid);
      assert.equal(await balanceOf(ownerUsdc), walletBefore, "a fee claimed into the escrow paid the wallet");

      // Nothing new has accrued, so a second claim moves nothing and records no escrow deposit.
      const again = await sendIxs(connection, owner, [
        await claimFeeToEscrow({ depositor: owner.address, poolId, nftMint: asset.mint }),
      ]);
      assert.deepEqual(
        (await eventsOf(again)).map((e) => e.name),
        ["FeeClaimed"],
      );
      assert.equal(await balanceOf(escrowVault), vaultBefore + feesPaid);
    });

    it("refuses a stranger claiming the owner's position into an escrow, and the owner's claim lands", async () => {
      assert.ok(survivorMint, "the fee-claim test did not record a surviving position");
      const ix = await claimFeeToEscrow({ depositor: owner.address, poolId, nftMint: survivorMint });
      await expectAnchorError(sendIxs(connection, stranger, [withSigner(ix, stranger.address)]), NOT_POSITION_OWNER);
      const signature = await sendIxs(connection, owner, [ix]);
      assert.deepEqual(
        (await eventsOf(signature)).map((e) => e.name),
        ["FeeClaimed"],
      );
    });
  });

  // ── The wrong-signer matrix rows this suite owns ────────────────────────────────────────────
  describe("authorisation", () => {
    it("refuses a stranger on every owner instruction at the seeds, and the owner's own call succeeds", async () => {
      const { wallet: late, usdcAccount: lateUsdc } = await walletWithUsdc(2n * USDC);
      const cases = [
        await openEscrow({ owner: late.address, usdcMint }),
        await escrowDeposit({ owner: late.address, ownerUsdc: lateUsdc, amount: 2n * USDC }),
        await escrowWithdraw({ owner: late.address, ownerUsdc: lateUsdc, amount: USDC }),
        await setEscrowArmed({ owner: late.address, armed: true }),
      ];
      for (const ix of cases) {
        await expectAnchorError(sendIxs(connection, stranger, [withSigner(ix, stranger.address)]), CONSTRAINT_SEEDS);
        await sendIxs(connection, late, [ix]);
      }
      const pdas = await deriveEscrow(late.address);
      assert.equal(await balanceOf(pdas.escrowVault), USDC);
      assert.equal((await escrowOf(pdas.escrow)).armed, true);
    });

    it("covers exactly the matrix rows this suite is answerable for", () => {
      assert.deepEqual(
        EXECUTED_ROWS.filter((r) => r.suite === "escrow").map((r) => `${r.instruction}.${r.signer}`),
        [
          "open_escrow.owner",
          "escrow_deposit.owner",
          "escrow_withdraw.owner",
          "set_escrow_armed.owner",
          "claim_fee_to_escrow.depositor",
        ],
      );
    });
  });
});
