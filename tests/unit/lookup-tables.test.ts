// What the lookup tables buy, measured per instruction, and the codec that writes them.
//
// The size numbers here are computed by @solana/kit's own compiler and encoder — the same code
// path that produces the bytes sent to a validator — rather than by a byte formula written
// alongside the assertion it is checked against. What this file supplies is the account census
// (from the IDL) and the tier map; the arithmetic is not this file's to get wrong.
//
// Addresses are synthesised, not real. Size depends on how many distinct keys a transaction
// carries and how many of them a table resolves, never on their values, so a deterministic
// distinct address per slot measures the same lengths a live deployment does — and the worst case
// is exactly the all-distinct one.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import {
  address,
  getAddressEncoder,
  getBase58Decoder,
  type Address,
  type Instruction,
} from "@solana/kit";
import { AccountRole } from "../../scripts/lib/anchor.js";
import {
  ACCOUNT_TIER,
  ACCOUNT_TIER_OVERRIDES,
  buildCreateLookupTableInstruction,
  buildExtendLookupTableInstruction,
  buildDeactivateLookupTableInstruction,
  buildCloseLookupTableInstruction,
  chunkAddresses,
  deriveLookupTableAddress,
  ADDRESS_LOOKUP_TABLE_PROGRAM,
  decodeLookupTable,
  EXTEND_CHUNK_SIZE,
  LOOKUP_TABLE_META_SIZE,
  LOOKUP_TABLE_NOT_DEACTIVATED,
  MAX_ADDRESSES_PER_TABLE,
  missingAddresses,
  resolveClusterTier,
  resolveDepositCommonTier,
  tierOf,
} from "../../scripts/lib/lookup-tables.js";
import {
  ASSOCIATED_TOKEN_PROGRAM,
  AUTH_RULES_PROGRAM,
  MPL_CORE_PROGRAM,
  SYSVAR_INSTRUCTIONS,
  SYSTEM_PROGRAM,
  TOKEN_METADATA_PROGRAM,
  TOKEN_PROGRAM,
} from "../../scripts/constants.js";
import {
  measureTransactionSize,
  TRANSACTION_SIZE_LIMIT,
  type LookupTableSet,
} from "../../scripts/lib/v0-tx.js";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

type IdlAccount = { name: string; writable?: boolean; signer?: boolean; optional?: boolean };
type IdlInstruction = { name: string; accounts: IdlAccount[]; args: unknown[] };

const idl: { instructions: IdlInstruction[] } = JSON.parse(
  readFileSync(resolve(REPO_ROOT, "idl/bye_machine.json"), "utf8"),
);

/** A distinct, valid, deterministic address per label — the same label always yields the same one. */
function syntheticAddress(label: string): Address {
  const bytes = createHash("sha256").update(label).digest().subarray(0, 32);
  // Clear the top byte so every synthetic address encodes to 32 bytes off-curve-or-not; length on
  // the wire is fixed at 32 regardless, and base58 length never enters the measurement.
  bytes[0] = 0;
  return address(getBase58Decoder().decode(bytes));
}

const PROGRAM = syntheticAddress("bye_machine:program");

/** The one table. Every tier goes into it, for every pool. */
const TABLE = syntheticAddress("table:bye_machine");

/**
 * The designed pool count. The extra pools contribute six keys each that no single transaction
 * touches, which is the property the sizing below has to hold across.
 */
const POOL_COUNT = 4;

/**
 * The address a slot carries. A tabled slot is keyed by account name alone, so the same name is
 * the same key in every instruction — which is the whole premise of tabling it. A dynamic slot is
 * keyed by instruction and name, so no two dynamic slots collide and the count is the worst case.
 */
function addressForSlot(ixName: string, account: IdlAccount): Address {
  const tier = tierOf(account.name, ixName);
  if (tier === undefined || tier === "dynamic") {
    return syntheticAddress(`dynamic:${ixName}.${account.name}`);
  }
  return syntheticAddress(tier === "pool" ? `pool0:${account.name}` : `${tier}:${account.name}`);
}

