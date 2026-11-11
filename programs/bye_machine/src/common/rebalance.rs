use anchor_lang::prelude::*;

use crate::common::constants::{ABOVE_TICKET_SLOT, BLANK_SLOTS, WEIGHT_C};
use crate::common::errors::ByeMachineError;
use crate::common::math::weight_of;

/// The below-ticket target: counts for slots 0, 1 and 2 (`$1`/`$2`/`$5` on the launch faces).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlankTargets {
    b1: u32,
    b2: u32,
    b5: u32,
}

/// The blank counts for every slot, so that a roll's expected payout is as close to
/// `ticket_target` as whole counts allow. With `H = C·n_real / w_real` the harmonic mean of the
/// real cards' values:
///
/// - `H > T`: the below-ticket search over slots 0–2, unchanged; slot 3 is 0.
/// - `H ≤ T` and slot 3 holds a face: slots 0–2 are 0 and slot 3 takes
///   [`above_ticket_count`].
/// - `H ≤ T` and slot 3 is 0: the below-ticket search, which saturates to all zeros there.
///
/// Slots 4–7 are always 0. `n_real == 0` is the protective no-priced-roll state.
pub fn evaluate(
    n_real: u32,
    w_real: u128,
    blank_faces: [u64; BLANK_SLOTS],
    ticket_target: u64,
) -> Result<[u32; BLANK_SLOTS]> {
    let mut counts = [0u32; BLANK_SLOTS];
    if n_real == 0 {
        return Ok(counts);
    }

    let above_face = blank_faces[ABOVE_TICKET_SLOT];
    if above_face != 0 && !harmonic_mean_above_ticket(n_real, w_real, ticket_target)? {
        counts[ABOVE_TICKET_SLOT] = above_ticket_count(n_real, w_real, above_face, ticket_target)?;
        return Ok(counts);
    }

    let below = evaluate_below_ticket(
        n_real,
        w_real,
        [blank_faces[0], blank_faces[1], blank_faces[2]],
        ticket_target,
    )?;
    counts[0] = below.b1;
    counts[1] = below.b2;
    counts[2] = below.b5;
    Ok(counts)
}

/// `H > T`, compared as `C·n_real > T·w_real` so no division rounds the boundary.
fn harmonic_mean_above_ticket(n_real: u32, w_real: u128, ticket_target: u64) -> Result<bool> {
    let scaled_n = u128::from(n_real)
        .checked_mul(WEIGHT_C)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let scaled_w = u128::from(ticket_target)
        .checked_mul(w_real)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    Ok(scaled_n > scaled_w)
}

