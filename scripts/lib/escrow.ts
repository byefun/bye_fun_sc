// Instruction encoders for the five escrow instructions: open_escrow, escrow_deposit,
// escrow_withdraw, set_escrow_armed and claim_fee_to_escrow. Account order and arg shapes follow
// each instruction's #[derive(Accounts)] struct in programs/bye_machine/src/instructions/escrow/,
// and are cross-checked against the IDL oracle.
//
// Every escrow account is derived from the owner's key — `escrow` from `[escrow, owner]`, its
// vault from `[escrow_vault, escrow]` — so the only caller-supplied account is the owner's own
// USDC account. `claim_fee_to_escrow` is `claim_fee` with `depositor_usdc` replaced by the
// depositor's escrow and vault, in the same order.
//
// `open_escrow`, `escrow_deposit` and `set_escrow_armed` fit in one transaction, which is how the
// web creates, funds and arms an escrow under one signature.

import type { Address, Instruction } from "@solana/kit";
import {
  Borsh,
  buildAnchorIx,
  derivePda,
  readonly,
  seedFromPubkey,
  signer,
  writable,
  writableSigner,
} from "./anchor.js";
import { SYSTEM_PROGRAM, TOKEN_PROGRAM } from "../constants.js";

// ── Seeds (bye_machine-specific — anchor.ts stays program-agnostic) ─────────────────────────
// Verbatim against programs/bye_machine/src/common/seeds.rs.
const PROTOCOL_CONFIG_SEED = "protocol";
const POOL_SEED = "pool";
const POSITION_SEED = "position";
const PRINCIPAL_VAULT_SEED = "principal";
const TOP_TIER_SEED = "tier";
export const ESCROW_SEED = "escrow";
export const ESCROW_VAULT_SEED = "escrow_vault";

function seedFromU16Le(n: number): Uint8Array {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return new Uint8Array(b);
}

/** The escrow PDA and its vault for one owner. */
export async function deriveEscrow(owner: Address): Promise<{ escrow: Address; escrowVault: Address }> {
  const { address: escrow } = await derivePda([ESCROW_SEED, seedFromPubkey(owner)]);
  const { address: escrowVault } = await derivePda([ESCROW_VAULT_SEED, seedFromPubkey(escrow)]);
  return { escrow, escrowVault };
}

// ── open_escrow ──────────────────────────────────────────────────────────────────────────────
/** `usdcMint` must be `protocol_config.usdc_mint`; the program refuses any other. */
export async function openEscrow(params: { owner: Address; usdcMint: Address }): Promise<Instruction> {
  const { owner, usdcMint } = params;
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const { escrow, escrowVault } = await deriveEscrow(owner);

  return buildAnchorIx({
    name: "open_escrow",
    accounts: [
      writableSigner(owner),
      readonly(protocolConfig),
      writable(escrow),
      writable(escrowVault),
      readonly(usdcMint),
      readonly(TOKEN_PROGRAM),
      readonly(SYSTEM_PROGRAM),
    ],
  });
}

// ── escrow_deposit ───────────────────────────────────────────────────────────────────────────
/** The program accepts any amount; the product's smallest deposit is enforced before signing. */
export async function escrowDeposit(params: {
  owner: Address;
  ownerUsdc: Address;
  amount: bigint;
}): Promise<Instruction> {
  const { owner, ownerUsdc, amount } = params;
  const { escrow, escrowVault } = await deriveEscrow(owner);

  return buildAnchorIx({
    name: "escrow_deposit",
    args: new Borsh().u64(amount).build(),
    accounts: [
      signer(owner),
      readonly(escrow),
      writable(escrowVault),
      writable(ownerUsdc),
      readonly(TOKEN_PROGRAM),
    ],
  });
}

// ── escrow_withdraw ──────────────────────────────────────────────────────────────────────────
export async function escrowWithdraw(params: {
  owner: Address;
  ownerUsdc: Address;
  amount: bigint;
}): Promise<Instruction> {
  const { owner, ownerUsdc, amount } = params;
  const { escrow, escrowVault } = await deriveEscrow(owner);

  return buildAnchorIx({
    name: "escrow_withdraw",
    args: new Borsh().u64(amount).build(),
    accounts: [
      signer(owner),
      readonly(escrow),
      writable(escrowVault),
      writable(ownerUsdc),
      readonly(TOKEN_PROGRAM),
    ],
  });
}

// ── set_escrow_armed ─────────────────────────────────────────────────────────────────────────
export async function setEscrowArmed(params: { owner: Address; armed: boolean }): Promise<Instruction> {
  const { owner, armed } = params;
  const { escrow } = await deriveEscrow(owner);

  return buildAnchorIx({
    name: "set_escrow_armed",
    args: new Borsh().bool(armed).build(),
    accounts: [signer(owner), writable(escrow)],
  });
}

// ── claim_fee_to_escrow ──────────────────────────────────────────────────────────────────────
/**
 * `claim_fee`'s payout into the depositor's escrow: an `Active` position's settled share, or a
 * `ClosedBelowFloor`/`Seized` position's stored `accrued`, which leaves it closed and its card
 * claimable.
 */
export async function claimFeeToEscrow(params: {
  depositor: Address;
  poolId: number;
  nftMint: Address;
}): Promise<Instruction> {
  const { depositor, poolId, nftMint } = params;
  const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
  const { address: position } = await derivePda([
    POSITION_SEED,
    seedFromPubkey(pool),
    seedFromPubkey(nftMint),
  ]);
  const { address: topTier } = await derivePda([TOP_TIER_SEED, seedFromPubkey(pool)]);
  const { address: principalVault } = await derivePda([PRINCIPAL_VAULT_SEED, seedFromPubkey(pool)]);
  const { escrow, escrowVault } = await deriveEscrow(depositor);

  return buildAnchorIx({
    name: "claim_fee_to_escrow",
    accounts: [
      signer(depositor),
      writable(pool),
      writable(position),
      readonly(topTier),
      writable(principalVault),
      readonly(escrow),
      writable(escrowVault),
      readonly(TOKEN_PROGRAM),
    ],
  });
}
