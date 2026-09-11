//! The MPL Core read surface — every custody predicate the `standard == 2` instructions share.
//!
//! One file because `deposit_core`, `return_rejected_core`, `withdraw_core`, `claim_nft_core` and
//! `close_seized` all read the same two accounts — the escrowed asset and its collection — for
//! the same facts, and a second copy of a custody predicate is a custody bug.
//!
//! The two "hooked" account wrappers MPL Core exposes are forbidden in this crate. Their
//! `deserialize` functions are the only callers of `registry_records_to_plugin_list`, which
//! overflows the SBF stack frame (4,224 bytes against a 4,096-byte maximum). `cargo-build-sbf`
//! reports that as an `Error:`-prefixed diagnostic but still exits 0, and the stripped release
//! `.so` carries no symbols to grep for afterward — so the ban is enforced by the whole-crate scan
//! at the bottom of this file rather than left as a convention. `BaseAssetV1::from_bytes` and
//! `fetch_plugin` reach the same data without ever building the full plugin list.

use anchor_lang::prelude::*;
use mpl_core::accounts::{BaseAssetV1, BaseCollectionV1, PluginHeaderV1};
use mpl_core::instructions::TransferV1CpiBuilder;
use mpl_core::types::{
    ExternalCheckResult, FreezeDelegate, HookableLifecycleEvent, Key as CoreKey,
    PermanentFreezeDelegate, PluginType, Royalties, RuleSet, UpdateAuthority,
};
use mpl_core::ID as MPL_CORE_ID;
use mpl_core::{fetch_plugin, DataBlob, PluginRegistryV1Safe, SolanaAccount};
use num_traits::FromPrimitive;

use crate::common::errors::ByeMachineError;
use crate::common::events::SeizureKind;
use crate::common::seeds::POSITION_VAULT_SEED;

/// One read of the escrowed asset account and its collection, classified.
///
/// The three arms answer the two questions the Core instructions ask: the exits need "can this
/// card still be moved out", `close_seized` needs "is this card beyond reach", and any
/// classification **both** refuse is a position neither can serve — the depositor cannot withdraw
/// and nobody can clean up a leaf that holds live `w_real` and is drawable. That is a pool halt,
/// not a lost NFT.
///
/// A `Transfer` can be blocked by mechanisms this program cannot enumerate exhaustively: a
/// collection-level `PermanentFreezeDelegate`, an asset detached from its collection, a
/// `Royalties { rule_set: ProgramDenyList }`, or an external `Oracle` / `LifecycleHook` adapter —
/// the last two are not even reachable through `fetch_plugin`, which only walks the internal
/// plugin registry. The set is **open**: MPL Core can add authority-managed plugins and adapters,
/// and a third-party collection authority controls them.
///
/// Attempt-and-classify — drive the release and take MPL Core's refusal as the answer — is not
/// implementable: a failed inner instruction is not recoverable by its caller.
/// `core-transfer-refused.test.ts` records MPL Core answering `0x9` to a deny-listed release and
/// the *outer* program failing with the same code, so a handler reading
/// `release_core_from_vault(..).is_ok()` never reaches its next line.
///
/// So the enumeration is inverted: [`RecordVerdict`] censuses the whole plugin registry of both
/// accounts and enumerates the records that provably *cannot* gate a transfer; everything else —
/// a *rejecting* `Royalties` rule set, an external adapter that declares a `Transfer` check, a
/// `plugin_type` byte from a later `mpl-core` — is [`Collateral::Undecidable`], and that arm is
/// admitted by both gates. No classification is refused by both, so nothing is stranded, whatever
/// the mechanism and whether or not this program has heard of it —
/// `no_classification_is_refused_by_both_gates` asserts the weaker claim that actually has to
/// hold, rather than the false claim that the two gates are exact complements.
///
/// Two records are decided by a *field* rather than by their type: mainnet's Collector Crypt
/// collection carries `Royalties { rule_set: None }` and a collection-level `BubblegumV2`, both
/// measured inert by an owner-signed `TransferV1` that succeeded inside it (see
/// [`internal_transfer_verdict`] and `the_live_collector_crypt_collection_registry_reads_settleable`).
/// Classifying either by type alone sends the entire production cohort into the catch-all, which
/// only stays a safe default while the configurations that reach it are rare.
///
/// **`Settleable` and `Undecidable` both carry the collection argument the release must use**,
/// which is why the read hands back an account and not only a verdict. `UpdateAuthority` has two
/// arms besides `Collection`, and an asset on either of them is still vault-owned and still
/// movable — but only if `TransferV1` is given `collection: None`, because MPL Core rejects a
/// collection account the asset does not name. Answering `Settleable` while the release supplied
/// one anyway would be the stranding gap in a second form, with the program itself supplying the
/// wrong account. The classification resolves what the CPI must be given, and the caller passes
/// it through unexamined.
///
/// No escrowed card can reach those two arms today: `deposit_core` refuses an asset declaring no
/// collection, and `mpl-core 0.11.1` refuses to move an asset out of a collection at all —
/// `UpdateV1` answers `NotAvailable` (23) to a new authority of `Address` or of `None`, with the
/// collection account readonly or writable (`core-collection-freeze.test.ts`). The arm is kept
/// because it costs one match arm, and because that test is the entire basis of "unreachable": if
/// a later `mpl-core` makes the transition available, the exits already handle it.
///
/// The causes a **read** can establish are a strict subset of the causes a seizure can have — a
/// separate type for exactly that reason. [`SeizureKind`] has a fourth, `Undecidable`, and it is
/// not one of these: it is the *absence* of an establishable cause, produced by the census rather
/// than by any of the four checks below, and only [`require_collateral_unreachable`] hands it out.
/// Carrying `SeizureKind` here instead would oblige both gates to write an arm for a value the
/// `Unreachable` path can never be handed — a dead arm that reads as coverage.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReadableCause {
    /// No account, or no `AssetV1` in it.
    Burned,
    /// An `AssetV1` whose `owner` is not the position's vault.
    Transferred,
    /// Vault-owned, and stopped by a freeze plugin on the asset or on its collection.
    Frozen,
}

impl From<ReadableCause> for SeizureKind {
    /// Wildcard-free, so a fourth readable cause has to be given a wire variant here rather than
    /// borrowing one — `PositionSeized.kind` is the only record of why a position closed.
    fn from(cause: ReadableCause) -> Self {
        match cause {
            ReadableCause::Burned => SeizureKind::Burned,
            ReadableCause::Transferred => SeizureKind::Transferred,
            ReadableCause::Frozen => SeizureKind::Frozen,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Collateral {
    /// Present, MPL-Core-owned, owned by the vault PDA, and carrying nothing in either plugin
    /// registry that could gate a transfer. The strongest of the three: every record the census
    /// walked is a type that provably cannot reject the Transfer lifecycle, or a freeze plugin
    /// whose flag is clear.
    Settleable,
    /// Beyond the program's reach, with the cause and the owner this read observed.
    /// `owner_observed` is [`Pubkey::default()`] on [`ReadableCause::Burned`], where there is no
    /// account left to read an owner from.
    Unreachable {
        cause: ReadableCause,
        owner_observed: Pubkey,
    },
    /// Present and vault-owned, and the plugin surface holds a record whose effect on a transfer
    /// this program cannot decide. **The one classification both gates admit**, which is what
    /// makes the complement hold by construction rather than by a list of mechanisms.
    Undecidable { owner_observed: Pubkey },
}

/// The asset account as an `AssetV1`, for the intake path — owner program, then discriminator,
/// then the body.
///
/// **The discriminator check is not redundant with the parse.** `BaseAssetV1::from_bytes` is a
/// bare borsh `deserialize` and reads whatever leading byte it finds as a [`CoreKey`]; only
/// `SolanaAccount::load` checks the key, and this module does not use it. An MPL-Core-owned
/// `BaseCollectionV1` leads with `key` then a bare `Pubkey`, so it can parse as an asset whose
/// `owner` is the collection's update authority. This is `escrow.rs`'s `MasterEditionV1` defect
/// in the other family: a successful parse alone distinguishes nothing, and only the key does.
pub fn read_core_asset(asset_info: &AccountInfo) -> Result<BaseAssetV1> {
    require_keys_eq!(
        *asset_info.owner,
        MPL_CORE_ID,
        ByeMachineError::StandardNotAdmitted
    );
    let data = asset_info.try_borrow_data()?;
    let asset = BaseAssetV1::from_bytes(&data).map_err(|_| ByeMachineError::StandardNotAdmitted)?;
    drop(data);
    require!(
        asset.key == CoreKey::AssetV1,
        ByeMachineError::StandardNotAdmitted
    );
    Ok(asset)
}

/// The collection the asset itself declares — the Core family's counterpart to `escrow.rs`'s
/// `declared_collection`, and the address `deposit_core` derives the `PoolCollection` record from.
///
/// **There is no `grouping` field on chain.** `AssetV1.grouping.collection` is a DAS index field
/// and not part of the account; `mpl-core 0.11.1`'s `BaseAssetV1` carries
/// `update_authority: UpdateAuthority` instead. `None` and `Address` both mean **this asset
/// belongs to no collection**, so there is no per-collection record for it to be admitted under
/// and no collection authority behind it — a rejection, not a missing lookup.
pub fn declared_core_collection(asset: &BaseAssetV1) -> Result<Pubkey> {
    match asset.update_authority {
        UpdateAuthority::Collection(collection) => Ok(collection),
        UpdateAuthority::None | UpdateAuthority::Address(_) => {
            Err(ByeMachineError::CollectionNotAdmitted.into())
        }
    }
}

/// What one plugin-registry record means for the `Transfer` lifecycle, **as far as this program
/// can establish it** — and the third arm is the whole point of the type.
///
/// Enumerating plugins that *gate* a transfer by name is incomplete by construction:
/// `Royalties { rule_set: ProgramDenyList }` refuses a release every named-plugin read passes
/// (measured — `core-transfer-refused.test.ts`), and the external `Oracle` / `LifecycleHook`
/// adapters are not even in the registry `fetch_plugin` walks. The set of mechanisms is **open**:
/// MPL Core can ship authority-managed plugins, and a third-party collection authority controls
/// them.
///
/// This enumerates the **inert** ones instead. Everything else — including a `plugin_type` byte no
/// `PluginType` in this build claims — is [`RecordVerdict::Undecidable`], and that verdict is
/// admitted by **both** of this module's gates. A mechanism nobody here has heard of therefore
/// costs a position its exit's *named* error and nothing else, where before it stranded the
/// position's weight behind a card nobody could move.
#[cfg_attr(test, derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TransferVerdict {
    /// Nothing here can stop a transfer, whatever the record contains.
    Clear,
    /// A freeze plugin — decidable, and only by reading its `frozen` flag.
    Freeze(FreezeKind),
    /// Whether a transfer would go through is not decidable from this program.
    Undecidable,
}

/// What **one registry record** means, before its body has been read — a strictly wider set than
/// [`TransferVerdict`], and a separate type so the extra arms cannot escape the walk that
/// resolves them.
///
/// Two plugin types are decided by a field rather than by their type, so the type table can only
/// name the read that settles them. [`registry_verdict`] performs that read and folds the answer
/// into a `TransferVerdict`; nothing downstream of the walk can be handed a `Freeze` whose flag
/// was never read or a `RuleSet` whose rule set was never looked at. **The alternative was one
/// enum and an unreachable arm in [`read_collateral`]**, which is a dead arm in a custody
/// predicate — the thing this module deletes wherever it finds it, because a dead arm reads as
/// coverage.
#[cfg_attr(test, derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum RecordVerdict {
    /// This record cannot stop a transfer, whatever it contains.
    Clear,
    /// A freeze plugin — decidable, and only by reading its `frozen` flag.
    Freeze(FreezeKind),
    /// A `Royalties` plugin — decidable, and only by reading its `rule_set`. A separate arm from
    /// [`RecordVerdict::Undecidable`] for the reason the freeze arm is: a record whose effect
    /// this program *can* establish must not be filed under the one it cannot.
    RuleSet,
    /// Whether this record would hold up a transfer is not decidable from this program.
    Undecidable,
}

/// Which of the two accounts a registry record was found on.
///
/// **A record's effect on a transfer is not always a property of its type alone**, and
/// `BubblegumV2` is the measured case: on a collection it permits compressed assets to be minted
/// alongside uncompressed ones and says nothing about moving an `AssetV1`, while on an asset this
/// program has measured nothing and keeps the conservative verdict. `FreezeKind::Delegate` records
/// the same asymmetry in prose — owner-managed, so MPL Core will not place one on a collection —
/// and this type is what lets the census act on it rather than only describe it.
#[cfg_attr(test, derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountKind {
    Asset,
    Collection,
}

