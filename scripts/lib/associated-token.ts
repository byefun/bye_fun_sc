// Associated Token Account instructions used by production tooling. Kept here rather than in a
// test helper so operational scripts and their exact account layout are covered together.

import type { Address, Instruction } from "@solana/kit";

import { ASSOCIATED_TOKEN_PROGRAM, SYSTEM_PROGRAM, TOKEN_PROGRAM } from "../constants.js";
import { deriveAssociatedTokenAddress, readonly, writable, writableSigner } from "./anchor.js";

/**
 * Creates `(owner, mint)`'s ATA using the ATA program's `CreateIdempotent` variant. The idempotent
 * form is essential for operational retries and races: if another transaction creates the ATA
 * first, this instruction still succeeds provided the existing account is the same ATA.
 */
export async function createAssociatedTokenAccountIdempotent(params: {
  payer: Address;
  owner: Address;
  mint: Address;
}): Promise<{ address: Address; instruction: Instruction }> {
  const { payer, owner, mint } = params;
  const address = await deriveAssociatedTokenAddress(owner, mint);
  return {
    address,
    instruction: {
      programAddress: ASSOCIATED_TOKEN_PROGRAM,
      accounts: [
        writableSigner(payer),
        writable(address),
        readonly(owner),
        readonly(mint),
        readonly(SYSTEM_PROGRAM),
        readonly(TOKEN_PROGRAM),
      ],
      data: new Uint8Array([1]),
    },
  };
}
