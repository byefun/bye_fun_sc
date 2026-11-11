// The instant sell against a live validator, on both custody variants.
//
// Three signers in one transaction is the shape nothing else in this program has, and it is the
// first thing this file proves: the seller, the Operator and the Treasury-03 authority all sign,
// and the transaction lands. `addSignersToInstruction` is the send path — the encoders stay
// Address-based like every other in scripts/lib, and the signers are attached here.
//
// **What this file does NOT cover, stated because the gap is invisible otherwise.** The
// `Pending` branch — a sell landing while `open_batches > 0` — cannot be driven here: the only
// writers of `open_batches` are `commit_rolls` and the batch terminals, and `commit_rolls`
// requires the MagicBlock VRF program, which this suite's validator deliberately does not load
// (see run-integration-tests.sh's "Cloned programs" note). That branch is driven in Rust instead,
// against the real `record_acquisition`, by `an_open_batch_leaves_the_position_pending_and_moves_
// no_weight` and `the_attestation_is_persisted_on_both_branches` in common/buyback.rs — and
// because both handlers reach that one shared implementation, driving it once covers both
// variants by construction rather than by resemblance. What is left, once a VRF fixture lands, is
// the end-to-end `Pending` case.
//
// **`activate_acquired` inherits that gap in full and has no integration file of its own.** It
// serves `Pending` treasury positions and nothing else, so every input it accepts is downstream
// of the branch this validator cannot reach — an integration suite for it could only assert that
// it refuses, which is the half a green run proves least. Its behaviour is driven in Rust against
// the real `activate_acquired_position` in common/buyback.rs: the stored attestation winning over
// the clock, each of the three admission limits re-checked from both directions, and the four
// refusals. It joins this file when the VRF fixture lands.