/// Which of the two freeze plugins a record is, carried out of the type table so the flag read
/// below stays wildcard-free and cannot be pointed at the wrong plugin.
#[cfg_attr(test, derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum FreezeKind {
    /// Owner-managed, revocable, and asset-only — a collection has no owner, so MPL Core will not
    /// place one on a collection.
    Delegate,
    /// Authority-managed, revocable by nobody, and it sits on an asset **or** on a collection.
    Permanent,
}

impl FreezeKind {
    fn plugin_type(self) -> PluginType {
        match self {
            FreezeKind::Delegate => PluginType::FreezeDelegate,
            FreezeKind::Permanent => PluginType::PermanentFreezeDelegate,
        }
    }
}

/// The `Transfer` verdict for one internal-registry `plugin_type` byte.
///
/// **Wildcard-free over `PluginType`, and `None` is the byte no variant claims.** `from_u8` is
/// `mpl-core`'s own `FromPrimitive` derive, so a plugin type added in a later release arrives here
/// as a `Some(..)` that **fails to compile** until somebody classifies it; and until that release
/// is picked up, the same byte arrives as `None` and is undecidable. Those are the only two ways a
/// new plugin type can appear, and both land somewhere safe rather than in a gap.
///
/// **`Clear` means "cannot reject the Transfer lifecycle", never "harmless".** A
/// `PermanentTransferDelegate` grants a transfer and never denies one, so it is `Clear` even
/// though it is exactly what makes a third-party seizure constructible. `FreezeExecute` and
/// `PermanentFreezeExecute` carry a `frozen: bool` and are `Clear` for a related reason: they gate
/// the **Execute** lifecycle only, so an asset carrying one transfers normally. `close_seized`
/// takes the inverse of this predicate, so an over-broad read here is not a conservative refusal
/// but a healthy position closed and a transferable card left in a deactivated position's vault.
///
/// **Two of these are decided by a field rather than by the type.** Mainnet's Collector Crypt Core
/// collection carries `Royalties { rule_set: None }` **and** `BubblegumV2`, and an owner-signed
/// `TransferV1` on a card inside it succeeded — so neither record rejects a transfer, even though
/// classifying either by type alone sends every card in that cohort to `Undecidable`, the
/// catch-all `close_seized` is permissionless on.
///
/// The four that are not plainly `Clear`:
///
/// * `FreezeDelegate` and `PermanentFreezeDelegate` gate Transfer on a flag this program reads,
///   which is what keeps the `Frozen` cause a *fact* rather than a guess.
/// * `Royalties` gates Transfer on its `RuleSet`, so the **rule set** is the verdict and the
///   plugin type is not: `RuleSet::None` has no branch that can reject and is `Clear`, while an
///   allow list or a deny list is [`RecordVerdict::Undecidable`], because evaluating either
///   needs the calling program's identity and belongs to MPL Core.
/// * `BubblegumV2` is `Clear` on a **collection** and `Undecidable` on an **asset**, and the two
///   are different facts rather than one hedged. On a collection it is the record that lets
///   compressed Bubblegum assets be minted into the same collection; it does not speak for an
///   uncompressed `AssetV1` in it. On an asset this program has no measurement at all, so it keeps
///   the conservative side.
fn internal_transfer_verdict(plugin_type: u8, on: AccountKind) -> RecordVerdict {
    match PluginType::from_u8(plugin_type) {
        // A byte this build has no variant for — a plugin type from a later `mpl-core`.
        None => RecordVerdict::Undecidable,
        Some(PluginType::FreezeDelegate) => RecordVerdict::Freeze(FreezeKind::Delegate),
        Some(PluginType::PermanentFreezeDelegate) => RecordVerdict::Freeze(FreezeKind::Permanent),
        Some(PluginType::Royalties) => RecordVerdict::RuleSet,
        Some(PluginType::BubblegumV2) => match on {
            AccountKind::Collection => RecordVerdict::Clear,
            AccountKind::Asset => RecordVerdict::Undecidable,
        },
        Some(PluginType::BurnDelegate)
        | Some(PluginType::TransferDelegate)
        | Some(PluginType::UpdateDelegate)
        | Some(PluginType::Attributes)
        | Some(PluginType::PermanentTransferDelegate)
        | Some(PluginType::PermanentBurnDelegate)
        | Some(PluginType::Edition)
        | Some(PluginType::MasterEdition)
        | Some(PluginType::AddBlocker)
        | Some(PluginType::ImmutableMetadata)
        | Some(PluginType::VerifiedCreators)
        | Some(PluginType::Autograph)
        | Some(PluginType::FreezeExecute)
        | Some(PluginType::PermanentFreezeExecute) => RecordVerdict::Clear,
    }
}

/// The `Transfer` verdict for one **external**-registry record — and here the record carries the
/// answer, so there is no table to keep current.
///
/// An external plugin adapter declares which lifecycle events it is consulted on, and MPL Core
/// stores that declaration in the registry itself: `lifecycle_checks` is a
/// `Vec<(HookableLifecycleEvent as u8, ExternalCheckResult)>`. An adapter with no `Transfer` entry
/// is never asked about a transfer and cannot hold one up, whatever kind of adapter it is — which
/// is why this stays correct for adapter types MPL Core has not shipped yet.
///
/// An adapter that **is** asked is undecidable. `ExternalCheckResult`'s flags would distinguish an
/// adapter that can only listen from one that can reject, but those bit values live in the
/// on-chain program rather than in this client crate, and a listen-only adapter read as
/// undecidable costs nothing: both gates admit that verdict.
///
/// Returns a `bool` rather than a [`TransferVerdict`], because only two of the three are reachable
/// from an external record — no external adapter is a freeze plugin — and an arm for the third
/// would be a dead arm in a custody predicate.
fn external_record_is_undecidable(checks: &Option<Vec<(u8, ExternalCheckResult)>>) -> bool {
    let Some(checks) = checks else {
        return false;
    };
    checks
        .iter()
        .any(|(event, _)| *event == HookableLifecycleEvent::Transfer as u8)
}

/// One freeze plugin's `frozen` flag, or `None` when the body cannot be read.
///
/// Generic over the account body for the same reason the census is: `PermanentFreezeDelegate` sits
/// on an asset or on a collection. The caller turns a `None` into `Undecidable`, which is admitted
/// by both gates and so promises nothing in either direction — an unprovable freeze must never
/// manufacture the `CollateralFrozen` that lets `close_seized` fire.
fn frozen_flag<T: DataBlob + SolanaAccount>(info: &AccountInfo, kind: FreezeKind) -> Option<bool> {
    match kind {
        FreezeKind::Delegate => fetch_plugin::<T, FreezeDelegate>(info, kind.plugin_type())
            .ok()
            .map(|(_, p, _)| p.frozen),
        FreezeKind::Permanent => {
            fetch_plugin::<T, PermanentFreezeDelegate>(info, kind.plugin_type())
                .ok()
                .map(|(_, p, _)| p.frozen)
        }
    }
}

/// One `Royalties` plugin's rule set, reduced to the only question the census asks of it: **can
/// this record reject a `Transfer`?** `None` when the body cannot be read.
///
/// Generic over the account body for the same reason the census is — a `Royalties` record sits on
/// an asset or on a collection, and the production collection carries one — and it reaches the
/// body through `fetch_plugin`, exactly as [`frozen_flag`] does, so it gets no closer to the
/// 4,224-byte frame this module's header bans.
///
/// **Wildcard-free over `RuleSet`, so a fourth rule-set kind fails to compile here** rather than
/// inheriting whichever answer is convenient. `None` is the only inert one: an allow list and a
/// deny list both reject on some caller, and which caller is a question about the *invoking
/// program's identity* that this program cannot answer for MPL Core.
fn royalty_rule_set_is_inert<T: DataBlob + SolanaAccount>(info: &AccountInfo) -> Option<bool> {
    fetch_plugin::<T, Royalties>(info, PluginType::Royalties)
        .ok()
        .map(|(_, royalties, _)| match royalties.rule_set {
            RuleSet::None => true,
            RuleSet::ProgramAllowList(_) | RuleSet::ProgramDenyList(_) => false,
        })
}

/// One account's whole plugin surface, censused into a single verdict.
///
/// **Generic over the account body because the census walks two account types**, and it must walk
/// them identically: the escrowed asset's registry and its collection's. `PermanentFreezeDelegate`
/// is authority-managed, so it sits on either — including on the collection, held by a third
/// party and nowhere in any card's own registry — and a census that walked one account would
/// miss that.
///
/// **`PluginRegistryV1Safe` is the reach, and it is not a new one.** It deserializes the registry
/// with `plugin_type` left as a raw `u8`, so a type too new for this build is *visible* rather
/// than a parse failure — and it is the same function `fetch_plugin` already calls internally, so
/// this walk gets no closer to `registry_records_to_plugin_list`, the 4,224-byte frame this
/// module's header bans. Nothing here deserializes a plugin **body**; only the freeze arm does,
/// through `fetch_plugin`, for the two types where the body *is* the answer.
///
/// **An account with no plugins has no header to read**, which `fetch_plugin` expresses as the
/// body length equalling the account length. That is `Clear`, and it is the ordinary case.
///
/// **`Freeze` outranks `Undecidable`, and the precedence is for the audit trail.** A card that is
/// both frozen and carrying a rule set is blocked either way and the remedy is the same, so the
/// cause `PositionSeized` publishes should be the one that is a fact.
fn registry_verdict<T: DataBlob + SolanaAccount>(
    info: &AccountInfo,
    on: AccountKind,
) -> TransferVerdict {
    let Ok(body) = T::load(info, 0) else {
        return TransferVerdict::Undecidable;
    };
    let body_len = body.len();
    if body_len == info.data_len() {
        return TransferVerdict::Clear;
    }

    // Scoped so the borrow is released before the freeze arm below, which borrows the same
    // account data again through `fetch_plugin`.
    let registry = {
        let Ok(data) = info.try_borrow_data() else {
            return TransferVerdict::Undecidable;
        };
        if body_len > data.len() {
            return TransferVerdict::Undecidable;
        }
        let Ok(header) = PluginHeaderV1::from_bytes(&data[body_len..]) else {
            return TransferVerdict::Undecidable;
        };
        let offset = header.plugin_registry_offset as usize;
        if offset > data.len() {
            return TransferVerdict::Undecidable;
        }
        match PluginRegistryV1Safe::from_bytes(&data[offset..]) {
            Ok(registry) => registry,
            Err(_) => return TransferVerdict::Undecidable,
        }
    };

    let mut undecidable = false;
    for record in &registry.registry {
        match internal_transfer_verdict(record.plugin_type, on) {
            RecordVerdict::Clear => {}
            RecordVerdict::Undecidable => undecidable = true,
            RecordVerdict::Freeze(kind) => match frozen_flag::<T>(info, kind) {
                Some(true) => return TransferVerdict::Freeze(kind),
                Some(false) => {}
                None => undecidable = true,
            },
            // An unreadable rule set is undecidable on the same reasoning an unreadable freeze
            // flag is: the record is there, and this program cannot say what it does.
            RecordVerdict::RuleSet => match royalty_rule_set_is_inert::<T>(info) {
                Some(true) => {}
                Some(false) | None => undecidable = true,
            },
        }
    }
    for record in &registry.external_registry {
        if external_record_is_undecidable(&record.lifecycle_checks) {
            undecidable = true;
        }
    }

    if undecidable {
        TransferVerdict::Undecidable
    } else {
        TransferVerdict::Clear
    }
}

/// The census over **both** accounts — the escrowed asset and, when it declares one, its
/// collection.
///
/// `collection_info` is `None` only where there is no collection, never where one was not
/// supplied: [`read_collateral`] resolves it from the asset's own `update_authority` and binds it
/// before handing it over, so a `None` here is the statement that this asset belongs to no
/// collection and therefore has no collection registry to be held up by.
///
/// A `Freeze` from either account wins, on the precedence above; otherwise any `Undecidable` wins;
/// otherwise `Clear`. **Both accounts are always censused** — an early return on the asset's freeze
/// would save one walk and cost the match a dead arm, and a dead arm in a custody predicate reads
/// as coverage.
fn transfer_surface(
    asset_info: &AccountInfo,
    collection_info: Option<&AccountInfo>,
) -> TransferVerdict {
    let asset = registry_verdict::<BaseAssetV1>(asset_info, AccountKind::Asset);
    let collection = collection_info
        .map(|info| registry_verdict::<BaseCollectionV1>(info, AccountKind::Collection))
        .unwrap_or(TransferVerdict::Clear);
    match (asset, collection) {
        (TransferVerdict::Freeze(kind), _) | (_, TransferVerdict::Freeze(kind)) => {
            TransferVerdict::Freeze(kind)
        }
        (TransferVerdict::Undecidable, _) | (_, TransferVerdict::Undecidable) => {
            TransferVerdict::Undecidable
        }
        (TransferVerdict::Clear, TransferVerdict::Clear) => TransferVerdict::Clear,
    }
}