/// The whole count `x` of above-ticket blanks minimising `|T·(w_real + x·w) − C·(n_real + x)|`,
/// the same score the below-ticket search minimises; a tie goes to the smaller count.
///
/// With `H ≤ T` the bracket starts at `d = T·w_real − C·n_real ≥ 0`, and each blank lowers it by
/// `step = C − T·w`, positive because the face is above the ticket. The score is `|d − x·step|`,
/// convex in `x`, so the minimiser is `⌊d / step⌋` or one more. A face at or below the ticket that
/// slipped past validation fails rather than producing a step of the wrong sign.
///
/// **The count saturates at `u32::MAX` rather than failing.** Every writer of `pool.blanks` runs
/// this, withdrawals and settlements included, so an error here would freeze the pool's exits.
/// A saturated count stays inside every later checked operation: a slot-3 weight is at most
/// `C / (T + 1 USDC)` ≤ 10¹², so `u32::MAX` blanks weigh under 2⁷³ in `aggregate_weight`, and
/// counts are summed as `u64` in `split_fee` and multiplied by `C` in `u128`.
fn above_ticket_count(n_real: u32, w_real: u128, face: u64, ticket_target: u64) -> Result<u32> {
    // Stated on the faces, not left to the step's sign: at a face equal to the ticket the floored
    // weight still leaves a tiny positive step, which would saturate instead of failing.
    require!(face > ticket_target, ByeMachineError::ArithmeticFailure);
    let t = u128::from(ticket_target);
    let d = t
        .checked_mul(w_real)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_sub(
            u128::from(n_real)
                .checked_mul(WEIGHT_C)
                .ok_or(ByeMachineError::ArithmeticFailure)?,
        )
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let step = WEIGHT_C
        .checked_sub(
            t.checked_mul(u128::from(weight_of(face)?))
                .ok_or(ByeMachineError::ArithmeticFailure)?,
        )
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    require!(step > 0, ByeMachineError::ArithmeticFailure);

    let floor = d
        .checked_div(step)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let below_gap = d
        .checked_rem(step)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let above_gap = step
        .checked_sub(below_gap)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    let count = if above_gap < below_gap {
        floor
            .checked_add(1)
            .ok_or(ByeMachineError::ArithmeticFailure)?
    } else {
        floor
    };
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
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

/// `den_d = T·w_d − C`, the amount one blank of denomination `d` moves `T·weighted − C·count`.
///
/// Positive for every face in slots 0–2, because `init_pool` and `update_config` both reject one
/// at or above `ticket_target` (`BlankFaceNotBelowTicket`) and `T·w_d > C` is exactly
/// `V_d < T`. The `checked_sub` is what asserts it: a face that slipped past validation yields
/// `ArithmeticFailure` rather than a wrapped step that would make every blank look free.
fn den(ticket_target: u128, weight_c: u128, weight_d: u128) -> Result<u128> {
    let scaled = ticket_target
        .checked_mul(weight_d)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    Ok(scaled
        .checked_sub(weight_c)
        .ok_or(ByeMachineError::ArithmeticFailure)?)
}

/// Widest `$1` count the scan considers. The published algorithm fixes the *structure* (closed
/// form + bounded scan + total order), not the width.
const A1_MAX: u128 = 2;

/// Widest `$5` count the scan considers. See [`A1_MAX`].
///
/// `(A1_MAX, A5_MAX) = (2, 8)` is the measured knee. Over a randomised domain of realistic pools
/// the mean `|H − T|/T` falls from 5.53% at `(1,1)` to 1.17% at `(1,4)` and 0.90% at `(2,8)`;
/// `(4,16)` reaches 0.88% for 3× the candidates. The residual is intrinsic — at `n_real = 3` a
/// `$5` blank is a coarse lump against three leaves and no blank count lands nearer.
const A5_MAX: u128 = 8;

/// The deterministic O(1) blank-targeting algorithm: for each `(b1, b5)` in
/// `0..=A1_MAX × 0..=A5_MAX`, a closed-form `$2` anchor re-solved against the residual that pair
/// leaves, scanned at the anchor and one above it — `(A1_MAX + 1)·(A5_MAX + 1)·2` candidates —
/// minimizing the ticket-price error, tie-broken by fewer total blanks, then a lower `$1` count,
/// then a lower `$5` count. `n_real == 0` is the protective no-priced-roll state, and a `w_real`
/// heavy enough that `T·w_real > n_real·C` — which a `ticket_target` raised past a value already
/// in the tree produces — yields zero blanks rather than an error.
///
/// **Re-anchoring is what makes the two knobs agree.** `b2*` derived once from `num` alone is
/// the anchor for a scan that has not yet spent anything; a candidate that then adds a `$1` or
/// `$5` blank is measured against an anchor solving a residual it no longer leaves, so the
/// counts fight each other. Solving `b2*` per `(b1, b5)` brackets the true optimum for that
/// pair, which is also why a `$1` blank is reachable at all: against `num` alone any `b1 = 1`
/// candidate scores at least `den1 − den2`, strictly worse than `(0, b2*, 0)`'s residual, and
/// `b1 > 0` could never win.
fn evaluate_below_ticket(
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
    // `T·w_real > n_real·C` is a **state, not a fault.** Every face in slots 0–2 is strictly below
    // `ticket_target` (`BlankFaceNotBelowTicket`), so every such blank widens `T·weighted − C·count`;
    // when the bracket is already positive no non-negative blank count solves it and the closest
    // candidate is `b1 = b2 = b5 = 0`. Saturating hands the scan `b2* = 0` and leaves `score`
    // the single source of the answer. A `checked_sub` would return `ArithmeticFailure` and
    // freeze `approve_deposit`, `withdraw` and `record_value` pool-wide. The state is reachable
    // because `update_config`'s `Pricing` arm raises `ticket_target` without checking it against
    // leaf values already in the tree, so every leaf attested under the old target can become
    // sub-target at once.
    let num = scaled_n.saturating_sub(scaled_w);

    let dens = [
        den(t, c, weights[0])?,
        den(t, c, weights[1])?,
        den(t, c, weights[2])?,
    ];
    let den2 = dens[1];
    require!(den2 > 0, ByeMachineError::ArithmeticFailure);

    let mut best: Option<((u128, u128, u128, u128), BlankTargets)> = None;
    for a1 in 0..=A1_MAX {
        for a5 in 0..=A5_MAX {
            // What this `(b1, b5)` pair already spends of `num`, and the residual the `$2`
            // anchor is then solved against. Computed once per pair — it does not depend on
            // `delta` — which is also the compute budget: `A1_MAX`·`A5_MAX` divisions, not one
            // per candidate.
            let spent = dens[0]
                .checked_mul(a1)
                .and_then(|from_b1| {
                    dens[2]
                        .checked_mul(a5)
                        .and_then(|from_b5| from_b1.checked_add(from_b5))
                })
                .ok_or(ByeMachineError::ArithmeticFailure)?;
            // Saturating for the reason `num` is: a pair that overspends the bracket leaves no
            // room for a `$2` blank, so the anchor is 0.
            let residual = num.saturating_sub(spent);
            let b2_star = residual
                .checked_div(den2)
                .ok_or(ByeMachineError::ArithmeticFailure)?;
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

    Ok(best
        .expect("the candidate scan is non-empty, so it always yields a result")
        .1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };

    /// The launch pool's below-ticket faces, the three the below-ticket search takes.
    const BELOW_FACES: [u64; 3] = [
        DEFAULT_BLANK_FACES[0],
        DEFAULT_BLANK_FACES[1],
        DEFAULT_BLANK_FACES[2],
    ];

    /// Independent brute force over the algorithm's own candidate domain — `a1 ∈ 0..=A1_MAX`,
    /// `a5 ∈ 0..=A5_MAX`, `b2 ∈ {b2*(a1,a5), b2*(a1,a5)+1}` — with its own scoring and
    /// tie-break, not `evaluate`'s. It does not claim a global optimum over arbitrary blank
    /// counts, only this domain's minimum.
    ///
    /// `b2*` is derived without a subtraction: it is the largest `b2` satisfying
    /// `T·(w_real + b2·w_2) ≤ C·(n_real + b2)`. The predicate is monotone because `T·w_2 > C`,
    /// and `b2 = 0` failing it yields `b2* = 0` rather than an error.
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

        // `b2*(a1, a5)` — the anchor re-solved against the residual this `(a1, a5)` pair leaves,
        // still by the subtraction-free predicate: the largest `b2` with
        // `T·(w_real + a1·w_1 + b2·w_2 + a5·w_5) ≤ C·(n_real + a1 + b2 + a5)`. Re-anchoring adds
        // the pair's own contribution to both sides; it does not add a difference.
        let b2_star_for = |a1: u128, a5: u128| -> u128 {
            let anchored_w = w_real + a1 * weights[0] + a5 * weights[2];
            let anchored_n = n_real_u + a1 + a5;
            let mut b2_star = 0u128;
            while t * (anchored_w + (b2_star + 1) * weights[1]) <= c * (anchored_n + b2_star + 1) {
                b2_star += 1;
                // Monotone, so this terminates on any config with `T·w_2 > C`; loud rather than
                // hanging on one without, since a silent oracle is worse than a failing one.
                assert!(
                    b2_star < 100_000,
                    "b2* search did not converge — is T·w_2 > C?"
                );
            }
            b2_star
        };

        let mut best: Option<((u128, u128, u128, u128), BlankTargets)> = None;
        for a1 in 0..=A1_MAX {
            for a5 in 0..=A5_MAX {
                let b2_star = b2_star_for(a1, a5);
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
        let out = evaluate_below_ticket(0, 999_999, BELOW_FACES, DEFAULT_TICKET_TARGET).unwrap();
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
            let a =
                evaluate_below_ticket(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET).unwrap();
            let b =
                evaluate_below_ticket(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET).unwrap();
            assert_eq!(a, b);
        }
    }

    // --- score-optimality against an independent brute-force oracle -------------------------

    #[test]
    fn matches_brute_force_optimum_over_the_scanned_candidate_domain() {
        for n_real in [1u32, 2, 5, 12, 30, 50] {
            for per_position_value in [
                DEFAULT_ADMISSION_FLOOR,
                DEFAULT_ADMISSION_FLOOR * 3,
                50_000_000,
                // Below `DEFAULT_TICKET_TARGET` (9M): the regime a raised target reaches.
                8_000_000,
                5_000_000,
                1_000_000,
            ] {
                let w_real = w_real_for(n_real, per_position_value);
                let got = evaluate_below_ticket(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET)
                    .unwrap();
                let want = brute_force_best(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET);
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
        let out = evaluate_below_ticket(
            4,
            w_real_for(4, DEFAULT_ADMISSION_FLOOR),
            BELOW_FACES,
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
            let got =
                evaluate_below_ticket(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET).unwrap();
            let want = brute_force_best(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET);
            assert_eq!(got, want, "n_real={n_real}");
        }
    }

    // --- tie order, hand-verified — equal blank_faces make every denomination cost
    // the same, so three scanned candidates score identically; only the tie-break decides ---

    #[test]
    fn tie_break_prefers_fewer_total_blanks_then_lower_b1_then_lower_b5() {
        // face[0] == face[1] == face[2] = $2 makes any single extra blank, of any denomination,
        // shift the score identically. With n_real=5 and this w_real the three one-blank
        // candidates — (b1=1,b2=0,b5=0), (b1=0,b2=1,b5=0), (b1=0,b2=0,b5=1) — tie for the
        // minimum score (874_999_999_992_000_000 each), beating the zero-blank and two-blank
        // tiers. Lower b1 eliminates the first; lower b5 then prefers the middle over the third.
        let blank_faces = [2_000_000u64, 2_000_000, 2_000_000];
        let n_real = 5u32;
        let w_real = 263_888_888_888u128;
        let got =
            evaluate_below_ticket(n_real, w_real, blank_faces, DEFAULT_TICKET_TARGET).unwrap();
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
        // face[0] == face[2] != face[1], so (b1=1,b2=0,b5=0) and (b1=0,b2=0,b5=1) carry identical
        // weight and total count, tie on score (999_999_999_985_000_000), and are separated by
        // nothing but the priority itself. Lower b1 must win.
        let blank_faces = [3_000_000u64, 2_000_000, 3_000_000];
        let n_real = 4u32;
        let w_real = 4 * u128::from(weight_of(12_000_000).unwrap());
        let got =
            evaluate_below_ticket(n_real, w_real, blank_faces, DEFAULT_TICKET_TARGET).unwrap();
        assert_eq!(
            got,
            BlankTargets {
                b1: 0,
                b2: 0,
                b5: 1
            }
        );
    }

    // --- the regime below `ticket_target`, which a raised target reaches --------------------

    /// `w_real` for a pool of `n_real` positions where **one** sits at `sub_target_value` and
    /// the rest at the admission floor — a mix, not a uniform pool, because the mix is what the
    /// crate actually produces and it is the harder input: the sub-target leaf drags `T·w_real`
    /// past `n_real·C` while every other leaf pulls the other way.
    ///
    /// Reached by raising `ticket_target` past a value already in the tree. A leaf's own weight
    /// never moves into the regime: `weight_of` is `C/V`, fixed at admission, and `record_value`
    /// only re-homes it to another at-or-above-floor value.
    fn w_real_with_one_sub_target_leaf(n_real: u32, sub_target_value: u64) -> u128 {
        assert!(n_real >= 1, "the sub-target position is one of the n_real");
        u128::from(weight_of(sub_target_value).unwrap())
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
        let scaled_w2 =
            u128::from(DEFAULT_TICKET_TARGET) * u128::from(weight_of(BELOW_FACES[1]).unwrap());
        i128::try_from(scaled_w2 - WEIGHT_C).unwrap()
    }

    #[test]
    fn a_leaf_below_the_ticket_target_yields_zero_blanks_rather_than_an_error() {
        // `T·w_real > n_real·C` is a state, not a fault: every blank only widens
        // `T·weighted − C·count`, so the closest candidate is `(0,0,0)`. A `checked_sub` on
        // `num` would instead return `ArithmeticFailure` and freeze the pool.
        let zeros = BlankTargets {
            b1: 0,
            b2: 0,
            b5: 0,
        };
        for sub_target_value in [8_000_000u64, 5_000_000, 2_000_000, 1_000_000] {
            for n_real in 1u32..=64 {
                let w_real = w_real_with_one_sub_target_leaf(n_real, sub_target_value);
                let got = evaluate_below_ticket(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET)
                    .unwrap_or_else(|e| {
                        panic!("sub_target={sub_target_value} n_real={n_real} errored: {e:?}")
                    });
                let want = brute_force_best(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET);
                assert_eq!(got, want, "sub_target={sub_target_value} n_real={n_real}");

                let num = num_signed(n_real, w_real);
                if num < 0 {
                    assert_eq!(
                        got, zeros,
                        "sub_target={sub_target_value} n_real={n_real} num={num}"
                    );
                } else if num >= den2() {
                    // Some blank must be minted, but not necessarily a `$2` one: `den5` is a
                    // finer step than `den2`, so the scan legitimately prefers `$5` on much of
                    // this sweep.
                    assert!(
                        got.b1 + got.b2 + got.b5 >= 1,
                        "sub_target={sub_target_value} n_real={n_real} num={num}: {got:?}"
                    );
                }
            }
        }
    }

    // --- where the zero-blank regime ends, measured on both sides ---------------------------

    #[test]
    fn the_zero_blank_regime_ends_where_the_headroom_runs_out() {
        // Each floor leaf adds `C − T·w(12M)` of headroom to `num` and the sub-target leaf
        // subtracts `T·w(V) − C`, so the boundary is where those cross. The largest `n_real`
        // still in the zero-blank regime, for one sub-target leaf at each value:
        //
        //   $8 → 1     $5 → 4     $2 → 14     $1 → 32
        //
        // Measured, not extrapolated: both sides of every boundary are asserted.
        let zeros = BlankTargets {
            b1: 0,
            b2: 0,
            b5: 0,
        };
        for (sub_target_value, last_zero_n) in [
            (8_000_000u64, 1u32),
            (5_000_000, 4),
            (2_000_000, 14),
            (1_000_000, 32),
        ] {
            let w_at = w_real_with_one_sub_target_leaf(last_zero_n, sub_target_value);
            assert!(
                num_signed(last_zero_n, w_at) < 0,
                "sub_target={sub_target_value}: n_real={last_zero_n} is not in the zero-blank regime"
            );
            assert_eq!(
                evaluate_below_ticket(last_zero_n, w_at, BELOW_FACES, DEFAULT_TICKET_TARGET)
                    .unwrap(),
                zeros,
                "sub_target={sub_target_value} n_real={last_zero_n}"
            );

            let after = last_zero_n + 1;
            let w_after = w_real_with_one_sub_target_leaf(after, sub_target_value);
            assert!(
                num_signed(after, w_after) >= 0,
                "sub_target={sub_target_value}: n_real={after} is still in the zero-blank regime, so \
                 the boundary above is not the boundary"
            );
            let got =
                evaluate_below_ticket(after, w_after, BELOW_FACES, DEFAULT_TICKET_TARGET).unwrap();
            assert_eq!(
                got,
                brute_force_best(after, w_after, BELOW_FACES, DEFAULT_TICKET_TARGET),
                "sub_target={sub_target_value} n_real={after}"
            );
        }
    }
    // --- the widened domain, measured against the one it replaced ---------------------------

    /// `|T·weighted − C·total|` for a blank set — the quantity the scan minimizes, recomputed
    /// here from the counts rather than by calling `score`, so a defect in `score` cannot hide
    /// inside the comparison it is being judged by.
    fn error_abs(n_real: u32, w_real: u128, targets: BlankTargets) -> u128 {
        let (weighted, total) = weighted_and_total(n_real, w_real, targets);
        (u128::from(DEFAULT_TICKET_TARGET) * weighted).abs_diff(WEIGHT_C * total)
    }

    /// `|H − T| / T` in parts per million, where `H = C·total / weighted` is the harmonic mean
    /// the ticket is priced against.
    ///
    /// Exact in integers: `|C·total/weighted − T| / T` is `|C·total − T·weighted| / (T·weighted)`,
    /// so no float enters. Note the denominator — this is **not** the scan's own objective
    /// divided by a constant. `error_abs` is `weighted · |T − H|`, weight-weighted, so a
    /// candidate can score lower and still price slightly further from the target. Which of the
    /// two the algorithm ought to minimize is a separate question from this one.
    fn price_error_ppm(n_real: u32, w_real: u128, targets: BlankTargets) -> u128 {
        let (weighted, total) = weighted_and_total(n_real, w_real, targets);
        let t = u128::from(DEFAULT_TICKET_TARGET);
        (WEIGHT_C * total).abs_diff(t * weighted) * 1_000_000 / (t * weighted)
    }

    fn weighted_and_total(n_real: u32, w_real: u128, targets: BlankTargets) -> (u128, u128) {
        let counts = [
            u128::from(targets.b1),
            u128::from(targets.b2),
            u128::from(targets.b5),
        ];
        let mut weighted = w_real;
        let mut total = u128::from(n_real);
        for d in 0..3 {
            weighted += u128::from(weight_of(BELOW_FACES[d]).unwrap()) * counts[d];
            total += counts[d];
        }
        (weighted, total)
    }

    /// A frozen baseline scan: `b1, b5 ∈ {0,1}` and `b2 ∈ {b2*, b2*+1}` about a `b2*` solved
    /// once from `num` alone, before the scan spends anything.
    fn unanchored_eight_candidate_evaluate(n_real: u32, w_real: u128) -> BlankTargets {
        let c = WEIGHT_C;
        let t = u128::from(DEFAULT_TICKET_TARGET);
        let weights = [
            u128::from(weight_of(BELOW_FACES[0]).unwrap()),
            u128::from(weight_of(BELOW_FACES[1]).unwrap()),
            u128::from(weight_of(BELOW_FACES[2]).unwrap()),
        ];
        let n = u128::from(n_real);
        let num = (n * c).saturating_sub(t * w_real);
        let b2_star = num / (t * weights[1] - c);

        let mut best: Option<((u128, u128, u128, u128), BlankTargets)> = None;
        for a1 in [0u128, 1] {
            for a5 in [0u128, 1] {
                for delta in [0u128, 1] {
                    let b2 = b2_star + delta;
                    let weighted = w_real + a1 * weights[0] + b2 * weights[1] + a5 * weights[2];
                    let total = n + a1 + b2 + a5;
                    let key = ((t * weighted).abs_diff(c * total), a1 + b2 + a5, a1, a5);
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

    /// A fixed pool generator: `n_real` in `1..=300`, each position at a whole-dollar value in
    /// `$12..=$300` — at or above the admission floor, which is the only shape the tree holds.
    /// Deterministic, so a failure is reproducible from the seed alone.
    struct Pools(u64);

    impl Pools {
        fn next_u31(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn next_pool(&mut self) -> (u32, u128) {
            let n_real = 1 + (self.next_u31() % 300) as u32;
            let mut w_real = 0u128;
            for _ in 0..n_real {
                let value = (12 + self.next_u31() % 289) * 1_000_000;
                w_real += u128::from(weight_of(value).unwrap());
            }
            (n_real, w_real)
        }
    }

    const POOL_SEED: u64 = 0x5EED_1234_5678_9ABC;
    const POOL_COUNT: usize = 2_000;

    #[test]
    fn a_one_dollar_blank_is_reachable() {
        // A `$1` blank cannot win an unanchored scan: with `b2*` fixed against `num` alone the
        // residual is bounded by `den2`, so every `b1 = 1` candidate scores worse than
        // `(0, b2*, 0)`. 32 positions at the $12 admission floor is the smallest uniform pool
        // where a `$1` blank is the exact optimum: `(1,0,0)` prices the ticket at the target to
        // within a part per million.
        let n_real = 32u32;
        let w_real = w_real_for(n_real, DEFAULT_ADMISSION_FLOOR);

        let got =
            evaluate_below_ticket(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET).unwrap();
        assert!(got.b1 > 0, "a $1 blank must be reachable, got {got:?}");
        assert_eq!(
            got,
            BlankTargets {
                b1: 1,
                b2: 0,
                b5: 0
            }
        );
        assert_eq!(price_error_ppm(n_real, w_real, got), 0);

        // The unanchored baseline picks differently on the same pool.
        assert_eq!(
            unanchored_eight_candidate_evaluate(n_real, w_real),
            BlankTargets {
                b1: 0,
                b2: 2,
                b5: 1
            }
        );

        // Not a property of one fixture: a `$1` blank is chosen across the randomised domain.
        let mut pools = Pools(POOL_SEED);
        let mut with_b1 = 0usize;
        for _ in 0..POOL_COUNT {
            let (n, w) = pools.next_pool();
            if evaluate_below_ticket(n, w, BELOW_FACES, DEFAULT_TICKET_TARGET)
                .unwrap()
                .b1
                > 0
            {
                with_b1 += 1;
            }
        }
        assert_eq!(
            with_b1, 1_313,
            "$1 blanks are chosen on a fixed fraction of the generated pools; a change here \
             means the scan moved, not that the generator did"
        );
    }

    #[test]
    fn the_scan_never_scores_worse_than_the_domain_it_replaced() {
        // Exact, no tolerance: for each `(b1, b5)` the re-anchored pair
        // `{b2*(b1,b5), b2*(b1,b5)+1}` brackets the minimizer of `|b2·den2 − residual|` over all
        // non-negative `b2`, so the scan never scores worse than the unanchored baseline.
        let mut pools = Pools(POOL_SEED);
        for i in 0..POOL_COUNT {
            let (n_real, w_real) = pools.next_pool();
            let got =
                evaluate_below_ticket(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET).unwrap();
            let before = unanchored_eight_candidate_evaluate(n_real, w_real);
            assert!(
                error_abs(n_real, w_real, got) <= error_abs(n_real, w_real, before),
                "pool {i} (n_real={n_real}): {got:?} scores worse than {before:?}"
            );
        }
    }

    #[test]
    fn the_ticket_price_error_is_bounded_above_twenty_positions() {
        // One percent. The worst shape found at `n_real ≥ 20` (20 positions uniformly at $13)
        // prices 8_714 ppm from the target; the worst over the randomised domain is 4_899 ppm.
        // Both are asserted exactly so the margin cannot erode silently.
        //
        // Below 20 positions the residual is intrinsic: a blank is a coarse lump against few
        // leaves, and no scan width fixes that.
        const BOUND_PPM: u128 = 10_000;

        let worst_shape = (20u32, w_real_for(20, 13_000_000));
        assert_eq!(
            price_error_ppm(
                worst_shape.0,
                worst_shape.1,
                evaluate_below_ticket(
                    worst_shape.0,
                    worst_shape.1,
                    BELOW_FACES,
                    DEFAULT_TICKET_TARGET
                )
                .unwrap()
            ),
            8_714
        );

        let mut pools = Pools(POOL_SEED);
        let mut worst = 0u128;
        for i in 0..POOL_COUNT {
            let (n_real, w_real) = pools.next_pool();
            if n_real < 20 {
                continue;
            }
            let got =
                evaluate_below_ticket(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET).unwrap();
            let ppm = price_error_ppm(n_real, w_real, got);
            assert!(
                ppm < BOUND_PPM,
                "pool {i} (n_real={n_real}): {got:?} prices {ppm} ppm from the target"
            );
            worst = worst.max(ppm);
        }
        assert_eq!(worst, 4_899);
    }

    #[test]
    fn every_measured_pool_shape_prices_nearer_the_target_than_before() {
        // Uniform-$12 pools at the admission floor. Price error feeds the exit percentage a
        // position's expected fee accrual, `equal_total · V / H`, actually delivers.
        for (n_real, before_ppm, after_ppm) in [
            (5u32, 81_081u128, 47_619u128),
            (10, 83_333, 7_751),
            (20, 32_863, 4_329),
            (50, 22_727, 1_692),
            (100, 2_770, 0),
        ] {
            let w_real = w_real_for(n_real, DEFAULT_ADMISSION_FLOOR);
            let got =
                evaluate_below_ticket(n_real, w_real, BELOW_FACES, DEFAULT_TICKET_TARGET).unwrap();
            let before = unanchored_eight_candidate_evaluate(n_real, w_real);
            assert_eq!(
                price_error_ppm(n_real, w_real, before),
                before_ppm,
                "n_real={n_real}: the measurement this change was justified by has moved"
            );
            assert_eq!(
                price_error_ppm(n_real, w_real, got),
                after_ppm,
                "n_real={n_real}"
            );
            assert!(after_ppm < before_ppm);
        }
    }

    // --- eight slots: Bronze and Silver unchanged, Gold's three cases -----------------------

    /// The three-slot controller, frozen verbatim (comments dropped) so a later edit to
    /// the below-ticket search cannot move Bronze's or Silver's blanks without failing here.
    mod v1_1_2 {
        use crate::common::constants::WEIGHT_C;
        use crate::common::errors::ByeMachineError;
        use crate::common::math::weight_of;
        use anchor_lang::prelude::*;

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct BlankTargets {
            pub b1: u32,
            pub b2: u32,
            pub b5: u32,
        }

        fn abs_diff(a: u128, b: u128) -> u128 {
            a.abs_diff(b)
        }

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

        fn den(ticket_target: u128, weight_c: u128, weight_d: u128) -> Result<u128> {
            let scaled = ticket_target
                .checked_mul(weight_d)
                .ok_or(ByeMachineError::ArithmeticFailure)?;
            Ok(scaled
                .checked_sub(weight_c)
                .ok_or(ByeMachineError::ArithmeticFailure)?)
        }

        const A1_MAX: u128 = 2;

        const A5_MAX: u128 = 8;

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
            let num = scaled_n.saturating_sub(scaled_w);

            let dens = [
                den(t, c, weights[0])?,
                den(t, c, weights[1])?,
                den(t, c, weights[2])?,
            ];
            let den2 = dens[1];
            require!(den2 > 0, ByeMachineError::ArithmeticFailure);

            let mut best: Option<((u128, u128, u128, u128), BlankTargets)> = None;
            for a1 in 0..=A1_MAX {
                for a5 in 0..=A5_MAX {
                    let spent = dens[0]
                        .checked_mul(a1)
                        .and_then(|from_b1| {
                            dens[2]
                                .checked_mul(a5)
                                .and_then(|from_b5| from_b1.checked_add(from_b5))
                        })
                        .ok_or(ByeMachineError::ArithmeticFailure)?;
                    let residual = num.saturating_sub(spent);
                    let b2_star = residual
                        .checked_div(den2)
                        .ok_or(ByeMachineError::ArithmeticFailure)?;
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
                            b1: u32::try_from(a1)
                                .map_err(|_| ByeMachineError::ArithmeticFailure)?,
                            b2: u32::try_from(b2)
                                .map_err(|_| ByeMachineError::ArithmeticFailure)?,
                            b5: u32::try_from(a5)
                                .map_err(|_| ByeMachineError::ArithmeticFailure)?,
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

            Ok(best
                .expect("the candidate scan is non-empty, so it always yields a result")
                .1)
        }
    }

    const SILVER_TICKET: u64 = 18_000_000;
    const SILVER_FACES: [u64; BLANK_SLOTS] = [2_000_000, 4_000_000, 10_000_000, 0, 0, 0, 0, 0];
    const GOLD_TICKET: u64 = 45_000_000;
    const GOLD_FACES: [u64; BLANK_SLOTS] =
        [5_000_000, 10_000_000, 25_000_000, 100_000_000, 0, 0, 0, 0];

    fn widen(targets: v1_1_2::BlankTargets) -> [u32; BLANK_SLOTS] {
        [targets.b1, targets.b2, targets.b5, 0, 0, 0, 0, 0]
    }

    fn w_real_of(values: &[u64]) -> u128 {
        values
            .iter()
            .map(|v| u128::from(weight_of(*v).unwrap()))
            .sum()
    }

    /// Random pools whose cards sit inside an admission window, so each pool stays the shape
    /// that pool's tree can hold.
    fn random_pool(pools: &mut Pools, floor_usdc: u64, ceiling_usdc: u64) -> (u32, u128) {
        let n_real = 1 + (pools.next_u31() % 300) as u32;
        let mut w_real = 0u128;
        for _ in 0..n_real {
            let value =
                (floor_usdc + pools.next_u31() % (ceiling_usdc - floor_usdc + 1)) * 1_000_000;
            w_real += u128::from(weight_of(value).unwrap());
        }
        (n_real, w_real)
    }

    #[test]
    fn bronze_and_silver_blanks_equal_v1_1_2_on_the_same_pools() {
        let bronze_below = [
            DEFAULT_BLANK_FACES[0],
            DEFAULT_BLANK_FACES[1],
            DEFAULT_BLANK_FACES[2],
        ];
        let silver_below = [SILVER_FACES[0], SILVER_FACES[1], SILVER_FACES[2]];
        let mut pools = Pools(POOL_SEED);
        let mut with_blanks = 0usize;
        for i in 0..POOL_COUNT {
            let (n, w) = random_pool(&mut pools, 10, 500);
            let got = evaluate(n, w, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET).unwrap();
            let want = widen(v1_1_2::evaluate(n, w, bronze_below, DEFAULT_TICKET_TARGET).unwrap());
            assert_eq!(got, want, "bronze pool {i} (n_real={n})");
            with_blanks += usize::from(got != [0; 8]);

            let (n, w) = random_pool(&mut pools, 20, 1_000);
            let got = evaluate(n, w, SILVER_FACES, SILVER_TICKET).unwrap();
            let want = widen(v1_1_2::evaluate(n, w, silver_below, SILVER_TICKET).unwrap());
            assert_eq!(got, want, "silver pool {i} (n_real={n})");
            with_blanks += usize::from(got != [0; 8]);
        }
        assert!(
            with_blanks > POOL_COUNT,
            "the comparison is vacuous unless most pools carry blanks"
        );

        // A ticket raised past cards already in the tree takes Bronze below its ticket; with no
        // above-ticket face it stays at zero blanks, as the three-slot controller did.
        for n_real in 1u32..=64 {
            let w = w_real_with_one_sub_target_leaf(n_real, 1_000_000);
            assert_eq!(
                evaluate(n_real, w, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET).unwrap(),
                widen(v1_1_2::evaluate(n_real, w, bronze_below, DEFAULT_TICKET_TARGET).unwrap()),
                "n_real={n_real}"
            );
        }

        // The one-dollar pin, read through the eight-slot entry point.
        assert_eq!(
            evaluate(
                32,
                w_real_for(32, DEFAULT_ADMISSION_FLOOR),
                DEFAULT_BLANK_FACES,
                DEFAULT_TICKET_TARGET
            )
            .unwrap(),
            [1, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn no_pool_is_ever_given_a_blank_in_a_reserved_slot() {
        let mut pools = Pools(POOL_SEED);
        for _ in 0..POOL_COUNT {
            let (n, w) = random_pool(&mut pools, 30, 2_500);
            let got = evaluate(n, w, GOLD_FACES, GOLD_TICKET).unwrap();
            assert_eq!(got[4..], [0; 4]);
            assert!(
                got[3] == 0 || got[..3] == [0; 3],
                "above- and below-ticket blanks are never mixed: {got:?}"
            );
        }
    }

    #[test]
    fn gold_above_its_ticket_sets_below_ticket_blanks_only() {
        // Ten cards at 100 USDC: H = 100 > 45. Hand-computed by exhaustive search over every
        // count of the 5 / 10 / 25 USDC faces up to 40 each: seven 25 USDC blanks.
        let w_real = w_real_of(&[100_000_000; 10]);
        assert_eq!(
            evaluate(10, w_real, GOLD_FACES, GOLD_TICKET).unwrap(),
            [0, 0, 7, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn gold_exactly_at_its_ticket_sets_no_blanks() {
        // Five cards at 50 and four at 40 USDC: their weights divide `C` exactly, and
        // 9 / (5/50 + 4/40) = 45, so `T·w_real == C·n_real` with no rounding.
        let w_real = w_real_of(&[
            50_000_000, 50_000_000, 50_000_000, 50_000_000, 50_000_000, 40_000_000, 40_000_000,
            40_000_000, 40_000_000,
        ]);
        assert_eq!(u128::from(GOLD_TICKET) * w_real, WEIGHT_C * 9);
        assert_eq!(
            evaluate(9, w_real, GOLD_FACES, GOLD_TICKET).unwrap(),
            [0; 8]
        );
    }

    /// `|T·(w_real + x·w₃) − C·(n_real + x)|`, from the definition, for the oracle below.
    fn above_ticket_score(n_real: u32, w_real: u128, x: u128) -> u128 {
        let w3 = u128::from(weight_of(GOLD_FACES[3]).unwrap());
        (u128::from(GOLD_TICKET) * (w_real + x * w3)).abs_diff(WEIGHT_C * (u128::from(n_real) + x))
    }

    #[test]
    fn gold_below_its_ticket_sets_the_100_usdc_blank_only() {
        // Seven cards at 30 and three at 500 USDC: H = 10 / (7/30 + 3/500) ≈ 41.78 USDC, so
        // q = (45 − H) / (100 − H) ≈ 5.53% of rolls should draw the 100 USDC blank. The whole
        // count nearest that is 1 (the real-valued count is 1.40); scores hand-computed:
        // x=0 → 7.6999e17, x=1 → 2.1999e17, x=2 → 3.3000e17.
        let values = [
            30_000_000,
            30_000_000,
            30_000_000,
            30_000_000,
            30_000_000,
            30_000_000,
            30_000_000,
            500_000_000,
            500_000_000,
            500_000_000,
        ];
        let w_real = w_real_of(&values);
        let got = evaluate(10, w_real, GOLD_FACES, GOLD_TICKET).unwrap();
        assert_eq!(got, [0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(above_ticket_score(10, w_real, 0), 769_999_999_895_000_000);
        assert_eq!(above_ticket_score(10, w_real, 1), 219_999_999_895_000_000);
        assert_eq!(above_ticket_score(10, w_real, 2), 330_000_000_105_000_000);

        // Within one blank of q: q's real-valued count is x = q·W / ((1 − q)·w₃).
        let h = WEIGHT_C as f64 * 10.0 / w_real as f64;
        let q = (GOLD_TICKET as f64 - h) / (GOLD_FACES[3] as f64 - h);
        assert!((q - 0.0553).abs() < 0.0001, "q = {q}");
        let w3 = weight_of(GOLD_FACES[3]).unwrap() as f64;
        let ideal = q * w_real as f64 / ((1.0 - q) * w3);
        assert!((f64::from(got[3]) - ideal).abs() < 1.0, "ideal {ideal}");
    }

    #[test]
    fn the_above_ticket_count_beats_one_fewer_and_ties_with_one_more_at_most() {
        let mut pools = Pools(POOL_SEED);
        let mut set = 0usize;
        for i in 0..POOL_COUNT {
            // Cards of 30 to 55 USDC, so most pools average below the ticket.
            let (n, w) = random_pool(&mut pools, 30, 55);
            if u128::from(GOLD_TICKET) * w < WEIGHT_C * u128::from(n) {
                continue;
            }
            let x = u128::from(evaluate(n, w, GOLD_FACES, GOLD_TICKET).unwrap()[3]);
            set += usize::from(x > 0);
            let at = above_ticket_score(n, w, x);
            assert!(
                at <= above_ticket_score(n, w, x + 1),
                "pool {i}: one more is nearer"
            );
            if x > 0 {
                assert!(
                    at < above_ticket_score(n, w, x - 1),
                    "pool {i}: one fewer is as near, and ties go to fewer blanks"
                );
            }
        }
        assert!(
            set > POOL_COUNT / 2,
            "only {set} pools set an above-ticket blank"
        );
    }

    #[test]
    fn a_tie_goes_to_fewer_above_ticket_blanks() {
        // One card at 25 USDC, a 30 USDC ticket and a 50 USDC face, all dividing `C` exactly:
        // d = 30·C/25 − C = C/5 and each blank lowers the bracket by C − 30·C/50 = 2C/5, so zero
        // blanks and one blank both leave |C/5|.
        let faces = [1_000_000, 2_000_000, 5_000_000, 50_000_000, 0, 0, 0, 0];
        let ticket = 30_000_000u64;
        let w_real = w_real_of(&[25_000_000]);
        let w3 = u128::from(weight_of(faces[3]).unwrap());
        let d = u128::from(ticket) * w_real - WEIGHT_C;
        let step = WEIGHT_C - u128::from(ticket) * w3;
        assert_eq!(
            (d, step),
            (WEIGHT_C / 5, 2 * WEIGHT_C / 5),
            "the fixture is an exact tie"
        );
        assert_eq!(evaluate(1, w_real, faces, ticket).unwrap(), [0; 8]);

        // One more card at 25 USDC doubles `d` to a whole step: exactly one blank.
        let w_real = w_real_of(&[25_000_000, 25_000_000]);
        assert_eq!(
            evaluate(2, w_real, faces, ticket).unwrap(),
            [0, 0, 0, 1, 0, 0, 0, 0]
        );
    }

    /// A 100 USDC face over a 99 USDC ticket against a card of 1 micro-USDC wants about 9.9·10⁹
    /// blanks — past `u32`. The count saturates instead of failing, and every operation the pool
    /// runs on its counts afterwards still succeeds.
    #[test]
    fn an_above_ticket_count_past_u32_saturates_and_stays_usable() {
        let faces = [1_000_000, 2_000_000, 5_000_000, 100_000_000, 0, 0, 0, 0];
        let ticket = 99_000_000u64;
        let w_real = u128::from(weight_of(1).unwrap());
        let counts = evaluate(1, w_real, faces, ticket).unwrap();
        assert_eq!(counts, [0, 0, 0, u32::MAX, 0, 0, 0, 0]);

        let w = crate::common::math::aggregate_weight(w_real, counts, faces).unwrap();
        crate::common::math::ticket_quote(1 + u64::from(u32::MAX), w).unwrap();
        crate::common::settlement_math::split_fee(
            9_900_000,
            ticket,
            [7_500, 500, 2_000],
            1,
            counts,
            faces,
            0,
        )
        .unwrap();
        crate::common::expansion::map_draw(w - 1, w_real, counts, faces).unwrap();
    }

    #[test]
    fn an_above_ticket_face_at_or_below_the_ticket_fails_rather_than_wrapping() {
        let w_real = w_real_of(&[30_000_000; 4]);
        for face in [GOLD_TICKET, GOLD_TICKET - 1] {
            let mut faces = GOLD_FACES;
            faces[3] = face;
            assert_eq!(
                evaluate(4, w_real, faces, GOLD_TICKET)
                    .map(|_| ())
                    .unwrap_err(),
                ByeMachineError::ArithmeticFailure.into()
            );
        }
    }
}
