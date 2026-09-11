use anchor_lang::prelude::*;

use crate::common::constants::FENWICK_CAPACITY;
use crate::common::errors::ByeMachineError;
use crate::common::math::weight_of;

/// One pool's real-position weight index: a Fenwick range-sum tree over `FENWICK_CAPACITY`
/// leaves with a LIFO free-slot stack. Not a PDA — a pre-created keypair account pinned by
/// `Pool.weight_index`.
#[account(zero_copy)]
#[derive(InitSpace)]
pub struct WeightIndex {
    pub pool: Pubkey,
    pub total_weight: u64,
    pub high_water: u32,
    pub free_count: u32,
    pub _padding: [u8; 8],
    pub tree: [u64; FENWICK_CAPACITY],
    pub free_stack: [u32; FENWICK_CAPACITY],
}

fn lowbit(i: usize) -> usize {
    i & i.wrapping_neg()
}

fn fenwick_add(tree: &mut [u64], mut i: usize, delta: u64) {
    let capacity = tree.len();
    while i <= capacity {
        tree[i - 1] = tree[i - 1]
            .checked_add(delta)
            .expect("fenwick node exceeds u64 range");
        i += lowbit(i);
    }
}

fn fenwick_sub(tree: &mut [u64], mut i: usize, delta: u64) {
    let capacity = tree.len();
    while i <= capacity {
        tree[i - 1] = tree[i - 1]
            .checked_sub(delta)
            .expect("fenwick node underflows below zero");
        i += lowbit(i);
    }
}

/// Sum of leaves `1..=i`; `i == 0` is the empty prefix.
fn fenwick_prefix(tree: &[u64], mut i: usize) -> u64 {
    let mut sum = 0u64;
    while i > 0 {
        sum = sum
            .checked_add(tree[i - 1])
            .expect("fenwick prefix exceeds u64 range");
        i -= lowbit(i);
    }
    sum
}

/// Leaf `i`'s own weight. The tree stores prefix sums rather than per-leaf weights, so reading
/// one slot back costs a second prefix walk. Caller must have bounds-checked `i`.
fn fenwick_leaf(tree: &[u64], i: usize) -> u64 {
    fenwick_prefix(tree, i)
        .checked_sub(fenwick_prefix(tree, i - 1))
        .expect("fenwick prefix is not monotonic")
}

fn fenwick_find(tree: &[u64], mut r: u64) -> usize {
    let capacity = tree.len();
    let mut pos = 0usize;
    let mut step = if capacity == 0 {
        0
    } else {
        1usize << capacity.ilog2()
    };
    while step > 0 {
        let next = pos + step;
        if next <= capacity && tree[next - 1] <= r {
            pos = next;
            r -= tree[next - 1];
        }
        step >>= 1;
    }
    pos
}

/// Hands back a slot whose leaf is empty — recycled first, else the next unallocated one. The
/// emptiness is asserted rather than imposed: an allocator that zeroed a dirty leaf would repair
/// the tree while leaving `total_weight` counting the residual, so a slot arriving with weight on
/// it is reported as `WeightDesync` instead.
fn slot_alloc(
    tree: &[u64],
    free_stack: &mut [u32],
    high_water: &mut u32,
    free_count: &mut u32,
) -> Result<u32> {
    let capacity = tree.len();
    let slot = if *free_count > 0 {
        *free_count -= 1;
        free_stack[*free_count as usize]
    } else {
        require!(
            (*high_water as usize) < capacity,
            ByeMachineError::WeightIndexFull
        );
        let slot = *high_water;
        *high_water += 1;
        slot
    };
    require!((slot as usize) < capacity, ByeMachineError::WeightDesync);
    require_eq!(
        fenwick_leaf(tree, slot as usize + 1),
        0u64,
        ByeMachineError::WeightDesync
    );
    Ok(slot)
}

/// The bound is the module's mirror of `slot_alloc`'s `high_water < capacity`, and it reuses
/// that guard's own code: `free_count` reaching the stack's length is the same array-capacity
/// condition, reached from the other end. Unreachable while `slot_alloc`'s zero-leaf assert
/// holds — a release can only follow an allocation — so this is defence in depth on the
/// module's last unguarded index rather than a live path.
fn slot_release(free_stack: &mut [u32], free_count: &mut u32, slot: u32) -> Result<()> {
    require!(
        (*free_count as usize) < free_stack.len(),
        ByeMachineError::WeightIndexFull
    );
    free_stack[*free_count as usize] = slot;
    *free_count += 1;
    Ok(())
}

fn ops_insert(
    tree: &mut [u64],
    free_stack: &mut [u32],
    total_weight: &mut u64,
    high_water: &mut u32,
    free_count: &mut u32,
    weight: u64,
    expected_total: u64,
) -> Result<u32> {
    let slot = slot_alloc(tree, free_stack, high_water, free_count)?;
    *total_weight = total_weight
        .checked_add(weight)
        .ok_or(ByeMachineError::WeightDesync)?;
    fenwick_add(tree, slot as usize + 1, weight);
    require_eq!(*total_weight, expected_total, ByeMachineError::WeightDesync);
    Ok(slot)
}

