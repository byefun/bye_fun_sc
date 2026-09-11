// Codec tests for scripts/lib/exit.ts — the two Token Metadata exit encoders. The three
// added at T-108 (withdraw_core, claim_nft_core, close_seized) are covered on this axis by
// tests/unit/idl-oracle.test.ts, whose coverage pin refuses any IDL instruction without one.
// Mirrors
// tests/unit/admin.test.ts's and tests/unit/deposit.test.ts's pattern: discriminator
// derivation, the account list's exact length, address AND role in order — never a length or
// Set-membership check alone, which would pass a transposition of two same-role accounts.
//
// The first describe block below is F-2 — the NFT exit destination is unvalidated. It is a
// unit-level scaffold,
// not a verdict: `depositor_token` on `return_rejected` is a bare, unvalidated address, and what a
// codec test can prove — and all it claims to prove — is that the encoder is a dumb, unconditional
// pass-through with no branch that could catch it either. `return_rejected`'s own encoder lives in
// scripts/lib/deposit.ts (T-40's file, already landed) — imported here read-only, never edited,
// because the substitution question is asked of that instruction's shape, not exit.ts's own.
//
// F-2 itself is now settled against a live validator, in
// tests/integration/return-rejected-custody.test.ts: an attacker-owned, already-existing
// `depositor_token` is rejected by Token Metadata's own `TransferV1` CPI with error 57
// (`IncorrectOwner`) — bye_machine's own program never gets a chance to accept or reject it. F-2
// closes as **Token Metadata's own catch**, not bye_machine's; nothing here or in that file found
// a defect. What this scaffold still proves, and only this: the encoder does not add a check of
// its own that would make that validator-level test redundant.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  AccountRole,
  getAddressDecoder,
  getAddressEncoder,
  type Address,
  type Instruction,
} from "@solana/kit";
import { derivePda, seedFromPubkey } from "../../scripts/lib/anchor.js";
import { withdraw, claimNft } from "../../scripts/lib/exit.js";
import { returnRejected } from "../../scripts/lib/deposit.js";
import {
  ASSOCIATED_TOKEN_PROGRAM,
  PROGRAM_ID,
  SYSTEM_PROGRAM,
  SYSVAR_INSTRUCTIONS,
  TOKEN_METADATA_PROGRAM,
  TOKEN_PROGRAM,
} from "../../scripts/constants.js";

// ── Fixtures ──────────────────────────────────────────────────────────────────────────────────
const addrEnc = getAddressEncoder();
const addrDec = getAddressDecoder();

/** A distinct, valid `Address` per `n` — arbitrary 32-byte pubkeys, not real keys. */
function dummy(n: number): Address {
  return addrDec.decode(Buffer.alloc(32, n));
}

// Seeds, sourced independently from programs/bye_machine/src/common/seeds.rs — not copied from
// exit.ts — so a seed typo or a swapped seed constant in production shows up as a derived-
// address mismatch below rather than passing by construction.
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

