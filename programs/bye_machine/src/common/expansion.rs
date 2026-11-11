//! The published draw expansion: one 32-byte randomness value to `n` draws, and the fixed order
//! that maps a draw onto a blank denomination or a real leaf.
//!
//! Both halves are part of the protocol's public contract — a third party reconstructs every
//! draw of a settled batch from the batch account and position history alone, and must arrive at
//! the same result byte for byte. So neither function reads an account, takes a pool struct, or
//! branches on anything the caller cannot publish; every input is passed explicitly and the
//! committed vector in `tests/fixtures/expansion-vector.json` is the artifact an independent
//! implementation is checked against.
//!
//! **The modulus is the pool's live total weight, not a value snapshotted at commitment.** The
//! randomness fixes a number and the pool fixes what that number means, so a change to total
//! weight, to a leaf's place in the range order, or to the blank prefix re-maps a draw whose
//! randomness is already public. What keeps that from being exploitable lives elsewhere — the
//! pool refuses every external weight-changing action while a batch is open, and the batch
//! carries a running expectation each settlement asserts before drawing. Recording the pool
//! snapshot at the draw makes the result *verifiable*; it does not make it *unsteerable*, and
//! this module supplies only the first of those two.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::hash::hashv;

use crate::common::constants::{BLANK_SLOTS, EXPANSION_MAX_ATTEMPTS};
use crate::common::errors::ByeMachineError;
use crate::common::math::{slot_is_empty, weight_of};

/// One draw's published result, as it is persisted on the batch record.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct Draw {
    /// The reduced draw value, in `0..w`.
    pub r: u128,
    /// Hash attempts consumed, `1..=EXPANSION_MAX_ATTEMPTS`. This is a **count**, so the
    /// attempt byte fed to the hash on the deciding attempt is one less than it.
    pub attempts: u8,
}

/// Where a draw landed, in the fixed published order: the blank slots in `blank_faces` order,
/// empty slots skipped, then the real leaves.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub enum DrawTarget {
    /// A blank denomination, by index into the pool's blank-face array.
    Blank { face_index: u8 },
    /// A real card. `key` is the draw value with the blank prefix **already removed** — the
    /// argument the Fenwick range search takes, never the raw draw value. The two are equal
    /// whenever the pool holds no blanks, which is the state most fixtures start in, so a
    /// caller that passes the raw value instead is wrong everywhere and fails nowhere obvious.
    Real { key: u64 },
}

/// Expands `randomness` into roll `roll_index`'s draw over live total weight `w`.
///
/// Rejection sampling against the largest multiple of `w` below 2^128 keeps the reduction
/// uniform. It is capped rather than unbounded: a compute budget cannot carry an unbounded
/// loop, and after the cap the last word is reduced regardless. The residual modulo bias that
/// fallback admits is below 2^-78 at any weight the pool can hold, and `attempts` is persisted
/// so a replay lands on the same word rather than having to guess which one was taken.
pub fn expand(randomness: &[u8; 32], roll_index: u8, w: u128) -> Result<Draw> {
    require!(w > 0, ByeMachineError::ArithmeticFailure);

    let bound = acceptance_bound(w)?;

    // Bounded by the range: a sampler that failed to terminate would exhaust the compute budget
    // on every retry and leave the roll permanently unsettleable — worse than a biased draw.
    let mut word = 0u128;
    let mut attempts = 0u8;
    for attempt in 0..EXPANSION_MAX_ATTEMPTS {
        word = draw_word(randomness, roll_index, attempt);
        attempts = attempt
            .checked_add(1)
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        if accepts(word, bound) {
            break;
        }
    }

    let r = word
        .checked_rem(w)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    Ok(Draw { r, attempts })
}