/// The four-check collateral read: **exists · MPL-Core-owned · owned by the vault PDA · a plugin
/// surface that cannot hold up the transfer**, classified rather than asserted so that both
/// directions come from one predicate.
///
/// `vault` is `PDA(["vault", position])`, derived by the caller's accounts struct. **What this
/// function cannot check is that `asset_info` is the position's own asset** — it reads whatever
/// account it is handed. The binding is the caller's, and it is not optional: without
/// `constraint = asset.key() == position.nft_mint` on the accounts struct, a permissionless
/// `close_seized` could present any burned account and seize a position whose card is safe. Every
/// caller owes that constraint.
///
/// **`collection_info` is bound here, not by the caller, and it is bound to the asset rather than
/// to the position.** The census walks the collection's plugin registry, and a caller who chose
/// which collection was walked could hand `close_seized` any frozen collection — or any collection
/// carrying a `Royalties` record — and seize a healthy position with it. That is the one attack
/// the whole classification has to be immune to. `Position`
/// stores no collection, so the only thing this can be checked against is the asset's own
/// `update_authority`, and a mismatch is `CollectionNotAdmitted` (6100) in **both** directions.
/// That is an account-supply fault rather than a classification: the caller can always name the
/// collection the asset declares, and it is the same shape as the `nft_mint` binding above.
///
/// **The fourth check is not a fourth way to fail the first three, which is why the classification
/// is ordered the way it is.** A frozen asset exists, is MPL-Core-owned, and its owner *is* the
/// vault PDA — it passes every absence test while being exactly as unsettleable. Ownership is
/// tested before the census so that an asset both moved out and frozen reports `Transferred` with
/// the new owner in `owner_observed`: the transfer is the fact an auditor needs and the freeze is
/// downstream of it. The collection binding sits inside the fourth check for the same reason —
/// a burned account has no `update_authority` left to bind against, and a transferred-out one is
/// already classified before the collection matters.
///
/// **It is also the only check with three answers**, and the third is what makes the two gates
/// cover everything: the census can decide *clear* and it can decide *frozen*, and for anything
/// else it says so rather than guessing. See [`Collateral`] and [`TransferVerdict`].
///
/// An MPL-Core-owned account that no longer parses as an `AssetV1` classifies as `Burned` rather
/// than erroring. Under the caller's address binding that account *is* the position's asset, so
/// "there is no `AssetV1` here" is the same fact as "the account is gone"; erroring instead would
/// fail both the exit and the cleanup and leave the position with no terminal at all.
pub fn read_collateral<'a, 'info>(
    asset_info: &AccountInfo<'info>,
    collection_info: &'a AccountInfo<'info>,
    vault: &Pubkey,
) -> Result<(Collateral, Option<&'a AccountInfo<'info>>)> {
    if asset_info.owner != &MPL_CORE_ID {
        return Ok((
            Collateral::Unreachable {
                cause: ReadableCause::Burned,
                owner_observed: Pubkey::default(),
            },
            None,
        ));
    }

    let data = asset_info.try_borrow_data()?;
    let parsed = BaseAssetV1::from_bytes(&data)
        .ok()
        .filter(|a| a.key == CoreKey::AssetV1);
    drop(data);

    let Some(asset) = parsed else {
        return Ok((
            Collateral::Unreachable {
                cause: ReadableCause::Burned,
                owner_observed: Pubkey::default(),
            },
            None,
        ));
    };

    if asset.owner != *vault {
        return Ok((
            Collateral::Unreachable {
                cause: ReadableCause::Transferred,
                owner_observed: asset.owner,
            },
            None,
        ));
    }

    // The collection binding, and it precedes the read it protects: the registry that gets
    // walked is the one the *asset* names, not the one the transaction supplied.
    let collection = match asset.update_authority {
        UpdateAuthority::Collection(declared) => {
            require_keys_eq!(
                collection_info.key(),
                declared,
                ByeMachineError::CollectionNotAdmitted
            );
            Some(collection_info)
        }
        // No collection, so no collection registry — only the asset's own two plugins remain.
        UpdateAuthority::None | UpdateAuthority::Address(_) => None,
    };

    // The fourth check is the plugin census, and it has three answers rather than two.
    match transfer_surface(asset_info, collection) {
        TransferVerdict::Clear => Ok((Collateral::Settleable, collection)),
        TransferVerdict::Freeze(_) => Ok((
            Collateral::Unreachable {
                cause: ReadableCause::Frozen,
                owner_observed: asset.owner,
            },
            None,
        )),
        // **The collection argument travels with this arm**, because the exits proceed on it: the
        // census could not promise the transfer will succeed, and the release is what finds out.
        TransferVerdict::Undecidable => Ok((
            Collateral::Undecidable {
                owner_observed: asset.owner,
            },
            collection,
        )),
    }
}

/// The pre-CPI gate for the three Core exits: nothing this program can read may block the move.
///
/// **A gate for the message, not for the outcome.** It cannot establish that the transfer will
/// succeed — see [`Collateral`] — and it is not asked to: the `TransferV1` two lines later is the
/// real test, and MPL Core's own error is what a caller gets when a plugin this program cannot
/// read vetoes the move. What this gate buys is the *named* refusal on the three causes that
/// **are** readable, ahead of the CPI, so a depositor is told `CollateralAbsent` or
/// `CollateralFrozen` with `close_seized` as the remedy instead of an opaque revert. On the
/// unreadable causes the depositor gets the opaque revert and `close_seized` still clears the
/// position, which is the property that matters and the one enumeration could not deliver.
///
/// Takes the same `collection` account the exit is about to hand `TransferV1`, and the
/// classification binds it to the asset's own declaration — so this gate is also where an exit
/// learns that the supplied collection is the wrong one, with `CollectionNotAdmitted` (6100)
/// rather than MPL Core's `InvalidCollection` from inside a reverted CPI.
///
/// **Returns the `collection` argument the release must use**, which is the account on an asset
/// that declares one and `None` on an asset that does not. Passing it on rather than re-reaching
/// for `ctx.accounts.collection` is what makes "the account the CPI uses is the account this gate
/// checked" true by construction: a handler that ignored the return value would have to say so.
///
/// Runs **before** the `TransferV1`, so a caller gets `CollateralAbsent` or `CollateralFrozen`
/// naming `close_seized` as the remedy rather than an opaque revert from inside MPL Core with the
/// phantom leaf still in place. The two codes are separate because the remedies a surface should
/// state differ — *the card is gone* against *the card is frozen and claimable only if the issuer
/// unfreezes it* — and the cause is the only thing the audit trail records.
pub fn require_collateral_settleable<'a, 'info>(
    asset_info: &AccountInfo<'info>,
    collection_info: &'a AccountInfo<'info>,
    vault: &Pubkey,
) -> Result<Option<&'a AccountInfo<'info>>> {
    match read_collateral(asset_info, collection_info, vault)? {
        (Collateral::Settleable, collection) => Ok(collection),
        // **The exit proceeds, and that is the arm's whole reason for existing.** The census said
        // only that it could not decide; refusing here would take a card MPL Core may well move
        // and make the depositor's own withdrawal impossible, which is the stranding this read is
        // supposed to prevent arriving from the other side. So the release runs and MPL Core
        // answers — with an opaque revert if it says no, which is a worse *message* than 6309 and
        // not a worse outcome, because `close_seized` admits this classification too.
        (Collateral::Undecidable { .. }, collection) => Ok(collection),
        (
            Collateral::Unreachable {
                cause: ReadableCause::Frozen,
                ..
            },
            _,
        ) => Err(ByeMachineError::CollateralFrozen.into()),
        (
            Collateral::Unreachable {
                cause: ReadableCause::Burned | ReadableCause::Transferred,
                ..
            },
            _,
        ) => Err(ByeMachineError::CollateralAbsent.into()),
    }
}

/// `close_seized`'s gate — the inverse direction, returning the cause and the observed owner so
/// the handler emits `PositionSeized` from the read that authorised it rather than performing a
/// second one.
///
/// The caller asserts nothing about which cause it expects: a caller who names a card the census
/// finds clear gets `CollateralPresent`, and a frozen card must not trip that guard.
///
/// `collection_info` is read, never trusted — the classification requires it to be the collection
/// the asset itself declares, which is what stops a caller from naming a frozen collection some
/// other asset belongs to and seizing a healthy position with it.
///
/// **[`Collateral::Undecidable`] is admitted here, and it is the same classification the exits
/// proceed on — the one overlap in the whole read, and the thing that makes the complement hold
/// by construction.** No classification is refused by *both* gates, so no position can be both
/// unwithdrawable and uncloseable, whatever mechanism MPL Core is holding the transfer up with and
/// whether or not this program has heard of it.
///
/// **What the overlap costs, stated rather than buried.** On this classification a position whose
/// card MPL Core *would* still move can be closed by a stranger: its weight leaves, the state goes
/// to `Seized`, and the depositor gets the card back through `claim_nft_core` having forfeited the
/// fee accrual `close_seized` settles but never pays. That is a nuisance a griefer can inflict,
/// bounded three ways — the census reaches this arm only for a surface carrying something beyond
/// the inert set, `admit_collection` is Administrator-gated so such a collection need never be
/// admitted in the first place, and the `Active` row is lock-gated on this cause because it leaves
/// the card in the vault.
///
/// **The first of those three is a claim about which real configurations land in the catch-all,
/// and it has to be re-checked whenever the classification above changes.** Reclassifying a plugin
/// by type instead of by field can put an entire production cohort's collection into "something
/// beyond the inert set" and turn the bound from a rare case into a common one. Against it: a
/// position that neither gate serves holds live `w_real` and a drawable Fenwick leaf behind a card
/// nobody can move, which is a pool halt. The trade is deliberate and it is in that direction.
pub fn require_collateral_unreachable<'info>(
    asset_info: &AccountInfo<'info>,
    collection_info: &AccountInfo<'info>,
    vault: &Pubkey,
) -> Result<(SeizureKind, Pubkey)> {
    match read_collateral(asset_info, collection_info, vault)? {
        (Collateral::Settleable, _) => Err(ByeMachineError::CollateralPresent.into()),
        (Collateral::Undecidable { owner_observed }, _) => {
            Ok((SeizureKind::Undecidable, owner_observed))
        }
        (
            Collateral::Unreachable {
                cause,
                owner_observed,
            },
            _,
        ) => Ok((cause.into(), owner_observed)),
    }
}

/// Moves one MPL Core asset via `TransferV1`. `signer_seeds` is empty for a user-signed transfer
/// (`deposit_core`, where `authority` is the depositor) and the position vault's own seeds for a
/// program-signed one (`return_rejected_core`, `withdraw_core`, `claim_nft_core`, where
/// `authority` is `PDA(["vault", position])`) — the same helper serves both custody directions,
/// exactly as `transfer_pnft` and `transfer_spl` do for the Token Metadata family.
///
/// **`authority` and `new_owner` may both be accountless PDAs, and that is the point of the
/// escrow shape rather than an edge case.** A Core position has no vault *token account*; the
/// vault PDA holds the asset directly as `AssetV1.owner`. Program signing does not require the
/// signing account to be allocated, so `invoke_signed` under the vault seeds authorises the
/// release without the vault ever existing as an account.
///
/// **`collection` is `Option` because MPL Core's account is, and the two must agree.** An asset
/// that declares `UpdateAuthority::Collection` cannot be transferred without that account —
/// MPL Core needs it to run the collection's own plugins over the move — and an asset that
/// declares no collection cannot be transferred *with* one, because MPL Core rejects a collection
/// the asset does not name. The argument is never chosen at a call site — [`read_collateral`]
/// resolves it from the asset's own `update_authority` and the exits pass what it returns — which
/// is what makes the second case cost nothing to be right about, since `mpl-core 0.11.1` gives an
/// escrowed card no way to reach it (see [`Collateral`]).
#[allow(clippy::too_many_arguments)]
pub fn transfer_core<'info>(
    asset: &AccountInfo<'info>,
    collection: Option<&AccountInfo<'info>>,
    new_owner: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    mpl_core_program: &AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    TransferV1CpiBuilder::new(mpl_core_program)
        .asset(asset)
        .collection(collection)
        .new_owner(new_owner)
        .authority(Some(authority))
        .payer(payer)
        .system_program(Some(system_program))
        .invoke_signed(signer_seeds)?;
    Ok(())
}

