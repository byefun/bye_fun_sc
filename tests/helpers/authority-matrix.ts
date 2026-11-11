// The wrong-signer matrix: which authority each instruction's signers carry, and which of those
// cells have an executed negative row.
//
// Two files consume this table and neither may diverge from it. The census sweeps the IDL and
// requires every signer the program declares to appear here, so a new instruction — or a new
// signer on an existing one — fails closed rather than joining the matrix unnoticed. The
// integration suite iterates the executed rows and runs them. Coverage is therefore structural:
// a cell is covered because a row exists and is executed, not because a list says so.
//
// **A row that only refuses is not a row.** Every rejection is paired with a control that runs
// the same call with the right signer and requires it to succeed. This is not ceremony. The
// evidence for a *missing* authority check is a success, and a matrix whose every row looks for a
// rejection cannot see one, even when every row is green. A row whose control does not pass has proved nothing about the
// rejection beside it, because the call may have failed for a reason that has nothing to do with
// who signed it.
//
// `AbsenceOfCheck` cells invert the assertion deliberately: where the program is permissionless
// on purpose, a stranger's call must *succeed*, and a row that expected a rejection there would
// red the moment the design was implemented correctly.

/** How a signer cell is authorised. Mirrors the classes the crate's own signer census partitions
 * on, because a negative row is only meaningful against the class it is attacking. */
export type AuthorityClass =
  | "Administrator"
  | "Operator"
  | "Depositor"
  | "BodyAuthorized"
  /** A signer checked against `ProtocolConfig.t02_authority`, read at call time, so a rotation
   * takes effect on the next call. */
  | "T02Authority"
  /** A signer the account it acts on is seeded from: an escrow is `[escrow, owner]`, so any other
   * signer derives a different address and is refused by the seeds before the handler runs. No
   * field is compared, which is why this is not `Depositor`. */
  | "SeedBound"
  | "AddressPinned"
  | "AbsenceOfCheck";

/** One signer on one instruction — the unit the matrix is indexed by. A handler with two
 * signers contributes two cells, each attacked separately, because each releases something the
 * other does not. */
export interface SignerCell {
  instruction: string;
  signer: string;
  authority: AuthorityClass;
}

/** What a cell's negative row expects when the wrong party signs. */
export type RowExpectation =
  /** The call is rejected. `code` is the Anchor error the rejection must carry — a row that
   * accepts any failure passes on a call that failed to build. */
  | { kind: "rejects"; code: number }
  /** The call succeeds: the instruction is permissionless by design, and the row exists to catch
   * a check being added where the design says there is none. */
  | { kind: "succeeds" };

/** Which integration file drives a row. A row lives where its fixture already lives: standing a
 * three-signer sale or a Core-custody roll up a second time, in a file of this matrix's own,
 * would be a second fixture to keep in step with the first for no gain in what is proved. */
export type MatrixSuite =
  | "wrong-signer-matrix"
  | "instant-sell"
  | "treasury-activation"
  | "collection-admission"
  | "pre-tge-config"
  | "core-standard-exit"
  | "escrow"
  | "legacy-standard-deposit"
  | "legacy-standard-exit"
  | "migrate-pool"
  | "consolidate-treasuries"
  | "treasury-ceiling-sweep"
  | "t02-authority"
  | "unexecuted-encoders";

export interface MatrixRow extends SignerCell {
  expectation: RowExpectation;
  /** Which party signs in place of the authorised one. */
  imposter: "stranger" | "operator" | "administrator" | "other-depositor";
  suite: MatrixSuite;
}

/** `Unauthorized` — the program's own code for a signer that is not the authority it must be. */
export const UNAUTHORIZED = 6503;

/** Anchor's `ConstraintAddress` — a signer pinned by `address =` fails here, before any handler
 * body runs, so an address-pinned cell reds differently from an attribute-constrained one. */
export const CONSTRAINT_ADDRESS = 2012;

/** Anchor's `ConstraintSeeds` — what a `SeedBound` cell raises: the wrong signer derives another
 * address, which is not the account passed. */
export const CONSTRAINT_SEEDS = 2006;