function instructionFor(ix: IdlInstruction): { instruction: Instruction; feePayer: Address } {
  const accounts = ix.accounts.map((a) => ({
    address: addressForSlot(ix.name, a),
    role: a.signer
      ? a.writable
        ? AccountRole.WRITABLE_SIGNER
        : AccountRole.READONLY_SIGNER
      : a.writable
        ? AccountRole.WRITABLE
        : AccountRole.READONLY,
  }));
  const firstSigner = ix.accounts.findIndex((a) => a.signer);
  const feePayer =
    firstSigner >= 0 ? accounts[firstSigner].address : syntheticAddress(`payer:${ix.name}`);
  // 8-byte discriminator plus a representative argument payload; the args' true widths are small
  // next to 32-byte keys and are not what this file measures.
  return {
    instruction: { programAddress: PROGRAM, accounts, data: new Uint8Array(8 + 8 * ix.args.length) },
    feePayer,
  };
}

/**
 * The table, holding every tabled key of every instruction — plus the per-pool keys of the other
 * three pools, which the measured transactions never name. Those entries must cost nothing.
 */
function buildTable(ixs: IdlInstruction[]): LookupTableSet {
  const keys: Address[] = [];
  const add = (a: Address) => {
    if (!keys.includes(a)) keys.push(a);
  };
  for (const ix of ixs) {
    for (const a of ix.accounts) {
      const tier = tierOf(a.name, ix.name);
      if (tier === undefined || tier === "dynamic") continue;
      add(addressForSlot(ix.name, a));
      if (tier === "pool") {
        for (let p = 1; p < POOL_COUNT; p++) add(syntheticAddress(`pool${p}:${a.name}`));
      }
    }
  }
  return { [TABLE]: keys };
}

const TABLES = buildTable(idl.instructions);

type Measurement = { name: string; accounts: number; before: number; after: number };

const measurements: Measurement[] = idl.instructions
  .map((ix) => {
    const { instruction, feePayer } = instructionFor(ix);
    return {
      name: ix.name,
      accounts: ix.accounts.length,
      before: measureTransactionSize({ feePayer, instructions: [instruction] }),
      after: measureTransactionSize({ feePayer, instructions: [instruction], lookupTables: TABLES }),
    };
  })
  .sort((a, b) => b.before - a.before);

describe("address lookup tables — classification", () => {
  it("classifies every account name the IDL declares", () => {
    const unclassified = new Set<string>();
    for (const ix of idl.instructions) {
      for (const a of ix.accounts) {
        if (tierOf(a.name, ix.name) === undefined) unclassified.add(`${ix.name}.${a.name}`);
      }
    }
    assert.deepEqual([...unclassified], [], [...unclassified].join(", "));
  });

  it("self-test: a name the IDL does not declare is unclassified", () => {
    assert.equal(tierOf("no_such_account_slot"), undefined);
  });

  it("classifies no name the IDL has stopped declaring", () => {
    const declared = new Set(idl.instructions.flatMap((ix) => ix.accounts.map((a) => a.name)));
    const orphans = Object.keys(ACCOUNT_TIER).filter((n) => !declared.has(n));
    assert.deepEqual(orphans, [], `tier map classifies names no instruction declares: ${orphans}`);
  });

  it("keeps every override live — naming a real slot, and changing its tier", () => {
    const problems: string[] = [];
    for (const key of Object.keys(ACCOUNT_TIER_OVERRIDES)) {
      const [ixName, acctName] = key.split(".");
      const ix = idl.instructions.find((i) => i.name === ixName);
      if (!ix) {
        problems.push(`${key}: no such instruction`);
        continue;
      }
      if (!ix.accounts.some((a) => a.name === acctName)) {
        problems.push(`${key}: ${ixName} declares no account ${acctName}`);
        continue;
      }
      if (ACCOUNT_TIER_OVERRIDES[key] === ACCOUNT_TIER[acctName]) {
        problems.push(`${key}: override equals the name-keyed tier — delete it`);
      }
    }
    assert.deepEqual(problems, [], problems.join("\n"));
  });
});

describe("deposit-common lookup-table bootstrap", () => {
  const common = [
    SYSTEM_PROGRAM,
    TOKEN_PROGRAM,
    ASSOCIATED_TOKEN_PROGRAM,
    TOKEN_METADATA_PROGRAM,
    MPL_CORE_PROGRAM,
    AUTH_RULES_PROGRAM,
    SYSVAR_INSTRUCTIONS,
  ];

  it("contains exactly the fixed accounts used by deposit and deposit_core", () => {
    assert.deepEqual(resolveDepositCommonTier(), common);
  });

  it("keeps the common tier as the full cluster tier's prefix", async () => {
    const cluster = await resolveClusterTier();
    assert.deepEqual(cluster.slice(0, common.length), common);
  });

  it("adds only the full-tier addresses absent from the first run's table", async () => {
    const cluster = await resolveClusterTier();
    assert.deepEqual(missingAddresses(cluster, resolveDepositCommonTier()), cluster.slice(common.length));
  });
});

