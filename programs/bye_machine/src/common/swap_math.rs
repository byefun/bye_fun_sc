//! The blank swap's `min_out` bound, and the 256-bit arithmetic it needs.
//!
//! Pure over explicit inputs — nothing here reads an account. A blank settlement values the
//! drawn face in $BYE at the pinned DAMM v2 pool's own price and refuses to accept less than
//! that figure minus the configured slippage budget, so this is the one place the bound is
//! derived and the only place the fixed-point convention is written down.
//!
//! **DAMM v2's `sqrt_price` is Q64.64 over *base units*, and the conversion is decimal-free.**
//! The identity is `price_raw(B per A) = sqrt_price² / 2¹²⁸`, base unit per base unit. Applying
//! a `10^(decA−decB)` factor is what the SDK's `getPriceFromSqrtPrice` does to produce a
//! *display* price, and importing that factor here would scale every bound by a thousand in one
//! direction or the other. $BYE's nine decimals fix the magnitudes of the vectors this module
//! is tested against, but no function here reads a mint's decimals and none may start.
//!
//! **The intermediates do not fit in u128, which is why [`U256`] exists.** `MAX_SQRT_PRICE²` is
//! about 2¹⁹² and `face · 2¹²⁸` reaches about 2¹⁵⁰, against a u128 ceiling of 2¹²⁸. Squaring
//! `sqrt_price` into a u128 is the specific mistake this module's shape exists to prevent: it
//! overflows for every price the pool can actually hold, and a `checked_mul` there turns every
//! blank settlement into an `ArithmeticFailure` rather than a payout.
//!
//! **Rounding is floor at both steps**, which biases `min_out` downward — toward completing the
//! swap rather than refusing it. `max_swap_slippage_bps` is the safety margin; rounding up would
//! add a second, implicit one and refuse settlements the bound was configured to admit.

use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;

/// A 256-bit unsigned integer as four little-endian 64-bit limbs.
///
/// Deliberately minimal: this carries the four operations the bound needs and nothing else. A wider
/// surface would be code no settlement executes, and the SBF frame budget charges for reachable
/// code whether or not a call site exists.
///
/// Not `pub` beyond the crate — the type is an implementation detail of [`blank_min_out`], and a
/// second caller would be a second fixed-point convention.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub(crate) struct U256 {
    /// Little-endian: `limbs[0]` is the least significant.
    limbs: [u64; 4],
}

impl U256 {
    pub(crate) const ZERO: Self = Self { limbs: [0; 4] };

    /// The low 128 bits, with the high 128 bits zeroed.
    pub(crate) fn from_u128(v: u128) -> Self {
        Self {
            limbs: [v as u64, (v >> 64) as u64, 0, 0],
        }
    }

    /// `v · 2¹²⁸`, exactly — the numerator when $BYE is the pool's token A.
    ///
    /// Written as a placement rather than a shift because that is what it is: the value lands in
    /// the high half and the low half is zero. A `checked_shl` formulation would have to defend
    /// against an overflow this construction cannot have.
    pub(crate) fn from_u128_shifted_128(v: u128) -> Self {
        Self {
            limbs: [0, 0, v as u64, (v >> 64) as u64],
        }
    }

    /// The exact 256-bit product of two 128-bit values. Cannot overflow: two factors below 2¹²⁸
    /// multiply to below 2²⁵⁶.
    pub(crate) fn mul_u128(a: u128, b: u128) -> Self {
        let (a_lo, a_hi) = (a as u64 as u128, a >> 64);
        let (b_lo, b_hi) = (b as u64 as u128, b >> 64);

        // Three-column schoolbook accumulation. Each partial product is at most
        // (2⁶⁴−1)², which fits a u128 with room for the carries folded in below.
        let ll = a_lo * b_lo;
        let lh = a_lo * b_hi;
        let hl = a_hi * b_lo;
        let hh = a_hi * b_hi;

        let mid = (ll >> 64) + (lh & 0xFFFF_FFFF_FFFF_FFFF) + (hl & 0xFFFF_FFFF_FFFF_FFFF);
        let lo = (ll & 0xFFFF_FFFF_FFFF_FFFF) | (mid << 64);
        let hi = hh + (lh >> 64) + (hl >> 64) + (mid >> 64);

        Self {
            limbs: [lo as u64, (lo >> 64) as u64, hi as u64, (hi >> 64) as u64],
        }
    }

