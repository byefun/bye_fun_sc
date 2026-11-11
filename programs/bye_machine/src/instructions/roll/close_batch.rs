//! Closes a finished batch and pays its rent to whoever signs.
//!
//! The batch is read by offset rather than through `Account<RollBatch>`, so a batch written in the
//! 1,100-byte three-slot layout closes as well as one in today's 1,360-byte layout: the fields
//! read here sit at the same offsets in both, and `bump` is the last byte of each. The roller
//! signs, or the Operator signs for them; the rent goes to the signer, so the Operator recovers
//! the rent of batches it closes.

use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_lang::Discriminator;

use crate::common::errors::ByeMachineError;
use crate::common::events::BatchClosed;
use crate::common::seeds::{BATCH_SEED, PROTOCOL_CONFIG_SEED};
use crate::state::{BatchState, ProtocolConfig, RollBatch};

/// A batch's account length, discriminator included, in the three-slot layout.
pub const LEGACY_BATCH_LEN: usize = 1_100;
/// A batch's account length today, discriminator included.
pub const BATCH_LEN: usize = 8 + RollBatch::INIT_SPACE;

const POOL_AT: usize = 8;
const ROLLER_AT: usize = 40;
const BATCH_ID_AT: usize = 72;
const STATE_AT: usize = 80;

#[derive(Accounts)]
pub struct CloseBatch<'info> {
    /// The batch's roller, or the Operator; receives the batch's rent.
    #[account(mut)]
    pub signer: Signer<'info>,

    /// CHECK: must be the roller the batch names, checked in the handler; it is never written.
    #[account(mut)]
    pub roller: UncheckedAccount<'info>,

    /// CHECK: owner, length, discriminator, address and state are checked in the handler, which
    /// reads it by offset so a batch of either layout closes.
    #[account(mut)]
    pub batch: UncheckedAccount<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
}

/// The fields `close_batch` reads, at offsets common to both layouts.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct BatchHeader {
    pool: Pubkey,
    roller: Pubkey,
    batch_id: u64,
    bump: u8,
}

/// A closable batch's header: owned by this program, at either layout's length, carrying
/// `RollBatch`'s discriminator, and `Recovered` or `Complete` — its liability fully resolved one
/// way or the other.
fn read_closable_batch(owner: &Pubkey, data: &[u8]) -> Result<BatchHeader> {
    require_keys_eq!(*owner, crate::ID, ErrorCode::AccountOwnedByWrongProgram);
    require!(
        data.len() == LEGACY_BATCH_LEN || data.len() == BATCH_LEN,
        ErrorCode::AccountDidNotDeserialize
    );
    require!(
        data.starts_with(RollBatch::DISCRIMINATOR),
        ErrorCode::AccountDiscriminatorMismatch
    );

    let state = data[STATE_AT];
    require!(
        state == BatchState::Recovered as u8 || state == BatchState::Complete as u8,
        ByeMachineError::BatchNotClosable
    );

    let pubkey_at = |at: usize| -> Result<Pubkey> {
        let bytes: [u8; 32] = data[at..at + 32]
            .try_into()
            .map_err(|_| ErrorCode::AccountDidNotDeserialize)?;
        Ok(Pubkey::new_from_array(bytes))
    };
    let batch_id = data[BATCH_ID_AT..BATCH_ID_AT + 8]
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| ErrorCode::AccountDidNotDeserialize)?;
    let bump = *data.last().ok_or(ErrorCode::AccountDidNotDeserialize)?;

    Ok(BatchHeader {
        pool: pubkey_at(POOL_AT)?,
        roller: pubkey_at(ROLLER_AT)?,
        batch_id,
        bump,
    })
}

/// The batch sits at its own PDA under the bump it stores.
fn assert_batch_address(batch: &Pubkey, header: &BatchHeader) -> Result<()> {
    let derived = Pubkey::create_program_address(
        &[
            BATCH_SEED,
            header.pool.as_ref(),
            header.batch_id.to_le_bytes().as_ref(),
            &[header.bump],
        ],
        &crate::ID,
    )
    .map_err(|_| ErrorCode::ConstraintSeeds)?;
    require_keys_eq!(derived, *batch, ErrorCode::ConstraintSeeds);
    Ok(())
}