describe("address lookup tables — signers stay static", () => {
  it("classifies every account the IDL marks a signer as dynamic", () => {
    const tabled: string[] = [];
    for (const ix of idl.instructions) {
      for (const a of ix.accounts) {
        if (!a.signer) continue;
        const tier = tierOf(a.name, ix.name);
        if (tier !== "dynamic") tabled.push(`${ix.name}.${a.name} -> ${tier}`);
      }
    }
    assert.deepEqual(tabled, [], tabled.join("\n"));
  });

  // The seven authorities by name, asserted independently of what the IDL happens to mark — a
  // signer flag dropped from the IDL would pass the test above and fail this one.
  it("keeps the seven named authorities out of every table", () => {
    const authorities = [
      "operator",
      "administrator",
      "depositor",
      "payer",
      "roller",
      "seller",
      "vrf_program_identity",
    ];
    for (const name of authorities) {
      assert.equal(tierOf(name), "dynamic", `${name} must never be tabled`);
    }
  });

  it("puts no signer's address into any table", () => {
    const signerAddresses = new Set<Address>();
    for (const ix of idl.instructions) {
      for (const a of ix.accounts) {
        if (a.signer) signerAddresses.add(addressForSlot(ix.name, a));
      }
    }
    const leaked: string[] = [];
    for (const [table, addrs] of Object.entries(TABLES)) {
      for (const addr of addrs) if (signerAddresses.has(addr)) leaked.push(`${table}: ${addr}`);
    }
    assert.deepEqual(leaked, [], leaked.join("\n"));
  });
});

describe("address lookup tables — serialized size per instruction", () => {
  it("prints the before/after measurement for all 52 instructions", () => {
    // Pinned, not derived: an instruction added without a tier for its new account slots should
    // red this file, and a count read off the same IDL the rows are built from could not.
    assert.equal(measurements.length, 52);
    const lines = measurements.map(
      (m) =>
        `${String(m.accounts).padStart(3)} accts  ${String(m.before).padStart(5)} -> ` +
        `${String(m.after).padStart(5)} bytes  ${m.name}`,
    );
    console.log(`\n${lines.join("\n")}\n`);
  });

  it("brings every instruction inside the transaction size limit", () => {
    const over = measurements.filter((m) => m.after > TRANSACTION_SIZE_LIMIT);
    assert.deepEqual(
      over.map((m) => `${m.name}: ${m.after} bytes`),
      [],
    );
  });

  it("shrinks every instruction that has a tabled account, and never grows one", () => {
    const grew = measurements.filter((m) => m.after > m.before);
    assert.deepEqual(grew.map((m) => m.name), []);
  });

  // The discriminating control. Without it the suite would pass on a tier map that tabled
  // nothing: every instruction would be under the limit except the ones that already were.
  it("self-test: a full-width settle_roll does NOT fit without the table", () => {
    const settleRoll = measurements.find((m) => m.name === "settle_roll");
    assert.ok(settleRoll, "settle_roll not found in the IDL");
    assert.equal(settleRoll.accounts, 40);
    assert.ok(
      settleRoll.before > TRANSACTION_SIZE_LIMIT,
      `expected an oversize uncompressed settle_roll, measured ${settleRoll.before} bytes`,
    );
  });

  it("fits a full-width settle_roll in one v0 transaction over the table", () => {
    const settleRoll = measurements.find((m) => m.name === "settle_roll")!;
    assert.ok(
      settleRoll.after <= TRANSACTION_SIZE_LIMIT,
      `settle_roll is ${settleRoll.after} bytes, limit ${TRANSACTION_SIZE_LIMIT}`,
    );
  });

  // This is the property that makes one table right for every pool: a transaction pays 32 bytes
  // for the table's address and one index byte per key IT resolves, so the other pools' entries
  // cost it nothing. If this ever stopped holding, per-pool tables would start earning their
  // extra reference and the layout would have to be revisited.
  it("charges a transaction nothing for entries it does not name", () => {
    const { instruction, feePayer } = instructionFor(
      idl.instructions.find((ix) => ix.name === "settle_roll")!,
    );
    const [everyPool] = Object.values(TABLES);
    const poolZeroOnly = everyPool.filter((a) => {
      for (let p = 1; p < POOL_COUNT; p++) {
        if (a === syntheticAddress(`pool${p}:pool`)) return false;
        if (a === syntheticAddress(`pool${p}:top_tier`)) return false;
        if (a === syntheticAddress(`pool${p}:weight_index`)) return false;
        if (a === syntheticAddress(`pool${p}:principal_vault`)) return false;
        if (a === syntheticAddress(`pool${p}:t02_identity`)) return false;
        if (a === syntheticAddress(`pool${p}:wallet_stats`)) return false;
      }
      return true;
    });
    assert.ok(
      poolZeroOnly.length < everyPool.length,
      "the wide table holds no extra pools' keys — the comparison is vacuous",
    );
    const wide = measureTransactionSize({ feePayer, instructions: [instruction], lookupTables: TABLES });
    const narrow = measureTransactionSize({
      feePayer,
      instructions: [instruction],
      lookupTables: { [TABLE]: poolZeroOnly },
    });
    assert.equal(wide, narrow, `${everyPool.length}-key table costs more than a ${poolZeroOnly.length}-key one`);
  });
});

