// Instruction encoders for the six deposit-lifecycle instructions: deposit, deposit_core,
// approve_deposit, reject_deposit, return_rejected, return_rejected_core. Account order and arg
// shapes are derived mechanically from
// target/idl/bye_machine.json, cross-checked against each instruction's #[derive(Accounts)]
// struct in programs/bye_machine/src/instructions/deposit/.
//
// Pure and synchronous with respect to I/O, same discipline as admin.ts: every state-dependent
// account (an existing pool's id, the depositor's own token/metadata accounts, the member an
// activation displaces) is a required parameter — these encoders derive this program's own PDAs
// but never read on-chain state themselves. `approve_deposit`'s `displaced` is the *result* of
// `tier-resolve.ts`'s `resolveDisplaced`, computed by the caller before calling this encoder.
//
// `deposit` and `return_rejected` both carry the pNFT `authorization_rules_program` /
// `authorization_rules` pair as Anchor `Option<Account>`s: a `None` is encoded by passing this
// program's own id in that slot (`optionalAddress`), never by omitting the account — omitting
// shifts every later account by one.

import type { Address, Instruction } from "@solana/kit";
import {
  Borsh,
  buildAnchorIx,
  derivePda,
  optionalAddress,
  readonly,
  seedFromPubkey,
  signer,
  writable,
  writableSigner,
} from "./anchor.js";
import {
  ASSOCIATED_TOKEN_PROGRAM,
  MPL_CORE_PROGRAM,
  SYSTEM_PROGRAM,
  SYSVAR_INSTRUCTIONS,
  TOKEN_METADATA_PROGRAM,
  TOKEN_PROGRAM,
} from "../constants.js";

// ── Seeds (bye_machine-specific — anchor.ts stays program-agnostic) ─────────────────────────
// Verbatim against programs/bye_machine/src/common/seeds.rs.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const COLLECTION_SEED = "collection";
const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const WALLET_STATS_SEED = "wallet";
const TOP_TIER_SEED = "tier";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

// ── deposit ──────────────────────────────────────────────────────────────────────────────────
/**
 * No args — `deposit`'s entire wire correctness is its 19-account list and order.
 * `depositorToken`/`metadata`/`masterEdition`/`depositorTokenRecord`/`positionTokenRecord` are
 * required params, not derived: they are PDAs (or plain accounts) of the Token Metadata /
 * associated-token programs, not of `PROGRAM_ID`, and this file derives only this program's own
 * PDAs (`pool`, `position`, `position_vault`, `wallet_stats`), matching admin.ts's convention for
 * accounts this program cannot derive itself.
 */
