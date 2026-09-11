// D-041 rule-set replay (T-48) — exercises deposit → escrow → withdraw against each historical
// revision of the eBJLFY…zyT9 rule-set account in turn, and records the measured per-revision
// outcome. Feasibility (lead resolution 5): all revisions live inside the one live account, so
// this file (via tests/helpers/rule-set.ts) parses its own trailing revision map rather than
// trusting a hardcoded count, and asserts that count against the manifest T-46/T-48 committed.
//
// The account holds 9 revisions, not the 7 the original D-041 row assumed — see rule-set.ts's
// doc comment and this file's own structural guard below. Revisions 0–6 are restrictive
// (`All[Amount==1, Any[ProgramOwnedList(14 marketplace programs), Source|Destination|Authority]]`,
// bye_machine among none of the 14) and reject; 7 and 8 are permissive and accept. Rejections are
// asserted by the *measured* exact numeric error code (never a bare "it failed") — and revisions
// 0–1 fail with a *different* code (18) than 2–6 (11), traced by msgpack decode to a schema
// difference: 0–1 key their catch-all rule `"Transfer:Base"`, 2–6 use bare `"Transfer"`, and the
// currently-deployed Auth Rules program only recognizes the latter as the `Transfer:Owner`
// scenario's `"Namespace"` (defer-to-catch-all) target. Collapsing these into one expected code
// would have passed 0–1 for the wrong reason.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadSuiteAdmin,
  address,
  type Address,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPnftCollection, mintAdmissiblePnft, tokenRecordPda, type PnftCollection } from "../helpers/fixtures.js";
import { launchProfile, createPool, admitPoolCollection } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { parseRevisionMap, readRuleSetAccountData, REVISION_RULE_SET_ADDRESSES } from "../helpers/rule-set.js";
import { deposit, approveDeposit } from "../../scripts/lib/deposit.js";
import { withdraw } from "../../scripts/lib/exit.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { sendIxs } from "../../scripts/_common.js";
import { AUTH_RULES_PROGRAM, SYSVAR_INSTRUCTIONS } from "../../scripts/constants.js";

const RULE_SET_ACCOUNT_PATH = "tests/fixtures/programs/rule-set.json";
// Verbatim against programs/bye_machine/src/common/seeds.rs — same convention deposit.ts/exit.ts
// each already follow (this file derives position/position_vault independently of those encoders'
// own internal derivation, since neither returns the intermediate PDA a token-record needs).
const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";

/** One row of the measured outcome table this file's own describe block builds. */
type RevisionExpectation =
  | { kind: "rejected"; errorCode: number }
  | { kind: "accepted" };

// Measured against a live validator (see the file header) — not presumed from the architecture
// doc, which only characterizes revision 8 as permissive and revisions 0–6 as uniformly
// restrictive without distinguishing 0–1's rejection reason from 2–6's.
const EXPECTED: readonly RevisionExpectation[] = [
  { kind: "rejected", errorCode: 18 }, // revision 0 — legacy "Transfer:Base" catch-all key
  { kind: "rejected", errorCode: 18 }, // revision 1 — same
  { kind: "rejected", errorCode: 11 }, // revision 2 — bare "Transfer" catch-all, rule evaluated
  { kind: "rejected", errorCode: 11 }, // revision 3
  { kind: "rejected", errorCode: 11 }, // revision 4
  { kind: "rejected", errorCode: 11 }, // revision 5
  { kind: "rejected", errorCode: 11 }, // revision 6
  { kind: "accepted" }, // revision 7 — already migrated to per-scenario Pass, no catch-all
  { kind: "accepted" }, // revision 8 — operative, all-Pass (architecture doc §5)
];

