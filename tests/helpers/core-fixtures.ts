// MPL Core fixture helpers (T-105).
//
// Creates **genuine** `AssetV1` and `CollectionV1` accounts by calling the real MPL Core program,
// which `run-integration-tests.sh` loads from `tests/fixtures/programs/mpl-core.so`. That
// distinction is the whole point of this file. `fixtures.ts`'s `mintMplCoreLikeAsset` builds an
// account with the system program and *assigns* it to MPL Core: it proves an owner check and
// nothing else — its data is 100 zero bytes, so it never parsed as an `AssetV1` and could never
// have been transferred. T-105's row rules a fabricated account out by name, because the question
// it exists to answer is what the MPL Core *program* does, and a hand-rolled account cannot
// answer that.
//
// Instruction encoding is hand-rolled, exactly as `fixtures.ts` hand-rolls Token Metadata's:
// this repo carries no Metaplex SDK. Layouts are read off the `mpl-core 0.11.1` Rust crate that
// `programs/bye_machine` itself depends on — `generated/instructions/{create_v1,
// create_collection_v1,transfer_v1,update_plugin_v1,burn_v1}.rs` for the account orders and
// discriminators, `generated/types/plugin.rs` for the plugin tags, and
// `generated/accounts/base_asset_v1.rs` for the decode below.
//
// **Absent optional accounts are the MPL Core program id**, not omitted: kinobi builds a
// fixed-length account list and pushes `MPL_CORE_ID` readonly where an `Option` is `None`
// (`create_collection_v1.rs:44`). Omitting the slot instead shifts every later account by one,
// which is a silent mis-parse rather than an error.

import {
  addSignersToInstruction,
  generateKeyPairSigner,
  type Address,
  type Instruction,
  type TransactionSigner,
} from "@solana/kit";
import type { Connection } from "./env.js";
import { address } from "./env.js";
import { Borsh, readonly, signer, writable, writableSigner } from "../../scripts/lib/anchor.js";
import { fetchAccountData, sendIxs } from "../../scripts/_common.js";
import { MPL_CORE_PROGRAM, SYSTEM_PROGRAM } from "../../scripts/constants.js";

// Re-exported, not redeclared: `deposit_core`'s encoder needs the same id, and a program address
// with two homes is a program address that can disagree with itself.
export { MPL_CORE_PROGRAM };

// Outer instruction discriminators — a single byte, unlike Token Metadata's two.
export const CREATE_V1_DISCRIMINATOR = 0;
export const CREATE_COLLECTION_V1_DISCRIMINATOR = 1;
export const UPDATE_PLUGIN_V1_DISCRIMINATOR = 6;
/** `UpdateV1` — the instruction that rewrites `AssetV1.update_authority`, and so the only way a
 * collection authority takes an asset out of its collection. */
export const UPDATE_V1_DISCRIMINATOR = 15;
/** `UpdateCollectionPluginV1` — the collection's registry, which is a different instruction from
 * the asset's and a different account list. A `permanent_freeze_delegate` on a collection freezes
 * every asset in it, and this is how that flag is flipped. */
export const UPDATE_COLLECTION_PLUGIN_V1_DISCRIMINATOR = 7;
export const BURN_V1_DISCRIMINATOR = 12;
export const TRANSFER_V1_DISCRIMINATOR = 14;

/** `generated/types/key.rs` — the account discriminator, declaration order. */
export const CORE_KEY = {
  Uninitialized: 0,
  AssetV1: 1,
  HashedAssetV1: 2,
  PluginHeaderV1: 3,
  PluginRegistryV1: 4,
  CollectionV1: 5,
} as const;

/**
 * `generated/types/plugin.rs` — the `Plugin` enum's own declaration order, which is the wire tag.
 * Only the variants this file mints are named: the remaining thirteen are one line each when a
 * row needs them, and a table of unused tags is a table nothing checks.
 *
 * **The three named here are the three causes of AC-84**, and they are not adjacent in the enum:
 * `PermanentFreezeDelegate` sits at 5, two variants before `PermanentTransferDelegate`, with
 * `PermanentBurnDelegate` immediately after it at 8. Counting from the permanent group rather
 * than from the enum's own first variant is how a reader arrives at the wrong three numbers.
 */
