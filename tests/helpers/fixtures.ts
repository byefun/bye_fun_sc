// pNFT fixture minting helpers (T-47).
//
// Mints real Token Metadata pNFTs — and the deliberately non-admissible variants — against the
// validator T-46 loads with the real Token Metadata / Token Auth Rules programs and the
// eBJLFY…zyT9 rule-set account. Every fixture here is a genuinely minted on-chain account, not a
// byte-injected stand-in: deposit.rs's `validate_admission` reads owner + PDA + parsed content
// straight off `metadata`/`master_edition`, and a Metadata/MasterEdition PDA can only be created by
// Token Metadata's own CPI (only the owning program can invoke_signed its own seeds), so nothing
// short of the real program creating these at their real addresses can stand in for one.
//
// Instruction encoding here is hand-rolled (Shank layout: 1-byte outer discriminator ‖ 1-byte inner
// discriminator ‖ borsh args), the same reason anchor.ts's own header gives for existing — this repo
// carries no Metaplex SDK dependency. Layouts are cross-checked against the `mpl-token-metadata`
// Rust crate (the exact version programs/bye_machine depends on) rather than re-derived by hand.

import {
  addSignersToInstruction,
  generateKeyPairSigner,
  type Address,
  type Instruction,
  type TransactionSigner,
} from "@solana/kit";
import { getTokenAccountAddress } from "solana-kite";
import type { Connection } from "./env.js";
import { address } from "./env.js";
import {
  Borsh,
  derivePda,
  readonly,
  seedFromPubkey,
  signer,
  writable,
  writableSigner,
} from "../../scripts/lib/anchor.js";
import { sendIxs } from "../../scripts/_common.js";
import {
  ASSOCIATED_TOKEN_PROGRAM,
  AUTH_RULES_PROGRAM,
  SYSTEM_PROGRAM,
  SYSVAR_INSTRUCTIONS,
  TOKEN_METADATA_PROGRAM,
  TOKEN_PROGRAM,
} from "../../scripts/constants.js";

