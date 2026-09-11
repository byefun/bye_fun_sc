// T-103's behavioural half: the three depositor exits on standard 1, against a live validator.
//
// flows §1.2 calls the exit "where INV-02 makes its strongest promise", and until this file the
// legacy half of that promise was untested on every one of the three. A card that can be escrowed
// and cannot be released is worse than one that was never admitted, and the source pins cannot see
// it: they assert the branch exists, not that the SPL leg actually moves a card out of a vault a
// PDA signs for.
//
// The fourth case is the destination guard, and it is the reason `release_from_vault` creates the
// ATA rather than transferring to whatever account it was handed. SPL `Transfer` validates the
// *source* authority and says nothing about who owns the destination — and `return_rejected` is
// permissionless, so on the legacy path the caller choosing that account is not the depositor.

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
import {
  createPnftCollection,
  mintPlainNonFungiblePnft,
  type MintedAsset,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import {
  deposit,
  approveDeposit,
  rejectDeposit,
  returnRejected,
} from "../../scripts/lib/deposit.js";
import { withdraw, claimNft } from "../../scripts/lib/exit.js";
import { beginSweep, recordValue, endSweep } from "../../scripts/lib/value.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { createAtaInstruction } from "../helpers/ata.js";

const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";

const PROFILE = launchProfile("11111111111111111111111111111111" as Address);
const ADMISSION_FLOOR = PROFILE.admissionFloor;
const TICKET_TARGET = PROFILE.ticketTarget;
const ADMITTED_VALUE = 20_000_000n;
const BELOW_FLOOR_VALUE = 1_000_000n;

const REJECT_REASON = 1;
/** Anchor's `ConstraintRaw`, per the NC-P9-4 ruling: what a supplied-when-absent account returns. */
const ANCHOR_CONSTRAINT_RAW = 2003;

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

describe("T-103 — the standard-1 depositor exits against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;
  let collection: PnftCollection;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 20);

    const { usdcMint, poolCounter, operator: op } = await ensureProtocol(connection, admin);
    operator = op;

    collection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "T-103 legacy exit fixtures",
    });

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

    // pNFT **and** legacy. Case 4's rejection is only meaningful against a record that admits the
    // standard — otherwise it would be refused at 6108 before the account shape was ever read.
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
      standards: 0b011,
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

  async function positionPdas(mint: Address) {
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(mint),
    ]);
    const { address: positionVault } = await derivePda([
      POSITION_VAULT_SEED,
      seedFromPubkey(position),
    ]);
    return { position, positionVault };
  }

  /** Mints one verified legacy `NonFungible` and escrows it — every case starts here. */
  async function depositLegacy(): Promise<MintedAsset> {
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
    const { positionVault } = await positionPdas(asset.mint);
    const escrowed = await connection.getTokenAccountBalance({ tokenAccount: positionVault });
    assert.equal(escrowed.amount, 1n, "the fixture did not reach escrow");
    return asset;
  }

  /** The card is back with its depositor and the vault it left is gone. */
  async function assertReleased(asset: MintedAsset) {
    const balance = await connection.getTokenAccountBalance({ tokenAccount: asset.tokenAccount });
    assert.equal(balance.amount, 1n, "the legacy NFT did not return to the depositor");
    const { position, positionVault } = await positionPdas(asset.mint);
    assert.equal(await fetchAccountData(connection, position), null, "the position is still open");
    assert.equal(
      await fetchAccountData(connection, positionVault),
      null,
      "the vault survived the exit, so a re-deposit at the same seeds is unsatisfiable",
    );
  }

  it("return_rejected releases a standard-1 card, and it is permissionless in doing so", async () => {
    const asset = await depositLegacy();
    await sendIxs(connection, operator, [
      await rejectDeposit({
        operator: operator.address,
        pool,
        nftMint: asset.mint,
        reason: REJECT_REASON,
      }),
    ]);

    // Signed by a wallet that is neither the depositor nor any protocol authority: the legacy leg
    // must not have acquired a signer the pNFT leg does not have.
    const stranger = await fundedWallet(connection, 5);
    await sendIxs(connection, stranger, [
      await returnRejected({
        payer: stranger.address,
        depositor: depositor.address,
        pool,
        nftMint: asset.mint,
        depositorToken: asset.tokenAccount,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        positionTokenRecord: null,
        depositorTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
      }),
    ]);
    await assertReleased(asset);
  });

  it("withdraw releases a standard-1 card from Active", async () => {
    const asset = await depositLegacy();
    await sendIxs(connection, operator, [
      await approveDeposit({
        operator: operator.address,
        poolId,
        nftMint: asset.mint,
        depositor: depositor.address,
        weightIndex,
        displaced: null,
        value: ADMITTED_VALUE,
        observedAt: nowSeconds(),
        lockUntil: 0n,
      }),
    ]);
    await sendIxs(connection, depositor, [
      await withdraw({
        depositor: depositor.address,
        poolId,
        nftMint: asset.mint,
        weightIndex,
        depositorUsdc: null,
        depositorToken: asset.tokenAccount,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        positionTokenRecord: null,
        depositorTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
        successor: null,
      }),
    ]);
    await assertReleased(asset);
  });

  it("claim_nft releases a standard-1 card from ClosedBelowFloor", async () => {
    const asset = await depositLegacy();
    await sendIxs(connection, operator, [
      await approveDeposit({
        operator: operator.address,
        poolId,
        nftMint: asset.mint,
        depositor: depositor.address,
        weightIndex,
        displaced: null,
        value: ADMITTED_VALUE,
        observedAt: nowSeconds(),
        lockUntil: 0n,
      }),
    ]);

    // One sweep at a below-floor value closes the position and leaves the card escrowed — which is
    // the only state `claim_nft` serves, and the only one this phase can reach. `Seized` is the
    // other admissible source and it is **unreachable on this standard**; see NC-P9-3.
    await sendIxs(connection, operator, [await beginSweep({ operator: operator.address, poolId })]);
    await sendIxs(connection, operator, [
      await recordValue({
        operator: operator.address,
        poolId,
        nftMint: asset.mint,
        depositor: depositor.address,
        weightIndex,
        value: BELOW_FLOOR_VALUE,
        observedAt: nowSeconds(),
        effect: { kind: "close", successor: null },
      }),
    ]);
    await sendIxs(connection, operator, [await endSweep({ operator: operator.address, poolId })]);

    await sendIxs(connection, depositor, [
      await claimNft({
        depositor: depositor.address,
        pool,
        nftMint: asset.mint,
        depositorToken: asset.tokenAccount,
        metadata: asset.metadata,
        masterEdition: asset.masterEdition,
        positionTokenRecord: null,
        depositorTokenRecord: null,
        sysvarInstructions: null,
        authorizationRulesProgram: null,
        authorizationRules: null,
      }),
    ]);
    await assertReleased(asset);
  });

  it("a permissionless standard-1 return cannot redirect the card to the caller's own account", async () => {
    // The finding this row's design turns on. SPL `Transfer` checks the *source* authority and
    // never the destination's owner, so without `create_idempotent`'s derivation a stranger
    // calling the permissionless `return_rejected` on a legacy position could name their own token
    // account for the mint and take the card. The pNFT path is protected by `TransferV1`'s own
    // `destination_owner` check; the legacy path has only this.
    const asset = await depositLegacy();
    await sendIxs(connection, operator, [
      await rejectDeposit({
        operator: operator.address,
        pool,
        nftMint: asset.mint,
        reason: REJECT_REASON,
      }),
    ]);

    const attacker = await fundedWallet(connection, 5);
    const { address: attackerAta, instruction: createAtaIx } = await createAtaInstruction({
      payer: attacker.address,
      owner: attacker.address,
      mint: asset.mint,
    });
    await sendIxs(connection, attacker, [createAtaIx]);

    await assert.rejects(
      sendIxs(connection, attacker, [
        await returnRejected({
          payer: attacker.address,
          depositor: depositor.address,
          pool,
          nftMint: asset.mint,
          // A real, initialised token account for the right mint — owned by the attacker. Only
          // the ATA derivation separates it from the depositor's.
          depositorToken: attackerAta,
          metadata: asset.metadata,
          masterEdition: asset.masterEdition,
          positionTokenRecord: null,
          depositorTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
        }),
      ]),
    );

    const attackerBalance = await connection.getTokenAccountBalance({ tokenAccount: attackerAta });
    assert.equal(attackerBalance.amount, 0n, "the attacker's account must remain empty");
    const { positionVault } = await positionPdas(asset.mint);
    const vaultBalance = await connection.getTokenAccountBalance({ tokenAccount: positionVault });
    assert.equal(vaultBalance.amount, 1n, "the card must still be in the position's vault");
  });

  it("a standard-1 exit that supplies a pNFT account it must not have is refused", async () => {
    const asset = await depositLegacy();
    await sendIxs(connection, operator, [
      await rejectDeposit({
        operator: operator.address,
        pool,
        nftMint: asset.mint,
        reason: REJECT_REASON,
      }),
    ]);
    await expectAnchorError(
      sendIxs(connection, depositor, [
        await returnRejected({
          payer: depositor.address,
          depositor: depositor.address,
          pool,
          nftMint: asset.mint,
          depositorToken: asset.tokenAccount,
          metadata: asset.metadata,
          masterEdition: asset.masterEdition,
          // The only argument that differs from the passing case above. What the account *is*
          // never matters — the guard is `is_none()`.
          positionTokenRecord: asset.mint,
          depositorTokenRecord: null,
          sysvarInstructions: null,
          authorizationRulesProgram: null,
          authorizationRules: null,
        }),
      ]),
      ANCHOR_CONSTRAINT_RAW,
    );
  });
});
