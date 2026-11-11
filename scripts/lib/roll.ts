// Instruction encoders for the roll-pipeline instructions: commit_rolls, vrf_callback,
// settle_roll, settle_roll_core, recover_batch, close_batch. Account order and arg shapes are derived mechanically from
// idl/bye_machine.json, cross-checked against each instruction's #[derive(Accounts)]
// struct in programs/bye_machine/src/instructions/roll/.
//
// Pure and synchronous with respect to I/O, same discipline as the other domain files: every
// state-dependent account — the pool's `batch_counter` at commit time, the batch's own `pool`
// and `batch_id` at callback time, `protocol_config.oracle_queue`, the roller's own USDC token
// account — is a required, non-defaulted parameter. These encoders derive this program's own
// PDAs but never read on-chain state themselves.

import type { Address, Instruction } from "@solana/kit";
import {
  Borsh,
  buildAnchorIx,
  derivePda,
  readonly,
  optionalAddress,
  seedFromPubkey,
  seedFromU64Le,
  signer,
  writable,
  writableSigner,
} from "./anchor.js";
import { deriveSharedTreasuries } from "./admin.js";
import {
  ASSOCIATED_TOKEN_PROGRAM,
  CP_AMM_EVENT_AUTHORITY_SEED,
  CP_AMM_POOL_AUTHORITY,
  CP_AMM_PROGRAM,
  MPL_CORE_PROGRAM,
  PROGRAM_ID,
  SYSTEM_PROGRAM,
  SYSVAR_SLOT_HASHES,
  TOKEN_METADATA_PROGRAM,
  TOKEN_PROGRAM,
  VRF_PROGRAM,
} from "../constants.js";

// ── Seeds (bye_machine-specific — anchor.ts stays program-agnostic) ─────────────────────────
// Verbatim against programs/bye_machine/src/common/seeds.rs.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const TOP_TIER_SEED = "tier";
export const BATCH_SEED = "batch";
const PRINCIPAL_VAULT_SEED = "principal";
const VRF_IDENTITY_SEED = "identity";
const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const WALLET_STATS_SEED = "wallet";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

// ── commit_rolls ─────────────────────────────────────────────────────────────────────────────
/**
 * One arg, `n` (u8), and a 14-account list. `batchCounter` is `pool.batch_counter` **before**
 * this call increments it — `commit_rolls.rs`'s own doc comment calls this out, since the batch
 * PDA is seeded on the pre-increment value, not the post-increment one this call writes.
 *
 * `oracleQueue` and `usdcMint` both name accounts the instruction validates against
 * `protocol_config`'s own stored fields rather than deriving — they are runtime configuration,
 * not PDAs, so the caller supplies them same as `rollerUsdc`.
 */