/** `NotPositionOwner` — what the buyback pair raises when its `seller` does not own the accounts
 * the sale moves. The authority *class* is `Depositor` (a signer checked against a recorded
 * owner) but the code is not `Unauthorized`, because here the binding is to the token
 * accounts rather than to a field on a position. A row that assumed one code for the class would
 * have been wrong on exactly these two cells. */
export const NOT_POSITION_OWNER = 6302;

/** `NotATreasuryPosition` — what the T02 instructions raise for a position whose depositor is not
 * the Treasury 02 identity. The right authority signing the wrong position is a different failure
 * from the wrong signer, and it is refused with its own code. */
export const NOT_A_TREASURY_POSITION = 6311;

/**
 * Every signer cell the program declares, with the authority it carries.
 *
 * Transcribed against the crate's signer census rather than invented here; the census test
 * cross-checks this table's extent against the IDL in both directions, so a cell that is wrong
 * about *which* signers exist reds, and only the class itself is asserted by hand.
 */
export const SIGNER_CELLS: readonly SignerCell[] = [
  // ── Administrator ────────────────────────────────────────────────────────────────────────
  { instruction: "admit_collection", signer: "administrator", authority: "Administrator" },
  { instruction: "init_pool", signer: "administrator", authority: "Administrator" },
  { instruction: "migrate_pool", signer: "administrator", authority: "Administrator" },
  { instruction: "set_pause", signer: "administrator", authority: "Administrator" },
  { instruction: "update_config", signer: "administrator", authority: "Administrator" },
  { instruction: "update_bye_mint", signer: "administrator", authority: "Administrator" },
  { instruction: "update_protocol_revenue", signer: "administrator", authority: "Administrator" },
  { instruction: "update_swap_pool", signer: "administrator", authority: "Administrator" },
  { instruction: "update_vrf_config", signer: "administrator", authority: "Administrator" },
  { instruction: "withdraw_collection", signer: "administrator", authority: "Administrator" },
  { instruction: "set_t04_ceiling", signer: "administrator", authority: "Administrator" },
  { instruction: "consolidate_treasuries", signer: "administrator", authority: "Administrator" },

  // ── Operator ─────────────────────────────────────────────────────────────────────────────
  { instruction: "approve_deposit", signer: "operator", authority: "Operator" },
  { instruction: "reject_deposit", signer: "operator", authority: "Operator" },
  { instruction: "begin_sweep", signer: "operator", authority: "Operator" },
  { instruction: "end_sweep", signer: "operator", authority: "Operator" },
  { instruction: "record_value", signer: "operator", authority: "Operator" },
  { instruction: "update_tier", signer: "operator", authority: "Operator" },
  { instruction: "close_seized", signer: "operator", authority: "Operator" },
  { instruction: "activate_acquired", signer: "operator", authority: "Operator" },
  { instruction: "recover_batch", signer: "operator", authority: "Operator" },
  { instruction: "settle_roll", signer: "operator", authority: "Operator" },
  { instruction: "settle_roll_core", signer: "operator", authority: "Operator" },
  { instruction: "sweep_t04_overflow", signer: "operator", authority: "Operator" },

  // ── Depositor (a signer checked against a recorded owner) ────────────────────────────────
  { instruction: "claim_fee", signer: "depositor", authority: "Depositor" },
  { instruction: "deposit", signer: "depositor", authority: "Depositor" },
  { instruction: "deposit_core", signer: "depositor", authority: "Depositor" },
  { instruction: "withdraw", signer: "depositor", authority: "Depositor" },
  { instruction: "withdraw_core", signer: "depositor", authority: "Depositor" },
  { instruction: "claim_nft", signer: "depositor", authority: "Depositor" },
  { instruction: "claim_nft_core", signer: "depositor", authority: "Depositor" },

  // ── The two-signer buyback pair ──────────────────────────────────────────────────────────
  { instruction: "instant_sell", signer: "seller", authority: "Depositor" },
  { instruction: "instant_sell", signer: "operator", authority: "Operator" },
  { instruction: "instant_sell_core", signer: "seller", authority: "Depositor" },
  { instruction: "instant_sell_core", signer: "operator", authority: "Operator" },

  // ── The T02 authority's instructions ────────────────────────────────────────────────────
  { instruction: "t02_claim_fee", signer: "t02_authority", authority: "T02Authority" },
  { instruction: "t02_withdraw", signer: "t02_authority", authority: "T02Authority" },
  { instruction: "t02_withdraw_core", signer: "t02_authority", authority: "T02Authority" },
  { instruction: "t02_claim_nft", signer: "t02_authority", authority: "T02Authority" },
  { instruction: "t02_claim_nft_core", signer: "t02_authority", authority: "T02Authority" },
  { instruction: "t02_deposit", signer: "t02_authority", authority: "T02Authority" },
  { instruction: "t02_deposit_core", signer: "t02_authority", authority: "T02Authority" },

  // ── The escrow owner's instructions, and the fee claim into an escrow ──────────────────────
  { instruction: "open_escrow", signer: "owner", authority: "SeedBound" },
  { instruction: "escrow_deposit", signer: "owner", authority: "SeedBound" },
  { instruction: "escrow_withdraw", signer: "owner", authority: "SeedBound" },
  { instruction: "set_escrow_armed", signer: "owner", authority: "SeedBound" },
  { instruction: "claim_fee_to_escrow", signer: "depositor", authority: "Depositor" },

  // ── One-off classes ──────────────────────────────────────────────────────────────────────
  { instruction: "set_authorities", signer: "signer", authority: "BodyAuthorized" },
  /** The batch's roller or the Operator, checked in the handler; the rent goes to the signer. */
  { instruction: "close_batch", signer: "signer", authority: "BodyAuthorized" },
  { instruction: "vrf_callback", signer: "vrf_program_identity", authority: "AddressPinned" },

  // ── Deliberate absences ──────────────────────────────────────────────────────────────────
  { instruction: "init_protocol", signer: "payer", authority: "AbsenceOfCheck" },
  { instruction: "return_rejected", signer: "payer", authority: "AbsenceOfCheck" },
  { instruction: "return_rejected_core", signer: "payer", authority: "AbsenceOfCheck" },
  { instruction: "commit_rolls", signer: "roller", authority: "AbsenceOfCheck" },
];