describe("Address Lookup Table program codec", () => {
  // bincode, not Borsh: a u32 variant tag and a u64 vector length. Pinned as bytes because the
  // two encodings differ only in width and a Borsh slip would still produce a plausible buffer.
  const lookupTable = syntheticAddress("alt:table");
  const authority = syntheticAddress("alt:authority");
  const payer = syntheticAddress("alt:payer");

  it("writes CreateLookupTable as tag 0, a u64 slot and a bump", () => {
    const ix = buildCreateLookupTableInstruction({
      lookupTable,
      authority,
      payer,
      recentSlot: 0x0102030405060708n,
      bump: 254,
    });
    assert.equal(ix.programAddress, ADDRESS_LOOKUP_TABLE_PROGRAM);
    assert.deepEqual(
      Buffer.from(ix.data!).toString("hex"),
      "00000000" + "0807060504030201" + "fe",
    );
    assert.deepEqual(
      ix.accounts!.map((a) => a.role),
      [
        AccountRole.WRITABLE,
        AccountRole.READONLY_SIGNER,
        AccountRole.WRITABLE_SIGNER,
        AccountRole.READONLY,
      ],
    );
  });

  it("writes ExtendLookupTable as tag 2, a u64 count and the raw keys", () => {
    const addrs = [syntheticAddress("k1"), syntheticAddress("k2")];
    const ix = buildExtendLookupTableInstruction({ lookupTable, authority, payer, addresses: addrs });
    const data = Buffer.from(ix.data!);
    assert.deepEqual(data.subarray(0, 4).toString("hex"), "02000000");
    assert.equal(data.readBigUInt64LE(4), 2n);
    assert.equal(data.length, 4 + 8 + 64);
  });

  it("refuses an extend with no addresses", () => {
    assert.throws(
      () => buildExtendLookupTableInstruction({ lookupTable, authority, payer, addresses: [] }),
      /no addresses/,
    );
  });

  it("writes the two authority-only instructions as a bare tag", () => {
    assert.deepEqual(
      Buffer.from(buildDeactivateLookupTableInstruction({ lookupTable, authority }).data!).toString("hex"),
      "03000000",
    );
    assert.deepEqual(
      Buffer.from(
        buildCloseLookupTableInstruction({ lookupTable, authority, recipient: payer }).data!,
      ).toString("hex"),
      "04000000",
    );
  });

  it("derives a table address from its authority and creation slot", async () => {
    const a = await deriveLookupTableAddress(authority, 100n);
    const b = await deriveLookupTableAddress(authority, 101n);
    const c = await deriveLookupTableAddress(payer, 100n);
    assert.notEqual(a.address, b.address, "a different slot must yield a different table");
    assert.notEqual(a.address, c.address, "a different authority must yield a different table");
    assert.equal((await deriveLookupTableAddress(authority, 100n)).address, a.address);
  });
});

