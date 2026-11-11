// The remaining four: `update_config`, `set_pause`, `set_authorities`, `update_tier` had never
// executed against a validator at all — the defect class this file exists to close has been found
// once already (a writable-sentinel bug that passed every unit instrument, in an encoder that
// *does* run). An unexecuted
// encoder is a place that class sits undetected.
//
// Each case below is a lifecycle pair: the wrong signer refused, then the real one observed
// succeeding and changing exactly the state the instruction claims to change — never `is_err()`
// or a bare assertion that the call didn't throw.

import { before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { addSignersToInstruction, type Address } from "@solana/kit";
import {
  assertProgramIsLive,
  expectAnchorError,
  fundedWallet,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  loadT02Authority,
  withAccountAt,
  withSigner,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { createPool, launchProfile, admitPoolCollection, smallTierProfile } from "../helpers/pools.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { createPnftCollection, mintPlainNonFungiblePnft, type PnftCollection } from "../helpers/fixtures.js";
import { createWeightIndexAccount } from "../../scripts/lib/weight-index.js";
import { setAuthorities, setPause, updateConfig } from "../../scripts/lib/admin.js";
import { deposit, approveDeposit } from "../../scripts/lib/deposit.js";
import { beginSweep, endSweep, recordValue } from "../../scripts/lib/value.js";
import { updateTier } from "../../scripts/lib/tier.js";
import {
  decodeEvent,
  decodePool,
  decodePosition,
  decodeProtocolConfig,
  type DecodedEvent,
} from "../../scripts/lib/decoders.js";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { SYSTEM_PROGRAM } from "../../scripts/constants.js";
import { UNAUTHORIZED } from "../helpers/authority-matrix.js";

const AUTHORITY_UNSET = 6517;
const AUTHORITY_UNCHANGED = 6518;

const POSITION_SEED = "position";
const PROTOCOL_CONFIG_SEED = "protocol";
const PROGRAM_DATA_PREFIX = "Program data: ";

function isTierChanged(
  event: DecodedEvent,
): event is Extract<DecodedEvent, { name: "TierChanged" }> {
  return event.name === "TierChanged";
}

function nowSeconds(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

describe("the four encoders never executed against a validator", () => {
  let connection: Connection;
  let admin: TransactionSigner;
  let operator: TransactionSigner;
  let depositor: TransactionSigner;
  let stranger: TransactionSigner;
  let poolId: number;
  let pool: Address;
  let weightIndex: Address;
  let collection: PnftCollection;
  let protocolConfig: Address;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();
    depositor = await fundedWallet(connection, 20);
    stranger = await fundedWallet(connection, 20);

    const { usdcMint, poolCounter, operator: op } = await ensureProtocol(connection, admin);
    operator = op;
    protocolConfig = (await derivePda([PROTOCOL_CONFIG_SEED])).address;

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
      // Small tier: update_tier's positive case needs a position that is Active and not yet in
      // the tier, which any admitted deposit above the floor already is (nothing joins the tier
      // at approval time — only update_tier and record_value's crossing path do).
      args: { ...smallTierProfile() },
    });
    pool = poolAccounts.pool;

    collection = await createPnftCollection({
      connection,
      payer: admin,
      authority: admin,
      name: "unexecuted-encoders fixtures",
    });
    await admitPoolCollection({
      connection,
      administrator: admin,
      poolId,
      collection: collection.mint,
      standards: 0b011,
    });
  });

  async function eventsOf(signature: string): Promise<DecodedEvent[]> {
    const logs = await connection.getLogs(signature);
    return logs
      .filter((line) => line.startsWith(PROGRAM_DATA_PREFIX))
      .map((line) => decodeEvent(Buffer.from(line.slice(PROGRAM_DATA_PREFIX.length), "base64")));
  }

  it("update_config: a stranger is refused; the administrator is not", async () => {
    const before_ = decodePool((await fetchAccountData(connection, pool))!);
    const raisedFloor = before_.admissionFloor + 1_000_000n;

    await expectAnchorError(
      sendIxs(connection, stranger, [
        await updateConfig({
          administrator: stranger.address,
          poolId,
          update: { kind: "AdmissionFloor", value: raisedFloor },
          demoted: [],
        }),
      ]),
      UNAUTHORIZED,
    );
    assert.equal(
      decodePool((await fetchAccountData(connection, pool))!).admissionFloor,
      before_.admissionFloor,
      "the refused call raised the floor anyway",
    );

    await sendIxs(connection, admin, [
      await updateConfig({
        administrator: admin.address,
        poolId,
        update: { kind: "AdmissionFloor", value: raisedFloor },
        demoted: [],
      }),
    ]);
    assert.equal(
      decodePool((await fetchAccountData(connection, pool))!).admissionFloor,
      raisedFloor,
      "the administrator's call did not raise the floor",
    );

    // Restored: later cases in this file assume the profile's own admission floor.
    await sendIxs(connection, admin, [
      await updateConfig({
        administrator: admin.address,
        poolId,
        update: { kind: "AdmissionFloor", value: before_.admissionFloor },
        demoted: [],
      }),
    ]);
    assert.equal(
      decodePool((await fetchAccountData(connection, pool))!).admissionFloor,
      before_.admissionFloor,
      "the restore did not land",
    );
  });

  it("set_pause: a stranger is refused; the administrator is not", async () => {
    const before_ = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(before_.depositsPaused, false, "the pool must start unpaused for this row to prove anything");

    await expectAnchorError(
      sendIxs(connection, stranger, [
        await setPause({ administrator: stranger.address, poolId, deposits: true, rolls: false, reason: 1 }),
      ]),
      UNAUTHORIZED,
    );
    assert.equal(
      decodePool((await fetchAccountData(connection, pool))!).depositsPaused,
      false,
      "the refused call paused deposits anyway",
    );

    await sendIxs(connection, admin, [
      await setPause({ administrator: admin.address, poolId, deposits: true, rolls: false, reason: 1 }),
    ]);
    const paused = decodePool((await fetchAccountData(connection, pool))!);
    assert.equal(paused.depositsPaused, true, "the administrator's call did not pause deposits");
    assert.equal(paused.rollsPaused, false, "rolls_paused moved when only deposits was set");
    assert.equal(paused.pauseReason, 1);

    // Restored: later deposits in this file must not be refused by a pause this row set.
    await sendIxs(connection, admin, [
      await setPause({ administrator: admin.address, poolId, deposits: false, rolls: false, reason: 0 }),
    ]);
    assert.equal(
      decodePool((await fetchAccountData(connection, pool))!).depositsPaused,
      false,
      "the restore did not land",
    );
  });

  it("set_authorities: only the administrator can rotate, bad keys are refused, and the round-trip restores the original", async () => {
    const original = await loadT02Authority();
    const replacement = await fundedWallet(connection, 5);

    const configBefore = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    assert.equal(configBefore.t02Authority, original.address, "fixture assumption: t02 starts at the local suite key");

    // Refused from a stranger, accepted from the administrator; the live authority moves at once.
    await expectAnchorError(
      sendIxs(connection, stranger, [
        await setAuthorities({ signer: stranger.address, role: "T02", newAuthority: replacement.address }),
      ]),
      UNAUTHORIZED,
    );
    await sendIxs(connection, admin, [
      await setAuthorities({ signer: admin.address, role: "T02", newAuthority: replacement.address }),
    ]);
    const rotated = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    assert.equal(rotated.t02Authority, replacement.address, "the rotation did not move the live authority");

    // The default key and a no-op rotation are refused, and the reserved slots stay untouched.
    await expectAnchorError(
      sendIxs(connection, admin, [
        await setAuthorities({ signer: admin.address, role: "T02", newAuthority: SYSTEM_PROGRAM }),
      ]),
      AUTHORITY_UNSET,
    );
    await expectAnchorError(
      sendIxs(connection, admin, [
        await setAuthorities({ signer: admin.address, role: "T02", newAuthority: replacement.address }),
      ]),
      AUTHORITY_UNCHANGED,
    );
    assert.deepEqual(
      [rotated.reserved0, rotated.reserved1, rotated.reserved2, rotated.reserved3],
      [configBefore.reserved0, configBefore.reserved1, configBefore.reserved2, configBefore.reserved3],
      "a reserved slot was written",
    );

    // Restored: every other file in this suite reads t02Authority from the local suite keypair.
    await sendIxs(connection, admin, [
      await setAuthorities({ signer: admin.address, role: "T02", newAuthority: original.address }),
    ]);
    const restored = decodeProtocolConfig((await fetchAccountData(connection, protocolConfig))!);
    assert.equal(restored.t02Authority, original.address, "the restore did not land");
  });

  it("update_tier: a stranger is refused; the Operator is not, and only an eligible candidate is promoted", async () => {
    // tier_size is 3 (smallTierProfile). Filling it with three high-value members first makes
    // the fourth's own approval reject tier entry — Active, not in_tier, gracefully, per
    // activate_position's PromoteOutcome::Rejected — without erroring the deposit itself. That
    // rejected-but-Active position is update_tier's only reachable positive case: nothing else
    // in the crate promotes a waiting candidate once the tier is already full.
    async function depositAndApprove(value: bigint): Promise<Address> {
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

    const highValueMembers = [];
    for (let i = 0; i < 3; i += 1) {
      highValueMembers.push(await depositAndApprove(50_000_000n));
    }
    const candidateMint = await depositAndApprove(13_000_000n);
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(candidateMint),
    ]);
    const rejected = decodePosition((await fetchAccountData(connection, position))!);
    assert.equal(rejected.state, "Active", "the candidate must be Active for update_tier to have anything eligible");
    assert.equal(rejected.inTier, false, "the full tier must have rejected this deposit's own entry attempt");

    await expectAnchorError(
      sendIxs(connection, stranger, [
        withSigner(
          await updateTier({ operator: operator.address, poolId, nftMint: candidateMint, displaced: null }),
          stranger.address,
        ),
      ]),
      UNAUTHORIZED,
    );
    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).inTier,
      false,
      "the refused call promoted the position anyway",
    );

    // Still full: the candidate does not outrank any of the three high-value members.
    await expectAnchorError(
      sendIxs(connection, operator, [
        await updateTier({ operator: operator.address, poolId, nftMint: candidateMint, displaced: null }),
      ]),
      6305, // CandidateOutranked
    );

    // The account-substitution sweep's `update_tier.position` row (a `Partial`-anchored slot —
    // registered in `tests/helpers/substitution-sweep.ts`): a same-pool sibling that is already
    // in_tier passes update_tier's own seeds check (both derive from their own nft_mint) but is
    // no longer an eligible candidate — is_eligible_candidate rejects it independent of seeds.
    const { address: siblingPosition } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(highValueMembers[1]!),
    ]);
    assert.equal(
      decodePosition((await fetchAccountData(connection, siblingPosition))!).inTier,
      true,
      "the substituted sibling must itself be in the tier for this row to prove anything",
    );
    await expectAnchorError(
      sendIxs(connection, operator, [
        withAccountAt(
          await updateTier({ operator: operator.address, poolId, nftMint: candidateMint, displaced: null }),
          4,
          siblingPosition,
        ),
      ]),
      6301, // InvalidPositionState
    );

    // Free a slot: close one high-value member below the floor.
    await sendIxs(connection, operator, [await beginSweep({ operator: operator.address, poolId })]);
    await sendIxs(connection, operator, [
      await recordValue({
        operator: operator.address,
        poolId,
        nftMint: highValueMembers[0]!,
        depositor: depositor.address,
        weightIndex,
        value: 1_000_000n,
        observedAt: nowSeconds(),
        effect: { kind: "close", successor: null },
      }),
    ]);
    await sendIxs(connection, operator, [await endSweep({ operator: operator.address, poolId })]);

    const promotionSignature = await sendIxs(connection, operator, [
      await updateTier({ operator: operator.address, poolId, nftMint: candidateMint, displaced: null }),
    ]);
    assert.equal(
      decodePosition((await fetchAccountData(connection, position))!).inTier,
      true,
      "the Operator's call did not promote the position once a slot opened",
    );

    // TierChanged's one positive-firing observation point in the whole suite: every other case
    // either asserts the event does not fire (core-seizure.test.ts) or decodes it from synthetic,
    // hand-built bytes (decoders.test.ts, idl-state-oracle.test.ts) — an oracle that recomputes
    // the expression under test cannot disagree with it. This is the real thing, off a real log.
    const tierChanged = (await eventsOf(promotionSignature)).filter(isTierChanged);
    assert.equal(tierChanged.length, 1, "expected exactly one TierChanged event");
    assert.equal(tierChanged[0].data.pool, pool);
    assert.equal(tierChanged[0].data.entered, position, "the candidate's own position must be the one that entered");
    assert.equal(tierChanged[0].data.left, null, "a freed slot is an insertion, not a swap — nothing should leave");

    // A position already in the tier is no longer an eligible candidate.
    await expectAnchorError(
      sendIxs(connection, operator, [
        await updateTier({ operator: operator.address, poolId, nftMint: candidateMint, displaced: null }),
      ]),
      6301, // InvalidPositionState
    );
  });
});
