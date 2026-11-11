// `init_protocol`'s signer, measured against a live validator.
//
// WHY THIS FILE MUST RUN FIRST, AND WHY IT CHECKS THAT IT DID. `protocol_config` is an init-once
// singleton, so the only ledger state in which `init_protocol`'s signer rule is observable at all
// is one where it has not yet been called. Every other integration file reaches a live pool
// through `ensureProtocol`, which calls `init_protocol` itself if the account is absent — so by
// the time any of them runs, the window is closed and a second call collides on `init` regardless
// of who signs.
//
// That is exactly the reasoning once used to EXCLUDE this case, and the reasoning was wrong: it
// concluded "not observable" from "not observable *after the first call*". On a pristine ledger
// the rule is perfectly observable — before the fix, an unprivileged `init_protocol`
// **succeeded**. The authority matrix missed it because every one of its rows looks for a
// rejection, and the evidence here was a success.
//
// `scripts/run-integration-tests.sh` boots a fresh `--reset` ledger and runs the glob at
// `--test-concurrency=1`, and this file's name sorts before every other one — but glob order is
// not a guarantee anybody wrote down, so `before()` below FAILS LOUD if `protocol_config` already
// exists rather than letting the negative cases pass for the wrong reason.
//
// THE POSITIVE CONTROL IS NOT OPTIONAL. A negative-only version of this file would stay green if
// `init_protocol` were unsatisfiable by *everyone* — which is precisely the state the discarded
// upgrade-authority design would have left the suite in (`--bpf-program` loads the program with
// upgrades disabled, so its ProgramData carries no upgrade authority at all). The third case
// below drives the pinned administrator through to success in the same validator run, on the same
// ledger, immediately after the refusals.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import {
  assertProgramIsLive,
  expectAnchorError,
  getConnection,
  loadOperator,
  loadSuiteAdmin,
  loadT02Authority,
  loadUnprivileged,
  fundedWallet,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { getTokenAccountAddress } from "solana-kite";
import { createAtaInstruction } from "../helpers/ata.js";
import { initProtocol, updateByeMint, updateVrfConfig } from "../../scripts/lib/admin.js";
import { decodeProtocolConfig } from "../../scripts/lib/decoders.js";
import { derivePda } from "../../scripts/lib/anchor.js";
import { sendIxs } from "../../scripts/_common.js";
import {
  BYE_MINT_FIXTURE,
  SWAP_POOL_FIXTURE,
  USDC_MINT_FIXTURE,
} from "../../scripts/lib/swap-fixtures.js";

// programs/bye_machine/src/common/errors.rs:69 — `Unauthorized`, in the group based at
// `AdmissionFloorNotAboveTicket = 500`, so 6503. The same code every other admin instruction
// rejects a wrong signer with.
const UNAUTHORIZED = 6503;

// The venue gate, in the group based at `AdmissionFloorNotAboveTicket = 500`.
const SWAP_POOL_INVALID = 6512;
const SWAP_SLIPPAGE_OUT_OF_BOUNDS = 6514;

// Verbatim against programs/bye_machine/src/common/seeds.rs, the convention this suite's other
// files follow for locally-derived PDAs.
const PROTOCOL_CONFIG_SEED = "protocol";