/// Maps a draw value onto its target in the fixed published order.
///
/// `r` must be the reduction over the same total weight these arguments describe; a value at or
/// past that total is rejected rather than clamped, because a draw outside the range means the
/// modulus and the range order were computed against different pool states.
pub fn map_draw(
    r: u128,
    w_real: u128,
    blanks: [u32; BLANK_SLOTS],
    blank_faces: [u64; BLANK_SLOTS],
) -> Result<DrawTarget> {
    let mut prefix = 0u128;
    for face_index in 0..BLANK_SLOTS {
        if slot_is_empty(face_index, blank_faces[face_index]) {
            continue;
        }
        let contribution = u128::from(weight_of(blank_faces[face_index])?)
            .checked_mul(u128::from(blanks[face_index]))
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        prefix = prefix
            .checked_add(contribution)
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        if r < prefix {
            let face_index =
                u8::try_from(face_index).map_err(|_| ByeMachineError::ArithmeticFailure)?;
            return Ok(DrawTarget::Blank { face_index });
        }
    }

    let key = r
        .checked_sub(prefix)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    require!(key < w_real, ByeMachineError::ArithmeticFailure);
    let key = u64::try_from(key).map_err(|_| ByeMachineError::ArithmeticFailure)?;
    Ok(DrawTarget::Real { key })
}

/// Whether a candidate word falls inside the uniform region.
///
/// **The bound is the first value outside that region, not the last one inside it.** `bound` is
/// itself a multiple of `w`, so accepting it would reduce to 0 and hand the draw value 0 one
/// extra chance in 2^128 — the exact non-uniformity the rejection step exists to remove. One
/// word in 2^128 decides it, so no amount of sampling reaches the boundary and the comparison
/// is isolated here to be checked against it directly.
fn accepts(word: u128, bound: Option<u128>) -> bool {
    match bound {
        Some(bound) => word < bound,
        None => true,
    }
}

/// The largest multiple of `w` below 2^128, or `None` when `w` divides 2^128 exactly and there
/// is no rejection region at all.
///
/// 2^128 does not fit a `u128`, so the modulus is taken one step down: `2^128 mod w` is
/// `(u128::MAX mod w + 1) mod w`, and the bound is then `u128::MAX` less one below the residue,
/// which keeps every intermediate in range.
fn acceptance_bound(w: u128) -> Result<Option<u128>> {
    let residue = u128::MAX
        .checked_rem(w)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_add(1)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_rem(w)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    if residue == 0 {
        return Ok(None);
    }

    let bound = u128::MAX
        .checked_sub(
            residue
                .checked_sub(1)
                .ok_or(ByeMachineError::ArithmeticFailure)?,
        )
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    Ok(Some(bound))
}