/// The roller account must be the roller the batch names, and only that roller or the Operator
/// may sign.
fn assert_closer(
    signer: &Pubkey,
    roller: &Pubkey,
    header: &BatchHeader,
    operator: &Pubkey,
) -> Result<()> {
    require_keys_eq!(*roller, header.roller, ByeMachineError::Unauthorized);
    require!(
        *signer == header.roller || *signer == *operator,
        ByeMachineError::Unauthorized
    );
    Ok(())
}

pub fn handler(ctx: Context<CloseBatch>) -> Result<()> {
    let batch = ctx.accounts.batch.to_account_info();
    let roller = ctx.accounts.roller.to_account_info();
    let signer = ctx.accounts.signer.to_account_info();

    let header = {
        let data = batch.try_borrow_data()?;
        read_closable_batch(batch.owner, &data)?
    };
    assert_batch_address(batch.key, &header)?;
    assert_closer(
        signer.key,
        roller.key,
        &header,
        &ctx.accounts.protocol_config.operator,
    )?;

    emit!(BatchClosed {
        pool: header.pool,
        slot: Clock::get()?.slot,
        batch: batch.key(),
        rent_recipient: signer.key(),
    });

    let rent = batch.lamports();
    let credited = signer
        .lamports()
        .checked_add(rent)
        .ok_or(ByeMachineError::ArithmeticFailure)?;
    **signer.try_borrow_mut_lamports()? = credited;
    **batch.try_borrow_mut_lamports()? = 0;
    batch.assign(&system_program::ID);
    batch.resize(0)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{code, production};
    use crate::state::{RollOutcome, RollRecord, RollStatus};

    const SRC: &str = include_str!("close_batch.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    fn undrawn() -> RollRecord {
        RollRecord {
            status: RollStatus::Pending,
            outcome: RollOutcome::None,
            selected_mint: Pubkey::default(),
            blank_face_idx: 0,
            draw_value: 0,
            attempts: 0,
            w_at_draw: 0,
            n_real_at_draw: 0,
            blanks_at_draw: [0; 8],
            refund_reason: 0,
        }
    }

    fn batch_in(state: BatchState) -> RollBatch {
        RollBatch {
            pool: Pubkey::new_unique(),
            roller: Pubkey::new_unique(),
            batch_id: 0x0102_0304_0506_0708,
            state,
            n: 1,
            next_roll: 1,
            settled: 1,
            refunded: 0,
            request_slot: 0,
            caller_seed: [0u8; 32],
            randomness: [0u8; 32],
            expected_w: 0,
            expected_blanks: [0; 8],
            s_price: 9_900_000,
            s_ticket: 9_000_000,
            s_fee: 900_000,
            s_alloc_bps: [7_500, 500, 2_000],
            s_blank_faces: [1_000_000, 2_000_000, 5_000_000, 0, 0, 0, 0, 0],
            rolls: [undrawn(); 10],
            bump: 253,
        }
    }

    fn account(batch: &RollBatch) -> Vec<u8> {
        let mut data = RollBatch::DISCRIMINATOR.to_vec();
        data.extend(batch.try_to_vec().unwrap());
        data
    }

    /// The three-slot layout of the same batch: identical through `expected_w`, 260 bytes shorter
    /// in the blank arrays and records, `bump` last.
    fn legacy_account(batch: &RollBatch) -> Vec<u8> {
        let mut data = account(batch)[..173].to_vec();
        data.resize(LEGACY_BATCH_LEN - 1, 0);
        data.push(batch.bump);
        data
    }

    fn header_of(batch: &RollBatch) -> BatchHeader {
        BatchHeader {
            pool: batch.pool,
            roller: batch.roller,
            batch_id: batch.batch_id,
            bump: batch.bump,
        }
    }

    #[test]
    fn the_two_lengths_are_the_two_layouts() {
        assert_eq!(BATCH_LEN, 1_360);
        assert_eq!(account(&batch_in(BatchState::Complete)).len(), BATCH_LEN);
        assert_eq!(BATCH_LEN - LEGACY_BATCH_LEN, 5 * 4 + 5 * 8 + 10 * 5 * 4);
    }

    /// The header offsets are read off a serialized batch, not restated.
    #[test]
    fn a_batch_of_either_layout_reads_back_its_header() {
        for state in [BatchState::Complete, BatchState::Recovered] {
            let batch = batch_in(state);
            for data in [account(&batch), legacy_account(&batch)] {
                assert_eq!(
                    read_closable_batch(&crate::ID, &data).unwrap(),
                    header_of(&batch),
                    "length {}",
                    data.len()
                );
            }
        }
    }

    #[test]
    fn a_batch_still_in_progress_is_refused_at_6211_in_either_layout() {
        for state in [BatchState::Committed, BatchState::Resolved] {
            let batch = batch_in(state);
            for data in [account(&batch), legacy_account(&batch)] {
                assert_eq!(
                    error_code(read_closable_batch(&crate::ID, &data).unwrap_err()),
                    6211
                );
            }
        }
    }

    /// The closable states are compared by the enum's own declaration index, which is its wire
    /// byte: 2 and 3, and nothing else.
    #[test]
    fn only_state_bytes_two_and_three_close() {
        let mut data = account(&batch_in(BatchState::Complete));
        for state in 0..=u8::MAX {
            data[STATE_AT] = state;
            assert_eq!(
                read_closable_batch(&crate::ID, &data).is_ok(),
                state == 2 || state == 3,
                "state byte {state}"
            );
        }
    }

    #[test]
    fn an_account_another_program_owns_is_refused() {
        let data = account(&batch_in(BatchState::Complete));
        for owner in [Pubkey::default(), Pubkey::new_unique()] {
            assert_eq!(
                error_code(read_closable_batch(&owner, &data).unwrap_err()),
                u32::from(ErrorCode::AccountOwnedByWrongProgram)
            );
        }
    }

    #[test]
    fn an_account_at_any_other_length_is_refused() {
        let data = account(&batch_in(BatchState::Complete));
        for len in [
            0,
            81,
            LEGACY_BATCH_LEN - 1,
            LEGACY_BATCH_LEN + 1,
            BATCH_LEN - 1,
            BATCH_LEN + 1,
        ] {
            let mut sized = data.clone();
            sized.resize(len, 0);
            assert_eq!(
                error_code(read_closable_batch(&crate::ID, &sized).unwrap_err()),
                u32::from(ErrorCode::AccountDidNotDeserialize),
                "length {len}"
            );
        }
    }

    #[test]
    fn an_account_without_the_batch_discriminator_is_refused() {
        for byte in 0..8 {
            let mut data = account(&batch_in(BatchState::Complete));
            data[byte] ^= 1;
            assert_eq!(
                error_code(read_closable_batch(&crate::ID, &data).unwrap_err()),
                u32::from(ErrorCode::AccountDiscriminatorMismatch),
                "byte {byte}"
            );
        }
    }

    fn at_its_pda(mut batch: RollBatch) -> (Pubkey, RollBatch) {
        let (key, bump) = Pubkey::find_program_address(
            &[
                BATCH_SEED,
                batch.pool.as_ref(),
                batch.batch_id.to_le_bytes().as_ref(),
            ],
            &crate::ID,
        );
        batch.bump = bump;
        (key, batch)
    }

    #[test]
    fn the_batch_must_sit_at_its_own_pda() {
        let (key, batch) = at_its_pda(batch_in(BatchState::Complete));
        assert!(assert_batch_address(&key, &header_of(&batch)).is_ok());

        let mut other_id = header_of(&batch);
        other_id.batch_id += 1;
        assert!(assert_batch_address(&key, &other_id).is_err());
        let mut other_pool = header_of(&batch);
        other_pool.pool = Pubkey::new_unique();
        assert!(assert_batch_address(&key, &other_pool).is_err());
        assert_eq!(
            error_code(
                assert_batch_address(&Pubkey::new_unique(), &header_of(&batch)).unwrap_err()
            ),
            u32::from(ErrorCode::ConstraintSeeds)
        );
    }

    #[test]
    fn the_roller_or_the_operator_may_close() {
        let batch = batch_in(BatchState::Complete);
        let header = header_of(&batch);
        let operator = Pubkey::new_unique();
        assert!(assert_closer(&batch.roller, &batch.roller, &header, &operator).is_ok());
        assert!(assert_closer(&operator, &batch.roller, &header, &operator).is_ok());
    }

    #[test]
    fn anyone_else_is_refused_and_the_rent_cannot_be_redirected() {
        let batch = batch_in(BatchState::Complete);
        let header = header_of(&batch);
        let operator = Pubkey::new_unique();
        let stranger = Pubkey::new_unique();
        assert_eq!(
            error_code(assert_closer(&stranger, &batch.roller, &header, &operator).unwrap_err()),
            6503
        );
        for signer in [batch.roller, operator] {
            assert_eq!(
                error_code(assert_closer(&signer, &stranger, &header, &operator).unwrap_err()),
                6503,
                "the roller slot must name the batch's roller whoever signs"
            );
            assert_eq!(
                error_code(assert_closer(&signer, &operator, &header, &operator).unwrap_err()),
                6503,
                "the Operator in the roller slot is not the batch's roller"
            );
        }
    }

    // --- structural ---------------------------------------------------------------------------

    /// Every check runs before the event and before a lamport moves; the account is then emptied
    /// to the signer, handed back to the system program and shrunk to nothing.
    #[test]
    fn the_checks_precede_the_close_and_the_close_is_complete() {
        let prod = code(SRC);
        let handler = &prod[prod.find("pub fn handler").unwrap()..];
        let read = handler.find("read_closable_batch(").unwrap();
        let address = handler.find("assert_batch_address(").unwrap();
        let closer = handler.find("assert_closer(").unwrap();
        let emit = handler.find("emit!(BatchClosed").unwrap();
        let credit = handler
            .find("**signer.try_borrow_mut_lamports()? = credited;")
            .unwrap();
        let drain = handler
            .find("**batch.try_borrow_mut_lamports()? = 0;")
            .unwrap();
        let assign = handler.find("batch.assign(&system_program::ID);").unwrap();
        let resize = handler.find("batch.resize(0)?;").unwrap();
        assert!(read < address && address < closer && closer < emit);
        assert!(emit < credit && credit < drain && drain < assign && assign < resize);
        assert!(handler.contains("rent_recipient: signer.key(),"));
        assert!(!handler.contains("roller.try_borrow_mut_lamports"));
    }

    #[test]
    fn the_batch_is_unchecked_and_no_anchor_close_runs() {
        let prod = production(SRC);
        assert_eq!(prod.matches("close = ").count(), 0);
        assert!(prod.contains("pub batch: UncheckedAccount<'info>,"));
        assert!(prod.contains("pub signer: Signer<'info>,"));
        assert_eq!(prod.matches("Signer<'info>").count(), 1);
    }

    /// The runtime lets a program credit only a writable account, so the signer is writable; the
    /// roller stays writable so its entry in the IDL is unchanged.
    #[test]
    fn the_signer_and_the_roller_are_writable() {
        let prod = production(SRC);
        for field in [
            "pub signer: Signer<'info>,",
            "pub roller: UncheckedAccount<'info>,",
        ] {
            let at = prod.find(field).unwrap();
            assert!(
                prod[..at].trim_end().ends_with("#[account(mut)]"),
                "{field} must be preceded by #[account(mut)]"
            );
        }
    }
}