describe("init_protocol — signer authority (pristine ledger)", () => {
  let connection: Connection;
  let admin: TransactionSigner;

  before(async () => {
    connection = getConnection();
    await assertProgramIsLive(connection);
    admin = await loadSuiteAdmin();

    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const existing = await connection.rpc
      .getAccountInfo(protocolConfig, { encoding: "base64" })
      .send();
    if (existing.value !== null) {
      throw new Error(
        `bootstrap-init-authority: protocol_config ${protocolConfig} already exists, so ` +
          `init_protocol's signer rule is no longer observable and the cases below would pass ` +
          `on an 'account already in use' collision instead of on 6503. Some other integration ` +
          `file ran before this one (glob order changed), or the ledger is not pristine — run ` +
          `via scripts/run-integration-tests.sh, which resets it.`,
      );
    }
  });

  // Args are irrelevant to every case here: `payer` is the FIRST field of `InitProtocol`, and
  // Anchor runs each field's constraints in declaration order, so the signer check fires before
  // `protocol_config`'s `init` and before any pinned address is read. Filling them with the
  // suite's own throwaways keeps the call well-formed without minting anything.
  async function argsFor(signer: TransactionSigner) {
    const [operator, unprivileged] = await Promise.all([loadOperator(), loadUnprivileged()]);
    return {
      payer: signer.address,
      args: {
        operator: operator.address,
        t02Authority: unprivileged.address,
        t05Authority: unprivileged.address,
        protocolRevenue: unprivileged.address,
        usdcMint: unprivileged.address,
        byeMint: unprivileged.address,
        vrfProgram: unprivileged.address,
        oracleQueue: unprivileged.address,
        swapPool: unprivileged.address,
        maxSwapSlippageBps: 0,
      },
    };
  }

  it("refuses a wallet nobody granted anything — the front-running case", async () => {
    // A freshly funded wallet, not one of the local suite authorities: this is the observer who
    // watches the deploy transaction land and races the deployer's init.
    const stranger = await fundedWallet(connection, 5);
    await expectAnchorError(
      sendIxs(connection, stranger, [await initProtocol(await argsFor(stranger))]),
      UNAUTHORIZED,
    );
  });

  it("refuses the Operator — privileged elsewhere is still not the pinned administrator", async () => {
    // Discriminates "must be INITIAL_ADMINISTRATOR" from the weaker "must be some known key".
    // The Operator holds six instructions in this program and none of them is this one.
    const operator = await loadOperator();
    await expectAnchorError(
      sendIxs(connection, operator, [await initProtocol(await argsFor(operator))]),
      UNAUTHORIZED,
    );
  });

  // The venue cases below all sign as the pinned administrator, so the signer constraint passes
  // and execution reaches the handler — which is where the venue is read. They must run before
  // the positive control, because that one creates `protocol_config` and closes the window.
  async function venueArgs(overrides: {
    swapPool?: string;
    maxSwapSlippageBps?: number;
  }): Promise<Awaited<ReturnType<typeof argsFor>>> {
    const base = await argsFor(admin);
    return {
      payer: base.payer,
      args: {
        ...base.args,
        usdcMint: USDC_MINT_FIXTURE,
        byeMint: BYE_MINT_FIXTURE,
        swapPool: (overrides.swapPool ?? SWAP_POOL_FIXTURE) as typeof SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: overrides.maxSwapSlippageBps ?? 100,
      },
    };
  }

  it("refuses a swap_pool the venue program does not own", async () => {
    // A wallet address is well-formed and passes every check an address-only gate could make.
    // Pinning it would leave blank settlement unable to pay for the life of the deployment.
    const unprivileged = await loadUnprivileged();
    await expectAnchorError(
      sendIxs(connection, admin, [
        await initProtocol(await venueArgs({ swapPool: unprivileged.address })),
      ]),
      SWAP_POOL_INVALID,
    );
  });

  it("refuses a slippage bound of 10,000 basis points", async () => {
    // At 10,000 the derived minimum collapses to zero and the swap carries no protection at all.
    // Refused here rather than at the first blank drawn, which is the only other place it shows.
    await expectAnchorError(
      sendIxs(connection, admin, [
        await initProtocol(await venueArgs({ maxSwapSlippageBps: 10_000 })),
      ]),
      SWAP_SLIPPAGE_OUT_OF_BOUNDS,
    );
  });

  it("refuses a valid pool paired with a different address in the argument", async () => {
    // The account is the one validated and the argument is the one recorded. If they may differ,
    // every check above runs against a pool the config will not name.
    const unprivileged = await loadUnprivileged();
    const params = await venueArgs({});
    const ix = await initProtocol(params);
    // Rebuild with the account slot left at the real pool and the argument pointing elsewhere.
    const tampered = await initProtocol({
      ...params,
      args: { ...params.args, swapPool: unprivileged.address as typeof SWAP_POOL_FIXTURE },
    });
    assert.notDeepEqual(tampered, ix, "the tampered instruction must differ from the honest one");
    await expectAnchorError(
      sendIxs(connection, admin, [
        { ...tampered, accounts: ix.accounts },
      ]),
      SWAP_POOL_INVALID,
    );
  });

  it("refuses a configured swap pool when $BYE is unset", async () => {
    const params = await venueArgs({});
    await expectAnchorError(
      sendIxs(connection, admin, [
        await initProtocol({ ...params, args: { ...params.args, byeMint: null } }),
      ]),
      SWAP_POOL_INVALID,
    );
  });

  it("admits the pinned administrator with both optional values unset", async () => {
    // The positive control, and the only case in this file that changes state — this call is
    // what actually creates `protocol_config` for the whole validator run, since every other
    // file's `ensureProtocol` finds it already there and only reads it back. Every field with no
    // corresponding `update_*` instruction must therefore be the real, suite-wide value here —
    // `argsFor`'s throwaways are fine for the two signer-only cases above, which never read past
    // the signer check, but wrong forever once this call is the one that lands.
    //
    // `protocol_revenue` is the sharpest of these: it is stored as a bare `Pubkey`, validated by
    // nothing at init, and its first consumer — settlement's fee split — loads it as
    // `Account<TokenAccount>`. A wallet address there is accepted here and then fails every
    // settlement in the suite with `AccountOwnedByWrongProgram`, naming an account the config
    // chose — which is `protocol.ts`'s own `ensureProtocol` fallback's reasoning for creating the
    // ATA before it ever names it, replicated here rather than diverging from it.
    const [t02Authority, protocolRevenue] = await Promise.all([
      loadT02Authority(),
      loadUnprivileged(),
    ]);
    const protocolRevenueUsdc = await getTokenAccountAddress(
      protocolRevenue.address,
      USDC_MINT_FIXTURE,
      false,
    );
    const { instruction: createRevenueAta } = await createAtaInstruction({
      payer: admin.address,
      owner: protocolRevenue.address,
      mint: USDC_MINT_FIXTURE,
    });
    await sendIxs(connection, admin, [createRevenueAta]);

    const params = await argsFor(admin);
    await sendIxs(connection, admin, [
      await initProtocol({
        ...params,
        args: {
          ...params.args,
          t02Authority: t02Authority.address,
          protocolRevenue: protocolRevenueUsdc,
          usdcMint: USDC_MINT_FIXTURE,
          byeMint: null,
          swapPool: null,
        },
      }),
    ]);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    const info = await connection.rpc
      .getAccountInfo(protocolConfig, { encoding: "base64" })
      .send();
    assert.notEqual(info.value, null, "init_protocol reported success but wrote no account");
    const [b64] = info.value!.data as [string, string];
    const decoded = decodeProtocolConfig(Buffer.from(b64, "base64"));
    assert.equal(
      decoded.administrator,
      admin.address,
      "administrator is not the key that signed — the handler no longer stores payer",
    );
    assert.equal(decoded.byeMint, "11111111111111111111111111111111");
    assert.equal(decoded.swapPool, "11111111111111111111111111111111");

    // Every other integration file's `ensureProtocol` expects to find the venue already wired —
    // `protocol.ts`'s own fallback path sets exactly this (`byeMint`/`swapPool` fixtures,
    // `vrfProgram`/`oracleQueue` at the admin placeholder, 100 bps) when it is the one creating
    // `protocol_config`. This test's own point was that `init_protocol` accepts both unset, not
    // that they stay unset — so it restores the config to that shared state immediately after
    // proving the acceptance, the same way `swap-venue-rotation.test.ts` restores what it rotates.
    await sendIxs(connection, admin, [
      await updateByeMint({ administrator: admin.address, byeMint: BYE_MINT_FIXTURE }),
    ]);
    await sendIxs(connection, admin, [
      await updateVrfConfig({
        administrator: admin.address,
        vrfProgram: admin.address,
        oracleQueue: admin.address,
        swapPool: SWAP_POOL_FIXTURE,
        maxSwapSlippageBps: 100,
      }),
    ]);
    const restoredInfo = await connection.rpc
      .getAccountInfo(protocolConfig, { encoding: "base64" })
      .send();
    const [restoredB64] = restoredInfo.value!.data as [string, string];
    const restored = decodeProtocolConfig(Buffer.from(restoredB64, "base64"));
    assert.equal(restored.byeMint, BYE_MINT_FIXTURE, "the restore did not set byeMint");
    assert.equal(restored.swapPool, SWAP_POOL_FIXTURE, "the restore did not set swapPool");
    assert.equal(restored.maxSwapSlippageBps, 100, "the restore did not set maxSwapSlippageBps");
  });
});
