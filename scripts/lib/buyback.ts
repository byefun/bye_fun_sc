// Instruction encoders for the buyback domain: instant_sell, instant_sell_core, and
// activate_acquired. Account order and arg shapes are derived mechanically from
// idl/bye_machine.json, cross-checked against each instruction's #[derive(Accounts)]
// struct in programs/bye_machine/src/instructions/buyback/.
//
// Pure and synchronous with respect to I/O, same discipline as deposit.ts: every state-dependent
// account (the pool's id, the seller's own token/USDC accounts, the member an activation
// displaces) is a required parameter — these encoders derive this program's own PDAs but never
// read on-chain state themselves.
//
// **The two sells are the only instructions in the program with two signers.** Each authorises a
// different leg and neither stands for the other: the seller releases the card, and the Operator
// attests the value that decides admission and payout. Treasury 03's USDC needs no signature at
// all — the float is the shared `PDA(["t03"])` and the program signs that leg with the
// `ProtocolConfig` PDA's seeds.
//
// `displaced` is the *result* of tier-resolve.ts's `resolveDisplaced`, computed by the caller
// before calling these encoders — and only reachable on the immediate-activation branch,
// since a sell landing while a batch is open creates a `Pending` position that ranks nothing.

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
import { deriveSharedTreasuries } from "./admin.js";
import { deriveEscrow } from "./escrow.js";
import {
  ASSOCIATED_TOKEN_PROGRAM,
  MPL_CORE_PROGRAM,
  SYSTEM_PROGRAM,
  SYSVAR_INSTRUCTIONS,
  TOKEN_METADATA_PROGRAM,
  TOKEN_PROGRAM,
} from "../constants.js";

// ── Seeds ────────────────────────────────────────────────────────────────────────────────────
// Verbatim against programs/bye_machine/src/common/seeds.rs.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const COLLECTION_SEED = "collection";
const POSITION_SEED = "position";
const POSITION_VAULT_SEED = "vault";
const WALLET_STATS_SEED = "wallet";
const TOP_TIER_SEED = "tier";
const TREASURY_02_SEED = "t02";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

/**
 * The PDAs both twins share, derived once. `walletStats` is keyed on the **T02 identity**, never
 * on the seller: it is the rollup a Treasury 03 sale credits, and the seller's own rollup is
 * neither read nor written. An escrow sale credits the loader's, passed in `LoaderAccounts`.
 */
export async function sharedAccounts(poolId: number, collection: Address, assetKey: Address) {
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: poolCollection } = await derivePda([
    COLLECTION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(collection),
  ]);
  const { address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(assetKey),
  ]);
  const { address: positionVault } = await derivePda([
    POSITION_VAULT_SEED,
    seedFromPubkey(position),
  ]);
  const { address: walletStats } = await derivePda([
    WALLET_STATS_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(t02Identity),
  ]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { treasury03 } = await deriveSharedTreasuries();
  return { protocolConfig, pool, poolCollection, t02Identity, position, positionVault, walletStats, topTier, treasury03 };
}

/**
 * The loader's three accounts, appended after every named account of both sells. The Operator
 * passes them when the off-chain record of the card's latest draw names a loader other than the
 * Treasury 02 identity, at the addresses derived from that loader whether or not its escrow
 * qualifies — the program decides at execution, and falls back to Treasury 03 when it does not.
 */
export interface LoaderAccounts {
  escrow: Address;
  escrowVault: Address;
  walletStats: Address;
}

/** The three loader accounts for `loader` in pool `poolId`, as the program binds them. */
export async function deriveLoaderAccounts(poolId: number, loader: Address): Promise<LoaderAccounts> {
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { escrow, escrowVault } = await deriveEscrow(loader);
  const { address: walletStats } = await derivePda([
    WALLET_STATS_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(loader),
  ]);
  return { escrow, escrowVault, walletStats };
}

/**
 * The three loader slots. `null` passes all three as the program's own id, which Anchor reads as
 * `None` — the sale then has no escrow path. A `None` placeholder is readonly, because a program
 * address may not be marked writable.
 */
