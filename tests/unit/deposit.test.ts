// Codec tests for scripts/lib/deposit.ts — five of the six deposit-lifecycle encoders.
// `return_rejected_core`, added at T-108, is covered on this axis by
// tests/unit/idl-oracle.test.ts, whose coverage pin refuses any IDL instruction without one.
// Mirrors tests/unit/admin.test.ts's pattern: discriminator derivation, fixed golden bytes for
// the arg encoding (independent of the encoder's own Borsh calls, so a field-order bug in
// production has to also survive an independently-written expected buffer here), and the
// account list's exact length and order — never a length or Set-membership check alone.

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
import { deposit, depositCore, approveDeposit, rejectDeposit, returnRejected } from "../../scripts/lib/deposit.js";
import {
  ASSOCIATED_TOKEN_PROGRAM,
  MPL_CORE_PROGRAM,
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
// deposit.ts — so a seed typo or a swapped seed constant in production shows up as a derived-
// address mismatch below rather than passing by construction.
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

// Buffer-level primitives, written independently of scripts/lib/anchor.ts's Borsh class, so the
// "golden bytes" below are a second path over the same spec rather than a round-trip.
const u16Buf = (n: number) => {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return b;
};
const u64Buf = (n: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(n);
  return b;
};
const i64Buf = (n: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigInt64LE(n);
  return b;
};

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

// ── deposit ───────────────────────────────────────────────────────────────────────────────────
describe("deposit", () => {
  const depositor = dummy(1);
  const nftMint = dummy(2);
  const depositorToken = dummy(3);
  const metadata = dummy(4);
  const masterEdition = dummy(5);
  const depositorTokenRecord = dummy(6);
  const positionTokenRecord = dummy(7);
  const collection = dummy(8);
  const poolId = 3;

  async function build(
    authorizationRulesProgram: Address | null = null,
    authorizationRules: Address | null = null,
  ) {
    return deposit({
      depositor,
      poolId,
      collection,
      nftMint,
      depositorToken,
      metadata,
      masterEdition,
      depositorTokenRecord,
      positionTokenRecord,
      sysvarInstructions: SYSVAR_INSTRUCTIONS,
      authorizationRulesProgram,
      authorizationRules,
    });
  }

  async function pdas() {
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
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
    const { address: poolCollection } = await derivePda([
      COLLECTION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(collection),
    ]);
    return { pool, poolCollection, position, positionVault, walletStats };
  }

  it("has the deposit discriminator", async () => {
    assertDiscriminator(await build(), "deposit");
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  it("has 19 accounts in exact IDL order and role", async () => {
    const ix = await build(dummy(90), dummy(91));
    const { pool, poolCollection, position, positionVault, walletStats } = await pdas();

    const expected = [
      { address: depositor, role: AccountRole.WRITABLE_SIGNER },
      { address: pool, role: AccountRole.WRITABLE },
      { address: poolCollection, role: AccountRole.READONLY },
      { address: position, role: AccountRole.WRITABLE },
      { address: positionVault, role: AccountRole.WRITABLE },
      { address: walletStats, role: AccountRole.WRITABLE },
      { address: nftMint, role: AccountRole.READONLY },
      { address: depositorToken, role: AccountRole.WRITABLE },
      { address: metadata, role: AccountRole.WRITABLE },
      { address: masterEdition, role: AccountRole.READONLY },
      { address: depositorTokenRecord, role: AccountRole.WRITABLE },
      { address: positionTokenRecord, role: AccountRole.WRITABLE },
      { address: TOKEN_METADATA_PROGRAM, role: AccountRole.READONLY },
      { address: dummy(90), role: AccountRole.READONLY },
      { address: dummy(91), role: AccountRole.READONLY },
      { address: SYSVAR_INSTRUCTIONS, role: AccountRole.READONLY },
      { address: TOKEN_PROGRAM, role: AccountRole.READONLY },
      { address: ASSOCIATED_TOKEN_PROGRAM, role: AccountRole.READONLY },
      { address: SYSTEM_PROGRAM, role: AccountRole.READONLY },
    ];
    assert.equal(expected.length, 19);
    assertAccountsExactly(ix, expected);
  });

  // Swap-sensitivity: every fixture address here is distinct, so `assertAccountsExactly`'s
  // ordered comparison fails if any two accounts (e.g. depositor_token_record ⇄
  // position_token_record, both writable and adjacent) were transposed in production —
  // a length or Set-membership check would not catch that.
  it("uses 19 distinct addresses in this fixture, so a transposition cannot pass by accident", async () => {
    const ix = await build(dummy(90), dummy(91));
    const addresses = accountsOf(ix).map((a) => a.address);
    assert.equal(new Set(addresses).size, addresses.length);
  });

  it("encodes a None authorization_rules pair as this program's own id, never by omitting the account", async () => {
    const ix = await build(null, null);
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 19);
    assert.equal(accounts[13].address, PROGRAM_ID);
    assert.equal(accounts[14].address, PROGRAM_ID);
    assert.equal(accounts[13].role, AccountRole.READONLY);
    assert.equal(accounts[14].role, AccountRole.READONLY);
  });

  it("encodes a Some authorization_rules pair as the given addresses, not omitted or shifted", async () => {
    const rulesProgram = dummy(92);
    const rules = dummy(93);
    const ix = await build(rulesProgram, rules);
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 19);
    assert.equal(accounts[13].address, rulesProgram);
    assert.equal(accounts[14].address, rules);
    // Every account after the optional pair still lines up — an omission would shift these.
    assert.equal(accounts[15].address, SYSVAR_INSTRUCTIONS);
    assert.equal(accounts[18].address, SYSTEM_PROGRAM);
  });
});

// ── deposit_core ─────────────────────────────────────────────────────────────────────────────
describe("depositCore", () => {
  const depositor = dummy(1);
  const asset = dummy(2);
  const collection = dummy(8);
  const poolId = 3;

  function build() {
    return depositCore({ depositor, poolId, collection, asset });
  }

  async function pdas() {
    const { address: pool } = await derivePda([POOL_SEED, seedFromU16Le(poolId)]);
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
    const { address: poolCollection } = await derivePda([
      COLLECTION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(collection),
    ]);
    return { pool, poolCollection, position, positionVault, walletStats };
  }

  it("has the deposit_core discriminator, which is not deposit's", async () => {
    const ix = await build();
    assertDiscriminator(ix, "deposit_core");
    // The two twins differ by one Anchor name and nothing else in their arg shape — both take
    // none — so the discriminator is the only thing separating them on the wire.
    assert.notDeepEqual(dataOf(ix).subarray(0, 8), ixDiscriminatorBytes("deposit"));
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  it("has 10 accounts in exact IDL order and role", async () => {
    const ix = await build();
    const { pool, poolCollection, position, positionVault, walletStats } = await pdas();

    const expected = [
      { address: depositor, role: AccountRole.WRITABLE_SIGNER },
      { address: pool, role: AccountRole.WRITABLE },
      { address: poolCollection, role: AccountRole.READONLY },
      { address: position, role: AccountRole.WRITABLE },
      { address: positionVault, role: AccountRole.READONLY },
      { address: walletStats, role: AccountRole.WRITABLE },
      { address: asset, role: AccountRole.WRITABLE },
      { address: collection, role: AccountRole.READONLY },
      { address: MPL_CORE_PROGRAM, role: AccountRole.READONLY },
      { address: SYSTEM_PROGRAM, role: AccountRole.READONLY },
    ];
    assert.equal(expected.length, 10);
    assertAccountsExactly(ix, expected);
  });

  it("uses 10 distinct addresses in this fixture, so a transposition cannot pass by accident", async () => {
    const addresses = accountsOf(await build()).map((a) => a.address);
    assert.equal(new Set(addresses).size, addresses.length);
  });

  /**
   * The `position` seed, which is the one derivation that differs from the twin's. Both seed on
   * `[POSITION_SEED, pool, <the instrument>]`; on Token Metadata the instrument is the mint, here
   * it is the `AssetV1` account. Asserted against a hand-built derivation rather than the
   * encoder's, so seeding on `collection` — the other 32-byte account in scope, and the one a
   * copy-paste from the record derivation would reach for — does not pass.
   */
  it("seeds position on the asset, never on the collection", async () => {
    const { pool, position } = await pdas();
    const { address: seededOnCollection } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(collection),
    ]);
    assert.notEqual(position, seededOnCollection);
    assert.equal(accountsOf(await build())[3].address, position);
  });

  /**
   * No Token Metadata account reaches this instruction. Stated as an absence over the whole
   * account list rather than by length: a client that reused `deposit`'s builder and dropped
   * nine slots would still be the wrong shape, and the length check above would catch that — but
   * a *tenth* Token Metadata account substituted for a Core one would not.
   */
  it("carries no Token Metadata, token or associated-token program account", async () => {
    const addresses = new Set(accountsOf(await build()).map((a) => a.address));
    for (const program of [TOKEN_METADATA_PROGRAM, TOKEN_PROGRAM, ASSOCIATED_TOKEN_PROGRAM, SYSVAR_INSTRUCTIONS]) {
      assert.equal(addresses.has(program), false, `${program} has no place in a Core deposit`);
    }
    assert.equal(addresses.has(MPL_CORE_PROGRAM), true);
  });
});

// ── approve_deposit ──────────────────────────────────────────────────────────────────────────
describe("approveDeposit", () => {
  const operator = dummy(10);
  const nftMint = dummy(11);
  const depositor = dummy(12);
  const weightIndex = dummy(13);
  const poolId = 5;
  const value = 12_000_000n;
  const observedAt = 1_800_000_000n;
  // Distinct from `observedAt` on purpose, and by more than a rounding error: the two are adjacent
  // i64s, so equal fixtures would make a transposition byte-identical and untestable.
  const lockUntil = 1_900_000_000n;

  async function build(displaced: Address | null) {
    return approveDeposit({
      operator,
      poolId,
      nftMint,
      depositor,
      weightIndex,
      displaced,
      value,
      observedAt,
      lockUntil,
    });
  }

  async function pdas() {
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
    return { protocolConfig, pool, position, topTier, walletStats };
  }

  it("has the approve_deposit discriminator", async () => {
    assertDiscriminator(await build(null), "approve_deposit");
  });

  it("encodes (value: u64, observed_at: i64, lock_until: i64) in that order", async () => {
    const ix = await build(null);
    assert.deepEqual(
      argsOf(ix),
      Buffer.concat([u64Buf(value), i64Buf(observedAt), i64Buf(lockUntil)]),
    );
  });

  // T-89. The last two args are both i64, so a transposition is length-preserving and type-clean.
  // `deepEqual` against the correct order already fails on it — this case exists to pin that the
  // *fixtures* discriminate, which they only do while the two values differ. Written as an
  // explicit inequality of the two encodings rather than a comment, so making the fixtures equal
  // reds a test instead of quietly disarming the one above.
  it("a transposed (lock_until, observed_at) encodes to different bytes — the fixtures discriminate", async () => {
    const correct = argsOf(await build(null));
    const swapped = argsOf(
      await approveDeposit({
        operator,
        poolId,
        nftMint,
        depositor,
        weightIndex,
        displaced: null,
        value,
        observedAt: lockUntil,
        lockUntil: observedAt,
      }),
    );
    assert.equal(correct.length, swapped.length, "a transposition must not change the arg length");
    assert.notDeepEqual(
      correct,
      swapped,
      "observedAt and lockUntil encode identically, so every ordering assertion in this file is \
vacuous — give them distinct fixture values",
    );
  });

  it("encodes a negative observed_at correctly (i64, not u64)", async () => {
    const ix = await approveDeposit({
      operator,
      poolId,
      nftMint,
      depositor,
      weightIndex,
      displaced: null,
      value,
      observedAt: -5n,
      lockUntil,
    });
    assert.deepEqual(argsOf(ix), Buffer.concat([u64Buf(value), i64Buf(-5n), i64Buf(lockUntil)]));
  });

  it("encodes lock_until = 0 — the non-Season-0 case the attestor sends by default", async () => {
    const ix = await approveDeposit({
      operator,
      poolId,
      nftMint,
      depositor,
      weightIndex,
      displaced: null,
      value,
      observedAt,
      lockUntil: 0n,
    });
    assert.deepEqual(argsOf(ix), Buffer.concat([u64Buf(value), i64Buf(observedAt), i64Buf(0n)]));
  });

  it("has 7 accounts in exact IDL order and role, operator not writable", async () => {
    const ix = await build(null);
    const { protocolConfig, pool, position, topTier, walletStats } = await pdas();

    const expected = [
      { address: operator, role: AccountRole.READONLY_SIGNER },
      { address: protocolConfig, role: AccountRole.READONLY },
      { address: pool, role: AccountRole.WRITABLE },
      { address: position, role: AccountRole.WRITABLE },
      { address: weightIndex, role: AccountRole.WRITABLE },
      { address: topTier, role: AccountRole.WRITABLE },
      { address: walletStats, role: AccountRole.WRITABLE },
    ];
    assert.equal(expected.length, 7);
    assertAccountsExactly(ix, expected);
  });

  it("appends no remaining_accounts when displaced is null", async () => {
    const ix = await build(null);
    assert.equal(accountsOf(ix).length, 7);
  });

  it("appends the displaced member as a single writable remaining_accounts entry when non-null", async () => {
    const displaced = dummy(50);
    const ix = await build(displaced);
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 8);
    assert.equal(accounts[7].address, displaced);
    assert.equal(accounts[7].role, AccountRole.WRITABLE);
  });
});