export async function deposit(params: {
  depositor: Address;
  poolId: number;
  /**
   * The collection the presented asset's metadata declares. Required and not defaulted: the
   * program re-derives the `PoolCollection` record from this pool's key and the record's own
   * `collection`, and the asset's metadata must name that same collection — so a caller that
   * guessed wrong fails on chain rather than silently depositing against another record.
   */
  collection: Address;
  nftMint: Address;
  depositorToken: Address;
  metadata: Address;
  masterEdition: Address;
  /**
   * The five pNFT accounts, **required-present on standard 0 and required-absent on standard 1**
   * (design §4.2). Nullable rather than optional parameters: the program resolves the standard
   * from the asset and then requires exactly this shape, so a caller that omitted the field
   * instead of passing `null` would encode a legacy deposit it never chose. A supplied account on
   * a legacy deposit fails on chain with Anchor's `ConstraintRaw` (2003), a missing one on a pNFT
   * deposit with `AccountNotEnoughKeys` (3005).
   */
  depositorTokenRecord: Address | null;
  positionTokenRecord: Address | null;
  sysvarInstructions: Address | null;
  authorizationRulesProgram: Address | null;
  authorizationRules: Address | null;
}): Promise<Instruction> {
  const {
    depositor,
    poolId,
    collection,
    nftMint,
    depositorToken,
    metadata,
    masterEdition,
    depositorTokenRecord,
    positionTokenRecord,
    sysvarInstructions,
    authorizationRulesProgram,
    authorizationRules,
  } = params;

  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: poolCollection } = await derivePda([
    COLLECTION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(collection),
  ]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(nftMint),
  ]);
  const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
  const { address: walletStats } = await derivePda([
    WALLET_STATS_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(depositor),
  ]);

  return buildAnchorIx({
    name: "deposit",
    accounts: [
      writableSigner(depositor),
      writable(pool),
      readonly(poolCollection),
      writable(position),
      writable(positionVault),
      writable(walletStats),
      readonly(nftMint),
      writable(depositorToken),
      writable(metadata),
      readonly(masterEdition),
      // A `None` optional is passed as the program's own id, and a program address may not be
      // marked writable — so the placeholder takes the readonly role while the present account
      // keeps the writable one `TransferV1` needs. `exit.ts`'s `depositorUsdc` is the same shape.
      depositorTokenRecord === null
        ? readonly(optionalAddress(depositorTokenRecord))
        : writable(depositorTokenRecord),
      positionTokenRecord === null
        ? readonly(optionalAddress(positionTokenRecord))
        : writable(positionTokenRecord),
      readonly(TOKEN_METADATA_PROGRAM),
      readonly(optionalAddress(authorizationRulesProgram)),
      readonly(optionalAddress(authorizationRules)),
      readonly(optionalAddress(sysvarInstructions)),
      readonly(TOKEN_PROGRAM),
      readonly(ASSOCIATED_TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── deposit_core ─────────────────────────────────────────────────────────────────────────────
/**
 * `deposit`'s MPL-Core twin: no args and **ten** accounts against the principal's nineteen.
 *
 * Twelve Token Metadata slots are gone and three Core ones take their place. None of the twelve
 * has a Core analogue — an `AssetV1` account *is* the instrument, so no mint derives it, no token
 * account holds it, there is no metadata PDA, no master edition, no token record and no auth-rules
 * pair — and `position_vault` names an address that is never allocated, because MPL Core writes
 * the new owner straight into `AssetV1.owner`.
 *
 * `collection` is required and derived from nothing. It is the collection the **asset** declares
 * through `UpdateAuthority::Collection`, which the program re-reads on chain and pins this slot
 * against; a caller who guesses returns `CollectionNotAdmitted` (6100) rather than depositing
 * against another record. `positionVault` is readonly here, not writable: nothing in this
 * program or in MPL Core writes to it, and marking a never-allocated address writable is how a
 * caller ends up paying to create the account the design exists without.
 */
export async function depositCore(params: {
  depositor: Address;
  poolId: number;
  /** The collection the asset's `update_authority` names — not a caller preference. */
  collection: Address;
  /** The `AssetV1` account itself. There is no mint on this family. */
  asset: Address;
}): Promise<Instruction> {
  const { depositor, poolId, collection, asset } = params;

  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: poolCollection } = await derivePda([
    COLLECTION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(collection),
  ]);
  // `position`'s second seed is the asset, where the Token Metadata twin uses the mint — the
  // same slot in the same derivation, because `Position.nft_mint` stores whichever one names the
  // escrowed instrument.
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(asset),
  ]);
  const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
  const { address: walletStats } = await derivePda([
    WALLET_STATS_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(depositor),
  ]);

  return buildAnchorIx({
    name: "deposit_core",
    accounts: [
      writableSigner(depositor),
      writable(pool),
      readonly(poolCollection),
      writable(position),
      readonly(positionVault),
      writable(walletStats),
      writable(asset),
      readonly(collection),
      readonly(MPL_CORE_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── approve_deposit ──────────────────────────────────────────────────────────────────────────
/**
 * `weightIndex` is `pool.weight_index` at call time — the caller's responsibility to read, same
 * as `initPool`'s `weightIndex` param. `depositor` is `position.depositor`, needed to derive
 * `wallet_stats`'s PDA — likewise caller-supplied, not read here.
 *
 * `lockUntil` is the Season-0 lock (D-091) — `season.tge_at + 14 days` for a Season-0 deposit, `0`
 * otherwise. **Required and non-defaulted, deliberately.** It sits directly after `observedAt` and
 * both are `i64`, so a default here would let every caller that forgot the lock encode a valid,
 * silently unlocked position — and the program-side pin cannot see a client omission. `0` must be
 * written by the caller who means it.
 *
 * `displaced` carries `PromoteOutcome::Swapped`'s evicted member as a `remaining_accounts` entry
 * matched **by key** (`tier::settle_displaced_member` → `.find()` → `AccountNotEnoughKeys` /
 * 3005 if absent). Required and non-defaulted — resolve it with `tier-resolve.ts`'s
 * `resolveDisplaced(rpc, pool, { kind: "activating", value })` before calling this encoder; a
 * defaulted or optional param here would let a client compile while silently failing every
 * activation that displaces a tier member once the tier fills.
 */
export async function approveDeposit(params: {
  operator: Address;
  poolId: number;
  nftMint: Address;
  depositor: Address;
  weightIndex: Address;
  displaced: Address | null;
  value: bigint;
  observedAt: bigint;
  lockUntil: bigint;
}): Promise<Instruction> {
  const { operator, poolId, nftMint, depositor, weightIndex, displaced, value, observedAt, lockUntil } =
    params;

  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(nftMint),
  ]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: walletStats } = await derivePda([
    WALLET_STATS_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(depositor),
  ]);

  return buildAnchorIx({
    name: "approve_deposit",
    // Order is (value, observedAt, lockUntil) — the last two are both i64 and transposing them
    // encodes to the same 24 bytes. See approve_deposit.rs's own pin.
    args: new Borsh().u64(value).i64(observedAt).i64(lockUntil).build(),
    accounts: [
      signer(operator),
      readonly(protocolConfig),
      writable(pool),
      writable(position),
      writable(weightIndex),
      writable(topTier),
      writable(walletStats),
    ],
    remaining: displaced === null ? [] : [writable(displaced)],
  });
}

// ── reject_deposit ───────────────────────────────────────────────────────────────────────────
/**
 * No args beyond `reason` — `position` is a PDA of `[POSITION_SEED, pool, nft_mint]`, seeded
 * directly from `pool`/`nftMint` rather than from a `poolId`: the IDL's own seed path
 * (`position.pool`) is the `Position` account's stored field, which the client supplies as the
 * PDA's own seed material, not something derived from a pool-counter argument.
 */
export async function rejectDeposit(params: {
  operator: Address;
  pool: Address;
  nftMint: Address;
  reason: number;
}): Promise<Instruction> {
  const { operator, pool, nftMint, reason } = params;

  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(nftMint),
  ]);

  return buildAnchorIx({
    name: "reject_deposit",
    args: new Borsh().u16(reason).build(),
    accounts: [signer(operator), readonly(protocolConfig), writable(position)],
  });
}

// ── return_rejected ──────────────────────────────────────────────────────────────────────────
/**
 * No args — `return_rejected`'s entire wire correctness is its 17-account list and order.
 * Permissionless: `payer` funds the transaction (`writableSigner`), `depositor` is an
 * `UncheckedAccount` pinned on-chain to `position.depositor` (`writable`, not a signer) — the
 * NFT and both rent refunds land there regardless of who submits the transaction. This differs
 * from `deposit`'s signer shape, where the depositor is the sole signer.
 *
 * `depositorToken` is a bare `UncheckedAccount` here (no mint/owner constraint on-chain), unlike
 * `deposit`'s constrained `depositor_token` — a known open custody question (F-2). This encoder
 * only encodes the account; it does not validate it.
 */
export async function returnRejected(params: {
  payer: Address;
  depositor: Address;
  pool: Address;
  nftMint: Address;
  depositorToken: Address;
  metadata: Address;
  masterEdition: Address;
  /**
   * The five pNFT accounts, **required-present on standard 0 and required-absent on standard 1**
   * (design §4.2 / flows §1.2). Nullable rather than optional parameters: the exit reads the
   * standard off the position and then requires exactly this shape, so a caller that omitted the
   * field instead of passing `null` would encode a legacy release it never chose. A supplied
   * account on a legacy release fails with Anchor's `ConstraintRaw` (2003), a missing one on a
   * pNFT release with `AccountNotEnoughKeys` (3005).
   */
  positionTokenRecord: Address | null;
  depositorTokenRecord: Address | null;
  sysvarInstructions: Address | null;
  authorizationRulesProgram: Address | null;
  authorizationRules: Address | null;
}): Promise<Instruction> {
  const {
    payer,
    depositor,
    pool,
    nftMint,
    depositorToken,
    metadata,
    masterEdition,
    positionTokenRecord,
    depositorTokenRecord,
    sysvarInstructions,
    authorizationRulesProgram,
    authorizationRules,
  } = params;

  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(nftMint),
  ]);
  const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);

  return buildAnchorIx({
    name: "return_rejected",
    accounts: [
      writableSigner(payer),
      writable(depositor),
      writable(position),
      writable(positionVault),
      readonly(nftMint),
      writable(depositorToken),
      writable(metadata),
      readonly(masterEdition),
      // A `None` optional is passed as the program's own id, and a program address may not be
      // marked writable — so the placeholder takes the readonly role while a present account keeps
      // the writable one `TransferV1` needs.
      positionTokenRecord === null ? readonly(optionalAddress(positionTokenRecord)) : writable(positionTokenRecord),
      depositorTokenRecord === null ? readonly(optionalAddress(depositorTokenRecord)) : writable(depositorTokenRecord),
      readonly(TOKEN_METADATA_PROGRAM),
      readonly(optionalAddress(authorizationRulesProgram)),
      readonly(optionalAddress(authorizationRules)),
      readonly(optionalAddress(sysvarInstructions)),
      readonly(TOKEN_PROGRAM),
      readonly(ASSOCIATED_TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── return_rejected_core ─────────────────────────────────────────────────────────────────────
/**
 * `return_rejected`'s MPL-Core twin: no args and **eight** accounts against the principal's
 * seventeen. Permissionless on exactly the principal's terms — `payer` funds the transaction and
 * `depositor` is an `UncheckedAccount` pinned on-chain to `position.depositor`, so both the card
 * and the position's rent land there whoever submits.
 *
 * Nine Token Metadata slots are gone and one Core slot takes their place, the same trade
 * `depositCore` makes in the other direction: an `AssetV1` account *is* the instrument, so there
 * is no mint, no destination token account, no metadata PDA, no master edition, no token-record
 * pair and no auth-rules pair — and `collection` appears because MPL Core needs the collection
 * account present to run its plugins over the transfer.
 *
 * `positionVault` is readonly, not writable: the escrow identity is never allocated as an
 * account — its only on-chain expression is `AssetV1.owner` — so nothing writes to it, and
 * marking a never-allocated address writable is how a caller ends up paying to create the
 * account the design exists without. `asset` is writable because `TransferV1` rewrites that
 * owner field.
 */
export async function returnRejectedCore(params: {
  payer: Address;
  depositor: Address;
  pool: Address;
  /** The `AssetV1` account itself. There is no mint on this family. */
  asset: Address;
  /** The collection the asset's `update_authority` names — not a caller preference. */
  collection: Address;
}): Promise<Instruction> {
  const { payer, depositor, pool, asset, collection } = params;

  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(asset),
  ]);
  const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);

  return buildAnchorIx({
    name: "return_rejected_core",
    accounts: [
      writableSigner(payer),
      writable(depositor),
      writable(position),
      readonly(positionVault),
      writable(asset),
      readonly(collection),
      readonly(MPL_CORE_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}
