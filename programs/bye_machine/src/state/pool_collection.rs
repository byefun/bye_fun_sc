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
    /// **The record is passed as an argument rather than as `&self`, and the address is
    /// re-derived from `declared_collection` rather than from anything on it.** Both are the
    /// guard. The obvious-looking alternative — deriving from the record's own
    /// `collection` field — is green against every case then written, because a record compared
    /// against itself trivially agrees; only a record that is self-consistent *for a collection
    /// the asset does not declare* separates them. Written as an associated function so a reader
    /// can see which value the derivation is seeded on without resolving a receiver.
    ///
    /// `standard` is the caller's: the Token Metadata intake resolves it from the metadata, the
    /// Core intake knows it from the branch it reached. Passing a constant here would silently
    /// admit one standard everywhere, which is why both call sites are cased on it on chain.
    ///
    /// This is the crate's **only** derivation of a record address outside this file — pinned
    /// below by a sweep over `src/`, because two copies of a custody predicate is how one gets
    /// repaired and the other does not.
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

    /// The literal-seed pin, and **the only test that catches a component swap inside
    /// `address`** — measured, not assumed. A same-order swap in the production derivation maps
    /// both `(pool, collection)` and `(collection, pool)` consistently, so a symmetry test passes
    /// straight through it; only a hard-coded expectation reds.
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

    /// The dimension that makes the intake's guard non-vacuous. If the derivation ignored `pool`,
    /// another pool's record for the same collection would derive to the same address and pass —
    /// and nothing else in the intake compares the record's pool.
    #[test]
    fn address_separates_two_pools_holding_the_same_collection() {
        let collection = Pubkey::new_unique();
        let a = PoolCollection::address(&Pubkey::new_unique(), &collection);
        let b = PoolCollection::address(&Pubkey::new_unique(), &collection);
        assert_ne!(a.0, b.0);
    }

    /// The other dimension: one pool's two records must not collide, or the wrong-pairing
    /// rejection could be passed by presenting the other collection's record.
    #[test]
    fn address_separates_two_collections_under_the_same_pool() {
        let pool = Pubkey::new_unique();
        let a = PoolCollection::address(&pool, &Pubkey::new_unique());
        let b = PoolCollection::address(&pool, &Pubkey::new_unique());
        assert_ne!(a.0, b.0);
    }
    // --- require_admits, for every standard -----------------------------------------------------
    //
    // The predicate lives here rather than per-intake. Both intakes reach
    // it through their own call, and each intake keeps a pin proving it still does.

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

    /// The other direction of the bit, so the rejection below is not passing on a mask that
    /// refuses everything: a record admitting **only** the presented standard is accepted.
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

    /// The wrong pairing: the collection is admitted, for other standards. The presented
    /// asset fails on the bit (6108), not on the derivation (6100) — two codes because the two
    /// remedies are different sentences: admit the standard, versus admit the collection.
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

    /// **The `standard` argument is the whole reason one predicate can serve two intakes, so it
    /// is exercised over all three rather than at the Core value the extraction happened to be
    /// driven from.** One record, admitting exactly one standard, presented against each: a
    /// predicate that ignored the argument — or a caller that hard-coded one — passes the
    /// diagonal and fails everything else, and only a full 3 × 3 sees that.
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

    /// The pool component of the derivation. Another pool's record for the *same* admitted
    /// collection is a real account with the right `collection` and the right `standards`; only
    /// the pool this position is opening under separates it, and only the derivation reads that.
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

    /// **The case that discriminates, and the only one that does.** The obvious
    /// alternative — derive the expected address from `record.collection` rather than from the
    /// collection the asset declares — passes every derivation case then written, because
    /// a record compared against its own field trivially agrees. Here the asset says `DECLARED`
    /// while the record's `collection` field **and** its address both say `OTHER`: consistent
    /// with itself, and still not the record this asset is admitted under.
    ///
    /// It is also measured on chain, in `tests/integration/core-standard-deposit.test.ts`, where
    /// the same substitution reds one case out of five.
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

    // --- the extraction's own guarantee --------------------------------------------------------

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

    /// **The reason this extraction exists, asserted as a property of the crate rather than of
    /// the two files it happened to touch.** A record address derived anywhere else is a second copy of
    /// this guard, and the failure mode is not that the copy is wrong today — it is that one copy
    /// gets repaired and the other does not. Swept over every `.rs` under `src/`, with test
    /// modules stripped: fixtures legitimately derive addresses to build cases, and this file's
    /// own `record_key_for` is one of them.
    ///
    /// The method derives through `Self::address`, so this file's production half does not match
    /// either — the expected count is **zero everywhere**, which is a stronger claim than "one
    /// here and none elsewhere" and needs no exception for the home file.
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

    /// The positive control for the sweep above. An absence test that cannot see a presence is
    /// not a test — and this one strips text before matching, so a broken `production` would
    /// report a clean crate forever.
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
