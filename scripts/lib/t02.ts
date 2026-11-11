// Instruction encoders for the T02 authority's seven instructions: t02_claim_fee, t02_withdraw,
// t02_withdraw_core, t02_claim_nft, t02_claim_nft_core, t02_deposit and t02_deposit_core. Account order and arg shapes are derived
// from idl/bye_machine.json and cross-checked against each instruction's #[derive(Accounts)]
// struct in programs/bye_machine/src/instructions/{exit,deposit}/t02_*.rs.
//
// Each acts on a position owned by the Treasury 02 identity PDA, signed by the wallet in
// `ProtocolConfig.t02_authority`. Fees always land in Treasury 03; the card of a withdrawal goes
// to the authority's own token account. `t02_deposit*` is `deposit` for a card in the authority's
// wallet: one signer, no args, a `Pending` position the Operator then approves or rejects.
//
// Pure with respect to I/O, same discipline as exit.ts and buyback.ts: every state-dependent
// account is a required parameter.

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
import { sharedAccounts } from "./buyback.js";
import {
  ASSOCIATED_TOKEN_PROGRAM,
  MPL_CORE_PROGRAM,
  SYSTEM_PROGRAM,
  TOKEN_METADATA_PROGRAM,
  TOKEN_PROGRAM,
} from "../constants.js";

const PRINCIPAL_VAULT_SEED = "principal";

function optionalWritable(address: Address | null) {
  // A `None` optional is passed as the program's own id, which may not be marked writable.
  return address === null ? readonly(optionalAddress(address)) : writable(address);
}

async function exitAccounts(poolId: number, nftMint: Address) {
  const s = await sharedAccounts(poolId, nftMint, nftMint);
  const { address: principalVault } = await derivePda([
    PRINCIPAL_VAULT_SEED,
    seedFromPubkey(s.pool),
  ]);
  return { ...s, principalVault };
}

// ── t02_claim_fee ────────────────────────────────────────────────────────────────────────────
/** 9 accounts, no args. No custody account: one instruction serves all three standards. */
export async function t02ClaimFee(params: {
  t02Authority: Address;
  poolId: number;
  nftMint: Address;
}): Promise<Instruction> {
  const { t02Authority, poolId, nftMint } = params;
  const s = await exitAccounts(poolId, nftMint);

  return buildAnchorIx({
    name: "t02_claim_fee",
    accounts: [
      signer(t02Authority),
      readonly(s.protocolConfig),
      writable(s.pool),
      readonly(s.t02Identity),
      writable(s.position),
      readonly(s.topTier),
      writable(s.principalVault),
      writable(s.treasury03),
      readonly(TOKEN_PROGRAM),
    ],
  });
}

// ── t02_withdraw ─────────────────────────────────────────────────────────────────────────────
/**
 * 24 accounts, no args. The five pNFT slots are required-present on standard 0 and
 * required-absent on standard 1, exactly as on `withdraw`. `successor` is
 * `resolveSuccessor`'s result, appended to `remaining_accounts` only when non-null.
 */