export const PLUGIN_TAG = {
  /** The enum's **first** variant, and the one that carries a rule set. */
  Royalties: 0,
  PermanentFreezeDelegate: 5,
  PermanentTransferDelegate: 7,
  PermanentBurnDelegate: 8,
} as const;

/**
 * `generated/types/rule_set.rs` — declaration order. `ProgramDenyList` names *programs*, and MPL
 * Core compares them against the owning program of the transfer's authority and of its new owner,
 * so a list holding the **system program** rejects every ordinary wallet-to-wallet move — which
 * is exactly what an escrow release is, since a vault PDA with no account reads as system-owned
 * too.
 */
export const RULE_SET_TAG = {
  None: 0,
  ProgramAllowList: 1,
  ProgramDenyList: 2,
} as const;

/**
 * `generated/types/plugin_authority.rs` — declaration order, and **not** the order a reader
 * guesses: `None` is 0 and `Owner` is 1, so an authority encoded from intuition names the wrong
 * party rather than failing to parse.
 */
export const PLUGIN_AUTHORITY_TAG = {
  None: 0,
  Owner: 1,
  UpdateAuthority: 2,
  Address: 3,
} as const;

/**
 * A permanent plugin, held by an address that is neither the asset's owner nor this program.
 *
 * **`PermanentTransferDelegate` is what makes a seizure constructible at all.** Once a card is
 * escrowed, `AssetV1.owner` is `PDA(["vault", position])`, and that PDA signs only through
 * `bye_machine` — so nothing a test can sign moves the card out from under it. A permanent
 * transfer delegate can, which is precisely design §3.4's third-party hazard: `close_seized`
 * exists because this plugin does.
 *
 * Permanent plugins are addable **only at creation**, which is why this travels through
 * `mintCoreAsset` rather than through an `AddPluginV1` after the fact.
 *
 * The authority is an explicit `Address` rather than a `None` that MPL Core would default to the
 * update authority: the party that reaches in must be nameable in an assertion, and it must not
 * be the collection authority the suite already signs with elsewhere.
 */
export type CorePlugin =
  | { kind: "permanentTransferDelegate"; authority: Address }
  | { kind: "permanentBurnDelegate"; authority: Address }
  /**
   * **Minted `frozen: false` and set afterwards, never `frozen: true` at creation** (T-110).
   * `deposit_core` escrows by `TransferV1`, which MPL Core refuses on a frozen asset, so an
   * asset born frozen cannot reach a vault and the position the freeze is supposed to strand is
   * never opened. The freeze therefore has to arrive *after* escrow, through
   * [`setPermanentFreeze`] — which is also what the cause is: a third party reaching into a card
   * that is already collateral.
   */
  | { kind: "permanentFreezeDelegate"; authority: Address; frozen: boolean }
  /**
   * **The plugin the program cannot read, which is the whole reason this variant exists.**
   * `Royalties` gates the Transfer lifecycle through its `rule_set`, and it is authority-managed
   * — so a third-party collection authority can put it on the collection and point it at a deny
   * list that refuses the escrow release. Nothing in `core_asset.rs` sees it: the freeze read
   * asks for `PermanentFreezeDelegate` and `FreezeDelegate` by plugin type, so the card reads
   * *settleable* while MPL Core will not move it. `Oracle` and `LifecycleHook` external adapters
   * are worse still — they are not even in the registry `fetch_plugin` walks.
   *
   * Created with `ruleSet: "none"` and pointed at a deny list afterwards, through
   * [`setCollectionRoyaltiesRuleSet`], for the same reason the freeze is: a rule set that refuses
   * transfers refuses `deposit_core`'s own, so a collection born with one can never hold an
   * escrowed card at all.
   *
   * `creator` takes the whole 100% — MPL Core's own `validate_create` rejects a creator list
   * whose percentages do not sum to 100.
   */
  | {
      kind: "royalties";
      authority: Address;
      basisPoints: number;
      creator: Address;
      ruleSet: "none" | { denyList: readonly Address[] };
    };

