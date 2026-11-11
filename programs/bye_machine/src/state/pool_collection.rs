use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::standards::mask_admits;

/// One admitted collection's record. Seeds: `[COLLECTION_SEED, pool, collection]`.
///
/// `collection` is the address read out of the presented asset itself, never a caller argument,
/// so seeding on it makes both a substituted record and a duplicate record for one collection
/// structurally impossible. `standards` is the bitmask from `common::standards`; the record
/// admits a collection *for* a set of standards, and a bit cleared today cannot reach a card
/// admitted yesterday because `Position.standard` is written once at intake and never re-read
/// from here.
#[account]
#[derive(InitSpace)]
pub struct PoolCollection {
    pub pool: Pubkey,
    pub collection: Pubkey,
    pub standards: u8,
    pub admitted_at: i64,
    pub bump: u8,
}

impl PoolCollection {
    /// The record address for `collection` under `pool`, with its bump.
    ///
    /// Both components are arguments rather than fields so a caller cannot derive a record's
    /// address from the record itself: a derivation seeded entirely on the account being checked
    /// moves with any substitution and admits every sibling. The intake passes **this pool's**
    /// key, which is what makes another pool's record for the same collection fail.
    pub fn address(pool: &Pubkey, collection: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[
                crate::common::seeds::COLLECTION_SEED,
                pool.as_ref(),
                collection.as_ref(),
            ],
            &crate::ID,
        )
    }

    /// For every standard: the presented record must be **the** record for
    /// this pool and the collection the *asset* declares, and it must admit `standard`.
    ///
    /// **The address is re-derived from `declared_collection` (read from the asset), never from
    /// the record's own `collection` field.** A record compared against its own field trivially
    /// agrees, so a record that is self-consistent for a collection the asset does not declare
    /// would pass.
    ///
    /// `standard` is the caller's: the Token Metadata intake resolves it from the metadata, the
    /// Core intake knows it from the branch it reached.
    ///
    /// This is the crate's only derivation of a record address outside this file; two copies of
    /// a custody predicate risk one being repaired and the other not.
    pub fn require_admits(
        pool: &Pubkey,
        declared_collection: &Pubkey,
        record_key: &Pubkey,
        record: &PoolCollection,
        standard: u8,
    ) -> Result<()> {
        let (expected_record, _) = Self::address(pool, declared_collection);
        require_keys_eq!(
            *record_key,
            expected_record,
            ByeMachineError::CollectionNotAdmitted
        );
        require!(
            mask_admits(record.standards, standard),
            ByeMachineError::StandardNotAdmittedForCollection
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::standards::{
        DEFINED_STANDARDS, STANDARDS_BIT_CORE, STANDARDS_BIT_LEGACY, STANDARDS_BIT_PNFT,
        STANDARD_CORE, STANDARD_LEGACY, STANDARD_PNFT,
    };
    use crate::common::test_support::production;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    const POOL: u8 = 40;
    /// The collection the asset declares in the accepted cases.
    const DECLARED: u8 = 9;
    /// A different, equally real collection — the one a substituted record is built around.
    const OTHER: u8 = 11;

    fn record_key_for(pool: u8, collection: u8) -> Pubkey {
        PoolCollection::address(&key(pool), &key(collection)).0
    }

    fn record(collection: u8, standards: u8) -> PoolCollection {
        PoolCollection {
            pool: key(POOL),
            collection: key(collection),
            standards,
            admitted_at: 1,
            bump: 1,
        }
    }

    /// Checks the literal seed layout. A swap of the two components inside `address` is
    /// self-consistent, so only a hard-coded expectation catches it.
    #[test]
    fn address_matches_the_documented_seed_derivation() {
        let pool = Pubkey::new_unique();
        let collection = Pubkey::new_unique();
        let expected = Pubkey::find_program_address(
            &[b"collection", pool.as_ref(), collection.as_ref()],
            &crate::ID,
        );
        assert_eq!(PoolCollection::address(&pool, &collection), expected);
    }

    /// If the derivation ignored `pool`, another pool's record for the same collection would
    /// pass; nothing else in the intake compares the record's pool.
    #[test]
    fn address_separates_two_pools_holding_the_same_collection() {
        let collection = Pubkey::new_unique();
        let a = PoolCollection::address(&Pubkey::new_unique(), &collection);
        let b = PoolCollection::address(&Pubkey::new_unique(), &collection);
        assert_ne!(a.0, b.0);
    }

    /// One pool's records for two collections must not collide.
    #[test]
    fn address_separates_two_collections_under_the_same_pool() {
        let pool = Pubkey::new_unique();
        let a = PoolCollection::address(&pool, &Pubkey::new_unique());
        let b = PoolCollection::address(&pool, &Pubkey::new_unique());
        assert_ne!(a.0, b.0);
    }
    // --- require_admits, for every standard -----------------------------------------------------

    #[test]
    fn a_correctly_derived_record_admitting_the_standard_is_accepted() {
        PoolCollection::require_admits(
            &key(POOL),
            &key(DECLARED),
            &record_key_for(POOL, DECLARED),
            &record(DECLARED, DEFINED_STANDARDS),
            STANDARD_CORE,
        )
        .unwrap();
    }

    /// A record admitting only the presented standard is accepted.
    #[test]
    fn a_record_admitting_only_that_one_standard_is_accepted() {
        PoolCollection::require_admits(
            &key(POOL),
            &key(DECLARED),
            &record_key_for(POOL, DECLARED),
            &record(DECLARED, STANDARDS_BIT_CORE),
            STANDARD_CORE,
        )
        .unwrap();
    }

    /// The collection is admitted, but for other standards: fails on the standard bit (6108),
    /// not on the derivation (6100).
    #[test]
    fn a_record_that_does_not_admit_the_standard_is_rejected_on_the_bit() {
        assert_eq!(
            error_code(
                PoolCollection::require_admits(
                    &key(POOL),
                    &key(DECLARED),
                    &record_key_for(POOL, DECLARED),
                    &record(DECLARED, STANDARDS_BIT_PNFT | STANDARDS_BIT_LEGACY),
                    STANDARD_CORE,
                )
                .unwrap_err()
            ),
            6108
        );
    }

    /// One record admitting exactly one standard, presented against each of the three
    /// standards: a predicate that ignored `standard` would pass only the diagonal.
    #[test]
    fn the_standard_argument_is_read_and_is_not_any_one_standard_in_disguise() {
        let all = [
            (STANDARD_PNFT, STANDARDS_BIT_PNFT),
            (STANDARD_LEGACY, STANDARDS_BIT_LEGACY),
            (STANDARD_CORE, STANDARDS_BIT_CORE),
        ];
        for (_, admitted_bit) in all {
            for (presented, _) in all {
                let result = PoolCollection::require_admits(
                    &key(POOL),
                    &key(DECLARED),
                    &record_key_for(POOL, DECLARED),
                    &record(DECLARED, admitted_bit),
                    presented,
                );
                let expected_ok = admitted_bit == (1u8 << presented);
                assert_eq!(
                    result.is_ok(),
                    expected_ok,
                    "standard {presented} against mask {admitted_bit:#05b}"
                );
                if !expected_ok {
                    assert_eq!(error_code(result.unwrap_err()), 6108);
                }
            }
        }
    }

    /// Another pool's record for the same collection has the right `collection` and
    /// `standards`; only the derivation's pool component rejects it.
    #[test]
    fn another_pools_record_for_the_same_collection_is_rejected() {
        assert_eq!(
            error_code(
                PoolCollection::require_admits(
                    &key(POOL),
                    &key(DECLARED),
                    &record_key_for(POOL + 1, DECLARED),
                    &record(DECLARED, DEFINED_STANDARDS),
                    STANDARD_CORE,
                )
                .unwrap_err()
            ),
            6100
        );
    }

    /// The asset declares `DECLARED` while the record's `collection` field and its address both
    /// say `OTHER`: self-consistent, yet not the record this asset is admitted under. Catches
    /// deriving the expected address from `record.collection`.
    #[test]
    fn a_self_consistent_record_for_a_collection_the_asset_does_not_declare_is_rejected() {
        assert_eq!(
            error_code(
                PoolCollection::require_admits(
                    &key(POOL),
                    &key(DECLARED),
                    &record_key_for(POOL, OTHER),
                    &record(OTHER, DEFINED_STANDARDS),
                    STANDARD_CORE,
                )
                .unwrap_err()
            ),
            6100
        );
    }

    // --- single derivation site ---------------------------------------------------------------

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("src/ must be readable") {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// No production code under `src/` derives a record address except through
    /// `require_admits`: a second copy of this guard could be repaired while the other is not.
    /// Test modules are stripped, since fixtures legitimately derive addresses.
    #[test]
    fn no_production_code_outside_this_method_derives_a_record_address() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&src, &mut files);
        assert!(
            files.len() > 20,
            "the sweep found {} files — it is not reading the crate",
            files.len()
        );

        let mut offenders = Vec::new();
        for file in &files {
            let text = fs::read_to_string(file).expect("readable source");
            if production(&text).contains("PoolCollection::address") {
                offenders.push(file.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "these files derive a record address outside PoolCollection::require_admits: {}",
            offenders.join(", ")
        );
    }

    /// Positive control for the sweep above: `production` must still see a derivation outside
    /// test modules.
    #[test]
    fn the_sweep_would_see_a_reintroduced_derivation() {
        let reintroduced =
            "fn admit() { let _ = PoolCollection::address(&a, &b); }\n#[cfg(test)]\nmod t {}";
        assert!(production(reintroduced).contains("PoolCollection::address"));
        let only_in_tests =
            "fn admit() {}\n#[cfg(test)]\nmod t { let _ = PoolCollection::address(&a, &b); }";
        assert!(!production(only_in_tests).contains("PoolCollection::address"));
    }
}