// ── Fixed addresses this file pins ──────────────────────────────────────────────────────────────
// The rule-set account `run-integration-tests.sh` loads via `--account` (not committed here —
// touching that script or tests/fixtures/programs/* is T-46's, not this file's).
export const RULE_SET_ADDRESS = address("eBJLFYPxJmMGKuFwpDWkzxZeUrad92kZRC5BJLpzyT9");
// MPL-Core's real, documented program id (same on every cluster). Not loaded on this validator
// (T-46 clones Token Metadata + Token Auth Rules only) — see `mintMplCoreLikeAsset`'s own doc
// comment for what that does and does not prove.
const MPL_CORE_PROGRAM_ID = address("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

// ── Metaplex PDAs (mpl-token-metadata 5.1.1: generated/accounts/{metadata,master_edition,token_record}.rs) ──
// Exported for codec-level unit tests (no validator) — see tests/unit/fixtures.test.ts.
export async function metadataPda(mint: Address): Promise<Address> {
  const { address: pda } = await derivePda(
    ["metadata", seedFromPubkey(TOKEN_METADATA_PROGRAM), seedFromPubkey(mint)],
    TOKEN_METADATA_PROGRAM,
  );
  return pda;
}

export async function masterEditionPda(mint: Address): Promise<Address> {
  const { address: pda } = await derivePda(
    ["metadata", seedFromPubkey(TOKEN_METADATA_PROGRAM), seedFromPubkey(mint), "edition"],
    TOKEN_METADATA_PROGRAM,
  );
  return pda;
}

export async function tokenRecordPda(mint: Address, token: Address): Promise<Address> {
  const { address: pda } = await derivePda(
    [
      "metadata",
      seedFromPubkey(TOKEN_METADATA_PROGRAM),
      seedFromPubkey(mint),
      "token_record",
      seedFromPubkey(token),
    ],
    TOKEN_METADATA_PROGRAM,
  );
  return pda;
}

// ── Borsh args (mpl-token-metadata 5.1.1: generated/instructions/{create_v1,mint_v1,verify_collection_v1}.rs) ──
// Outer/inner discriminator pairs — the crate's own `InstructionData::new()` literals, not derived.
export const CREATE_V1_DISCRIMINATOR = [42, 0] as const;
export const MINT_V1_DISCRIMINATOR = [43, 0] as const;
export const VERIFY_COLLECTION_V1_DISCRIMINATOR = [52, 1] as const;

export function mplIxData(discriminator: readonly [number, number], args?: Buffer): Buffer {
  const head = Buffer.from(discriminator);
  return args ? Buffer.concat([head, args]) : head;
}

/** `TokenStandard` (generated/types/token_standard.rs) — declaration-order tags. */
export const TOKEN_STANDARD_TAG = {
  NonFungible: 0,
  ProgrammableNonFungible: 4,
} as const;
type TokenStandardName = keyof typeof TOKEN_STANDARD_TAG;

/** `Option<Collection>` — `Collection { verified: bool, key: Pubkey }` (field order matters). */
export function encodeOptionCollection(collection: { key: Address; verified: boolean } | null): Buffer {
  const b = new Borsh();
  if (collection === null) return b.u8(0).build();
  return b.u8(1).bool(collection.verified).pubkey(collection.key).build();
}

/**
 * `CreateV1InstructionArgs`, in declared field order. Fixed across every fixture this file mints:
 * empty symbol/uri, no seller fee, no creators, `is_mutable: true`, decimals `Some(0)`,
 * `print_supply: Some(PrintSupply::Zero)` (a true, non-printable 1/1 — tag 0, unit variant).
 */
/**
 * `mpl-token-metadata` 5.1.1's `MAX_NAME_LENGTH` (`src/lib.rs:10`). Enforced here, client-side,
 * because the on-chain failure is unhelpful out of proportion to the mistake: `CreateV1` rejects a
 * 33-byte name with **`NameTooLong`, custom program error #11** — an unnamed foreign code, raised
 * from inside whichever `before()` hook built the fixture, after every account that precedes it has
 * already been created. Measured: a 35-character collection name cost 15 s and reported only
 * `custom program error: #11`. Every caller of `mintAdmissiblePnft` also inherits this limit
 * *through a template*, with no signal that it has a byte budget at all — see `label`'s guard.
 */
export const MAX_NAME_LENGTH = 32;

function assertNameFits(name: string, context: string): void {
  // Bytes, not characters: Token Metadata bounds the borsh-serialised string.
  const bytes = Buffer.byteLength(name, "utf8");
  if (bytes > MAX_NAME_LENGTH) {
    throw new Error(
      `${context}: name "${name}" is ${bytes} bytes, over Token Metadata's MAX_NAME_LENGTH of ` +
        `${MAX_NAME_LENGTH}. CreateV1 would reject this as NameTooLong (custom program error #11), ` +
        `from inside whatever hook is building this fixture`,
    );
  }
}

export function encodeCreateV1Args(params: {
  name: string;
  tokenStandard: TokenStandardName;
  collection: { key: Address; verified: boolean } | null;
  ruleSet: Address | null;
}): Buffer {
  assertNameFits(params.name, "encodeCreateV1Args");
  return new Borsh()
    .string(params.name)
    .string("") // symbol
    .string("") // uri
    .u16(0) // seller_fee_basis_points
    .u8(0) // creators: None
    .bool(false) // primary_sale_happened
    .bool(true) // is_mutable
    .u8(TOKEN_STANDARD_TAG[params.tokenStandard])
    .bytes(encodeOptionCollection(params.collection))
    .u8(0) // uses: None
    .u8(0) // collection_details: None
    .optionPubkey(params.ruleSet)
    .optionU8(0) // decimals: Some(0)
    .u8(1) // print_supply: Some(...)
    .u8(0) // PrintSupply::Zero
    .build();
}

/** `MintV1InstructionArgs`: `amount`, `authorization_data: None` (no rule-set challenge supplied). */
export function encodeMintV1Args(amount: bigint): Buffer {
  return new Borsh().u64(amount).u8(0).build();
}

// ── Minted asset shape ───────────────────────────────────────────────────────────────────────────
export type MintedAsset = {
  mint: Address;
  metadata: Address;
  /** The PDA `MasterEdition::find_pda(mint)` computes — present even when nothing was ever
   * created there (see `mintNoMasterEditionPnft`), since `deposit`'s account slot still needs an
   * address to pass. */
  masterEdition: Address;
  tokenAccount: Address;
};

/**
 * `CreateV1` + `MintV1` for one asset: a fresh mint (co-signed via `addSignersToInstruction`,
 * matching `weight-index.ts`'s established pattern for a plain-keypair account this file's own
 * `signer()`/`writableSigner()` role helpers can't carry a signature for on their own), an
 * optional master edition, and 1 unit minted into `owner`'s ATA (created by `MintV1` itself via
 * its own `spl_ata_program` account — no separate ATA-creation step).
 */
async function mintOneAsset(params: {
  connection: Connection;
  payer: TransactionSigner;
  authority: TransactionSigner;
  owner: Address;
  name: string;
  tokenStandard: TokenStandardName;
  collection: { key: Address; verified: boolean } | null;
  ruleSet: Address | null;
  createMasterEdition: boolean;
}): Promise<MintedAsset> {
  const { connection, payer, authority, owner, name, tokenStandard, collection, ruleSet, createMasterEdition } =
    params;
  const isProgrammable = tokenStandard === "ProgrammableNonFungible";

  const mintSigner = await generateKeyPairSigner();
  const mint = mintSigner.address;
  const metadata = await metadataPda(mint);
  const masterEdition = await masterEditionPda(mint);
  const tokenAccount = await getTokenAccountAddress(owner, mint, false);

  const createIx: Instruction = addSignersToInstruction([authority, mintSigner], {
    programAddress: TOKEN_METADATA_PROGRAM,
    accounts: [
      writable(metadata),
      createMasterEdition ? writable(masterEdition) : readonly(TOKEN_METADATA_PROGRAM),
      writableSigner(mint),
      signer(authority.address),
      writableSigner(payer.address),
      signer(authority.address),
      readonly(SYSTEM_PROGRAM),
      readonly(SYSVAR_INSTRUCTIONS),
      readonly(TOKEN_PROGRAM),
    ],
    data: new Uint8Array(
      mplIxData(
        CREATE_V1_DISCRIMINATOR,
        encodeCreateV1Args({
          name,
          tokenStandard,
          collection,
          ruleSet: isProgrammable ? ruleSet : null,
        }),
      ),
    ),
  });
  await sendIxs(connection, payer, [createIx]);

  const mintIx: Instruction = addSignersToInstruction([authority, payer], {
    programAddress: TOKEN_METADATA_PROGRAM,
    accounts: [
      writable(tokenAccount),
      readonly(owner),
      readonly(metadata),
      createMasterEdition ? writable(masterEdition) : readonly(TOKEN_METADATA_PROGRAM),
      isProgrammable
        ? writable(await tokenRecordPda(mint, tokenAccount))
        : readonly(TOKEN_METADATA_PROGRAM),
      writable(mint),
      signer(authority.address),
      readonly(TOKEN_METADATA_PROGRAM), // delegate_record: none
      writableSigner(payer.address),
      readonly(SYSTEM_PROGRAM),
      readonly(SYSVAR_INSTRUCTIONS),
      readonly(TOKEN_PROGRAM),
      readonly(ASSOCIATED_TOKEN_PROGRAM),
      isProgrammable ? readonly(AUTH_RULES_PROGRAM) : readonly(TOKEN_METADATA_PROGRAM),
      isProgrammable && ruleSet ? readonly(ruleSet) : readonly(TOKEN_METADATA_PROGRAM),
    ],
    data: new Uint8Array(mplIxData(MINT_V1_DISCRIMINATOR, encodeMintV1Args(1n))),
  });
  await sendIxs(connection, payer, [mintIx]);

  return { mint, metadata, masterEdition, tokenAccount };
}

// ── Collection ───────────────────────────────────────────────────────────────────────────────────
export type PnftCollection = {
  mint: Address;
  metadata: Address;
  masterEdition: Address;
  /** The collection's own update authority — must co-sign every `verifyCollection` call against it. */
  authority: TransactionSigner;
};

/** Mints a plain `NonFungible` asset with its own master edition, to stand in as a collection. */
export async function createPnftCollection(params: {
  connection: Connection;
  payer: TransactionSigner;
  authority: TransactionSigner;
  name?: string;
}): Promise<PnftCollection> {
  const { connection, payer, authority, name = "bye.fun T-47 fixture collection" } = params;
  const asset = await mintOneAsset({
    connection,
    payer,
    authority,
    owner: authority.address,
    name,
    tokenStandard: "NonFungible",
    collection: null,
    ruleSet: null,
    createMasterEdition: true,
  });
  return { mint: asset.mint, metadata: asset.metadata, masterEdition: asset.masterEdition, authority };
}

/** `VerifyCollectionV1` — flips `itemMetadata`'s `collection.verified` to `true`. */
async function verifyCollection(params: {
  connection: Connection;
  payer: TransactionSigner;
  itemMetadata: Address;
  collection: PnftCollection;
}): Promise<void> {
  const { connection, payer, itemMetadata, collection } = params;
  const ix: Instruction = addSignersToInstruction([collection.authority], {
    programAddress: TOKEN_METADATA_PROGRAM,
    accounts: [
      signer(collection.authority.address),
      readonly(TOKEN_METADATA_PROGRAM), // delegate_record: none
      writable(itemMetadata),
      readonly(collection.mint),
      writable(collection.metadata),
      readonly(collection.masterEdition),
      readonly(SYSTEM_PROGRAM),
      readonly(SYSVAR_INSTRUCTIONS),
    ],
    data: new Uint8Array(mplIxData(VERIFY_COLLECTION_V1_DISCRIMINATOR)),
  });
  await sendIxs(connection, payer, [ix]);
}

// ── The admissible fixture (TA-9, TA-1's tie-break data, TA-12) ────────────────────────────────
// `collection.verified == true`, `ProgrammableNFT`, `MasterEditionV2`, rule set `eBJLFY…zyT9`.
export type AdmissibleFixture = MintedAsset & { value: bigint; label: string };

/**
 * One admissible fixture at a given intended `value`. `value` is not encoded on-chain anywhere —
 * `deposit` records zero value at deposit time (deposit.rs:67-69) and the real value is whatever a
 * later `approve_deposit`/`record_value` call is given — so `value` here is the caller's plan for
 * what to record this position at, carried alongside the mint so a TA-9/TA-1/TA-12 test doesn't
 * have to invent its own bookkeeping for which mint means what.
 */
export async function mintAdmissiblePnft(params: {
  connection: Connection;
  payer: TransactionSigner;
  authority: TransactionSigner;
  owner: Address;
  collection: PnftCollection;
  value: bigint;
  label?: string;
  ruleSet?: Address;
}): Promise<AdmissibleFixture> {
  const {
    connection,
    payer,
    authority,
    owner,
    collection,
    value,
    label = `admissible-${value}`,
    ruleSet = RULE_SET_ADDRESS,
  } = params;
  // The template is 24 bytes, so `label` has 8 to spend against `MAX_NAME_LENGTH`. Stated here
  // because the constraint is otherwise invisible at the call site — every existing caller happens
  // to be under it, which is not the same as knowing about it.
  const name = `bye.fun T-47 admissible ${label}`;
  assertNameFits(name, `mintAdmissiblePnft(label: "${label}")`);
  const asset = await mintOneAsset({
    connection,
    payer,
    authority,
    owner,
    name,
    tokenStandard: "ProgrammableNonFungible",
    collection: { key: collection.mint, verified: false },
    ruleSet,
    createMasterEdition: true,
  });
  await verifyCollection({ connection, payer, itemMetadata: asset.metadata, collection });
  return { ...asset, value, label };
}

/**
 * One admissible fixture per entry in `values`, in order. Passing the same value twice — e.g.
 * `[floor, tier, tie, tie]` — is how a caller realises the identical-value tie-break pair (F-1):
 * two admissible positions differing only in `activated_at`/`position_id`, which `mintOneAsset`'s
 * fresh-mint-per-call already guarantees (each gets its own position, deposited/approved at its
 * own later moment) without this file needing to know anything about `activated_at` itself.
 */
export async function mintAdmissiblePnfts(params: {
  connection: Connection;
  payer: TransactionSigner;
  authority: TransactionSigner;
  owner: Address;
  collection: PnftCollection;
  values: bigint[];
  ruleSet?: Address;
}): Promise<AdmissibleFixture[]> {
  const { connection, payer, authority, owner, collection, values, ruleSet } = params;
  const fixtures: AdmissibleFixture[] = [];
  for (let i = 0; i < values.length; i++) {
    fixtures.push(
      await mintAdmissiblePnft({
        connection,
        payer,
        authority,
        owner,
        collection,
        value: values[i],
        label: `v${i}`,
        ruleSet,
      }),
    );
  }
  return fixtures;
}

// ── The 5 rejection fixtures — each isolates exactly one deposit.rs admission guard ────────────
// Every one of these reaches `transfer_pnft`'s `TransferV1` CPI never — `validate_admission` runs
// (and rejects) before it in `deposit.rs`, so none of them needs a delegate, a locked token state,
// or any rule-set enforcement to be a faithful fixture for that rejection path.

/** Right standard, right master edition, verified against a *different* collection — isolates
 * `check_collection`'s key-equality branch (6100), never its `verified` branch. */
export async function mintWrongCollectionPnft(params: {
  connection: Connection;
  payer: TransactionSigner;
  authority: TransactionSigner;
  owner: Address;
  wrongCollection: PnftCollection;
  ruleSet?: Address;
}): Promise<MintedAsset> {
  const { connection, payer, authority, owner, wrongCollection, ruleSet = RULE_SET_ADDRESS } = params;
  const asset = await mintOneAsset({
    connection,
    payer,
    authority,
    owner,
    name: "T-47: wrong-collection",
    tokenStandard: "ProgrammableNonFungible",
    collection: { key: wrongCollection.mint, verified: false },
    ruleSet,
    createMasterEdition: true,
  });
  await verifyCollection({ connection, payer, itemMetadata: asset.metadata, collection: wrongCollection });
  return asset;
}

/** Right collection key, deliberately never verified — isolates `check_collection`'s `verified`
 * branch (6100), never its key-equality branch. */
export async function mintUnverifiedCollectionPnft(params: {
  connection: Connection;
  payer: TransactionSigner;
  authority: TransactionSigner;
  owner: Address;
  collection: PnftCollection;
  ruleSet?: Address;
}): Promise<MintedAsset> {
  const { connection, payer, authority, owner, collection, ruleSet = RULE_SET_ADDRESS } = params;
  return mintOneAsset({
    connection,
    payer,
    authority,
    owner,
    name: "T-47: unverified-collection",
    tokenStandard: "ProgrammableNonFungible",
    collection: { key: collection.mint, verified: false },
    ruleSet,
    createMasterEdition: true,
  });
}

/** Verified, right collection, real `MasterEditionV2` — but minted `NonFungible`, never
 * `ProgrammableNonFungible` — isolates `check_standard` (6101), never the collection or
 * master-edition checks either side of it. */
export async function mintPlainNonFungiblePnft(params: {
  connection: Connection;
  payer: TransactionSigner;
  authority: TransactionSigner;
  owner: Address;
  collection: PnftCollection;
}): Promise<MintedAsset> {
  const { connection, payer, authority, owner, collection } = params;
  const asset = await mintOneAsset({
    connection,
    payer,
    authority,
    owner,
    name: "T-47: plain-non-fungible",
    tokenStandard: "NonFungible",
    collection: { key: collection.mint, verified: false },
    ruleSet: null,
    createMasterEdition: true,
  });
  await verifyCollection({ connection, payer, itemMetadata: asset.metadata, collection });
  return asset;
}

/**
 * Verified, right collection, right standard, real `MasterEditionV2` on the item's own mint — but
 * the `masterEdition` this returns is `collection.masterEdition`, a real, valid `MasterEditionV2`
 * that belongs to a *different* mint. Isolates the master-edition PDA-key check (6101) — the exact
 * scenario escrow.rs's own `validate_admission_rejects_a_master_edition_that_is_not_this_mints_pda`
 * unit test covers — never `check_standard` or `check_collection`.
 *
 * This file's other two literal readings of the row ("no master edition" / "MasterEditionV1") were
 * tried against this validator and are **not achievable via real CPI here**: `CreateV1` refuses to
 * create `ProgrammableNonFungible` metadata without a master edition (observed: custom program
 * error 167, "Missing master edition account" — mpl-token-metadata 5.1.1's own error table), and
 * the deployed program has no live instruction that creates a legacy `MasterEditionV1` (only
 * `MasterEditionV2` is ever constructed by `CreateV1`/`create_master_edition_v3`). Both are
 * genuine on-chain constraints, not shortcuts taken to avoid the work — see this file's own
 * verification notes in the T-47 handoff.
 */
export async function mintWrongMasterEditionPnft(params: {
  connection: Connection;
  payer: TransactionSigner;
  authority: TransactionSigner;
  owner: Address;
  collection: PnftCollection;
  ruleSet?: Address;
}): Promise<MintedAsset> {
  const { connection, payer, authority, owner, collection, ruleSet = RULE_SET_ADDRESS } = params;
  const asset = await mintOneAsset({
    connection,
    payer,
    authority,
    owner,
    name: "T-47: wrong-master-edition",
    tokenStandard: "ProgrammableNonFungible",
    collection: { key: collection.mint, verified: false },
    ruleSet,
    createMasterEdition: true,
  });
  await verifyCollection({ connection, payer, itemMetadata: asset.metadata, collection });
  return { ...asset, masterEdition: collection.masterEdition };
}

/**
 * Stands in for an MPL-Core asset. **Not a genuine MPL-Core account**: T-46 clones Token Metadata
 * and Token Auth Rules only, so the real MPL-Core program is not loaded on this validator and
 * cannot be invoked to mint one. What this builds instead is a real, on-chain account — a fresh
 * keypair, created via a plain `SystemProgram::CreateAccount` — whose `owner` field is set to
 * MPL-Core's real program id. `validate_admission`'s first check is exactly
 * `metadata_info.owner == TOKEN_METADATA_ID` (escrow.rs:105-109), which this genuinely fails the
 * same way a real MPL-Core asset would (own-program ownership is all that check inspects; it never
 * parses the account's bytes), and account ownership assignment is a System Program privilege that
 * does not require the target owner's program to exist or be executable. `nft_mint`/`depositor_token`
 * are a real, ordinary SPL Token mint — MPL-Core assets carry no such Metaplex apparatus, but
 * `Deposit`'s own `nft_mint: Account<'info, Mint>` constraint requires *some* valid SPL mint
 * regardless of which admission guard the fixture is built to trip.
 */
export async function mintMplCoreLikeAsset(params: {
  connection: Connection;
  payer: TransactionSigner;
  owner: Address;
}): Promise<MintedAsset> {
  const { connection, payer, owner } = params;
  const mint = await connection.createTokenMint({
    mintAuthority: payer,
    decimals: 0,
    useTokenExtensions: false,
  });
  await connection.mintTokens(mint, payer, 1n, owner, false);
  const tokenAccount = await getTokenAccountAddress(owner, mint, false);

  const assetSigner = await generateKeyPairSigner();
  const space = 100n;
  const lamports = await connection.rpc.getMinimumBalanceForRentExemption(space).send();
  const createAssetIx: Instruction = addSignersToInstruction([payer, assetSigner], {
    programAddress: SYSTEM_PROGRAM,
    accounts: [writableSigner(payer.address), writableSigner(assetSigner.address)],
    data: new Uint8Array(
      new Borsh().u32(0).u64(lamports).u64(space).pubkey(MPL_CORE_PROGRAM_ID).build(),
    ),
  });
  await sendIxs(connection, payer, [createAssetIx]);

  return {
    mint,
    metadata: assetSigner.address,
    masterEdition: await masterEditionPda(mint),
    tokenAccount,
  };
}