/** `RuleSet`, as the enum is encoded: the variant tag, then a `Vec<Pubkey>` on the two list arms. */
function encodeRuleSet(out: Borsh, ruleSet: "none" | { denyList: readonly Address[] }): void {
  if (ruleSet === "none") {
    out.u8(RULE_SET_TAG.None);
    return;
  }
  out.u8(RULE_SET_TAG.ProgramDenyList).u32(ruleSet.denyList.length);
  for (const program of ruleSet.denyList) out.pubkey(program);
}

/**
 * One `Plugin`, as the enum is encoded on the wire: the variant tag, then that variant's own
 * fields. Two of the three are fieldless structs, where the tag is the whole payload;
 * `PermanentFreezeDelegate` carries a `frozen: bool` and so is one byte longer.
 */
function encodePluginBody(out: Borsh, plugin: CorePlugin): void {
  switch (plugin.kind) {
    case "permanentTransferDelegate":
      out.u8(PLUGIN_TAG.PermanentTransferDelegate);
      return;
    case "permanentBurnDelegate":
      out.u8(PLUGIN_TAG.PermanentBurnDelegate);
      return;
    case "permanentFreezeDelegate":
      out.u8(PLUGIN_TAG.PermanentFreezeDelegate).bool(plugin.frozen);
      return;
    case "royalties":
      out
        .u8(PLUGIN_TAG.Royalties)
        .u16(plugin.basisPoints)
        // creators: Vec<Creator>, each `{ address: Pubkey, percentage: u8 }`.
        .u32(1)
        .pubkey(plugin.creator)
        .u8(100);
      encodeRuleSet(out, plugin.ruleSet);
      return;
  }
}

/** `Option<Vec<PluginAuthorityPair>>`, as `CreateV1`'s fourth argument expects it. */
function encodePlugins(plugins: readonly CorePlugin[]): Buffer {
  if (plugins.length === 0) return new Borsh().u8(0).build();
  const out = new Borsh().u8(1).u32(plugins.length);
  for (const plugin of plugins) {
    encodePluginBody(out, plugin);
    out.u8(1).u8(PLUGIN_AUTHORITY_TAG.Address).pubkey(plugin.authority);
  }
  return out.build();
}

/** `generated/types/update_authority.rs` — the enum tag `BaseAssetV1.update_authority` carries. */
export const UPDATE_AUTHORITY_TAG = {
  None: 0,
  Address: 1,
  Collection: 2,
} as const;

export type CoreAsset = {
  key: number;
  owner: Address;
  updateAuthorityTag: number;
  /** The pubkey the `Address` and `Collection` arms carry; `null` on `None`. */
  updateAuthority: Address | null;
  name: string;
  uri: string;
};

/**
 * `BaseAssetV1`, decoded off the live account.
 *
 * Returns `null` when the account does not exist — the state a burn leaves, and one the caller
 * has to be able to tell apart from a transfer.
 */
export async function readCoreAsset(
  connection: Connection,
  asset: Address,
): Promise<CoreAsset | null> {
  const data = await fetchAccountData(connection, asset);
  if (data === null) return null;

  let offset = 0;
  const key = data.readUInt8(offset);
  offset += 1;
  const owner = base58(data.subarray(offset, offset + 32));
  offset += 32;
  const updateAuthorityTag = data.readUInt8(offset);
  offset += 1;
  let updateAuthority: Address | null = null;
  if (updateAuthorityTag !== UPDATE_AUTHORITY_TAG.None) {
    updateAuthority = base58(data.subarray(offset, offset + 32));
    offset += 32;
  }
  const nameLen = data.readUInt32LE(offset);
  offset += 4;
  const name = data.subarray(offset, offset + nameLen).toString("utf8");
  offset += nameLen;
  const uriLen = data.readUInt32LE(offset);
  offset += 4;
  const uri = data.subarray(offset, offset + uriLen).toString("utf8");

  return { key, owner, updateAuthorityTag, updateAuthority, name, uri };
}

