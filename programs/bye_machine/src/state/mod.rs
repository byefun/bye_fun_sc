pub mod escrow;
pub mod pool;
pub mod pool_collection;
pub mod position;
pub mod protocol_config;
pub mod roll_batch;
pub mod top_tier;
pub mod wallet_stats;
pub mod weight_index;

pub use escrow::*;
pub use pool::*;
pub use pool_collection::*;
pub use position::*;
pub use protocol_config::*;
pub use roll_batch::*;
pub use top_tier::*;
pub use wallet_stats::*;
pub use weight_index::*;

#[cfg(test)]
mod size_tests {
    use super::*;
    use crate::common::constants::FENWICK_CAPACITY;
    use anchor_lang::Space;

    // Each figure is summed from the fields' own byte widths, independently of the derive.
    // `INIT_SPACE` excludes the 8-byte Anchor discriminator; account size = 8 + INIT_SPACE.

    #[test]
    fn protocol_config_size() {
        // 13 Pubkey(32) + t04_ceiling: u64 + reserved_0: [u8; 24] + max_swap_slippage_bps: u16
        // + pool_counter: u16 + bump: u8 — the size it had when t04_ceiling's bytes were a Pubkey.
        assert_eq!(ProtocolConfig::INIT_SPACE, 13 * 32 + 8 + 24 + 2 * 2 + 1);
        assert_eq!(ProtocolConfig::INIT_SPACE, 14 * 32 + 2 * 2 + 1);
    }

    #[test]
    fn pool_size() {
        assert_eq!(
            Pool::INIT_SPACE,
            2               // pool_id: u16
                + 2 * 32    // weight_index, treasury_03: Pubkey
                + 3 * 8     // price, ticket_target, fee: u64
                + 3 * 2     // alloc_equal_bps, alloc_tier_bps, alloc_protocol_bps: u16
                + 3 * 8     // admission_floor, admission_ceiling, wallet_value_cap: u64
                + 1         // max_n: u8
                + 2         // buyback_rate_bps: u16
                + 8 * 8     // blank_faces: [u64; 8]
                + 8         // t04_ceiling: u64
                + 2         // sweep_cadence_hours: u16
                + 1         // tier_size: u8
                + 2         // deposits_paused, rolls_paused: bool
                + 2         // pause_reason: u16
                + 4         // n_real: u32
                + 8 * 4     // blanks: [u32; 8]
                + 16        // w_real: u128
                + 4         // open_batches: u32
                + 2 * 8     // batch_counter, position_counter: u64
                + 2 * 8     // pending_roll_liability, owed_fees: u64
                + 16        // acc_equal: u128
                + 1         // sweep_pending: bool
                + 8         // sweep_epoch: u64
                + 8         // last_sweep_at: i64
                + 4         // sweep_updates: u32
                + 1         // bump: u8
                + 2         // escrow_bid_bps: u16
                + 32        // treasury_04: Pubkey
                + 32 // reserved: [u8; 32]
        );
        assert_eq!(Pool::INIT_SPACE, 394);
        assert_eq!(8 + Pool::INIT_SPACE, 402); // discriminator + struct
    }

    #[test]
    fn pool_collection_size() {
        // pool, collection: Pubkey(32) + standards: u8 + admitted_at: i64 + bump: u8
        assert_eq!(PoolCollection::INIT_SPACE, 2 * 32 + 1 + 8 + 1);
    }

    #[test]
    fn position_state_is_one_byte() {
        assert_eq!(PositionState::INIT_SPACE, 1);
    }

    #[test]
    fn position_size() {
        assert_eq!(
            Position::INIT_SPACE,
            3 * 32          // pool, depositor, nft_mint: Pubkey
                + 8         // position_id: u64
                + 1         // state: PositionState
                + 8         // recorded_value: u64
                + 8         // value_observed_at: i64
                + 8         // deposit_value: u64
                + 4         // slot_index: u32
                + 8         // activated_at: i64
                + 16        // equal_checkpoint: u128
                + 8         // accrued: u64
                + 1         // in_tier: bool
                + 16        // tier_checkpoint: u128
                + 8         // lock_until: i64
                + 2         // reject_reason: u16
                + 1         // standard: u8
                + 1         // bump: u8
                + 1 // vault_bump: u8
        );
    }

    #[test]
    fn escrow_size() {
        // owner: Pubkey + armed: bool + bump, vault_bump: u8 (2) + reserved: [u8; 64]
        assert_eq!(Escrow::INIT_SPACE, 32 + 1 + 2 + 64);
        assert_eq!(8 + Escrow::INIT_SPACE, 107); // discriminator + struct
    }

