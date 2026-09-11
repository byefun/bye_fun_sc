// Minimal Anchor instruction codec + PDA helpers for @solana/kit.
//
// solana-kite / @solana/kit ship no Anchor IDL client, so instructions are
// hand-encoded here. Anchor instruction data layout:
//   [ 8-byte discriminator ][ borsh-serialized args in IDL order ]
// Account metas follow the IDL `accounts` order; remaining_accounts are appended.
//
// This file is program-agnostic — it is the encoder foundation the per-instruction
// builders (scripts/lib/<domain>.ts) will be written on once the program lands.

import { createHash } from "node:crypto";
import {
  AccountRole,
  getAddressEncoder,
  getProgramDerivedAddress,
  type Address,
  type Instruction,
  type AccountMeta,
} from "@solana/kit";
import { PROGRAM_ID } from "../constants.js";

export { AccountRole };

const addrEnc = getAddressEncoder();

// ── Borsh writer ─────────────────────────────────────────────────────────────
export class Borsh {
  private chunks: Buffer[] = [];
  u8(n: number) { const b = Buffer.alloc(1); b.writeUInt8(n); this.chunks.push(b); return this; }
  u16(n: number) { const b = Buffer.alloc(2); b.writeUInt16LE(n); this.chunks.push(b); return this; }
  u32(n: number) { const b = Buffer.alloc(4); b.writeUInt32LE(n >>> 0); this.chunks.push(b); return this; }
  u64(n: bigint) { const b = Buffer.alloc(8); b.writeBigUInt64LE(n); this.chunks.push(b); return this; }
  i64(n: bigint) { const b = Buffer.alloc(8); b.writeBigInt64LE(n); this.chunks.push(b); return this; }
  u128(n: bigint) {
    const b = Buffer.alloc(16);
    b.writeBigUInt64LE(n & 0xffffffffffffffffn, 0);
    b.writeBigUInt64LE(n >> 64n, 8);
    this.chunks.push(b);
    return this;
  }
  bool(v: boolean) { return this.u8(v ? 1 : 0); }
  bytes(b: Buffer | Uint8Array) { this.chunks.push(Buffer.from(b)); return this; }
  fixed(b: Buffer | Uint8Array, len: number) {
    const buf = Buffer.from(b);
    if (buf.length !== len) throw new Error(`expected ${len} bytes, got ${buf.length}`);
    this.chunks.push(buf);
    return this;
  }
  /** Borsh `String`: u32 byte-length prefix ‖ UTF-8 bytes. */
  string(s: string) {
    const buf = Buffer.from(s, "utf8");
    return this.u32(buf.length).bytes(buf);
  }
  pubkey(a: Address) { this.chunks.push(Buffer.from(addrEnc.encode(a))); return this; }
  // Option<T>: 0x00 = None, 0x01 ‖ T = Some.
  optionU8(n: number | null) { return n === null ? this.u8(0) : this.u8(1).u8(n); }
  optionU16(n: number | null) { return n === null ? this.u8(0) : this.u8(1).u16(n); }
  optionU32(n: number | null) { return n === null ? this.u8(0) : this.u8(1).u32(n); }
  optionU64(n: bigint | null) { return n === null ? this.u8(0) : this.u8(1).u64(n); }
  optionI64(n: bigint | null) { return n === null ? this.u8(0) : this.u8(1).i64(n); }
  optionPubkey(a: Address | null) { return a === null ? this.u8(0) : this.u8(1).pubkey(a); }
  // Vec<T>: u32 length prefix ‖ elements.
  vecU64(items: bigint[]) {
    this.u32(items.length);
    for (const it of items) this.u64(it);
    return this;
  }
  vecPubkey(items: Address[]) {
    this.u32(items.length);
    for (const it of items) this.pubkey(it);
    return this;
  }
  // Fixed-size array `[T; N]`: elements only, no length prefix (unlike Vec<T>).
  arrayU64(items: bigint[], len: number) {
    if (items.length !== len) throw new Error(`expected ${len} items, got ${items.length}`);
    for (const it of items) this.u64(it);
    return this;
  }
  arrayU32(items: number[], len: number) {
    if (items.length !== len) throw new Error(`expected ${len} items, got ${items.length}`);
    for (const it of items) this.u32(it);
    return this;
  }
  // Enum: u8 variant index (declaration order) ‖ variant payload, if any.
  enumVariant(index: number, payload?: Buffer | Uint8Array) {
    this.u8(index);
    if (payload) this.bytes(payload);
    return this;
  }
  build(): Buffer { return Buffer.concat(this.chunks); }
}

