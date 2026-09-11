// F-2 — the NFT exit destination is unvalidated. `return_rejected` is permissionless
// (`payer: Signer`,
// `depositor: UncheckedAccount`, pinned only by `position.depositor == depositor.key()` @ 6302)
// and takes `depositor_token` as a bare, unvalidated `UncheckedAccount` — the `/// CHECK` comment
// on it asserts Token Metadata's own `TransferV1` processor validates the destination when the
// account already exists. That assertion had never been exercised against a live validator; this
// file is what exercises it.
//
// An ATA's address is deterministic on `(owner, mint)`, so the create branch is safe by
// construction — an attacker's account cannot *be* the depositor's own ATA. The exposure, if any,
// is confined to the already-exists branch, so both cases below drive the destination token
// account into that branch before calling `return_rejected`:
//   - control: the destination is the true depositor's own ATA — already existing (created as the
//     mint destination, then only emptied, never closed, by `deposit`).
//   - attack:  the destination is an ATA the attacker (payer) owns, hand-created
//     (tests/helpers/ata.ts) before the call, while `depositor` is still pinned to the true
//     depositor by 6302.
// Each needs its own position — `return_rejected` closes the position and its vault on success,
// so the two cases cannot share one fixture.
//
// MEASURED RESULT: the attack case fails, every time, with Token Metadata's own error 57 (0x39,
// `IncorrectOwner`, logged verbatim as "Incorrect account owner") from inside its `TransferV1`
// CPI — bye_machine's own program never gets a chance to accept or reject anything; the outer
// instruction just propagates TransferV1's failure. `TransferV1CpiBuilder` is built in
// `common/escrow.rs::transfer_pnft` with `.destination_owner(destination_owner)`, and
// `return_rejected.rs`'s call site passes `ctx.accounts.depositor` (the pinned true depositor, not
// whatever `depositor_token`'s own on-chain owner happens to be) as that argument — so TransferV1
// itself checks the destination token account's real owner against the pinned depositor and
// rejects the mismatch. F-2 closes as **Token Metadata's own catch**, not bye_machine's: the
// `/// CHECK` comment's claim holds, now for the first time as measured evidence rather than an
// unexercised assumption.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import {
  createPnftCollection,
  mintAdmissiblePnft,
  tokenRecordPda,
  RULE_SET_ADDRESS,
  type AdmissibleFixture,
  type PnftCollection,
} from "../helpers/fixtures.js";
import { createAtaInstruction } from "../helpers/ata.js";
import { launchProfile, createPool, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { deposit, rejectDeposit, returnRejected } from "../../scripts/lib/deposit.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { sendIxs } from "../../scripts/_common.js";
import { AUTH_RULES_PROGRAM, SYSVAR_INSTRUCTIONS } from "../../scripts/constants.js";

// Verbatim against programs/bye_machine/src/common/seeds.rs — same convention
// rule-set-replay.test.ts already follows for the same two seeds.
const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";

/** `reject_deposit`'s `reason` is an opaque u16 the client chooses — any nonzero value serves
 * this file's purpose (moving `Pending` → `Rejected`); it is not itself under test here. */
const REJECT_REASON = 1;

/** mpl-token-metadata 5.1.1's `MetadataError::IncorrectOwner` (generated/errors/mpl_token_metadata.rs:184-186)
 * — Token Metadata's own `TransferV1` catch, not bye_machine's. Named here rather than left as a
 * bare literal so a future reader does not mistake it for one of bye_machine's own error codes. */
const TOKEN_METADATA_INCORRECT_OWNER = 57;

describe("F-2 — return_rejected's depositor_token against a live validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  let attacker: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let collection: PnftCollection;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 20);
    attacker = await fundedWallet(connection, 20);

    const { usdcMint, poolCounter, operator: op } = await ensureProtocol(connection, admin);
    operator = op;

    collection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "F-2 custody fixture collection",
    });

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], createWiIx)]);

    poolId = poolCounter;
    const poolAccounts = await createPool({
      connection,
      administrator: admin,
      poolId,
      weightIndex: weightIndexSigner.address,
      usdcMint,
      args: { ...launchProfile(admin.address) },
    });
    pool = poolAccounts.pool;

    // `init_pool` creates no admission record, so the pool takes no deposit until this runs.
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
    });
  });

  /** Mints, deposits, and rejects one admissible pNFT — the shared precondition both cases below
   * need before `return_rejected` is even callable (`position.state == Rejected`). */
  async function depositThenReject(label: string): Promise<AdmissibleFixture> {
    const fixture = await mintAdmissiblePnft({
      connection,
      payer: admin,
      authority: admin,
      owner: depositor.address,
      collection,
      value: 20_000_000n,
      label,
    });

    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(fixture.mint),
    ]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    const depositorTokenRecord = await tokenRecordPda(fixture.mint, fixture.tokenAccount);
    const positionTokenRecord = await tokenRecordPda(fixture.mint, positionVault);

    await sendIxs(connection, depositor, [
      await deposit({
        depositor: depositor.address,
        poolId,
        collection: collection.mint,
        nftMint: fixture.mint,
        depositorToken: fixture.tokenAccount,
        metadata: fixture.metadata,
        masterEdition: fixture.masterEdition,
        depositorTokenRecord,
        positionTokenRecord,
        sysvarInstructions: SYSVAR_INSTRUCTIONS,
        authorizationRulesProgram: AUTH_RULES_PROGRAM,
        authorizationRules: RULE_SET_ADDRESS,
      }),
    ]);

    await sendIxs(connection, operator, [
      await rejectDeposit({
        operator: operator.address,
        pool,
        nftMint: fixture.mint,
        reason: REJECT_REASON,
      }),
    ]);

    return fixture;
  }

  it("control: return_rejected against the true depositor's own, already-existing ATA succeeds and the NFT lands with the depositor", async () => {
    const fixture = await depositThenReject("control");

    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(fixture.mint),
    ]);
    const positionTokenRecord = await tokenRecordPda(
      fixture.mint,
      (await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)])).address,
    );

    // The depositor's own ATA for this mint already exists — `mintAdmissiblePnft` created it as
    // the mint destination, and `deposit` only emptied it, never closed it. It is already the
    // "already exists" branch F-2 is about; no separate `Create` is needed or even valid here.
    const depositorAta = fixture.tokenAccount;
    const depositorTokenRecord = await tokenRecordPda(fixture.mint, depositorAta);

    await sendIxs(connection, attacker, [
      await returnRejected({
        payer: attacker.address,
        depositor: depositor.address,
        pool,
        nftMint: fixture.mint,
        depositorToken: depositorAta,
        metadata: fixture.metadata,
        masterEdition: fixture.masterEdition,
        positionTokenRecord,
        depositorTokenRecord,
        sysvarInstructions: SYSVAR_INSTRUCTIONS,
        authorizationRulesProgram: AUTH_RULES_PROGRAM,
        authorizationRules: RULE_SET_ADDRESS,
      }),
    ]);

    const balance = await connection.getTokenAccountBalance({ tokenAccount: depositorAta });
    assert.equal(balance.amount, 1n, "the true depositor's own ATA must hold the returned pNFT");
  });

  it("attack: return_rejected against an attacker-owned, already-existing depositor_token — Token Metadata's TransferV1 rejects it with IncorrectOwner (57)", async () => {
    const fixture = await depositThenReject("attack");

    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(fixture.mint),
    ]);
    const positionTokenRecord = await tokenRecordPda(
      fixture.mint,
      (await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)])).address,
    );

    const { address: attackerAta, instruction: createAtaIx } = await createAtaInstruction({
      payer: attacker.address,
      owner: attacker.address,
      mint: fixture.mint,
    });
    await sendIxs(connection, attacker, [createAtaIx]);
    const depositorTokenRecord = await tokenRecordPda(fixture.mint, attackerAta);

    // Asserted by the exact numeric code, never a bare `assert.rejects` — a transaction that
    // failed to build, or rejected for an unrelated reason, must not read as this finding.
    await expectAnchorError(
      sendIxs(connection, attacker, [
        await returnRejected({
          payer: attacker.address,
          depositor: depositor.address,
          pool,
          nftMint: fixture.mint,
          depositorToken: attackerAta,
          metadata: fixture.metadata,
          masterEdition: fixture.masterEdition,
          positionTokenRecord,
          depositorTokenRecord,
          sysvarInstructions: SYSVAR_INSTRUCTIONS,
          authorizationRulesProgram: AUTH_RULES_PROGRAM,
          authorizationRules: RULE_SET_ADDRESS,
        }),
      ]),
      TOKEN_METADATA_INCORRECT_OWNER,
    );

    // The rejected transfer must have moved nothing: the attacker's ATA stays empty, and the
    // pNFT is still in the position's vault — the position was never touched, let alone closed.
    const attackerBalance = await connection.getTokenAccountBalance({ tokenAccount: attackerAta });
    assert.equal(attackerBalance.amount, 0n, "the attacker's ATA must remain empty after the rejected transfer");

    const positionVault = (await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)])).address;
    const vaultBalance = await connection.getTokenAccountBalance({ tokenAccount: positionVault });
    assert.equal(vaultBalance.amount, 1n, "the pNFT must still be held in the position's vault");
  });
});
