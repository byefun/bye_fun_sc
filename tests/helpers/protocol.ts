// Idempotent protocol bootstrap shared by every integration file that needs a live pool.
//
// `protocol_config` is an on-chain init-once singleton (init_protocol.rs's `#[account(init, ...)]`),
// and `init_pool`'s own `usdc_mint` account is pinned to it
// (`#[account(address = protocol_config.usdc_mint)]`, init_pool.rs:248) — so at most one
// integration test FILE, whichever the runner happens to execute first, may actually call
// `init_protocol`; every other file must read the config back and reuse its `usdc_mint`, never
// mint its own. `pool_id` is likewise not a caller's free choice: init_pool.rs:111 reads it from
// `protocol_config.pool_counter` server-side, so a client only ever passes whatever that counter
// currently is, purely to make its own PDA derivation agree with the program's.
//
// `scripts/run-integration-tests.sh` runs the whole `tests/integration/**/*.test.ts` glob with
// `--test-concurrency=1` — one test file at a time, start to finish — which is what makes this
// module's read-then-maybe-write safe: a concurrent second file racing the same check would not be.

import type { Address, Connection, TransactionSigner } from "./env.js";
import { loadOperator, loadT03Authority, loadT02Authority, loadUnprivileged } from "./env.js";
import { derivePda } from "../../scripts/lib/anchor.js";
import { decodeProtocolConfig } from "../../scripts/lib/decoders.js";
import { initProtocol } from "../../scripts/lib/admin.js";
import { sendIxs } from "../../scripts/_common.js";

// Verbatim against programs/bye_machine/src/common/seeds.rs — this file's own cross-check, the
// same convention rule-set-replay.test.ts already follows for `position`/`vault`.
const PROTOCOL_CONFIG_SEED = "protocol";

export type ProtocolBootstrap = {
  protocolConfig: Address;
  /** `protocol_config.usdc_mint` — every `init_pool` call in this validator run must pass this
   * exact mint, whether or not this call is the one that created the config. */
  usdcMint: Address;
  /** `protocol_config.pool_counter` at the moment this ran — the first of however many
   * sequential `poolId`s a caller means to consume next. */
  poolCounter: number;
  /** The committed `operator` authority `protocol_config.operator` is seeded with — every
   * Operator-gated instruction (`approve_deposit`, `reject_deposit`, `record_value`,
   * `begin_sweep`, `end_sweep`, `update_tier`) must sign with this, never `admin`. */
  operator: TransactionSigner;
};

/**
 * Reads `protocol_config` back if it already exists (some earlier test file's `before()` already
 * created it this validator run); otherwise mints a fresh USDC stand-in and calls `init_protocol`
 * itself. Either way, returns the mint, pool counter and operator a caller needs to build calls
 * that agree with whichever file actually owns initialization this run — callers must never
 * assume they are that file.
 *
 * `operator`/`t03Authority`/`t02Authority` are the committed keypairs `accounts/dev/{operator,
 * t03,t02}.json` — distinct from `admin` on every validator run, so the wrong-signer matrix
 * compares a genuinely different key rather than a key against itself. `protocolRevenue` reads no
 * check anywhere in the crate and is seeded from the committed `unprivileged` key for the same
 * "not admin" reason, without needing a fifth file.
 */
export async function ensureProtocol(
  connection: Connection,
  admin: TransactionSigner,
): Promise<ProtocolBootstrap> {
  const operator = await loadOperator();
  const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
  const existing = await connection.rpc
    .getAccountInfo(protocolConfig, { encoding: "base64" })
    .send();

  if (existing.value !== null) {
    const [b64] = existing.value.data as [string, string];
    const decoded = decodeProtocolConfig(Buffer.from(b64, "base64"));
    // Read back rather than assume: this file did not create `protocol_config`, so the operator it
    // installed is only knowable from the account. Returning the loaded keypair unchecked would hand
    // callers a signer that silently does not match `protocol_config.operator` — every Operator-gated
    // case then fails as an authority rejection rather than as the fixture mismatch it is.
    if (decoded.operator !== operator.address) {
      throw new Error(
        `ensureProtocol: protocol_config.operator is ${decoded.operator}, but accounts/dev/operator.json ` +
          `is ${operator.address} — the ledger predates the current committed authority keys`,
      );
    }
    return {
      protocolConfig,
      usdcMint: decoded.usdcMint,
      poolCounter: decoded.poolCounter,
      operator,
    };
  }

  const [t03Authority, t02Authority, protocolRevenue] = await Promise.all([
    loadT03Authority(),
    loadT02Authority(),
    loadUnprivileged(),
  ]);

  const usdcMint = await connection.createTokenMint({
    mintAuthority: admin,
    decimals: 6,
    useTokenExtensions: false,
  });

  await sendIxs(connection, admin, [
    await initProtocol({
      payer: admin.address,
      args: {
        operator: operator.address,
        t03Authority: t03Authority.address,
        t02Authority: t02Authority.address,
        protocolRevenue: protocolRevenue.address,
        usdcMint,
        byeMint: usdcMint,
        vrfProgram: admin.address,
        oracleQueue: admin.address,
        swapPool: admin.address,
        maxSwapSlippageBps: 0,
      },
    }),
  ]);

  return { protocolConfig, usdcMint, poolCounter: 0, operator };
}