// ── reject_deposit ───────────────────────────────────────────────────────────────────────────
describe("rejectDeposit", () => {
  const operator = dummy(20);
  const pool = dummy(21);
  const nftMint = dummy(22);

  async function build(reason: number) {
    return rejectDeposit({ operator, pool, nftMint, reason });
  }

  it("has the reject_deposit discriminator", async () => {
    assertDiscriminator(await build(0), "reject_deposit");
  });

  it("encodes reason as a bare u16", async () => {
    const ix = await build(0x0102);
    assert.deepEqual(argsOf(ix), u16Buf(0x0102));
  });

  it("has 3 accounts in order: operator, protocol_config, position — operator not writable", async () => {
    const ix = await build(1);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const { address: position } = await derivePda([
      POSITION_SEED,
      seedFromPubkey(pool),
      seedFromPubkey(nftMint),
    ]);

    assertAccountsExactly(ix, [
      { address: operator, role: AccountRole.READONLY_SIGNER },
      { address: protocolConfig, role: AccountRole.READONLY },
      { address: position, role: AccountRole.WRITABLE },
    ]);
  });

  it("derives position from pool/nft_mint directly — a different pool changes the PDA", async () => {
    const ixA = await rejectDeposit({ operator, pool: dummy(21), nftMint, reason: 0 });
    const ixB = await rejectDeposit({ operator, pool: dummy(23), nftMint, reason: 0 });
    assert.notEqual(accountsOf(ixA)[2].address, accountsOf(ixB)[2].address);
  });
});

