use anchor_lang::error::ErrorCode;
use anchor_lang::prelude::*;
use anchor_spl::associated_token::{
    create_idempotent, get_associated_token_address, Create as CreateAta,
};
use anchor_spl::token::{transfer, Transfer, ID as SPL_TOKEN_ID};
use mpl_token_metadata::accounts::{MasterEdition, Metadata};
use mpl_token_metadata::instructions::TransferV1CpiBuilder;
use mpl_token_metadata::types::{Key as MetaKey, TokenStandard};
use mpl_token_metadata::ID as TOKEN_METADATA_ID;

use crate::common::errors::ByeMachineError;
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::state::PoolCollection;

/// Moves one pNFT via Metaplex `TransferV1`. `signer_seeds` is empty for a user-signed transfer
/// (deposit) and the position PDA's own seeds for a program-signed transfer (`return_rejected`,
/// `withdraw`) — the same helper serves both custody directions.
#[allow(clippy::too_many_arguments)]
pub fn transfer_pnft<'info>(
    source_token: &AccountInfo<'info>,
    source_owner: &AccountInfo<'info>,
    destination_token: &AccountInfo<'info>,
    destination_owner: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    metadata: &AccountInfo<'info>,
    edition: &AccountInfo<'info>,
    source_token_record: &AccountInfo<'info>,
    destination_token_record: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    sysvar_instructions: &AccountInfo<'info>,
    spl_token_program: &AccountInfo<'info>,
    spl_ata_program: &AccountInfo<'info>,
    token_metadata_program: &AccountInfo<'info>,
    authorization_rules_program: Option<&AccountInfo<'info>>,
    authorization_rules: Option<&AccountInfo<'info>>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    TransferV1CpiBuilder::new(token_metadata_program)
        .token(source_token)
        .token_owner(source_owner)
        .destination_token(destination_token)
        .destination_owner(destination_owner)
        .mint(mint)
        .metadata(metadata)
        .edition(Some(edition))
        .token_record(Some(source_token_record))
        .destination_token_record(Some(destination_token_record))
        .authority(authority)
        .payer(payer)
        .system_program(system_program)
        .sysvar_instructions(sysvar_instructions)
        .spl_token_program(spl_token_program)
        .spl_ata_program(spl_ata_program)
        .authorization_rules_program(authorization_rules_program)
        .authorization_rules(authorization_rules)
        .amount(1)
        .invoke_signed(signer_seeds)?;
    Ok(())
}

/// Moves one legacy `NonFungible` (standard 1) with SPL Token's own `Transfer`. `signer_seeds` is
/// empty for a user-signed transfer (deposit) and the position PDA's own seeds for a
/// program-signed one, exactly as [`transfer_pnft`] — the two helpers are the same custody move
/// under the two standards a Token Metadata collection can be admitted for.
///
/// A legacy NFT has no token record and no authorization rules, so the five accounts
/// [`transfer_pnft`] needs for those are **absent** here rather than passed as `None`: routing a
/// legacy mint through `TransferV1` would require a token record Token Metadata never created.
/// `amount` is `1` for the reason it is there too — a `0` moves nothing and leaves the NFT in a
/// vault its owner is about to close.
pub fn transfer_spl<'info>(
    source_token: &AccountInfo<'info>,
    destination_token: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    transfer(
        CpiContext::new_with_signer(
            token_program.clone(),
            Transfer {
                from: source_token.clone(),
                to: destination_token.clone(),
                authority: authority.clone(),
            },
            signer_seeds,
        ),
        1,
    )
}

/// The five accounts a pNFT release needs and a legacy one must not have — the
/// "required-absent" set, carried as **one named-field value** rather than five more positionals.
///
/// The grouping is the point. `transfer_pnft` takes 16 same-typed positional `&AccountInfo` and
/// every one of its call sites needs an ordered-literal pin because a transposition compiles;
/// adding five more optionals to a release helper would extend that hazard rather than contain it.
/// Named fields cannot be transposed, and the set gets one place to be checked present or absent
/// instead of one per instruction.
pub struct PnftRelease<'a, 'info> {
    pub position_token_record: Option<&'a AccountInfo<'info>>,
    pub depositor_token_record: Option<&'a AccountInfo<'info>>,
    pub sysvar_instructions: Option<&'a AccountInfo<'info>>,
    pub authorization_rules_program: Option<&'a AccountInfo<'info>>,
    pub authorization_rules: Option<&'a AccountInfo<'info>>,
}

impl PnftRelease<'_, '_> {
    /// Every one of the five is absent. The legacy branch's guard, as one call: five separate
    /// `require!`s at three call sites is fifteen chances to check four.
    fn all_absent(&self) -> bool {
        self.position_token_record.is_none()
            && self.depositor_token_record.is_none()
            && self.sysvar_instructions.is_none()
            && self.authorization_rules_program.is_none()
            && self.authorization_rules.is_none()
    }
}

