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
// The two mints and the pinned swap pool are **validator fixtures**, loaded with `--account`
// before boot, not created here: the pool's bytes name its pair, so the mints must exist at
// addresses known before any test runs. `accounts/dev/deployer.json` is their mint authority, so
// minting against them works exactly as it did against the mint this helper used to create.
//
// `scripts/run-integration-tests.sh` runs the whole `tests/integration/**/*.test.ts` glob with
// `--test-concurrency=1` — one test file at a time, start to finish — which is what makes this
// module's read-then-maybe-write safe: a concurrent second file racing the same check would not be.

import type { Address, Connection, TransactionSigner } from "./env.js";
import { loadOperator, loadT02Authority, loadUnprivileged } from "./env.js";
import { derivePda } from "../../scripts/lib/anchor.js";
import { decodeProtocolConfig } from "../../scripts/lib/decoders.js";
import { initProtocol } from "../../scripts/lib/admin.js";
import { getTokenAccountAddress } from "solana-kite";
import { createAssociatedTokenAccountIdempotent } from "../../scripts/lib/associated-token.js";
import { sendIxs } from "../../scripts/_common.js";
import {
  BYE_MINT_FIXTURE,
  SWAP_POOL_FIXTURE,
  USDC_MINT_FIXTURE,
} from "../../scripts/lib/swap-fixtures.js";

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
  /** The local `operator` authority `protocol_config.operator` is seeded with — every
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
 * `operator`/`t02Authority` are the local keypairs `accounts/dev/{operator,t02}.json` —
 * distinct from `admin` on every validator run, so the wrong-signer matrix
 * compares a genuinely different key rather than a key against itself. `protocolRevenue` reads no
 * check anywhere in the crate and is seeded from the local `unprivileged` key for the same
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
          `is ${operator.address} — the ledger predates the current local authority keys`,
      );
    }
    return {
      protocolConfig,
      usdcMint: decoded.usdcMint,
      poolCounter: decoded.poolCounter,
      operator,
    };
  }

  const [t02Authority, protocolRevenue, t05Authority] = await Promise.all([
    loadT02Authority(),
    loadUnprivileged(),
    loadUnprivileged(),
  ]);

  // `protocol_revenue` is stored as a bare `Pubkey` and validated by nothing at init, but its
  // first consumer — the settlement's fee split — declares it `Account<TokenAccount>` on the
  // principal vault's mint. A wallet address there is accepted at init and then fails every
  // settlement with `AccountOwnedByWrongProgram`, naming an account the config chose. So the
  // config names the USDC account itself, and the account is created here so it exists by the
  // time a settlement reads it. The create is idempotent, as production bootstrap's is, so a ledger
  // that already holds the account does not fail the rerun.
  const protocolRevenueUsdc = await getTokenAccountAddress(
    protocolRevenue.address,
    USDC_MINT_FIXTURE,
    false,
  );
  const { instruction: createRevenueAta } = await createAssociatedTokenAccountIdempotent({
    payer: admin.address,
    owner: protocolRevenue.address,
    mint: USDC_MINT_FIXTURE,
  });
  await sendIxs(connection, admin, [createRevenueAta]);

  // Treasury 05 is not a stored address — it is the USDC ATA of `t05_authority`, pinned by
  // Anchor's own `associated_token::` constraints. Nothing creates it on the way in, and
  // every settlement in every pool credits it, so an absent account here fails each of them.
  const { instruction: createT05Ata } = await createAssociatedTokenAccountIdempotent({
    payer: admin.address,
    owner: t05Authority.address,
    mint: USDC_MINT_FIXTURE,
  });
  await sendIxs(connection, admin, [createT05Ata]);

  await sendIxs(connection, admin, [
    await initProtocol({
      payer: admin.address,
      args: {
        operator: operator.address,
        t02Authority: t02Authority.address,
        t05Authority: t05Authority.address,
        protocolRevenue: protocolRevenueUsdc,
        usdcMint: USDC_MINT_FIXTURE,
        byeMint: BYE_MINT_FIXTURE,
        vrfProgram: admin.address,
        oracleQueue: admin.address,
        swapPool: SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: 100,
      },
    }),
  ]);

  return { protocolConfig, usdcMint: USDC_MINT_FIXTURE, poolCounter: 0, operator };
}