    /// `self · v`, refused rather than wrapped when the product leaves 256 bits.
    pub(crate) fn checked_mul_u64(self, v: u64) -> Option<Self> {
        let mut out = [0u64; 4];
        let mut carry = 0u128;
        for (i, slot) in out.iter_mut().enumerate() {
            let acc = u128::from(self.limbs[i]) * u128::from(v) + carry;
            *slot = acc as u64;
            carry = acc >> 64;
        }
        if carry != 0 {
            return None;
        }
        Some(Self { limbs: out })
    }

    /// `self >> 128`, as a u128 — the divide-by-2¹²⁸ when $BYE is the pool's token B.
    ///
    /// Exact and lossless in the sense that matters: the discarded low half is the fractional
    /// part, so discarding it *is* the specified floor.
    pub(crate) fn shr_128_to_u128(self) -> u128 {
        u128::from(self.limbs[2]) | (u128::from(self.limbs[3]) << 64)
    }

    pub(crate) fn is_zero(self) -> bool {
        self.limbs == [0; 4]
    }

    /// Index of the highest set bit plus one; zero for zero. Bounds the division loop below to
    /// the operand's actual width instead of a flat 256 iterations.
    fn bit_len(self) -> u32 {
        for i in (0..4).rev() {
            if self.limbs[i] != 0 {
                return (i as u32) * 64 + (64 - self.limbs[i].leading_zeros());
            }
        }
        0
    }

    fn bit(self, i: u32) -> bool {
        (self.limbs[(i / 64) as usize] >> (i % 64)) & 1 == 1
    }

    fn set_bit(&mut self, i: u32) {
        self.limbs[(i / 64) as usize] |= 1u64 << (i % 64);
    }

    /// `self << 1`, dropping any bit shifted off the top. Only ever applied to a running
    /// remainder that is strictly below the divisor, so the dropped bit is always zero.
    fn shl1(self) -> Self {
        let mut out = [0u64; 4];
        let mut carry = 0u64;
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = (self.limbs[i] << 1) | carry;
            carry = self.limbs[i] >> 63;
        }
        Self { limbs: out }
    }

    /// `self − other`, wrapping. Only called under a `self >= other` test.
    fn wrapping_sub(self, other: Self) -> Self {
        let mut out = [0u64; 4];
        let mut borrow = 0u64;
        for (i, slot) in out.iter_mut().enumerate() {
            let (d, b1) = self.limbs[i].overflowing_sub(other.limbs[i]);
            let (d, b2) = d.overflowing_sub(borrow);
            *slot = d;
            borrow = u64::from(b1 || b2);
        }
        Self { limbs: out }
    }

    /// Floor division, `None` on a zero divisor.
    ///
    /// Restoring binary long division — one shift, one compare and one conditional subtract per
    /// bit of the numerator. Chosen over a limb-wise quotient estimate because it is correct by
    /// inspection at every width and needs no normalisation step; the operands here are at most
    /// 215 bits, so the loop it costs is bounded and paid once per blank settlement.
    pub(crate) fn div(self, divisor: Self) -> Option<Self> {
        if divisor.is_zero() {
            return None;
        }
        let mut quotient = Self::ZERO;
        let mut remainder = Self::ZERO;

        for i in (0..self.bit_len()).rev() {
            remainder = remainder.shl1();
            if self.bit(i) {
                remainder.set_bit(0);
            }
            if remainder >= divisor {
                remainder = remainder.wrapping_sub(divisor);
                quotient.set_bit(i);
            }
        }
        Some(quotient)
    }

    /// The value as a u64, `None` when it does not fit — a narrowing that must surface as a
    /// returned `ArithmeticFailure` rather than a silent truncation.
    pub(crate) fn to_u64(self) -> Option<u64> {
        if self.limbs[1] != 0 || self.limbs[2] != 0 || self.limbs[3] != 0 {
            return None;
        }
        Some(self.limbs[0])
    }
}

