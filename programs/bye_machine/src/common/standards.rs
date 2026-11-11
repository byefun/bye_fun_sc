//! Token-standard discriminants and the admitted-standards bitmask.
//!
//! | `Position.standard` | Standard | `PoolCollection.standards` bit |
//! |---|---|---|
//! | 0 | pNFT (`ProgrammableNonFungible`) | `1 << 0` |
//! | 1 | legacy (`NonFungible`, `V1_NFT`) | `1 << 1` |
//! | 2 | MPL Core (`MplCoreAsset`) | `1 << 2` |
//!
//! The discriminant and the bit are separate namespaces that happen to correspond; nothing here
//! computes one from the other by shifting an unvalidated `u8`.

pub const STANDARD_PNFT: u8 = 0;
pub const STANDARD_LEGACY: u8 = 1;
pub const STANDARD_CORE: u8 = 2;

pub const STANDARDS_BIT_PNFT: u8 = 1 << STANDARD_PNFT;
pub const STANDARDS_BIT_LEGACY: u8 = 1 << STANDARD_LEGACY;
pub const STANDARDS_BIT_CORE: u8 = 1 << STANDARD_CORE;

/// Every bit any `PoolCollection.standards` may set. The five bits above it are spare capacity
/// for standards a later version admits, and a mask that sets one of them is rejected rather
/// than ignored.
pub const DEFINED_STANDARDS: u8 = STANDARDS_BIT_PNFT | STANDARDS_BIT_LEGACY | STANDARDS_BIT_CORE;

/// The mask bit for a `Position.standard` discriminant, or `None` if the discriminant is not one
/// of the three this program supports.
///
/// Deliberately a closed match rather than `1 << standard`: the shift is a debug-build panic for
/// `standard >= 8` and silently wraps into a defined bit for `standard >= 8` in release, so an
/// unvalidated discriminant reaching it could admit itself against a mask that never named it.
pub fn mask_for(standard: u8) -> Option<u8> {
    match standard {
        STANDARD_PNFT => Some(STANDARDS_BIT_PNFT),
        STANDARD_LEGACY => Some(STANDARDS_BIT_LEGACY),
        STANDARD_CORE => Some(STANDARDS_BIT_CORE),
        _ => None,
    }
}

/// Whether `mask` admits `standard`. False for every discriminant outside the supported three,
/// so an unsupported standard cannot be admitted by any mask.
pub fn mask_admits(mask: u8, standard: u8) -> bool {
    match mask_for(standard) {
        Some(bit) => mask & bit != 0,
        None => false,
    }
}

/// Whether `mask` is a usable admitted-standards value: at least one standard, and no bit outside
/// the defined three. An all-zero mask reads as admitted while failing every deposit, and an
/// undefined bit is a standard no custody path exists for.
pub fn mask_is_valid(mask: u8) -> bool {
    mask != 0 && mask & !DEFINED_STANDARDS == 0
}