describe("T-48 rule-set replay — D-041", () => {
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
      name: "T-48 replay collection",
    });

    const { signer: weightIndexSigner, instruction: createWiIx } = await createWeightIndexAccount({
      rpc: connection,
      payer: admin.address,
    });
    await sendIxs(connection, admin, [addSignersToInstruction([admin, weightIndexSigner], createWiIx)]);
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

    // `init_pool` creates no admission record, so the pool takes no deposit until this runs.
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
    });
  });

  // The discriminating control: a revision-map parser that silently returned 0 (or any count
  // other than what the account actually holds) would let every case below "pass" having replayed
  // nothing. `parseRevisionMap` throws rather than returning zero on a short/malformed buffer —
  // see tests/helpers/rule-set.ts — so this assertion is the only thing standing between a
  // genuine 9-revision replay and a vacuous one.
  it("the eBJLFY…zyT9 account's own revision map parses to exactly 9 revisions", () => {
    const data = readRuleSetAccountData(RULE_SET_ACCOUNT_PATH);
    const map = parseRevisionMap(data);
    assert.equal(map.count, 9);
    assert.equal(map.offsets.length, 9);
    assert.equal(
      REVISION_RULE_SET_ADDRESSES.length,
      map.count,
      "the fixture manifest's address count must track the account's own revision count",
    );
    assert.equal(EXPECTED.length, map.count, "replay exactly the number of revisions found, no more, no fewer");
  });

  for (let i = 0; i < 9; i++) {
    const expectation = EXPECTED[i];
    it(`revision ${i}: deposit → escrow → withdraw is ${expectation.kind}`, async () => {
      const ruleSet = address(REVISION_RULE_SET_ADDRESSES[i]);
      const fixture = await mintAdmissiblePnft({
        connection,
        payer: admin,
        authority: admin,
        owner: depositor.address,
        collection,
        value: 20_000_000n,
        label: `rev${i}`,
        ruleSet,
      });

      const { address: position } = await derivePda([
        POSITION_SEED,
        seedFromPubkey(pool),
        seedFromPubkey(fixture.mint),
      ]);
      const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
      const depositorTokenRecord = await tokenRecordPda(fixture.mint, fixture.tokenAccount);
      const positionTokenRecord = await tokenRecordPda(fixture.mint, positionVault);

      const depositIx = await deposit({
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
        authorizationRules: ruleSet,
      });

      if (expectation.kind === "rejected") {
        await expectAnchorError(sendIxs(connection, depositor, [depositIx]), expectation.errorCode);
        return;
      }

      await sendIxs(connection, depositor, [depositIx]);

      const now = BigInt(Math.floor(Date.now() / 1000));
      await sendIxs(connection, operator, [
        await approveDeposit({
          operator: operator.address,
          poolId,
          nftMint: fixture.mint,
          depositor: depositor.address,
          weightIndex,
          displaced: null,
          value: 20_000_000n,
          observedAt: now,
          // No Season-0 lock: this replay's subject is the rule set, and it withdraws immediately
          // afterwards. `0` is the non-Season-0 value the attestor sends outside a lock window.
          lockUntil: 0n,
        }),
      ]);

      await sendIxs(connection, depositor, [
        await withdraw({
          depositor: depositor.address,
          poolId,
          nftMint: fixture.mint,
          weightIndex,
          depositorUsdc: null,
          depositorToken: fixture.tokenAccount,
          metadata: fixture.metadata,
          masterEdition: fixture.masterEdition,
          positionTokenRecord,
          depositorTokenRecord,
          sysvarInstructions: SYSVAR_INSTRUCTIONS,
          authorizationRulesProgram: AUTH_RULES_PROGRAM,
          authorizationRules: ruleSet,
          successor: null,
        }),
      ]);

      // "Each success asserted by the NFT actually returning" (owner ruling) — not just that
      // withdraw's transaction didn't throw.
      const balance = await connection.getTokenAccountBalance({ tokenAccount: fixture.tokenAccount });
      assert.equal(balance.amount, 1n, "the depositor's own token account must hold the pNFT again");

      const positionAccountAfter = await connection.rpc.getAccountInfo(position).send();
      assert.equal(positionAccountAfter.value, null, "withdraw closes the position account");
    });
  }
});