/// The candidate word for one attempt: the low 16 bytes of
/// `sha256(randomness ‖ roll_index ‖ attempt)`, little-endian.
fn draw_word(randomness: &[u8; 32], roll_index: u8, attempt: u8) -> u128 {
    let digest = hashv(&[randomness, &[roll_index], &[attempt]]).to_bytes();
    let mut low = [0u8; 16];
    low.copy_from_slice(&digest[0..16]);
    u128::from_le_bytes(low)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{DEFAULT_BLANK_FACES, WEIGHT_C};
    use serde_json::Value;

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn seed(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    struct Xorshift64(u64);

    impl Xorshift64 {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    // --- (a) the published vector ---------------------------------------------------------

    /// The committed artifact a third-party implementation is checked against. Its expected
    /// values come from an independent implementation of the written formula, not from `expand`.
    const PUBLISHED_VECTOR: &str = include_str!("../../../../tests/fixtures/expansion-vector.json");

    fn vector_cases() -> Vec<Value> {
        let parsed: Value =
            serde_json::from_str(PUBLISHED_VECTOR).expect("published vector is not valid JSON");
        parsed["cases"]
            .as_array()
            .expect("published vector has no `cases` array")
            .clone()
    }

    fn u128_field(case: &Value, key: &str) -> u128 {
        case[key]
            .as_str()
            .unwrap_or_else(|| panic!("case field `{key}` is not a decimal string: {case}"))
            .parse()
            .unwrap_or_else(|_| panic!("case field `{key}` does not parse as u128: {case}"))
    }

    fn randomness_field(case: &Value) -> [u8; 32] {
        let hex = case["randomness"]
            .as_str()
            .expect("randomness is not a string");
        assert_eq!(hex.len(), 64, "randomness is not 32 hex bytes: {hex}");
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .unwrap_or_else(|_| panic!("randomness is not hex: {hex}"));
        }
        out
    }

    #[test]
    fn published_vector_reproduces_exactly() {
        let cases = vector_cases();
        for case in &cases {
            let name = case["name"].as_str().unwrap_or("<unnamed>");
            let drawn = expand(
                &randomness_field(case),
                u8::try_from(case["k"].as_u64().expect("k is not a number")).expect("k exceeds u8"),
                u128_field(case, "w"),
            )
            .unwrap_or_else(|e| panic!("case `{name}` failed: {e:?}"));

            assert_eq!(
                drawn.r,
                u128_field(case, "r"),
                "case `{name}`: reduced draw value diverged from the published vector"
            );
            assert_eq!(
                u64::from(drawn.attempts),
                case["attempts"].as_u64().expect("attempts is not a number"),
                "case `{name}`: attempt count diverged from the published vector"
            );
        }
    }

    /// The vector covers the rejection branch, which is unreachable by chance.
    #[test]
    fn published_vector_covers_every_branch_of_the_expansion() {
        let cases = vector_cases();
        assert!(
            cases.len() >= 6,
            "the published vector shrank to {} cases",
            cases.len()
        );

        let attempts: Vec<u64> = cases
            .iter()
            .map(|c| c["attempts"].as_u64().expect("attempts is not a number"))
            .collect();

        assert!(
            attempts.contains(&1),
            "no first-attempt acceptance in the published vector"
        );
        assert!(
            attempts
                .iter()
                .any(|a| (2..u64::from(EXPANSION_MAX_ATTEMPTS)).contains(a)),
            "no mid-run acceptance in the published vector — the rejection branch is unpinned"
        );
        assert!(
            attempts.contains(&u64::from(EXPANSION_MAX_ATTEMPTS)),
            "no fallback case in the published vector — the capped branch is unpinned"
        );
        assert!(
            cases.iter().any(|c| u128_field(c, "w") == 1),
            "no degenerate single-weight case in the published vector"
        );
        assert!(
            cases.iter().any(|c| u128_field(c, "w") == 1u128 << 127),
            "no case where the weight divides 2^128 and there is no rejection region"
        );
    }

    // --- (a) the rejection branch, reached deterministically -------------------------------

    /// One past a half of the range: `2^128 mod w` is `w - 2`, so the rejection region is very
    /// nearly half of `2^128` and a rejection is reached in a handful of tries. At a realistic
    /// weight the same branch has probability about 2^-78 and no amount of random sampling
    /// reaches it.
    const HALF_RANGE_PLUS_ONE: u128 = (1u128 << 127) + 1;

    #[test]
    fn acceptance_bound_is_the_largest_multiple_of_the_weight_below_two_to_the_128() {
        // Independent of `acceptance_bound`'s own expression: for each weight, the bound must be
        // a multiple of it, and one more multiple must not fit under 2^128.
        for w in [
            HALF_RANGE_PLUS_ONE,
            3,
            7,
            1_000_000_007,
            u128::from(u64::MAX),
            u128::MAX,
        ] {
            let bound = acceptance_bound(w)
                .unwrap()
                .expect("expected a rejection region");
            assert_eq!(bound % w, 0, "bound is not a multiple of w={w}");
            assert!(
                bound.checked_add(w).is_none(),
                "w={w}: another multiple still fits below 2^128, so the bound is not the largest"
            );
        }
    }

    /// The one comparison sampling cannot reach: exactly one word in 2^128 sits on the boundary.
    #[test]
    fn the_acceptance_boundary_is_exclusive() {
        let w = HALF_RANGE_PLUS_ONE;
        let bound = acceptance_bound(w).unwrap();
        let edge = bound.expect("expected a rejection region");
        assert_eq!(
            edge % w,
            0,
            "the bound must be a multiple of w, so it reduces to 0"
        );

        assert!(
            accepts(edge - 1, bound),
            "the last word inside the region was rejected"
        );
        assert!(
            !accepts(edge, bound),
            "the bound itself was accepted — it reduces to 0, which hands the draw value 0 one \
             extra chance in 2^128 and is exactly the bias the rejection step removes"
        );
        assert!(
            !accepts(edge + 1, bound),
            "a word past the bound was accepted"
        );
        assert!(accepts(0, bound));
    }

    #[test]
    fn with_no_rejection_region_every_word_is_accepted_including_the_largest() {
        assert!(accepts(0, None));
        assert!(accepts(u128::MAX, None));
    }

    #[test]
    fn weights_dividing_two_to_the_128_have_no_rejection_region() {
        for shift in 0..128u32 {
            assert_eq!(
                acceptance_bound(1u128 << shift).unwrap(),
                None,
                "2^{shift} divides 2^128 exactly, so no word can be rejected"
            );
        }
    }

    #[test]
    fn a_rejected_word_advances_the_attempt_and_changes_the_hash_input() {
        let bound = acceptance_bound(HALF_RANGE_PLUS_ONE).unwrap().unwrap();

        let mut saw_rejection = false;
        for k in 0..64u8 {
            let randomness = seed(0xA5);
            if draw_word(&randomness, k, 0) < bound {
                continue;
            }
            saw_rejection = true;

            let drawn = expand(&randomness, k, HALF_RANGE_PLUS_ONE).unwrap();
            assert!(
                drawn.attempts >= 2,
                "k={k}: attempt 0 was rejected but the count stayed at {}",
                drawn.attempts
            );

            // The deciding word must be the one for `attempts - 1`, not attempt 0 re-reduced.
            let deciding = draw_word(&randomness, k, drawn.attempts - 1);
            assert_eq!(
                drawn.r,
                deciding % HALF_RANGE_PLUS_ONE,
                "k={k}: the recorded draw does not come from the attempt it recorded"
            );
            assert_ne!(
                draw_word(&randomness, k, 0),
                deciding,
                "k={k}: the attempt byte did not change the hash input"
            );
        }
        assert!(
            saw_rejection,
            "no rejection reached at a weight where roughly half of all words are rejected"
        );
    }

    #[test]
    fn the_cap_falls_back_to_reducing_the_last_word_rather_than_looping() {
        let bound = acceptance_bound(HALF_RANGE_PLUS_ONE).unwrap().unwrap();

        let mut saw_fallback = false;
        for byte in 0..=255u8 {
            let randomness = seed(byte);
            let all_rejected =
                (0..EXPANSION_MAX_ATTEMPTS).all(|a| draw_word(&randomness, 0, a) >= bound);
            if !all_rejected {
                continue;
            }
            saw_fallback = true;

            let drawn = expand(&randomness, 0, HALF_RANGE_PLUS_ONE).unwrap();
            assert_eq!(
                drawn.attempts, EXPANSION_MAX_ATTEMPTS,
                "every attempt was rejected, so the cap must be recorded in full"
            );
            assert_eq!(
                drawn.r,
                draw_word(&randomness, 0, EXPANSION_MAX_ATTEMPTS - 1) % HALF_RANGE_PLUS_ONE,
                "the fallback must reduce the last word drawn, not an earlier one"
            );
        }
        assert!(
            saw_fallback,
            "no seed rejected all {EXPANSION_MAX_ATTEMPTS} attempts at a weight that rejects \
             roughly half of all words — expected about one seed in 16"
        );
    }

    #[test]
    fn a_draw_is_always_inside_the_weight_and_the_count_is_always_in_range() {
        let mut rng = Xorshift64(0x1234_5678_9abc_def1);
        for k in 0..16u8 {
            for _ in 0..64 {
                let w = u128::from(rng.next()).max(1);
                let drawn = expand(&seed(u8::try_from(rng.below(256)).unwrap()), k, w).unwrap();
                assert!(drawn.r < w, "draw {} is not below w={w}", drawn.r);
                assert!(
                    (1..=EXPANSION_MAX_ATTEMPTS).contains(&drawn.attempts),
                    "attempt count {} is outside 1..={EXPANSION_MAX_ATTEMPTS}",
                    drawn.attempts
                );
            }
        }
    }

    #[test]
    fn the_roll_index_separates_the_draws_of_one_batch() {
        let randomness = seed(0x11);
        let w = u128::from(u64::MAX);
        let draws: Vec<u128> = (0..10u8)
            .map(|k| expand(&randomness, k, w).unwrap().r)
            .collect();
        for (i, a) in draws.iter().enumerate() {
            for (j, b) in draws.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "rolls {i} and {j} expanded to the same draw");
                }
            }
        }
    }

    #[test]
    fn expand_rejects_a_zero_weight() {
        assert_eq!(error_code(expand(&seed(1), 0, 0).unwrap_err()), 6703);
    }

    // --- (b) the draw mapping ---------------------------------------------------------------

    /// Naive ground truth for the mapping: build the published range order as an explicit list
    /// of one interval per blank denomination followed by the real span, then scan it. Shares no
    /// expression with `map_draw` — a linear scan cannot be wrong in the same way a running
    /// prefix can.
    fn scan_bands(
        r: u128,
        w_real: u128,
        blanks: [u32; BLANK_SLOTS],
        blank_faces: [u64; BLANK_SLOTS],
    ) -> Option<DrawTarget> {
        let mut bands: Vec<(u128, Option<u8>)> = Vec::new();
        for d in 0..8usize {
            if d >= 3 && blank_faces[d] == 0 {
                continue;
            }
            let width = (WEIGHT_C / u128::from(blank_faces[d])) * u128::from(blanks[d]);
            bands.push((width, Some(u8::try_from(d).unwrap())));
        }
        bands.push((w_real, None));

        let mut low = 0u128;
        for (width, face) in bands {
            let high = low + width;
            if r >= low && r < high {
                return Some(match face {
                    Some(face_index) => DrawTarget::Blank { face_index },
                    None => DrawTarget::Real {
                        key: u64::try_from(r - low).unwrap(),
                    },
                });
            }
            low = high;
        }
        None
    }

    fn blank_prefix(blanks: [u32; BLANK_SLOTS], blank_faces: [u64; BLANK_SLOTS]) -> u128 {
        crate::common::math::aggregate_weight(0, blanks, blank_faces).unwrap()
    }

    #[test]
    fn every_band_edge_maps_from_both_sides() {
        let faces = DEFAULT_BLANK_FACES;
        let blanks = [3u32, 2, 1, 0, 0, 0, 0, 0];
        let w_real = u128::from(weight_of(50_000_000).unwrap());

        let w1 = u128::from(weight_of(faces[0]).unwrap()) * 3;
        let w2 = u128::from(weight_of(faces[1]).unwrap()) * 2;
        let w5 = u128::from(weight_of(faces[2]).unwrap());

        let edges = [w1, w1 + w2, w1 + w2 + w5];
        let expected_below = [
            DrawTarget::Blank { face_index: 0 },
            DrawTarget::Blank { face_index: 1 },
            DrawTarget::Blank { face_index: 2 },
        ];
        let expected_at = [
            DrawTarget::Blank { face_index: 1 },
            DrawTarget::Blank { face_index: 2 },
            DrawTarget::Real { key: 0 },
        ];

        for (i, edge) in edges.iter().enumerate() {
            assert_eq!(
                map_draw(edge - 1, w_real, blanks, faces).unwrap(),
                expected_below[i],
                "edge {i}: the last value below the edge left its own band"
            );
            assert_eq!(
                map_draw(*edge, w_real, blanks, faces).unwrap(),
                expected_at[i],
                "edge {i}: the edge itself did not open the next band"
            );
        }
    }

    /// The real branch takes the draw with the blank prefix removed. Pinning it needs a pool that
    /// actually holds blanks: at zero blanks the prefix is zero and the correct and the wrong
    /// argument are the same number.
    #[test]
    fn the_real_branch_argument_has_the_blank_prefix_removed() {
        let faces = DEFAULT_BLANK_FACES;
        let blanks = [7u32, 5, 3, 0, 0, 0, 0, 0];
        let w_real = u128::from(weight_of(50_000_000).unwrap()) * 4;
        let prefix = blank_prefix(blanks, faces);
        assert!(
            prefix > 0,
            "this test is vacuous without a non-zero blank prefix"
        );

        for offset in [0u128, 1, 17, w_real - 1] {
            let r = prefix + offset;
            assert_eq!(
                map_draw(r, w_real, blanks, faces).unwrap(),
                DrawTarget::Real {
                    key: u64::try_from(offset).unwrap()
                },
                "the real branch was handed the raw draw value instead of the offset into the \
                 real span"
            );
        }
    }

    #[test]
    fn the_prefix_the_mapping_walks_is_the_pool_aggregate_blank_weight() {
        // The modulus the draw reduces by and the range order the draw is mapped through must be
        // computed from the same blank weight; if they diverge, every draw near the boundary
        // lands in the wrong band while the middle of each band still looks correct.
        let faces = DEFAULT_BLANK_FACES;
        let w_real = u128::from(weight_of(20_000_000).unwrap());
        for blanks in [
            [0u32, 0, 0, 0, 0, 0, 0, 0],
            [1, 0, 0, 0, 0, 0, 0, 0],
            [0, 1, 0, 0, 0, 0, 0, 0],
            [0, 0, 1, 0, 0, 0, 0, 0],
            [9, 4, 2, 0, 0, 0, 0, 0],
        ] {
            let prefix = blank_prefix(blanks, faces);
            assert_eq!(
                map_draw(prefix, w_real, blanks, faces).unwrap(),
                DrawTarget::Real { key: 0 },
                "blanks={blanks:?}: the real span does not start at the aggregate blank weight"
            );
            if prefix > 0 {
                assert!(
                    matches!(
                        map_draw(prefix - 1, w_real, blanks, faces).unwrap(),
                        DrawTarget::Blank { .. }
                    ),
                    "blanks={blanks:?}: the last blank value fell through to the real span"
                );
            }
        }
    }

    #[test]
    fn mapping_matches_a_naive_linear_scan_over_randomized_pools() {
        let mut rng = Xorshift64(0x0f0f_1234_5678_9001);
        for i in 0..20_000 {
            // Alternately the launch faces and a pool with an above-ticket face in slot 3.
            let faces = if i % 2 == 0 {
                DEFAULT_BLANK_FACES
            } else {
                [5_000_000, 10_000_000, 25_000_000, 100_000_000, 0, 0, 0, 0]
            };
            let mut blanks = [0u32; 8];
            for (slot, count) in blanks.iter_mut().enumerate() {
                if faces[slot] != 0 {
                    *count = u32::try_from(rng.below(40)).unwrap();
                }
            }
            let w_real = u128::from(weight_of(12_000_000 + rng.below(500_000_000)).unwrap())
                * u128::from(1 + rng.below(30));
            let total = blank_prefix(blanks, faces) + w_real;
            let r = u128::from(rng.next()) % total;

            assert_eq!(
                map_draw(r, w_real, blanks, faces).unwrap(),
                scan_bands(r, w_real, blanks, faces).unwrap(),
                "r={r} blanks={blanks:?} w_real={w_real}"
            );
        }
    }

    #[test]
    fn a_draw_at_or_past_the_total_weight_is_rejected() {
        let faces = DEFAULT_BLANK_FACES;
        let blanks = [2u32, 2, 2, 0, 0, 0, 0, 0];
        let w_real = u128::from(weight_of(30_000_000).unwrap());
        let total = blank_prefix(blanks, faces) + w_real;

        assert_eq!(
            error_code(map_draw(total, w_real, blanks, faces).unwrap_err()),
            6703
        );
        assert_eq!(
            error_code(map_draw(total + 1, w_real, blanks, faces).unwrap_err()),
            6703
        );
        assert!(map_draw(total - 1, w_real, blanks, faces).is_ok());
    }

    #[test]
    fn a_pool_with_no_real_weight_maps_only_into_blanks() {
        let faces = DEFAULT_BLANK_FACES;
        let blanks = [1u32, 1, 1, 0, 0, 0, 0, 0];
        let prefix = blank_prefix(blanks, faces);

        for r in [0u128, prefix - 1] {
            assert!(matches!(
                map_draw(r, 0, blanks, faces).unwrap(),
                DrawTarget::Blank { .. }
            ));
        }
        assert_eq!(
            error_code(map_draw(prefix, 0, blanks, faces).unwrap_err()),
            6703
        );
    }

    #[test]
    fn a_zero_face_fails_in_slots_0_to_2_and_is_skipped_in_slot_3_and_after() {
        // Slots 0–2 always hold a face, so a 0 there is a fault the draw refuses (`ZeroValue`).
        let mut faces = [1_000_000, 0, 5_000_000, 0, 0, 0, 0, 0];
        assert_eq!(
            error_code(map_draw(0, 1, [0, 0, 1, 0, 0, 0, 0, 0], faces).unwrap_err()),
            6401
        );
        // Slot 3 with no face is empty: its stale count has no band, so draw 0 is a real card.
        faces[1] = 2_000_000;
        assert_eq!(
            map_draw(0, 1, [0, 0, 0, 7, 7, 0, 0, 0], faces).unwrap(),
            DrawTarget::Real { key: 0 }
        );
        // The low-value case: every face set, below-ticket counts 0.
        let gold = [5_000_000, 10_000_000, 25_000_000, 100_000_000, 0, 0, 0, 0];
        assert_eq!(
            map_draw(0, 1, [0, 0, 0, 1, 0, 0, 0, 0], gold).unwrap(),
            DrawTarget::Blank { face_index: 3 }
        );
    }

    #[test]
    fn the_above_ticket_slot_bands_after_the_below_ticket_slots() {
        let faces = [5_000_000, 10_000_000, 25_000_000, 100_000_000, 0, 0, 0, 0];
        let blanks = [1u32, 1, 1, 2, 0, 0, 0, 0];
        let w_real = u128::from(weight_of(30_000_000).unwrap());
        let below = blank_prefix([1, 1, 1, 0, 0, 0, 0, 0], faces);
        let slot_3 = u128::from(weight_of(faces[3]).unwrap()) * 2;
        assert_eq!(
            map_draw(below, w_real, blanks, faces).unwrap(),
            DrawTarget::Blank { face_index: 3 }
        );
        assert_eq!(
            map_draw(below + slot_3 - 1, w_real, blanks, faces).unwrap(),
            DrawTarget::Blank { face_index: 3 }
        );
        assert_eq!(
            map_draw(below + slot_3, w_real, blanks, faces).unwrap(),
            DrawTarget::Real { key: 0 }
        );
    }

    // --- expansion and mapping composed -----------------------------------------------------

    #[test]
    fn an_expanded_draw_always_maps_somewhere() {
        let faces = DEFAULT_BLANK_FACES;
        let mut rng = Xorshift64(0xabcd_ef01_2345_6789);
        for k in 0..10u8 {
            for _ in 0..200 {
                let blanks = [
                    u32::try_from(rng.below(20)).unwrap(),
                    u32::try_from(rng.below(20)).unwrap(),
                    u32::try_from(rng.below(20)).unwrap(),
                    0,
                    0,
                    0,
                    0,
                    0,
                ];
                let w_real = u128::from(weight_of(12_000_000 + rng.below(200_000_000)).unwrap())
                    * u128::from(1 + rng.below(50));
                let w = blank_prefix(blanks, faces) + w_real;

                let drawn = expand(&seed(u8::try_from(rng.below(256)).unwrap()), k, w).unwrap();
                map_draw(drawn.r, w_real, blanks, faces)
                    .unwrap_or_else(|e| panic!("r={} w={w} did not map: {e:?}", drawn.r));
            }
        }
    }

    // --- grep-assertable — no unchecked arithmetic in this module's production code ---------

    #[test]
    fn no_unchecked_arithmetic_outside_test_module() {
        let source = include_str!("expansion.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for (line_no, line) in production.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("use ") {
                continue; // glob imports (`prelude::*`) are not arithmetic
            }
            let code = line.split("//").next().unwrap_or("").replace("->", "");
            for op in ['+', '-', '*', '/', '%'] {
                assert!(
                    !code.contains(op),
                    "raw `{op}` at expansion.rs:{}: {line}",
                    line_no + 1
                );
            }
        }
    }
}