// `Borsh.pubkey` encodes an Address; the decode direction has no helper in anchor.ts, and
// decoders.ts's BorshReader is scoped to this program's own accounts.
function base58(bytes: Buffer): Address {
  const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let value = 0n;
  for (const byte of bytes) value = (value << 8n) | BigInt(byte);
  let out = "";
  while (value > 0n) {
    out = ALPHABET[Number(value % 58n)] + out;
    value /= 58n;
  }
  for (const byte of bytes) {
    if (byte !== 0) break;
    out = "1" + out;
  }
  return address(out);
}

/**
 * A real `CollectionV1`, created by MPL Core.
 *
 * `updateAuthority` is passed **explicitly** rather than left to MPL Core's default, and the
 * signer is returned with the address because `CreateV1` needs it to sign whenever an asset joins
 * the collection. Measured, not assumed: the collection keypair is *not* the update authority —
 * unstated, MPL Core defaults it to the **payer**, and signing the mint with the collection
 * keypair returns `NoApprovals` (26).
 */
export async function createCoreCollection(params: {
  connection: Connection;
  payer: TransactionSigner;
  updateAuthority?: TransactionSigner;
  name?: string;
  /**
   * Plugins on the **collection**, which is a different registry from an asset's and applies to
   * every asset in it. `permanent_freeze_delegate` is authority-managed, so it lives here as
   * readily as on a card — and on the Core collection this pool admits, design §3.4 records it
   * living exactly here, held by a third party. Passed at creation for the same reason an
   * asset's permanent plugins are: MPL Core will not add one afterwards. Flip the flag with
   * [`setCollectionPermanentFreeze`].
   */
  plugins?: readonly CorePlugin[];
}): Promise<{ collection: Address; authority: TransactionSigner }> {
  const {
    connection,
    payer,
    updateAuthority = payer,
    name = "bye-core-collection",
    plugins = [],
  } = params;
  const collection = await generateKeyPairSigner();

  const data = new Borsh()
    .u8(CREATE_COLLECTION_V1_DISCRIMINATOR)
    .string(name)
    .string("https://example.invalid/collection.json")
    .bytes(encodePlugins(plugins))
    .build();

  const ix: Instruction = addSignersToInstruction([payer, collection], {
    programAddress: MPL_CORE_PROGRAM,
    accounts: [
      writableSigner(collection.address),
      readonly(updateAuthority.address),
      writableSigner(payer.address),
      readonly(SYSTEM_PROGRAM),
    ],
    data: new Uint8Array(data),
  });
  await sendIxs(connection, payer, [ix]);
  return { collection: collection.address, authority: updateAuthority };
}

/**
 * A real `AssetV1` inside `collection`, owned by `owner`.
 *
 * `collectionAuthority` signs as `[2] authority`: MPL Core will not let an asset join a
 * collection without the collection's update authority, which is what makes
 * `UpdateAuthority::Collection` a claim the program itself vouches for rather than one the
 * depositor asserts — and therefore what `deposit_core` is entitled to derive a `PoolCollection`
 * record from.
 */
export async function mintCoreAsset(params: {
  connection: Connection;
  payer: TransactionSigner;
  collection: Address;
  collectionAuthority: TransactionSigner;
  owner: Address;
  name?: string;
  /** Permanent plugins, which MPL Core accepts at creation and never afterwards. */
  plugins?: readonly CorePlugin[];
}): Promise<Address> {
  const {
    connection,
    payer,
    collection,
    collectionAuthority,
    owner,
    name = "bye-core-asset",
    plugins = [],
  } = params;
  const asset = await generateKeyPairSigner();

  const data = new Borsh()
    .u8(CREATE_V1_DISCRIMINATOR)
    .u8(0) // data_state: DataState::AccountState
    .string(name)
    .string("https://example.invalid/asset.json")
    .bytes(encodePlugins(plugins)) // plugins: Option<Vec<PluginAuthorityPair>>
    .build();

  const ix: Instruction = addSignersToInstruction([payer, asset, collectionAuthority], {
    programAddress: MPL_CORE_PROGRAM,
    accounts: [
      writableSigner(asset.address),
      writable(collection),
      signer(collectionAuthority.address),
      writableSigner(payer.address),
      readonly(owner),
      readonly(MPL_CORE_PROGRAM), // update_authority: None → derived from the collection
      readonly(SYSTEM_PROGRAM),
      readonly(MPL_CORE_PROGRAM), // log_wrapper: None
    ],
    data: new Uint8Array(data),
  });
  await sendIxs(connection, payer, [ix]);
  return asset.address;
}

