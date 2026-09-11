use anchor_lang::prelude::*;

use crate::common::constants::WEIGHT_C;
use crate::common::errors::ByeMachineError;
use crate::common::math::weight_of;

/// A blank-denomination target: `$1`/`$2`/`$5` counts respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlankTargets {
    pub b1: u32,
    pub b2: u32,
    pub b5: u32,
}

fn abs_diff(a: u128, b: u128) -> u128 {
    if a >= b {
        a.checked_sub(b).expect("a >= b by the branch condition")
    } else {
        b.checked_sub(a).expect("b > a by the branch condition")
    }
}

/// `|T·(w_real + Σ b_d·w_d) − C·(n_real + Σ b_d)|`, the absolute ticket-price error a
/// candidate blank set would leave.
fn score(
    ticket_target: u128,
    weight_c: u128,
    w_real: u128,
    n_real: u128,
    weights: [u128; 3],
    counts: [u128; 3],
) -> Result<u128> {
    let mut weighted = w_real;
    let mut total_count = n_real;
    for d in 0..3 {
        let contribution = weights[d]
            .checked_mul(counts[d])
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        weighted = weighted
            .checked_add(contribution)
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        total_count = total_count
            .checked_add(counts[d])
            .ok_or(ByeMachineError::ArithmeticFailure)?;
    }
    let lhs = ticket_target
        .checked_mul(weighted)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let rhs = weight_c
        .checked_mul(total_count)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    Ok(abs_diff(lhs, rhs))
}

