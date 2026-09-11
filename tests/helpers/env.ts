// Shared test environment: localnet connection, the committed suite admin, funded
// throwaway wallets, and an Anchor error-code matcher.

import { connect, loadWalletFromFile } from "solana-kite";
import { address, lamports } from "@solana/kit";
import type { Address, TransactionSigner } from "@solana/kit";

export { address, lamports };
export type { Address, TransactionSigner };
import { PROGRAM_ID, env } from "../../scripts/constants.js";
export { PROGRAM_ID, env };

export type Connection = ReturnType<typeof connect>;

export function getConnection(): Connection {
  return connect("localnet");
}

/**
 * The one shared admin every integration test signs config/admin ops with.
 * Program config is expected to be an init-once singleton whose `admin` binds on first
 * init, so the suite must use a single deterministic key — hence the committed
 * throwaway `accounts/dev/deployer.json`. `scripts/run-integration-tests.sh` airdrops it.
 */
export async function loadSuiteAdmin(): Promise<TransactionSigner> {
  return loadWalletFromFile("accounts/dev/deployer.json");
}

/**
 * The suite's committed `operator` authority — distinct from `loadSuiteAdmin`'s administrator key
 * on every validator run, so the six Operator-gated instructions have a genuinely different
 * privileged wallet to test the wrong-signer matrix against. `scripts/run-integration-tests.sh`
 * airdrops it beside `DEPLOYER_ADDR`.
 */
export async function loadOperator(): Promise<TransactionSigner> {
  return loadWalletFromFile("accounts/dev/operator.json");
}

/** The suite's committed `t03_authority` — gates nothing in slice 1, kept as the "privileged but
 * wrong" wrong-signer case for handlers it does not authorize. */
export async function loadT03Authority(): Promise<TransactionSigner> {
  return loadWalletFromFile("accounts/dev/t03.json");
}

/** The suite's committed `t02_authority` — same role as `loadT03Authority`. */
export async function loadT02Authority(): Promise<TransactionSigner> {
  return loadWalletFromFile("accounts/dev/t02.json");
}

/** Signs nothing legitimate anywhere in the suite; exists only to be rejected by the
 * wrong-signer matrix's negative cases. */
export async function loadUnprivileged(): Promise<TransactionSigner> {
  return loadWalletFromFile("accounts/dev/unprivileged.json");
}

/** Create a fresh funded wallet for a test that needs an independent signer. */
export async function fundedWallet(
  connection: Connection,
  sol = 10,
): Promise<TransactionSigner> {
  const [wallet] = await connection.createWallets(1, { airdropAmount: lamports(BigInt(sol) * 1_000_000_000n) });
  return wallet;
}

/**
 * The suite's named wallets for the authority/signer matrix (TA-7). `t03Authority`/
 * `t02Authority` gate no instruction in slice 1 — they only live in `ProtocolConfig`, written at
 * `init_protocol` and rotated by `set_authorities` — but stay in the matrix as the best
 * "privileged but wrong" wrong-signer cases. `unprivileged` signs nothing legitimate anywhere
 * else; it exists only to be rejected by TA-7's negative cases.
 */
export type SuiteWallets = {
  operator: TransactionSigner;
  t03Authority: TransactionSigner;
  t02Authority: TransactionSigner;
  depositors: [TransactionSigner, TransactionSigner, TransactionSigner];
  unprivileged: TransactionSigner;
};

/** Funds and returns every named wallet the integration suite signs with, besides the admin. */
export async function loadSuiteWallets(connection: Connection, sol = 10): Promise<SuiteWallets> {
  const airdropAmount = lamports(BigInt(sol) * 1_000_000_000n);
  const [operator, t03Authority, t02Authority, depositorA, depositorB, depositorC, unprivileged] =
    await connection.createWallets(7, { airdropAmount });
  return {
    operator,
    t03Authority,
    t02Authority,
    depositors: [depositorA, depositorB, depositorC],
    unprivileged,
  };
}

/**
 * Asserts `PROGRAM_ID` is a live, executable account on the connected cluster, before any test
 * runs. `constants.ts` has no localnet branch — `PROGRAM_ID` comes from `CLUSTER`, defaulting to
 * `devnet`, which happens to hold the right address by coincidence. Calling this first turns a
 * misconfigured cluster into one error naming the mismatch, instead of a wall of unrelated
 * PDA/account-not-found errors from every test that follows.
 */
export async function assertProgramIsLive(connection: Connection): Promise<void> {
  const { value: accountInfo } = await connection.rpc
    .getAccountInfo(PROGRAM_ID, { encoding: "base64" })
    .send();
  if (accountInfo === null) {
    throw new Error(
      `assertProgramIsLive: ${PROGRAM_ID} does not exist on cluster "${env.cluster}" — ` +
        `wrong CLUSTER / RPC_URL?`,
    );
  }
  if (!accountInfo.executable) {
    throw new Error(
      `assertProgramIsLive: ${PROGRAM_ID} exists on cluster "${env.cluster}" but is not executable`,
    );
  }
}

const ERROR_NUMBER_RE = /Error Number: (\d+)/;
const CUSTOM_HEX_RE = /custom program error: 0x([0-9a-fA-F]+)/;
const CUSTOM_DEC_RE = /custom program error: (\d+)/;

