// D-041 rule-set replay support (T-48): parses the live eBJLFY…zyT9 rule-set account's own
// revision map, and names the 9 synthetic single-revision accounts `scripts/run-integration-
// tests.sh` loads at boot (one per historical revision, at its own address — see that script's
// own comment for how each was built from this same account). Addresses are duplicated here
// rather than read from the shell script, the same convention `RULE_SET_ADDRESS` already follows
// across `fixtures.ts` and `run-integration-tests.sh`.

import { readFileSync } from "node:fs";

/**
 * mpl-token-auth-rules' on-disk layout: a 9-byte header (`u8` version ‖ `u64` LE offset to the
 * revision map), one msgpack-encoded `RuleSetV1` per revision back-to-back, then the map itself
 * (`u8` version ‖ `u32` LE count ‖ count × `u64` LE absolute offsets, one per revision, ascending).
 * Parsed independently of the Python one-off that built the fixture files — this is what the
 * *test* trusts, not a copy of the generator's own arithmetic.
 */
export type RevisionMap = {
  count: number;
  /** Absolute byte offset of each revision's msgpack body, ascending, `offsets.length === count`. */
  offsets: number[];
  /** Absolute byte offset where the map itself begins (also the exclusive end of the last revision). */
  mapStart: number;
};

/**
 * Parses the trailing revision map off a raw rule-set account buffer. Throws rather than
 * returning a zero-length map on anything unexpected — a parser that silently reports "0
 * revisions" on a malformed or truncated buffer would let the replay "pass" having tested
 * nothing (see this file's own unit coverage for the discriminating control).
 */
export function parseRevisionMap(data: Buffer): RevisionMap {
  if (data.length < 9) {
    throw new Error(`parseRevisionMap: buffer too short for even the header (${data.length} bytes)`);
  }
  const headerVersion = data.readUInt8(0);
  if (headerVersion !== 1) {
    throw new Error(`parseRevisionMap: unexpected header version ${headerVersion}, expected 1`);
  }
  const mapStart = Number(data.readBigUInt64LE(1));
  if (mapStart < 9 || mapStart > data.length) {
    throw new Error(`parseRevisionMap: header's map offset ${mapStart} is out of bounds (len ${data.length})`);
  }

  let offset = mapStart;
  const mapVersion = data.readUInt8(offset);
  offset += 1;
  if (mapVersion !== 1) {
    throw new Error(`parseRevisionMap: unexpected revision-map version ${mapVersion}, expected 1`);
  }
  const count = data.readUInt32LE(offset);
  offset += 4;
  if (count === 0) {
    throw new Error("parseRevisionMap: revision map reports 0 revisions — refusing a vacuous replay");
  }

  const offsets: number[] = [];
  for (let i = 0; i < count; i++) {
    offsets.push(Number(data.readBigUInt64LE(offset)));
    offset += 8;
  }
  if (offset !== data.length) {
    throw new Error(
      `parseRevisionMap: ${data.length - offset} trailing byte(s) after the map — layout assumption is wrong`,
    );
  }
  for (let i = 1; i < offsets.length; i++) {
    if (!(offsets[i] > offsets[i - 1])) {
      throw new Error(`parseRevisionMap: offsets not strictly ascending at index ${i}: ${offsets}`);
    }
  }
  if (!(mapStart > offsets[offsets.length - 1])) {
    throw new Error("parseRevisionMap: map start does not follow the last revision's offset");
  }

  return { count, offsets, mapStart };
}

/** Reads and base64-decodes the committed `solana account` dump's own `account.data` field. */
export function readRuleSetAccountData(path: string): Buffer {
  const dump = JSON.parse(readFileSync(path, "utf8")) as {
    account: { data: [string, string] };
  };
  const [b64, encoding] = dump.account.data;
  if (encoding !== "base64") {
    throw new Error(`readRuleSetAccountData: unexpected encoding "${encoding}", expected base64`);
  }
  return Buffer.from(b64, "base64");
}

/**
 * The 9 synthetic per-revision accounts `run-integration-tests.sh` loads, in ascending revision
 * order — index `i` here is revision `i`. Kept in sync with that script's `REVISION_RULE_SETS`
 * by a structural assertion in `rule-set-replay.test.ts`, not by import (the shell script and
 * this file cannot share a source without a build step neither has).
 */
export const REVISION_RULE_SET_ADDRESSES = [
  "y1UDsgq3ff1tXtEjJNQ9Vv9BViK1oRLS8i1U2MMZCkJ",
  "4akQWcyASwc5ucfgMbmE9Ck1s34JcU6eFmjTNbMquz9V",
  "6ztH99b9a7WtBN1Rx6tV2ardn6jAHF3ZAANxM5jBk7sk",
  "7cCzkSMttJWLDVhgWQEUytB3ksMc73YQrkHjHB6sxZmF",
  "Gz6qVdriqDecVqhSZmfmMYsPgRgh8HGgvQ287qmztBPq",
  "85LJQktqJRwiPCD3RutHaBG9kPspnMbf5H9BXQNkZTKb",
  "8yYVLWJJLsmbWG6uxYqvkebSTEPizAUZbRj1aBadim85",
  "F8sPtXnhnrxaa4ghj1YY8MQNS6nNh2ez25fbikMr7AXB",
  "Hd4z5EdEk6UTLZY85ZkW1rRpeFHcfsZ4whbHfqhGrcBt",
] as const;