// ── Discriminators ───────────────────────────────────────────────────────────
/** Anchor instruction discriminator: first 8 bytes of sha256("global:<snake_case_name>"). */
export function ixDiscriminator(instructionName: string): Buffer {
  return createHash("sha256").update(`global:${instructionName}`).digest().subarray(0, 8);
}

/** Anchor account discriminator: first 8 bytes of sha256("account:<PascalCaseName>"). */
export function accountDiscriminator(accountName: string): Buffer {
  return createHash("sha256").update(`account:${accountName}`).digest().subarray(0, 8);
}

/** Anchor event discriminator: first 8 bytes of sha256("event:<PascalCaseName>"). */
export function eventDiscriminator(eventName: string): Buffer {
  return createHash("sha256").update(`event:${eventName}`).digest().subarray(0, 8);
}

// ── PDA derivation ───────────────────────────────────────────────────────────
export type Seed = Buffer | Uint8Array | string;

function toSeedBytes(seed: Seed): Uint8Array {
  return typeof seed === "string" ? new Uint8Array(Buffer.from(seed, "utf8")) : new Uint8Array(seed);
}

/** Derive a PDA of this program (or of `programId` when given). */
export async function derivePda(
  seeds: Seed[],
  programId: Address = PROGRAM_ID,
): Promise<{ address: Address; bump: number }> {
  const [pda, bump] = await getProgramDerivedAddress({
    programAddress: programId,
    seeds: seeds.map(toSeedBytes),
  });
  return { address: pda, bump };
}

export function seedFromPubkey(a: Address): Uint8Array {
  return new Uint8Array(addrEnc.encode(a));
}

export function seedFromU64Le(n: bigint): Uint8Array {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(n);
  return new Uint8Array(b);
}

// ── Instruction builder ──────────────────────────────────────────────────────
/**
 * Assemble an Anchor instruction from a name, borsh-encoded args, and ordered metas.
 * `remaining` metas are appended after the named accounts, matching Anchor's
 * `ctx.remaining_accounts`.
 */
export function buildAnchorIx(params: {
  name: string;
  args?: Buffer;
  accounts: AccountMeta[];
  remaining?: AccountMeta[];
  programId?: Address;
}): Instruction {
  const { name, args, accounts, remaining = [], programId = PROGRAM_ID } = params;
  const data = args ? Buffer.concat([ixDiscriminator(name), args]) : ixDiscriminator(name);
  return {
    programAddress: programId,
    accounts: [...accounts, ...remaining],
    data: new Uint8Array(data),
  };
}

// Meta helpers — terser than spelling out AccountRole at every call site.
export const readonly = (a: Address): AccountMeta => ({ address: a, role: AccountRole.READONLY });
export const writable = (a: Address): AccountMeta => ({ address: a, role: AccountRole.WRITABLE });
export const signer = (a: Address): AccountMeta => ({ address: a, role: AccountRole.READONLY_SIGNER });
export const writableSigner = (a: Address): AccountMeta => ({
  address: a,
  role: AccountRole.WRITABLE_SIGNER,
});

/**
 * Anchor's `Option<Account>` convention: a `None` optional account is passed
 * as the program's own id, never omitted — omitting one shifts every later
 * account by one and surfaces as an unrelated constraint error. Wrap the
 * result in `readonly`/`writable`/etc. for the role the slot needs.
 */
export function optionalAddress(address: Address | null, programId: Address = PROGRAM_ID): Address {
  return address ?? programId;
}