import { describe, it, before } from "node:test";
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
import { createPool, launchProfile, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import {
  createPnftCollection,
  mintAdmissiblePnft,
  mintPlainNonFungiblePnft,
  tokenRecordPda,
  RULE_SET_ADDRESS,
  type MintedAsset,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { AUTH_RULES_PROGRAM, SYSVAR_INSTRUCTIONS, TOKEN_PROGRAM } from "../../scripts/constants.js";
import { createCoreCollection, mintCoreAsset, readCoreAsset } from "../helpers/core-fixtures.js";
import { EXECUTED_ROWS, NOT_POSITION_OWNER, UNAUTHORIZED } from "../helpers/authority-matrix.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { instantSell, instantSellCore } from "../../scripts/lib/buyback.js";
import { derivePda, seedFromPubkey, signer, writable } from "../../scripts/lib/anchor.js";
import { decodePosition, decodeWalletStats, decodePool } from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const WALLET_STATS_SEED = "wallet";
const TREASURY_02_SEED = "t02";

/** Verbatim against `common/standards.rs`, not read back from a decoder that could agree with a
 * wrong program. */
const STANDARD_PNFT = 0;
const STANDARD_LEGACY = 1;
const STANDARD_CORE = 2;
const POSITION_STATE_ACTIVE = "Active";

/** Anchor's own codes: an omitted required account is
 * `AccountNotEnoughKeys`, a supplied must-be-absent one is `ConstraintRaw`. There is deliberately
 * no `6xxx` for either — the standard is not resolved until the handler reads the asset, so no
 * account constraint can express the condition. */
const ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS = 3005;
const ANCHOR_CONSTRAINT_RAW = 2003;

/** `T03Shortfall`, `common/errors.rs`'s 66xx group. **6602, not 6601** — 6601 is
 * `T04Shortfall`, its immediate neighbour and the one a transcription slip reaches for.
 * Verified against the numeric table in errors.rs, which pins every variant by value. */
const T03_SHORTFALL = 6602;

/** SPL Token `MintTo`: instruction 7, then the amount as a little-endian u64. */
function mintToData(amount: bigint): Uint8Array {
  const data = new Uint8Array(9);
  data[0] = 7;
  new DataView(data.buffer).setBigUint64(1, amount, true);
  return data;
}

/** `launchProfile`'s own rate and floor. Restated here rather than imported so a change to the
 * profile reds this file instead of silently moving what it asserts. */
const BUYBACK_RATE_BPS = 8_500n;
const ADMISSION_FLOOR = 12_000_000n;

/** `fetchAccountData` resolves `null` for an account that does not exist. Every call below means
 * to read an account this test has just caused to exist, so a `null` is a failure with a name
 * rather than a `TypeError` three lines later. */
async function mustFetch(connection: Connection, address: Address, what: string): Promise<Buffer> {
  const data = await fetchAccountData(connection, address);
  assert.ok(data, `${what} (${address}) does not exist on chain`);
  return Buffer.from(data);
}

describe("the instant sell against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let seller: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;
  let treasury03: Address;
  let sellerUsdc: Address;
  let t02Identity: Address;
  let usdcMint: Address;
  let pnftCollection: PnftCollection;
  let coreCollection: Address;

  /** The value every sell in this file attests, and the payout it must produce. Above the floor
   * so admission is not the thing under test. */
  const VALUE = ADMISSION_FLOOR * 2n;
  const EXPECTED_PAYOUT = (VALUE * BUYBACK_RATE_BPS) / 10_000n;
  const OBSERVED_AT = 1_800_000_000n;

  async function tokenBalance(account: Address): Promise<bigint> {
    const raw = await mustFetch(connection, account, "token account");
    // SPL token account layout: mint(32) ‖ owner(32) ‖ amount(u64 le).
    return raw.readBigUInt64LE(64);
  }

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    operator = await loadOperator();
    seller = await fundedWallet(connection, 20);

    const protocolState = await ensureProtocol(connection, admin);
    usdcMint = protocolState.usdcMint;
    poolId = protocolState.poolCounter;

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], createWiIx)]);
    weightIndex = weightIndexSigner.address;

    // Zero tokens: this call is here to create the seller's USDC account, not to fund it —
    // the payout is what must fund it, and a pre-funded balance would hide a payout of 0.
    //
    // `false` is load-bearing: kite's `mintTokens` defaults `useTokenExtensions` to **true**
    // while its own `getTokenAccountAddress` defaults it to **false**. Taking both defaults
    // mints through Token-2022 and derives a legacy ATA — and the USDC fixture is a legacy
    // Token mint, so the mint instruction fails with `incorrect program id for instruction`.
    await connection.mintTokens(usdcMint, admin, 0n, seller.address, false);
    sellerUsdc = await getTokenAccountAddress(seller.address, usdcMint, false);

    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex,
      usdcMint,
      args: { ...launchProfile() },
    });
    pool = poolAccounts.pool;

    // Treasury 03 is the shared `PDA(["t03"])`, which `init_pool` creates when absent, authorised
    // by the ProtocolConfig PDA — no outside key can move it, so funding it is a bare `MintTo` against an
    // account that already exists rather than a `mintTokens` that would try to create it.
    // Funded well past one payout so the shortfall case below has to be engineered rather than
    // stumbled into.
    treasury03 = poolAccounts.treasury03;
    await sendIxs(connection, admin, [
      {
        programAddress: TOKEN_PROGRAM,
        accounts: [writable(usdcMint), writable(treasury03), signer(admin.address)],
        data: mintToData(1_000_000_000n),
      },
    ]);
    ({ address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]));

    pnftCollection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "sell pNFT + legacy",
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: pnftCollection.mint,
      standards: 0b011,
    });

    const core = await createCoreCollection({ connection, payer: admin, name: "sell core" });
    coreCollection = core.collection;
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: coreCollection,
      standards: 0b100,
    });
  });

  async function positionFor(assetKey: Address) {
    const { address } = await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(assetKey)]);
    return { address, decoded: decodePosition(await mustFetch(connection, address, "Position")) };
  }

  async function t02Stats() {
    const { address } = await derivePda([
      WALLET_STATS_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(t02Identity),
    ]);
    return { address, decoded: decodeWalletStats(await mustFetch(connection, address, "T02 WalletStats")) };
  }

  /** The two-signer send. Operator is the fee payer because it is the rent payer on chain. */
  async function send(ix: Awaited<ReturnType<typeof instantSell>>) {
    return sendIxs(connection, operator, [
      addSignersToInstruction([seller, operator], ix),
    ]);
  }

  // --- the success path, which every rejection below needs as its control -------------------

  it("legacy: pays the live rate, escrows the card, and activates the position", async () => {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: seller.address,
      collection: pnftCollection,
    });

    const sellerBefore = await tokenBalance(sellerUsdc);
    const treasuryBefore = await tokenBalance(treasury03);

    await send(
      await instantSell({
        seller: seller.address,
        operator: operator.address,
        poolId,
        collection: pnftCollection.mint,
        nftMint: asset.mint,
        sellerToken: asset.tokenAccount,
        sellerUsdc,
        weightIndex,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        // Required-absent on a legacy sell, exactly as on `deposit`.
        sellerTokenRecord: null,
        positionTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
        displaced: null,
        loader: null,
        value: VALUE,
        observedAt: OBSERVED_AT,
      }),
    );

    // The payout as money actually moved, not as arithmetic re-run by the test.
    assert.equal(await tokenBalance(sellerUsdc), sellerBefore + EXPECTED_PAYOUT);
    assert.equal(await tokenBalance(treasury03), treasuryBefore - EXPECTED_PAYOUT);

    const { decoded } = await positionFor(asset.mint);
    assert.equal(decoded.state, POSITION_STATE_ACTIVE, "no batch is open, so the position activates");
    assert.equal(decoded.standard, STANDARD_LEGACY);
    assert.equal(decoded.recordedValue, VALUE);
    assert.equal(decoded.valueObservedAt, OBSERVED_AT);
    // A treasury-acquired position earns no Season-0 score.
    assert.equal(decoded.lockUntil, 0n);
    // The position is the treasury's, which is what leaves `activate_acquired` its only
    // activation path — `approve_deposit` and `reject_deposit` both refuse a t02 depositor.
    assert.equal(decoded.depositor, t02Identity);
    assert.notEqual(decoded.depositor, seller.address);

    const poolState = decodePool(await mustFetch(connection, pool, "Pool"));
    assert.equal(poolState.nReal, 1, "the activation entered the draw domain");
    assert.ok(poolState.wReal > 0n);
  });

  it("creates the T02 rollup on the first acquisition and never the seller's own", async () => {
    const { decoded } = await t02Stats();
    assert.equal(decoded.owner, t02Identity);
    assert.equal(decoded.activePositions, 1);
    assert.equal(decoded.activeValue, VALUE);

    // No seller rollup is created or touched. Asserted as an absence of the account,
    // which is the only form the claim takes.
    const { address: sellerStats } = await derivePda([
      WALLET_STATS_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(seller.address),
    ]);
    assert.equal(
      await fetchAccountData(connection, sellerStats),
      null,
      "the seller's own WalletStats was created — none should be",
    );
  });

  // --- the twin, on the same pool and the same treasury ---------------------------------------

  it("core: the twin moves the asset through MPL Core and lands the same position shape", async () => {
    const asset = await mintCoreAsset({
      connection,
      payer: admin,
      collection: coreCollection,
      collectionAuthority: admin,
      owner: seller.address,
    });

    const sellerBefore = await tokenBalance(sellerUsdc);

    await send(
      await instantSellCore({
        seller: seller.address,
        operator: operator.address,
        poolId,
        collection: coreCollection,
        asset,
        sellerUsdc,
        weightIndex,
        displaced: null,
        loader: null,
        value: VALUE,
        observedAt: OBSERVED_AT,
      }),
    );

    assert.equal(await tokenBalance(sellerUsdc), sellerBefore + EXPECTED_PAYOUT);

    const { address: position, decoded } = await positionFor(asset);
    assert.equal(decoded.state, POSITION_STATE_ACTIVE);
    assert.equal(decoded.standard, STANDARD_CORE);
    assert.equal(decoded.depositor, t02Identity);
    assert.equal(decoded.recordedValue, VALUE);

    // Custody actually moved: the asset's owner is the vault PDA, an address MPL Core writes
    // into AssetV1.owner without ever allocating.
    const { address: vault } = await derivePda(["vault", seedFromPubkey(position)]);
    const onChain = await readCoreAsset(connection, asset);
    assert.ok(onChain, "the Core asset account vanished");
    assert.equal(onChain.owner, vault);
  });

  /** **`init_if_needed` is where a second call silently resets a counter.** The acceptance asks
   * for this case by name, and it is only observable on a second acquisition. */
  it("a second acquisition adds to the T02 rollup instead of resetting it", async () => {
    const { decoded } = await t02Stats();
    assert.equal(decoded.activePositions, 2, "the first acquisition's count was reset");
    assert.equal(decoded.activeValue, VALUE * 2n, "the first acquisition's value was reset");
  });

  // --- the pNFT branch: the five two-sided optional accounts --------------------------------
  //
  // `standard == 0` is the third custody path and the only one with conditional accounts. The
  // Rust pins can see that the branch calls `transfer_pnft` and that the five accounts are read;
  // what they cannot see is whether the assembled account list actually executes through Token
  // Metadata — account order, the payer/authority split, and the `None`-as-program-id convention
  // are all only observable against a live validator.

  /** One pNFT sale. `overrides` lets a case break exactly one account and change nothing else. */
  async function sellPnft(
    asset: Awaited<ReturnType<typeof mintAdmissiblePnft>>,
    overrides: Partial<Parameters<typeof instantSell>[0]> = {},
  ) {
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(asset.mint),
    ]);
    const { address: positionVault } = await derivePda([
      POSITION_VAULT_SEED,
      seedFromPubkey(position),
    ]);
    return send(
      await instantSell({
        seller: seller.address,
        operator: operator.address,
        poolId,
        collection: pnftCollection.mint,
        nftMint: asset.mint,
        sellerToken: asset.tokenAccount,
        sellerUsdc,
        weightIndex,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        sellerTokenRecord: await tokenRecordPda(asset.mint, asset.tokenAccount),
        positionTokenRecord: await tokenRecordPda(asset.mint, positionVault),
        sysvarInstructions: SYSVAR_INSTRUCTIONS,
        authorizationRulesProgram: AUTH_RULES_PROGRAM,
        authorizationRules: RULE_SET_ADDRESS,
        displaced: null,
        loader: null,
        value: VALUE,
        observedAt: OBSERVED_AT,
        ...overrides,
      }),
    );
  }

  it("pnft: a ProgrammableNonFungible sale escrows through Token Metadata and records standard 0", async () => {
    const asset = await mintAdmissiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: seller.address,
      collection: pnftCollection,
      value: VALUE,
      label: "p0",
    });

    const sellerBefore = await tokenBalance(sellerUsdc);
    await sellPnft(asset);
    assert.equal(await tokenBalance(sellerUsdc), sellerBefore + EXPECTED_PAYOUT);

    const { decoded } = await positionFor(asset.mint);
    assert.equal(decoded.standard, STANDARD_PNFT);
    assert.equal(decoded.state, POSITION_STATE_ACTIVE);
    assert.equal(decoded.depositor, t02Identity);
    assert.equal(decoded.recordedValue, VALUE);
  });

  /** Required-**present** on standard 0. The control above is what makes this case mean
   * something: without an asserted success path, a rejection matrix passes an operation that
   * never completes. */
  it("pnft: omitting the token records is refused with AccountNotEnoughKeys", async () => {
    const asset = await mintAdmissiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: seller.address,
      collection: pnftCollection,
      value: VALUE,
      label: "p0-none",
    });
    await expectAnchorError(
      sellPnft(asset, { sellerTokenRecord: null, positionTokenRecord: null }),
      ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS,
    );
  });

  /** Required-**absent** on standard 1, and this is the half no compile-time pin can observe.
   * Without it, a legacy sell that supplied a token record would move the card, pay the seller
   * and open the position with the account carried unread in a transaction whose shape claims
   * pNFT — no CPI fails and nothing in the crate looks. */
  it("legacy: supplying a token record it must not have is refused with ConstraintRaw", async () => {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: seller.address,
      collection: pnftCollection,
    });
    await expectAnchorError(
      send(
        await instantSell({
          seller: seller.address,
          operator: operator.address,
            poolId,
          collection: pnftCollection.mint,
          nftMint: asset.mint,
          sellerToken: asset.tokenAccount,
          sellerUsdc,
          weightIndex,
          metadata: asset.metadata,
          masterEdition: asset.masterEdition,
          // Any address at all: the guard is `is_none()`, so what the account *is* never
          // matters — only that the caller sent one. Reusing the mint keeps the case from
          // depending on a token record legacy mints do not have in the first place.
          sellerTokenRecord: asset.mint,
          positionTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
          displaced: null,
          loader: null,
          value: VALUE,
          observedAt: OBSERVED_AT,
        }),
      ),
      ANCHOR_CONSTRAINT_RAW,
    );
  });

  // --- fails naming the failing condition ----------------------------------------------------

  it("a Treasury-03 balance short of the payout fails naming the condition", async () => {
    const asset = await mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: seller.address,
      collection: pnftCollection,
    });

    // Attest a value whose 85% exceeds everything Treasury 03 holds. Nothing else about the sell
    // changes, so the rejection is the balance and not admission or custody.
    const treasuryNow = await tokenBalance(treasury03);
    const tooLarge = ((treasuryNow + 1_000_000n) * 10_000n) / BUYBACK_RATE_BPS + 1n;

    await expectAnchorError(
      send(
        await instantSell({
          seller: seller.address,
          operator: operator.address,
            poolId,
          collection: pnftCollection.mint,
          nftMint: asset.mint,
          sellerToken: asset.tokenAccount,
          sellerUsdc,
          weightIndex,
          metadata: asset.metadata,
          masterEdition: asset.masterEdition,
          sellerTokenRecord: null,
          positionTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
          displaced: null,
          loader: null,
          value: tooLarge,
          observedAt: OBSERVED_AT,
        }),
      ),
      T03_SHORTFALL,
    );
  });

  // --- the wrong-signer rows ------------------------------------------------------------------
  //
  // Three signers is what makes this instruction worth attacking cell by cell: each of the three
  // releases something the other two do not — the card, the protocol's assent, and the treasury's
  // money — so classifying the handler once would collapse three different authorities into one
  // row. Each is substituted in turn while the other two stay correct, so the only difference
  // between a refusal here and the sale that follows it is who signed.
  //
  // The control is shared across the three rows of a variant deliberately: it sells the *same
  // asset*, through the same accounts, with the right three signers. A per-row control would
  // prove less, not more — what these rows need to rule out is that the call failed for some
  // reason other than the signer, and the same inputs succeeding is exactly that.
  describe("the wrong-signer rows", () => {
    /** Which cells this file is answerable for. Asserted against what actually ran, below. */
    const OWNED_ROWS = EXECUTED_ROWS.filter((row) => row.suite === "instant-sell");
    const covered = new Set<string>();

    let imposter: TransactionSigner;
    let tmAsset: MintedAsset;
    let coreAsset: Address;

    before(async () => {
      imposter = await fundedWallet(connection, 20);
      tmAsset = await mintPlainNonFungiblePnft({
        connection,
        payer: admin,
        authority: admin,
        owner: seller.address,
        collection: pnftCollection,
      });
      coreAsset = await mintCoreAsset({
        connection,
        payer: admin,
        collection: coreCollection,
        collectionAuthority: admin,
        owner: seller.address,
      });
    });

    /** The sale, with one of the two signers replaced by a wallet that holds neither. */
    async function sellAs(cell: "seller" | "operator", variant: "legacy" | "core") {
      const who = (slot: "seller" | "operator") =>
        slot === cell ? imposter : { seller, operator }[slot];

      const ix =
        variant === "legacy"
          ? await instantSell({
              seller: who("seller").address,
              operator: who("operator").address,
              poolId,
              collection: pnftCollection.mint,
              nftMint: tmAsset.mint,
              sellerToken: tmAsset.tokenAccount,
              sellerUsdc,
              weightIndex,
              metadata: tmAsset.metadata,
              masterEdition: tmAsset.masterEdition,
              sellerTokenRecord: null,
              positionTokenRecord: null,
              sysvarInstructions: null,
              authorizationRulesProgram: null,
              authorizationRules: null,
              displaced: null,
              loader: null,
              value: VALUE,
              observedAt: OBSERVED_AT,
            })
          : await instantSellCore({
              seller: who("seller").address,
              operator: who("operator").address,
              poolId,
              collection: coreCollection,
              asset: coreAsset,
              sellerUsdc,
              weightIndex,
              displaced: null,
              loader: null,
              value: VALUE,
              observedAt: OBSERVED_AT,
            });

      // The imposter pays, so the transaction is signable whichever slot it took.
      return sendIxs(connection, imposter, [
        addSignersToInstruction([who("seller"), who("operator")], ix),
      ]);
    }

    for (const variant of ["legacy", "core"] as const) {
      const instruction = variant === "legacy" ? "instant_sell" : "instant_sell_core";

      for (const cell of ["seller", "operator"] as const) {
        // The seller is bound through the token accounts the sale moves, not through a field on
        // a position, so its refusal names a different code than the operator cell even though
        // the authority class is the same. The table is what says which.
        const code = cell === "seller" ? NOT_POSITION_OWNER : UNAUTHORIZED;

        it(`${instruction}: a wallet that is not the ${cell} is refused`, async () => {
          await expectAnchorError(sellAs(cell, variant), code);
          covered.add(`${instruction}.${cell}`);
        });
      }
    }

    it("legacy: the same asset sells when the right three sign", async () => {
      const before = await tokenBalance(sellerUsdc);
      await send(
        await instantSell({
          seller: seller.address,
          operator: operator.address,
            poolId,
          collection: pnftCollection.mint,
          nftMint: tmAsset.mint,
          sellerToken: tmAsset.tokenAccount,
          sellerUsdc,
          weightIndex,
          metadata: tmAsset.metadata,
          masterEdition: tmAsset.masterEdition,
          sellerTokenRecord: null,
          positionTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
          displaced: null,
          loader: null,
          value: VALUE,
          observedAt: OBSERVED_AT,
        }),
      );
      assert.equal(
        await tokenBalance(sellerUsdc),
        before + EXPECTED_PAYOUT,
        "the controlled sale moved no money, so the refusals above proved nothing",
      );
    });

    it("core: the same asset sells when the right three sign", async () => {
      const before = await tokenBalance(sellerUsdc);
      await send(
        await instantSellCore({
          seller: seller.address,
          operator: operator.address,
            poolId,
          collection: coreCollection,
          asset: coreAsset,
          sellerUsdc,
          weightIndex,
          displaced: null,
          loader: null,
          value: VALUE,
          observedAt: OBSERVED_AT,
        }),
      );
      assert.equal(
        await tokenBalance(sellerUsdc),
        before + EXPECTED_PAYOUT,
        "the controlled sale moved no money, so the refusals above proved nothing",
      );
    });

    it("covers exactly the matrix rows this suite is answerable for", () => {
      // The structural link between the table and what ran. Deleting a row above reds here
      // rather than quietly shrinking the matrix's real extent while the census stays green.
      assert.deepEqual(
        [...covered].sort(),
        OWNED_ROWS.map((row) => `${row.instruction}.${row.signer}`).sort(),
      );
    });
  });
});
