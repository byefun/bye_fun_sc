// Instruction encoders for the five exit-domain instructions: withdraw, claim_nft and, from
// T-108, the two MPL-Core twins withdraw_core / claim_nft_core plus close_seized. Account order
// and arg shapes are derived mechanically from target/idl/bye_machine.json, cross-checked against
// each instruction's #[derive(Accounts)] struct in programs/bye_machine/src/instructions/exit/.
// All five take no args at all, so their entire wire correctness is the account list and its
// order (22 withdraw, 16 claim_nft, 14 withdraw_core, 9 close_seized, 7 claim_nft_core).
//
// `close_seized` is in this file because it is in that domain, and it is in that domain by owner
// ruling rather than because it exits anything: it moves no custody and makes no CPI. What puts
// it here is the terminal it writes and the `CollateralAbsent` message on the two Core claims
// that names it as the call a depositor should make instead.
//
// Pure and synchronous with respect to I/O, same discipline as admin.ts/deposit.ts: every
// state-dependent account (an existing pool's id, `withdraw`'s `weightIndex`, the tier
// successor) is a required parameter — these encoders derive this program's own PDAs but never
// read on-chain state themselves.
//
// `withdraw.rs:153`'s `fill_vacancy_from_candidate` reads its candidate off
// `ctx.remaining_accounts` POSITIONALLY (`.first()`), not by key, and returns `Ok(None)` —
// silently, with no error — when the array is empty. `successor` below is `resolveSuccessor`'s
// (`tier-resolve.ts`) result: required and non-defaulted, appended only when non-null. AC-30's
// "highest-ranked non-member promotes" clause has no on-chain backstop — `resolveSuccessor`'s
// scan is the only thing enforcing it, so `successor` must never be a caller-supplied arbitrary
// address.
//
// Both instructions carry the `authorization_rules_program` / `authorization_rules` pair as
// Anchor `Option<Account>`s (`optionalAddress`, same convention as deposit.ts); `withdraw` also
// carries `depositor_usdc` as a third optional slot. A `None` is encoded by passing this
// program's own id, never by omitting the account — omitting shifts every later account by one.
//
// The four exits are depositor-signed (`withdraw.rs:262`, `claim_nft.rs:78`) — unlike
// `return_rejected` and `close_seized`, which are permissionless. `close_seized`'s signer is the
// only one in this file that is not writable, because it closes no account and so receives no
// rent refund. See exit.test.ts's first describe block for F-2, the
// open custody question on `depositor_token`, which this file's two instructions share the
// shape of at lower severity (the depositor signs here, so misdirection is user error, not a
// program-enforced guarantee failing) — the Critical case sits on `return_rejected`, not here.

