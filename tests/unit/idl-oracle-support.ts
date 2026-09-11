// Shared engine for T-50's differential oracle: parses target/idl/bye_machine.json at test run
// time and encodes instruction args / account bodies / event bodies generically from its
// declared types, so the expected bytes a test compares against are derived mechanically from
// the IDL rather than hand-transcribed by whoever also wrote the encoder or decoder under test.
//
// Byte primitives are written independently of scripts/lib/anchor.ts's `Borsh` (bit-shifted, not
// `Buffer.write*LE`), the same hedge tests/unit/decoders.test.ts already takes for its golden
// fixtures. What this file does NOT reimplement independently is little-endian arithmetic itself
// — there is one correct way to write a u64 LE — the property this oracle exists to check is
// field ORDER and TYPE per instruction/account/event, which come from parsing the IDL, never
// from a list this file's own author transcribed by hand.
//
// Scope, stated once rather than left implicit: the IDL is generated from the same Rust as the
// encoders/decoders it is compared against, so every check in this file proves client↔program
// agreement, never program↔spec correctness. And `idl.address` is the literal string
// "PROGRAMID" — `declare_id!` takes a const, so Anchor's IDL generator emits the identifier's
// *name*, not a resolved value — nothing here reads `idl.address` as a program id.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { AccountRole, address, getAddressEncoder, type Address } from "@solana/kit";

const addrEnc = getAddressEncoder();

// ── IDL shape (only the parts this oracle reads) ─────────────────────────────────────────────

export type IdlTypeRef =
  | "u8"
  | "u16"
  | "u32"
  | "u64"
  | "u128"
  | "i64"
  | "bool"
  | "string"
  | "pubkey"
  | { array: [IdlTypeRef, number] }
  | { vec: IdlTypeRef }
  | { option: IdlTypeRef }
  | { defined: { name: string } };

export type IdlField = { name: string; type: IdlTypeRef };
export type IdlEnumVariant = { name: string; fields?: IdlTypeRef[] | IdlField[] };
export type IdlDefinedType = {
  name: string;
  type: { kind: "struct"; fields: IdlField[] } | { kind: "enum"; variants: IdlEnumVariant[] };
};

export type IdlAccountItem = {
  name: string;
  writable?: boolean;
  signer?: boolean;
  optional?: boolean;
  address?: string;
};

export type IdlInstruction = {
  name: string;
  discriminator: number[];
  accounts: IdlAccountItem[];
  args: IdlField[];
};

export type IdlNamedDiscriminator = { name: string; discriminator: number[] };

export type Idl = {
  address: string;
  instructions: IdlInstruction[];
  accounts: IdlNamedDiscriminator[];
  events: IdlNamedDiscriminator[];
  types: IdlDefinedType[];
};

const IDL_PATH = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../../target/idl/bye_machine.json",
);

let cached: Idl | null = null;
/** Reads and parses `target/idl/bye_machine.json` once per test run; cached across `it`s. */
export function loadIdl(): Idl {
  if (!cached) cached = JSON.parse(readFileSync(IDL_PATH, "utf8")) as Idl;
  return cached;
}

export function findInstruction(idl: Idl, name: string): IdlInstruction {
  const ix = idl.instructions.find((i) => i.name === name);
  if (!ix) throw new Error(`no such instruction in IDL: ${name}`);
  return ix;
}

export function resolveDefined(idl: Idl, name: string): IdlDefinedType {
  const def = idl.types.find((t) => t.name === name);
  if (!def) throw new Error(`no such defined type in IDL: ${name}`);
  return def;
}

/** "observed_at" -> "observedAt" — the exact convention scripts/lib and decoders.ts already use
 * for every IDL snake_case field name, so a fixture built with a domain type's own field names
 * can be fed to this file's encoders unchanged. */
export function snakeToCamel(s: string): string {
  return s.replace(/_([a-z0-9])/g, (_m, c: string) => c.toUpperCase());
}

// ── Byte primitives — bit-shifted, independent of Buffer's write*LE methods ─────────────────