export async function commitRolls(params: {
  roller: Address;
  poolId: number;
  batchCounter: bigint;
  rollerUsdc: Address;
  usdcMint: Address;
  oracleQueue: Address;
  n: number;
}): Promise<Instruction> {
  const { roller, poolId, batchCounter, rollerUsdc, usdcMint, oracleQueue, n } = params;

  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: batch } = await derivePda([
    BATCH_SEED,
    seedFromPubkey(pool),
    seedFromU64Le(batchCounter),
  ]);
  const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
  const { address: programIdentity } = await derivePda([VRF_IDENTITY_SEED]);

  return buildAnchorIx({
    name: "commit_rolls",
    args: new Borsh().u8(n).build(),
    accounts: [
      writableSigner(roller),
      readonly(protocolConfig),
      writable(pool),
      readonly(topTier),
      writable(batch),
      writable(rollerUsdc),
      writable(principalVault),
      readonly(usdcMint),
      writable(oracleQueue),
      readonly(programIdentity),
      readonly(VRF_PROGRAM),
      readonly(SYSVAR_SLOT_HASHES),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── vrf_callback ─────────────────────────────────────────────────────────────────────────────
/**
 * One arg, `randomness` (`[u8; 32]`), and a 3-account list. `pool` and `batchId` are read off
 * the target `RollBatch` itself (`batch.pool`, `batch.batch_id`) rather than derived from a
 * `poolId` — they are the values the batch was actually committed against.
 *
 * `vrfProgramIdentity` is not a PDA of this program: it is the VRF provider's own
 * per-callback-program identity, `find_program_address([b"identity", PROGRAM_ID], VRF_PROGRAM)`
 * (`ephemeral-vrf-sdk`'s `scoped_vrf_identity`) — seeded with this program's id but derived
 * under the VRF program, which is why `derivePda`'s second argument names `VRF_PROGRAM` rather
 * than the default.
 */
export async function vrfCallback(params: {
  pool: Address;
  batchId: bigint;
  randomness: Uint8Array;
}): Promise<Instruction> {
  const { pool, batchId, randomness } = params;

  const { address: batch } = await derivePda([
    BATCH_SEED,
    seedFromPubkey(pool),
    seedFromU64Le(batchId),
  ]);
  const { address: vrfProgramIdentity } = await derivePda(
    [VRF_IDENTITY_SEED, seedFromPubkey(PROGRAM_ID)],
    VRF_PROGRAM,
  );

  return buildAnchorIx({
    name: "vrf_callback",
    args: new Borsh().fixed(randomness, 32).build(),
    accounts: [signer(vrfProgramIdentity), writable(batch), readonly(pool)],
  });
}

// ── test_resolve_batch ───────────────────────────────────────────────────────────────────────
/**
 * One arg, `randomness` (`[u8; 32]`), and a 4-account list. Present only in a program built with
 * the `testing` feature — the devnet and mainnet artifacts carry no discriminator for it.
 *
 * Same transition as `vrfCallback` above and deliberately so: it stands in for a delivery the
 * suite cannot produce, because the callback's signer is an identity no party to this system
 * holds. What differs is the authority — the Operator signs here, so the instruction is not
 * permissionless even in the build that has it.
 */
export async function testResolveBatch(params: {
  operator: Address;
  pool: Address;
  batchId: bigint;
  randomness: Uint8Array;
}): Promise<Instruction> {
  const { operator, pool, batchId, randomness } = params;

  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: batch } = await derivePda([
    BATCH_SEED,
    seedFromPubkey(pool),
    seedFromU64Le(batchId),
  ]);

  return buildAnchorIx({
    name: "test_resolve_batch",
    args: new Borsh().fixed(randomness, 32).build(),
    accounts: [signer(operator), readonly(protocolConfig), writable(batch), readonly(pool)],
  });
}

// ── settle_roll ──────────────────────────────────────────────────────────────────────────────
/**
 * One arg, `rollIndex` (u8), and a 29-account list. The index is not optional and not inferable
 * here: rolls settle strictly in order and the program refuses anything but `batch.next_roll`, so
 * the caller reads that field off the batch rather than counting locally.
 *
 * `rollerUsdc` is the refund destination for a roll that ends refunded, validated on-chain
 * against `batch.roller` rather than derived — same as `recoverBatch` below. `protocolRevenue` is
 * `protocol_config.protocol_revenue`, read live and pinned by an `address` constraint, so a wrong
 * account fails validation rather than misrouting the protocol's fee share.
 *
 * **Sixteen of the twenty-nine are optional, and they are optional together.** Everything from
 * `position` through `sysvar_instructions` describes the position a real card draw settles; a
 * roll that draws nothing supplies none of them. Key bytes alone crowd a legacy transaction's
 * envelope at this width, so an operator's settlement transaction is a v0 transaction with a
 * per-pool address lookup table over the static keys.
 */
export async function settleRoll(params: {
  operator: Address;
  pool: Address;
  batchId: bigint;
  rollIndex: number;
  rollerUsdc: Address;
  protocolRevenue: Address;
  /**
   * Treasury 05 — the USDC associated token account of `protocol_config.t05_authority`,
   * credited 25% of this roll's protocol share. Passed rather than derived, on
   * `protocolRevenue`'s own terms: the authority it hangs off is config this encoder does
   * not read. This pool's Treasury 03, the other new destination, *is* derived — it is a
   * PDA of the pool, like Treasury 04.
   */
  treasury05: Address;
  weightIndex: Address;
  /** The batch's own roller — the card's destination owner, pinned on-chain to `batch.roller`. */
  roller: Address;
  /**
   * The drawn card's mint, and with it every account bound to the position holding it. **`null`
   * on a roll that settles no position** — a roll over an emptied pool draws nothing, and there
   * is no position for the caller to name. The operator predicts the draw to build this list at
   * all; a wrong prediction fails on-chain and costs one retry.
   */
  nftMint: Address | null;
  depositor: Address | null;
  /** The depositor's USDC account, or the pool's Treasury 03 float for a Treasury-02 position. */
  settlementUsdc: Address | null;
  rollerToken: Address | null;
  metadata: Address | null;
  masterEdition: Address | null;
  /**
   * The five pNFT accounts, **required-present on standard 0 and required-absent on standard 1**,
   * on exactly the terms `withdraw` takes them: the settlement reads the standard off the drawn
   * position and then requires this shape, so a caller that omitted the field instead of passing
   * `null` would encode a legacy release it never chose.
   */
  positionTokenRecord: Address | null;
  rollerTokenRecord: Address | null;
  sysvarInstructions: Address | null;
  authorizationRulesProgram: Address | null;
  authorizationRules: Address | null;
  /** The optional tier successor, read from `remaining_accounts` only if the drawn position was a member. */
  successor: Address | null;
  /**
   * The venue accounts a **drawn blank** settles through, or `null` on a roll that draws a card.
   * They travel as one group because the blank leg needs all of them and the card leg needs none;
   * the three the venue itself fixes — its program, its global authority and its event PDA — are
   * derived here rather than taken, so a caller cannot supply a different one.
   */
  blank: {
    swapPool: Address;
    rollerBye: Address;
    byeMint: Address;
    usdcMint: Address;
    tokenAVault: Address;
    tokenBVault: Address;
  } | null;
}): Promise<Instruction> {
  const {
    operator,
    pool,
    batchId,
    rollIndex,
    rollerUsdc,
    protocolRevenue,
    treasury05,
    weightIndex,
    roller,
    nftMint,
    depositor,
    settlementUsdc,
    rollerToken,
    metadata,
    masterEdition,
    positionTokenRecord,
    rollerTokenRecord,
    sysvarInstructions,
    authorizationRulesProgram,
    authorizationRules,
    successor,
    blank,
  } = params;

  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: batch } = await derivePda([
    BATCH_SEED,
    seedFromPubkey(pool),
    seedFromU64Le(batchId),
  ]);
  const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
  const { treasury03, treasury04 } = await deriveSharedTreasuries();

  // Derived here rather than taken as parameters, because all three are this program's own PDAs
  // over accounts the caller already names. They travel together with `nftMint`/`depositor`: a
  // roll that settles no position supplies none of them.
  const position =
    nftMint === null
      ? null
      : (await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(nftMint)])).address;
  const positionVault =
    position === null
      ? null
      : (await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)])).address;
  const walletStats =
    depositor === null
      ? null
      : (await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]))
          .address;

  const { address: cpAmmEventAuthority } = await derivePda(
    [CP_AMM_EVENT_AUTHORITY_SEED],
    CP_AMM_PROGRAM,
  );

  return buildAnchorIx({
    name: "settle_roll",
    args: new Borsh().u8(rollIndex).build(),
    accounts: [
      writableSigner(operator),
      readonly(protocolConfig),
      writable(pool),
      writable(topTier),
      writable(batch),
      writable(principalVault),
      writable(rollerUsdc),
      writable(treasury04),
      writable(protocolRevenue),
      writable(treasury03),
      writable(treasury05),
      writable(weightIndex),
      // A `None` optional is passed as the program's own id, and a program address may not be
      // marked writable — so each placeholder takes the readonly role while a present account
      // keeps the writable one the settlement needs.
      position === null ? readonly(optionalAddress(position)) : writable(position),
      walletStats === null ? readonly(optionalAddress(walletStats)) : writable(walletStats),
      depositor === null ? readonly(optionalAddress(depositor)) : writable(depositor),
      settlementUsdc === null
        ? readonly(optionalAddress(settlementUsdc))
        : writable(settlementUsdc),
      positionVault === null ? readonly(optionalAddress(positionVault)) : writable(positionVault),
      readonly(optionalAddress(nftMint)),
      readonly(roller),
      rollerToken === null ? readonly(optionalAddress(rollerToken)) : writable(rollerToken),
      metadata === null ? readonly(optionalAddress(metadata)) : writable(metadata),
      readonly(optionalAddress(masterEdition)),
      positionTokenRecord === null
        ? readonly(optionalAddress(positionTokenRecord))
        : writable(positionTokenRecord),
      rollerTokenRecord === null
        ? readonly(optionalAddress(rollerTokenRecord))
        : writable(rollerTokenRecord),
      readonly(TOKEN_METADATA_PROGRAM),
      readonly(optionalAddress(authorizationRulesProgram)),
      readonly(optionalAddress(authorizationRules)),
      readonly(optionalAddress(sysvarInstructions)),
      blank === null ? readonly(optionalAddress(null)) : writable(blank.swapPool),
      blank === null ? readonly(optionalAddress(null)) : writable(blank.rollerBye),
      readonly(optionalAddress(blank === null ? null : blank.byeMint)),
      readonly(optionalAddress(blank === null ? null : blank.usdcMint)),
      blank === null ? readonly(optionalAddress(null)) : writable(blank.tokenAVault),
      blank === null ? readonly(optionalAddress(null)) : writable(blank.tokenBVault),
      readonly(optionalAddress(blank === null ? null : CP_AMM_POOL_AUTHORITY)),
      readonly(optionalAddress(blank === null ? null : cpAmmEventAuthority)),
      readonly(optionalAddress(blank === null ? null : CP_AMM_PROGRAM)),
      readonly(TOKEN_PROGRAM),
      readonly(ASSOCIATED_TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
    remaining: successor !== null ? [writable(successor)] : [],
  });
}

// ── settle_roll_core ─────────────────────────────────────────────────────────────────────────
/**
 * The MPL Core twin: same arg, same guards, same arithmetic, and a **21-account** list rather
 * than 29 — no mint, no metadata, no master edition, no token records, no roller ATA, and no
 * associated-token or auth-rules programs, because a Core asset's owner is a field on the asset
 * itself. What it adds is the asset, its collection and the MPL Core program.
 *
 * **The caller chooses the variant by the drawn position's standard**, which it must predict to
 * build the account list at all. A mis-prediction fails on the standard assert and costs one
 * retry — the same cost as a wrong-position prediction, not a new class of failure.
 *
 * `collection` is not a caller preference: it is the collection the asset's own `update_authority`
 * names, and the program pins it to that. A blank drawn on this variant is refused, not settled —
 * blanks move no NFT, so they settle through `settleRoll` on either variant.
 */
export async function settleRollCore(params: {
  operator: Address;
  pool: Address;
  batchId: bigint;
  rollIndex: number;
  rollerUsdc: Address;
  protocolRevenue: Address;
  /**
   * Treasury 05 — the USDC associated token account of `protocol_config.t05_authority`,
   * credited 25% of this roll's protocol share. Passed rather than derived, on
   * `protocolRevenue`'s own terms: the authority it hangs off is config this encoder does
   * not read. This pool's Treasury 03, the other new destination, *is* derived — it is a
   * PDA of the pool, like Treasury 04.
   */
  treasury05: Address;
  weightIndex: Address;
  roller: Address;
  /** The drawn asset, and with it every position-bound slot. `null` on a roll that draws nothing. */
  asset: Address | null;
  collection: Address | null;
  depositor: Address | null;
  settlementUsdc: Address | null;
  successor: Address | null;
}): Promise<Instruction> {
  const {
    operator,
    pool,
    batchId,
    rollIndex,
    rollerUsdc,
    protocolRevenue,
    treasury05,
    weightIndex,
    roller,
    asset,
    collection,
    depositor,
    settlementUsdc,
    successor,
  } = params;

  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: batch } = await derivePda([
    BATCH_SEED,
    seedFromPubkey(pool),
    seedFromU64Le(batchId),
  ]);
  const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
  const { treasury03, treasury04 } = await deriveSharedTreasuries();

  // On this family the asset account *is* the position's seed component — there is no separate
  // mint — so the position and its accountless vault derive from it exactly as they do on the
  // Token Metadata side.
  const position =
    asset === null
      ? null
      : (await derivePda([POSITION_SEED, seedFromPubkey(pool), seedFromPubkey(asset)])).address;
  const positionVault =
    position === null
      ? null
      : (await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)])).address;
  const walletStats =
    depositor === null
      ? null
      : (await derivePda([WALLET_STATS_SEED, seedFromPubkey(pool), seedFromPubkey(depositor)]))
          .address;

  return buildAnchorIx({
    name: "settle_roll_core",
    args: new Borsh().u8(rollIndex).build(),
    accounts: [
      writableSigner(operator),
      readonly(protocolConfig),
      writable(pool),
      writable(topTier),
      writable(batch),
      writable(principalVault),
      writable(rollerUsdc),
      writable(treasury04),
      writable(protocolRevenue),
      writable(treasury03),
      writable(treasury05),
      writable(weightIndex),
      position === null ? readonly(optionalAddress(position)) : writable(position),
      walletStats === null ? readonly(optionalAddress(walletStats)) : writable(walletStats),
      depositor === null ? readonly(optionalAddress(depositor)) : writable(depositor),
      settlementUsdc === null
        ? readonly(optionalAddress(settlementUsdc))
        : writable(settlementUsdc),
      readonly(optionalAddress(positionVault)),
      asset === null ? readonly(optionalAddress(asset)) : writable(asset),
      collection === null ? readonly(optionalAddress(collection)) : writable(collection),
      readonly(roller),
      readonly(MPL_CORE_PROGRAM),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
    remaining: successor !== null ? [writable(successor)] : [],
  });
}

