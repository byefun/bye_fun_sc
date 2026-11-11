//! Grows a pool written with three blank slots to the eight-slot layout, in place.
//!
//! `Account<Pool>` cannot deserialize a pool at the old size, so every instruction that takes one
//! fails until it is grown; `pool` is therefore unchecked here and validated by hand. The grown
//! pool reads back every field it held, with face and count slots 3–7 zero. A pool already at the
//! current size is accepted unchanged, so a second call is a no-op.

use anchor_lang::prelude::*;
use anchor_lang::system_program::{self, Transfer};
use anchor_lang::Discriminator;

use crate::common::errors::ByeMachineError;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::state::{Pool, ProtocolConfig};

/// A pool's account length, discriminator included, with three blank slots.
pub const LEGACY_POOL_LEN: usize = 342;
/// A pool's account length today, discriminator included.
pub const POOL_LEN: usize = 8 + Pool::INIT_SPACE;

/// The end of the three-slot `blank_faces`; every byte before it keeps its offset.
const LEGACY_FACES_END: usize = 155;
/// The end of the three-slot `blanks`; every field from here on moves by both widenings.
const LEGACY_COUNTS_END: usize = 186;
/// `open_batches` in the three-slot layout: right after `w_real`, a `u128` at the counts' end.
const LEGACY_OPEN_BATCHES_AT: usize = LEGACY_COUNTS_END + 16;
/// The five added face slots, `u64` each.
const ADDED_FACE_BYTES: usize = 5 * 8;
/// The five added count slots, `u32` each.
const ADDED_COUNT_BYTES: usize = 5 * 4;

pub fn handler(ctx: Context<MigratePool>) -> Result<()> {
    let pool = ctx.accounts.pool.to_account_info();
    let needs_growth = {
        let data = pool.try_borrow_data()?;
        needs_growth(pool.owner, &data)?
    };
    if !needs_growth {
        return Ok(());
    }

    let shortfall = Rent::get()?
        .minimum_balance(POOL_LEN)
        .saturating_sub(pool.lamports());
    if shortfall > 0 {
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.administrator.to_account_info(),
                    to: pool.clone(),
                },
            ),
            shortfall,
        )?;
    }

    pool.resize(POOL_LEN)?;
    widen_blank_slots(&mut pool.try_borrow_mut_data()?);
    Ok(())
}

/// Whether `pool` is an old-size `Pool` to grow (`true`) or a current one to leave alone
/// (`false`). Anything else is refused: an account this program does not own, one without
/// `Pool`'s discriminator, a `Pool`-tagged account at neither length, and an old-size pool with a
/// roll batch open — that batch was committed against three-slot blank arrays the growth moves.
fn needs_growth(owner: &Pubkey, data: &[u8]) -> Result<bool> {
    require_keys_eq!(*owner, crate::ID, ErrorCode::AccountOwnedByWrongProgram);
    require!(
        data.starts_with(Pool::DISCRIMINATOR),
        ErrorCode::AccountDiscriminatorMismatch
    );
    match data.len() {
        LEGACY_POOL_LEN => {
            let open_batches = data[LEGACY_OPEN_BATCHES_AT..LEGACY_OPEN_BATCHES_AT + 4]
                .try_into()
                .map(u32::from_le_bytes)
                .map_err(|_| ErrorCode::AccountDidNotDeserialize)?;
            require_eq!(open_batches, 0, ByeMachineError::RollBatchInFlight);
            Ok(true)
        }
        POOL_LEN => Ok(false),
        _ => err!(ErrorCode::AccountDidNotDeserialize),
    }
}

/// Rewrites a `POOL_LEN` buffer holding a three-slot pool in its first `LEGACY_POOL_LEN` bytes into
/// the eight-slot layout. Bytes 155–185 (`t04_ceiling` through the three live counts) move 40
/// later, bytes 186–341 move 60 later, and the new face slots (155–194) and count slots
/// (226–245) are zeroed. The later block moves first, so neither copy reads a byte the other has
/// already overwritten.
fn widen_blank_slots(data: &mut [u8]) {
    let counts_shift = ADDED_FACE_BYTES + ADDED_COUNT_BYTES;
    data.copy_within(
        LEGACY_COUNTS_END..LEGACY_POOL_LEN,
        LEGACY_COUNTS_END + counts_shift,
    );
    data.copy_within(
        LEGACY_FACES_END..LEGACY_COUNTS_END,
        LEGACY_FACES_END + ADDED_FACE_BYTES,
    );
    data[LEGACY_FACES_END..LEGACY_FACES_END + ADDED_FACE_BYTES].fill(0);
    let added_counts = LEGACY_COUNTS_END + ADDED_FACE_BYTES;
    data[added_counts..added_counts + ADDED_COUNT_BYTES].fill(0);
}