    #[test]
    fn wallet_stats_size() {
        // pool, owner: Pubkey + active_positions: u32 + active_value: u64 + bump: u8
        assert_eq!(WalletStats::INIT_SPACE, 2 * 32 + 4 + 8 + 1);
    }

    #[test]
    fn tier_entry_size() {
        // position: Pubkey + value: u64 + activated_at: i64 + position_id: u64
        assert_eq!(TierEntry::INIT_SPACE, 32 + 8 + 8 + 8);
    }

    #[test]
    fn top_tier_size() {
        // pool: Pubkey + len: u8 + entries: [TierEntry; 20] + acc_tier: u128 + bump: u8
        assert_eq!(
            TopTier::INIT_SPACE,
            32 + 1 + 20 * TierEntry::INIT_SPACE + 16 + 1
        );
    }

    #[test]
    fn round_two_enums_are_one_byte_each() {
        assert_eq!(BatchState::INIT_SPACE, 1);
        assert_eq!(RollStatus::INIT_SPACE, 1);
        assert_eq!(RollOutcome::INIT_SPACE, 1);
    }

    #[test]
    fn roll_record_size() {
        // status: RollStatus + outcome: RollOutcome + selected_mint: Pubkey
        // + blank_face_idx: u8 + draw_value: u128 + attempts: u8
        // + w_at_draw: u128 + n_real_at_draw: u32 + blanks_at_draw: [u32; 8]
        // + refund_reason: u16
        assert_eq!(
            RollRecord::INIT_SPACE,
            1 + 1 + 32 + 1 + 16 + 1 + 16 + 4 + 8 * 4 + 2
        );
        assert_eq!(RollRecord::INIT_SPACE, 106);
    }

    #[test]
    fn roll_batch_size() {
        // pool + roller: Pubkey + batch_id: u64
        const IDENTITY: usize = 32 + 32 + 8;
        // state: BatchState + n + next_roll + settled + refunded: u8
        const PROGRESS: usize = 1 + 4;
        // request_slot: u64 + caller_seed: [u8; 32] + randomness: [u8; 32]
        const VRF: usize = 8 + 32 + 32;
        // expected_w: u128 + expected_blanks: [u32; 8] — the batch's running expectation
        const EXPECTATION: usize = 16 + 8 * 4;
        // s_price + s_ticket + s_fee: u64 + s_alloc_bps: [u16; 3] + s_blank_faces: [u64; 8].
        // No s_W — the draw reduces over live W.
        const TERMS: usize = 8 + 8 + 8 + 3 * 2 + 8 * 8;
        const ROLLS: usize = 10 * RollRecord::INIT_SPACE;
        const BUMP: usize = 1;

        assert_eq!(
            RollBatch::INIT_SPACE,
            IDENTITY + PROGRESS + VRF + EXPECTATION + TERMS + ROLLS + BUMP
        );
        assert_eq!(RollBatch::INIT_SPACE, 1_352);
        assert_eq!(8 + RollBatch::INIT_SPACE, 1_360); // discriminator + struct
    }

    #[test]
    fn weight_index_size() {
        const HEADER: usize = 32 // pool: Pubkey
            + 8                  // total_weight: u64
            + 4                  // high_water: u32
            + 4                  // free_count: u32
            + 8; // _padding: [u8; 8]
        const TREE: usize = 8 * FENWICK_CAPACITY; // tree: [u64; FENWICK_CAPACITY]
        const FREE_STACK: usize = 4 * FENWICK_CAPACITY; // free_stack: [u32; FENWICK_CAPACITY]
        const STRUCT_SIZE: usize = HEADER + TREE + FREE_STACK;

        assert_eq!(std::mem::size_of::<WeightIndex>(), STRUCT_SIZE);
        assert_eq!(WeightIndex::INIT_SPACE, STRUCT_SIZE);
        assert_eq!(STRUCT_SIZE, 196_664);

        const ACCOUNT_SIZE: usize = 8 + STRUCT_SIZE; // discriminator + struct
        assert_eq!(ACCOUNT_SIZE, 196_672);
    }
}

/// A struct's field order is the on-chain byte layout. Swapping two same-typed fields (e.g.
/// `ProtocolConfig`'s `administrator` ⇄ `operator`) compiles clean, keeps the size, and
/// silently reinterprets every deployed account.
///
/// Each test gives every field a distinct value, serializes with `try_to_vec()`, and walks the
/// bytes with a cursor in the declared field order. The final `is_empty()` assert fails if a
/// field is added without extending the test.
#[cfg(test)]
mod layout_tests {
    use super::*;
    use anchor_lang::prelude::*;