fn ops_remove(
    tree: &mut [u64],
    free_stack: &mut [u32],
    total_weight: &mut u64,
    free_count: &mut u32,
    slot: u32,
    weight: u64,
    expected_total: u64,
) -> Result<()> {
    let leaf_index = slot as usize + 1;
    require!(leaf_index <= tree.len(), ByeMachineError::WeightDesync);
    require_eq!(
        fenwick_leaf(tree, leaf_index),
        weight,
        ByeMachineError::WeightDesync
    );
    *total_weight = total_weight
        .checked_sub(weight)
        .ok_or(ByeMachineError::WeightDesync)?;
    fenwick_sub(tree, slot as usize + 1, weight);
    slot_release(free_stack, free_count, slot)?;
    require_eq!(*total_weight, expected_total, ByeMachineError::WeightDesync);
    Ok(())
}

fn ops_update(
    tree: &mut [u64],
    total_weight: &mut u64,
    slot: u32,
    old_weight: u64,
    new_weight: u64,
    expected_total: u64,
) -> Result<()> {
    let leaf_index = slot as usize + 1;
    require!(leaf_index <= tree.len(), ByeMachineError::WeightDesync);
    require_eq!(
        fenwick_leaf(tree, leaf_index),
        old_weight,
        ByeMachineError::WeightDesync
    );
    match new_weight.checked_sub(old_weight) {
        Some(delta) => {
            *total_weight = total_weight
                .checked_add(delta)
                .ok_or(ByeMachineError::WeightDesync)?;
            fenwick_add(tree, leaf_index, delta);
        }
        None => {
            let delta = old_weight - new_weight;
            *total_weight = total_weight
                .checked_sub(delta)
                .ok_or(ByeMachineError::WeightDesync)?;
            fenwick_sub(tree, leaf_index, delta);
        }
    }
    require_eq!(*total_weight, expected_total, ByeMachineError::WeightDesync);
    Ok(())
}

fn ops_prefix_search(tree: &[u64], total_weight: u64, r: u64) -> Option<u32> {
    if total_weight == 0 || r >= total_weight {
        return None;
    }
    Some(fenwick_find(tree, r) as u32)
}

impl WeightIndex {
    /// Narrows `Pool.w_real` (`u128`) to the tree's own `u64` aggregate. A value that does not
    /// fit `u64` cannot have been produced by a synced tree, so it is reported as `WeightDesync`
    /// rather than silently truncated.
    pub fn narrow_expected_total(w_real: u128) -> Result<u64> {
        u64::try_from(w_real).map_err(|_| ByeMachineError::WeightDesync.into())
    }

    /// Allocates a slot (recycled first, else `high_water`), adds `value`'s weight at it, and
    /// asserts the resulting aggregate equals `expected_total` (the caller's already-updated
    /// `w_real`). The leaf weight is derived here from `math::weight_of`, so a caller cannot
    /// supply one that disagrees with the value it recorded — it never supplies one at all.
    /// The allocated slot is required to hold weight zero, so a recycled slot can never
    /// accumulate onto a residual.
    pub fn insert(&mut self, value: u64, expected_total: u64) -> Result<u32> {
        let weight = weight_of(value)?;
        ops_insert(
            &mut self.tree,
            &mut self.free_stack,
            &mut self.total_weight,
            &mut self.high_water,
            &mut self.free_count,
            weight,
            expected_total,
        )
    }

    /// Subtracts `value`'s weight from `slot`, recycles it onto the free stack, and asserts the
    /// resulting aggregate equals `expected_total`. `value` must be the position's
    /// `recorded_value`, re-derived through the same `math::weight_of` its insertion used.
    ///
    /// A `value` that does not weigh what the leaf holds is rejected in *both* directions: the
    /// derived weight is checked against the leaf's own weight, read back out of the tree, and a
    /// mismatch fails closed with `WeightDesync` before anything is mutated. A released slot
    /// therefore always carries weight zero, which `insert` re-asserts when it recycles one.
    pub fn remove(&mut self, slot: u32, value: u64, expected_total: u64) -> Result<()> {
        let weight = weight_of(value)?;
        ops_remove(
            &mut self.tree,
            &mut self.free_stack,
            &mut self.total_weight,
            &mut self.free_count,
            slot,
            weight,
            expected_total,
        )
    }

    /// Moves `slot`'s leaf from `old_value`'s weight to `new_value`'s, in place: same slot, no
    /// allocation, no release, no touch of `free_stack`/`free_count`/`high_water`. This is the
    /// refresh a live position's changed attestation takes; a below-floor close still goes
    /// through `remove`.
    ///
    /// Both weights are derived here from `math::weight_of`, so neither can be supplied
    /// pre-computed. The leaf is read back out of the tree and checked against `old_value`'s
    /// weight before anything is written, the way `remove` verifies it, and a mismatch fails
    /// closed with `WeightDesync` in either direction. The resulting aggregate is then asserted
    /// against `expected_total`, covering `new_value` weighing more or less than `old_value` —
    /// weight is inversely related to value, so neither direction can be assumed. A no-op
    /// (`old_value == new_value`) runs this same path rather than a special case.
    pub fn update(
        &mut self,
        slot: u32,
        old_value: u64,
        new_value: u64,
        expected_total: u64,
    ) -> Result<()> {
        let old_weight = weight_of(old_value)?;
        let new_weight = weight_of(new_value)?;
        ops_update(
            &mut self.tree,
            &mut self.total_weight,
            slot,
            old_weight,
            new_weight,
            expected_total,
        )
    }