/**
 * Pulls the on-chain custom error code out of a thrown object, trying (in order):
 *   1. SolanaError.context.code — structured on-chain custom code
 *   2. SolanaError.cause.context.code — wrapped preflight / confirm errors
 *   3. Anchor canonical on-chain formats:
 *        "Error Number: <decimal>"         (Anchor program-error log line)
 *        "custom program error: 0x<hex>"   (RPC simulation / preflight log)
 *        "custom program error: <decimal>" (alternate RPC format)
 * Returns `null` when none of the three match — the thrown object carries no numeric code at
 * all, e.g. a transaction that failed to build.
 */
function extractErrorCode(thrownObject: unknown): number | null {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const errAny = thrownObject as any;
  if (typeof errAny?.context?.code === "number") return errAny.context.code;
  if (typeof errAny?.cause?.context?.code === "number") return errAny.cause.context.code;
  const errorString = String(thrownObject);
  const hexMatch = errorString.match(CUSTOM_HEX_RE);
  if (hexMatch) return parseInt(hexMatch[1], 16);
  const decMatch = errorString.match(ERROR_NUMBER_RE) ?? errorString.match(CUSTOM_DEC_RE);
  if (decMatch) return parseInt(decMatch[1], 10);
  return null;
}

/** Generic error-code matcher for use inside assert.rejects. See `extractErrorCode` above. */
export function checkErrorCode(thrownObject: unknown, code: number): boolean {
  return extractErrorCode(thrownObject) === code;
}

/**
 * Asserts `promise` rejects with Anchor error code `code`, specifically. `assert.rejects(p)`
 * with no predicate passes on *any* rejection — including a transaction that failed to build —
 * and `checkErrorCode` used the same way inside `assert.rejects(p, () => true)` shares that
 * blind spot. On a mismatch this throws naming the *observed* code, never a bare assertion
 * failure, so a misdiagnosed test failure doesn't have to be re-run under a debugger to find out
 * what actually happened.
 */
export async function expectAnchorError(promise: Promise<unknown>, code: number): Promise<void> {
  let thrown: unknown;
  let resolved = false;
  try {
    await promise;
    resolved = true;
  } catch (err) {
    thrown = err;
  }
  if (resolved) {
    throw new Error(`expectAnchorError: expected Anchor error ${code}, but the promise resolved`);
  }
  const observed = extractErrorCode(thrown);
  if (observed === code) return;
  throw new Error(
    observed === null
      ? `expectAnchorError: expected Anchor error ${code}, but the thrown value carried no ` +
        `numeric code: ${String(thrown)}`
      : `expectAnchorError: expected Anchor error ${code}, observed ${observed}`,
  );
}

const PROGRAM_FAILED_RE = /^Program (\S+) failed: custom program error: (0x[0-9a-fA-F]+|\d+)$/;

/**
 * `solana-kite`'s send path re-fetches and attaches the confirmed transaction on every failure
 * (`sendTransactionFromInstructions`'s catch block sets `error.transaction`), so
 * `transaction.meta.logMessages` is present on every on-chain rejection this suite produces.
 */
function extractProgramLogs(thrownObject: unknown): string[] | null {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const logs = (thrownObject as any)?.transaction?.meta?.logMessages;
  return Array.isArray(logs) ? (logs as string[]) : null;
}

/**
 * Finds the program whose own `Program <id> failed: custom program error: …` log line carries
 * `code` — the format is identical for every program on the CPI stack, so this is what
 * discriminates `bye_machine`'s codes from an identically-numbered one a foreign program (Token
 * Metadata, Token Auth Rules) returns through the same CPI. Returns `null` when no log line
 * carries this exact code, from any program.
 */
function findEmittingProgram(logMessages: string[], code: number): string | null {
  for (const line of logMessages) {
    const match = line.match(PROGRAM_FAILED_RE);
    if (!match) continue;
    const raw = match[2];
    const observed = raw.startsWith("0x") ? parseInt(raw, 16) : parseInt(raw, 10);
    if (observed === code) return match[1];
  }
  return null;
}

/**
 * Asserts `promise` rejects with error `code`, emitted specifically by `programId`.
 * `expectAnchorError`/`extractErrorCode` discard the emitting program at every extraction
 * branch, which is incidental against `bye_machine` alone but structural once a CPI stack can
 * return the same small code from more than one program — this reads the program id off the
 * failing transaction's own logs instead.
 */
export async function expectProgramError(
  promise: Promise<unknown>,
  programId: Address,
  code: number,
): Promise<void> {
  let thrown: unknown;
  let resolved = false;
  try {
    await promise;
    resolved = true;
  } catch (err) {
    thrown = err;
  }
  if (resolved) {
    throw new Error(
      `expectProgramError: expected ${programId} error ${code}, but the promise resolved`,
    );
  }
  const logMessages = extractProgramLogs(thrown);
  if (logMessages === null) {
    throw new Error(
      `expectProgramError: expected ${programId} error ${code}, but the thrown value carried no ` +
        `transaction logs to attribute it to an emitting program: ${String(thrown)}`,
    );
  }
  const emittingProgram = findEmittingProgram(logMessages, code);
  if (emittingProgram === null) {
    throw new Error(
      `expectProgramError: expected ${programId} error ${code}, but no log line reported that ` +
        `code from any program`,
    );
  }
  if (emittingProgram !== programId) {
    throw new Error(
      `expectProgramError: error ${code} was emitted by ${emittingProgram}, not the expected ${programId}`,
    );
  }
}

/** Anchor's first custom error code — `#[error_code]` enums start here. */
export const ANCHOR_ERROR_OFFSET = 6000;