describe("extend chunking", () => {
  it("splits in order and never past the chunk size", () => {
    const addrs = Array.from({ length: 57 }, (_, i) => syntheticAddress(`chunk:${i}`));
    const chunks = chunkAddresses(addrs);
    assert.deepEqual(chunks.map((c) => c.length), [25, 25, 7]);
    assert.deepEqual(chunks.flat(), addrs);
  });

  it("keeps a full extend transaction inside the size limit", () => {
    const authority = syntheticAddress("alt:chunk-authority");
    const addrs = Array.from({ length: EXTEND_CHUNK_SIZE }, (_, i) => syntheticAddress(`x${i}`));
    const ix = buildExtendLookupTableInstruction({
      lookupTable: syntheticAddress("alt:chunk-table"),
      authority,
      payer: authority,
      addresses: addrs,
    });
    const size = measureTransactionSize({ feePayer: authority, instructions: [ix] });
    assert.ok(size <= TRANSACTION_SIZE_LIMIT, `extend of ${EXTEND_CHUNK_SIZE} keys is ${size} bytes`);
  });

  // The control: one more key than the chunk size must overrun, or the chunk size is not the
  // binding constraint and this suite would pass on any value.
  it("self-test: one key past the limit overruns the envelope", () => {
    const authority = syntheticAddress("alt:chunk-authority");
    let n = EXTEND_CHUNK_SIZE;
    let size = 0;
    do {
      n += 1;
      const ix = buildExtendLookupTableInstruction({
        lookupTable: syntheticAddress("alt:chunk-table"),
        authority,
        payer: authority,
        addresses: Array.from({ length: n }, (_, i) => syntheticAddress(`x${i}`)),
      });
      size = measureTransactionSize({ feePayer: authority, instructions: [ix] });
    } while (size <= TRANSACTION_SIZE_LIMIT && n < 64);
    assert.ok(n <= 40, `chunk size ${EXTEND_CHUNK_SIZE} leaves ${n - EXTEND_CHUNK_SIZE} keys of slack`);
  });

  it("holds every tier for every pool well inside one table", () => {
    const [addrs] = Object.values(TABLES);
    assert.ok(
      addrs.length <= MAX_ADDRESSES_PER_TABLE,
      `the table holds ${addrs.length} addresses, cap ${MAX_ADDRESSES_PER_TABLE}`,
    );
    // Not a tight bound — a reminder of the slack, so a future tier that doubled the table would
    // still be nowhere near the cap and this assertion would not be mistaken for one that checks.
    assert.ok(addrs.length < MAX_ADDRESSES_PER_TABLE / 2, `${addrs.length} keys at ${POOL_COUNT} pools`);
  });
});

describe("reading a lookup table back", () => {
  // The bytes are laid out here from the account's documented shape rather than produced by an
  // encoder written next to the decoder: a u32 discriminator, two u64 slots, a u8 start index, a
  // 1-byte Option tag with its 32-byte authority, two padding bytes, then the addresses. An
  // encoder sharing the decoder's idea of those offsets would agree with it while both were wrong.
  function layOutTable(params: {
    deactivationSlot: bigint;
    lastExtendedSlot: bigint;
    authority: Address | null;
    addresses: Address[];
  }): Buffer {
    const meta = Buffer.alloc(LOOKUP_TABLE_META_SIZE);
    meta.writeUInt32LE(1, 0);
    meta.writeBigUInt64LE(params.deactivationSlot, 4);
    meta.writeBigUInt64LE(params.lastExtendedSlot, 12);
    meta.writeUInt8(0, 20);
    meta.writeUInt8(params.authority === null ? 0 : 1, 21);
    if (params.authority !== null) {
      Buffer.from(getAddressEncoder().encode(params.authority)).copy(meta, 22);
    }
    const keys = params.addresses.map((a) => Buffer.from(getAddressEncoder().encode(a)));
    return Buffer.concat([meta, ...keys]);
  }

  const authority = syntheticAddress("decode:authority");
  const keys = [syntheticAddress("decode:a"), syntheticAddress("decode:b")];

  it("reads the slots, the authority and every address", () => {
    const decoded = decodeLookupTable(
      layOutTable({
        deactivationSlot: LOOKUP_TABLE_NOT_DEACTIVATED,
        lastExtendedSlot: 42n,
        authority,
        addresses: keys,
      }),
    );
    assert.equal(decoded.deactivationSlot, LOOKUP_TABLE_NOT_DEACTIVATED);
    assert.equal(decoded.lastExtendedSlot, 42n);
    assert.equal(decoded.authority, authority);
    assert.deepEqual(decoded.addresses, keys);
  });

  it("reports a frozen table as having no authority", () => {
    const decoded = decodeLookupTable(
      layOutTable({
        deactivationSlot: LOOKUP_TABLE_NOT_DEACTIVATED,
        lastExtendedSlot: 42n,
        authority: null,
        addresses: keys,
      }),
    );
    assert.equal(decoded.authority, null);
    assert.deepEqual(decoded.addresses, keys);
  });

  it("reads an empty table as an empty address list, not as a failure", () => {
    const decoded = decodeLookupTable(
      layOutTable({
        deactivationSlot: LOOKUP_TABLE_NOT_DEACTIVATED,
        lastExtendedSlot: 0n,
        authority,
        addresses: [],
      }),
    );
    assert.deepEqual(decoded.addresses, []);
  });

  it("refuses a truncated account rather than reading a short address list", () => {
    const full = layOutTable({
      deactivationSlot: 0n,
      lastExtendedSlot: 0n,
      authority,
      addresses: keys,
    });
    assert.throws(() => decodeLookupTable(full.subarray(0, LOOKUP_TABLE_META_SIZE - 1)), /under the/);
    assert.throws(() => decodeLookupTable(full.subarray(0, full.length - 1)), /multiple of 32/);
  });
});