// ── recover_batch ────────────────────────────────────────────────────────────────────────────
/**
 * No args, an 8-account list. `pool` and `batchId` are read off the target `RollBatch`, same as
 * `vrfCallback` above — recovery acts on a batch that already exists, so nothing here is
 * derived from a `poolId`/`batchCounter` pair the way `commitRolls` is.
 *
 * `rollerUsdc` is the refund destination and is validated on-chain against `batch.roller`'s
 * token account, not derived — the caller supplies it same as `commitRolls`' own `rollerUsdc`.
 * `oracleQueue` is `protocol_config.oracle_queue`, read live rather than derived, exactly as in
 * `commitRolls`.
 */
export async function recoverBatch(params: {
  operator: Address;
  pool: Address;
  batchId: bigint;
  rollerUsdc: Address;
  oracleQueue: Address;
}): Promise<Instruction> {
  const { operator, pool, batchId, rollerUsdc, oracleQueue } = params;

  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: batch } = await derivePda([
    BATCH_SEED,
    seedFromPubkey(pool),
    seedFromU64Le(batchId),
  ]);
  const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);

  return buildAnchorIx({
    name: "recover_batch",
    accounts: [
      signer(operator),
      readonly(protocolConfig),
      writable(pool),
      writable(batch),
      writable(rollerUsdc),
      writable(principalVault),
      readonly(oracleQueue),
      readonly(TOKEN_PROGRAM),
    ],
  });
}

// ── close_batch ──────────────────────────────────────────────────────────────────────────────
/**
 * No args, a 4-account list. `pool` and `batchId` are read off the target `RollBatch`, same as
 * `recoverBatch` above. `signer` is the batch's own roller or the Operator, and receives the rent;
 * `roller` is the roller the batch recorded — the program refuses any other account there with
 * `Unauthorized`. When the roller signs, pass its address as both. Closes a batch in either the 1,100-byte or the 1,360-byte layout.
 */
export async function closeBatch(params: {
  signer: Address;
  roller: Address;
  pool: Address;
  batchId: bigint;
}): Promise<Instruction> {
  const { signer: closer, roller, pool, batchId } = params;

  const { address: batch } = await derivePda([
    BATCH_SEED,
    seedFromPubkey(pool),
    seedFromU64Le(batchId),
  ]);
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);

  return buildAnchorIx({
    name: "close_batch",
    accounts: [writableSigner(closer), writable(roller), writable(batch), readonly(protocolConfig)],
  });
}
