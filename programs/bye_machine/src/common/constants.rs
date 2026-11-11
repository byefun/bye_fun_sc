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
pub const DEFAULT_BLANK_FACES: [u64; BLANK_SLOTS] =
    [1_000_000, 2_000_000, 5_000_000, 0, 0, 0, 0, 0];

/// Blank-face slots per pool, and the length of every per-slot array on `Pool`, `RollBatch`,
/// `RollRecord` and the events that carry them. Slots `0..BELOW_TICKET_SLOTS` hold faces below
/// the ticket, `ABOVE_TICKET_SLOT` holds the one face above it (0 = none), and every later slot
/// is reserved and must hold 0.
pub const BLANK_SLOTS: usize = 8;
pub const BELOW_TICKET_SLOTS: usize = 3;
pub const ABOVE_TICKET_SLOT: usize = 3;

/// The least the above-ticket face may sit over the ticket, in micro-USDC. The controller's
/// above-ticket count is `D / (C − T·w)`, whose step shrinks to nothing as the face nears the
/// ticket, so a face just over it would set a count large enough to freeze the pool.
pub const MIN_ABOVE_TICKET_GAP: u64 = 1_000_000;

/// The escrow bid deployment scripts set on a pool, in bps of a card's value. The program never
/// applies it: a pool's bid is 0 until an Administrator sets it through `update_config`.
pub const DEFAULT_ESCROW_BID_BPS: u16 = 9_250;

/// At zero the sampler would take no attempt at all and record a draw of 0 for every roll, with
/// no error and no branch to notice it.
const _: () = assert!(EXPANSION_MAX_ATTEMPTS > 0);

const _: () = assert!(DEFAULT_FEE == DEFAULT_PRICE - DEFAULT_TICKET_TARGET);
const _: () = assert!(
    DEFAULT_ALLOC_EQUAL_BPS as u32
        + DEFAULT_ALLOC_TIER_BPS as u32
        + DEFAULT_ALLOC_PROTOCOL_BPS as u32
        == 10_000
);
const _: () = assert!(DEFAULT_ADMISSION_FLOOR > DEFAULT_TICKET_TARGET);
const _: () =
    assert!(DEFAULT_BUYBACK_RATE_BPS <= DEFAULT_ESCROW_BID_BPS && DEFAULT_ESCROW_BID_BPS <= 10_000);
const _: () = assert!(ABOVE_TICKET_SLOT == BELOW_TICKET_SLOTS && ABOVE_TICKET_SLOT < BLANK_SLOTS);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::guards::validate_blank_faces;

    #[test]
    fn the_default_faces_pass_the_blank_face_guard() {
        assert!(validate_blank_faces(
            DEFAULT_TICKET_TARGET,
            DEFAULT_ADMISSION_FLOOR,
            &DEFAULT_BLANK_FACES
        )
        .is_ok());
    }
}