function loaderSlots(loader: LoaderAccounts | null) {
  if (loader === null) {
    return [readonly(optionalAddress(null)), readonly(optionalAddress(null)), readonly(optionalAddress(null))];
  }
  return [readonly(loader.escrow), writable(loader.escrowVault), writable(loader.walletStats)];
}

/**
 * `(value, observedAt)` and **no `lockUntil`**: a treasury-acquired position earns no
 * Season-0 score, so it carries no lock and there is no argument by which a caller could ask for
 * one. That is the one arg-shape difference from `approve_deposit`, and it is deliberate rather
 * than an omission.
 */
function instantSellArgs(value: bigint, observedAt: bigint): Buffer {
  return new Borsh().u64(value).i64(observedAt).build();
}

// ── instant_sell ─────────────────────────────────────────────────────────────────────────────
/**
 * 29 accounts. `sellerTokenRecord`/`positionTokenRecord`/`sysvarInstructions`/
 * `authorizationRulesProgram`/`authorizationRules` are the five pNFT slots, **required-present on
 * standard 0 and required-absent on standard 1**, exactly as on `deposit`. Nullable rather than
 * optional parameters: the program resolves the standard from the asset and then requires exactly
 * this shape, so a caller that omitted the field instead of passing `null` would encode a legacy
 * sell it never chose.
 */
export async function instantSell(params: {
  seller: Address;
  operator: Address;
  poolId: number;
  /** The collection the presented asset's metadata declares — not a caller preference. */
  collection: Address;
  nftMint: Address;
  sellerToken: Address;
  /** Where the payout lands. Pinned on chain to the seller and to Treasury 03's own mint. */
  sellerUsdc: Address;
  weightIndex: Address;
  metadata: Address;
  masterEdition: Address;
  sellerTokenRecord: Address | null;
  positionTokenRecord: Address | null;
  sysvarInstructions: Address | null;
  authorizationRulesProgram: Address | null;
  authorizationRules: Address | null;
  /** Only reachable when `open_batches == 0`; a `Pending` position ranks nothing. */
  displaced: Address | null;
  /** The card's loader, when it is not the Treasury 02 identity; see `deriveLoaderAccounts`. */
  loader: LoaderAccounts | null;
  value: bigint;
  observedAt: bigint;
}): Promise<Instruction> {
  const {
    seller,
    operator,
    poolId,
    collection,
    nftMint,
    sellerToken,
    sellerUsdc,
    weightIndex,
    metadata,
    masterEdition,
    sellerTokenRecord,
    positionTokenRecord,
    sysvarInstructions,
    authorizationRulesProgram,
    authorizationRules,
    displaced,
    loader,
    value,
    observedAt,
  } = params;

  const s = await sharedAccounts(poolId, collection, nftMint);

  return buildAnchorIx({
    name: "instant_sell",
    args: instantSellArgs(value, observedAt),
    accounts: [
      // The seller is not a payer — the Operator carries every rent-bearing account here.
      signer(seller),
      writableSigner(operator),
      readonly(s.protocolConfig),
      writable(s.pool),
      readonly(s.poolCollection),
      readonly(s.t02Identity),
      writable(s.position),
      writable(s.positionVault),
      writable(s.walletStats),
      writable(weightIndex),
      writable(s.topTier),
      writable(s.treasury03),
      writable(sellerUsdc),
      readonly(nftMint),
      writable(sellerToken),
      writable(metadata),
      readonly(masterEdition),
      // A `None` optional is passed as the program's own id, and a program address may not be
      // marked writable — so the placeholder takes the readonly role while the present account
      // keeps the writable one `TransferV1` needs.
      sellerTokenRecord === null
        ? readonly(optionalAddress(sellerTokenRecord))
        : writable(sellerTokenRecord),
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
      ...loaderSlots(loader),
    ],
    remaining: displaced === null ? [] : [writable(displaced)],
  });
}