/// Whether moving from `old` to `new` clears at least one bit. This is the condition
/// `admit_collection`'s mandatory `reason` is attached to — it is a property of the *effect*, not
/// of which entry path ran, so a creation (`old == 0`) is never a narrowing.
pub fn clears_a_bit(old: u8, new: u8) -> bool {
    new & old != old
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminants_and_bits_are_pinned_to_their_exact_values() {
        assert_eq!(STANDARD_PNFT, 0);
        assert_eq!(STANDARD_LEGACY, 1);
        assert_eq!(STANDARD_CORE, 2);
        assert_eq!(STANDARDS_BIT_PNFT, 0b001);
        assert_eq!(STANDARDS_BIT_LEGACY, 0b010);
        assert_eq!(STANDARDS_BIT_CORE, 0b100);
        assert_eq!(DEFINED_STANDARDS, 0b111);
    }

    /// The discriminant is stored on `Position` and read by every custody-moving instruction, so
    /// a renumbering relocates which transfer path an already-escrowed card leaves by. Pinning
    /// the constants above is not enough on its own: `mask_for` is the mapping the intake and the
    /// record check share, and it is what a renumbering would have to keep consistent.
    #[test]
    fn mask_for_maps_each_supported_standard_to_its_own_single_bit() {
        assert_eq!(mask_for(STANDARD_PNFT), Some(0b001));
        assert_eq!(mask_for(STANDARD_LEGACY), Some(0b010));
        assert_eq!(mask_for(STANDARD_CORE), Some(0b100));
    }

    /// Every `u8` outside the three, not a sampled few — the domain is small enough to exhaust,
    /// and a `_ => Some(...)` arm added later must red something.
    #[test]
    fn mask_for_rejects_every_unsupported_discriminant() {
        for standard in 3u8..=u8::MAX {
            assert_eq!(
                mask_for(standard),
                None,
                "discriminant {standard} resolved to a mask bit; only 0, 1 and 2 are supported"
            );
        }
    }

    /// The full `mask x standard` cross-product over the defined domain: 8 masks, 3 standards.
    /// A per-standard spot check passes even if `mask_admits` returns `mask != 0`, which would
    /// admit a Core asset from a pNFT-only collection — a wrong-pairing case.
    #[test]
    fn mask_admits_is_exactly_the_bit_for_that_standard() {
        for mask in 0u8..=0b111 {
            for standard in [STANDARD_PNFT, STANDARD_LEGACY, STANDARD_CORE] {
                let bit = mask_for(standard).unwrap();
                assert_eq!(
                    mask_admits(mask, standard),
                    mask & bit != 0,
                    "mask {mask:#05b} against standard {standard}"
                );
            }
        }
    }

    #[test]
    fn mask_admits_refuses_an_unsupported_standard_under_every_mask() {
        for mask in 0u8..=u8::MAX {
            for standard in 3u8..=u8::MAX {
                assert!(
                    !mask_admits(mask, standard),
                    "mask {mask:#010b} admitted unsupported standard {standard}"
                );
            }
        }
    }

    /// Exhaustive over `u8`: the valid set is exactly `0b001..=0b111`, so the test also proves no
    /// mask with a spare bit set is accepted — the "standard no custody path exists for" case.
    #[test]
    fn mask_is_valid_is_exactly_one_to_seven() {
        for mask in 0u8..=u8::MAX {
            assert_eq!(
                mask_is_valid(mask),
                (1..=0b111).contains(&mask),
                "mask {mask:#010b}"
            );
        }
    }

    /// The reason predicate over the whole `old x new` cross-product, 8 x 8, rather than a list of
    /// interesting pairs. `clears_a_bit` is the *only* thing standing between a bit-by-bit
    /// narrowing and `withdraw_collection`'s mandatory motive, so the partition into
    /// widening / narrowing / no-op has to be exact and not merely plausible.
    #[test]
    fn clears_a_bit_is_true_exactly_when_new_drops_one_of_olds_bits() {
        for old in 0u8..=0b111 {
            for new in 0u8..=0b111 {
                let dropped = (0..3).any(|i| old & (1 << i) != 0 && new & (1 << i) == 0);
                assert_eq!(
                    clears_a_bit(old, new),
                    dropped,
                    "old {old:#05b} -> new {new:#05b}"
                );
            }
        }
    }

    /// A creation is `old == 0`, and no mask can narrow nothing. Stated as its own case because
    /// `admit_collection` rejects a `reason` on creation as meaningless, and that rejection is
    /// only correct if this holds for every mask.
    #[test]
    fn a_creation_is_never_a_narrowing() {
        for new in 0u8..=u8::MAX {
            assert!(
                !clears_a_bit(0, new),
                "creating with mask {new:#010b} read as a narrowing"
            );
        }
    }

    /// A no-op is neither, which is why it needs its own rejection rather than falling out of the
    /// reason rule: `new == old` clears no bit, so it would pass as a pure widening and emit a
    /// change event recording no change.
    #[test]
    fn a_no_op_clears_nothing() {
        for mask in 0u8..=u8::MAX {
            assert!(!clears_a_bit(mask, mask), "mask {mask:#010b}");
        }
    }
}