/**
 * MPL Core `TransferV1`, called directly rather than through `bye_machine`.
 *
 * Direct on purpose: T-105 has to establish what the *dependency* does before a handler is
 * written around it, and routing the question through our own program would leave any failure
 * ambiguous between the two.
 */
export async function transferCoreAsset(params: {
  connection: Connection;
  payer: TransactionSigner;
  asset: Address;
  collection: Address;
  authority: TransactionSigner;
  newOwner: Address;
}): Promise<void> {
  const { connection, payer, asset, collection, authority, newOwner } = params;

  const data = new Borsh()
    .u8(TRANSFER_V1_DISCRIMINATOR)
    .u8(0) // compression_proof: Option<CompressionProof> = None
    .build();

  const ix: Instruction = addSignersToInstruction([payer, authority], {
    programAddress: MPL_CORE_PROGRAM,
    accounts: [
      writable(asset),
      readonly(collection),
      writableSigner(payer.address),
      signer(authority.address),
      readonly(newOwner),
      readonly(SYSTEM_PROGRAM),
      readonly(MPL_CORE_PROGRAM), // log_wrapper: None
    ],
    data: new Uint8Array(data),
  });
  await sendIxs(connection, payer, [ix]);
}

/**
 * MPL Core `BurnV1`, signed by the permanent burn delegate — AC-84's second cause.
 *
 * The account order is `TransferV1`'s with the `new_owner` slot removed, which is the one
 * difference and the one a copy of the transfer encoder would keep. `payer` receives the asset's
 * rent, and it is deliberately the delegate rather than the depositor: what a burn leaves behind
 * has to be attributable to the party that reached in.
 *
 * What this leaves for `read_collateral` is **not** an `AssetV1` that says "burned" — MPL Core
 * deallocates the account, so the program is handed an account the runtime supplies with no data
 * and the system program as owner. That is the first of `read_collateral`'s four checks and the
 * reason `SeizureKind::Burned` carries `Pubkey::default()` as its observed owner: there is
 * nobody holding the card.
 */
export async function burnCoreAsset(params: {
  connection: Connection;
  payer: TransactionSigner;
  asset: Address;
  collection: Address;
  authority: TransactionSigner;
}): Promise<void> {
  const { connection, payer, asset, collection, authority } = params;

  const data = new Borsh()
    .u8(BURN_V1_DISCRIMINATOR)
    .u8(0) // compression_proof: Option<CompressionProof> = None
    .build();

  const ix: Instruction = addSignersToInstruction([payer, authority], {
    programAddress: MPL_CORE_PROGRAM,
    accounts: [
      writable(asset),
      writable(collection),
      writableSigner(payer.address),
      signer(authority.address),
      readonly(SYSTEM_PROGRAM),
      readonly(MPL_CORE_PROGRAM), // log_wrapper: None
    ],
    data: new Uint8Array(data),
  });
  await sendIxs(connection, payer, [ix]);
}

/**
 * MPL Core `UpdatePluginV1`, flipping an already-attached `PermanentFreezeDelegate` — AC-84's
 * third cause, and the only one of the three that is applied **after** escrow rather than being
 * an act on the card itself.
 *
 * **It has to be two steps, and the reason is a deadlock rather than a style choice.** The plugin
 * is permanent, so it can only be attached at creation; but an asset created already `frozen`
 * cannot be transferred, and `deposit_core` escrows by `TransferV1` — so a card born frozen never
 * becomes collateral and there is no position for a freeze to strand. Mint `frozen: false`,
 * deposit, then freeze.
 *
 * The same call runs in reverse. AC-84 requires the claim to *succeed* on this cause once the
 * asset is released, because a freeze is reversible by the party that set it — which is what
 * separates it from the burn.
 */