/** The nine roll-pipeline and buyback instructions. The census requires every one of their cells
 * to carry an executed row, so none of them sits outside the matrix's extent. */
export const ROLL_PIPELINE_INSTRUCTIONS: readonly string[] = [
  "commit_rolls",
  "vrf_callback",
  "settle_roll",
  "settle_roll_core",
  "recover_batch",
  "close_batch",
  "instant_sell",
  "instant_sell_core",
  "activate_acquired",
];

/**
 * The rows the integration suite executes.
 *
 * Scoped to the nine roll-pipeline and buyback instructions. The cells outside that set are listed in
 * `DEFERRED_CELLS` with the reason they are not executed yet, so the matrix's true extent is
 * readable rather than implied by what happens to be here.
 */
export const EXECUTED_ROWS: readonly MatrixRow[] = [
  {
    instruction: "commit_rolls",
    signer: "roller",
    authority: "AbsenceOfCheck",
    imposter: "stranger",
    // Permissionless by design: a roller is any user, and the signer binds only the USDC account
    // it is charged from. A rejection here would be the regression.
    expectation: { kind: "succeeds" },
    suite: "wrong-signer-matrix",
  },
  {
    instruction: "vrf_callback",
    signer: "vrf_program_identity",
    authority: "AddressPinned",
    imposter: "stranger",
    expectation: { kind: "rejects", code: CONSTRAINT_ADDRESS },
    suite: "wrong-signer-matrix",
  },
  {
    instruction: "settle_roll",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "wrong-signer-matrix",
  },
  {
    instruction: "recover_batch",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "wrong-signer-matrix",
  },
  {
    instruction: "close_batch",
    signer: "signer",
    authority: "BodyAuthorized",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "wrong-signer-matrix",
  },
  {
    instruction: "settle_roll_core",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "wrong-signer-matrix",
  },
  {
    instruction: "activate_acquired",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "treasury-activation",
  },
  {
    instruction: "instant_sell",
    signer: "seller",
    authority: "Depositor",
    imposter: "stranger",
    expectation: { kind: "rejects", code: NOT_POSITION_OWNER },
    suite: "instant-sell",
  },
  {
    instruction: "instant_sell",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "instant-sell",
  },
  {
    instruction: "instant_sell_core",
    signer: "seller",
    authority: "Depositor",
    imposter: "stranger",
    expectation: { kind: "rejects", code: NOT_POSITION_OWNER },
    suite: "instant-sell",
  },
  {
    instruction: "instant_sell_core",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "instant-sell",
  },

  // ── The T02 authority's instructions ────────────────────────────────────────────────────
  {
    instruction: "t02_claim_fee",
    signer: "t02_authority",
    authority: "T02Authority",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "t02-authority",
  },
  {
    instruction: "t02_withdraw",
    signer: "t02_authority",
    authority: "T02Authority",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "t02-authority",
  },
  {
    instruction: "t02_withdraw_core",
    signer: "t02_authority",
    authority: "T02Authority",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "t02-authority",
  },
  {
    instruction: "t02_claim_nft",
    signer: "t02_authority",
    authority: "T02Authority",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "t02-authority",
  },
  {
    instruction: "t02_claim_nft_core",
    signer: "t02_authority",
    authority: "T02Authority",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "t02-authority",
  },
  {
    instruction: "t02_deposit",
    signer: "t02_authority",
    authority: "T02Authority",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "t02-authority",
  },
  {
    instruction: "t02_deposit_core",
    signer: "t02_authority",
    authority: "T02Authority",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "t02-authority",
  },

  // ── Remaining cells, moved out of DEFERRED_CELLS one at a time ──────────────────────────────
  {
    instruction: "init_pool",
    signer: "administrator",
    authority: "Administrator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "collection-admission",
  },
  {
    instruction: "admit_collection",
    signer: "administrator",
    authority: "Administrator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "collection-admission",
  },
  {
    instruction: "withdraw_collection",
    signer: "administrator",
    authority: "Administrator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "collection-admission",
  },
  {
    instruction: "update_bye_mint",
    signer: "administrator",
    authority: "Administrator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "pre-tge-config",
  },
  {
    instruction: "update_protocol_revenue",
    signer: "administrator",
    authority: "Administrator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "pre-tge-config",
  },
  {
    instruction: "update_swap_pool",
    signer: "administrator",
    authority: "Administrator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "pre-tge-config",
  },
  {
    instruction: "update_vrf_config",
    signer: "administrator",
    authority: "Administrator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "wrong-signer-matrix",
  },
  {
    instruction: "approve_deposit",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "core-standard-exit",
  },
  {
    instruction: "reject_deposit",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "core-standard-exit",
  },
  {
    instruction: "begin_sweep",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "core-standard-exit",
  },
  {
    instruction: "end_sweep",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "core-standard-exit",
  },
  {
    instruction: "record_value",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "core-standard-exit",
  },
  {
    instruction: "withdraw_core",
    signer: "depositor",
    authority: "Depositor",
    imposter: "stranger",
    expectation: { kind: "rejects", code: NOT_POSITION_OWNER },
    suite: "core-standard-exit",
  },
  {
    instruction: "claim_nft_core",
    signer: "depositor",
    authority: "Depositor",
    imposter: "stranger",
    expectation: { kind: "rejects", code: NOT_POSITION_OWNER },
    suite: "core-standard-exit",
  },
  {
    instruction: "deposit",
    signer: "depositor",
    authority: "Depositor",
    imposter: "stranger",
    expectation: { kind: "rejects", code: NOT_POSITION_OWNER },
    suite: "legacy-standard-deposit",
  },
  {
    instruction: "withdraw",
    signer: "depositor",
    authority: "Depositor",
    imposter: "stranger",
    expectation: { kind: "rejects", code: NOT_POSITION_OWNER },
    suite: "legacy-standard-exit",
  },
  {
    instruction: "claim_nft",
    signer: "depositor",
    authority: "Depositor",
    imposter: "stranger",
    expectation: { kind: "rejects", code: NOT_POSITION_OWNER },
    suite: "legacy-standard-exit",
  },

  // ── The four instructions that had never executed against a validator at all ────────────────
  {
    instruction: "update_config",
    signer: "administrator",
    authority: "Administrator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "unexecuted-encoders",
  },
  {
    instruction: "set_pause",
    signer: "administrator",
    authority: "Administrator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "unexecuted-encoders",
  },
  {
    instruction: "set_authorities",
    signer: "signer",
    authority: "BodyAuthorized",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "unexecuted-encoders",
  },
  {
    instruction: "migrate_pool",
    signer: "administrator",
    authority: "Administrator",
    imposter: "operator",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "migrate-pool",
  },
  {
    instruction: "consolidate_treasuries",
    signer: "administrator",
    authority: "Administrator",
    imposter: "operator",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "consolidate-treasuries",
  },
  {
    instruction: "set_t04_ceiling",
    signer: "administrator",
    authority: "Administrator",
    imposter: "operator",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "treasury-ceiling-sweep",
  },
  {
    instruction: "sweep_t04_overflow",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "treasury-ceiling-sweep",
  },
  // A stranger signing against the owner's escrow. The four owner cells fail at the seeds; the fee
  // claim fails earlier, at the position's recorded depositor.
  {
    instruction: "open_escrow",
    signer: "owner",
    authority: "SeedBound",
    imposter: "stranger",
    expectation: { kind: "rejects", code: CONSTRAINT_SEEDS },
    suite: "escrow",
  },
  {
    instruction: "escrow_deposit",
    signer: "owner",
    authority: "SeedBound",
    imposter: "stranger",
    expectation: { kind: "rejects", code: CONSTRAINT_SEEDS },
    suite: "escrow",
  },
  {
    instruction: "escrow_withdraw",
    signer: "owner",
    authority: "SeedBound",
    imposter: "stranger",
    expectation: { kind: "rejects", code: CONSTRAINT_SEEDS },
    suite: "escrow",
  },
  {
    instruction: "set_escrow_armed",
    signer: "owner",
    authority: "SeedBound",
    imposter: "stranger",
    expectation: { kind: "rejects", code: CONSTRAINT_SEEDS },
    suite: "escrow",
  },
  {
    instruction: "claim_fee_to_escrow",
    signer: "depositor",
    authority: "Depositor",
    imposter: "stranger",
    expectation: { kind: "rejects", code: NOT_POSITION_OWNER },
    suite: "escrow",
  },
  {
    instruction: "update_tier",
    signer: "operator",
    authority: "Operator",
    imposter: "stranger",
    expectation: { kind: "rejects", code: UNAUTHORIZED },
    suite: "unexecuted-encoders",
  },
];