/// **Ordering walks the limbs from the most significant down**, which a `derive` would not: the
/// limbs are little-endian, so a derived `Ord` compares `limbs[0]` first and orders 2⁶⁴−1 above
/// 2¹⁹². The division loop's `remainder >= divisor` test is the only consumer, and under the
/// derived ordering it subtracts at the wrong bits and returns a wrong quotient for operands
/// that differ above the low limb — which is every operand this module actually divides.
impl Ord for U256 {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        for i in (0..4).rev() {
            match self.limbs[i].cmp(&other.limbs[i]) {
                core::cmp::Ordering::Equal => continue,
                non_eq => return non_eq,
            }
        }
        core::cmp::Ordering::Equal
    }
}

impl PartialOrd for U256 {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// How the pinned pool orders the pair. Read from the pool account, never assumed: a
/// sagapad-created pool puts $BYE at `token_a`, but `swap_pool` may be any DAMM v2 $BYE/USDC
/// pool and the two orderings invert the price quantity.
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub enum PairOrdering {
    /// `token_a` is $BYE and `token_b` is USDC. `sqrt_price²/2¹²⁸` is USDC per $BYE, so the
    /// face divides by it.
    ByeIsA,
    /// `token_a` is USDC and `token_b` is $BYE. `sqrt_price²/2¹²⁸` is $BYE per USDC, so the
    /// face multiplies by it.
    ByeIsB,
}

/// A pair of values reordered into the venue's own `(token_a, token_b)` sequence.
///
/// Generic so the one decision serves both callers: the settlement passes account handles, and
/// the tests pass addresses they can compare. A second `match` on the ordering elsewhere is a
/// second chance to get it backwards — and backwards is not a rejected pool but a *valid* one
/// whose swap the venue refuses, which leaves the roll unsettled and the batch stalled.
pub fn in_pair_order<T>(ordering: PairOrdering, bye: T, usdc: T) -> (T, T) {
    match ordering {
        PairOrdering::ByeIsA => (bye, usdc),
        PairOrdering::ByeIsB => (usdc, bye),
    }
}

/// The least $BYE a blank settlement will accept for `face_raw` micro-USDC, at the pool's own
/// price, less the configured slippage budget.
///
/// `sqrt_price` is the pool's live Q64.64 value, read in the same instruction as the swap CPI.
/// The bound it produces covers the venue's trading fee and the curve's price impact — not price
/// drift, which the same-instruction read excludes.
pub fn blank_min_out(
    face_raw: u64,
    sqrt_price: u128,
    ordering: PairOrdering,
    max_swap_slippage_bps: u16,
) -> Result<u64> {
    require!(sqrt_price > 0, ByeMachineError::ArithmeticFailure);
    // At 10_000 the bound collapses to zero, which is a swap with no slippage protection at all
    // — a configuration this function will not silently price.
    require!(
        max_swap_slippage_bps < 10_000,
        ByeMachineError::ArithmeticFailure
    );

    let sqrt_price_squared = U256::mul_u128(sqrt_price, sqrt_price);

    let expected_out: U256 = match ordering {
        // USDC is B, $BYE is A: expected_out = face · 2¹²⁸ / sqrt_price²
        PairOrdering::ByeIsA => U256::from_u128_shifted_128(u128::from(face_raw))
            .div(sqrt_price_squared)
            .ok_or(ByeMachineError::ArithmeticFailure)?,
        // USDC is A, $BYE is B: expected_out = face · sqrt_price² / 2¹²⁸
        PairOrdering::ByeIsB => U256::from_u128(
            sqrt_price_squared
                .checked_mul_u64(face_raw)
                .ok_or(ByeMachineError::ArithmeticFailure)?
                .shr_128_to_u128(),
        ),
    };

    let keep_bps = 10_000u64
        .checked_sub(u64::from(max_swap_slippage_bps))
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    let min_out = expected_out
        .checked_mul_u64(keep_bps)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .div(U256::from_u128(10_000))
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    min_out
        .to_u64()
        .ok_or_else(|| ByeMachineError::ArithmeticFailure.into())
}

/// `sqrt_price` for $BYE at 0.01 USDC with $BYE at `token_a`, from the published vector table.
///
/// Shared rather than duplicated: the committed integration fixture is built at this price and
/// asserts against this constant, so the unit vectors and the on-validator pool cannot come to
/// price at two different points.
#[cfg(test)]
pub(crate) const SQRT_PRICE_BYE_IS_A: u128 = 58_333_726_687_135_160;

#[cfg(test)]
mod tests {
    use super::*;