/// The depositor exit leg, branched on the **stored** standard — the mirror move, shared
/// by `return_rejected`, `withdraw` and `claim_nft` because a second copy of a custody predicate
/// is a custody bug.
///
/// **The legacy branch pins the destination to the depositor's own ATA and then creates it, and
/// the pin is a guard rather than a convenience.** SPL `Transfer` validates the *source* authority
/// and says nothing about who owns the destination, so a bare `transfer_spl` to an
/// `UncheckedAccount` would send the card to any token account for the mint — and
/// `return_rejected` is **permissionless**, so the caller choosing that account is not the
/// depositor.
///
/// **The derivation is asserted here rather than deferred to the CPI, and that is the whole of
/// the change.** `create_idempotent` does check it — the Associated Token program compares the
/// account against `PDA([wallet, token_program, mint])` and answers `InvalidSeeds` otherwise —
/// so this `require_keys_eq!` is not a second check of a different fact. It is the same fact
/// stated where the custody decision is made, because the alternative is a custody guard whose
/// only expression is a side effect of a CPI made for another reason: an edit that reordered the
/// two calls, made the creation conditional on the account already existing, or moved to a
/// creation helper that skips the comparison would delete the guard while every test that only
/// checks the happy path stayed green. The explicit form fails with **this** crate's
/// `NotPositionOwner` (6302) rather than the ATA program's `InvalidSeeds`, which also tells a
/// caller what they got wrong.
///
/// The pNFT branch needs no equivalent: Token Metadata's `TransferV1` checks `destination_owner`
/// against the destination token account itself, and creates the ATA too. Creation is idempotent,
/// so the ordinary case — the depositor still holds the account the card was deposited from —
/// costs one no-op CPI.
#[allow(clippy::too_many_arguments)]
pub fn release_from_vault<'info>(
    standard: u8,
    position_vault: &AccountInfo<'info>,
    position: &AccountInfo<'info>,
    depositor_token: &AccountInfo<'info>,
    depositor: &AccountInfo<'info>,
    nft_mint: &AccountInfo<'info>,
    metadata: &AccountInfo<'info>,
    master_edition: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    associated_token_program: &AccountInfo<'info>,
    token_metadata_program: &AccountInfo<'info>,
    pnft: PnftRelease<'_, 'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    match standard {
        STANDARD_PNFT => transfer_pnft(
            position_vault,
            position,
            depositor_token,
            depositor,
            nft_mint,
            metadata,
            master_edition,
            pnft.position_token_record
                .ok_or(ErrorCode::AccountNotEnoughKeys)?,
            pnft.depositor_token_record
                .ok_or(ErrorCode::AccountNotEnoughKeys)?,
            position,
            payer,
            system_program,
            pnft.sysvar_instructions
                .ok_or(ErrorCode::AccountNotEnoughKeys)?,
            token_program,
            associated_token_program,
            token_metadata_program,
            pnft.authorization_rules_program,
            pnft.authorization_rules,
            signer_seeds,
        ),
        STANDARD_LEGACY => {
            require!(pnft.all_absent(), ErrorCode::ConstraintRaw);
            // The destination is the depositor's own associated token account and no other —
            // stated before the CPI that would otherwise be the only thing establishing it.
            require_keys_eq!(
                depositor_token.key(),
                get_associated_token_address(&depositor.key(), &nft_mint.key()),
                ByeMachineError::NotPositionOwner
            );
            create_idempotent(CpiContext::new(
                associated_token_program.clone(),
                CreateAta {
                    payer: payer.clone(),
                    associated_token: depositor_token.clone(),
                    authority: depositor.clone(),
                    mint: nft_mint.clone(),
                    system_program: system_program.clone(),
                    token_program: token_program.clone(),
                },
            ))?;
            transfer_spl(
                position_vault,
                depositor_token,
                position,
                token_program,
                signer_seeds,
            )
        }
        // `standard` is a `u8`, so the arm is required by the match. Each caller's accounts struct
        // already refuses anything outside {0, 1} before the handler runs; this rejects rather
        // than falling through, because falling through closes a position whose card never moved.
        _ => Err(ByeMachineError::WrongStandardForInstruction.into()),
    }
}

/// The collection the **asset itself** declares, verified against `metadata.collection.key`.
///
/// It returns the address rather than comparing it against one, because step 5 derives the
/// `PoolCollection` record from it: a comparison against a caller-supplied collection would let
/// the caller choose which record the asset is checked against, which is the substitution the
/// whole per-collection design exists to make impossible.
fn declared_collection(metadata: &Metadata) -> Result<Pubkey> {
    let collection = metadata
        .collection
        .as_ref()
        .ok_or(ByeMachineError::CollectionNotAdmitted)?;
    require!(collection.verified, ByeMachineError::CollectionNotAdmitted);
    Ok(collection.key)
}

/// The `token_standard` match: the **closed** set
/// `{ProgrammableNonFungible → 0, NonFungible → 1}`, everything else rejected.
///
/// Wildcard-free on purpose, and the absence of a catch-all arm is the load-bearing part. A
/// wildcard arm resolving to the legacy standard would route `FungibleAsset`, a print edition, or
/// a Token Metadata standard that does not exist yet onto the permissive SPL path — a fungible
/// balance or an edition print escrowed as if it were the card. Enumerating every variant also
/// means a new one in a later `mpl-token-metadata` **fails to compile here** rather than
/// resolving to a standard nobody chose.
///
/// The catch-all is written out nowhere in this file, prose included: the pin that enforces its
/// absence reads raw source, so an illustration of the fault would red it. Describing the arm
/// instead of writing it is the cost of a pin with no parser in it.
fn resolve_standard(metadata: &Metadata) -> Result<u8> {
    match metadata.token_standard {
        Some(TokenStandard::ProgrammableNonFungible) => Ok(STANDARD_PNFT),
        Some(TokenStandard::NonFungible) => Ok(STANDARD_LEGACY),
        Some(TokenStandard::FungibleAsset)
        | Some(TokenStandard::Fungible)
        | Some(TokenStandard::NonFungibleEdition)
        | Some(TokenStandard::ProgrammableNonFungibleEdition)
        | None => Err(ByeMachineError::StandardNotAdmitted.into()),
    }
}

/// `data` must deserialize as a `MetaKey::MasterEditionV2` master edition. A `MasterEditionV1`
/// account shares `MasterEdition`'s leading fields and deserializes without error, so a
/// successful parse alone does not distinguish them — only this key check does. MPL-Core has no
/// master edition at all and fails to parse.
fn check_master_edition_v2(data: &[u8]) -> Result<()> {
    let edition =
        MasterEdition::from_bytes(data).map_err(|_| ByeMachineError::StandardNotAdmitted)?;
    require!(
        edition.key == MetaKey::MasterEditionV2,
        ByeMachineError::StandardNotAdmitted
    );
    Ok(())
}