/// The deterministic O(1) blank-targeting algorithm: a closed-form `$2` anchor plus an
/// 8-candidate scan (`$1`, `$2±1` about the anchor, `$5`, each 0 or 1 beyond the anchor's own
/// count) minimizing the ticket-price error, tie-broken by fewer total blanks, then a lower
/// `$1` count, then a lower `$5` count. `n_real == 0` is the protective no-priced-roll state, and
/// a `w_real` heavy enough that `T·w_real > n_real·C` — which a leaf re-attested below
/// `ticket_target` and retained under a lock produces — yields zero blanks rather than an error.
pub fn evaluate(
    n_real: u32,
    w_real: u128,
    blank_faces: [u64; 3],
    ticket_target: u64,
) -> Result<BlankTargets> {
    if n_real == 0 {
        return Ok(BlankTargets {
            b1: 0,
            b2: 0,
            b5: 0,
        });
    }

    let c = WEIGHT_C;
    let t = u128::from(ticket_target);
    let weights = [
        u128::from(weight_of(blank_faces[0])?),
        u128::from(weight_of(blank_faces[1])?),
        u128::from(weight_of(blank_faces[2])?),
    ];
    let n_real_u = u128::from(n_real);

    let scaled_n = n_real_u
        .checked_mul(c)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let scaled_w = t
        .checked_mul(w_real)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    // `T·w_real > n_real·C` is a **state, not a fault.** Every configured face is strictly below
    // `ticket_target` (`init_pool`, `update_config` — `BlankFaceNotBelowTicket`), so `T·w_d > C`
    // for all three and every blank widens `T·weighted − C·count`; `den2 > 0` below asserts it
    // for `$2` directly. When the bracket is already positive no non-negative blank count solves
    // `C·(n_real + Σb) = T·(w_real + Σb·w_d)`, and the closest candidate is `b1 = b2 = b5 = 0`.
    // Saturating hands the scan `b2* = 0` and leaves `score` the single source of the answer,
    // rather than adding a branch that could disagree with it. A `checked_sub` here would return
    // `ArithmeticFailure` instead, freezing `approve_deposit`, `withdraw` and `record_value`
    // pool-wide over a state `record_value`'s below-floor `Retained` branch can reach.
    let num = scaled_n.saturating_sub(scaled_w);

    let scaled_w2 = t
        .checked_mul(weights[1])
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let den2 = scaled_w2
        .checked_sub(c)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    require!(den2 > 0, ByeMachineError::ArithmeticFailure);
    let b2_star = num
        .checked_div(den2)
        .ok_or(ByeMachineError::ArithmeticFailure)?;

    let mut best: Option<((u128, u128, u128, u128), BlankTargets)> = None;
    for a1 in [0u128, 1] {
        for a5 in [0u128, 1] {
            for delta in [0u128, 1] {
                let b2 = b2_star
                    .checked_add(delta)
                    .ok_or(ByeMachineError::ArithmeticFailure)?;
                let counts = [a1, b2, a5];
                let candidate_score = score(t, c, w_real, n_real_u, weights, counts)?;
                let total_blanks = a1
                    .checked_add(b2)
                    .and_then(|v| v.checked_add(a5))
                    .ok_or(ByeMachineError::ArithmeticFailure)?;
                let key = (candidate_score, total_blanks, a1, a5);
                let candidate = BlankTargets {
                    b1: u32::try_from(a1).map_err(|_| ByeMachineError::ArithmeticFailure)?,
                    b2: u32::try_from(b2).map_err(|_| ByeMachineError::ArithmeticFailure)?,
                    b5: u32::try_from(a5).map_err(|_| ByeMachineError::ArithmeticFailure)?,
                };
                best = Some(match best {
                    None => (key, candidate),
                    Some((best_key, best_candidate)) => {
                        if key < best_key {
                            (key, candidate)
                        } else {
                            (best_key, best_candidate)
                        }
                    }
                });
            }
        }
    }

    Ok(best.expect("the 8-candidate scan always yields a result").1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };

    /// Brute force over exactly the algorithm's own 8-candidate domain — `a1, a5 ∈ {0,1}`,
    /// `b2 ∈ {b2*, b2*+1}` — written as a fresh scan and comparison rather than calling
    /// `evaluate`'s scoring/tie-break code. What this oracle checks independently is whether the
    /// 8 candidates are scored and compared correctly. The design does not claim the 8-candidate
    /// domain is a global optimum over arbitrary blank counts — only that this domain's own
    /// minimum is found deterministically and tie-broken as specified.
    ///
    /// **`b2*` is derived here without a subtraction, and that is the point.** This oracle
    /// used to recompute `n_real·C − T·w_real` in native `u128`, which is the *same* arithmetic
    /// the production code was getting wrong: on a `w_real` heavy enough to make that difference
    /// negative the oracle panicked instead of disagreeing, so it agreed with the defect over
    /// every input that could have exposed it. `b2*` is equivalently **the largest `b2`
    /// satisfying `T·(w_real + b2·w_2) ≤ C·(n_real + b2)`** — two sums and one comparison, no
    /// difference anywhere. The predicate is monotone because `T·w_2 > C` (the `den2 > 0`
    /// production asserts), and `b2 = 0` failing it yields `b2* = 0`, which is the answer in that
    /// regime rather than an error.
    fn brute_force_best(
        n_real: u32,
        w_real: u128,
        blank_faces: [u64; 3],
        ticket_target: u64,
    ) -> BlankTargets {
        let c = WEIGHT_C;
        let t = u128::from(ticket_target);
        let weights = [
            u128::from(weight_of(blank_faces[0]).unwrap()),
            u128::from(weight_of(blank_faces[1]).unwrap()),
            u128::from(weight_of(blank_faces[2]).unwrap()),
        ];
        let n_real_u = u128::from(n_real);

        let mut b2_star = 0u128;
        while t * (w_real + (b2_star + 1) * weights[1]) <= c * (n_real_u + b2_star + 1) {
            b2_star += 1;
            // Monotone, so this terminates on any config with `T·w_2 > C`; loud rather than
            // hanging on one without, since a silent oracle is worse than a failing one.
            assert!(
                b2_star < 100_000,
                "b2* search did not converge — is T·w_2 > C?"
            );
        }

        let mut best: Option<((u128, u128, u128, u128), BlankTargets)> = None;
        for a1 in [0u128, 1] {
            for a5 in [0u128, 1] {
                for delta in [0u128, 1] {
                    let b2 = b2_star + delta;
                    let weighted = w_real + a1 * weights[0] + b2 * weights[1] + a5 * weights[2];
                    let total = n_real_u + a1 + b2 + a5;
                    let lhs = t * weighted;
                    let rhs = c * total;
                    let sc = lhs.abs_diff(rhs);
                    let key = (sc, a1 + b2 + a5, a1, a5);
                    let candidate = BlankTargets {
                        b1: a1 as u32,
                        b2: b2 as u32,
                        b5: a5 as u32,
                    };
                    best = Some(match best {
                        None => (key, candidate),
                        Some((bk, bc)) => {
                            if key < bk {
                                (key, candidate)
                            } else {
                                (bk, bc)
                            }
                        }
                    });
                }
            }
        }
        best.unwrap().1
    }

    fn w_real_for(n_real: u32, per_position_value: u64) -> u128 {
        u128::from(weight_of(per_position_value).unwrap()) * u128::from(n_real)
    }

    // --- protective case -------------------------------------------------------------------

    #[test]
    fn n_real_zero_is_the_protective_state() {
        let out = evaluate(0, 999_999, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET).unwrap();
        assert_eq!(
            out,
            BlankTargets {
                b1: 0,
                b2: 0,
                b5: 0
            }
        );
    }

    // --- determinism -----------------------------------------------------------------------

    #[test]
    fn identical_inputs_produce_identical_outputs() {
        for n_real in [1u32, 7, 20, 63] {
            let w_real = w_real_for(n_real, DEFAULT_ADMISSION_FLOOR);
            let a = evaluate(n_real, w_real, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET).unwrap();
            let b = evaluate(n_real, w_real, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET).unwrap();
            assert_eq!(a, b);
        }
    }

    // --- score-optimality against an independent brute-force oracle -------------------------

    #[test]
    fn matches_brute_force_optimum_over_the_8_candidate_domain() {
        for n_real in [1u32, 2, 5, 12, 30, 50] {
            for per_position_value in [
                DEFAULT_ADMISSION_FLOOR,
                DEFAULT_ADMISSION_FLOOR * 3,
                50_000_000,
                // Below `DEFAULT_TICKET_TARGET` (9M), which is the half of the domain this list
                // used to omit entirely: all three values above are above the floor and therefore
                // above the target, so `n_real·C − T·w_real` was positive for every input the
                // oracle ever saw. See the regime-below-ticket-target test below for why that \
                // regime is reachable.
                8_000_000,
                5_000_000,
                1_000_000,
            ] {
                let w_real = w_real_for(n_real, per_position_value);
                let got =
                    evaluate(n_real, w_real, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET).unwrap();
                let want =
                    brute_force_best(n_real, w_real, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET);
                assert_eq!(
                    got, want,
                    "n_real={n_real} per_position_value={per_position_value}"
                );
            }
        }
    }

    // --- denominations bounded to the configured faces --------------------------------------

    #[test]
    fn result_only_ever_names_the_three_configured_denominations() {
        // Structural: BlankTargets has exactly one count per configured face — there is no
        // fourth field a result could carry a denomination outside `blank_faces` in.
        let out = evaluate(
            4,
            w_real_for(4, DEFAULT_ADMISSION_FLOOR),
            DEFAULT_BLANK_FACES,
            DEFAULT_TICKET_TARGET,
        )
        .unwrap();
        let _: (u32, u32, u32) = (out.b1, out.b2, out.b5);
    }

    // --- tie order against the brute-force oracle, over realistic config --------------------

    #[test]
    fn matches_brute_force_over_further_n_real_values() {
        for n_real in [3u32, 9, 21] {
            let w_real = w_real_for(n_real, DEFAULT_ADMISSION_FLOOR);
            let got = evaluate(n_real, w_real, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET).unwrap();
            let want = brute_force_best(n_real, w_real, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET);
            assert_eq!(got, want, "n_real={n_real}");
        }
    }

    // --- tie order, hand-verified — equal blank_faces make every denomination cost
    // the same, so three of the 8 candidates score identically; only the tie-break decides ---

    #[test]
    fn tie_break_prefers_fewer_total_blanks_then_lower_b1_then_lower_b5() {
        // face[0] == face[1] == face[2] = $2 makes any single extra blank, of any denomination,
        // shift the score identically. With n_real=5 and this w_real the three one-blank
        // candidates — (b1=1,b2=0,b5=0), (b1=0,b2=1,b5=0), (b1=0,b2=0,b5=1) — tie for the
        // minimum score (hand-computed: 874_999_999_992_000_000, each), beating both the
        // zero-blank and two-blank tiers. Lower b1 eliminates the first; lower b5 then prefers
        // the middle one over the third.
        let blank_faces = [2_000_000u64, 2_000_000, 2_000_000];
        let n_real = 5u32;
        let w_real = 263_888_888_888u128;
        let got = evaluate(n_real, w_real, blank_faces, DEFAULT_TICKET_TARGET).unwrap();
        assert_eq!(
            got,
            BlankTargets {
                b1: 0,
                b2: 1,
                b5: 0
            }
        );
    }

    #[test]
    fn tie_break_ranks_lower_b1_above_lower_b5() {
        // The equal-faces fixture above cannot see this: the candidates it leaves tied differ
        // only in b5, so swapping the b1/b5 priority yields the same winner. Here face[0] ==
        // face[2] != face[1], so (b1=1,b2=0,b5=0) and (b1=0,b2=0,b5=1) carry identical weight
        // and identical total counts, tie on score, and are separated by nothing but the
        // priority itself. Lower b1 must win.
        let blank_faces = [3_000_000u64, 2_000_000, 3_000_000];
        let n_real = 4u32;
        let w_real = 4 * u128::from(weight_of(12_000_000).unwrap());
        let got = evaluate(n_real, w_real, blank_faces, DEFAULT_TICKET_TARGET).unwrap();
        assert_eq!(
            got,
            BlankTargets {
                b1: 0,
                b2: 0,
                b5: 1
            }
        );
    }

    // --- the regime below `ticket_target`, which `record_value` can reach -------------------

    /// `w_real` for a pool of `n_real` positions where **one** sits at `retained_value` and the
    /// rest at the admission floor. This is the shape `record_value`'s below-floor `Retained`
    /// branch leaves behind: a locked leaf re-attested under the floor keeps its Fenwick slot at
    /// the new weight, and no other path in the crate moves a leaf's weight after admission. So
    /// the admission floor bounds per-leaf weight *at admission only*, and `w_real` can carry a
    /// leaf far heavier than any floor leaf.
    fn w_real_with_one_retained(n_real: u32, retained_value: u64) -> u128 {
        assert!(n_real >= 1, "the retained position is one of the n_real");
        u128::from(weight_of(retained_value).unwrap())
            + w_real_for(n_real - 1, DEFAULT_ADMISSION_FLOOR)
    }

    /// `n_real·C − T·w_real` in `i128`. Signed and in its own width on purpose: naming which
    /// regime an input is in must not go through the `u128` difference the production code
    /// computes, or the test inherits whatever that gets wrong.
    fn num_signed(n_real: u32, w_real: u128) -> i128 {
        let scaled_n = i128::try_from(u128::from(n_real) * WEIGHT_C).unwrap();
        let scaled_w = i128::try_from(u128::from(DEFAULT_TICKET_TARGET) * w_real).unwrap();
        scaled_n - scaled_w
    }

    /// `den2 = T·w_2 − C`, the per-`$2`-blank step. Once `num` reaches it the closed form puts
    /// `b2* ≥ 1`, which is the discriminating half of the sweep below.
    fn den2() -> i128 {
        let scaled_w2 = u128::from(DEFAULT_TICKET_TARGET)
            * u128::from(weight_of(DEFAULT_BLANK_FACES[1]).unwrap());
        i128::try_from(scaled_w2 - WEIGHT_C).unwrap()
    }

    #[test]
    fn a_leaf_below_the_ticket_target_yields_zero_blanks_rather_than_an_error() {
        // `T·w_real > n_real·C` is a state, not a fault: `den2 > 0` is asserted and every
        // configured face is below `ticket_target`, so every blank only widens
        // `T·weighted − C·count` and no non-negative blank count solves the equation. The
        // closest candidate is `(0,0,0)` — which a `checked_sub` on `num` turned into 6703,
        // freezing `approve_deposit`, `withdraw` and `record_value` pool-wide.
        let zeros = BlankTargets {
            b1: 0,
            b2: 0,
            b5: 0,
        };
        for retained_value in [8_000_000u64, 5_000_000, 2_000_000, 1_000_000] {
            for n_real in 1u32..=64 {
                let w_real = w_real_with_one_retained(n_real, retained_value);
                let got = evaluate(n_real, w_real, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET)
                    .unwrap_or_else(|e| {
                        panic!("retained={retained_value} n_real={n_real} errored: {e:?}")
                    });
                let want =
                    brute_force_best(n_real, w_real, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET);
                assert_eq!(got, want, "retained={retained_value} n_real={n_real}");

                let num = num_signed(n_real, w_real);
                if num < 0 {
                    assert_eq!(
                        got, zeros,
                        "retained={retained_value} n_real={n_real} num={num}"
                    );
                } else if num >= den2() {
                    // The discriminating half. Without it the case above passes on a function
                    // that returns `(0,0,0)` for every input, which is exactly what a fix that
                    // short-circuited ahead of the scan would produce.
                    assert!(
                        got.b2 >= 1,
                        "retained={retained_value} n_real={n_real} num={num}: {got:?}"
                    );
                }
            }
        }
    }

    // --- where the zero-blank regime ends, measured on both sides ---------------------------

    #[test]
    fn the_zero_blank_regime_ends_where_the_headroom_runs_out() {
        // Each floor leaf adds `C − T·w(12M)` of headroom to `num` and the retained leaf
        // subtracts `T·w(V) − C`, so the boundary is where those cross. The largest `n_real`
        // still in the zero-blank regime, for one retained leaf at each value:
        //
        //   $8 → 1     $5 → 4     $2 → 14     $1 → 32
        //
        // Measured against this function, not extrapolated — and the doubling that 1/4/8/16
        // suggests is not what the arithmetic does, which is why both sides of every boundary
        // are asserted here rather than the pattern being trusted.
        let zeros = BlankTargets {
            b1: 0,
            b2: 0,
            b5: 0,
        };
        for (retained_value, last_zero_n) in [
            (8_000_000u64, 1u32),
            (5_000_000, 4),
            (2_000_000, 14),
            (1_000_000, 32),
        ] {
            let w_at = w_real_with_one_retained(last_zero_n, retained_value);
            assert!(
                num_signed(last_zero_n, w_at) < 0,
                "retained={retained_value}: n_real={last_zero_n} is not in the zero-blank regime"
            );
            assert_eq!(
                evaluate(
                    last_zero_n,
                    w_at,
                    DEFAULT_BLANK_FACES,
                    DEFAULT_TICKET_TARGET
                )
                .unwrap(),
                zeros,
                "retained={retained_value} n_real={last_zero_n}"
            );

            let after = last_zero_n + 1;
            let w_after = w_real_with_one_retained(after, retained_value);
            assert!(
                num_signed(after, w_after) >= 0,
                "retained={retained_value}: n_real={after} is still in the zero-blank regime, so \
                 the boundary above is not the boundary"
            );
            let got = evaluate(after, w_after, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET).unwrap();
            assert_eq!(
                got,
                brute_force_best(after, w_after, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET),
                "retained={retained_value} n_real={after}"
            );
        }
    }
}