import type { Address, Instruction } from "@solana/kit";
import {
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
const POOL_SEED = "pool";
const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const PRINCIPAL_VAULT_SEED = "principal";
const TOP_TIER_SEED = "tier";
const WALLET_STATS_SEED = "wallet";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

// ── withdraw ─────────────────────────────────────────────────────────────────────────────────
/**
 * `weightIndex` is `pool.weight_index` at call time — the caller's responsibility to read, same
 * as `initPool`'s and `approveDeposit`'s `weightIndex` param. `depositorUsdc` is optional: the
 * fee payout is structurally zero for the whole of slice 1 (round 2's concern), so withdrawal
 * availability never depends on the depositor holding a USDC account — `null` encodes as this
 * program's own id, same as the auth-rules pair. Unlike the auth-rules pair, this slot's *role*
 * is polarity-dependent, not a blanket `readonly`: `withdraw.rs`'s `depositor_usdc` is `mut` when
 * present (it receives the principal payout), so the `null` case must encode `readonly` — marking
 * this program's own address `writable` while it is also the invoked program is illegal at the
 * transaction-format level ("invoked and marked writable"), independent of anything the handler
 * itself checks (found by T-48's rule-set replay: every `null`-payout withdraw failed before ever
 * reaching the escrow logic).
 *
 * `successor` is `resolveSuccessor(rpc, pool, position)`'s result (`tier-resolve.ts`) —
 * required, never defaulted or optional. `null` means "resolved: no successor is owed" (the
 * closing position wasn't a tier member, or no eligible non-member exists); it is appended to
 * `remaining_accounts` only when non-null, and never with a placeholder address — unlike the
 * `Option<Account>` slots above, `remaining_accounts` has no "None" encoding of its own, an
 * entry is either present or absent.
 */
export async function withdraw(params: {
  depositor: Address;
  poolId: number;
  nftMint: Address;
  weightIndex: Address;
  depositorUsdc: Address | null;
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
  successor: Address | null;
}): Promise<Instruction> {
  const {
    depositor,
    poolId,
    nftMint,
    weightIndex,
    depositorUsdc,
    depositorToken,
    metadata,
    masterEdition,
    positionTokenRecord,
    depositorTokenRecord,
    sysvarInstructions,
    authorizationRulesProgram,
    authorizationRules,
    successor,
  } = params;

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
  const { address: principalVault } = await derivePda([
    PRINCIPAL_VAULT_SEED,
    seedFromPubkey(pool),
  ]);
  const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);

  return buildAnchorIx({
    name: "withdraw",
    accounts: [
      writableSigner(depositor),
      writable(pool),
      writable(position),
      writable(weightIndex),
      writable(topTier),
      writable(walletStats),
      writable(principalVault),
      depositorUsdc === null ? readonly(optionalAddress(depositorUsdc)) : writable(depositorUsdc),
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
    remaining: successor !== null ? [writable(successor)] : [],
  });
}

// ── claim_nft ────────────────────────────────────────────────────────────────────────────────
/**
 * `claim_nft` carries no `pool` account at all — `position.pool` and `position.nft_mint` are
 * read off the already-loaded `position` account on-chain (`claim_nft.rs`'s own handler reads
 * them that way, and its accounts struct declares neither `pool` nor `protocol_config`). `pool`
 * is taken here anyway, same as `rejectDeposit`/`returnRejected` in `deposit.ts`, solely to
 * derive `position`'s own PDA client-side — it never appears in the instruction's account list.
 */
export async function claimNft(params: {
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
    name: "claim_nft",
    accounts: [
      writableSigner(depositor),
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

// ── withdraw_core ────────────────────────────────────────────────────────────────────────────
/**
 * `withdraw`'s MPL-Core twin: no args and **fourteen** accounts against the principal's
 * twenty-two. Every one of the eight missing slots is Token Metadata's account model rather than
 * a dropped check — no `nft_mint`, no `position_vault` *token* account, no destination token
 * account, no `metadata`, no `master_edition`, and none of the five pNFT accounts — with
 * `collection` and `mpl_core_program` added in their place.
 *
 * `depositorUsdc` keeps the principal's polarity-dependent role exactly: `null` encodes as this
 * program's own id and must take the **readonly** role, because marking the invoked program
 * writable is illegal at the transaction-format level regardless of what the handler checks. A
 * present account is writable — it receives the principal payout.
 *
 * `successor` is `resolveSuccessor(rpc, pool, position)`'s result (`tier-resolve.ts`), on the
 * principal's terms and for the same reason: the tier work stays in the handler rather than in the
 * shared `apply_withdraw`, and `withdraw_core.rs` calls `tier::fill_vacancy_from_candidate` with
 * `ctx.remaining_accounts` exactly as `withdraw.rs` does — read positionally, so the candidate is
 * appended only when non-null and never with a placeholder address.
 *
 * `positionVault` is readonly and `asset` writable, the split every Core exit carries: the escrow
 * identity is never an allocated account, and `TransferV1` rewrites `AssetV1.owner`.
 */
export async function withdrawCore(params: {
  depositor: Address;
  poolId: number;
  /** The `AssetV1` account itself. There is no mint on this family. */
  asset: Address;
  /** The collection the asset's `update_authority` names — not a caller preference. */
  collection: Address;
  weightIndex: Address;
  depositorUsdc: Address | null;
  successor: Address | null;
}): Promise<Instruction> {
  const { depositor, poolId, asset, collection, weightIndex, depositorUsdc, successor } = params;

  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(asset),
  ]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: walletStats } = await derivePda([
    WALLET_STATS_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(depositor),
  ]);
  const { address: principalVault } = await derivePda([
    PRINCIPAL_VAULT_SEED,
    seedFromPubkey(pool),
  ]);
  const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);

  return buildAnchorIx({
    name: "withdraw_core",
    accounts: [
      writableSigner(depositor),
      writable(pool),
      writable(position),
      writable(weightIndex),
      writable(topTier),
      writable(walletStats),
      writable(principalVault),
      depositorUsdc === null ? readonly(optionalAddress(depositorUsdc)) : writable(depositorUsdc),
      readonly(positionVault),
      writable(asset),
      readonly(collection),
      readonly(MPL_CORE_PROGRAM),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
    remaining: successor !== null ? [writable(successor)] : [],
  });
}

// ── claim_nft_core ───────────────────────────────────────────────────────────────────────────
/**
 * `claim_nft`'s MPL-Core twin: no args and **seven** accounts against the principal's sixteen —
 * the smallest instruction in the program. It carries no `pool`, no weight, no tier and no
 * counter surface, because every one of those moved when the position closed; `pool` is taken
 * here anyway, exactly as `claimNft` takes it, solely to derive `position`'s own PDA
 * client-side — it never appears in the instruction's account list.
 */
export async function claimNftCore(params: {
  depositor: Address;
  pool: Address;
  /** The `AssetV1` account itself. There is no mint on this family. */
  asset: Address;
  /** The collection the asset's `update_authority` names — not a caller preference. */
  collection: Address;
}): Promise<Instruction> {
  const { depositor, pool, asset, collection } = params;

  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(asset),
  ]);
  const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);

  return buildAnchorIx({
    name: "claim_nft_core",
    accounts: [
      writableSigner(depositor),
      writable(position),
      readonly(positionVault),
      writable(asset),
      readonly(collection),
      readonly(MPL_CORE_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── close_seized ─────────────────────────────────────────────────────────────────────────────
/**
 * **Permissionless, and the only exit-domain instruction whose signer is not writable.** `caller`
 * is a bare `Signer` with no `mut`: this instruction closes no account on any source state
 * (D-115), so there is no rent refund for a caller to receive and nothing to fund — a
 * `writableSigner` here would be a role the program never asked for. It is also the only Core
 * instruction that takes no `mpl_core_program`: it reads the two accounts and makes no CPI at
 * all, which is why both `asset` and `collection` are readonly here where every exit marks the
 * asset writable.
 *
 * **`collection` is required even though nothing is transferred**, and it is not the CPI's
 * account: a `permanent_freeze_delegate` sits on an asset *or* on a collection, and the live one
 * on this pool's Core collection is held by a third party — so the program cannot see the freeze
 * it is being asked to clean up without reading the collection's own plugin registry. The
 * program checks it against the asset's `update_authority` and refuses `6100` on a mismatch, so
 * this must be the collection the asset itself names, exactly as on the three exits.
 *
 * **No `successor`, unlike `withdraw`'s otherwise identical close.** This instruction vacates a
 * tier seat and does not fill it: the candidate a caller supplies cannot be checked on chain to
 * be the next-ranked non-member — `TopTier` holds only the members — and this caller may be
 * anybody, so the choice would be a stranger's over a seat that earns 5% of every ticket. The
 * vacancy is left for `update_tier`, which is Operator-signed. Callers that resolved a successor
 * for `withdraw` should not pass it here; there is no slot for it.
 */
export async function closeSeized(params: {
  caller: Address;
  poolId: number;
  /** The `AssetV1` account itself. There is no mint on this family. */
  asset: Address;
  /**
   * The collection the asset's `update_authority` names — not a caller preference: the program
   * compares it against the asset's own declaration and refuses `6100` on a mismatch.
   */
  collection: Address;
  depositor: Address;
  weightIndex: Address;
}): Promise<Instruction> {
  const { caller, poolId, asset, collection, depositor, weightIndex } = params;

  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(asset),
  ]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: walletStats } = await derivePda([
    WALLET_STATS_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(depositor),
  ]);
  const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);

  return buildAnchorIx({
    name: "close_seized",
    accounts: [
      signer(caller),
      writable(pool),
      writable(position),
      writable(weightIndex),
      writable(topTier),
      writable(walletStats),
      readonly(positionVault),
      readonly(asset),
      readonly(collection),
    ],
  });
}