function ixDiscriminatorBytes(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

/** `buildAnchorIx` always sets `data`/`accounts` — the base `Instruction` type just allows for
 * instructions with neither. */
function dataOf(ix: Instruction): Buffer {
  assert.ok(ix.data, "instruction has no data");
  return Buffer.from(ix.data);
}

function accountsOf(ix: Instruction) {
  assert.ok(ix.accounts, "instruction has no accounts");
  return ix.accounts;
}

function assertDiscriminator(ix: Instruction, name: string) {
  assert.deepEqual(dataOf(ix).subarray(0, 8), ixDiscriminatorBytes(name));
}

function argsOf(ix: Instruction): Buffer {
  return dataOf(ix).subarray(8);
}

/** Asserts the full ordered (address, role) list — never a length or set check, which would
 * pass a transposition of two same-role accounts. */
function assertAccountsExactly(
  ix: Instruction,
  expected: Array<{ address: Address; role: AccountRole }>,
) {
  const accounts = accountsOf(ix);
  assert.equal(accounts.length, expected.length);
  assert.deepEqual(
    accounts.map((a) => a.address),
    expected.map((e) => e.address),
  );
  assert.deepEqual(
    accounts.map((a) => a.role),
    expected.map((e) => e.role),
  );
}

// ── F-2: return_rejected's depositor_token is a bare, unvalidated pass-through ──────────────
describe("F-2 scaffold — return_rejected's depositor_token substitution (unit-level, no validator)", () => {
  const payer = dummy(90); // the attacker: signs, funds the tx
  const depositor = dummy(91); // the true depositor: pinned by position.depositor == depositor.key() @ 6302, never a signer
  const pool = dummy(92);
  const nftMint = dummy(93);
  const metadata = dummy(94);
  const masterEdition = dummy(95);
  const positionTokenRecord = dummy(96);
  const depositorTokenRecord = dummy(97);

  async function build(depositorToken: Address) {
    return returnRejected({
      payer,
      depositor,
      pool,
      nftMint,
      depositorToken,
      metadata,
      masterEdition,
      positionTokenRecord,
      depositorTokenRecord,
      sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: null,
      authorizationRules: null,
    });
  }

  // Severity split: `depositor` is pinned by a
  // constraint on `position`'s own attribute block, not on `depositor`'s — an attacker as
  // `payer` is forced by 6302 to name the true `depositor`, but nothing forces the
  // `depositor_token` they supply to actually belong to that depositor. This is the encoder's
  // whole surface for that question: does it accept an attacker-controlled `depositor_token`
  // exactly as readily as the depositor's own, with `depositor` unaffected either way?
  it("places an attacker-controlled depositor_token verbatim, at the same slot, regardless of whose it is", async () => {
    const attackerOwned = dummy(1); // stands in for a token account the attacker (payer) controls
    const depositorOwned = dummy(2); // stands in for the true depositor's own ATA for (depositor, nftMint)

    const ixAttacker = await build(attackerOwned);
    const ixLegitimate = await build(depositorOwned);

    const attackerAccounts = accountsOf(ixAttacker);
    const legitimateAccounts = accountsOf(ixLegitimate);

    // Slot 5 is depositor_token (payer, depositor, position, position_vault, nft_mint, then
    // depositor_token) — verified against deposit.ts's own returnRejected account order.
    assert.equal(attackerAccounts[5].address, attackerOwned);
    assert.equal(legitimateAccounts[5].address, depositorOwned);
    assert.equal(attackerAccounts[5].role, AccountRole.WRITABLE);
    assert.equal(legitimateAccounts[5].role, AccountRole.WRITABLE);

    // Nothing else in the instruction changes with depositor_token — in particular `depositor`
    // itself (slot 1) is identical in both, and identical to the pinned true depositor. The
    // encoder has no way to know, and adds no check, that slot 5 belongs to the party named in
    // slot 1: swapping only that one slot is the entire diff between the two instructions.
    assert.equal(attackerAccounts[1].address, depositor);
    assert.equal(legitimateAccounts[1].address, depositor);
    const withoutDepositorToken = (accs: typeof attackerAccounts) =>
      accs.filter((_, i) => i !== 5).map((a) => a.address);
    assert.deepEqual(withoutDepositorToken(attackerAccounts), withoutDepositorToken(legitimateAccounts));
  });

  it("payer signs and depositor does not — the encoder expresses return_rejected's exact permissionless shape", async () => {
    const ix = await build(dummy(3));
    const accounts = accountsOf(ix);
    assert.equal(accounts[0].address, payer);
    assert.equal(accounts[0].role, AccountRole.WRITABLE_SIGNER);
    assert.equal(accounts[1].address, depositor);
    assert.equal(accounts[1].role, AccountRole.WRITABLE);
    assert.notEqual(payer, depositor, "payer and the pinned depositor must be distinct addresses in this fixture");
  });
});

// ── withdraw ─────────────────────────────────────────────────────────────────────────────────
describe("withdraw", () => {
  const depositor = dummy(1);
  const poolId = 5;
  const nftMint = dummy(2);
  const weightIndex = dummy(3);
  const depositorToken = dummy(5);
  const metadata = dummy(6);
  const masterEdition = dummy(7);
  const positionTokenRecord = dummy(8);
  const depositorTokenRecord = dummy(9);

  async function build(opts: {
    depositorUsdc?: Address | null;
    authorizationRulesProgram?: Address | null;
    authorizationRules?: Address | null;
    successor?: Address | null;
  } = {}) {
    return withdraw({
      depositor,
      poolId,
      nftMint,
      weightIndex,
      depositorUsdc: opts.depositorUsdc ?? null,
      depositorToken,
      metadata,
      masterEdition,
      positionTokenRecord,
      depositorTokenRecord,
      sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: opts.authorizationRulesProgram ?? null,
      authorizationRules: opts.authorizationRules ?? null,
      successor: opts.successor ?? null,
    });
  }

  async function pdas() {
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
    return { pool, position, topTier, walletStats, principalVault, positionVault };
  }

  it("has the withdraw discriminator", async () => {
    assertDiscriminator(await build(), "withdraw");
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  it("has 22 accounts in exact IDL order and role", async () => {
    const rulesProgram = dummy(10);
    const rules = dummy(11);
    const depositorUsdc = dummy(4);
    const ix = await build({ depositorUsdc, authorizationRulesProgram: rulesProgram, authorizationRules: rules });
    const { pool, position, topTier, walletStats, principalVault, positionVault } = await pdas();

    const expected = [
      { address: depositor, role: AccountRole.WRITABLE_SIGNER },
      { address: pool, role: AccountRole.WRITABLE },
      { address: position, role: AccountRole.WRITABLE },
      { address: weightIndex, role: AccountRole.WRITABLE },
      { address: topTier, role: AccountRole.WRITABLE },
      { address: walletStats, role: AccountRole.WRITABLE },
      { address: principalVault, role: AccountRole.WRITABLE },
      { address: depositorUsdc, role: AccountRole.WRITABLE },
      { address: positionVault, role: AccountRole.WRITABLE },
      { address: nftMint, role: AccountRole.READONLY },
      { address: depositorToken, role: AccountRole.WRITABLE },
      { address: metadata, role: AccountRole.WRITABLE },
      { address: masterEdition, role: AccountRole.READONLY },
      { address: positionTokenRecord, role: AccountRole.WRITABLE },
      { address: depositorTokenRecord, role: AccountRole.WRITABLE },
      { address: TOKEN_METADATA_PROGRAM, role: AccountRole.READONLY },
      { address: rulesProgram, role: AccountRole.READONLY },
      { address: rules, role: AccountRole.READONLY },
      { address: SYSVAR_INSTRUCTIONS, role: AccountRole.READONLY },
      { address: TOKEN_PROGRAM, role: AccountRole.READONLY },
      { address: ASSOCIATED_TOKEN_PROGRAM, role: AccountRole.READONLY },
      { address: SYSTEM_PROGRAM, role: AccountRole.READONLY },
    ];
    assert.equal(expected.length, 22);
    assertAccountsExactly(ix, expected);
  });

  it("uses 22 distinct addresses in this fixture, so a transposition cannot pass by accident", async () => {
    const ix = await build({
      depositorUsdc: dummy(4),
      authorizationRulesProgram: dummy(10),
      authorizationRules: dummy(11),
    });
    const addresses = accountsOf(ix).map((a) => a.address);
    assert.equal(new Set(addresses).size, addresses.length);
  });

  it("fails to match if two same-role accounts are swapped", async () => {
    const ix = await build();
    const actual = accountsOf(ix).map((a) => a.address);
    // position_token_record ⇄ depositor_token_record: adjacent, both writable, both an
    // UncheckedAccount — the exact shape this project has already found transposed twice
    // elsewhere (principal_vault ⇄ treasury_04, in Rust and again in admin.ts).
    const swapped = [...actual];
    [swapped[13], swapped[14]] = [swapped[14], swapped[13]];
    assert.notDeepEqual(actual, swapped);
    assert.equal(actual.length, swapped.length, "a length check alone would not distinguish these");
    assert.equal(new Set(actual).size, new Set(swapped).size, "nor would a set-membership check");
  });

  it("encodes a None depositor_usdc as this program's own id, marked READONLY — never omitted, never writable", async () => {
    // Unlike the auth-rules pair below, this slot's role is polarity-dependent:
    // `withdraw.rs`'s `depositor_usdc` is `mut` when present (it receives the principal payout),
    // so the `null` case must be `readonly` — this program's own address is also the invoked
    // program in the same transaction, and marking it `writable` here is illegal at the
    // transaction-format level ("invoked and marked writable"), independent of anything the
    // handler itself checks. T-48's rule-set replay found every `null`-payout withdraw failing
    // this way before the escrow logic ever ran.
    const ix = await build({ depositorUsdc: null });
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 22);
    assert.equal(accounts[7].address, PROGRAM_ID);
    assert.equal(accounts[7].role, AccountRole.READONLY);
  });

  it("encodes a Some depositor_usdc as the given address, WRITABLE, not omitted or shifted", async () => {
    const depositorUsdc = dummy(4);
    const ix = await build({ depositorUsdc });
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 22);
    assert.equal(accounts[7].address, depositorUsdc);
    assert.equal(
      accounts[7].role,
      AccountRole.WRITABLE,
      "depositor_usdc is mut in withdraw.rs when present — a regression to a blanket readonly must fail here",
    );
    assert.equal(accounts[8].address, (await pdas()).positionVault, "position_vault must not shift");
  });

  it("never marks this program's own address writable, in either depositor_usdc polarity", async () => {
    // The actual invariant a runtime enforces: an account equal to the invoked program's own
    // address can never be writable in the same transaction. Checked generally, not just at
    // index 7, so a future None-sentinel slot added to this instruction inherits the same net.
    for (const opts of [{ depositorUsdc: null }, { depositorUsdc: dummy(4) }] as const) {
      const ix = await build(opts);
      for (const account of accountsOf(ix)) {
        if (account.address === PROGRAM_ID) {
          assert.notEqual(account.role, AccountRole.WRITABLE, `PROGRAM_ID marked writable (opts: ${JSON.stringify(opts)})`);
          assert.notEqual(
            account.role,
            AccountRole.WRITABLE_SIGNER,
            `PROGRAM_ID marked writable-signer (opts: ${JSON.stringify(opts)})`,
          );
        }
      }
    }
  });

  it("encodes a None authorization_rules pair as this program's own id, never by omitting the account", async () => {
    const ix = await build({ authorizationRulesProgram: null, authorizationRules: null });
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 22);
    assert.equal(accounts[16].address, PROGRAM_ID);
    assert.equal(accounts[17].address, PROGRAM_ID);
    assert.equal(accounts[16].role, AccountRole.READONLY);
    assert.equal(accounts[17].role, AccountRole.READONLY);
  });

  it("encodes a Some authorization_rules pair as the given addresses, not omitted or shifted", async () => {
    const rulesProgram = dummy(10);
    const rules = dummy(11);
    const ix = await build({ authorizationRulesProgram: rulesProgram, authorizationRules: rules });
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 22);
    assert.equal(accounts[16].address, rulesProgram);
    assert.equal(accounts[17].address, rules);
    assert.equal(accounts[18].address, SYSVAR_INSTRUCTIONS);
    assert.equal(accounts[21].address, SYSTEM_PROGRAM);
  });

  // `withdraw.rs:153`'s `fill_vacancy_from_candidate` reads `ctx.remaining_accounts`
  // positionally (`.first()`), not by key — `Ok(None)` on an empty array, no error at all.
  it("omits the successor from remaining_accounts entirely when null — never a placeholder address", async () => {
    const ix = await build({ successor: null });
    assert.equal(accountsOf(ix).length, 22, "no 23rd account when there is no successor");
  });

  it("appends a non-null successor as one writable, non-signer remaining_account, after the 22 named accounts", async () => {
    const successor = dummy(20);
    const ix = await build({ successor });
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 23);
    assert.equal(accounts[22].address, successor);
    assert.equal(accounts[22].role, AccountRole.WRITABLE);
  });

  it("depositor signs and is the only signer", async () => {
    const ix = await build();
    const accounts = accountsOf(ix);
    assert.equal(accounts[0].role, AccountRole.WRITABLE_SIGNER);
    for (const a of accounts.slice(1)) {
      assert.notEqual(a.role, AccountRole.WRITABLE_SIGNER);
      assert.notEqual(a.role, AccountRole.READONLY_SIGNER);
    }
  });
});

