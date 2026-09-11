/// `w = WEIGHT_C / V`, floor division; leaf value stored as `u64`.
pub const WEIGHT_C: u128 = 1_000_000_000_000_000_000;

/// Fee-accumulator fixed-point scaling.
pub const ACC_PRECISION: u128 = 1_000_000_000_000;

/// VRF provider queue TTL, in slots, before a batch becomes recoverable.
pub const VRF_TIMEOUT_SLOTS: u64 = 240;

/// Array capacity of `RollRecord`s per batch; the enforced `max_n` is config `<=` this.
pub const MAX_BATCH_CAPACITY: usize = 10;

/// Rejection-sampling attempt cap when deriving a draw from `RollBatch.randomness`.
pub const EXPANSION_MAX_ATTEMPTS: u8 = 4;

/// Leaves per `WeightIndex` Fenwick tree.
pub const FENWICK_CAPACITY: usize = 16_384;

/// Launch-config defaults seeded at `init_pool`; an Administrator may change most of
/// these afterward through `update_config`.
pub const DEFAULT_PRICE: u64 = 9_900_000;
pub const DEFAULT_TICKET_TARGET: u64 = 9_000_000;
pub const DEFAULT_FEE: u64 = 900_000;
pub const DEFAULT_ALLOC_EQUAL_BPS: u16 = 7_500;
pub const DEFAULT_ALLOC_TIER_BPS: u16 = 500;
pub const DEFAULT_ALLOC_PROTOCOL_BPS: u16 = 2_000;
pub const DEFAULT_ADMISSION_FLOOR: u64 = 12_000_000;
pub const DEFAULT_T04_CEILING: u64 = 2_000_000_000;
pub const DEFAULT_SWEEP_CADENCE_HOURS: u16 = 24;
pub const DEFAULT_TIER_SIZE: u8 = 20;
pub const DEFAULT_BUYBACK_RATE_BPS: u16 = 8_500;
pub const DEFAULT_BLANK_FACES: [u64; 3] = [1_000_000, 2_000_000, 5_000_000];

const fn all_faces_below(faces: [u64; 3], ceiling: u64) -> bool {
    let mut i = 0;
    while i < faces.len() {
        if faces[i] >= ceiling {
            return false;
        }
        i += 1;
    }
    true
}

const _: () = assert!(DEFAULT_FEE == DEFAULT_PRICE - DEFAULT_TICKET_TARGET);
const _: () = assert!(
    DEFAULT_ALLOC_EQUAL_BPS as u32
        + DEFAULT_ALLOC_TIER_BPS as u32
        + DEFAULT_ALLOC_PROTOCOL_BPS as u32
        == 10_000
);
const _: () = assert!(DEFAULT_ADMISSION_FLOOR > DEFAULT_TICKET_TARGET);
const _: () = assert!(all_faces_below(DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET));