#[derive(Accounts)]
pub struct MigratePool<'info> {
    #[account(mut)]
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    /// CHECK: owner, discriminator and length are checked in the handler; `Account<Pool>` cannot
    /// read a pool at the old length, which is the only pool this instruction changes.
    #[account(mut)]
    pub pool: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{accounts_body, calls, code, production};

    const SRC: &str = include_str!("migrate_pool.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn pool_bytes(len: usize) -> Vec<u8> {
        let mut data = vec![0xAB; len];
        data[..8].copy_from_slice(Pool::DISCRIMINATOR);
        data
    }

    /// `Pool` as it was with three blank slots, field for field.
    #[derive(AnchorSerialize)]
    struct ThreeSlotPool {
        pool_id: u16,
        weight_index: Pubkey,
        treasury_03: Pubkey,
        price: u64,
        ticket_target: u64,
        fee: u64,
        alloc_equal_bps: u16,
        alloc_tier_bps: u16,
        alloc_protocol_bps: u16,
        admission_floor: u64,
        admission_ceiling: u64,
        wallet_value_cap: u64,
        max_n: u8,
        buyback_rate_bps: u16,
        blank_faces: [u64; 3],
        t04_ceiling: u64,
        sweep_cadence_hours: u16,
        tier_size: u8,
        deposits_paused: bool,
        rolls_paused: bool,
        pause_reason: u16,
        n_real: u32,
        blanks: [u32; 3],
        w_real: u128,
        open_batches: u32,
        batch_counter: u64,
        position_counter: u64,
        pending_roll_liability: u64,
        owed_fees: u64,
        acc_equal: u128,
        sweep_pending: bool,
        sweep_epoch: u64,
        last_sweep_at: i64,
        sweep_updates: u32,
        bump: u8,
        escrow_bid_bps: u16,
        reserved: [u8; 64],
    }

    /// Every field distinct and non-zero, the reserved bytes included, so a byte copied to the
    /// wrong place shows up in the comparison.
    fn three_slot_pool() -> ThreeSlotPool {
        let mut reserved = [0u8; 64];
        for (i, byte) in reserved.iter_mut().enumerate() {
            *byte = 0xC0 ^ i as u8;
        }
        ThreeSlotPool {
            pool_id: 2,
            weight_index: Pubkey::new_unique(),
            treasury_03: Pubkey::new_unique(),
            price: 19_800_000,
            ticket_target: 18_000_000,
            fee: 1_800_000,
            alloc_equal_bps: 7_500,
            alloc_tier_bps: 500,
            alloc_protocol_bps: 2_000,
            admission_floor: 20_000_000,
            admission_ceiling: 1_000_000_000,
            wallet_value_cap: 77,
            max_n: 10,
            buyback_rate_bps: 8_500,
            blank_faces: [2_000_000, 4_000_000, 10_000_000],
            t04_ceiling: 2_000_000_001,
            sweep_cadence_hours: 24,
            tier_size: 20,
            deposits_paused: true,
            rolls_paused: true,
            pause_reason: 0x1234,
            n_real: 41,
            blanks: [3, 5, 7],
            w_real: 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10,
            open_batches: 9,
            batch_counter: 1_684,
            position_counter: 500,
            pending_roll_liability: 11,
            owed_fees: 12,
            acc_equal: 0x1112_1314_1516_1718_191A_1B1C_1D1E_1F20,
            sweep_pending: true,
            sweep_epoch: 13,
            last_sweep_at: -14,
            sweep_updates: 15,
            bump: 254,
            escrow_bid_bps: 9_250,
            reserved,
        }
    }

    fn legacy_account(old: &ThreeSlotPool) -> Vec<u8> {
        let mut data = Pool::DISCRIMINATOR.to_vec();
        data.extend(old.try_to_vec().unwrap());
        assert_eq!(data.len(), LEGACY_POOL_LEN);
        data
    }

    #[test]
    fn the_legacy_length_is_today_s_less_five_face_and_five_count_slots() {
        assert_eq!(POOL_LEN, 402);
        assert_eq!(
            POOL_LEN - LEGACY_POOL_LEN,
            ADDED_FACE_BYTES + ADDED_COUNT_BYTES
        );
        assert_eq!(ADDED_FACE_BYTES + ADDED_COUNT_BYTES, 60);
    }

    /// The two boundaries are where the three-slot arrays ended, read off a serialized pool
    /// rather than restated: `blank_faces` and `blanks` both start where they always did.
    #[test]
    fn the_boundaries_are_where_the_three_slot_arrays_ended() {
        let old = legacy_account(&three_slot_pool());
        let faces_at = LEGACY_FACES_END - 3 * 8;
        let counts_at = LEGACY_COUNTS_END - 3 * 4;
        assert_eq!(&old[faces_at..faces_at + 8], &2_000_000u64.to_le_bytes());
        assert_eq!(&old[counts_at..counts_at + 4], &3u32.to_le_bytes());

        let mut pool = Pool::blank();
        pool.blank_faces = [u64::MAX; 8];
        pool.blanks = [u32::MAX; 8];
        let mut new = Pool::DISCRIMINATOR.to_vec();
        new.extend(pool.try_to_vec().unwrap());
        assert_eq!(new.iter().position(|b| *b == 0xFF), Some(faces_at));
        let counts_now = counts_at + ADDED_FACE_BYTES;
        assert_eq!(&new[counts_now..counts_now + 32], &[0xFF; 32]);
        assert_eq!(new[counts_now - 1], 0);
    }

    #[test]
    fn a_grown_pool_reads_back_every_field_it_held_with_the_new_slots_zero() {
        let old = three_slot_pool();
        let mut data = legacy_account(&old);
        data.resize(POOL_LEN, 0);
        widen_blank_slots(&mut data);

        let grown = Pool::try_deserialize(&mut data.as_slice()).unwrap();
        let expected = Pool {
            pool_id: old.pool_id,
            weight_index: old.weight_index,
            treasury_03: old.treasury_03,
            price: old.price,
            ticket_target: old.ticket_target,
            fee: old.fee,
            alloc_equal_bps: old.alloc_equal_bps,
            alloc_tier_bps: old.alloc_tier_bps,
            alloc_protocol_bps: old.alloc_protocol_bps,
            admission_floor: old.admission_floor,
            admission_ceiling: old.admission_ceiling,
            wallet_value_cap: old.wallet_value_cap,
            max_n: old.max_n,
            buyback_rate_bps: old.buyback_rate_bps,
            blank_faces: [
                old.blank_faces[0],
                old.blank_faces[1],
                old.blank_faces[2],
                0,
                0,
                0,
                0,
                0,
            ],
            t04_ceiling: old.t04_ceiling,
            sweep_cadence_hours: old.sweep_cadence_hours,
            tier_size: old.tier_size,
            deposits_paused: old.deposits_paused,
            rolls_paused: old.rolls_paused,
            pause_reason: old.pause_reason,
            n_real: old.n_real,
            blanks: [old.blanks[0], old.blanks[1], old.blanks[2], 0, 0, 0, 0, 0],
            w_real: old.w_real,
            open_batches: old.open_batches,
            batch_counter: old.batch_counter,
            position_counter: old.position_counter,
            pending_roll_liability: old.pending_roll_liability,
            owed_fees: old.owed_fees,
            acc_equal: old.acc_equal,
            sweep_pending: old.sweep_pending,
            sweep_epoch: old.sweep_epoch,
            last_sweep_at: old.last_sweep_at,
            sweep_updates: old.sweep_updates,
            bump: old.bump,
            escrow_bid_bps: old.escrow_bid_bps,
            treasury_04: Pubkey::try_from(&old.reserved[..32]).unwrap(),
            reserved: old.reserved[32..].try_into().unwrap(),
        };
        assert_eq!(grown.try_to_vec().unwrap(), expected.try_to_vec().unwrap());
        assert_eq!(&data[..8], Pool::DISCRIMINATOR);
    }

    /// The new slots are zeroed by the rewrite itself, not by whatever the resize left there.
    #[test]
    fn the_new_slots_are_zeroed_whatever_the_grown_tail_held() {
        let mut data = legacy_account(&three_slot_pool());
        data.resize(POOL_LEN, 0xEE);
        widen_blank_slots(&mut data);
        let grown = Pool::try_deserialize(&mut data.as_slice()).unwrap();
        assert_eq!(grown.blank_faces[3..], [0; 5]);
        assert_eq!(grown.blanks[3..], [0; 5]);
        assert_eq!(
            [grown.treasury_04.to_bytes().as_slice(), &grown.reserved].concat(),
            three_slot_pool().reserved
        );
    }

    /// An old-size pool grows only with no batch open, read off the three-slot layout itself.
    #[test]
    fn an_old_size_pool_with_a_batch_open_is_refused_at_6200() {
        let mut open = three_slot_pool();
        open.open_batches = 9;
        assert_eq!(
            error_code(needs_growth(&crate::ID, &legacy_account(&open)).unwrap_err()),
            6200
        );
        let mut closed = three_slot_pool();
        closed.open_batches = 0;
        assert!(needs_growth(&crate::ID, &legacy_account(&closed)).unwrap());
    }

    #[test]
    fn an_old_size_pool_is_grown_and_a_current_one_is_left_alone() {
        let mut legacy = pool_bytes(LEGACY_POOL_LEN);
        legacy[LEGACY_OPEN_BATCHES_AT..LEGACY_OPEN_BATCHES_AT + 4].fill(0);
        assert!(needs_growth(&crate::ID, &legacy).unwrap());
        assert!(!needs_growth(&crate::ID, &pool_bytes(POOL_LEN)).unwrap());
    }

    #[test]
    fn an_account_another_program_owns_is_refused() {
        for owner in [Pubkey::default(), Pubkey::new_unique()] {
            assert_eq!(
                error_code(needs_growth(&owner, &pool_bytes(LEGACY_POOL_LEN)).unwrap_err()),
                u32::from(ErrorCode::AccountOwnedByWrongProgram)
            );
        }
    }

    #[test]
    fn an_account_without_the_pool_discriminator_is_refused() {
        let mut data = pool_bytes(LEGACY_POOL_LEN);
        data[..8].copy_from_slice(crate::state::Position::DISCRIMINATOR);
        assert_eq!(
            error_code(needs_growth(&crate::ID, &data).unwrap_err()),
            u32::from(ErrorCode::AccountDiscriminatorMismatch)
        );
        for byte in 0..8 {
            let mut data = pool_bytes(LEGACY_POOL_LEN);
            data[byte] ^= 1;
            assert!(needs_growth(&crate::ID, &data).is_err(), "byte {byte}");
        }
        assert!(needs_growth(&crate::ID, &[]).is_err());
    }

    /// The pre-escrow 276-byte pool is among the refused lengths: it is not this migration's input.
    #[test]
    fn a_pool_at_any_other_length_is_refused() {
        for len in [
            8,
            276,
            LEGACY_POOL_LEN - 1,
            LEGACY_POOL_LEN + 1,
            POOL_LEN - 1,
            POOL_LEN + 1,
            10_240,
        ] {
            assert_eq!(
                error_code(needs_growth(&crate::ID, &pool_bytes(len)).unwrap_err()),
                u32::from(ErrorCode::AccountDidNotDeserialize),
                "length {len}"
            );
        }
    }

    #[test]
    fn the_administrator_is_the_only_signer_and_pays() {
        let prod = production(SRC);
        assert_eq!(
            prod.matches(
                "constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized"
            )
            .count(),
            1
        );
        assert_eq!(accounts_body(SRC).matches("Signer<'info>").count(), 1);
        assert!(prod.contains("from: ctx.accounts.administrator.to_account_info()"));
    }

    /// The checks run before anything moves, the pool is grown only after it is funded for the new
    /// length, and the bytes are rewritten only once the account is long enough to hold them.
    #[test]
    fn the_checks_precede_the_rent_transfer_the_resize_and_the_rewrite() {
        let prod = code(SRC);
        let check = prod.find("needs_growth(pool.owner").unwrap();
        let transfer = prod.find("system_program::transfer(").unwrap();
        let resize = prod.find("pool.resize(POOL_LEN)").unwrap();
        let rewrite = prod.find("widen_blank_slots(&mut pool").unwrap();
        assert!(check < transfer && transfer < resize && resize < rewrite);
        assert_eq!(calls(SRC, "resize"), 1);
        assert_eq!(calls(SRC, "widen_blank_slots"), 1);
    }

    /// No `Pool` is deserialized, no pause flag is read, and no event is emitted: the migration
    /// moves bytes, refusing only an old-size pool whose batch count is not zero.
    #[test]
    fn no_pool_is_deserialized_and_nothing_is_emitted() {
        let prod = code(SRC);
        assert_eq!(calls(SRC, "emit!"), 0);
        for forbidden in [
            "Account<'info, Pool>",
            "try_deserialize",
            "deposits_paused",
            "rolls_paused",
        ] {
            assert!(!prod.contains(forbidden), "{forbidden}");
        }
    }
}