/// The Core family's exit leg — `escrow.rs`'s `release_from_vault` counterpart, and one home for
/// the same reason: three exits, three copies of an eight-argument custody CPI, and every
/// interesting transposition compiles. `return_rejected_core`, `withdraw_core` and
/// `claim_nft_core` differ in **who pays**, and in nothing else this function can see.
///
/// **The vault's signature is the whole of it.** `PDA(["vault", position])` is `AssetV1.owner`
/// and has no account behind it, so the release is authorised by `invoke_signed` under the
/// vault's own seeds rather than by anything that exists on chain.
///
/// **What this function deliberately does not do is test the collateral.** Every caller runs
/// `require_collateral_settleable` at the **top of its handler**, not here: `withdraw_core` makes
/// a fee-payout CPI before it touches the card, so a test folded in here would run after a
/// transfer had already happened and would return its named code one CPI too late. Uniform across
/// all three — the three exits must not drift into two shapes.
#[allow(clippy::too_many_arguments)]
pub fn release_core_from_vault<'info>(
    asset: &AccountInfo<'info>,
    collection: Option<&AccountInfo<'info>>,
    depositor: &AccountInfo<'info>,
    position_vault: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    mpl_core_program: &AccountInfo<'info>,
    position: &Pubkey,
    vault_bump: u8,
) -> Result<()> {
    let vault_seeds: &[&[u8]] = &[POSITION_VAULT_SEED, position.as_ref(), &[vault_bump]];
    transfer_core(
        asset,
        collection,
        depositor,
        position_vault,
        payer,
        system_program,
        mpl_core_program,
        &[vault_seeds],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{crate_sources, flat, production, without_doc_comments};
    use borsh::BorshSerialize;
    use mpl_core::accounts::PluginHeaderV1;
    use mpl_core::types::{
        BubblegumV2, Creator, FreezeExecute, PermanentBurnDelegate, PermanentTransferDelegate,
        Plugin, PluginAuthority, Royalties, RuleSet, UpdateDelegate,
    };
    use mpl_core::DataBlob;
    use mpl_core::SolanaAccount;
    use std::fs;

    const CORE_ASSET_SRC: &str = include_str!("core_asset.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    /// The vault PDA every collateral case below is read against.
    const VAULT: u8 = 70;
    /// The collection the fixture asset declares.
    const COLLECTION: u8 = 71;
    /// A third party — the owner a transferred-out asset reports.
    const STRANGER: u8 = 72;
    /// The collection's own update authority — neither the depositor nor this program.
    const ISSUER: u8 = 73;
    /// A collection the fixture asset does **not** declare.
    const OTHER_COLLECTION: u8 = 74;

    fn base_asset(owner: Pubkey) -> BaseAssetV1 {
        BaseAssetV1 {
            key: CoreKey::AssetV1,
            owner,
            update_authority: UpdateAuthority::Collection(key(COLLECTION)),
            name: "c".to_string(),
            uri: "u".to_string(),
            seq: None,
        }
    }

    /// A whole Core account's data: the body, and — when a plugin is given — the header, the
    /// plugin and the one-record registry laid out exactly as `fetch_plugin` walks them.
    ///
    /// **Generic over the body because the census walks two account types**, and it must walk
    /// them the same way: `AssetV1` for the card's own plugins and `CollectionV1` for the
    /// collection's. A second hand-rolled builder for the second account is how a fixture ends up
    /// proving the collection read works against bytes MPL Core would not have written.
    ///
    /// Built out of `mpl-core`'s own types and its own `DataBlob::len`, never hand-rolled bytes.
    /// The offsets `fetch_plugin` uses are derived from that `len`, so a fixture that computed
    /// them independently would agree with itself and not with the library.
    fn account_data<T: DataBlob + SolanaAccount>(body: &T, plugin: Option<Plugin>) -> Vec<u8> {
        account_data_full(body, plugin.map(|plugin| (plugin, None)).as_slice(), None)
    }

    /// One **external**-adapter registry record, as the census reads it: the adapter's type byte
    /// and the lifecycle events it declares a check on.
    ///
    /// `checks` is what makes the external half of the census need no per-type table — an
    /// adapter with no `Transfer` entry is never asked about a transfer — so a fixture has to be
    /// able to vary it independently of the type.
    struct ExternalFixture {
        plugin_type: u8,
        checks: Option<Vec<(u8, ExternalCheckResult)>>,
    }

    /// The whole account: body, plugin header, one internal plugin's body, and the registry —
    /// `registry` and `external_registry` both — laid out exactly as `PluginRegistryV1Safe`
    /// walks them.
    ///
    /// **One builder, extended rather than copied.** A second hand-rolled layout is how a
    /// fixture ends up proving the census works against bytes MPL Core would not have written,
    /// so the `plugin_type` override and the external record are parameters here instead of a
    /// sibling function. The override exists for the one case that cannot be built out of
    /// `mpl-core`'s own types at all: a `plugin_type` byte no `PluginType` in this build claims.
    fn account_data_full<T: DataBlob + SolanaAccount>(
        body: &T,
        internal: &[(Plugin, Option<u8>)],
        external: Option<ExternalFixture>,
    ) -> Vec<u8> {
        let mut data = Vec::new();
        body.serialize(&mut data).unwrap();
        assert_eq!(
            data.len(),
            body.len(),
            "the serialized body and DataBlob::len disagree, so every plugin offset below \
             would be measured from the wrong place"
        );

        if internal.is_empty() && external.is_none() {
            return data;
        }

        // Each plugin's body laid down in turn, with the offset the registry record will point
        // at recorded as it goes. **A slice rather than one plugin, because the defect this
        // fixture had to reproduce is a property of a registry's *combination*:** `Royalties`
        // and `BubblegumV2` are each individually classifiable, and it was the two of them
        // together on one collection — the live `CCryptUfe…9CRac` layout — that a
        // one-plugin-at-a-time fixture could not express and so could not catch.
        let mut plugin_bytes = Vec::new();
        let first_plugin_at = data.len() + PluginHeaderV1::LEN;
        let mut offsets = Vec::new();
        for (plugin, _) in internal {
            offsets.push(first_plugin_at + plugin_bytes.len());
            plugin.serialize(&mut plugin_bytes).unwrap();
        }
        let registry_at = first_plugin_at + plugin_bytes.len();

        PluginHeaderV1 {
            key: CoreKey::PluginHeaderV1,
            plugin_registry_offset: registry_at as u64,
        }
        .serialize(&mut data)
        .unwrap();
        data.extend_from_slice(&plugin_bytes);

        CoreKey::PluginRegistryV1.serialize(&mut data).unwrap();
        (internal.len() as u32).serialize(&mut data).unwrap();
        for ((plugin, type_byte), plugin_at) in internal.iter().zip(&offsets) {
            match type_byte {
                Some(byte) => byte.serialize(&mut data).unwrap(),
                None => PluginType::from(plugin).serialize(&mut data).unwrap(),
            }
            PluginAuthority::Owner.serialize(&mut data).unwrap();
            (*plugin_at as u64).serialize(&mut data).unwrap();
        }
        match &external {
            Some(record) => {
                1u32.serialize(&mut data).unwrap();
                record.plugin_type.serialize(&mut data).unwrap();
                PluginAuthority::UpdateAuthority
                    .serialize(&mut data)
                    .unwrap();
                record.checks.serialize(&mut data).unwrap();
                (registry_at as u64).serialize(&mut data).unwrap();
                None::<u64>.serialize(&mut data).unwrap();
                None::<u64>.serialize(&mut data).unwrap();
            }
            None => 0u32.serialize(&mut data).unwrap(),
        }
        data
    }

    fn asset_data(asset: &BaseAssetV1, plugin: Option<Plugin>) -> Vec<u8> {
        account_data(asset, plugin)
    }

    /// The collection `base_asset` declares, as an account. `update_authority` is a third party
    /// on purpose — a collection-level `permanent_freeze_delegate` is held by the issuer, neither
    /// the depositor nor this program.
    fn collection_body() -> BaseCollectionV1 {
        BaseCollectionV1 {
            key: CoreKey::CollectionV1,
            update_authority: key(ISSUER),
            name: "c".to_string(),
            uri: "u".to_string(),
            num_minted: 1,
            current_size: 1,
        }
    }

    fn collection_data(plugin: Option<Plugin>) -> Vec<u8> {
        account_data(&collection_body(), plugin)
    }

    /// `Collateral` carries no `Debug` — `SeizureKind` is an on-wire event enum and does not
    /// derive one — so cases compare this instead of the value. It keeps `assert_eq!`'s failure
    /// message, which a bare `matches!` would not.
    fn describe(read: (Collateral, bool)) -> (&'static str, Pubkey) {
        match read.0 {
            Collateral::Settleable => ("settleable", Pubkey::default()),
            Collateral::Unreachable {
                cause: ReadableCause::Burned,
                owner_observed,
            } => ("burned", owner_observed),
            Collateral::Unreachable {
                cause: ReadableCause::Transferred,
                owner_observed,
            } => ("transferred", owner_observed),
            Collateral::Unreachable {
                cause: ReadableCause::Frozen,
                owner_observed,
            } => ("frozen", owner_observed),
            Collateral::Undecidable { owner_observed } => ("undecidable", owner_observed),
        }
    }

    /// One classification, over an asset account and the collection account it is read against.
    ///
    /// The short form supplies the collection the fixture asset declares, with a clean registry —
    /// so every pre-existing case below still varies exactly one thing, and a case that wants to
    /// vary the *collection* says so.
    macro_rules! collateral_case {
        (owner_program = $owner_program:expr, data = $data:expr $(,)?) => {
            collateral_case!(
                owner_program = $owner_program,
                data = $data,
                collection_key = key(COLLECTION),
                collection_data = collection_data(None),
            )
        };
        (
            owner_program = $owner_program:expr,
            data = $data:expr,
            collection_key = $collection_key:expr,
            collection_data = $collection_data:expr $(,)?
        ) => {{
            let asset_key = key(1);
            let owner_program = $owner_program;
            let mut data = $data;
            let mut lamports = 1u64;
            let asset_info = AccountInfo::new(
                &asset_key,
                false,
                false,
                &mut lamports,
                &mut data,
                &owner_program,
                false,
                0,
            );

            let collection_key = $collection_key;
            let collection_owner = MPL_CORE_ID;
            let mut collection_bytes = $collection_data;
            let mut collection_lamports = 1u64;
            let collection_info = AccountInfo::new(
                &collection_key,
                false,
                false,
                &mut collection_lamports,
                &mut collection_bytes,
                &collection_owner,
                false,
                0,
            );

            // The borrow the read hands back cannot leave this block, so it is collapsed to the
            // one fact a case asserts about it: whether the release will be given a collection.
            read_collateral(&asset_info, &collection_info, &key(VAULT))
                .map(|(collateral, collection)| (collateral, collection.is_some()))
        }};
    }

    /// The fixture every freeze case below varies exactly one thing against: present,
    /// MPL-Core-owned, vault-owned, no plugins.
    fn settleable_data() -> Vec<u8> {
        asset_data(&base_asset(key(VAULT)), None)
    }

    // --- the read surface: owner program, discriminator, collection --------------------------

    #[test]
    fn read_core_asset_rejects_an_account_owned_by_another_program() {
        let asset_key = key(1);
        let owner_program = key(99);
        let mut data = settleable_data();
        let mut lamports = 1u64;
        let info = AccountInfo::new(
            &asset_key,
            false,
            false,
            &mut lamports,
            &mut data,
            &owner_program,
            false,
            0,
        );
        assert_eq!(error_code(read_core_asset(&info).unwrap_err()), 6101);
    }

    /// The discriminator check earning its place: `BaseAssetV1::from_bytes` is a bare borsh
    /// parse, so an MPL-Core-owned account carrying a *different* `Key` deserializes without
    /// complaint and reports whatever the following 32 bytes are as its `owner`. Only the key
    /// comparison separates them — `escrow.rs`'s `MasterEditionV1` defect in the Core family.
    #[test]
    fn read_core_asset_rejects_an_mpl_core_account_that_is_not_an_asset_v1() {
        let mut impostor = base_asset(key(VAULT));
        impostor.key = CoreKey::CollectionV1;
        let mut data = Vec::new();
        impostor.serialize(&mut data).unwrap();
        assert!(
            BaseAssetV1::from_bytes(&data).is_ok(),
            "the impostor must parse, or this test proves the parse rejects it and not the key \
             check that is under test"
        );

        let asset_key = key(1);
        let owner_program = MPL_CORE_ID;
        let mut lamports = 1u64;
        let info = AccountInfo::new(
            &asset_key,
            false,
            false,
            &mut lamports,
            &mut data,
            &owner_program,
            false,
            0,
        );
        assert_eq!(error_code(read_core_asset(&info).unwrap_err()), 6101);
    }

    #[test]
    fn read_core_asset_accepts_a_well_formed_asset_and_returns_its_owner() {
        let asset_key = key(1);
        let owner_program = MPL_CORE_ID;
        let mut data = settleable_data();
        let mut lamports = 1u64;
        let info = AccountInfo::new(
            &asset_key,
            false,
            false,
            &mut lamports,
            &mut data,
            &owner_program,
            false,
            0,
        );
        let asset = read_core_asset(&info).unwrap();
        assert_eq!(asset.owner, key(VAULT));
        assert_eq!(
            declared_core_collection(&asset).unwrap(),
            key(COLLECTION),
            "the record deposit_core derives is keyed on this address"
        );
    }

    /// `grouping.collection` cannot express "no collection";
    /// `UpdateAuthority` has two arms that mean exactly that, and both are a rejection rather
    /// than a lookup that finds nothing.
    #[test]
    fn declared_core_collection_rejects_both_arms_that_mean_no_collection() {
        for authority in [UpdateAuthority::None, UpdateAuthority::Address(key(5))] {
            let mut asset = base_asset(key(VAULT));
            asset.update_authority = authority;
            assert_eq!(
                error_code(declared_core_collection(&asset).unwrap_err()),
                6100
            );
        }
    }

    // --- the three absence tests -------------------------------------------------------------

    #[test]
    fn collateral_reads_settleable_on_a_present_vault_owned_unfrozen_asset() {
        assert_eq!(
            describe(
                collateral_case!(owner_program = MPL_CORE_ID, data = settleable_data()).unwrap()
            ),
            ("settleable", Pubkey::default())
        );
    }

    /// A burned Core asset's account is closed, which on Solana means zero data and the system
    /// program as owner — so the existence test and the owner-program test are the same test.
    #[test]
    fn collateral_reads_burned_when_the_account_is_no_longer_mpl_core_owned() {
        assert_eq!(
            describe(
                collateral_case!(owner_program = System::id(), data = Vec::<u8>::new()).unwrap()
            ),
            ("burned", Pubkey::default())
        );
    }

    #[test]
    fn collateral_reads_transferred_and_reports_the_owner_it_observed() {
        assert_eq!(
            describe(
                collateral_case!(
                    owner_program = MPL_CORE_ID,
                    data = asset_data(&base_asset(key(STRANGER)), None),
                )
                .unwrap()
            ),
            ("transferred", key(STRANGER)),
            "owner_observed is the only record of where the card went"
        );
    }

    // --- the fourth read, asserted independently of the three above --------------------------

    /// The reason the freeze read is a fourth *check* rather than a fourth way to fail one of
    /// the three: this fixture is present, MPL-Core-owned and
    /// vault-owned — it passes every absence test above — and differs from
    /// `collateral_reads_settleable_on_a_present_vault_owned_unfrozen_asset` in the plugin alone.
    #[test]
    fn a_frozen_asset_passes_all_three_absence_tests_and_is_still_unreachable() {
        for plugin in [
            Plugin::PermanentFreezeDelegate(PermanentFreezeDelegate { frozen: true }),
            Plugin::FreezeDelegate(FreezeDelegate { frozen: true }),
        ] {
            let data = asset_data(&base_asset(key(VAULT)), Some(plugin));
            assert_eq!(
                describe(collateral_case!(owner_program = MPL_CORE_ID, data = data).unwrap()),
                ("frozen", key(VAULT)),
                "the vault still owns it, which is exactly why the absence tests cannot see this"
            );
        }
    }

    /// Both freeze plugins are read, and a thawed one is not a freeze — a predicate keyed on the
    /// plugin's *presence* rather than its `frozen` field would red here.
    #[test]
    fn a_thawed_freeze_plugin_leaves_the_asset_settleable() {
        for plugin in [
            Plugin::PermanentFreezeDelegate(PermanentFreezeDelegate { frozen: false }),
            Plugin::FreezeDelegate(FreezeDelegate { frozen: false }),
        ] {
            let data = asset_data(&base_asset(key(VAULT)), Some(plugin));
            assert_eq!(
                describe(collateral_case!(owner_program = MPL_CORE_ID, data = data).unwrap()),
                ("settleable", Pubkey::default())
            );
        }
    }

    /// **Four plugins in `mpl-core 0.11.1` carry `frozen: bool`, and only two of them stop a
    /// transfer.** `FreezeExecute` gates the Execute lifecycle — the asset cannot run
    /// instructions through its Asset Signer PDA — and transfers normally while set. Reading it
    /// as a freeze would hand `close_seized`, which is permissionless and takes the inverse of
    /// this predicate, a healthy vault-owned card to seize: the position closes, its weight
    /// leaves, and a perfectly transferable asset is stranded in the vault of a deactivated
    /// position. This case is the difference between selecting the plugins by what they gate and
    /// selecting them by what their field is called.
    #[test]
    fn an_execute_freeze_is_not_a_transfer_freeze() {
        let data = asset_data(
            &base_asset(key(VAULT)),
            Some(Plugin::FreezeExecute(FreezeExecute { frozen: true })),
        );
        assert_eq!(
            describe(collateral_case!(owner_program = MPL_CORE_ID, data = data).unwrap()),
            ("settleable", Pubkey::default())
        );
    }

    // --- the collection's own registry, which the asset's cannot show ------------------------

    /// **The read the whole `Seized` terminal turns on, and the one a fixture over the asset
    /// account alone cannot reach.** `permanent_freeze_delegate` is authority-managed, so it sits
    /// on an asset *or* on a collection, and a collection's copy freezes every asset in it. A
    /// freeze read that walked only the asset would classify the position `Settleable` while MPL
    /// Core refuses every `TransferV1` against it: the exits fail opaquely and `close_seized`
    /// answers `CollateralPresent`, leaving a phantom weighted leaf nothing can clear.
    ///
    /// The asset here is byte-identical to the settleable case's fixture above. Only the
    /// collection account differs.
    #[test]
    fn a_collection_level_permanent_freeze_is_seen_by_the_gate() {
        assert_eq!(
            describe(
                collateral_case!(
                    owner_program = MPL_CORE_ID,
                    data = settleable_data(),
                    collection_key = key(COLLECTION),
                    collection_data = collection_data(Some(Plugin::PermanentFreezeDelegate(
                        PermanentFreezeDelegate { frozen: true }
                    ))),
                )
                .unwrap()
            ),
            ("frozen", key(VAULT)),
            "the card is present and vault-owned — the freeze is on the collection, and only \
             the collection account carries it"
        );
    }

    /// The other direction of the collection's own flag: a collection carrying the plugin
    /// *thawed* is not a freeze. A predicate keyed on the plugin's presence rather than its
    /// `frozen` field would strand every position in the collection permanently — and it would
    /// do it to the live collection, which carries the plugin today.
    #[test]
    fn a_thawed_collection_freeze_leaves_the_asset_settleable() {
        assert_eq!(
            describe(
                collateral_case!(
                    owner_program = MPL_CORE_ID,
                    data = settleable_data(),
                    collection_key = key(COLLECTION),
                    collection_data = collection_data(Some(Plugin::PermanentFreezeDelegate(
                        PermanentFreezeDelegate { frozen: false }
                    ))),
                )
                .unwrap()
            ),
            ("settleable", Pubkey::default())
        );
    }

    /// **The binding, and it is what makes the collection read safe to hand a permissionless
    /// instruction.** `fetch_plugin` walks whatever account it is given, so a `close_seized`
    /// caller who could choose the collection would name any frozen collection on chain and
    /// seize a healthy position with it — weight removed, card stranded. The classification
    /// requires the account to be the collection the **asset** declares, and refuses with
    /// `CollectionNotAdmitted` (6100) otherwise, in both directions.
    #[test]
    fn a_collection_the_asset_does_not_declare_is_refused_rather_than_read() {
        let frozen = Plugin::PermanentFreezeDelegate(PermanentFreezeDelegate { frozen: true });
        // `unwrap_err` would need `Debug` on `Collateral`, which carries the `SeizureKind` wire
        // enum and deliberately derives none.
        match collateral_case!(
            owner_program = MPL_CORE_ID,
            data = settleable_data(),
            collection_key = key(OTHER_COLLECTION),
            collection_data = collection_data(Some(frozen)),
        ) {
            Err(err) => assert_eq!(
                error_code(err),
                6100,
                "a frozen collection the asset is not in must not be able to classify it"
            ),
            Ok(collateral) => panic!("expected 6100, got {}", describe(collateral).0),
        }
    }

    /// And the binding is not a way to *refuse* a burned card. The collection account is read
    /// only after the asset has been established present and vault-owned, so a caller cleaning
    /// up a burned position — who has no asset left to read a declaration from, and so cannot
    /// know what to supply — still gets the classification they need.
    #[test]
    fn a_burned_asset_classifies_without_regard_to_the_collection_supplied() {
        assert_eq!(
            describe(
                collateral_case!(
                    owner_program = System::id(),
                    data = Vec::<u8>::new(),
                    collection_key = key(OTHER_COLLECTION),
                    collection_data = Vec::<u8>::new(),
                )
                .unwrap()
            ),
            ("burned", Pubkey::default())
        );
    }

    /// The same for a transferred-out card, which does still have a declaration to read: the
    /// transfer is classified before the collection is consulted, so the cause an auditor sees
    /// is the transfer and not a collection mismatch.
    #[test]
    fn a_transferred_asset_classifies_before_the_collection_is_consulted() {
        assert_eq!(
            describe(
                collateral_case!(
                    owner_program = MPL_CORE_ID,
                    data = asset_data(&base_asset(key(STRANGER)), None),
                    collection_key = key(OTHER_COLLECTION),
                    collection_data = collection_data(None),
                )
                .unwrap()
            ),
            ("transferred", key(STRANGER))
        );
    }

    /// An asset on either of `UpdateAuthority`'s non-collection arms declares none, so there is
    /// no collection registry to read and its own two plugins are the whole freeze surface. It
    /// classifies `Settleable`, the collection account supplied is not compared against anything
    /// — there is nothing to compare it to — and **the read hands the release `None`**, which is
    /// the half that makes `Settleable` true rather than merely hopeful: MPL Core refuses a
    /// collection account the asset does not name, so a release that passed one anyway would fail
    /// the exit and leave a position no seizure could clean up either.
    ///
    /// **No escrowed card reaches this arm under `mpl-core 0.11.1`** — intake refuses an asset
    /// with no collection and the dependency refuses to take one out of a collection, measured in
    /// `core-collection-freeze.test.ts`. The arm is one match arm wide and this is what makes it
    /// correct if that ever changes. Both non-collection variants are driven, because they share
    /// an arm today and a future edit could split them.
    #[test]
    fn an_asset_declaring_no_collection_is_settleable_and_releases_without_one() {
        for authority in [UpdateAuthority::Address(key(ISSUER)), UpdateAuthority::None] {
            let mut orphan = base_asset(key(VAULT));
            orphan.update_authority = authority;
            let read = collateral_case!(
                owner_program = MPL_CORE_ID,
                data = asset_data(&orphan, None),
                collection_key = key(OTHER_COLLECTION),
                collection_data = collection_data(Some(Plugin::PermanentFreezeDelegate(
                    PermanentFreezeDelegate { frozen: true }
                ))),
            )
            .unwrap();
            assert_eq!(
                describe(read),
                ("settleable", Pubkey::default()),
                "no declaration means no collection registry — not the registry of whatever \
                 account the caller supplied"
            );
            assert!(
                !read.1,
                "the release must be given None, or MPL Core refuses the transfer of an asset \
                 that names no collection"
            );
        }
    }

    /// The other direction, without which the assertion above passes on a read that returns
    /// `None` for everything: an asset that still declares its collection resolves to the
    /// account, so the release supplies it and MPL Core runs the collection's plugins.
    #[test]
    fn an_asset_still_in_its_collection_releases_with_that_collection() {
        let read = collateral_case!(owner_program = MPL_CORE_ID, data = settleable_data()).unwrap();
        assert_eq!(describe(read), ("settleable", Pubkey::default()));
        assert!(read.1, "a declared collection must reach TransferV1");
    }

    /// And such an asset's own freeze is still read: dropping the collection arm must not drop
    /// the two asset-level plugins with it.
    #[test]
    fn an_asset_declaring_no_collection_is_still_frozen_by_its_own_plugin() {
        let mut orphan = base_asset(key(VAULT));
        orphan.update_authority = UpdateAuthority::None;
        // The collection supplied is frozen and is *not* this asset's; the freeze that classifies
        // it comes from its own registry, which is the only one there is to read.
        assert_eq!(
            describe(
                collateral_case!(
                    owner_program = MPL_CORE_ID,
                    data = asset_data(
                        &orphan,
                        Some(Plugin::FreezeDelegate(FreezeDelegate { frozen: true })),
                    ),
                    collection_key = key(OTHER_COLLECTION),
                    collection_data = collection_data(None),
                )
                .unwrap()
            ),
            ("frozen", key(VAULT))
        );
    }

    /// Ordering, which no single-cause case can see. An asset that was moved out **and** frozen
    /// satisfies both tests, and `PositionSeized.kind` is the only record of why a position
    /// closed — reporting a transfer as a freeze is `events.rs`'s relocated-wire-byte fault
    /// arriving through the classifier instead of through the enum. The transfer is the fact an
    /// auditor needs, and it is tested first.
    #[test]
    fn an_asset_both_transferred_and_frozen_reports_the_transfer() {
        let data = asset_data(
            &base_asset(key(STRANGER)),
            Some(Plugin::PermanentFreezeDelegate(PermanentFreezeDelegate {
                frozen: true,
            })),
        );
        assert_eq!(
            describe(collateral_case!(owner_program = MPL_CORE_ID, data = data).unwrap()),
            ("transferred", key(STRANGER))
        );
    }

    /// The discriminator check on the *collateral* path, which is a different call site from
    /// `read_core_asset`'s and needs its own case. Without it an MPL-Core-owned account that
    /// merely parses is classified on whatever its bytes put where `owner` belongs — and this
    /// fixture puts the vault there, so it would read `Settleable`: the exits would drive a
    /// `TransferV1` at a non-asset and `close_seized` would refuse the position forever, which
    /// is the one outcome the two gates being complements is supposed to rule out.
    #[test]
    fn an_mpl_core_account_that_is_not_an_asset_v1_is_unreachable_not_settleable() {
        let mut impostor = base_asset(key(VAULT));
        impostor.key = CoreKey::CollectionV1;
        let mut data = Vec::new();
        impostor.serialize(&mut data).unwrap();
        assert!(
            BaseAssetV1::from_bytes(&data).is_ok_and(|a| a.owner == key(VAULT)),
            "the impostor must parse *and* land the vault in the owner slot, or it cannot \
             discriminate the missing key check from the ownership test"
        );
        assert_eq!(
            describe(collateral_case!(owner_program = MPL_CORE_ID, data = data).unwrap()),
            ("burned", Pubkey::default())
        );
    }

    // --- the two directions, and that they are exact complements -----------------------------

    /// **Both gates over one set of accounts**, so the complement is asserted over a single
    /// classification rather than over two reads that might disagree.
    ///
    /// Returns `(settleable, unreachable)` with the settleable side collapsed to *whether a
    /// collection reaches the release* — the borrow it hands back cannot leave this block.
    macro_rules! gates_over {
        (
            owner_program = $owner_program:expr,
            data = $data:expr,
            collection_data = $collection_data:expr $(,)?
        ) => {{
            let asset_key = key(1);
            let owner_program = $owner_program;
            let mut data = $data;
            let mut lamports = 1u64;
            let asset_info = AccountInfo::new(
                &asset_key,
                false,
                false,
                &mut lamports,
                &mut data,
                &owner_program,
                false,
                0,
            );

            let collection_key = key(COLLECTION);
            let collection_owner = MPL_CORE_ID;
            let mut collection_bytes = $collection_data;
            let mut collection_lamports = 1u64;
            let collection_info = AccountInfo::new(
                &collection_key,
                false,
                false,
                &mut collection_lamports,
                &mut collection_bytes,
                &collection_owner,
                false,
                0,
            );

            let settleable =
                require_collateral_settleable(&asset_info, &collection_info, &key(VAULT))
                    .map(|collection| collection.is_some());
            let unreachable =
                require_collateral_unreachable(&asset_info, &collection_info, &key(VAULT));
            (settleable, unreachable)
        }};
    }

    /// Every code by exact numeric value, in both directions, over every classification the read
    /// can produce. `CollateralFrozen` splits off `CollateralAbsent` so the depositor of a
    /// frozen card is told something different from the depositor of a burned one, because
    /// their remedies differ.
    ///
    /// **The complement asserted at the bottom is "not refused by both", the weaker claim that
    /// actually holds — not "exactly one refuses".** `Undecidable` is admitted by both
    /// gates on purpose: it is the classification for a plugin surface this program cannot decide,
    /// so the exit proceeds and MPL Core answers, *and* the cleanup can act if it will not. A
    /// classification both gates admit strands nothing. A classification both gates *refuse* is a
    /// position that can neither exit nor be cleaned up, and that is the one this loop forbids.
    #[test]
    fn no_classification_is_refused_by_both_gates() {
        let frozen_permanent =
            Plugin::PermanentFreezeDelegate(PermanentFreezeDelegate { frozen: true });
        let royalties = Plugin::Royalties(Royalties {
            basis_points: 500,
            creators: vec![],
            rule_set: RuleSet::ProgramDenyList(vec![]),
        });

        /// `(asset data, owner program, collection data, the code
        /// `require_collateral_settleable` returns, the code `require_collateral_unreachable`
        /// returns)` — `None` meaning that gate accepts.
        type Case = (
            &'static str,
            Vec<u8>,
            Pubkey,
            Vec<u8>,
            Option<u32>,
            Option<u32>,
        );

        let cases: [Case; 7] = [
            (
                "clean",
                settleable_data(),
                MPL_CORE_ID,
                collection_data(None),
                None,
                Some(6308),
            ),
            (
                "no account",
                Vec::new(),
                System::id(),
                collection_data(None),
                Some(6307),
                None,
            ),
            (
                "moved out",
                asset_data(&base_asset(key(STRANGER)), None),
                MPL_CORE_ID,
                collection_data(None),
                Some(6307),
                None,
            ),
            (
                "frozen on the asset",
                asset_data(&base_asset(key(VAULT)), Some(frozen_permanent.clone())),
                MPL_CORE_ID,
                collection_data(None),
                Some(6309),
                None,
            ),
            // The classification only the collection account carries: a clean asset registry
            // inside a frozen collection. Without the collection read this row is
            // `(None, Some(6308))` — the exits refused by MPL Core and the cleanup refusing to
            // run, which is a position neither gate serves.
            (
                "frozen on the collection",
                settleable_data(),
                MPL_CORE_ID,
                collection_data(Some(frozen_permanent)),
                Some(6309),
                None,
            ),
            // **The two rows where both gates accept**, one per account. A `Royalties` rule set
            // is MPL Core's to evaluate, so neither direction is refused: the exit runs and finds
            // out, and the cleanup can close the position if it does not.
            (
                "a rule set on the asset",
                asset_data(&base_asset(key(VAULT)), Some(royalties.clone())),
                MPL_CORE_ID,
                collection_data(None),
                None,
                None,
            ),
            (
                "a rule set on the collection",
                settleable_data(),
                MPL_CORE_ID,
                collection_data(Some(royalties)),
                None,
                None,
            ),
        ];

        for (label, bytes, owner_program, collection_bytes, settleable_err, unreachable_err) in
            cases
        {
            let (settleable, unreachable) = gates_over!(
                owner_program = owner_program,
                data = bytes,
                collection_data = collection_bytes,
            );

            match settleable_err {
                Some(code) => assert_eq!(
                    error_code(settleable.unwrap_err()),
                    code,
                    "{label}: the exits' gate"
                ),
                None => {
                    settleable.unwrap();
                }
            }

            // Matched rather than `unwrap_err`ed, which would need `Debug` on the success type —
            // and it carries `SeizureKind`, an on-wire event enum that deliberately derives none.
            match (unreachable_err, unreachable) {
                (Some(code), Err(err)) => assert_eq!(error_code(err), code, "{label}: the cleanup"),
                (None, Ok(_)) => {}
                (Some(code), Ok(_)) => panic!("{label}: expected {code}, got a classification"),
                (None, Err(err)) => {
                    panic!(
                        "{label}: expected a classification, got {}",
                        error_code(err)
                    )
                }
            }

            assert!(
                !(settleable_err.is_some() && unreachable_err.is_some()),
                "{label}: a classification both gates refuse is a position that can neither \
                 exit nor be cleaned up"
            );
        }
    }

    #[test]
    fn the_unreachable_gate_hands_the_handler_the_cause_it_will_emit() {
        let (_, unreachable) = gates_over!(
            owner_program = MPL_CORE_ID,
            data = asset_data(&base_asset(key(STRANGER)), None),
            collection_data = collection_data(None),
        );
        let (kind, owner_observed) = match unreachable {
            Ok(pair) => pair,
            Err(err) => panic!("expected a classification, got {}", error_code(err)),
        };
        assert!(kind == SeizureKind::Transferred);
        assert_eq!(owner_observed, key(STRANGER));
    }

    // --- the census: what makes the undecidable arm reachable ---------------------------------

    /// **Every way a record can reach the undecidable arm, and the reason the arm exists.**
    /// Naming gating plugins by name is incomplete by construction: a *rejecting* `Royalties`
    /// rule set refuses a release every named read passes, an **asset-level** `BubblegumV2`
    /// record is one this program has never measured, and an external adapter is not even in
    /// the registry `fetch_plugin` walks. The set is open — a later `mpl-core` can ship a plugin
    /// type this build has no variant for.
    ///
    /// **Both plugin rows here are narrower than they read.** A `Royalties` record with
    /// `rule_set: None` is `Clear`, and a
    /// `BubblegumV2` record on a *collection* is `Clear`; classifying either by its type alone
    /// put the whole production cohort in this arm. The rows below are the shapes that genuinely
    /// reach it — see `the_live_collector_crypt_collection_registry_reads_settleable` for the
    /// shape that must not.
    ///
    /// So the census enumerates the **inert** plugins and defaults everything else here, which is
    /// what makes the last row the important one: a `plugin_type` byte of 200 is not a plugin
    /// anybody has designed, and it classifies safely without this file being told anything about
    /// it. That row is the whole difference between this read and the three that preceded it.
    #[test]
    fn every_undecidable_record_shape_classifies_as_undecidable() {
        let royalties = Plugin::Royalties(Royalties {
            basis_points: 0,
            creators: vec![],
            rule_set: RuleSet::ProgramAllowList(vec![key(ISSUER)]),
        });
        let bubblegum = Plugin::BubblegumV2(BubblegumV2 {});

        for (label, data) in [
            (
                "a royalties rule set",
                asset_data(&base_asset(key(VAULT)), Some(royalties)),
            ),
            (
                "a bubblegum plugin",
                asset_data(&base_asset(key(VAULT)), Some(bubblegum)),
            ),
            (
                // The row that ends the pattern: no `PluginType` in this build claims byte 200,
                // so `from_u8` answers `None` and the census does not have to know what it is.
                "a plugin type byte from a later mpl-core",
                account_data_full(
                    &base_asset(key(VAULT)),
                    &[(
                        Plugin::PermanentTransferDelegate(PermanentTransferDelegate {}),
                        Some(200),
                    )],
                    None,
                ),
            ),
            (
                "an external adapter that declares a Transfer check",
                account_data_full(
                    &base_asset(key(VAULT)),
                    &[],
                    Some(ExternalFixture {
                        plugin_type: 1, // Oracle
                        checks: Some(vec![(
                            HookableLifecycleEvent::Transfer as u8,
                            ExternalCheckResult { flags: 4 },
                        )]),
                    }),
                ),
            ),
        ] {
            assert_eq!(
                describe(collateral_case!(owner_program = MPL_CORE_ID, data = data).unwrap()).0,
                "undecidable",
                "{label} must not read as settleable"
            );
        }
    }

    /// **The live production registry, reproduced record for record, must read `Settleable`.**
    ///
    /// This is a transcription of
    /// mainnet rather than a shape somebody thought plausible: `getAccountInfo` on the Collector
    /// Crypt Core collection `CCryptUfeFSZ3Fgc9FLeKrhLVAP67FSqi1GuVoj9CRac` decodes exactly these
    /// six internal records and an empty external registry, and the sampled card
    /// `64MAdsUZev5TbAHKFwyvoRW4fWF68akr5usLrQoNZsfD` in it carries one, a
    /// `PermanentFreezeDelegate` with `frozen: false`.
    ///
    /// **Two of the six were classified by type and both were classified wrong**, which is why
    /// the assertion is the whole registry and not the two records: each is individually
    /// harmless-looking, and it took the combination on one account for the defect to be
    /// visible. `Royalties { rule_set: None }` and a collection-level `BubblegumV2` each sent the
    /// verdict to `Undecidable`, so **every card in the cohort this pool exists to escrow** read
    /// undecidable — and `close_seized` is permissionless on that classification, with no lock
    /// gate at all on a `Pending` row. Any funded wallet could end any deposit, permanently, for
    /// the price of one transaction.
    ///
    /// **That the two records do not gate a transfer is measured, not reasoned.** Transaction
    /// `2hRzZ4KvGcoiuMGQdVr2nhmEpgdUyY1EHjDJ5eTTpjD8LGBiMLMTyaqVWdrg3eGcDfruzy8GBWmv8eKTKikkh5Qu`
    /// (2026-08-31) is an owner-signed `TransferV1` on that card inside that collection, and it
    /// succeeded: the authority is neither of the collection's delegate holders, so MPL Core
    /// would have answered `0x9` had any record rejected. The `frozen: false` matters as much as
    /// the rest — this asserts the cohort is settleable **today**, and the moment the issuer sets
    /// that flag the verdict must become `Frozen` instead, which the row below drives.
    #[test]
    fn the_live_collector_crypt_collection_registry_reads_settleable() {
        // The six records the collection carries on mainnet, in the order the registry lists
        // them. Authorities are not reproduced: the census reads `plugin_type` and, for the two
        // records whose body decides, the body — never who holds the plugin.
        fn collector_crypt_records(frozen: bool) -> Vec<(Plugin, Option<u8>)> {
            vec![
                (Plugin::BubblegumV2(BubblegumV2 {}), None),
                (
                    Plugin::PermanentTransferDelegate(PermanentTransferDelegate {}),
                    None,
                ),
                (
                    Plugin::PermanentBurnDelegate(PermanentBurnDelegate {}),
                    None,
                ),
                (
                    Plugin::PermanentFreezeDelegate(PermanentFreezeDelegate { frozen }),
                    None,
                ),
                (
                    Plugin::Royalties(Royalties {
                        basis_points: 200,
                        creators: vec![Creator {
                            address: key(ISSUER),
                            percentage: 100,
                        }],
                        rule_set: RuleSet::None,
                    }),
                    None,
                ),
                (
                    Plugin::UpdateDelegate(UpdateDelegate {
                        additional_delegates: vec![key(ISSUER)],
                    }),
                    None,
                ),
            ]
        }

        let card = account_data_full(
            &base_asset(key(VAULT)),
            &[(
                Plugin::PermanentFreezeDelegate(PermanentFreezeDelegate { frozen: false }),
                None,
            )],
            None,
        );

        let live = account_data_full(&collection_body(), &collector_crypt_records(false), None);
        let read = collateral_case!(
            owner_program = MPL_CORE_ID,
            data = card.clone(),
            collection_key = key(COLLECTION),
            collection_data = live.clone(),
        )
        .unwrap();
        assert_eq!(
            describe(read).0,
            "settleable",
            "the live Collector Crypt registry must read settleable — anything else makes every \
             card in the production cohort closeable by a stranger"
        );

        // The consequence, driven through the gate that would do the damage rather than left as
        // a claim about the classification.
        let (_, unreachable) = gates_over!(
            owner_program = MPL_CORE_ID,
            data = card.clone(),
            collection_data = live,
        );
        // Matched rather than unwrapped: `SeizureKind` carries no `Debug`, and an `Ok` here is
        // the whole defect — a stranger having been handed a cause for a healthy card.
        let refusal = match unreachable {
            Ok(_) => panic!(
                "close_seized was handed a seizure cause for the live production registry — \
                 that is any funded wallet ending any deposit in the cohort"
            ),
            Err(err) => err,
        };
        assert_eq!(
            error_code(refusal),
            6308,
            "close_seized must refuse a healthy production card with CollateralPresent"
        );

        // And the same registry with the issuer's freeze flag set: the one record here that can
        // still stop a transfer, so the verdict has to move and the cause has to be the fact.
        let frozen = account_data_full(&collection_body(), &collector_crypt_records(true), None);
        assert_eq!(
            describe(
                collateral_case!(
                    owner_program = MPL_CORE_ID,
                    data = card,
                    collection_key = key(COLLECTION),
                    collection_data = frozen,
                )
                .unwrap()
            )
            .0,
            "frozen",
            "the freeze flag is the live registry's one live blocker, and it must still bite"
        );
    }

    /// **A `Royalties` record is decided by its rule set, on either account** — stated as a
    /// table so a future edit cannot quietly collapse it back to the plugin type.
    ///
    /// `RuleSet::None` has no branch that can reject: MPL Core evaluates nothing, so the record
    /// cannot hold up a transfer and the card stays settleable. An allow list and a deny list
    /// both reject on some caller, and *which* caller is a question about the invoking program's
    /// identity that this program cannot answer for MPL Core — undecidable, and admitted by both
    /// gates. Both halves are measured on localnet by `core-transfer-refused.test.ts`: its
    /// fixture collection is created with `rule_set: None` and every deposit in the file drives a
    /// real `TransferV1` through it, and flipping that same collection to a deny list is what
    /// produces the `0x9` the file exists to record.
    #[test]
    fn a_royalties_record_is_decided_by_its_rule_set_and_not_by_its_type() {
        fn royalties(rule_set: RuleSet) -> Plugin {
            Plugin::Royalties(Royalties {
                basis_points: 200,
                creators: vec![],
                rule_set,
            })
        }

        for (label, rule_set, expected) in [
            ("no rule set", RuleSet::None, "settleable"),
            (
                "an allow list",
                RuleSet::ProgramAllowList(vec![key(ISSUER)]),
                "undecidable",
            ),
            (
                "a deny list",
                RuleSet::ProgramDenyList(vec![key(ISSUER)]),
                "undecidable",
            ),
        ] {
            // On the asset, and on the collection — the record sits on either, and the census
            // must read the body of whichever account it walked.
            assert_eq!(
                describe(
                    collateral_case!(
                        owner_program = MPL_CORE_ID,
                        data =
                            asset_data(&base_asset(key(VAULT)), Some(royalties(rule_set.clone()))),
                    )
                    .unwrap()
                )
                .0,
                expected,
                "{label} on the asset"
            );
            assert_eq!(
                describe(
                    collateral_case!(
                        owner_program = MPL_CORE_ID,
                        data = settleable_data(),
                        collection_key = key(COLLECTION),
                        collection_data = collection_data(Some(royalties(rule_set))),
                    )
                    .unwrap()
                )
                .0,
                expected,
                "{label} on the collection"
            );
        }
    }

    /// **And the inert records do not reach it**, which is the half that keeps the arm from
    /// swallowing every position. A `PermanentTransferDelegate` is exactly what makes a
    /// third-party seizure constructible and it cannot *stop* a transfer; an external adapter
    /// that declares checks on other lifecycle events is never asked about one.
    /// Without these rows the census could pass by classifying everything undecidable, which
    /// would make every healthy position closeable by a stranger.
    #[test]
    fn inert_records_still_read_as_settleable() {
        for (label, data) in [
            (
                "a permanent transfer delegate",
                asset_data(
                    &base_asset(key(VAULT)),
                    Some(Plugin::PermanentTransferDelegate(
                        PermanentTransferDelegate {},
                    )),
                ),
            ),
            (
                "an unset permanent freeze",
                asset_data(
                    &base_asset(key(VAULT)),
                    Some(Plugin::PermanentFreezeDelegate(PermanentFreezeDelegate {
                        frozen: false,
                    })),
                ),
            ),
            (
                "an external adapter with no Transfer check",
                account_data_full(
                    &base_asset(key(VAULT)),
                    &[],
                    Some(ExternalFixture {
                        plugin_type: 0, // LifecycleHook
                        checks: Some(vec![(
                            HookableLifecycleEvent::Burn as u8,
                            ExternalCheckResult { flags: 4 },
                        )]),
                    }),
                ),
            ),
            (
                "an external adapter that declares nothing",
                account_data_full(
                    &base_asset(key(VAULT)),
                    &[],
                    Some(ExternalFixture {
                        plugin_type: 2, // AppData
                        checks: None,
                    }),
                ),
            ),
        ] {
            assert_eq!(
                describe(collateral_case!(owner_program = MPL_CORE_ID, data = data).unwrap()).0,
                "settleable",
                "{label} cannot hold up a transfer, so a card carrying it must still be \
                 withdrawable — and must not be closeable by a stranger"
            );
        }
    }

    /// **The whole type table, byte by byte, on both accounts, with its shape pinned.**
    /// `internal_transfer_verdict` is exhaustive over `PluginType` at compile time, which catches
    /// a variant *added* — but not a variant silently reclassified, and not the count drifting.
    /// So the partition is asserted here, and it is asserted **per account kind**, because one
    /// plugin type no longer has one answer.
    ///
    /// | | `Clear` | `Freeze` | `RuleSet` | `Undecidable` |
    /// |---|---|---|---|---|
    /// | on an asset | 14 | 2 | 1 | 1 (`BubblegumV2`) |
    /// | on a collection | 15 | 2 | 1 | 0 |
    ///
    /// **The single-cell difference is the finding this test exists to hold.** Classifying
    /// `BubblegumV2` as undecidable on *both* accounts is what put the entire production cohort
    /// into the catch-all — mainnet's `CCryptUfe…9CRac` carries that record at the collection
    /// level — while an owner-signed `TransferV1` inside that collection succeeded on chain. A
    /// future edit that "simplifies" the two rows back into one reds here.
    ///
    /// **The zero is load-bearing too.** No plugin type in this build is undecidable on a
    /// collection, so an admitted collection reaching `Undecidable` now takes an external
    /// adapter, an unreadable body, or a plugin type from a later `mpl-core` — which is the bound
    /// [`require_collateral_unreachable`]'s doc claims, restored to being true.
    ///
    /// **The last two assertions are the ones that matter most.** `mpl-core 0.11.1` ships
    /// eighteen plugin types, so byte 18 is claimed by nothing — exactly the case the census
    /// exists to handle safely. If a later `mpl-core` claims it, this reds *and* the match above
    /// fails to compile, and the new type has to be classified in both places rather than
    /// inheriting a default.
    #[test]
    fn the_plugin_type_table_partitions_per_account_kind() {
        for (label, on, expected) in [
            ("asset", AccountKind::Asset, (14, 2, 1, 1)),
            ("collection", AccountKind::Collection, (15, 2, 1, 0)),
        ] {
            let (mut inert, mut freezes, mut rule_sets, mut undecidable) = (0, 0, 0, 0);
            for byte in 0u8..18 {
                assert!(
                    PluginType::from_u8(byte).is_some(),
                    "byte {byte} is not a PluginType in mpl-core 0.11.1 — the range is wrong"
                );
                match internal_transfer_verdict(byte, on) {
                    RecordVerdict::Clear => inert += 1,
                    RecordVerdict::Freeze(_) => freezes += 1,
                    RecordVerdict::RuleSet => rule_sets += 1,
                    RecordVerdict::Undecidable => undecidable += 1,
                }
            }
            assert_eq!(
                (inert, freezes, rule_sets, undecidable),
                expected,
                "{label}: the table's partition moved — a plugin reclassified from undecidable \
                 to inert is a transfer this program now promises will succeed, and one moved \
                 the other way is a healthy position a stranger may close"
            );

            assert_eq!(
                internal_transfer_verdict(18, on),
                RecordVerdict::Undecidable,
                "{label}: byte 18 is claimed by no PluginType in this build, and the safe \
                 default for it is the whole point of the census"
            );
            assert_eq!(
                internal_transfer_verdict(u8::MAX, on),
                RecordVerdict::Undecidable
            );
        }

        // The one cell the table above summarises, named directly rather than left to a count:
        // a count can be kept whole by two errors that cancel.
        assert_eq!(
            internal_transfer_verdict(PluginType::BubblegumV2 as u8, AccountKind::Collection),
            RecordVerdict::Clear,
            "collection-level BubblegumV2 admits compressed mints alongside uncompressed ones \
             and does not speak for an AssetV1's transfer — measured on mainnet, tx 2hRzZ4Kv…h5Qu"
        );
        assert_eq!(
            internal_transfer_verdict(PluginType::BubblegumV2 as u8, AccountKind::Asset),
            RecordVerdict::Undecidable,
            "asset-level BubblegumV2 is unmeasured, and the conservative side of unmeasured is \
             the arm both gates admit"
        );
        assert_eq!(
            internal_transfer_verdict(PluginType::Royalties as u8, AccountKind::Asset),
            RecordVerdict::RuleSet,
            "Royalties is decided by its rule set on either account — never by its type"
        );
        assert_eq!(
            internal_transfer_verdict(PluginType::Royalties as u8, AccountKind::Collection),
            RecordVerdict::RuleSet
        );
    }

    /// **A set freeze outranks an undecidable record, and the precedence is for the audit
    /// trail.** A card that is both frozen and carrying a rule set is blocked either way and the
    /// remedy is the same, so `PositionSeized.kind` should carry the cause that is a fact. Driven
    /// with the freeze on the *collection* and the rule set on the *asset*, so the precedence is
    /// asserted across the two accounts rather than within one registry.
    #[test]
    fn a_set_freeze_outranks_an_undecidable_record_on_either_account() {
        let royalties = Plugin::Royalties(Royalties {
            basis_points: 0,
            creators: vec![],
            rule_set: RuleSet::ProgramDenyList(vec![]),
        });
        assert_eq!(
            describe(
                collateral_case!(
                    owner_program = MPL_CORE_ID,
                    data = asset_data(&base_asset(key(VAULT)), Some(royalties)),
                    collection_key = key(COLLECTION),
                    collection_data = collection_data(Some(Plugin::PermanentFreezeDelegate(
                        PermanentFreezeDelegate { frozen: true }
                    ))),
                )
                .unwrap()
            ),
            ("frozen", key(VAULT))
        );
    }

    // --- source pins --------------------------------------------------------------------------

    /// The CPI's parameter → builder-method mapping, which no call site can pin: transposing
    /// `.authority()` and `.new_owner()` sends the card to the signer and signs with the
    /// destination, and both compile. `.collection(Some(collection))` is here because MPL Core
    /// takes it as an optional account and a `None` skips the collection's own plugin checks.
    #[test]
    fn every_transfer_v1_builder_argument_is_bound_to_its_own_parameter() {
        assert!(flat(CORE_ASSET_SRC).contains(
            "pub fn transfer_core<'info>( asset: &AccountInfo<'info>, \
             collection: Option<&AccountInfo<'info>>, new_owner: &AccountInfo<'info>, \
             authority: &AccountInfo<'info>, payer: &AccountInfo<'info>, \
             system_program: &AccountInfo<'info>, mpl_core_program: &AccountInfo<'info>, \
             signer_seeds: &[&[&[u8]]], ) -> Result<()> { \
             TransferV1CpiBuilder::new(mpl_core_program) .asset(asset) \
             .collection(collection) .new_owner(new_owner) .authority(Some(authority)) \
             .payer(payer) .system_program(Some(system_program)) \
             .invoke_signed(signer_seeds)?;"
        ));
        // `.collection(collection)` passes through what the caller resolved; a `Some(collection)`
        // here would re-introduce the bug this parameter exists to fix, by supplying a collection
        // account for an asset that no longer names one — which MPL Core refuses.
        assert_eq!(
            production(CORE_ASSET_SRC)
                .matches(".collection(Some(")
                .count(),
            0,
            "the CPI's collection is resolved by read_collateral, never wrapped at the builder"
        );
        assert_eq!(
            production(CORE_ASSET_SRC)
                .matches("TransferV1CpiBuilder::new(")
                .count(),
            1,
            "a second builder chain would carry its own unpinned parameter mapping"
        );
    }

    /// **The exit leg's own parameter mapping, and the vault seeds it builds** — neither of
    /// which any call site can pin. `depositor` and `position_vault` are adjacent parameters of
    /// identical type, and transposed they hand `transfer_core` a `new_owner` of the vault and
    /// an `authority` of the depositor: the card stays where it is, the depositor's signature is
    /// demanded for an account they do not own, and every one of the three exits closes its
    /// position on the way. The seed array is the other half — `POSITION_VAULT_SEED` with the
    /// position's key and the **stored** bump is the only signature MPL Core will accept for an
    /// asset the vault owns.
    #[test]
    fn the_exit_leg_binds_every_argument_and_builds_the_vault_seeds() {
        assert!(flat(CORE_ASSET_SRC).contains(
            "let vault_seeds: &[&[u8]] = &[POSITION_VAULT_SEED, position.as_ref(), &[vault_bump]]; transfer_core( asset, collection, depositor, position_vault, payer, system_program, mpl_core_program, &[vault_seeds], )"
        ));
        assert_eq!(
            production(CORE_ASSET_SRC)
                .matches("fn release_core_from_vault")
                .count(),
            1,
            "a second exit leg would carry its own unpinned argument order"
        );
        // Counted over the code with the doc comments stripped: this module's prose *names*
        // `release_core_from_vault(..).is_ok()` on purpose — recording why that attempt is not
        // made here is the whole content of the decision — and a raw count reads that
        // explanation as the call it denies.
        assert_eq!(
            without_doc_comments(production(CORE_ASSET_SRC))
                .matches("release_core_from_vault(")
                .count(),
            0,
            "declared here and called only by the three exits — a *call site* inside this \
             module would be that attempt, and `core-transfer-refused.test.ts` measures that a \
             refused CPI cannot be read by its caller at all"
        );
        // The positive control: the leg is still declared, so the absence above is about call
        // sites and not about the function having been deleted. The declaration reads
        // `fn release_core_from_vault<'info>(`, which is why it does not match the form counted.
        assert_eq!(
            production(CORE_ASSET_SRC)
                .matches("pub fn release_core_from_vault")
                .count(),
            1
        );
    }

    /// **And it is the only place in the crate that signs as the vault.** Every accounts struct
    /// derives `[POSITION_VAULT_SEED, position]` to pin an address; what must not be duplicated
    /// is the *signer* form — the seed array with a bump appended, which is a custody
    /// authorisation rather than a derivation. Swept over `src/` rather than asserted about a
    /// fixed file list, which a future file could be added without. Carries its own positive
    /// control, since an absence test over stripped text reports a clean crate forever if the
    /// stripping breaks.
    #[test]
    fn no_production_code_outside_this_module_signs_as_the_position_vault() {
        let files = crate_sources();
        assert!(
            files.len() > 20,
            "the sweep found {} files — it is not reading the crate",
            files.len()
        );

        let mut offenders = Vec::new();
        for file in &files {
            let text = fs::read_to_string(file).expect("readable source");
            if file.file_name().is_some_and(|n| n == "core_asset.rs") {
                continue;
            }
            if production(&text).contains("&[POSITION_VAULT_SEED") {
                offenders.push(file.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "these files build vault signer seeds outside release_core_from_vault: {}",
            offenders.join(", ")
        );
    }

    /// The positive control for the sweep above.
    #[test]
    fn the_vault_signer_sweep_would_see_a_reintroduced_signature() {
        let reintroduced = "fn release() { let s: &[&[u8]] = &[POSITION_VAULT_SEED, k, &[b]]; }\n#[cfg(test)]\nmod t {}";
        assert!(production(reintroduced).contains("&[POSITION_VAULT_SEED"));
        assert!(
            production(CORE_ASSET_SRC).contains("&[POSITION_VAULT_SEED"),
            "and the home file must still hold the one legitimate occurrence"
        );
    }

    /// **The plugin-body surface, by call form: three reads, each bound to its own account
    /// type.** Every one goes through a named predicate the census calls only for the plugin
    /// types where the *body* is the answer — two freeze flags and one rule set — and in each the
    /// type parameter and the account are independent, so
    /// `fetch_plugin::<BaseCollectionV1, _>(asset_info, ..)` compiles and would silently read the
    /// wrong registry. Both predicates take the census's own generic parameter, which is what
    /// ties each read to the account that was walked.
    ///
    /// **The third read is the fix for classifying `Royalties` by type.** It is pinned by call
    /// form beside the freezes rather than counted loosely, because it is the same hazard: a
    /// rule set read off the collection while the asset's registry was the one walked would
    /// answer a question nobody asked.
    ///
    /// **The two Execute freezes are named in the census's table and never at a call site**, and
    /// the pin is on the call form for that reason: this module's documentation names them on
    /// purpose — saying which plugins deliberately do not gate Transfer is the whole content of
    /// the decision — so a name-based pin would forbid the explanation along with the code.
    #[test]
    fn exactly_the_three_transfer_gating_body_reads_are_made() {
        let prod = production(CORE_ASSET_SRC);
        assert_eq!(prod.matches("fetch_plugin::<").count(), 3);
        assert!(flat(CORE_ASSET_SRC).contains(
            "fetch_plugin::<T, Royalties>(info, PluginType::Royalties) .ok() .map(|(_, \
             royalties, _)| match royalties.rule_set {"
        ));
        assert_eq!(
            prod.matches("fn royalty_rule_set_is_inert").count(),
            1,
            "a second rule-set predicate is a second answer to whether a royalty record bites"
        );
        assert!(flat(CORE_ASSET_SRC).contains(
            "FreezeKind::Delegate => fetch_plugin::<T, FreezeDelegate>(info, \
             kind.plugin_type()) .ok() .map(|(_, p, _)| p.frozen),"
        ));
        assert!(flat(CORE_ASSET_SRC).contains(
            "FreezeKind::Permanent => { fetch_plugin::<T, PermanentFreezeDelegate>(info, \
             kind.plugin_type()) .ok() .map(|(_, p, _)| p.frozen) }"
        ));
        assert_eq!(
            prod.matches("fn frozen_flag").count(),
            1,
            "a second freeze predicate is a second answer to whether a card can leave"
        );
        // The census is what walks both accounts, and it must stay the only thing that does: a
        // second walk could drop the collection's registry from one direction of the
        // classification while keeping it in the other.
        // Each walk is pinned with its `AccountKind` as well as its body type, because the two
        // must agree: a collection walked as `AccountKind::Asset` would send the production
        // cohort's `BubblegumV2` back to `Undecidable`, which is the defect this pair guards.
        assert!(flat(CORE_ASSET_SRC).contains(
            "let asset = registry_verdict::<BaseAssetV1>(asset_info, AccountKind::Asset);"
        ));
        assert!(flat(CORE_ASSET_SRC).contains(
            "let collection = collection_info .map(|info| registry_verdict::<BaseCollectionV1>(info, AccountKind::Collection))"
        ));
        assert_eq!(
            prod.matches("fn registry_verdict").count(),
            1,
            "one census, walked over both account types"
        );
        assert_eq!(
            prod.matches("PluginRegistryV1Safe::from_bytes").count(),
            1,
            "and one place the registry is deserialized — the reach this module's header bounds"
        );
        // The banned pair, still banned: the census gets no closer to
        // `registry_records_to_plugin_list` than `fetch_plugin` already was.
        for banned in ["Asset::deserialize", "Collection::deserialize"] {
            assert_eq!(
                prod.matches(banned).count(),
                0,
                "{banned} is banned crate-wide"
            );
        }
    }

    /// The two classifications must stay wildcard-free, because a `_` arm is how a fourth
    /// `ReadableCause` would silently acquire a default treatment in a gate rather than a compile
    /// error at the one place that has to decide.
    #[test]
    fn neither_gate_carries_a_catch_all_over_the_classification() {
        let prod = production(CORE_ASSET_SRC);
        assert_eq!(prod.matches("_ =>").count(), 0);
        assert_eq!(prod.matches("_ if").count(), 0);
    }

    /// **The stack-overflow prohibition, enforced crate-wide rather than described.** The two
    /// hooked wrappers' `deserialize` functions are the only callers of the 4,224-byte-frame
    /// `registry_records_to_plugin_list`, and they are also the first API a developer reaches for
    /// to read a Core asset — which is why this is a test and not a comment. Nothing downstream
    /// can catch the fault: `cargo-build-sbf` prints the overflow prefixed `Error:` and exits 0,
    /// and the stripped release `.so` reports no symbols for anything, so a symbol grep is a
    /// confident false negative.
    ///
    /// This is the crate's first pin that walks the filesystem instead of taking `include_str!`,
    /// and it has to be: "here and everywhere" cannot be asserted from one file's text, and a
    /// per-file list is a list someone adds a file without.
    ///
    /// **Each file's `#[cfg(test)]` half is out of scope, and deliberately.** The frame overflow
    /// is an SBF codegen fault in the deployed `.so`, which test code is never linked into — and
    /// this module's own fixtures could not name the banned functions otherwise.
    ///
    /// **The open paren is load-bearing.** The scan reads the production half of raw source, so
    /// naming the banned functions in prose would red it; matching the *call* form lets this
    /// module's own documentation say what is banned. Whoever finds a way to be called without
    /// one should tighten this rather than delete it.
    #[test]
    fn no_source_file_calls_the_two_stack_overflowing_core_deserializers() {
        for path in crate_sources() {
            let source = fs::read_to_string(&path).unwrap();
            for banned in ["Asset::deserialize(", "Collection::deserialize("] {
                assert_eq!(
                    production(&source).matches(banned).count(),
                    0,
                    "{} calls {banned} — the only path to mpl-core's 4,224-byte stack frame, \
                     which the build reports at exit code 0 and the stripped .so cannot be \
                     grepped for. Read with BaseAssetV1::from_bytes and fetch_plugin instead.",
                    path.display()
                );
            }
        }
    }

    /// A positive control for the walk above: the same predicate over a fabricated source must
    /// fire. Without it, a `crate_sources` that silently returned the wrong set — or a
    /// `production` that truncated every file at its first line — would report the crate clean.
    #[test]
    fn the_prohibition_predicate_fires_on_a_file_that_violates_it() {
        let violating = "fn read(a: &AccountInfo) { let x = Asset::deserialize(a).unwrap(); }\n";
        assert_eq!(
            production(violating).matches("Asset::deserialize(").count(),
            1
        );
        let documenting = "//! never call Asset::deserialize on an account\nfn read() {}\n";
        assert_eq!(
            production(documenting)
                .matches("Asset::deserialize(")
                .count(),
            0,
            "prose naming the ban must not red the scan; that is what the open paren buys"
        );
    }
}