// ── instant_sell_core ────────────────────────────────────────────────────────────────────────
/**
 * `instant_sell`'s MPL-Core twin: **21** accounts against the principal's 29, same two args.
 *
 * The eleven Token Metadata slots are gone and three Core ones take their place — an `AssetV1`
 * account *is* the instrument, so no mint derives it, no token account holds it, and there is no
 * metadata PDA, master edition, token record or auth-rules pair.
 *
 * `positionVault` is **readonly** here, not writable: MPL Core writes the new owner straight into
 * `AssetV1.owner` and never allocates the address, and marking a never-allocated address writable
 * is how a caller ends up paying to create the account the design exists without.
 */
export async function instantSellCore(params: {
  seller: Address;
  operator: Address;
  poolId: number;
  /** The collection the asset's `update_authority` names — not a caller preference. */
  collection: Address;
  /** The `AssetV1` account itself. There is no mint on this family. */
  asset: Address;
  sellerUsdc: Address;
  weightIndex: Address;
  displaced: Address | null;
  /** The card's loader, when it is not the Treasury 02 identity; see `deriveLoaderAccounts`. */
  loader: LoaderAccounts | null;
  value: bigint;
  observedAt: bigint;
}): Promise<Instruction> {
  const {
    seller,
    operator,
    poolId,
    collection,
    asset,
    sellerUsdc,
    weightIndex,
    displaced,
    loader,
    value,
    observedAt,
  } = params;

  const s = await sharedAccounts(poolId, collection, asset);

  return buildAnchorIx({
    name: "instant_sell_core",
    args: instantSellArgs(value, observedAt),
    accounts: [
      signer(seller),
      writableSigner(operator),
      readonly(s.protocolConfig),
      writable(s.pool),
      readonly(s.poolCollection),
      readonly(s.t02Identity),
      writable(s.position),
      readonly(s.positionVault),
      writable(s.walletStats),
      writable(weightIndex),
      writable(s.topTier),
      writable(s.treasury03),
      writable(sellerUsdc),
      writable(asset),
      readonly(collection),
      readonly(MPL_CORE_PROGRAM),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
      ...loaderSlots(loader),
    ],
    remaining: displaced === null ? [] : [writable(displaced)],
  });
}

// ── activate_acquired ────────────────────────────────────────────────────────────────────────
/**
 * 7 accounts, **no args**, one signer.
 *
 * The missing argument list is the instruction. `value` and `observedAt` were attested when the
 * card was bought and live on the position; this instruction reads them back rather than taking
 * them, so a client cannot re-price a card Treasury 03 has already paid `rate × V` for. There is
 * no `lockUntil` either — a treasury-acquired position carries no Season-0 lock and activates at
 * zero, which the program writes as a literal.
 *
 * Only reachable for a position a sale left `Pending`, which is the branch it takes when a batch
 * was open at acceptance. A sell that landed with no batch open activated on the spot and
 * has nothing for this instruction to do.
 *
 * `nftMint` is the position's recorded mint — the `AssetV1` address on the Core family, since the
 * position PDA is seeded on whichever one the sell presented. This instruction moves no asset and
 * never reads the stored standard, so there is no Core twin and no second encoder.
 *
 * `walletStats` is keyed on the position's depositor, which the program seeds it on: the T02
 * identity for a Treasury 03 sale's position, the loader for an escrow sale's.
 */
export async function activateAcquired(params: {
  operator: Address;
  poolId: number;
  /** The mint (or `AssetV1` address) the position was opened over. */
  nftMint: Address;
  weightIndex: Address;
  /** The tier member this activation displaces, when it enters a full tier. */
  displaced: Address | null;
  /** The loader that owns an escrow sale's position; `null` for a Treasury 03 sale's. */
  depositor: Address | null;
}): Promise<Instruction> {
  const { operator, poolId, nftMint, weightIndex, displaced, depositor } = params;

  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: t02Identity } = await derivePda([TREASURY_02_SEED, seedFromPubkey(pool)]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(nftMint),
  ]);
  const { address: walletStats } = await derivePda([
    WALLET_STATS_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(depositor ?? t02Identity),
  ]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);

  return buildAnchorIx({
    name: "activate_acquired",
    args: Buffer.alloc(0),
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