    /// `Ord` on the limb array is not the ordering the division loop needs. This is the tripwire
    /// for that: it fails if the derive is ever relied on where `cmp_be` is meant.
    #[test]
    fn ordering_compares_most_significant_limb_first() {
        let small_high_limb = U256 {
            limbs: [u64::MAX, 0, 0, 0],
        };
        let large_high_limb = U256 {
            limbs: [0, 0, 0, 1],
        };
        assert_eq!(
            small_high_limb.cmp(&large_high_limb),
            core::cmp::Ordering::Less,
            "2⁶⁴−1 must order below 2¹⁹²"
        );
    }

    fn u256_from_str(decimal: &str) -> U256 {
        // Test-only decimal parse, built from the type's own multiply and add so a vector can be
        // written as the literal the spec table prints rather than as four limbs.
        let mut acc = U256::ZERO;
        for byte in decimal.bytes() {
            let digit = byte - b'0';
            acc = acc.checked_mul_u64(10).expect("vector fits 256 bits");
            acc = add(acc, U256::from_u128(u128::from(digit)));
        }
        acc
    }

    fn add(a: U256, b: U256) -> U256 {
        let mut out = [0u64; 4];
        let mut carry = 0u64;
        for (i, slot) in out.iter_mut().enumerate() {
            let sum = u128::from(a.limbs[i]) + u128::from(b.limbs[i]) + u128::from(carry);
            *slot = sum as u64;
            carry = (sum >> 64) as u64;
        }
        U256 { limbs: out }
    }

    /// The same economics with the pair inverted — a different quantity, not a rescaling.
    const SQRT_PRICE_BYE_IS_B: u128 = 5_833_372_668_713_516_400_640;

    const FACE_1_USDC: u64 = 1_000_000;
    const FACE_2_USDC: u64 = 2_000_000;
    const FACE_5_USDC: u64 = 5_000_000;

    /// The configured deploy value.
    const SLIPPAGE_BPS: u16 = 100;

    /// The published vectors, both orderings, all three faces. These are the artifact a
    /// reviewer checks the implementation against, so they are asserted as the exact integers the
    /// table prints — a tolerance here would accept the decimal-scaling error the module exists
    /// to prevent, which is off by a factor of 10³.
    #[test]
    fn matches_the_published_vectors_with_bye_as_token_a() {
        for (face, min_out) in [
            (FACE_1_USDC, 98_999_999_999u64),
            (FACE_2_USDC, 197_999_999_999),
            (FACE_5_USDC, 494_999_999_999),
        ] {
            assert_eq!(
                blank_min_out(
                    face,
                    SQRT_PRICE_BYE_IS_A,
                    PairOrdering::ByeIsA,
                    SLIPPAGE_BPS
                )
                .unwrap(),
                min_out,
                "face {face}"
            );
        }
    }

    #[test]
    fn matches_the_published_vectors_with_bye_as_token_b() {
        for (face, min_out) in [
            (FACE_1_USDC, 99_000_000_000u64),
            (FACE_2_USDC, 198_000_000_000),
            (FACE_5_USDC, 495_000_000_000),
        ] {
            assert_eq!(
                blank_min_out(
                    face,
                    SQRT_PRICE_BYE_IS_B,
                    PairOrdering::ByeIsB,
                    SLIPPAGE_BPS
                )
                .unwrap(),
                min_out,
                "face {face}"
            );
        }
    }

