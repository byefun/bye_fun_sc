// Hand-encoded SPL Associated Token Account `Create` instruction — same reason
// scripts/lib/weight-index.ts's own `SystemProgram::CreateAccount` is hand-encoded there rather
// than adding a dependency. Creating a token account for a mint OWNED BY A THIRD PARTY is exactly
// the shape F-2's custody question needs and no existing encoder in this repo produces: every
// fixture-minting path in fixtures.ts creates the mint's OWN owner's ATA via Token Metadata's
// `MintV1` CPI, never an arbitrary third party's.
//
// Layout (spl-associated-token-account): `Create` (legacy variant, instruction index 0 — the
// program treats empty instruction data as `Create`) takes no args. Accounts, in order:
// funding account (signer, writable) ‖ the ATA itself (writable) ‖ wallet (readonly) ‖
// mint (readonly) ‖ system program (readonly) ‖ token program (readonly).

import type { Address, Instruction } from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import { readonly, writable, writableSigner } from "../../scripts/lib/anchor.js";
import { ASSOCIATED_TOKEN_PROGRAM, SYSTEM_PROGRAM, TOKEN_PROGRAM } from "../../scripts/constants.js";

/**
 * Builds the `Create` instruction for `(owner, mint)`'s associated token account, funded by
 * `payer`, and returns the account's own address alongside it. Fails on-chain if the account
 * already exists — this repo has no use here for `CreateIdempotent` (index 1): every call site
 * means to prove the account did NOT exist before this instruction ran.
 */
export async function createAtaInstruction(params: {
  payer: Address;
  owner: Address;
  mint: Address;
}): Promise<{ address: Address; instruction: Instruction }> {
  const { payer, owner, mint } = params;
  const address = await getTokenAccountAddress(owner, mint, false);
  const instruction: Instruction = {
    programAddress: ASSOCIATED_TOKEN_PROGRAM,
    accounts: [
      writableSigner(payer),
      writable(address),
      readonly(owner),
      readonly(mint),
      readonly(SYSTEM_PROGRAM),
      readonly(TOKEN_PROGRAM),
    ],
    data: new Uint8Array(0),
  };
  return { address, instruction };
}
