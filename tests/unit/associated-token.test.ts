import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { AccountRole, address } from "@solana/kit";

import { ASSOCIATED_TOKEN_PROGRAM, SYSTEM_PROGRAM, TOKEN_PROGRAM } from "../../scripts/constants.js";
import { createAssociatedTokenAccountIdempotent } from "../../scripts/lib/associated-token.js";
import { deriveAssociatedTokenAddress } from "../../scripts/lib/anchor.js";

const PAYER = address("11111111111111111111111111111111");
const OWNER = address("BPFLoader1111111111111111111111111111111111");
const MINT = address("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

describe("createAssociatedTokenAccountIdempotent", () => {
  it("derives the canonical ATA and encodes CreateIdempotent with the required account order", async () => {
    const built = await createAssociatedTokenAccountIdempotent({ payer: PAYER, owner: OWNER, mint: MINT });
    assert.equal(built.address, await deriveAssociatedTokenAddress(OWNER, MINT));
    assert.equal(built.instruction.programAddress, ASSOCIATED_TOKEN_PROGRAM);
    assert.deepEqual(built.instruction.data, new Uint8Array([1]));
    assert.ok(built.instruction.accounts);
    assert.deepEqual(
      built.instruction.accounts.map(({ address: account, role }) => [account, role]),
      [
        [PAYER, AccountRole.WRITABLE_SIGNER],
        [built.address, AccountRole.WRITABLE],
        [OWNER, AccountRole.READONLY],
        [MINT, AccountRole.READONLY],
        [SYSTEM_PROGRAM, AccountRole.READONLY],
        [TOKEN_PROGRAM, AccountRole.READONLY],
      ],
    );
  });
});
