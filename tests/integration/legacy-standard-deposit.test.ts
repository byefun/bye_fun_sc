// T-102's behavioural half: the standard-1 deposit path against a live validator.
//
// Every other instrument for this branch is a source pin, and design §4.2's own wording is why
// that is not enough — the five pNFT accounts are **required-absent** on a legacy deposit, and
// absence is not a thing a compile-time pin can observe. `transfer_spl` ignores those five
// accounts entirely, so a legacy deposit that supplied a token record would move the card, open
// the position and emit `DepositPending` with the account carried unread in a transaction whose
// shape claims pNFT. No CPI fails, no state is wrong, and nothing in the crate looks.
//
// Three cases, and the first one is the control the other two need: without an asserted success
// path a rejection matrix passes an operation that never completes.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { type Address } from "@solana/kit";
import { addSignersToInstruction } from "@solana/kit";
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
import { createPnftCollection, mintPlainNonFungiblePnft, type PnftCollection } from "../helpers/fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { deposit } from "../../scripts/lib/deposit.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { decodePosition } from "../../scripts/lib/decoders.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";

/** `Position.standard`'s legacy discriminant, verbatim against `common/standards.rs` and not read
 * back from a decoder that could agree with a wrong program. `state` is compared against the
 * decoder's own name because that is what `decodePosition` returns — the wire byte behind it is
 * pinned in Rust by `position_state_variant_order_is_the_wire_discriminant`. */
const STANDARD_LEGACY = 1;
const POSITION_STATE_PENDING = "Pending";

/** Anchor's own codes, per the NC-P9-4 ruling. `ConstraintRaw` is what a supplied-when-absent
 * account returns; there is deliberately no `6xxx` for it. */
const ANCHOR_CONSTRAINT_RAW = 2003;
/** `StandardNotAdmittedForCollection` — the record exists for this collection and does not carry
 * the bit for the standard the asset resolved to. */
const STANDARD_NOT_ADMITTED_FOR_COLLECTION = 6108;

describe("T-102 — the standard-1 transfer branch against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let depositor: TransactionSigner;
  let poolId: number;
  let pool: Address;
  /** Admitted for pNFT **and** legacy (`0b011`) — the launch shape of `CCryptWBY…N2Yf`. */
  let bothStandards: PnftCollection;
  /** Admitted for pNFT only (`0b001`). Same pool, so the wrong-pairing case below differs from
   * the success case in exactly one bit and nothing else. */
  let pnftOnly: PnftCollection;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 20);

    const { usdcMint, poolCounter } = await ensureProtocol(connection, admin);
    poolId = poolCounter;

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
      args: { ...launchProfile(admin.address) },
    });
    pool = poolAccounts.pool;

    bothStandards = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "T-102 pNFT + legacy",
    });
    pnftOnly = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "T-102 pNFT only",
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: bothStandards.mint,
      standards: 0b011,
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: pnftOnly.mint,
      standards: 0b001,
    });
  });

  /** One verified legacy `NonFungible` with a real `MasterEditionV2`, owned by the depositor. */
  async function mintLegacy(collection: PnftCollection) {
    return mintPlainNonFungiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection,
    });
  }

  /**
   * A legacy deposit, with every one of the five pNFT accounts overridable. Defaults are `null`
   * — the shape design §4.2 requires — so a case that supplies one has to say so, and the
   * rejection case differs from the success case in exactly one argument.
   */
  async function depositLegacy(
    asset: { mint: Address; metadata: Address; masterEdition: Address; tokenAccount: Address },
    collection: PnftCollection,
    overrides: { depositorTokenRecord?: Address } = {},
  ) {
    return sendIxs(connection, depositor, [
      await deposit({
        depositor: depositor.address,
        poolId,
        collection: collection.mint,
        nftMint: asset.mint,
        depositorToken: asset.tokenAccount,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        depositorTokenRecord: overrides.depositorTokenRecord ?? null,
        positionTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
      }),
    ]);
  }

  it("a legacy NonFungible from a collection admitted for it escrows and records standard 1", async () => {
    const asset = await mintLegacy(bothStandards);
    await depositLegacy(asset, bothStandards);

    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(asset.mint),
    ]);
    const data = await fetchAccountData(connection, position);
    assert.ok(data, "the legacy deposit created no position account");
    const decoded = decodePosition(data);

    // The whole point of the row: the standard is *resolved*, not assumed. A handler that kept
    // writing STANDARD_PNFT would pass every source pin and land a card whose exit path is the
    // pNFT one it cannot take.
    assert.equal(decoded.standard, STANDARD_LEGACY);
    assert.equal(decoded.state, POSITION_STATE_PENDING);
    assert.equal(decoded.nftMint, asset.mint);
    assert.equal(decoded.depositor, depositor.address);

    // INV-01/INV-55, and the reason this case is a control rather than a smoke test: a `Position`
    // that exists proves the handler ran, never that `transfer_spl` moved anything. Both sides of
    // the move are read — a transfer that credited the vault without debiting the depositor, or
    // one that ran against the wrong source, shows up in exactly one of these two numbers.
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const vaultBalance = await connection.getTokenAccountBalance({ tokenAccount: positionVault });
    assert.equal(vaultBalance.amount, 1n, "the legacy NFT must be held in the position's vault");
    const depositorBalance = await connection.getTokenAccountBalance({
      tokenAccount: asset.tokenAccount,
    });
    assert.equal(depositorBalance.amount, 0n, "the legacy NFT must have left the depositor");
  });

  it("the same deposit is refused when it supplies a token record it must not have", async () => {
    const asset = await mintLegacy(bothStandards);
    // Any address at all: the guard is `is_none()`, so what the account *is* never matters — only
    // that the caller sent one. Reusing the mint keeps the case from depending on a token record
    // that legacy mints do not have in the first place.
    await expectAnchorError(
      depositLegacy(asset, bothStandards, { depositorTokenRecord: asset.mint }),
      ANCHOR_CONSTRAINT_RAW,
    );
  });

  it("a legacy NonFungible from a pNFT-only collection is refused on the standards bit", async () => {
    const asset = await mintLegacy(pnftOnly);
    await expectAnchorError(depositLegacy(asset, pnftOnly), STANDARD_NOT_ADMITTED_FOR_COLLECTION);
  });
});