    /// Half-open range search: the occupied slot whose cumulative interval contains `r`.
    /// `None` for an empty tree or `r >= total_weight`.
    pub fn prefix_search(&self, r: u64) -> Option<u32> {
        ops_prefix_search(&self.tree, self.total_weight, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{DEFAULT_ADMISSION_FLOOR, WEIGHT_C};

    fn boxed_zeroed() -> Box<WeightIndex> {
        unsafe {
            let layout = std::alloc::Layout::new::<WeightIndex>();
            let ptr = std::alloc::alloc_zeroed(layout).cast::<WeightIndex>();
            assert!(!ptr.is_null(), "allocation failed");
            Box::from_raw(ptr)
        }
    }

    /// Naive ground truth: raw per-slot weights, summed/scanned directly. Independent of
    /// `fenwick_add`/`fenwick_sub`/`fenwick_find` on purpose — this is what the tree is checked
    /// against, not another copy of the same algorithm.
    #[derive(Default)]
    struct Oracle {
        leaves: Vec<u64>,
    }

    impl Oracle {
        fn new(capacity: usize) -> Self {
            Self {
                leaves: vec![0u64; capacity],
            }
        }

        fn total(&self) -> u64 {
            self.leaves.iter().sum()
        }

        fn prefix_search(&self, r: u64) -> Option<usize> {
            let total = self.total();
            if total == 0 || r >= total {
                return None;
            }
            let mut cum = 0u64;
            for (i, &w) in self.leaves.iter().enumerate() {
                if r < cum + w {
                    return Some(i);
                }
                cum += w;
            }
            None
        }
    }

    /// Σ of the tree's own leaves, walked back out one at a time. This is what `total_weight`
    /// cannot see: the aggregate is a separate scalar, so a leaf carrying weight nobody counted
    /// leaves it perfectly agreeing with the caller. Written independently of `fenwick_prefix`
    /// on purpose — a fault planted in the production reader must not be able to hide behind it.
    fn sum_of_leaves(tree: &[u64]) -> u64 {
        fn prefix(tree: &[u64], mut i: usize) -> u64 {
            let mut sum = 0u64;
            while i > 0 {
                sum += tree[i - 1];
                i &= i - 1;
            }
            sum
        }
        (1..=tree.len())
            .map(|i| prefix(tree, i) - prefix(tree, i - 1))
            .sum()
    }

    /// Thin harness at a small, exhaustively-searchable capacity, wrapping the exact same
    /// `ops_*`/`fenwick_*` functions the real `WeightIndex` methods call — so a bug planted in
    /// those functions is caught here, not masked by a second hand-written implementation.
    struct SmallIndex {
        tree: Vec<u64>,
        free_stack: Vec<u32>,
        total_weight: u64,
        high_water: u32,
        free_count: u32,
    }

    impl SmallIndex {
        fn new(capacity: usize) -> Self {
            Self {
                tree: vec![0u64; capacity],
                free_stack: vec![0u32; capacity],
                total_weight: 0,
                high_water: 0,
                free_count: 0,
            }
        }

        fn insert(&mut self, weight: u64, expected_total: u64) -> Result<u32> {
            ops_insert(
                &mut self.tree,
                &mut self.free_stack,
                &mut self.total_weight,
                &mut self.high_water,
                &mut self.free_count,
                weight,
                expected_total,
            )
        }

        fn remove(&mut self, slot: u32, weight: u64, expected_total: u64) -> Result<()> {
            ops_remove(
                &mut self.tree,
                &mut self.free_stack,
                &mut self.total_weight,
                &mut self.free_count,
                slot,
                weight,
                expected_total,
            )
        }

        fn update(
            &mut self,
            slot: u32,
            old_weight: u64,
            new_weight: u64,
            expected_total: u64,
        ) -> Result<()> {
            ops_update(
                &mut self.tree,
                &mut self.total_weight,
                slot,
                old_weight,
                new_weight,
                expected_total,
            )
        }

        fn prefix_search(&self, r: u64) -> Option<u32> {
            ops_prefix_search(&self.tree, self.total_weight, r)
        }
    }

    /// Deterministic xorshift64 — no external RNG dependency for a seeded, reproducible sequence.
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

    // --- exhaustive small-tree oracle ---------------------------------------------------------

    #[test]
    fn exhaustive_insert_orders_capacity_4() {
        const CAP: usize = 4;
        let weights = [11u64, 23, 7, 31];
        let mut order = [0usize, 1, 2, 3];

        fn permute(arr: &mut [usize], k: usize, out: &mut Vec<Vec<usize>>) {
            if k == arr.len() {
                out.push(arr.to_vec());
                return;
            }
            for i in k..arr.len() {
                arr.swap(k, i);
                permute(arr, k + 1, out);
                arr.swap(k, i);
            }
        }

        let mut orders = Vec::new();
        permute(&mut order, 0, &mut orders);
        assert_eq!(orders.len(), 24);

        for insert_order in &orders {
            for remove_order in &orders {
                let mut sut = SmallIndex::new(CAP);
                let mut oracle = Oracle::new(CAP);
                let mut total = 0u64;

                for &leaf in insert_order {
                    total += weights[leaf];
                    let slot = sut.insert(weights[leaf], total).unwrap();
                    oracle.leaves[slot as usize] = weights[leaf];

                    assert_eq!(sut.total_weight, oracle.total());
                    for r in 0..oracle.total() {
                        assert_eq!(
                            sut.prefix_search(r).map(|s| s as usize),
                            oracle.prefix_search(r)
                        );
                    }
                }

                // `weights` are pairwise distinct, so each leaf's current slot is found by
                // matching its known weight in the oracle.
                for &leaf in remove_order {
                    let w = weights[leaf];
                    let slot = oracle
                        .leaves
                        .iter()
                        .position(|&x| x == w)
                        .expect("leaf still present") as u32;
                    total -= w;
                    sut.remove(slot, w, total).unwrap();
                    oracle.leaves[slot as usize] = 0;

                    assert_eq!(sut.total_weight, oracle.total());
                    if oracle.total() > 0 {
                        for r in 0..oracle.total() {
                            assert_eq!(
                                sut.prefix_search(r).map(|s| s as usize),
                                oracle.prefix_search(r)
                            );
                        }
                    } else {
                        assert_eq!(sut.prefix_search(0), None);
                    }
                }
                assert_eq!(sut.total_weight, 0);
                assert_eq!(sut.free_count as usize, CAP);
            }
        }
    }

    // --- randomized op sequences ---------------------------------------------------------------

    #[test]
    fn randomized_insert_remove_recycle_prefix_search() {
        const CAP: usize = 64;
        for seed in [1u64, 2, 42, 1_000_003, 0xdead_beef] {
            let mut rng = Xorshift64(seed | 1);
            let mut sut = SmallIndex::new(CAP);
            let mut oracle = Oracle::new(CAP);
            let mut total = 0u64;
            let mut occupied: Vec<u32> = Vec::new();

            for _ in 0..5_000 {
                let action = if occupied.is_empty() {
                    0
                } else if occupied.len() >= CAP {
                    1 + rng.below(2)
                } else {
                    rng.below(3)
                };

                if action == 0 {
                    let weight = 1 + rng.below(1_000_000);
                    let free_before = sut.free_count;
                    let high_water_before = sut.high_water;
                    total += weight;
                    let slot = sut.insert(weight, total).unwrap();
                    oracle.leaves[slot as usize] = weight;
                    occupied.push(slot);

                    if free_before > 0 {
                        assert_eq!(sut.free_count, free_before - 1);
                    } else {
                        assert_eq!(sut.high_water, high_water_before + 1);
                    }
                } else if action == 1 {
                    let idx = rng.below(occupied.len() as u64) as usize;
                    let slot = occupied[idx];
                    let old_weight = oracle.leaves[slot as usize];
                    let new_weight = 1 + rng.below(1_000_000);
                    let free_before = sut.free_count;
                    let high_water_before = sut.high_water;
                    total = total - old_weight + new_weight;
                    sut.update(slot, old_weight, new_weight, total).unwrap();
                    oracle.leaves[slot as usize] = new_weight;

                    assert_eq!(
                        sut.free_count, free_before,
                        "update must not release a slot"
                    );
                    assert_eq!(
                        sut.high_water, high_water_before,
                        "update must not allocate a slot"
                    );
                } else {
                    let idx = rng.below(occupied.len() as u64) as usize;
                    let slot = occupied.swap_remove(idx);
                    let weight = oracle.leaves[slot as usize];
                    total -= weight;
                    sut.remove(slot, weight, total).unwrap();
                    oracle.leaves[slot as usize] = 0;
                }

                assert_eq!(sut.total_weight, oracle.total());
                assert_eq!(sut.total_weight, total);
                assert_eq!(sut.total_weight, sum_of_leaves(&sut.tree));

                if !occupied.is_empty() {
                    let r = rng.below(sut.total_weight);
                    assert_eq!(
                        sut.prefix_search(r).map(|s| s as usize),
                        oracle.prefix_search(r)
                    );
                }
            }
        }
    }

    #[test]
    fn recycled_slot_reused_before_high_water_grows() {
        let mut sut = SmallIndex::new(4);
        let s0 = sut.insert(10, 10).unwrap();
        let _s1 = sut.insert(20, 30).unwrap();
        assert_eq!(sut.high_water, 2);

        sut.remove(s0, 10, 20).unwrap();
        assert_eq!(sut.free_count, 1);

        let s2 = sut.insert(30, 50).unwrap();
        assert_eq!(s2, s0, "freed slot must be reused before high_water grows");
        assert_eq!(
            sut.high_water, 2,
            "high_water must not grow while a free slot exists"
        );
    }

    // --- desync error fires ----------------------------------------------------------------------

    fn error_code_number(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    /// The module's last unguarded index. Driven directly, because the bound is
    /// unreachable through `insert`/`remove` — a release can only follow an allocation, so
    /// `free_count` cannot outrun the stack while `slot_alloc`'s zero-leaf assert holds. Both
    /// sides of the boundary are pinned, so the guard cannot be loosened to `<=` either.
    #[test]
    fn slot_release_rejects_a_free_stack_at_capacity() {
        let mut free_stack = [0u32; 2];
        let mut free_count = 2;
        let err = slot_release(&mut free_stack, &mut free_count, 7).unwrap_err();
        assert_eq!(error_code_number(err), 6701);
        assert_eq!(
            free_count, 2,
            "a rejected release must not advance the count"
        );
    }

    #[test]
    fn slot_release_accepts_the_last_free_stack_slot() {
        let mut free_stack = [0u32; 2];
        let mut free_count = 1;
        slot_release(&mut free_stack, &mut free_count, 7).unwrap();
        assert_eq!(free_count, 2);
        assert_eq!(free_stack[1], 7);
    }

    #[test]
    fn insert_reports_weight_desync_on_mismatched_expected_total() {
        let mut sut = SmallIndex::new(4);
        let err = sut.insert(10, 999).unwrap_err();
        assert_eq!(error_code_number(err), 6700);
    }

    #[test]
    fn remove_reports_weight_desync_on_mismatched_expected_total() {
        let mut sut = SmallIndex::new(4);
        let slot = sut.insert(10, 10).unwrap();
        let err = sut.remove(slot, 10, 999).unwrap_err();
        assert_eq!(error_code_number(err), 6700);
    }

    /// The leaf weight is not the caller's to choose. `insert`/`remove` take the value and
    /// derive it through `math::weight_of`, so a weight disagreeing with the recorded value has
    /// no parameter left to travel through.
    #[test]
    fn insert_and_remove_move_the_tree_by_the_derived_weight() {
        let value = DEFAULT_ADMISSION_FLOOR;
        let weight = weight_of(value).unwrap();

        let mut sut = boxed_zeroed();
        let slot = sut.insert(value, weight).unwrap();
        assert_eq!(sut.total_weight, weight);

        sut.remove(slot, value, 0).unwrap();
        assert_eq!(sut.total_weight, 0);
    }

    /// The divergence that is still expressible: a caller aggregate that drifted from the tree.
    /// The desync guard catches it in both directions.
    #[test]
    fn an_aggregate_that_drifted_from_the_tree_is_rejected() {
        let value = DEFAULT_ADMISSION_FLOOR;
        let weight = weight_of(value).unwrap();

        // Ahead of the tree: the caller counted more than the leaf `insert` actually added.
        let mut sut = boxed_zeroed();
        sut.insert(value, weight).unwrap();
        assert_eq!(
            error_code_number(sut.insert(value, 2 * weight + 1).unwrap_err()),
            6700
        );

        // Behind the tree: the caller counted less than the leaf `remove` actually subtracted.
        let mut sut = boxed_zeroed();
        let slot = sut.insert(value, weight).unwrap();
        assert_eq!(
            error_code_number(sut.remove(slot, value, 1).unwrap_err()),
            6700
        );
    }

    /// The fault as measured: `insert` at the floor, `remove` at twice the value — half the
    /// weight. It used to succeed, both aggregates landing on 41,666,666,667 in agreement while
    /// the freed slot kept the difference and `prefix_search` kept returning it.
    #[test]
    fn remove_rejects_a_value_that_does_not_weigh_what_the_leaf_holds() {
        let value = DEFAULT_ADMISSION_FLOOR;
        let weight = weight_of(value).unwrap();

        // Weighing *less* than the leaf: the direction that used to agree with itself.
        let mut sut = boxed_zeroed();
        let slot = sut.insert(value, weight).unwrap();
        let agreeing_total = weight - weight_of(2 * value).unwrap();
        assert_eq!(agreeing_total, 41_666_666_667);
        assert_eq!(
            error_code_number(sut.remove(slot, 2 * value, agreeing_total).unwrap_err()),
            6700
        );
        assert_eq!(
            sut.total_weight, weight,
            "a rejected remove must not mutate the aggregate"
        );
        assert_eq!(
            sut.free_count, 0,
            "a rejected remove must not release the slot"
        );
        assert_eq!(sum_of_leaves(&sut.tree), weight);

        // Weighing *more*: the direction that already underflowed the aggregate.
        let mut sut = boxed_zeroed();
        let slot = sut.insert(value, weight).unwrap();
        let over = weight_of(value / 2).unwrap();
        assert!(over > weight);
        assert_eq!(
            error_code_number(sut.remove(slot, value / 2, 0).unwrap_err()),
            6700
        );
        assert_eq!(sut.total_weight, weight);
        assert_eq!(sut.free_count, 0);
    }

    /// The compounding half: a residual left at a released slot is picked up by the next
    /// `insert`, which `fenwick_add`s on top of it, and from then on Σ leaves exceeds
    /// `total_weight` permanently. Asserted against `sum_of_leaves`, never against
    /// `total_weight` — the aggregate is the thing that cannot see the drift.
    #[test]
    fn a_recycled_slot_carries_no_residual_into_the_next_insert() {
        let mut sut = SmallIndex::new(8);
        let a = sut.insert(100, 100).unwrap();
        sut.insert(50, 150).unwrap();

        // 150 - 40 == 110, so the aggregate check agrees while the leaf holds 100.
        assert_eq!(error_code_number(sut.remove(a, 40, 110).unwrap_err()), 6700);

        sut.remove(a, 100, 50).unwrap();
        let recycled = sut.insert(70, 120).unwrap();
        assert_eq!(recycled, a, "the released slot is the one reused");
        assert_eq!(sum_of_leaves(&sut.tree), 120);
        assert_eq!(sut.total_weight, sum_of_leaves(&sut.tree));
    }

    /// The second guard, driven directly: whatever route a residual could arrive by, the
    /// allocator must not build on top of one.
    #[test]
    fn allocating_a_slot_that_still_carries_weight_fails_closed() {
        let mut sut = SmallIndex::new(4);
        let a = sut.insert(100, 100).unwrap();
        sut.remove(a, 100, 0).unwrap();

        fenwick_add(&mut sut.tree, a as usize + 1, 7);
        assert_eq!(error_code_number(sut.insert(30, 30).unwrap_err()), 6700);
    }

    #[test]
    fn insert_rejects_a_value_with_no_representable_weight() {
        let mut sut = boxed_zeroed();
        assert_eq!(error_code_number(sut.insert(0, 0).unwrap_err()), 6401);
        assert_eq!(
            error_code_number(sut.insert(WEIGHT_C as u64 + 1, 0).unwrap_err()),
            6703
        );
        assert_eq!(
            sut.high_water, 0,
            "a rejected value must not allocate a slot"
        );
    }

    #[test]
    fn narrow_expected_total_rejects_values_above_u64_max() {
        let over = u128::from(u64::MAX) + 1;
        assert!(WeightIndex::narrow_expected_total(over).is_err());
        assert_eq!(WeightIndex::narrow_expected_total(42u128).unwrap(), 42u64);
    }

    // --- boundary cases ----------------------------------------------------------------------

    #[test]
    fn empty_tree_prefix_search_is_none() {
        let sut = SmallIndex::new(8);
        assert_eq!(sut.prefix_search(0), None);
    }

    #[test]
    fn single_leaf_boundaries() {
        let mut sut = SmallIndex::new(8);
        let slot = sut.insert(5, 5).unwrap();
        assert_eq!(sut.prefix_search(0), Some(slot));
        assert_eq!(sut.prefix_search(4), Some(slot));
        assert_eq!(sut.prefix_search(5), None);
    }

    #[test]
    fn r_zero_and_r_w_minus_one() {
        let mut sut = SmallIndex::new(8);
        let mut total = 0u64;
        let weights = [5u64, 3, 7];
        let mut slots = Vec::new();
        for &w in &weights {
            total += w;
            slots.push(sut.insert(w, total).unwrap());
        }
        assert_eq!(sut.prefix_search(0), Some(slots[0]));
        assert_eq!(sut.prefix_search(total - 1), Some(slots[2]));
    }

    #[test]
    fn r_lands_exactly_on_cumulative_boundary() {
        let mut sut = SmallIndex::new(8);
        let a = sut.insert(5, 5).unwrap();
        let b = sut.insert(3, 8).unwrap();
        // [0,5) -> a, [5,8) -> b. r=4 is the last value still in a; r=5 is the first in b.
        assert_eq!(sut.prefix_search(4), Some(a));
        assert_eq!(sut.prefix_search(5), Some(b));
    }

    #[test]
    fn removing_the_only_leaf_empties_the_tree() {
        let mut sut = SmallIndex::new(8);
        let slot = sut.insert(9, 9).unwrap();
        sut.remove(slot, 9, 0).unwrap();
        assert_eq!(sut.total_weight, 0);
        assert_eq!(sut.prefix_search(0), None);

        let slot2 = sut.insert(4, 4).unwrap();
        assert_eq!(slot2, slot);
    }

    #[test]
    fn max_leaf_weight_at_scale_no_overflow() {
        let max_leaf_weight = (WEIGHT_C / u128::from(DEFAULT_ADMISSION_FLOOR)) as u64;
        assert_eq!(max_leaf_weight, 83_333_333_333);

        let mut sut = boxed_zeroed();
        let mut w_real = 0u128;
        for _ in 0..FENWICK_CAPACITY {
            w_real = w_real.checked_add(u128::from(max_leaf_weight)).unwrap();
            let expected_total = WeightIndex::narrow_expected_total(w_real).unwrap();
            sut.insert(DEFAULT_ADMISSION_FLOOR, expected_total).unwrap();
        }
        let total = u64::try_from(w_real).unwrap();
        assert_eq!(sut.total_weight, total);
        assert_eq!(sut.prefix_search(0), Some(0));
        assert_eq!(
            sut.prefix_search(total - 1),
            Some((FENWICK_CAPACITY - 1) as u32)
        );
    }

    // --- capacity ------------------------------------------------------------------------------

    #[test]
    fn insert_past_capacity_fails_cleanly() {
        // `WEIGHT_C` is the largest value that still floors to a weight, and it floors to 1.
        let unit_weight_value = WEIGHT_C as u64;
        let mut sut = boxed_zeroed();
        let mut total: u64 = 0;
        for _ in 0..FENWICK_CAPACITY {
            total += 1;
            sut.insert(unit_weight_value, total).unwrap();
        }
        assert_eq!(sut.high_water as usize, FENWICK_CAPACITY);

        let err = sut.insert(unit_weight_value, total + 1).unwrap_err();
        assert_eq!(error_code_number(err), 6701);
        assert_eq!(
            sut.total_weight, total,
            "a failed insert must not mutate the aggregate"
        );
    }

    // --- update ----------------------------------------------------------------------------------

    #[test]
    fn update_refreshes_a_leaf_upward_in_place() {
        let mut sut = SmallIndex::new(4);
        let slot = sut.insert(10, 10).unwrap();
        sut.update(slot, 10, 25, 25).unwrap();
        assert_eq!(sut.total_weight, 25);
        assert_eq!(sum_of_leaves(&sut.tree), 25);
        assert_eq!(sut.free_count, 0, "update must not release the slot");
        assert_eq!(sut.high_water, 1, "update must not allocate a slot");
    }

    #[test]
    fn update_refreshes_a_leaf_downward_in_place() {
        let mut sut = SmallIndex::new(4);
        let slot = sut.insert(25, 25).unwrap();
        sut.update(slot, 25, 10, 10).unwrap();
        assert_eq!(sut.total_weight, 10);
        assert_eq!(sum_of_leaves(&sut.tree), 10);
        assert_eq!(sut.free_count, 0, "update must not release the slot");
        assert_eq!(sut.high_water, 1, "update must not allocate a slot");
    }

    #[test]
    fn update_is_a_no_op_when_the_value_does_not_change() {
        let mut sut = SmallIndex::new(4);
        let slot = sut.insert(10, 10).unwrap();
        sut.update(slot, 10, 10, 10).unwrap();
        assert_eq!(sut.total_weight, 10);
        assert_eq!(sum_of_leaves(&sut.tree), 10);
    }

    /// The desync guard, both directions: a claimed `old_weight` that the leaf does not hold is
    /// rejected before the tree is touched, whether the caller understated or overstated it.
    #[test]
    fn update_rejects_a_wrong_old_weight_in_both_directions() {
        let mut sut = SmallIndex::new(4);
        let slot = sut.insert(10, 10).unwrap();
        assert_eq!(
            error_code_number(sut.update(slot, 5, 20, 25).unwrap_err()),
            6700
        );
        assert_eq!(
            sut.total_weight, 10,
            "a rejected update must not mutate the aggregate"
        );
        assert_eq!(sum_of_leaves(&sut.tree), 10);

        let mut sut = SmallIndex::new(4);
        let slot = sut.insert(10, 10).unwrap();
        assert_eq!(
            error_code_number(sut.update(slot, 15, 20, 15).unwrap_err()),
            6700
        );
        assert_eq!(sut.total_weight, 10);
        assert_eq!(sum_of_leaves(&sut.tree), 10);
    }

    #[test]
    fn update_rejects_an_out_of_bounds_slot() {
        let mut sut = SmallIndex::new(4);
        let err = sut.update(4, 10, 20, 20).unwrap_err();
        assert_eq!(error_code_number(err), 6700);
    }

    #[test]
    fn update_reports_weight_desync_on_mismatched_expected_total() {
        let mut sut = SmallIndex::new(4);
        let slot = sut.insert(10, 10).unwrap();
        let err = sut.update(slot, 10, 20, 999).unwrap_err();
        assert_eq!(error_code_number(err), 6700);
    }

    /// The highest-risk path: a refresh sitting between an insert and the eventual
    /// remove that recycles the slot, with the oracle agreeing at each step.
    #[test]
    fn insert_update_remove_insert_recycles_the_slot_with_oracle_agreement() {
        let mut sut = SmallIndex::new(4);
        let mut oracle = Oracle::new(4);

        let slot = sut.insert(10, 10).unwrap();
        oracle.leaves[slot as usize] = 10;
        assert_eq!(sut.total_weight, oracle.total());
        assert_eq!(sum_of_leaves(&sut.tree), oracle.total());

        sut.update(slot, 10, 40, 40).unwrap();
        oracle.leaves[slot as usize] = 40;
        assert_eq!(sut.total_weight, oracle.total());
        assert_eq!(sum_of_leaves(&sut.tree), oracle.total());

        sut.remove(slot, 40, 0).unwrap();
        oracle.leaves[slot as usize] = 0;
        assert_eq!(sut.total_weight, oracle.total());
        assert_eq!(sum_of_leaves(&sut.tree), oracle.total());
        assert_eq!(sut.free_count, 1);

        let recycled = sut.insert(15, 15).unwrap();
        oracle.leaves[recycled as usize] = 15;
        assert_eq!(recycled, slot, "the released slot is the one reused");
        assert_eq!(sut.total_weight, oracle.total());
        assert_eq!(sum_of_leaves(&sut.tree), oracle.total());
    }

    /// The same property extended to `update`: the leaf weight is derived from the value, never
    /// supplied.
    #[test]
    fn update_moves_the_tree_by_the_derived_weight() {
        let old_value = DEFAULT_ADMISSION_FLOOR;
        let new_value = 2 * DEFAULT_ADMISSION_FLOOR;
        let old_weight = weight_of(old_value).unwrap();
        let new_weight = weight_of(new_value).unwrap();

        let mut sut = boxed_zeroed();
        let slot = sut.insert(old_value, old_weight).unwrap();
        sut.update(slot, old_value, new_value, new_weight).unwrap();
        assert_eq!(sut.total_weight, new_weight);
    }

    /// `old_value == new_value` runs the same guarded path as any other refresh — a wrong leaf,
    /// a wrong `expected_total`, an unallocated slot and an out-of-bounds slot must all still
    /// fail closed on a no-op.
    #[test]
    fn no_op_update_rejects_a_leaf_that_does_not_match() {
        let mut sut = SmallIndex::new(4);
        let slot = sut.insert(10, 10).unwrap();
        let err = sut.update(slot, 20, 20, 10).unwrap_err();
        assert_eq!(error_code_number(err), 6700);
        assert_eq!(
            sut.total_weight, 10,
            "a rejected no-op update must not mutate the aggregate"
        );
    }

    #[test]
    fn no_op_update_rejects_a_mismatched_expected_total() {
        let mut sut = SmallIndex::new(4);
        let slot = sut.insert(10, 10).unwrap();
        let err = sut.update(slot, 10, 10, 999).unwrap_err();
        assert_eq!(error_code_number(err), 6700);
    }

    #[test]
    fn no_op_update_rejects_a_never_allocated_slot() {
        let mut sut = SmallIndex::new(4);
        let err = sut.update(0, 5, 5, 0).unwrap_err();
        assert_eq!(error_code_number(err), 6700);
    }

    #[test]
    fn no_op_update_rejects_an_out_of_bounds_slot() {
        let mut sut = SmallIndex::new(4);
        let err = sut.update(4, 10, 10, 10).unwrap_err();
        assert_eq!(error_code_number(err), 6700);
    }

    // --- field order is the zero-copy struct's own memory layout ------------------------------

    /// `WeightIndex` is `#[account(zero_copy)]` — `Pod`/`Zeroable`, `#[repr(C)]`, no borsh
    /// framing — so its declared field order *is* the raw byte layout `bytemuck::bytes_of` reads
    /// straight off the account, with no `try_to_vec()` round trip to go through. A
    /// `high_water` ⇄ `free_count` swap (both `u32`) compiles clean, leaves `weight_index_size`
    /// above unchanged (the total byte count is the same), and silently swaps which counter a
    /// reader of the raw account sees. `tree[0]`/`free_stack[0]` are also checked, so a swap of
    /// the header block with either array — which would need a size change too, but is cheap to
    /// rule out here — is caught as well.
    #[test]
    fn weight_index_field_order() {
        let mut sut = boxed_zeroed();
        sut.pool = Pubkey::new_unique();
        sut.total_weight = 111;
        sut.high_water = 222;
        sut.free_count = 333;
        sut.tree[0] = 444;
        sut.free_stack[0] = 555;

        let bytes = bytemuck::bytes_of(&*sut);
        assert_eq!(bytes.len(), std::mem::size_of::<WeightIndex>());

        assert_eq!(&bytes[0..32], sut.pool.as_ref(), "pool moved");
        assert_eq!(
            u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
            sut.total_weight,
            "total_weight moved"
        );
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            sut.high_water,
            "high_water moved — the exact high_water/free_count swap this pin closes"
        );
        assert_eq!(
            u32::from_le_bytes(bytes[44..48].try_into().unwrap()),
            sut.free_count,
            "free_count moved"
        );
        assert_eq!(
            &bytes[48..56],
            &[0u8; 8],
            "_padding carried a non-zero byte — a header field moved into or past it"
        );

        const TREE_OFFSET: usize = 56;
        assert_eq!(
            u64::from_le_bytes(bytes[TREE_OFFSET..TREE_OFFSET + 8].try_into().unwrap()),
            sut.tree[0],
            "tree moved — no longer directly after the header"
        );

        const FREE_STACK_OFFSET: usize = TREE_OFFSET + 8 * FENWICK_CAPACITY;
        assert_eq!(
            u32::from_le_bytes(
                bytes[FREE_STACK_OFFSET..FREE_STACK_OFFSET + 4]
                    .try_into()
                    .unwrap()
            ),
            sut.free_stack[0],
            "free_stack moved — no longer directly after tree"
        );
        assert_eq!(
            FREE_STACK_OFFSET + 4 * FENWICK_CAPACITY,
            bytes.len(),
            "free_stack's own extent does not reach the struct's end — a field exists after it \
             that this pin does not account for"
        );
    }
}