    /// The `expected_out` column of the same table — the figure before the haircut. Asserted
    /// separately because a bug that scales both columns equally passes the `min_out` rows by
    /// cancelling against the ratio.
    #[test]
    fn expected_out_before_the_haircut_matches_the_vectors() {
        for (face, expected) in [
            (FACE_1_USDC, 99_999_999_999u64),
            (FACE_2_USDC, 199_999_999_999),
            (FACE_5_USDC, 499_999_999_999),
        ] {
            assert_eq!(
                blank_min_out(face, SQRT_PRICE_BYE_IS_A, PairOrdering::ByeIsA, 0).unwrap(),
                expected,
                "face {face}, zero slippage"
            );
        }
        for (face, expected) in [
            (FACE_1_USDC, 100_000_000_000u64),
            (FACE_2_USDC, 200_000_000_000),
            (FACE_5_USDC, 500_000_000_000),
        ] {
            assert_eq!(
                blank_min_out(face, SQRT_PRICE_BYE_IS_B, PairOrdering::ByeIsB, 0).unwrap(),
                expected,
                "face {face}, zero slippage"
            );
        }
    }

    /// The two orderings must not agree. If a refactor ever makes `PairOrdering` a no-op — the
    /// natural shape of "read the ordering, then ignore it" — every vector above still passes,
    /// because each is checked against its own `sqrt_price`. This is what catches that.
    #[test]
    fn the_two_orderings_are_not_interchangeable() {
        let as_a = blank_min_out(
            FACE_5_USDC,
            SQRT_PRICE_BYE_IS_A,
            PairOrdering::ByeIsA,
            SLIPPAGE_BPS,
        )
        .unwrap();
        let mislabelled = blank_min_out(
            FACE_5_USDC,
            SQRT_PRICE_BYE_IS_A,
            PairOrdering::ByeIsB,
            SLIPPAGE_BPS,
        )
        .unwrap();
        assert_ne!(
            as_a, mislabelled,
            "the same sqrt_price read under the wrong ordering must not produce the same bound"
        );
    }

    /// The squaring that must never be attempted in a u128. `MAX_SQRT_PRICE` squares to about
    /// 2¹⁹², so a u128 intermediate wraps or refuses; this asserts the U256 path carries it.
    #[test]
    fn the_widest_sqrt_price_squares_without_overflow() {
        // DAMM v2's MAX_SQRT_PRICE.
        const MAX_SQRT_PRICE: u128 = 79_226_673_521_066_979_257_578_248_091;
        let squared = U256::mul_u128(MAX_SQRT_PRICE, MAX_SQRT_PRICE);
        assert_eq!(
            squared,
            u256_from_str("6276865797213735624986666139454542730126328067628745144281"),
            "MAX_SQRT_PRICE² must be exact at 256 bits"
        );
        assert!(
            MAX_SQRT_PRICE.checked_mul(MAX_SQRT_PRICE).is_none(),
            "the same squaring in a u128 must be the overflow this module routes around"
        );
    }

    /// At the widest price with $BYE as B, `expected_out` leaves u64 — and that is an error
    /// return, not a truncation to a bound the swap would trivially clear.
    #[test]
    fn a_result_that_leaves_u64_is_refused() {
        const MAX_SQRT_PRICE: u128 = 79_226_673_521_066_979_257_578_248_091;
        let err = blank_min_out(
            FACE_5_USDC,
            MAX_SQRT_PRICE,
            PairOrdering::ByeIsB,
            SLIPPAGE_BPS,
        )
        .expect_err("must not narrow silently");
        assert_eq!(err, ByeMachineError::ArithmeticFailure.into());
    }