/// The full on-chain admission gate for a Token Metadata deposit — the family whose assets are
/// SPL mints — returning **the standard it resolved**, which the
/// caller writes to `Position.standard` once and never recomputes.
///
/// Step 1 keys on `mint_info`'s **owner program**, not on its data: an MPL-Core asset presents no
/// SPL mint and a compressed leaf presents no account at all, so both are refused here before
/// anything is parsed. That is what makes the Bubblegum exclusion structural rather than a
/// blocklist — there is nothing for a DAS label to lie about on this path.
///
/// Step 5 is the reason `pool` and the record arrive as three separate arguments. The record's
/// address is re-derived from **this pool's** key and the collection the *asset* declares, so
/// another pool's record for this collection and this pool's record for another collection both
/// fail the derivation, and a Core asset presented against a Token-Metadata collection's record
/// fails the standards bit even though its collection is admitted — the wrong-pairing case. The
/// record's own `pool` and `collection` fields are deliberately **not** re-compared: both are the
/// seeds `admit_collection` created the account under, so the derivation already pins them, and a
/// second comparison against fields the derivation controls would read as a check while asserting
/// nothing new.
pub fn validate_admission(
    mint_info: &AccountInfo,
    metadata_info: &AccountInfo,
    master_edition_info: &AccountInfo,
    pool: &Pubkey,
    record_key: &Pubkey,
    record: &PoolCollection,
) -> Result<u8> {
    require_keys_eq!(
        *mint_info.owner,
        SPL_TOKEN_ID,
        ByeMachineError::StandardNotAdmitted
    );
    let mint = mint_info.key();

    require_keys_eq!(
        *metadata_info.owner,
        TOKEN_METADATA_ID,
        ByeMachineError::StandardNotAdmitted
    );
    let (expected_metadata, _) = Metadata::find_pda(&mint);
    require_keys_eq!(
        metadata_info.key(),
        expected_metadata,
        ByeMachineError::StandardNotAdmitted
    );
    let metadata_data = metadata_info.try_borrow_data()?;
    let metadata =
        Metadata::from_bytes(&metadata_data).map_err(|_| ByeMachineError::StandardNotAdmitted)?;
    let standard = resolve_standard(&metadata)?;
    let collection = declared_collection(&metadata)?;
    drop(metadata_data);

    require_keys_eq!(
        *master_edition_info.owner,
        TOKEN_METADATA_ID,
        ByeMachineError::StandardNotAdmitted
    );
    let (expected_master_edition, _) = MasterEdition::find_pda(&mint);
    require_keys_eq!(
        master_edition_info.key(),
        expected_master_edition,
        ByeMachineError::StandardNotAdmitted
    );
    let master_edition_data = master_edition_info.try_borrow_data()?;
    check_master_edition_v2(&master_edition_data)?;
    drop(master_edition_data);

    PoolCollection::require_admits(pool, &collection, record_key, record, standard)?;

    Ok(standard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::standards::{
        DEFINED_STANDARDS, STANDARDS_BIT_CORE, STANDARDS_BIT_LEGACY, STANDARDS_BIT_PNFT,
    };
    use crate::common::test_support::{flat, production};
    use borsh::BorshSerialize;
    use mpl_token_metadata::types::Collection;

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    fn base_metadata() -> Metadata {
        Metadata {
            key: MetaKey::MetadataV1,
            update_authority: key(1),
            mint: key(2),
            name: "x".to_string(),
            symbol: "x".to_string(),
            uri: "x".to_string(),
            seller_fee_basis_points: 0,
            creators: None,
            primary_sale_happened: true,
            is_mutable: false,
            edition_nonce: Some(255),
            token_standard: Some(TokenStandard::ProgrammableNonFungible),
            collection: Some(Collection {
                verified: true,
                key: key(9),
            }),
            uses: None,
            collection_details: None,
            programmable_config: None,
        }
    }

    fn serialized_metadata(metadata: &Metadata) -> Vec<u8> {
        let mut buf = Vec::new();
        metadata.serialize(&mut buf).unwrap();
        buf
    }

    fn serialized_master_edition_v2() -> Vec<u8> {
        let edition = MasterEdition {
            key: MetaKey::MasterEditionV2,
            supply: 0,
            max_supply: None,
        };
        let mut buf = Vec::new();
        edition.serialize(&mut buf).unwrap();
        buf
    }

    /// The mint every admission case below derives its two PDAs from.
    const ADMITTED_MINT: u8 = 2;
    /// The collection `base_metadata` declares, and the one the admitted record is derived for.
    const ALLOWED_COLLECTION: u8 = 9;
    /// The pool the record is derived under. Distinct from every other fixture byte so a
    /// derivation that dropped the pool component fails the address comparison rather than
    /// colliding with it.
    const POOL: u8 = 40;

    fn pool_key() -> Pubkey {
        key(POOL)
    }

    /// The record account address a caller would have to present for `collection` under [`POOL`].
    fn record_key_for(collection: u8) -> Pubkey {
        PoolCollection::address(&pool_key(), &key(collection)).0
    }

    /// A record admitting `standards`. Its `pool` and `collection` fields are the ones
    /// `admit_collection` would have written; `validate_admission` reads neither, which is why
    /// the cases below vary the *address* rather than these fields.
    fn record(standards: u8) -> PoolCollection {
        PoolCollection {
            pool: pool_key(),
            collection: key(ALLOWED_COLLECTION),
            standards,
            admitted_at: 1,
            bump: 1,
        }
    }

    // --- the account-boundary wrapper: owner and PDA derivation, all four accounts -----------

    macro_rules! admission_case {
        (
            mint_owner = $mint_owner:expr,
            metadata_key = $metadata_key:expr,
            metadata_owner = $metadata_owner:expr,
            edition_key = $edition_key:expr,
            edition_owner = $edition_owner:expr,
            metadata = $metadata:expr,
            edition_data = $edition_data:expr,
            record_key = $record_key:expr,
            record = $record:expr $(,)?
        ) => {{
            let mint_key = key(ADMITTED_MINT);
            let mint_owner = $mint_owner;
            let metadata_key = $metadata_key;
            let metadata_owner = $metadata_owner;
            let edition_key = $edition_key;
            let edition_owner = $edition_owner;
            let mut mint_data = Vec::new();
            let mut metadata_data = serialized_metadata(&$metadata);
            let mut edition_data = $edition_data;
            let mut mint_lamports = 1u64;
            let mut metadata_lamports = 1u64;
            let mut edition_lamports = 1u64;
            let mint_info = AccountInfo::new(
                &mint_key,
                false,
                false,
                &mut mint_lamports,
                &mut mint_data,
                &mint_owner,
                false,
                0,
            );
            let metadata_info = AccountInfo::new(
                &metadata_key,
                false,
                false,
                &mut metadata_lamports,
                &mut metadata_data,
                &metadata_owner,
                false,
                0,
            );
            let edition_info = AccountInfo::new(
                &edition_key,
                false,
                false,
                &mut edition_lamports,
                &mut edition_data,
                &edition_owner,
                false,
                0,
            );
            validate_admission(
                &mint_info,
                &metadata_info,
                &edition_info,
                &pool_key(),
                &$record_key,
                &$record,
            )
        }};
    }

    fn metadata_pda() -> Pubkey {
        Metadata::find_pda(&key(ADMITTED_MINT)).0
    }

    fn edition_pda() -> Pubkey {
        MasterEdition::find_pda(&key(ADMITTED_MINT)).0
    }

    #[test]
    fn validate_admission_accepts_a_correctly_derived_token_metadata_pair() {
        assert_eq!(
            admission_case!(
                mint_owner = SPL_TOKEN_ID,
                metadata_key = metadata_pda(),
                metadata_owner = TOKEN_METADATA_ID,
                edition_key = edition_pda(),
                edition_owner = TOKEN_METADATA_ID,
                metadata = base_metadata(),
                edition_data = serialized_master_edition_v2(),
                record_key = record_key_for(ALLOWED_COLLECTION),
                record = record(DEFINED_STANDARDS),
            )
            .unwrap(),
            STANDARD_PNFT
        );
    }

    /// An MPL-Core asset or any other program's account fails here, before any parse.
    #[test]
    fn validate_admission_rejects_metadata_owned_by_another_program() {
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = metadata_pda(),
                    metadata_owner = key(77),
                    edition_key = edition_pda(),
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = base_metadata(),
                    edition_data = serialized_master_edition_v2(),
                    record_key = record_key_for(ALLOWED_COLLECTION),
                    record = record(DEFINED_STANDARDS),
                )
                .unwrap_err()
            ),
            6101
        );
    }

    /// A well-formed metadata account belonging to a *different* mint is rejected —
    /// the account must sit at the PDA the deposited mint derives.
    #[test]
    fn validate_admission_rejects_metadata_that_is_not_this_mints_pda() {
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = Metadata::find_pda(&key(200)).0,
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = edition_pda(),
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = base_metadata(),
                    edition_data = serialized_master_edition_v2(),
                    record_key = record_key_for(ALLOWED_COLLECTION),
                    record = record(DEFINED_STANDARDS),
                )
                .unwrap_err()
            ),
            6101
        );
    }

    #[test]
    fn validate_admission_rejects_a_master_edition_owned_by_another_program() {
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = metadata_pda(),
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = edition_pda(),
                    edition_owner = key(77),
                    metadata = base_metadata(),
                    edition_data = serialized_master_edition_v2(),
                    record_key = record_key_for(ALLOWED_COLLECTION),
                    record = record(DEFINED_STANDARDS),
                )
                .unwrap_err()
            ),
            6101
        );
    }

    #[test]
    fn validate_admission_rejects_a_master_edition_that_is_not_this_mints_pda() {
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = metadata_pda(),
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = MasterEdition::find_pda(&key(200)).0,
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = base_metadata(),
                    edition_data = serialized_master_edition_v2(),
                    record_key = record_key_for(ALLOWED_COLLECTION),
                    record = record(DEFINED_STANDARDS),
                )
                .unwrap_err()
            ),
            6101
        );
    }

    // --- the wrapper reaches each inner check ------------------------------------------------

    /// The record must be the one **this** collection derives under **this** pool. Presenting a
    /// different collection's record — the wrong-pairing case — fails the derivation, and
    /// it is the derivation that catches it: nothing compares the record's own `collection` field.
    #[test]
    fn validate_admission_rejects_a_record_derived_for_another_collection() {
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = metadata_pda(),
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = edition_pda(),
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = base_metadata(),
                    edition_data = serialized_master_edition_v2(),
                    record_key = record_key_for(99),
                    record = record(DEFINED_STANDARDS),
                )
                .unwrap_err()
            ),
            6100
        );
    }

    /// `NonFungible` cannot be this case's probe, since it is admitted as standard 1, so the
    /// probe here is a standard the closed set still rejects. Changing the probe rather than the assertion
    /// is the point: the case asserts the wrapper *reaches* `resolve_standard`, and a probe that
    /// has become admissible would have turned it green by admitting the fixture.
    #[test]
    fn validate_admission_reaches_the_standard_resolution() {
        let mut metadata = base_metadata();
        metadata.token_standard = Some(TokenStandard::FungibleAsset);
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = metadata_pda(),
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = edition_pda(),
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = metadata,
                    edition_data = serialized_master_edition_v2(),
                    record_key = record_key_for(ALLOWED_COLLECTION),
                    record = record(DEFINED_STANDARDS),
                )
                .unwrap_err()
            ),
            6101
        );
    }

    /// A legacy `NonFungible` from a collection admitted for it resolves to standard 1 and passes
    /// the same wrapper the pNFT does — the widening asserted end to end, not just in
    /// `resolve_standard`. `check_master_edition_v2` still runs on this path, which is why the
    /// fixture carries a V2 edition.
    #[test]
    fn validate_admission_accepts_a_legacy_non_fungible_and_resolves_standard_one() {
        let mut metadata = base_metadata();
        metadata.token_standard = Some(TokenStandard::NonFungible);
        assert_eq!(
            admission_case!(
                mint_owner = SPL_TOKEN_ID,
                metadata_key = metadata_pda(),
                metadata_owner = TOKEN_METADATA_ID,
                edition_key = edition_pda(),
                edition_owner = TOKEN_METADATA_ID,
                metadata = metadata,
                edition_data = serialized_master_edition_v2(),
                record_key = record_key_for(ALLOWED_COLLECTION),
                record = record(DEFINED_STANDARDS),
            )
            .unwrap(),
            STANDARD_LEGACY
        );
    }

    /// **The one case that sees which standard the wrapper hands the predicate.** Every other
    /// fixture in this module
    /// resolves to pNFT, or presents a record admitting all three — so `validate_admission`
    /// passing a *constant* `STANDARD_PNFT` in place of the value `resolve_standard` returned
    /// left the whole suite green. Here the asset is a legacy `NonFungible` and the record admits
    /// **legacy only**: a constant pNFT fails the bit, and only the resolved value passes.
    ///
    /// The fault predates the extraction — the same constant inlined at the old `mask_admits`
    /// call read identically — but one predicate with an argument is what made it nameable.
    #[test]
    fn validate_admission_passes_the_resolved_standard_and_not_a_constant() {
        let mut metadata = base_metadata();
        metadata.token_standard = Some(TokenStandard::NonFungible);
        assert_eq!(
            admission_case!(
                mint_owner = SPL_TOKEN_ID,
                metadata_key = metadata_pda(),
                metadata_owner = TOKEN_METADATA_ID,
                edition_key = edition_pda(),
                edition_owner = TOKEN_METADATA_ID,
                metadata = metadata,
                edition_data = serialized_master_edition_v2(),
                record_key = record_key_for(ALLOWED_COLLECTION),
                record = record(STANDARDS_BIT_LEGACY),
            )
            .unwrap(),
            STANDARD_LEGACY
        );
    }

    /// A print/V1 edition at the right PDA and owner still fails — the wrapper must run the
    /// version check, not just the account-boundary checks that precede it.
    #[test]
    fn validate_admission_reaches_the_master_edition_version_check() {
        let v1 = mpl_token_metadata::accounts::DeprecatedMasterEditionV1 {
            key: MetaKey::MasterEditionV1,
            supply: 0,
            max_supply: None,
            printing_mint: key(3),
            one_time_printing_authorization_mint: key(4),
        };
        let mut v1_data = Vec::new();
        v1.serialize(&mut v1_data).unwrap();

        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = metadata_pda(),
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = edition_pda(),
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = base_metadata(),
                    edition_data = v1_data,
                    record_key = record_key_for(ALLOWED_COLLECTION),
                    record = record(DEFINED_STANDARDS),
                )
                .unwrap_err()
            ),
            6101
        );
    }

    // --- the declared collection ------------------------------------------------------------

    #[test]
    fn declared_collection_returns_the_verified_collection_the_asset_names() {
        assert_eq!(declared_collection(&base_metadata()).unwrap(), key(9));
    }

    #[test]
    fn declared_collection_rejects_a_metadata_with_no_collection() {
        let mut metadata = base_metadata();
        metadata.collection = None;
        assert_eq!(
            error_code(declared_collection(&metadata).unwrap_err()),
            6100
        );
    }

    /// The half a caller controls. Anyone can mint an NFT naming any collection; only the
    /// collection authority can set `verified`, so without this check a card from a collection
    /// nobody admitted derives the admitted collection's record and passes.
    #[test]
    fn declared_collection_rejects_an_unverified_collection() {
        let mut metadata = base_metadata();
        metadata.collection = Some(Collection {
            verified: false,
            key: key(9),
        });
        assert_eq!(
            error_code(declared_collection(&metadata).unwrap_err()),
            6100
        );
    }

    // --- the closed standard set --------------------------------------------------------------

    /// Every `TokenStandard` `mpl-token-metadata 5.1.1` declares, not the two that pass, with
    /// each expectation written out rather than computed — an oracle that recomputed the match
    /// could not disagree with it. The `None` case is the seventh: a metadata account predating
    /// `token_standard` carries no standard at all and must not resolve to one.
    ///
    /// The six variants are exhaustive by construction, not by this list: `resolve_standard` is
    /// wildcard-free, so a seventh variant in a later `mpl-token-metadata` is a compile error
    /// there before it is a missing row here.
    #[test]
    fn resolve_standard_is_a_closed_two_element_set_over_every_token_standard() {
        let expected: [(Option<TokenStandard>, Option<u8>); 7] = [
            (Some(TokenStandard::NonFungible), Some(STANDARD_LEGACY)),
            (Some(TokenStandard::FungibleAsset), None),
            (Some(TokenStandard::Fungible), None),
            (Some(TokenStandard::NonFungibleEdition), None),
            (
                Some(TokenStandard::ProgrammableNonFungible),
                Some(STANDARD_PNFT),
            ),
            (Some(TokenStandard::ProgrammableNonFungibleEdition), None),
            (None, None),
        ];
        for (token_standard, admitted) in expected {
            let mut metadata = base_metadata();
            metadata.token_standard = token_standard;
            match admitted {
                Some(standard) => assert_eq!(
                    resolve_standard(&metadata).unwrap(),
                    standard,
                    "{token_standard:?} did not resolve to standard {standard}"
                ),
                None => assert_eq!(
                    error_code(resolve_standard(&metadata).unwrap_err()),
                    6101,
                    "{token_standard:?} was admitted"
                ),
            }
        }
    }

    /// The two admitted standards must not collapse onto one discriminant. Without this, a
    /// mapping that returned `STANDARD_PNFT` for both would keep every case above green while
    /// sending a legacy NFT down `withdraw`'s pNFT branch, which needs a token record it has not
    /// got — an asset escrowed by a path that cannot return it.
    #[test]
    fn the_two_admitted_standards_resolve_to_different_discriminants() {
        let mut pnft = base_metadata();
        pnft.token_standard = Some(TokenStandard::ProgrammableNonFungible);
        let mut legacy = base_metadata();
        legacy.token_standard = Some(TokenStandard::NonFungible);
        assert_ne!(
            resolve_standard(&pnft).unwrap(),
            resolve_standard(&legacy).unwrap()
        );
    }

    // --- step 1: the owner program, before anything is parsed --------------------------------

    /// The mint's **owner program** is what selects the Token Metadata
    /// family, so an MPL-Core asset account and a mint from a fork of SPL Token are both refused
    /// here — before the metadata is parsed, and without consulting any label the asset carries.
    /// A compressed leaf never reaches this at all: it has no mint account to present.
    #[test]
    fn validate_admission_rejects_a_mint_owned_by_another_program() {
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = key(88),
                    metadata_key = metadata_pda(),
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = edition_pda(),
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = base_metadata(),
                    edition_data = serialized_master_edition_v2(),
                    record_key = record_key_for(ALLOWED_COLLECTION),
                    record = record(DEFINED_STANDARDS),
                )
                .unwrap_err()
            ),
            6101
        );
    }

    // --- step 5: the record the asset derives, and the bit it must have ----------------------

    /// The pool component of the derivation, exercised through the wrapper rather than through
    /// `PoolCollection::address` alone. Another pool's record for the *same* admitted collection
    /// is a real account with the right `collection` and the right `standards`; only the pool
    /// this position is opening under separates it, and only the derivation reads that.
    #[test]
    fn validate_admission_rejects_another_pools_record_for_the_same_collection() {
        let other_pool_record = PoolCollection::address(&key(41), &key(ALLOWED_COLLECTION)).0;
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = metadata_pda(),
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = edition_pda(),
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = base_metadata(),
                    edition_data = serialized_master_edition_v2(),
                    record_key = other_pool_record,
                    record = record(DEFINED_STANDARDS),
                )
                .unwrap_err()
            ),
            6100
        );
    }

    /// **The fault the re-signature exists to prevent, and the only case here that can see it.**
    /// The asset declares the admitted collection; the record's own `collection` field names a
    /// different one and the presented account is *that* collection's PDA. Deriving the expected
    /// address from the record — from any value the caller supplies — accepts, because a record
    /// trivially agrees with itself, and the pool would take a card from a collection it never
    /// admitted. Deriving it from the asset rejects. Every other derivation case in this module
    /// stays green under that fault, which is why this one is written out separately.
    #[test]
    fn validate_admission_derives_the_record_from_the_asset_not_from_the_record() {
        const SUBSTITUTED: u8 = 123;
        let mut substituted_record = record(DEFINED_STANDARDS);
        substituted_record.collection = key(SUBSTITUTED);
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = metadata_pda(),
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = edition_pda(),
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = base_metadata(),
                    edition_data = serialized_master_edition_v2(),
                    record_key = record_key_for(SUBSTITUTED),
                    record = substituted_record,
                )
                .unwrap_err()
            ),
            6100
        );
    }

    /// The wrong pairing, on the standard rather than on the collection: the record is the
    /// right one for this pool and this collection, and it admits Core and legacy — but not the
    /// pNFT the asset actually is. Its own code (6108), distinct from the 6100 a wrong record
    /// returns, because the remedies differ: admit the standard, versus admit the collection.
    #[test]
    fn validate_admission_rejects_a_standard_the_record_does_not_admit() {
        assert_eq!(
            error_code(
                admission_case!(
                    mint_owner = SPL_TOKEN_ID,
                    metadata_key = metadata_pda(),
                    metadata_owner = TOKEN_METADATA_ID,
                    edition_key = edition_pda(),
                    edition_owner = TOKEN_METADATA_ID,
                    metadata = base_metadata(),
                    edition_data = serialized_master_edition_v2(),
                    record_key = record_key_for(ALLOWED_COLLECTION),
                    record = record(STANDARDS_BIT_LEGACY | STANDARDS_BIT_CORE),
                )
                .unwrap_err()
            ),
            6108
        );
    }

    /// The other direction of the same bit, so the case above is not passing on a mask that
    /// rejects everything: the identical fixture against a record admitting **only** pNFT is
    /// accepted. Without this, `mask_admits` degenerated to `false` would look correct.
    #[test]
    fn validate_admission_accepts_a_record_admitting_only_that_one_standard() {
        assert_eq!(
            admission_case!(
                mint_owner = SPL_TOKEN_ID,
                metadata_key = metadata_pda(),
                metadata_owner = TOKEN_METADATA_ID,
                edition_key = edition_pda(),
                edition_owner = TOKEN_METADATA_ID,
                metadata = base_metadata(),
                edition_data = serialized_master_edition_v2(),
                record_key = record_key_for(ALLOWED_COLLECTION),
                record = record(STANDARDS_BIT_PNFT),
            )
            .unwrap(),
            STANDARD_PNFT
        );
    }

    // --- master edition ----------------------------------------------------------------------

    #[test]
    fn check_master_edition_v2_accepts_a_v2_edition() {
        let edition = MasterEdition {
            key: MetaKey::MasterEditionV2,
            supply: 0,
            max_supply: None,
        };
        let mut buf = Vec::new();
        edition.serialize(&mut buf).unwrap();
        assert!(check_master_edition_v2(&buf).is_ok());
    }

    #[test]
    fn check_master_edition_v2_rejects_a_v1_edition_even_though_it_parses() {
        // MasterEditionV1's leading fields (key, supply, max_supply) are a strict prefix of
        // MasterEditionV2's layout, so `MasterEdition::from_bytes` parses it without error —
        // the `key` check below is the only thing that tells them apart.
        let v1 = mpl_token_metadata::accounts::DeprecatedMasterEditionV1 {
            key: MetaKey::MasterEditionV1,
            supply: 0,
            max_supply: None,
            printing_mint: key(3),
            one_time_printing_authorization_mint: key(4),
        };
        let mut buf = Vec::new();
        v1.serialize(&mut buf).unwrap();

        let parsed = MasterEdition::from_bytes(&buf).expect("V1 prefix parses as MasterEdition");
        assert_eq!(parsed.key, MetaKey::MasterEditionV1);

        assert_eq!(error_code(check_master_edition_v2(&buf).unwrap_err()), 6101);
    }

    #[test]
    fn check_master_edition_v2_rejects_data_that_does_not_parse_at_all() {
        // Stand-in for an MPL-Core asset: not a Token Metadata master edition account shape.
        let garbage = [0xFFu8; 4];
        assert_eq!(
            error_code(check_master_edition_v2(&garbage).unwrap_err()),
            6101
        );
    }
    // --- the positional CPI contract the four callers pin against ----------------------------

    const ESCROW_SRC: &str = include_str!("escrow.rs");

    /// `transfer_pnft` takes 16 positional `&AccountInfo<'info>` of identical type, and each of
    /// its four call sites pins its own 16 arguments as an ordered literal. Those pins assert
    /// the **argument** order and say nothing about the **parameter** order: transposing two
    /// adjacent parameters here compiles, silently changes what all four callers move, and
    /// leaves every one of their pins green. All 15 adjacent transpositions passed a green
    /// suite, `authority`/`payer` among them.
    #[test]
    fn the_positional_signature_the_call_sites_pin_against_is_itself_pinned() {
        assert!(flat(ESCROW_SRC).contains(
            "pub fn transfer_pnft<'info>( source_token: &AccountInfo<'info>, \
             source_owner: &AccountInfo<'info>, destination_token: &AccountInfo<'info>, \
             destination_owner: &AccountInfo<'info>, mint: &AccountInfo<'info>, \
             metadata: &AccountInfo<'info>, edition: &AccountInfo<'info>, \
             source_token_record: &AccountInfo<'info>, \
             destination_token_record: &AccountInfo<'info>, authority: &AccountInfo<'info>, \
             payer: &AccountInfo<'info>, system_program: &AccountInfo<'info>, \
             sysvar_instructions: &AccountInfo<'info>, spl_token_program: &AccountInfo<'info>, \
             spl_ata_program: &AccountInfo<'info>, token_metadata_program: &AccountInfo<'info>, \
             authorization_rules_program: Option<&AccountInfo<'info>>, \
             authorization_rules: Option<&AccountInfo<'info>>, signer_seeds: &[&[&[u8]]], \
             ) -> Result<()> {"
        ));
        assert_eq!(
            production(ESCROW_SRC).matches("fn transfer_pnft").count(),
            1,
            "a second pNFT transfer helper would carry its own unpinned signature"
        );
    }

    /// The other half of the same contract: the parameter → builder-method mapping. Rebinding
    /// `.authority()` to `payer`, or `.token()` to `destination_token`, reverses custody
    /// direction or the signing authority for every caller and is invisible to every call-site
    /// pin. `.amount(1)` belongs here too — a `0` moves nothing and leaves the pNFT in a vault
    /// its owner is about to close. All 25 mapping faults injected passed a green suite.
    #[test]
    fn every_transfer_v1_builder_argument_is_bound_to_its_own_parameter() {
        assert!(flat(ESCROW_SRC).contains(
            "TransferV1CpiBuilder::new(token_metadata_program) .token(source_token) \
             .token_owner(source_owner) .destination_token(destination_token) \
             .destination_owner(destination_owner) .mint(mint) .metadata(metadata) \
             .edition(Some(edition)) .token_record(Some(source_token_record)) \
             .destination_token_record(Some(destination_token_record)) .authority(authority) \
             .payer(payer) .system_program(system_program) \
             .sysvar_instructions(sysvar_instructions) .spl_token_program(spl_token_program) \
             .spl_ata_program(spl_ata_program) \
             .authorization_rules_program(authorization_rules_program) \
             .authorization_rules(authorization_rules) .amount(1) \
             .invoke_signed(signer_seeds)?;"
        ));
        assert_eq!(
            production(ESCROW_SRC)
                .matches("TransferV1CpiBuilder::new(")
                .count(),
            1,
            "a second builder chain would carry its own unpinned parameter mapping"
        );
    }

    /// The same contract for the legacy leg. `Transfer`'s three fields are all `AccountInfo` of
    /// one type, so swapping `from` and `to` reverses custody — a `withdraw` that moves the card
    /// back into the vault it is leaving — and no call-site pin can see it. `amount` is `1` here
    /// for the reason it is `1` in the pNFT chain: a `0` reports success having moved nothing.
    #[test]
    fn every_spl_transfer_argument_is_bound_to_its_own_parameter() {
        assert!(flat(ESCROW_SRC).contains(
            "pub fn transfer_spl<'info>( source_token: &AccountInfo<'info>, \
             destination_token: &AccountInfo<'info>, authority: &AccountInfo<'info>, \
             token_program: &AccountInfo<'info>, signer_seeds: &[&[&[u8]]], ) -> Result<()> { \
             transfer( CpiContext::new_with_signer( token_program.clone(), Transfer { \
             from: source_token.clone(), to: destination_token.clone(), \
             authority: authority.clone(), }, signer_seeds, ), 1, ) }"
        ));
        assert_eq!(
            production(ESCROW_SRC).matches("fn transfer_spl").count(),
            1,
            "a second SPL transfer helper would carry its own unpinned parameter mapping"
        );
    }

    /// `release_from_vault`'s own two CPI call sites, which no caller's pin can reach: the
    /// callers pin the arguments they hand *this* function, and everything below happens inside
    /// it. `transfer_spl(position_vault, depositor_token, position, ...)` transposed sends the
    /// card nowhere or signs with the wrong account, and every one of those permutations compiles.
    ///
    /// `create_idempotent`'s mapping is the load-bearing half. It is not a convenience: SPL
    /// `Transfer` validates the source authority and says nothing about who owns the destination,
    /// so on the legacy path the destination is bound to the depositor here or nowhere. Bind
    /// `authority` to anything else and a permissionless `return_rejected` hands the card to
    /// whoever the caller named.
    ///
    /// **The `require_keys_eq!` above it is the same binding stated in this crate's own code**,
    /// and it is asserted first because it is the half a refactor can delete without breaking a
    /// happy-path test: the ATA program's `InvalidSeeds` is a side effect of a call made to
    /// *create* the account, so a creation made conditional, reordered, or swapped for a helper
    /// that skips the comparison would take the guard with it. Both are pinned, and the explicit
    /// one must precede the CPI.
    #[test]
    fn the_legacy_branch_binds_its_destination_to_the_depositor_before_transferring() {
        assert!(flat(ESCROW_SRC).contains(
            "require_keys_eq!( depositor_token.key(), \
             get_associated_token_address(&depositor.key(), &nft_mint.key()), \
             ByeMachineError::NotPositionOwner );"
        ));
        let prod = production(ESCROW_SRC);
        // Anchored on `depositor_token.key()` rather than on `require_keys_eq!(`, of which this
        // file has six — five belong to `read_metadata`'s account checks, which precede
        // everything and would make the ordering assertion true for a reason that has nothing to
        // do with this branch.
        assert!(
            prod.find("depositor_token.key(),").unwrap() < prod.find("create_idempotent(").unwrap(),
            "the explicit derivation check must precede the creation, not lean on it"
        );
        assert_eq!(
            prod.matches("depositor_token.key(),").count(),
            1,
            "one such check, in one branch — the pNFT path's destination is bound by Token \
             Metadata's own processor and a copy here would be a second answer"
        );
        assert_eq!(
            u32::from(ByeMachineError::NotPositionOwner),
            6302,
            "and it fails with this crate's code, not the ATA program's InvalidSeeds"
        );
        assert!(flat(ESCROW_SRC).contains(
            "create_idempotent(CpiContext::new( associated_token_program.clone(), CreateAta { \
             payer: payer.clone(), associated_token: depositor_token.clone(), \
             authority: depositor.clone(), mint: nft_mint.clone(), \
             system_program: system_program.clone(), token_program: token_program.clone(), }, ))?;"
        ));
        assert!(flat(ESCROW_SRC).contains(
            "transfer_spl( position_vault, depositor_token, position, token_program, \
             signer_seeds, )"
        ));
        // Ordering: a transfer that ran before the derivation check would already have moved the
        // card by the time the destination was rejected.
        let prod = production(ESCROW_SRC);
        assert!(prod.find("create_idempotent(").unwrap() < prod.find("transfer_spl(\n").unwrap());
    }

    /// All five, counted by name. `all_absent` is where the required-absent set is
    /// decided for all three exits at once, so a field dropped from it is four accounts checked
    /// and one silently accepted — and the behavioural case can only ever probe one account at a
    /// time.
    #[test]
    fn the_required_absent_set_is_all_five_accounts() {
        assert!(flat(ESCROW_SRC).contains(
            "fn all_absent(&self) -> bool { self.position_token_record.is_none() \
             && self.depositor_token_record.is_none() && self.sysvar_instructions.is_none() \
             && self.authorization_rules_program.is_none() \
             && self.authorization_rules.is_none() }"
        ));
        assert_eq!(
            production(ESCROW_SRC)
                .matches("pub position_token_record")
                .count(),
            1,
            "PnftRelease is the one home for the set; a second declaration is a second set"
        );
    }

    /// **The `token_standard` match has no default arm.** The compiler enforces
    /// exhaustiveness but not the *absence* of a catch-all — a wildcard arm added to an already
    /// exhaustive match is an `unreachable_patterns` warning, which reds under
    /// `clippy -D warnings` and not under `cargo build-sbf`, and one added to a match someone has
    /// meanwhile made non-exhaustive is no diagnostic at all.
    ///
    /// Scoped to `resolve_standard`'s own body rather than the whole module, because a
    /// second match exists that this claim cannot be made about: `release_from_vault` matches on a `u8`, where a
    /// wildcard is **forced by the type** and no enumeration can retire it. So the module is
    /// allowed exactly one, and what it does is pinned — a forced arm that fell through would
    /// close a position whose card never moved. Two assertions, because "there is one wildcard"
    /// and "the wildcard rejects" are different claims and only the second is the dangerous one.
    ///
    /// It reads raw text, so a doc comment *illustrating* a catch-all reds it too. That is why
    /// `resolve_standard` describes the arm it refuses to have rather than writing it out — a
    /// false positive that costs one sentence and fails closed.
    #[test]
    fn resolve_standard_has_no_catch_all_and_the_modules_one_forced_wildcard_rejects() {
        let prod = production(ESCROW_SRC);
        let from_fn = &prod[prod.find("fn resolve_standard").unwrap()..];
        let body = &from_fn[..from_fn.find("\n}\n").unwrap()];
        assert_eq!(
            body.matches("_ =>").count(),
            0,
            "the token_standard match acquired a default arm"
        );
        assert_eq!(body.matches("_ if").count(), 0);

        assert_eq!(
            prod.matches("_ =>").count(),
            1,
            "only release_from_vault's u8 match may carry a wildcard"
        );
        assert!(flat(ESCROW_SRC)
            .contains("_ => Err(ByeMachineError::WrongStandardForInstruction.into()),"));
    }
}
