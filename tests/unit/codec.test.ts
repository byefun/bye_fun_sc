// Pure codec tests — no validator required, so they run in CI (`npm test`) from day one.
// They pin the Anchor wire conventions every hand-written encoder in scripts/lib depends
// on: discriminator derivation, Borsh field widths, and Option/Vec framing.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { address } from "@solana/kit";
import {
  Borsh,
  accountDiscriminator,
  eventDiscriminator,
  ixDiscriminator,
  optionalAddress,
  seedFromU64Le,
} from "../../scripts/lib/anchor.js";

describe("anchor discriminators", () => {
  it("derives an instruction discriminator as sha256('global:<name>')[0..8]", () => {
    const expected = createHash("sha256")
      .update("global:initialize_config")
      .digest()
      .subarray(0, 8);
    assert.deepEqual(ixDiscriminator("initialize_config"), expected);
    assert.equal(ixDiscriminator("initialize_config").length, 8);
  });

  it("uses distinct namespaces for instructions, accounts, and events", () => {
    assert.notDeepEqual(ixDiscriminator("Config"), accountDiscriminator("Config"));
    assert.notDeepEqual(accountDiscriminator("Config"), eventDiscriminator("Config"));
  });
});

describe("borsh writer", () => {
  it("encodes integers little-endian at their declared widths", () => {
    assert.deepEqual(new Borsh().u8(1).build(), Buffer.from([0x01]));
    assert.deepEqual(new Borsh().u16(0x0201).build(), Buffer.from([0x01, 0x02]));
    assert.deepEqual(new Borsh().u32(0x04030201).build(), Buffer.from([1, 2, 3, 4]));
    assert.deepEqual(
      new Borsh().u64(1n).build(),
      Buffer.from([1, 0, 0, 0, 0, 0, 0, 0]),
    );
    assert.deepEqual(new Borsh().i64(-1n).build(), Buffer.alloc(8, 0xff));
    assert.equal(new Borsh().u128(1n).build().length, 16);
  });

  it("frames Option<T> as 0x00 | 0x01 ‖ T", () => {
    assert.deepEqual(new Borsh().optionU16(null).build(), Buffer.from([0x00]));
    assert.deepEqual(new Borsh().optionU16(7).build(), Buffer.from([0x01, 0x07, 0x00]));
  });

  it("frames Vec<T> and String with a u32 length prefix", () => {
    assert.deepEqual(
      new Borsh().vecU64([1n]).build(),
      Buffer.from([1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]),
    );
    assert.deepEqual(new Borsh().string("bye").build(), Buffer.from([3, 0, 0, 0, 0x62, 0x79, 0x65]));
  });

  it("rejects a fixed-width field of the wrong length", () => {
    assert.throws(() => new Borsh().fixed(Buffer.alloc(31), 32), /expected 32 bytes, got 31/);
  });

  it("frames a fixed-size array with no length prefix, unlike Vec<T>", () => {
    const values = [1n, 2n, 3n];
    assert.deepEqual(
      new Borsh().arrayU64(values, 3).build(),
      Buffer.from([1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0]),
    );
    assert.deepEqual(
      new Borsh().arrayU32([1, 2, 3], 3).build(),
      Buffer.from([1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0]),
    );
    // Same three values as a Vec<u64> carry a u32 length prefix the fixed form must not have.
    const fixed = new Borsh().arrayU64(values, 3).build();
    const vec = new Borsh().vecU64(values).build();
    assert.equal(fixed.length, vec.length - 4);
    assert.deepEqual(vec.subarray(4), fixed);
  });

  it("rejects a fixed-size array of the wrong length", () => {
    assert.throws(() => new Borsh().arrayU64([1n, 2n], 3), /expected 3 items, got 2/);
  });

  it("frames an enum as u8 variant index ‖ payload, pinned at a non-zero index", () => {
    assert.deepEqual(new Borsh().enumVariant(0).build(), Buffer.from([0x00]));
    assert.deepEqual(
      new Borsh().enumVariant(3, new Borsh().u64(9n).build()).build(),
      Buffer.from([0x03, 9, 0, 0, 0, 0, 0, 0, 0]),
    );
  });
});

describe("optional account convention", () => {
  const programId = address("11111111111111111111111111111111");
  const someAddress = address("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

  it("substitutes the program id for a None optional account, never omitting it", () => {
    assert.equal(optionalAddress(null, programId), programId);
  });

  it("passes a Some optional account through unchanged", () => {
    assert.equal(optionalAddress(someAddress, programId), someAddress);
  });
});

describe("pda seed helpers", () => {
  it("encodes a u64 seed little-endian in 8 bytes", () => {
    assert.deepEqual(
      Buffer.from(seedFromU64Le(258n)),
      Buffer.from([2, 1, 0, 0, 0, 0, 0, 0]),
    );
  });
});