function leU8(n: number): number[] {
  return [n & 0xff];
}
function leU16(n: number): number[] {
  return [n & 0xff, (n >> 8) & 0xff];
}
function leU32(n: number): number[] {
  return [n & 0xff, (n >>> 8) & 0xff, (n >>> 16) & 0xff, (n >>> 24) & 0xff];
}
function leU64(n: bigint): number[] {
  let x = n < 0n ? n + (1n << 64n) : n;
  const out: number[] = [];
  for (let i = 0; i < 8; i++) {
    out.push(Number(x & 0xffn));
    x >>= 8n;
  }
  return out;
}
const leI64 = leU64;
function leU128(n: bigint): number[] {
  let x = n < 0n ? n + (1n << 128n) : n;
  const out: number[] = [];
  for (let i = 0; i < 16; i++) {
    out.push(Number(x & 0xffn));
    x >>= 8n;
  }
  return out;
}
function boolByte(v: boolean): number[] {
  return [v ? 1 : 0];
}
function strBytes(s: string): number[] {
  const utf8 = Array.from(Buffer.from(s, "utf8"));
  return [...leU32(utf8.length), ...utf8];
}

function isNamedFields(fields: IdlTypeRef[] | IdlField[]): fields is IdlField[] {
  const first = fields[0] as unknown;
  return typeof first === "object" && first !== null && "name" in (first as object);
}

// ── Generic recursive encoder ────────────────────────────────────────────────────────────────

function encodeValue(idl: Idl, type: IdlTypeRef, value: unknown, out: number[]): void {
  if (type === "u8") return void out.push(...leU8(value as number));
  if (type === "u16") return void out.push(...leU16(value as number));
  if (type === "u32") return void out.push(...leU32(value as number));
  if (type === "u64") return void out.push(...leU64(value as bigint));
  if (type === "u128") return void out.push(...leU128(value as bigint));
  if (type === "i64") return void out.push(...leI64(value as bigint));
  if (type === "bool") return void out.push(...boolByte(value as boolean));
  if (type === "string") return void out.push(...strBytes(value as string));
  if (type === "pubkey") return void out.push(...Array.from(addrEnc.encode(value as Address)));

  if ("array" in type) {
    const [elemType, len] = type.array;
    const arr = value as unknown[];
    if (arr.length !== len) {
      throw new Error(`array: expected ${len} element(s), got ${arr.length}`);
    }
    for (const el of arr) encodeValue(idl, elemType, el, out);
    return;
  }
  if ("vec" in type) {
    const arr = value as unknown[];
    out.push(...leU32(arr.length));
    for (const el of arr) encodeValue(idl, type.vec, el, out);
    return;
  }
  if ("option" in type) {
    if (value === null || value === undefined) return void out.push(0);
    out.push(1);
    encodeValue(idl, type.option, value, out);
    return;
  }
  if ("defined" in type) {
    const def = resolveDefined(idl, type.defined.name);
    if (def.type.kind === "struct") return encodeStruct(idl, def.type.fields, value, out);
    return encodeEnum(idl, def.type.variants, value, out);
  }
  throw new Error(`unsupported IDL type: ${JSON.stringify(type)}`);
}

function encodeStruct(idl: Idl, fields: IdlField[], value: unknown, out: number[]): void {
  const obj = value as Record<string, unknown>;
  for (const f of fields) {
    const key = snakeToCamel(f.name);
    if (!(key in obj)) {
      throw new Error(`missing field '${f.name}' (as '${key}') in fixture`);
    }
    encodeValue(idl, f.type, obj[key], out);
  }
}

/**
 * Enum value convention, matching scripts/lib/admin.ts and scripts/lib/decoders.ts exactly, so a
 * domain type's own value is a valid fixture here unchanged: a fieldless variant is a bare
 * string (its name); a variant with fields is `{ kind: "<name>", ...payload }`, where a
 * single-field tuple variant's payload key is `value` (`ConfigValue`'s convention) and a
 * struct-style variant's payload keys are its field names, camelCased.
 */
function encodeEnum(idl: Idl, variants: IdlEnumVariant[], value: unknown, out: number[]): void {
  const variantName = typeof value === "string" ? value : (value as { kind: string }).kind;
  const index = variants.findIndex((v) => v.name === variantName);
  if (index < 0) throw new Error(`unknown enum variant: ${String(variantName)}`);
  out.push(index);

  const fields = variants[index].fields;
  if (!fields || fields.length === 0) return;
  const payload = value as Record<string, unknown>;

  if (isNamedFields(fields)) {
    for (const f of fields) {
      const key = snakeToCamel(f.name);
      if (!(key in payload)) {
        throw new Error(`enum ${variantName}: missing field '${f.name}' (as '${key}')`);
      }
      encodeValue(idl, f.type, payload[key], out);
    }
    return;
  }
  if (fields.length === 1) {
    if (!("value" in payload)) throw new Error(`enum ${variantName}: missing 'value'`);
    encodeValue(idl, fields[0], payload.value, out);
    return;
  }
  throw new Error(
    `enum ${variantName}: unsupported multi-field tuple variant (${fields.length} fields) — ` +
      `not present in this IDL as of writing`,
  );
}