export async function setPermanentFreeze(params: {
  connection: Connection;
  payer: TransactionSigner;
  asset: Address;
  collection: Address;
  authority: TransactionSigner;
  frozen: boolean;
}): Promise<void> {
  const { connection, payer, asset, collection, authority, frozen } = params;

  const data = new Borsh()
    .u8(UPDATE_PLUGIN_V1_DISCRIMINATOR)
    // plugin: Plugin — the whole enum, not a field patch. `UpdatePluginV1` replaces the record,
    // so the variant tag travels with the new `frozen` byte.
    .u8(PLUGIN_TAG.PermanentFreezeDelegate)
    .bool(frozen)
    .build();

  const ix: Instruction = addSignersToInstruction([payer, authority], {
    programAddress: MPL_CORE_PROGRAM,
    accounts: [
      writable(asset),
      writable(collection),
      writableSigner(payer.address),
      signer(authority.address),
      readonly(SYSTEM_PROGRAM),
      readonly(MPL_CORE_PROGRAM), // log_wrapper: None
    ],
    data: new Uint8Array(data),
  });
  await sendIxs(connection, payer, [ix]);
}

/**
 * Flips the **collection's** `permanent_freeze_delegate`, which freezes or thaws every asset in
 * it at once.
 *
 * `UpdateCollectionPluginV1` (disc 7), not `UpdatePluginV1` (disc 6): a collection's plugin
 * registry is its own account's, and the instruction that writes it takes a different account
 * list — no asset slot at all. Mixing the two is the fixture-level version of the defect this
 * helper exists to test for, which is a freeze read that only ever looks at the asset.
 *
 * The plugin must already be on the collection, added at creation through
 * [`createCoreCollection`]'s `plugins` — MPL Core will not add a permanent one afterwards. Like
 * the asset-level freeze, this runs in both directions: a thaw is what makes the depositor's
 * claim succeed afterwards, which is why the cause counts as reversible.
 */
export async function setCollectionPermanentFreeze(params: {
  connection: Connection;
  payer: TransactionSigner;
  collection: Address;
  authority: TransactionSigner;
  frozen: boolean;
}): Promise<void> {
  const { connection, payer, collection, authority, frozen } = params;

  const data = new Borsh()
    .u8(UPDATE_COLLECTION_PLUGIN_V1_DISCRIMINATOR)
    // plugin: Plugin — the whole enum. `UpdateCollectionPluginV1` replaces the record, so the
    // variant tag travels with the new `frozen` byte.
    .u8(PLUGIN_TAG.PermanentFreezeDelegate)
    .bool(frozen)
    .build();

  const ix: Instruction = addSignersToInstruction([payer, authority], {
    programAddress: MPL_CORE_PROGRAM,
    accounts: [
      writable(collection),
      writableSigner(payer.address),
      signer(authority.address),
      readonly(SYSTEM_PROGRAM),
      readonly(MPL_CORE_PROGRAM), // log_wrapper: None
    ],
    data: new Uint8Array(data),
  });
  await sendIxs(connection, payer, [ix]);
}

/**
 * `UpdateCollectionPluginV1` pointing the collection's `Royalties` rule set at a program deny
 * list, or back at `None` — **the blocker the program's collateral read cannot see.**
 *
 * The freeze read asks MPL Core for `PermanentFreezeDelegate` and `FreezeDelegate` by plugin
 * type, so a `Royalties` plugin that vetoes the Transfer lifecycle leaves the card classified
 * *settleable* while every release of it is refused. That is the class of defect three review
 * rounds each found one instance of, and the reason `close_seized` stopped enumerating
 * mechanisms and started attempting the release: `Royalties` is one mechanism, the `Oracle` and
 * `LifecycleHook` external adapters are two more that `fetch_plugin` cannot reach at all, and
 * MPL Core can ship a fourth.
 *
 * Runs in both directions, like the freeze — a rule set put back to `None` is what makes the
 * depositor's claim succeed afterwards, which is why the cause counts as reversible.
 *
 * The plugin must already be on the collection, added at creation through
 * [`createCoreCollection`]'s `plugins`.
 */
