// Issue #7 / D-156 — `init_protocol`'s signer, measured against a live validator.
//
// WHY THIS FILE MUST RUN FIRST, AND WHY IT CHECKS THAT IT DID. `protocol_config` is an init-once
// singleton, so the only ledger state in which `init_protocol`'s signer rule is observable at all
// is one where it has not yet been called. Every other integration file reaches a live pool
// through `ensureProtocol`, which calls `init_protocol` itself if the account is absent — so by
// the time any of them runs, the window is closed and a second call collides on `init` regardless
// of who signs.
//
// That is exactly the reasoning that earlier excluded this case,
// and the reasoning was wrong: it concluded "not observable" from "not observable *after the
// first call*". On a pristine ledger the rule is perfectly observable — before the fix, an
// unprivileged `init_protocol` **succeeded**. The authority matrix (A25–A30) missed it because
// every one of its rows looks for a rejection, and the evidence here was a success.
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
  loadUnprivileged,
  fundedWallet,
  type Connection,
  type TransactionSigner,
} from "../helpers/env.js";
import { ensureProtocol } from "../helpers/protocol.js";
import { initProtocol } from "../../scripts/lib/admin.js";
import { decodeProtocolConfig } from "../../scripts/lib/decoders.js";
import { derivePda } from "../../scripts/lib/anchor.js";
import { sendIxs } from "../../scripts/_common.js";

// programs/bye_machine/src/common/errors.rs:69 — `Unauthorized`, in the group based at
// `AdmissionFloorNotAboveTicket = 500`, so 6503. The same code every other admin instruction
// rejects a wrong signer with.
const UNAUTHORIZED = 6503;

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
        t03Authority: unprivileged.address,
        t02Authority: unprivileged.address,
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

  it("refuses a wallet nobody granted anything — the front-running case from issue #7", async () => {
    // A freshly funded wallet, not one of the committed authorities: this is the observer who
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

  it("admits the pinned administrator, and the account it writes names that same key", async () => {
    // The positive control, and the only case in this file that changes state. `ensureProtocol`
    // is the suite's own bootstrap — using it here rather than a hand-rolled call means the
    // control exercises the exact path every other integration file depends on.
    const bootstrap = await ensureProtocol(connection, admin);
    const { address: protocolConfig } = await derivePda([PROTOCOL_CONFIG_SEED]);
    assert.equal(bootstrap.protocolConfig, protocolConfig);

    const info = await connection.rpc
      .getAccountInfo(protocolConfig, { encoding: "base64" })
      .send();
    assert.notEqual(info.value, null, "init_protocol reported success but wrote no account");
    const [b64] = info.value!.data as [string, string];
    assert.equal(
      decodeProtocolConfig(Buffer.from(b64, "base64")).administrator,
      admin.address,
      "administrator is not the key that signed — the handler no longer stores payer",
    );
  });
});