// ── Entry points ─────────────────────────────────────────────────────────────────────────────

/**
 * Encodes an instruction's args generically from the IDL's declared field list and types.
 * `value` may carry extra properties beyond the instruction's own args (e.g. the same params
 * object a domain builder call takes, which also carries account-identifying fields) — only the
 * IDL-declared arg names (camelCased) are read.
 */
export function encodeInstructionArgs(idl: Idl, instructionName: string, value: unknown): Buffer {
  const ix = findInstruction(idl, instructionName);
  const out: number[] = [];
  encodeStruct(idl, ix.args, value, out);
  return Buffer.from(out);
}

/** Encodes an account's or event's body (post-discriminator bytes) from its `types` entry. */
export function encodeStateBody(idl: Idl, typeName: string, value: unknown): Buffer {
  const def = resolveDefined(idl, typeName);
  if (def.type.kind !== "struct") throw new Error(`${typeName} is not a struct type`);
  const out: number[] = [];
  encodeStruct(idl, def.type.fields, value, out);
  return Buffer.from(out);
}

/** Byte width of a fixed-size IDL type — `pubkey`/integers/bools and fixed arrays of those.
 * Throws for a variable-size type (`string`, `vec`, `option`, `defined`); used only for
 * `WeightIndex`, whose `bytemuck`/`repr(C)` layout is read at pinned offsets rather than with a
 * sequential borsh cursor, so its offsets are computed here from the IDL's own field list rather
 * than duplicating `decoders.ts`'s hardcoded constants. */
export function idlFixedByteSize(type: IdlTypeRef): number {
  if (type === "u8" || type === "bool") return 1;
  if (type === "u16") return 2;
  if (type === "u32") return 4;
  if (type === "u64" || type === "i64") return 8;
  if (type === "u128") return 16;
  if (type === "pubkey") return 32;
  if (typeof type === "object" && "array" in type) {
    const [elem, len] = type.array;
    return idlFixedByteSize(elem) * len;
  }
  throw new Error(`idlFixedByteSize: not a fixed-size type: ${JSON.stringify(type)}`);
}

// ── Instruction account-list helpers ────────────────────────────────────────────────────────

export type IdlAccountRole = { name: string; writable: boolean; signer: boolean };

/** The account list's declared order, plus each slot's writable/signer flags — read straight
 * off the IDL, never hand-copied. */
export function idlAccountRoles(idl: Idl, instructionName: string): IdlAccountRole[] {
  return findInstruction(idl, instructionName).accounts.map((a) => ({
    name: a.name,
    writable: a.writable === true,
    signer: a.signer === true,
  }));
}

export function roleFromFlags(writable: boolean, signer: boolean): AccountRole {
  if (writable && signer) return AccountRole.WRITABLE_SIGNER;
  if (writable) return AccountRole.WRITABLE;
  if (signer) return AccountRole.READONLY_SIGNER;
  return AccountRole.READONLY;
}

/** The literal address IDL pins for a constant-program account slot (e.g. `token_program`), or
 * `undefined` when the slot has no IDL-declared literal. */
export function idlAccountAddress(
  idl: Idl,
  instructionName: string,
  accountName: string,
): Address | undefined {
  const entry = findInstruction(idl, instructionName).accounts.find((a) => a.name === accountName);
  return entry?.address ? address(entry.address) : undefined;
}

/**
 * Builds the expected ordered address list for an instruction from the IDL's own account order:
 * each name resolves either from `resolved` (a per-test map of markers/PDAs the caller supplies)
 * or, failing that, from the IDL's own literal `address` field. Throws if a slot has neither —
 * a silent gap here would read as a passing test that checked nothing for that slot.
 */
export function expectedAccountAddresses(
  idl: Idl,
  instructionName: string,
  resolved: Record<string, Address>,
): Address[] {
  return idlAccountRoles(idl, instructionName).map(({ name }) => {
    if (name in resolved) return resolved[name];
    const literal = idlAccountAddress(idl, instructionName, name);
    if (literal) return literal;
    throw new Error(`no resolution for account '${name}' of instruction '${instructionName}'`);
  });
}