export async function t02Withdraw(params: {
  t02Authority: Address;
  poolId: number;
  nftMint: Address;
  weightIndex: Address;
  /** The T02 authority's token account for the card. */
  authorityToken: Address;
  metadata: Address;
  masterEdition: Address;
  positionTokenRecord: Address | null;
  authorityTokenRecord: Address | null;
  sysvarInstructions: Address | null;
  authorizationRulesProgram: Address | null;
  authorizationRules: Address | null;
  successor: Address | null;
}): Promise<Instruction> {
  const {
    t02Authority,
    poolId,
    nftMint,
    weightIndex,
    authorityToken,
    metadata,
    masterEdition,
    positionTokenRecord,
    authorityTokenRecord,
    sysvarInstructions,
    authorizationRulesProgram,
    authorizationRules,
    successor,
  } = params;
  const s = await exitAccounts(poolId, nftMint);

  return buildAnchorIx({
    name: "t02_withdraw",
    accounts: [
      writableSigner(t02Authority),
      readonly(s.protocolConfig),
      writable(s.pool),
      readonly(s.t02Identity),
      writable(s.position),
      writable(weightIndex),
      writable(s.topTier),
      writable(s.walletStats),
      writable(s.principalVault),
      writable(s.treasury03),
      writable(s.positionVault),
      readonly(nftMint),
      writable(authorityToken),
      writable(metadata),
      readonly(masterEdition),
      optionalWritable(positionTokenRecord),
      optionalWritable(authorityTokenRecord),
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

// ── t02_withdraw_core ────────────────────────────────────────────────────────────────────────
/** 16 accounts, no args. `positionVault` is readonly and `asset` writable, as on every Core exit. */
export async function t02WithdrawCore(params: {
  t02Authority: Address;
  poolId: number;
  /** The `AssetV1` account itself. There is no mint on this family. */
  asset: Address;
  /** The collection the asset's `update_authority` names. */
  collection: Address;
  weightIndex: Address;
  successor: Address | null;
}): Promise<Instruction> {
  const { t02Authority, poolId, asset, collection, weightIndex, successor } = params;
  const s = await exitAccounts(poolId, asset);

  return buildAnchorIx({
    name: "t02_withdraw_core",
    accounts: [
      writableSigner(t02Authority),
      readonly(s.protocolConfig),
      writable(s.pool),
      readonly(s.t02Identity),
      writable(s.position),
      writable(weightIndex),
      writable(s.topTier),
      writable(s.walletStats),
      writable(s.principalVault),
      writable(s.treasury03),
      readonly(s.positionVault),
      writable(asset),
      readonly(collection),
      readonly(MPL_CORE_PROGRAM),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
    remaining: successor !== null ? [writable(successor)] : [],
  });
}

// ── t02_claim_nft ────────────────────────────────────────────────────────────────────────────
/**
 * 21 accounts, no args. Pays a Treasury 02 position closed below the floor its stored `accrued`
 * into Treasury 03, then releases the card to the T02 authority. The five pNFT slots are
 * required-present on standard 0 and required-absent on standard 1. `pool` is writable for
 * `owed_fees`, and the Treasury 02 identity is derived from it.
 */
export async function t02ClaimNft(params: {
  t02Authority: Address;
  poolId: number;
  nftMint: Address;
  authorityToken: Address;
  metadata: Address;
  masterEdition: Address;
  positionTokenRecord: Address | null;
  authorityTokenRecord: Address | null;
  sysvarInstructions: Address | null;
  authorizationRulesProgram: Address | null;
  authorizationRules: Address | null;
}): Promise<Instruction> {
  const {
    t02Authority,
    poolId,
    nftMint,
    authorityToken,
    metadata,
    masterEdition,
    positionTokenRecord,
    authorityTokenRecord,
    sysvarInstructions,
    authorizationRulesProgram,
    authorizationRules,
  } = params;
  const s = await exitAccounts(poolId, nftMint);

  return buildAnchorIx({
    name: "t02_claim_nft",
    accounts: [
      writableSigner(t02Authority),
      readonly(s.protocolConfig),
      writable(s.pool),
      readonly(s.t02Identity),
      writable(s.position),
      writable(s.positionVault),
      writable(s.principalVault),
      writable(s.treasury03),
      readonly(nftMint),
      writable(authorityToken),
      writable(metadata),
      readonly(masterEdition),
      optionalWritable(positionTokenRecord),
      optionalWritable(authorityTokenRecord),
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

// ── t02_claim_nft_core ───────────────────────────────────────────────────────────────────────
/**
 * 13 accounts, no args. Pays the stored `accrued` into Treasury 03 on `ClosedBelowFloor` and
 * `Seized` alike, then releases the card. `positionVault` is readonly and `asset` writable, as on
 * every Core exit.
 */
export async function t02ClaimNftCore(params: {
  t02Authority: Address;
  poolId: number;
  /** The `AssetV1` account itself. There is no mint on this family. */
  asset: Address;
  /** The collection the asset's `update_authority` names. */
  collection: Address;
}): Promise<Instruction> {
  const { t02Authority, poolId, asset, collection } = params;
  const s = await exitAccounts(poolId, asset);

  return buildAnchorIx({
    name: "t02_claim_nft_core",
    accounts: [
      writableSigner(t02Authority),
      readonly(s.protocolConfig),
      writable(s.pool),
      readonly(s.t02Identity),
      writable(s.position),
      readonly(s.positionVault),
      writable(s.principalVault),
      writable(s.treasury03),
      writable(asset),
      readonly(collection),
      readonly(MPL_CORE_PROGRAM),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── t02_deposit ──────────────────────────────────────────────────────────────────────────────
/**
 * 21 accounts, no args: `deposit`'s account list with the T02 authority in the depositor's place
 * and `protocol_config` and `t02_identity` added. The position it creates is `Pending` with no
 * value; the Operator's `approve_deposit` attests and activates it. The five pNFT slots are
 * required-present on standard 0 and required-absent on standard 1.
 */
export async function t02Deposit(params: {
  t02Authority: Address;
  poolId: number;
  /** The collection the presented asset's metadata declares. */
  collection: Address;
  nftMint: Address;
  authorityToken: Address;
  metadata: Address;
  masterEdition: Address;
  authorityTokenRecord: Address | null;
  positionTokenRecord: Address | null;
  sysvarInstructions: Address | null;
  authorizationRulesProgram: Address | null;
  authorizationRules: Address | null;
}): Promise<Instruction> {
  const {
    t02Authority,
    poolId,
    collection,
    nftMint,
    authorityToken,
    metadata,
    masterEdition,
    authorityTokenRecord,
    positionTokenRecord,
    sysvarInstructions,
    authorizationRulesProgram,
    authorizationRules,
  } = params;
  const s = await sharedAccounts(poolId, collection, nftMint);

  return buildAnchorIx({
    name: "t02_deposit",
    accounts: [
      writableSigner(t02Authority),
      readonly(s.protocolConfig),
      writable(s.pool),
      readonly(s.t02Identity),
      readonly(s.poolCollection),
      writable(s.position),
      writable(s.positionVault),
      writable(s.walletStats),
      readonly(nftMint),
      writable(authorityToken),
      writable(metadata),
      readonly(masterEdition),
      optionalWritable(authorityTokenRecord),
      optionalWritable(positionTokenRecord),
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

// ── t02_deposit_core ─────────────────────────────────────────────────────────────────────────
/** 12 accounts, no args. `positionVault` is readonly: MPL Core never allocates it. */
export async function t02DepositCore(params: {
  t02Authority: Address;
  poolId: number;
  collection: Address;
  /** The `AssetV1` account itself. There is no mint on this family. */
  asset: Address;
}): Promise<Instruction> {
  const { t02Authority, poolId, collection, asset } = params;
  const s = await sharedAccounts(poolId, collection, asset);

  return buildAnchorIx({
    name: "t02_deposit_core",
    accounts: [
      writableSigner(t02Authority),
      readonly(s.protocolConfig),
      writable(s.pool),
      readonly(s.t02Identity),
      readonly(s.poolCollection),
      writable(s.position),
      readonly(s.positionVault),
      writable(s.walletStats),
      writable(asset),
      readonly(collection),
      readonly(MPL_CORE_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}