// ── claim_nft ────────────────────────────────────────────────────────────────────────────────
describe("claimNft", () => {
  const depositor = dummy(50);
  const pool = dummy(51);
  const nftMint = dummy(52);
  const depositorToken = dummy(53);
  const metadata = dummy(54);
  const masterEdition = dummy(55);
  const positionTokenRecord = dummy(56);
  const depositorTokenRecord = dummy(57);

  async function build(
    authorizationRulesProgram: Address | null = null,
    authorizationRules: Address | null = null,
  ) {
    return claimNft({
      depositor,
      pool,
      nftMint,
      depositorToken,
      metadata,
      masterEdition,
      positionTokenRecord,
      depositorTokenRecord,
      sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram,
      authorizationRules,
    });
  }

  async function pdas() {
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(nftMint),
    ]);
    const { address: positionVault } = await derivePda([POSITION_VAULT_SEED, seedFromPubkey(position)]);
    return { position, positionVault };
  }

  it("has the claim_nft discriminator", async () => {
    assertDiscriminator(await build(), "claim_nft");
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  it("has 16 accounts in exact IDL order and role — no pool, no protocol_config account", async () => {
    const ix = await build(dummy(58), dummy(59));
    const { position, positionVault } = await pdas();

    const expected = [
      { address: depositor, role: AccountRole.WRITABLE_SIGNER },
      { address: position, role: AccountRole.WRITABLE },
      { address: positionVault, role: AccountRole.WRITABLE },
      { address: nftMint, role: AccountRole.READONLY },
      { address: depositorToken, role: AccountRole.WRITABLE },
      { address: metadata, role: AccountRole.WRITABLE },
      { address: masterEdition, role: AccountRole.READONLY },
      { address: positionTokenRecord, role: AccountRole.WRITABLE },
      { address: depositorTokenRecord, role: AccountRole.WRITABLE },
      { address: TOKEN_METADATA_PROGRAM, role: AccountRole.READONLY },
      { address: dummy(58), role: AccountRole.READONLY },
      { address: dummy(59), role: AccountRole.READONLY },
      { address: SYSVAR_INSTRUCTIONS, role: AccountRole.READONLY },
      { address: TOKEN_PROGRAM, role: AccountRole.READONLY },
      { address: ASSOCIATED_TOKEN_PROGRAM, role: AccountRole.READONLY },
      { address: SYSTEM_PROGRAM, role: AccountRole.READONLY },
    ];
    assert.equal(expected.length, 16);
    assertAccountsExactly(ix, expected);
    assert.notEqual(pool, position, "pool must not itself appear as an account in this instruction");
    for (const a of accountsOf(ix)) assert.notEqual(a.address, pool);
  });

  it("uses 16 distinct addresses in this fixture, so a transposition cannot pass by accident", async () => {
    const ix = await build(dummy(58), dummy(59));
    const addresses = accountsOf(ix).map((a) => a.address);
    assert.equal(new Set(addresses).size, addresses.length);
  });

  it("fails to match if two same-role accounts are swapped", async () => {
    const ix = await build();
    const actual = accountsOf(ix).map((a) => a.address);
    // position_token_record ⇄ depositor_token_record — same shape as withdraw's own instance
    // of this class, checked independently here since claim_nft is a distinct instruction.
    const swapped = [...actual];
    [swapped[7], swapped[8]] = [swapped[8], swapped[7]];
    assert.notDeepEqual(actual, swapped);
    assert.equal(actual.length, swapped.length, "a length check alone would not distinguish these");
    assert.equal(new Set(actual).size, new Set(swapped).size, "nor would a set-membership check");
  });

  it("encodes a None authorization_rules pair as this program's own id, never by omitting the account", async () => {
    const ix = await build(null, null);
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 16);
    assert.equal(accounts[10].address, PROGRAM_ID);
    assert.equal(accounts[11].address, PROGRAM_ID);
    assert.equal(accounts[10].role, AccountRole.READONLY);
    assert.equal(accounts[11].role, AccountRole.READONLY);
  });

  it("encodes a Some authorization_rules pair as the given addresses, not omitted or shifted", async () => {
    const rulesProgram = dummy(58);
    const rules = dummy(59);
    const ix = await build(rulesProgram, rules);
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 16);
    assert.equal(accounts[10].address, rulesProgram);
    assert.equal(accounts[11].address, rules);
    assert.equal(accounts[12].address, SYSVAR_INSTRUCTIONS);
    assert.equal(accounts[15].address, SYSTEM_PROGRAM);
  });

  it("depositor signs and is the only signer", async () => {
    const ix = await build();
    const accounts = accountsOf(ix);
    assert.equal(accounts[0].role, AccountRole.WRITABLE_SIGNER);
    for (const a of accounts.slice(1)) {
      assert.notEqual(a.role, AccountRole.WRITABLE_SIGNER);
      assert.notEqual(a.role, AccountRole.READONLY_SIGNER);
    }
  });

  it("re-derives position with a different pool and gets a different address", async () => {
    const ixA = await claimNft({
      depositor,
      pool,
      nftMint,
      depositorToken,
      metadata,
      masterEdition,
      positionTokenRecord,
      depositorTokenRecord,
      sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: null,
      authorizationRules: null,
    });
    const ixB = await claimNft({
      depositor,
      pool: dummy(61),
      nftMint,
      depositorToken,
      metadata,
      masterEdition,
      positionTokenRecord,
      depositorTokenRecord,
      sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram: null,
      authorizationRules: null,
    });
    assert.notEqual(accountsOf(ixA)[1].address, accountsOf(ixB)[1].address);
  });
});