describe("idempotent extension", () => {
  const a = syntheticAddress("sync:a");
  const b = syntheticAddress("sync:b");
  const c = syntheticAddress("sync:c");

  it("returns nothing to add when the table already holds everything", () => {
    assert.deepEqual(missingAddresses([a, b], [b, a, c]), []);
  });

  it("returns the absentees in the order they were wanted", () => {
    assert.deepEqual(missingAddresses([a, b, c], [b]), [a, c]);
  });

  it("does not ask for the same address twice", () => {
    assert.deepEqual(missingAddresses([a, a, b], []), [a, b]);
  });
});

describe("what a table can never hold", () => {
  // Both of these are properties of the transaction format rather than of this program, and both
  // are premises the tier map is built on — so they are checked against the compiler rather than
  // asserted in a comment. A runtime that compressed either would make the tier map wrong.
  const payer = syntheticAddress("premise:payer");
  const other = syntheticAddress("premise:other");

  function staticKeyCount(lookupTables?: LookupTableSet): number {
    const ix: Instruction = {
      programAddress: PROGRAM,
      accounts: [
        { address: payer, role: AccountRole.WRITABLE_SIGNER },
        { address: other, role: AccountRole.READONLY },
      ],
      data: new Uint8Array(8),
    };
    return measureTransactionSize({ feePayer: payer, instructions: [ix], lookupTables });
  }

  it("leaves the invoked program static even when a table holds its address", () => {
    const table = syntheticAddress("premise:table");
    // The table holds the program id and nothing else. If the program id could be resolved from
    // it, naming the table would save 32 bytes; because it cannot, naming it only costs, and the
    // subset selection drops it — leaving the size identical to having no table at all.
    const withProgramTabled = staticKeyCount({ [table]: [PROGRAM] });
    assert.equal(withProgramTabled, staticKeyCount());
  });

  it("leaves the fee payer static even when a table holds its address", () => {
    const table = syntheticAddress("premise:table2");
    assert.equal(staticKeyCount({ [table]: [payer] }), staticKeyCount());
  });

  // The control: plain readonly accounts in a table must shrink the transaction, or the two
  // assertions above would pass on a compressor that never compresses anything. Three of them,
  // not one — a table that resolves a single key costs three bytes more than it saves, so a
  // one-key control would be indistinguishable from no compression at all.
  it("self-test: plain readonly accounts in a table do shrink it", () => {
    const table = syntheticAddress("premise:table3");
    const extras = [0, 1, 2].map((i) => syntheticAddress(`premise:extra${i}`));
    const ix: Instruction = {
      programAddress: PROGRAM,
      accounts: [
        { address: payer, role: AccountRole.WRITABLE_SIGNER },
        ...extras.map((a) => ({ address: a, role: AccountRole.READONLY })),
      ],
      data: new Uint8Array(8),
    };
    const uncompressed = measureTransactionSize({ feePayer: payer, instructions: [ix] });
    const compressed = measureTransactionSize({
      feePayer: payer,
      instructions: [ix],
      lookupTables: { [table]: extras },
    });
    assert.ok(
      compressed < uncompressed,
      `compression is inert (${uncompressed} -> ${compressed}) — the two premises above prove nothing`,
    );
  });
});