/**
 * Cells with no executed row, and why.
 *
 * This list is the matrix's declared debt. It is not an exemption: the census requires every
 * entry to name a cell the program actually declares, so an instruction cannot be parked here by
 * misspelling it, and nothing may sit here without a reason a reader can weigh.
 */
export const DEFERRED_CELLS: readonly (SignerCell & { reason: string })[] = [
  // Cells whose negative row already exists, written before this table, in a suite of its own.
  {
    instruction: "init_protocol",
    signer: "payer",
    authority: "AbsenceOfCheck",
    reason: "executed in the protocol-initialisation authority suite, with its success control",
  },
  {
    instruction: "close_seized",
    signer: "operator",
    authority: "Operator",
    reason: "executed in the Core seizure suite, with its Operator control",
  },
  {
    instruction: "deposit_core",
    signer: "depositor",
    authority: "Depositor",
    reason: "executed in the Core standard-deposit suite as a non-owner intake",
  },
  {
    instruction: "return_rejected",
    signer: "payer",
    authority: "AbsenceOfCheck",
    reason: "executed in the rejected-return custody suite as a third-party payer",
  },
  {
    instruction: "return_rejected_core",
    signer: "payer",
    authority: "AbsenceOfCheck",
    reason: "its Token Metadata twin is executed, this one is not",
  },
  {
    instruction: "claim_fee",
    signer: "depositor",
    authority: "Depositor",
    reason:
      "executed in the claim-fee suite as a stranger signing another's position, against " +
      "NotPositionOwner 6302, with a position-untouched control",
  },
];

/** Every cell, keyed for lookup. */
export function cellKey(cell: SignerCell): string {
  return `${cell.instruction}.${cell.signer}`;
}