    #[test]
    fn a_zero_sqrt_price_is_refused_rather_than_dividing_by_zero() {
        let err = blank_min_out(FACE_1_USDC, 0, PairOrdering::ByeIsA, SLIPPAGE_BPS)
            .expect_err("a zero price has no bound");
        assert_eq!(err, ByeMachineError::ArithmeticFailure.into());
    }

    #[test]
    fn a_full_slippage_budget_is_refused_rather_than_priced_at_zero() {
        let err = blank_min_out(
            FACE_1_USDC,
            SQRT_PRICE_BYE_IS_A,
            PairOrdering::ByeIsA,
            10_000,
        )
        .expect_err("a zero bound is no bound");
        assert_eq!(err, ByeMachineError::ArithmeticFailure.into());
    }

    /// Floor at both steps, biased toward completing the swap. A `min_out` that rounded up would
    /// exceed the exact quotient and refuse settlements the budget was set to admit.
    #[test]
    fn rounding_is_floor_at_both_steps() {
        // 3 · 2¹²⁸ / 2¹²⁸ is exact; the haircut is what rounds. 7 · 9_900 / 10_000 = 6.93.
        let seven = U256::from_u128(7);
        let hair = seven
            .checked_mul_u64(9_900)
            .unwrap()
            .div(U256::from_u128(10_000))
            .unwrap();
        assert_eq!(hair.to_u64().unwrap(), 6, "floor, not 7");
    }

    /// Division must agree with the reference at widths the settlement path actually reaches.
    #[test]
    fn division_agrees_with_u128_where_both_are_defined() {
        for (n, d) in [
            (1u128, 1u128),
            (0, 7),
            (u128::MAX, 1),
            (u128::MAX, u128::MAX),
            (u128::MAX, 3),
            (1_000_000, 999_983),
            (58_333_726_687_135_160, 12_345),
        ] {
            let got = U256::from_u128(n).div(U256::from_u128(d)).unwrap().to_u64();
            let want = n / d;
            if let Ok(want_u64) = u64::try_from(want) {
                assert_eq!(got, Some(want_u64), "{n} / {d}");
            }
        }
    }

    #[test]
    fn division_by_zero_is_none_rather_than_a_panic() {
        assert!(U256::from_u128(1).div(U256::ZERO).is_none());
    }

    /// `mul_u128` must be exact across the carry boundaries, where a schoolbook accumulation that
    /// drops a carry still agrees with the reference on small operands.
    #[test]
    fn multiplication_is_exact_across_the_limb_boundaries() {
        for (a, b) in [
            (0u128, 0u128),
            (1, 1),
            (u64::MAX as u128, u64::MAX as u128),
            (1u128 << 64, 1u128 << 64),
            ((1u128 << 64) - 1, (1u128 << 64) + 1),
            (u128::MAX, 2),
        ] {
            let got = U256::mul_u128(a, b);
            // Where the product still fits a u128, the reference is the native multiply.
            if let Some(want) = a.checked_mul(b) {
                assert_eq!(got, U256::from_u128(want), "{a} · {b}");
            }
        }
        assert_eq!(
            U256::mul_u128(u128::MAX, u128::MAX),
            u256_from_str(
                "115792089237316195423570985008687907852589419931798687112530834793049593217025"
            ),
            "(2¹²⁸−1)² must be exact"
        );
    }

    /// `shr_128_to_u128` is the specified divide-by-2¹²⁸ and must discard exactly the low half.
    #[test]
    fn shifting_right_by_128_keeps_the_high_half() {
        let v = U256::mul_u128(1u128 << 100, 1u128 << 100); // 2²⁰⁰
        assert_eq!(v.shr_128_to_u128(), 1u128 << 72);
        assert_eq!(
            U256::from_u128(u128::MAX).shr_128_to_u128(),
            0,
            "a value below 2¹²⁸ floors to zero"
        );
    }

    #[test]
    fn placing_a_value_at_2_to_the_128_round_trips() {
        assert_eq!(
            U256::from_u128_shifted_128(12_345).shr_128_to_u128(),
            12_345,
            "the placement and the shift are inverses"
        );
    }
}