    fn take<'a>(cursor: &mut &'a [u8], n: usize) -> &'a [u8] {
        let (head, tail) = cursor.split_at(n);
        *cursor = tail;
        head
    }
    fn take_pubkey(cursor: &mut &[u8]) -> [u8; 32] {
        take(cursor, 32).try_into().unwrap()
    }
    fn take_u8(cursor: &mut &[u8]) -> u8 {
        take(cursor, 1)[0]
    }
    fn take_bool(cursor: &mut &[u8]) -> bool {
        take_u8(cursor) != 0
    }
    fn take_u16(cursor: &mut &[u8]) -> u16 {
        u16::from_le_bytes(take(cursor, 2).try_into().unwrap())
    }
    fn take_u32(cursor: &mut &[u8]) -> u32 {
        u32::from_le_bytes(take(cursor, 4).try_into().unwrap())
    }
    fn take_u64(cursor: &mut &[u8]) -> u64 {
        u64::from_le_bytes(take(cursor, 8).try_into().unwrap())
    }
    fn take_i64(cursor: &mut &[u8]) -> i64 {
        i64::from_le_bytes(take(cursor, 8).try_into().unwrap())
    }
    fn take_u128(cursor: &mut &[u8]) -> u128 {
        u128::from_le_bytes(take(cursor, 16).try_into().unwrap())
    }

    #[test]
    fn protocol_config_field_order() {
        let administrator = Pubkey::new_unique();
        let t04_ceiling: u64 = 0x0102_0304_0506_0708;
        let mut reserved_0 = [0u8; 24];
        for (i, byte) in reserved_0.iter_mut().enumerate() {
            *byte = 0x40 | i as u8;
        }
        let reserved_1 = Pubkey::new_unique();
        let reserved_2 = Pubkey::new_unique();
        let reserved_3 = Pubkey::new_unique();
        let operator = Pubkey::new_unique();
        let t05_authority = Pubkey::new_unique();
        let t02_authority = Pubkey::new_unique();
        let protocol_revenue = Pubkey::new_unique();
        let usdc_mint = Pubkey::new_unique();
        let bye_mint = Pubkey::new_unique();
        let vrf_program = Pubkey::new_unique();
        let oracle_queue = Pubkey::new_unique();
        let swap_pool = Pubkey::new_unique();
        let max_swap_slippage_bps: u16 = 11_111;
        let pool_counter: u16 = 22_222;
        let bump: u8 = 33;

        let config = ProtocolConfig {
            administrator,
            t04_ceiling,
            reserved_0,
            reserved_1,
            reserved_2,
            reserved_3,
            operator,
            t05_authority,
            t02_authority,
            protocol_revenue,
            usdc_mint,
            bye_mint,
            vrf_program,
            oracle_queue,
            swap_pool,
            max_swap_slippage_bps,
            pool_counter,
            bump,
        };
        let bytes = config.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(
            take_pubkey(&mut cursor),
            administrator.to_bytes(),
            "administrator moved"
        );
        assert_eq!(take_u64(&mut cursor), t04_ceiling, "t04_ceiling moved");
        assert_eq!(
            take(&mut cursor, 24),
            reserved_0.as_slice(),
            "reserved_0 moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            reserved_1.to_bytes(),
            "reserved_1 moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            reserved_2.to_bytes(),
            "reserved_2 moved — the reserved slots hold the deployed layout"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            reserved_3.to_bytes(),
            "reserved_3 moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            operator.to_bytes(),
            "operator moved — the exact administrator/operator swap this pin closes"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            t05_authority.to_bytes(),
            "t05_authority moved — Treasury 05 reuses Treasury 03's former byte positions, so this pin holds the deployed layout"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            t02_authority.to_bytes(),
            "t02_authority moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            protocol_revenue.to_bytes(),
            "protocol_revenue moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            usdc_mint.to_bytes(),
            "usdc_mint moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            bye_mint.to_bytes(),
            "bye_mint moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            vrf_program.to_bytes(),
            "vrf_program moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            oracle_queue.to_bytes(),
            "oracle_queue moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            swap_pool.to_bytes(),
            "swap_pool moved"
        );
        assert_eq!(
            take_u16(&mut cursor),
            max_swap_slippage_bps,
            "max_swap_slippage_bps moved"
        );
        assert_eq!(take_u16(&mut cursor), pool_counter, "pool_counter moved");
        assert_eq!(take_u8(&mut cursor), bump, "bump moved");
        assert!(
            cursor.is_empty(),
            "ProtocolConfig grew a field this pin does not account for — extend it"
        );
    }

    #[test]
    fn pool_field_order() {
        let pool_id: u16 = 1;
        let weight_index = Pubkey::new_unique();
        let treasury_03 = Pubkey::new_unique();
        let price: u64 = 2;
        let ticket_target: u64 = 3;
        let fee: u64 = 4;
        let alloc_equal_bps: u16 = 5;
        let alloc_tier_bps: u16 = 6;
        let alloc_protocol_bps: u16 = 7;
        let admission_floor: u64 = 8;
        let admission_ceiling: u64 = 9;
        let wallet_value_cap: u64 = 10;
        let max_n: u8 = 11;
        let buyback_rate_bps: u16 = 12;
        let blank_faces: [u64; 8] = [1_301, 1_302, 1_303, 1_304, 1_305, 1_306, 1_307, 1_308];
        let t04_ceiling: u64 = 16;
        let sweep_cadence_hours: u16 = 17;
        let tier_size: u8 = 18;
        let pause_reason: u16 = 19;
        let n_real: u32 = 20;
        let blanks: [u32; 8] = [2_101, 2_102, 2_103, 2_104, 2_105, 2_106, 2_107, 2_108];
        let w_real: u128 = 24;
        let open_batches: u32 = 25;
        let batch_counter: u64 = 26;
        let position_counter: u64 = 27;
        let pending_roll_liability: u64 = 28;
        let owed_fees: u64 = 29;
        let acc_equal: u128 = 30;
        let sweep_epoch: u64 = 31;
        let last_sweep_at: i64 = 32;
        let sweep_updates: u32 = 33;
        let bump: u8 = 34;
        let escrow_bid_bps: u16 = 35;
        let treasury_04 = Pubkey::new_unique();
        let mut reserved = [0u8; 32];
        for (i, byte) in reserved.iter_mut().enumerate() {
            *byte = 0x80 | i as u8;
        }

        // `Pool` carries three `bool`s, which cannot take distinct sentinels. One-hot over the
        // three makes any swap among them move a `true` into a position expecting `false`.
        for (deposits_paused, rolls_paused, sweep_pending) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let pool = Pool {
                pool_id,
                weight_index,
                treasury_03,
                price,
                ticket_target,
                fee,
                alloc_equal_bps,
                alloc_tier_bps,
                alloc_protocol_bps,
                admission_floor,
                admission_ceiling,
                wallet_value_cap,
                max_n,
                buyback_rate_bps,
                blank_faces,
                t04_ceiling,
                sweep_cadence_hours,
                tier_size,
                deposits_paused,
                rolls_paused,
                pause_reason,
                n_real,
                blanks,
                w_real,
                open_batches,
                batch_counter,
                position_counter,
                pending_roll_liability,
                owed_fees,
                acc_equal,
                sweep_pending,
                sweep_epoch,
                last_sweep_at,
                sweep_updates,
                bump,
                escrow_bid_bps,
                treasury_04,
                reserved,
            };
            let bytes = pool.try_to_vec().unwrap();
            let mut cursor = bytes.as_slice();

            assert_eq!(take_u16(&mut cursor), pool_id, "pool_id moved");
            assert_eq!(
                take_pubkey(&mut cursor),
                weight_index.to_bytes(),
                "weight_index moved"
            );
            assert_eq!(
                take_pubkey(&mut cursor),
                treasury_03.to_bytes(),
                "treasury_03 moved"
            );
            assert_eq!(take_u64(&mut cursor), price, "price moved");
            assert_eq!(take_u64(&mut cursor), ticket_target, "ticket_target moved");
            assert_eq!(take_u64(&mut cursor), fee, "fee moved");
            assert_eq!(
                take_u16(&mut cursor),
                alloc_equal_bps,
                "alloc_equal_bps moved"
            );
            assert_eq!(
                take_u16(&mut cursor),
                alloc_tier_bps,
                "alloc_tier_bps moved"
            );
            assert_eq!(
                take_u16(&mut cursor),
                alloc_protocol_bps,
                "alloc_protocol_bps moved"
            );
            assert_eq!(
                take_u64(&mut cursor),
                admission_floor,
                "admission_floor moved"
            );
            assert_eq!(
                take_u64(&mut cursor),
                admission_ceiling,
                "admission_ceiling moved"
            );
            assert_eq!(
                take_u64(&mut cursor),
                wallet_value_cap,
                "wallet_value_cap moved"
            );
            assert_eq!(take_u8(&mut cursor), max_n, "max_n moved");
            assert_eq!(
                take_u16(&mut cursor),
                buyback_rate_bps,
                "buyback_rate_bps moved"
            );
            assert_eq!(
                std::array::from_fn::<u64, 8, _>(|_| take_u64(&mut cursor)),
                blank_faces,
                "blank_faces moved"
            );
            assert_eq!(take_u64(&mut cursor), t04_ceiling, "t04_ceiling moved");
            assert_eq!(
                take_u16(&mut cursor),
                sweep_cadence_hours,
                "sweep_cadence_hours moved"
            );
            assert_eq!(take_u8(&mut cursor), tier_size, "tier_size moved");
            assert_eq!(
                take_bool(&mut cursor),
                deposits_paused,
                "deposits_paused moved"
            );
            assert_eq!(take_bool(&mut cursor), rolls_paused, "rolls_paused moved");
            assert_eq!(take_u16(&mut cursor), pause_reason, "pause_reason moved");
            assert_eq!(take_u32(&mut cursor), n_real, "n_real moved");
            assert_eq!(
                std::array::from_fn::<u32, 8, _>(|_| take_u32(&mut cursor)),
                blanks,
                "blanks moved"
            );
            assert_eq!(take_u128(&mut cursor), w_real, "w_real moved");
            assert_eq!(take_u32(&mut cursor), open_batches, "open_batches moved");
            assert_eq!(take_u64(&mut cursor), batch_counter, "batch_counter moved");
            assert_eq!(
                take_u64(&mut cursor),
                position_counter,
                "position_counter moved"
            );
            assert_eq!(
                take_u64(&mut cursor),
                pending_roll_liability,
                "pending_roll_liability moved"
            );
            assert_eq!(take_u64(&mut cursor), owed_fees, "owed_fees moved");
            assert_eq!(take_u128(&mut cursor), acc_equal, "acc_equal moved");
            assert_eq!(take_bool(&mut cursor), sweep_pending, "sweep_pending moved");
            assert_eq!(take_u64(&mut cursor), sweep_epoch, "sweep_epoch moved");
            assert_eq!(take_i64(&mut cursor), last_sweep_at, "last_sweep_at moved");
            assert_eq!(take_u32(&mut cursor), sweep_updates, "sweep_updates moved");
            assert_eq!(take_u8(&mut cursor), bump, "bump moved");
            assert_eq!(
                take_u16(&mut cursor),
                escrow_bid_bps,
                "escrow_bid_bps moved"
            );
            assert_eq!(
                take_pubkey(&mut cursor),
                treasury_04.to_bytes(),
                "treasury_04 moved"
            );
            assert_eq!(take(&mut cursor, 32), reserved.as_slice(), "reserved moved");
            assert!(
                cursor.is_empty(),
                "Pool grew a field this pin does not account for — extend it"
            );
        }
    }

    #[test]
    fn position_field_order() {
        let pool = Pubkey::new_unique();
        let depositor = Pubkey::new_unique();
        let nft_mint = Pubkey::new_unique();
        let position_id: u64 = 1;
        let state = PositionState::ClosedBelowFloor;
        let recorded_value: u64 = 2;
        let value_observed_at: i64 = 3;
        let deposit_value: u64 = 4;
        let slot_index: u32 = 5;
        let activated_at: i64 = 6;
        let equal_checkpoint: u128 = 7;
        let accrued: u64 = 8;
        let in_tier = true;
        let tier_checkpoint: u128 = 9;
        let lock_until: i64 = 10;
        let reject_reason: u16 = 11;
        let standard: u8 = 2;
        let bump: u8 = 12;
        let vault_bump: u8 = 13;

        let position = Position {
            pool,
            depositor,
            nft_mint,
            position_id,
            state,
            recorded_value,
            value_observed_at,
            deposit_value,
            slot_index,
            activated_at,
            equal_checkpoint,
            accrued,
            in_tier,
            tier_checkpoint,
            lock_until,
            reject_reason,
            standard,
            bump,
            vault_bump,
        };
        let bytes = position.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(
            take_pubkey(&mut cursor),
            depositor.to_bytes(),
            "depositor moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            nft_mint.to_bytes(),
            "nft_mint moved"
        );
        assert_eq!(take_u64(&mut cursor), position_id, "position_id moved");
        assert_eq!(
            take_u8(&mut cursor),
            2,
            "state moved — PositionState::ClosedBelowFloor's own byte"
        );
        assert_eq!(
            take_u64(&mut cursor),
            recorded_value,
            "recorded_value moved"
        );
        assert_eq!(
            take_i64(&mut cursor),
            value_observed_at,
            "value_observed_at moved"
        );
        assert_eq!(take_u64(&mut cursor), deposit_value, "deposit_value moved");
        assert_eq!(take_u32(&mut cursor), slot_index, "slot_index moved");
        assert_eq!(take_i64(&mut cursor), activated_at, "activated_at moved");
        assert_eq!(
            take_u128(&mut cursor),
            equal_checkpoint,
            "equal_checkpoint moved"
        );
        assert_eq!(take_u64(&mut cursor), accrued, "accrued moved");
        assert_eq!(take_bool(&mut cursor), in_tier, "in_tier moved");
        assert_eq!(
            take_u128(&mut cursor),
            tier_checkpoint,
            "tier_checkpoint moved"
        );
        assert_eq!(take_i64(&mut cursor), lock_until, "lock_until moved");
        assert_eq!(take_u16(&mut cursor), reject_reason, "reject_reason moved");
        assert_eq!(take_u8(&mut cursor), standard, "standard moved");
        assert_eq!(take_u8(&mut cursor), bump, "bump moved");
        assert_eq!(take_u8(&mut cursor), vault_bump, "vault_bump moved");
        assert!(
            cursor.is_empty(),
            "Position grew a field this pin does not account for — extend it"
        );
    }

    /// `standards` and `bump` are both `u8`, so they hold distinct values other than `0`/`1`
    /// to make a swap between them visible.
    #[test]
    fn pool_collection_field_order() {
        let pool = Pubkey::new_unique();
        let collection = Pubkey::new_unique();
        let standards: u8 = 0b101;
        let admitted_at: i64 = 1_700_000_000;
        let bump: u8 = 254;

        let record = PoolCollection {
            pool,
            collection,
            standards,
            admitted_at,
            bump,
        };
        let bytes = record.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(
            take_pubkey(&mut cursor),
            collection.to_bytes(),
            "collection moved"
        );
        assert_eq!(take_u8(&mut cursor), standards, "standards moved");
        assert_eq!(take_i64(&mut cursor), admitted_at, "admitted_at moved");
        assert_eq!(take_u8(&mut cursor), bump, "bump moved");
        assert!(
            cursor.is_empty(),
            "PoolCollection grew a field this pin does not account for — extend it"
        );
    }

    /// `armed` is the only `bool`, and `bump` and `vault_bump` hold distinct values other than
    /// `0`/`1`, so no swap among the three one-byte fields survives. `reserved` carries a
    /// non-zero pattern of its own, so a field moved into or out of it is visible too.
    #[test]
    fn escrow_field_order() {
        let owner = Pubkey::new_unique();
        let armed = true;
        let bump: u8 = 254;
        let vault_bump: u8 = 253;
        let mut reserved = [0u8; 64];
        for (i, byte) in reserved.iter_mut().enumerate() {
            *byte = 0x80 | i as u8;
        }

        let escrow = Escrow {
            owner,
            armed,
            bump,
            vault_bump,
            reserved,
        };
        let bytes = escrow.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), owner.to_bytes(), "owner moved");
        assert_eq!(take_bool(&mut cursor), armed, "armed moved");
        assert_eq!(take_u8(&mut cursor), bump, "bump moved");
        assert_eq!(take_u8(&mut cursor), vault_bump, "vault_bump moved");
        assert_eq!(take(&mut cursor, 64), reserved.as_slice(), "reserved moved");
        assert!(
            cursor.is_empty(),
            "Escrow grew a field this pin does not account for — extend it"
        );
    }

    #[test]
    fn wallet_stats_field_order() {
        let pool = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let active_positions: u32 = 1;
        let active_value: u64 = 2;
        let bump: u8 = 3;

        let stats = WalletStats {
            pool,
            owner,
            active_positions,
            active_value,
            bump,
        };
        let bytes = stats.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_pubkey(&mut cursor), owner.to_bytes(), "owner moved");
        assert_eq!(
            take_u32(&mut cursor),
            active_positions,
            "active_positions moved"
        );
        assert_eq!(take_u64(&mut cursor), active_value, "active_value moved");
        assert_eq!(take_u8(&mut cursor), bump, "bump moved");
        assert!(
            cursor.is_empty(),
            "WalletStats grew a field this pin does not account for — extend it"
        );
    }

    /// `TopTier.entries` is `[TierEntry; 20]` inlined into the account, so `TierEntry`'s field
    /// order is part of this layout. `entries[0]` carries distinct sentinels; `entries[1..20]`
    /// are all-zero, so nothing outside the 56-byte stride may have moved.
    #[test]
    fn top_tier_field_order() {
        let pool = Pubkey::new_unique();
        let len: u8 = 1;
        let entry_position = Pubkey::new_unique();
        let entry_value: u64 = 111;
        let entry_activated_at: i64 = 222;
        let entry_position_id: u64 = 333;
        let acc_tier: u128 = 444;
        let bump: u8 = 5;

        let zero_entry = TierEntry {
            position: Pubkey::default(),
            value: 0,
            activated_at: 0,
            position_id: 0,
        };
        let mut entries = [zero_entry; 20];
        entries[0] = TierEntry {
            position: entry_position,
            value: entry_value,
            activated_at: entry_activated_at,
            position_id: entry_position_id,
        };

        let top_tier = TopTier {
            pool,
            len,
            entries,
            acc_tier,
            bump,
        };
        let bytes = top_tier.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u8(&mut cursor), len, "len moved");
        assert_eq!(
            take_pubkey(&mut cursor),
            entry_position.to_bytes(),
            "entries[0].position moved"
        );
        assert_eq!(take_u64(&mut cursor), entry_value, "entries[0].value moved");
        assert_eq!(
            take_i64(&mut cursor),
            entry_activated_at,
            "entries[0].activated_at moved"
        );
        assert_eq!(
            take_u64(&mut cursor),
            entry_position_id,
            "entries[0].position_id moved"
        );
        let remaining_entries_bytes = 56 * 19; // entries[1..20], TierEntry::INIT_SPACE each
        assert_eq!(
            take(&mut cursor, remaining_entries_bytes),
            vec![0u8; remaining_entries_bytes].as_slice(),
            "entries[1..20] carried a non-zero byte — something outside entries[0]'s 56-byte \
             stride moved"
        );
        assert_eq!(take_u128(&mut cursor), acc_tier, "acc_tier moved");
        assert_eq!(take_u8(&mut cursor), bump, "bump moved");
        assert!(
            cursor.is_empty(),
            "TopTier grew a field this pin does not account for — extend it"
        );
    }

    /// A fieldless enum serializes as its declaration index, so the wire byte is fixed by
    /// variant order. Asserting the byte (not a symbolic round trip) fails on any reorder or
    /// mid-list insert; variants may only be appended at the end.
    #[test]
    fn round_two_enum_discriminants_are_their_declaration_order() {
        assert_eq!(BatchState::Committed.try_to_vec().unwrap(), vec![0]);
        assert_eq!(BatchState::Resolved.try_to_vec().unwrap(), vec![1]);
        assert_eq!(BatchState::Recovered.try_to_vec().unwrap(), vec![2]);
        assert_eq!(BatchState::Complete.try_to_vec().unwrap(), vec![3]);

        assert_eq!(RollStatus::Pending.try_to_vec().unwrap(), vec![0]);
        assert_eq!(RollStatus::Settled.try_to_vec().unwrap(), vec![1]);
        assert_eq!(RollStatus::Refunded.try_to_vec().unwrap(), vec![2]);

        assert_eq!(RollOutcome::None.try_to_vec().unwrap(), vec![0]);
        assert_eq!(RollOutcome::RealCard.try_to_vec().unwrap(), vec![1]);
        assert_eq!(RollOutcome::Blank.try_to_vec().unwrap(), vec![2]);
    }

    /// `rolls` is ten inlined `RollRecord`s, so its field order is part of `RollBatch`'s layout.
    /// `rolls[0]` carries distinct sentinels and `rolls[1..10]` is asserted all-zero.
    /// Adjacent same-width fields (the `u8` counters, the one-byte enums, the `u128` draw
    /// values) get distinct values so a swap between them is visible.
    #[test]
    fn roll_batch_field_order() {
        let pool = Pubkey::new_unique();
        let roller = Pubkey::new_unique();
        let batch_id: u64 = 7_001;
        let n: u8 = 9;
        let next_roll: u8 = 4;
        let settled: u8 = 3;
        let refunded: u8 = 1;
        let request_slot: u64 = 77_777;
        let caller_seed = [0x11u8; 32];
        let randomness = [0x22u8; 32];
        let expected_w: u128 = 123_456_789;
        let expected_blanks: [u32; 8] = [41, 42, 43, 44, 45, 46, 47, 48];
        let s_price: u64 = 9_900_000;
        let s_ticket: u64 = 9_000_000;
        let s_fee: u64 = 900_000;
        let s_alloc_bps: [u16; 3] = [7_500, 500, 2_000];
        let s_blank_faces: [u64; 8] =
            [1_000_000, 2_000_000, 5_000_000, 100_000_000, 51, 52, 53, 54];
        let bump: u8 = 254;

        let selected_mint = Pubkey::new_unique();
        let blank_face_idx: u8 = 2;
        let draw_value: u128 = 555_555;
        let attempts: u8 = 3;
        let w_at_draw: u128 = 666_666;
        let n_real_at_draw: u32 = 12;
        let blanks_at_draw: [u32; 8] = [21, 22, 23, 24, 25, 26, 27, 28];
        let refund_reason: u16 = 6_205;

        let zero_roll = RollRecord {
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
        };
        let mut rolls = [zero_roll; 10];
        rolls[0] = RollRecord {
            status: RollStatus::Settled,
            outcome: RollOutcome::Blank,
            selected_mint,
            blank_face_idx,
            draw_value,
            attempts,
            w_at_draw,
            n_real_at_draw,
            blanks_at_draw,
            refund_reason,
        };

        let batch = RollBatch {
            pool,
            roller,
            batch_id,
            state: BatchState::Recovered,
            n,
            next_roll,
            settled,
            refunded,
            request_slot,
            caller_seed,
            randomness,
            expected_w,
            expected_blanks,
            s_price,
            s_ticket,
            s_fee,
            s_alloc_bps,
            s_blank_faces,
            rolls,
            bump,
        };
        let bytes = batch.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_pubkey(&mut cursor), roller.to_bytes(), "roller moved");
        assert_eq!(take_u64(&mut cursor), batch_id, "batch_id moved");
        assert_eq!(take_u8(&mut cursor), 2, "state moved (Recovered == 2)");
        assert_eq!(take_u8(&mut cursor), n, "n moved");
        assert_eq!(take_u8(&mut cursor), next_roll, "next_roll moved");
        assert_eq!(take_u8(&mut cursor), settled, "settled moved");
        assert_eq!(take_u8(&mut cursor), refunded, "refunded moved");
        assert_eq!(take_u64(&mut cursor), request_slot, "request_slot moved");
        assert_eq!(take(&mut cursor, 32), caller_seed, "caller_seed moved");
        assert_eq!(take(&mut cursor, 32), randomness, "randomness moved");
        assert_eq!(take_u128(&mut cursor), expected_w, "expected_w moved");
        for (i, expected) in expected_blanks.iter().enumerate() {
            assert_eq!(
                take_u32(&mut cursor),
                *expected,
                "expected_blanks[{i}] moved"
            );
        }
        assert_eq!(take_u64(&mut cursor), s_price, "s_price moved");
        assert_eq!(take_u64(&mut cursor), s_ticket, "s_ticket moved");
        assert_eq!(take_u64(&mut cursor), s_fee, "s_fee moved");
        for (i, expected) in s_alloc_bps.iter().enumerate() {
            assert_eq!(take_u16(&mut cursor), *expected, "s_alloc_bps[{i}] moved");
        }
        for (i, expected) in s_blank_faces.iter().enumerate() {
            assert_eq!(take_u64(&mut cursor), *expected, "s_blank_faces[{i}] moved");
        }

        // rolls[0] — RollRecord's own declared order, inlined
        assert_eq!(
            take_u8(&mut cursor),
            1,
            "rolls[0].status moved (Settled == 1)"
        );
        assert_eq!(
            take_u8(&mut cursor),
            2,
            "rolls[0].outcome moved (Blank == 2)"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            selected_mint.to_bytes(),
            "rolls[0].selected_mint moved"
        );
        assert_eq!(
            take_u8(&mut cursor),
            blank_face_idx,
            "rolls[0].blank_face_idx moved"
        );
        assert_eq!(
            take_u128(&mut cursor),
            draw_value,
            "rolls[0].draw_value moved"
        );
        assert_eq!(take_u8(&mut cursor), attempts, "rolls[0].attempts moved");
        assert_eq!(
            take_u128(&mut cursor),
            w_at_draw,
            "rolls[0].w_at_draw moved"
        );
        assert_eq!(
            take_u32(&mut cursor),
            n_real_at_draw,
            "rolls[0].n_real_at_draw moved"
        );
        for (i, expected) in blanks_at_draw.iter().enumerate() {
            assert_eq!(
                take_u32(&mut cursor),
                *expected,
                "rolls[0].blanks_at_draw[{i}] moved"
            );
        }
        assert_eq!(
            take_u16(&mut cursor),
            refund_reason,
            "rolls[0].refund_reason moved"
        );

        let remaining_rolls_bytes = 106 * 9; // rolls[1..10], RollRecord::INIT_SPACE each
        assert_eq!(
            take(&mut cursor, remaining_rolls_bytes),
            vec![0u8; remaining_rolls_bytes].as_slice(),
            "rolls[1..10] carried a non-zero byte — something outside rolls[0]'s 106-byte stride \
             moved"
        );
        assert_eq!(take_u8(&mut cursor), bump, "bump moved");
        assert!(
            cursor.is_empty(),
            "RollBatch grew a field this pin does not account for — extend it"
        );
    }
}