// ── return_rejected ──────────────────────────────────────────────────────────────────────────
describe("returnRejected", () => {
  const payer = dummy(30);
  const depositor = dummy(31);
  const pool = dummy(32);
  const nftMint = dummy(33);
  const depositorToken = dummy(34);
  const metadata = dummy(35);
  const masterEdition = dummy(36);
  const positionTokenRecord = dummy(37);
  const depositorTokenRecord = dummy(38);

  async function build(
    authorizationRulesProgram: Address | null = null,
    authorizationRules: Address | null = null,
  ) {
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

  it("has the return_rejected discriminator", async () => {
    assertDiscriminator(await build(), "return_rejected");
  });

  it("takes no args", async () => {
    assert.equal(argsOf(await build()).length, 0);
  });

  it("has 17 accounts in exact IDL order and role — payer signs, depositor does not", async () => {
    const ix = await build(dummy(94), dummy(95));
    const { position, positionVault } = await pdas();

    const expected = [
      { address: payer, role: AccountRole.WRITABLE_SIGNER },
      { address: depositor, role: AccountRole.WRITABLE },
      { address: position, role: AccountRole.WRITABLE },
      { address: positionVault, role: AccountRole.WRITABLE },
      { address: nftMint, role: AccountRole.READONLY },
      { address: depositorToken, role: AccountRole.WRITABLE },
      { address: metadata, role: AccountRole.WRITABLE },
      { address: masterEdition, role: AccountRole.READONLY },
      { address: positionTokenRecord, role: AccountRole.WRITABLE },
      { address: depositorTokenRecord, role: AccountRole.WRITABLE },
      { address: TOKEN_METADATA_PROGRAM, role: AccountRole.READONLY },
      { address: dummy(94), role: AccountRole.READONLY },
      { address: dummy(95), role: AccountRole.READONLY },
      { address: SYSVAR_INSTRUCTIONS, role: AccountRole.READONLY },
      { address: TOKEN_PROGRAM, role: AccountRole.READONLY },
      { address: ASSOCIATED_TOKEN_PROGRAM, role: AccountRole.READONLY },
      { address: SYSTEM_PROGRAM, role: AccountRole.READONLY },
    ];
    assert.equal(expected.length, 17);
    assertAccountsExactly(ix, expected);
  });

  // Swap-sensitivity: return_rejected's position_token_record/depositor_token_record pair is
  // the exact shape (adjacent, both writable, both UncheckedAccount) that this project has
  // already found transposed twice elsewhere (principal_vault ⇄ treasury_04, in Rust and again
  // in admin.ts) — distinct fixture addresses make the ordered comparison above sensitive to it.
  it("uses 17 distinct addresses in this fixture, so a transposition cannot pass by accident", async () => {
    const ix = await build(dummy(94), dummy(95));
    const addresses = accountsOf(ix).map((a) => a.address);
    assert.equal(new Set(addresses).size, addresses.length);
  });

  it("encodes a None authorization_rules pair as this program's own id, never by omitting the account", async () => {
    const ix = await build(null, null);
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 17);
    assert.equal(accounts[11].address, PROGRAM_ID);
    assert.equal(accounts[12].address, PROGRAM_ID);
    assert.equal(accounts[11].role, AccountRole.READONLY);
    assert.equal(accounts[12].role, AccountRole.READONLY);
  });

  it("encodes a Some authorization_rules pair as the given addresses, not omitted or shifted", async () => {
    const rulesProgram = dummy(96);
    const rules = dummy(97);
    const ix = await build(rulesProgram, rules);
    const accounts = accountsOf(ix);
    assert.equal(accounts.length, 17);
    assert.equal(accounts[11].address, rulesProgram);
    assert.equal(accounts[12].address, rules);
    assert.equal(accounts[13].address, SYSVAR_INSTRUCTIONS);
    assert.equal(accounts[16].address, SYSTEM_PROGRAM);
  });

  it("depositor is writable but not a signer — return_rejected is permissionless for payer only", async () => {
    const ix = await build();
    const accounts = accountsOf(ix);
    assert.equal(accounts[0].role, AccountRole.WRITABLE_SIGNER); // payer
    assert.equal(accounts[1].role, AccountRole.WRITABLE); // depositor — no signer bit
  });
});