export async function setCollectionRoyaltiesRuleSet(params: {
  connection: Connection;
  payer: TransactionSigner;
  collection: Address;
  authority: TransactionSigner;
  basisPoints: number;
  creator: Address;
  ruleSet: "none" | { denyList: readonly Address[] };
}): Promise<void> {
  const { connection, payer, collection, authority, basisPoints, creator, ruleSet } = params;

  const out = new Borsh().u8(UPDATE_COLLECTION_PLUGIN_V1_DISCRIMINATOR);
  // plugin: Plugin — the whole enum, because `UpdateCollectionPluginV1` replaces the record.
  encodePluginBody(out, { kind: "royalties", authority: authority.address, basisPoints, creator, ruleSet });

  const ix: Instruction = addSignersToInstruction([payer, authority], {
    programAddress: MPL_CORE_PROGRAM,
    accounts: [
      writable(collection),
      writableSigner(payer.address),
      signer(authority.address),
      readonly(SYSTEM_PROGRAM),
      readonly(MPL_CORE_PROGRAM), // log_wrapper: None
    ],
    data: new Uint8Array(out.build()),
  });
  await sendIxs(connection, payer, [ix]);
}

/**
 * `UpdateV1` with a new `update_authority` — the only instruction that could take an asset **out**
 * of its collection, signed by the collection's own update authority because MPL Core lets nobody
 * else touch an asset's membership.
 *
 * **It does not work, and measuring that is the point of this helper.** `mpl-core 0.11.1` answers
 * `NotAvailable` (23) to every shape of it: a new authority of `Address` or of `None`, with the
 * collection account readonly or writable. An asset that is in a collection cannot leave it — and
 * since `deposit_core` refuses an asset that declares no collection at intake, no escrowed card
 * can ever reach the `UpdateAuthority::Address | None` arm of the program's own collateral read.
 * The exits still resolve the `TransferV1` collection argument from the asset rather than
 * assuming one, because the arm costs a match and closes the stranding this measurement is the
 * only thing standing between the program and.
 *
 * `new_name`/`new_uri` are `None`: this would change membership and nothing else.
 */
export async function updateCoreAssetAuthority(params: {
  connection: Connection;
  payer: TransactionSigner;
  asset: Address;
  collection: Address;
  collectionAuthority: TransactionSigner;
  /** `null` encodes `UpdateAuthority::None`; an address encodes `UpdateAuthority::Address`. */
  newUpdateAuthority: Address | null;
  /** MPL Core's own client marks the collection readonly here; both are refused, and a case that
   * drives only one has not measured the refusal. */
  collectionWritable?: boolean;
}): Promise<void> {
  const {
    connection,
    payer,
    asset,
    collection,
    collectionAuthority,
    newUpdateAuthority,
    collectionWritable = false,
  } = params;

  const args = new Borsh()
    .u8(UPDATE_V1_DISCRIMINATOR)
    .u8(0) // new_name: Option<String> = None
    .u8(0) // new_uri: Option<String> = None
    .u8(1); // new_update_authority: Option<UpdateAuthority> = Some(..)
  if (newUpdateAuthority === null) args.u8(UPDATE_AUTHORITY_TAG.None);
  else args.u8(UPDATE_AUTHORITY_TAG.Address).pubkey(newUpdateAuthority);

  const ix: Instruction = addSignersToInstruction([payer, collectionAuthority], {
    programAddress: MPL_CORE_PROGRAM,
    accounts: [
      writable(asset),
      collectionWritable ? writable(collection) : readonly(collection),
      writableSigner(payer.address),
      signer(collectionAuthority.address),
      readonly(SYSTEM_PROGRAM),
      readonly(MPL_CORE_PROGRAM), // log_wrapper: None
    ],
    data: new Uint8Array(args.build()),
  });
  await sendIxs(connection, payer, [ix]);
}
