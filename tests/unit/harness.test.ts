// Self-tests for tests/helpers/env.ts's error matcher: checkErrorCode and expectAnchorError.
// No validator needed — these exercise the parsing logic against synthetic thrown objects
// shaped like the real SolanaError / Anchor log formats, not a live transaction.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { address, checkErrorCode, expectAnchorError, expectProgramError } from "../helpers/env.js";

// bye_machine's own deployed id and Token Metadata's — two real, distinct program addresses,
// standing in for "the program under test" and "some other program on the same CPI stack".
const BYE_MACHINE = address("4exRPJN7N8MVZh37efYAkwnGGxoJKMVUARQqf7awmYkJ");
const TOKEN_METADATA = address("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");

function programFailedLog(programId: string, code: number): string {
  return `Program ${programId} failed: custom program error: 0x${code.toString(16)}`;
}

/** A thrown object shaped like the one `solana-kite`'s send path produces on failure: a message,
 * plus the re-fetched `transaction.meta.logMessages` its catch block attaches. */
function txErrorWithLogs(logMessages: string[]): Error {
  const err = new Error("failed to send transaction");
  Object.assign(err, { transaction: { meta: { logMessages } } });
  return err;
}

function structuredError(code: number): unknown {
  return { context: { code } };
}

function wrappedError(code: number): unknown {
  return { cause: { context: { code } } };
}

function anchorLogError(code: number): Error {
  return new Error(`Error Number: ${code}. Error Message: Some message.`);
}

function rpcHexError(code: number): Error {
  return new Error(`custom program error: 0x${code.toString(16)}`);
}

function rpcDecimalError(code: number): Error {
  return new Error(`custom program error: ${code}`);
}

describe("checkErrorCode", () => {
  it("matches a structured SolanaError.context.code", () => {
    assert.equal(checkErrorCode(structuredError(6501), 6501), true);
  });

  it("matches a wrapped SolanaError.cause.context.code", () => {
    assert.equal(checkErrorCode(wrappedError(6501), 6501), true);
  });

  it("matches an Anchor 'Error Number:' log line", () => {
    assert.equal(checkErrorCode(anchorLogError(6501), 6501), true);
  });

  it("matches an RPC 'custom program error: 0x..' line", () => {
    assert.equal(checkErrorCode(rpcHexError(6501), 6501), true);
  });

  it("matches an RPC 'custom program error: <decimal>' line", () => {
    assert.equal(checkErrorCode(rpcDecimalError(6501), 6501), true);
  });

  it("does not match a different code", () => {
    assert.equal(checkErrorCode(structuredError(6501), 6502), false);
  });

  it("does not match a thrown value carrying no code at all", () => {
    assert.equal(checkErrorCode(new Error("transaction failed to build"), 6501), false);
  });
});

describe("expectAnchorError", () => {
  it("resolves when the promise rejects with the expected code", async () => {
    await expectAnchorError(Promise.reject(anchorLogError(6501)), 6501);
  });

  it("rejects when the promise resolves instead of rejecting", async () => {
    await assert.rejects(expectAnchorError(Promise.resolve("ok"), 6501), /promise resolved/);
  });

  // The one requirement this whole helper exists for: a wrong-code rejection must not read as
  // a pass. A helper that only ever passes when things are right is untested.
  it("rejects, naming the observed code, when the promise rejects with a different code", async () => {
    await assert.rejects(
      expectAnchorError(Promise.reject(anchorLogError(6502)), 6501),
      /observed 6502/,
    );
  });

  it("rejects, naming the absence of a code, when the thrown value carries none", async () => {
    await assert.rejects(
      expectAnchorError(Promise.reject(new Error("transaction failed to build")), 6501),
      /carried no numeric code/,
    );
  });
});

describe("expectProgramError", () => {
  it("resolves when the code is emitted by the expected program", async () => {
    const err = txErrorWithLogs([
      `Program ${BYE_MACHINE} invoke [1]`,
      programFailedLog(BYE_MACHINE, 6501),
    ]);
    await expectProgramError(Promise.reject(err), BYE_MACHINE, 6501);
  });

  it("rejects when the promise resolves instead of rejecting", async () => {
    await assert.rejects(
      expectProgramError(Promise.resolve("ok"), BYE_MACHINE, 6501),
      /promise resolved/,
    );
  });

  // The mandatory case this helper exists for: the same numeric code, emitted by a program
  // other than the one under test, must not read as a match — without this case the helper adds
  // nothing over expectAnchorError's code-only match.
  it("rejects when the code was emitted by a different program", async () => {
    const err = txErrorWithLogs([
      `Program ${BYE_MACHINE} invoke [1]`,
      `Program ${TOKEN_METADATA} invoke [2]`,
      programFailedLog(TOKEN_METADATA, 57),
    ]);
    await assert.rejects(
      expectProgramError(Promise.reject(err), BYE_MACHINE, 57),
      /was emitted by/,
    );
  });

  it("rejects when no log line reports the expected code from any program", async () => {
    const err = txErrorWithLogs([
      `Program ${BYE_MACHINE} invoke [1]`,
      programFailedLog(BYE_MACHINE, 6502),
    ]);
    await assert.rejects(
      expectProgramError(Promise.reject(err), BYE_MACHINE, 6501),
      /no log line reported/,
    );
  });

  it("rejects when the thrown value carries no transaction logs", async () => {
    await assert.rejects(
      expectProgramError(Promise.reject(new Error("transaction failed to build")), BYE_MACHINE, 6501),
      /carried no transaction logs/,
    );
  });
});
