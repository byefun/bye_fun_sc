use anchor_lang::prelude::*;

use crate::state::{PositionState, RollOutcome};

/// A single scalar `Pool`/`ProtocolConfig` field value, old or new, as carried by `ConfigChanged`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub enum ConfigValue {
    Amount(u64),
    Bps(u16),
    Count(u8),
    Faces([u64; 3]),
    Hours(u16),
}

/// The observed cause of a third-party seizure, as `close_seized` established it.
///
/// `Burned` and `Transferred` are both *gone*; `Frozen` and `Undecidable` both leave the card
/// present and in the vault, and the four are separate variants because the remedies differ — a
/// burn cannot be undone, while a freeze, a transfer-out and a plugin holding up the move are all
/// reversible by the authority that caused them.
///
/// **`Undecidable` is the open-ended one, and it exists so the set does not have to be.** The
/// other three are facts: the account is gone, the owner is not the vault, a freeze flag is set.
/// This one is the absence of a fact — the asset's or its collection's plugin registry holds a
/// record whose effect on the `Transfer` lifecycle **this program cannot decide**: a `Royalties`
/// rule set that is an allow list or a deny list, an external `Oracle` or `LifecycleHook` adapter
/// that declares a Transfer check, an **asset-level** `BubblegumV2` plugin, or a `plugin_type`
/// byte a later `mpl-core` ships and this build has never heard of.
///
/// **Each of those is narrower than the plugin it names, and the narrowing was paid for.** A
/// `Royalties` record with `rule_set: None` and a `BubblegumV2` record on a *collection* are both
/// `Clear`; classifying either by plugin type put every card in the production cohort on this
/// cause, where `close_seized` is permissionless. It is deliberately *not* a claim that the transfer will fail; it is the statement
/// that nothing here can promise it will succeed.
///
/// **Which is why it is the one cause both of `core_asset.rs`'s gates admit.** The exits proceed
/// on it and get MPL Core's own verdict; `close_seized` accepts it as a cause. Enumerating
/// *decidable* gating mechanisms one at a time always leaves a position able to be refused by
/// both gates at once — unwithdrawable and uncloseable, holding live
/// `w_real` behind a card nobody can move. Enumerating the **inert** mechanisms instead and
/// defaulting the rest to this variant is what avoids that.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum SeizureKind {
    Burned,
    Transferred,
    Frozen,
    Undecidable,
}

/// **Every `SeizureKind`, in declaration order — the list this crate's sweeps over the cause
/// iterate instead of writing their own**, on `ALL_POSITION_STATES`'s reasoning and after the
/// same defect: three hand-listed sweeps in this crate still read four `PositionState`s after the
/// fifth arrived. `seizure_kind_rejects_out_of_range_discriminant` reds the moment a variant is
/// added without being listed here.
///
/// Test-only: nothing on chain enumerates the causes, and a `const` the program never reads is
/// weight in the `.so`.
#[cfg(test)]
pub const ALL_SEIZURE_KINDS: [SeizureKind; 4] = [
    SeizureKind::Burned,
    SeizureKind::Transferred,
    SeizureKind::Frozen,
    SeizureKind::Undecidable,
];

impl SeizureKind {
    /// Whether a depositor could **arrange this cause and keep the card** — which is what
    /// `close_seized`'s lock gate turns on, and the reason the predicate lives beside the enum
    /// rather than in that handler.
    ///
    /// `close_seized` is permissionless, so a depositor may call it on their own position. On
    /// any cause they can bring about while still ending up with the card, the call is a way to
    /// take weight out of the pool before `season.tge_at + 14 days` — the Season-0 lock
    /// dissolved by its own holder. So the gate asks about the depositor's position after the
    /// fact, not about where the card currently sits.
    ///
    /// **`Burned` is the only cause that answers no**, and it answers no because the card is
    /// destroyed: there is nothing to collude for, and a depositor who burns their own
    /// collateral to leave early has paid more than the lock was worth. The other three all end
    /// with the card recoverable — `Frozen` and `Undecidable` leave it in the vault behind a
    /// plugin the authority that imposed it can lift, and `Transferred` puts it somewhere the
    /// party that moved it chose, which on a collection carrying a
    /// `PermanentTransferDelegate` is wherever the depositor and that authority agree.
    ///
    /// **The cost of the `Transferred` row is real and falls on the honest case.** A depositor
    /// whose card is genuinely stolen keeps weight in the pool until their lock expires, and the
    /// pool pays draw odds and fee accrual on a card it no longer holds for that long. It is
    /// accepted because the alternative is a lock that any depositor whose collection authority
    /// will cooperate can step out of, which is not a lock. Narrowing the row to
    /// `owner_observed == depositor` was considered and rejected: a colluding depositor names
    /// any other wallet they control, so it would buy the appearance of a gate and not a gate.
    ///
    /// Wildcard-free: a fifth cause has to choose a side here rather than inherit one.
    pub fn depositor_may_keep_the_card(self) -> bool {
        match self {
            SeizureKind::Burned => false,
            SeizureKind::Transferred | SeizureKind::Frozen | SeizureKind::Undecidable => true,
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityRole {
    Administrator,
    Operator,
    T03,
    T02,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum RotationPhase {
    Proposed,
    Accepted,
}

#[event]
pub struct ProtocolInitialized {
    pub slot: u64,
    pub administrator: Pubkey,
    pub operator: Pubkey,
    pub t02_authority: Pubkey,
    pub t03_authority: Pubkey,
    pub protocol_revenue: Pubkey,
    pub usdc_mint: Pubkey,
    pub bye_mint: Pubkey,
    pub vrf_program: Pubkey,
    pub oracle_queue: Pubkey,
    pub swap_pool: Pubkey,
    pub max_swap_slippage_bps: u16,
}

#[event]
pub struct PoolInitialized {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub pool_id: u16,
    pub weight_index: Pubkey,
    pub treasury_03: Pubkey,
    pub price: u64,
    pub ticket_target: u64,
    pub fee: u64,
    pub alloc_equal_bps: u16,
    pub alloc_tier_bps: u16,
    pub alloc_protocol_bps: u16,
    pub admission_floor: u64,
    pub admission_ceiling: u64,
    pub wallet_value_cap: u64,
    pub max_n: u8,
    pub buyback_rate_bps: u16,
    pub blank_faces: [u64; 3],
    pub t04_ceiling: u64,
    pub sweep_cadence_hours: u16,
    pub tier_size: u8,
}

#[event]
pub struct ConfigChanged {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub param: String,
    pub old: ConfigValue,
    pub new: ConfigValue,
}

/// The three fields move together rather than through `ConfigChanged`'s per-field form: they are
/// protocol-wide, not `Pool`-scoped, and `ConfigValue` has no `Pubkey` variant.
#[event]
pub struct VrfConfigChanged {
    pub slot: u64,
    pub authority: Pubkey,
    pub old_vrf_program: Pubkey,
    pub old_oracle_queue: Pubkey,
    pub old_swap_pool: Pubkey,
    pub vrf_program: Pubkey,
    pub oracle_queue: Pubkey,
    pub swap_pool: Pubkey,
}

#[event]
pub struct PauseSet {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub deposits_paused: bool,
    pub rolls_paused: bool,
    pub reason: u16,
}

#[event]
pub struct AuthorityRotated {
    pub slot: u64,
    pub authority: Pubkey,
    pub role: AuthorityRole,
    pub old: Pubkey,
    pub new: Pubkey,
    pub phase: RotationPhase,
}

#[event]
pub struct DepositPending {
    pub pool: Pubkey,
    pub slot: u64,
    pub position: Pubkey,
    pub depositor: Pubkey,
    pub nft_mint: Pubkey,
}

#[event]
pub struct DepositApproved {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub position: Pubkey,
    pub value: u64,
    pub observed_at: i64,
    pub observed_age_seconds: i64,
}

/// The rebalance evaluation, emitted on every activation/exit/value-refresh trigger even when
/// the outcome leaves the blank set unchanged.
#[event]
pub struct RebalanceEvaluated {
    pub pool: Pubkey,
    pub slot: u64,
    pub n_real: u32,
    pub w_real: u128,
    pub blanks_before: [u32; 3],
    pub blanks_after: [u32; 3],
}

/// Tier membership change: the member removed (if any) and the member added (if any).
#[event]
pub struct TierChanged {
    pub pool: Pubkey,
    pub slot: u64,
    pub left: Option<Pubkey>,
    pub entered: Option<Pubkey>,
}

#[event]
pub struct DepositRejected {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub position: Pubkey,
    pub reason: u16,
}

#[event]
pub struct NftReturned {
    pub pool: Pubkey,
    pub slot: u64,
    pub position: Pubkey,
    pub depositor: Pubkey,
    pub nft_mint: Pubkey,
}

#[event]
pub struct Withdrawn {
    pub pool: Pubkey,
    pub slot: u64,
    pub position: Pubkey,
    pub depositor: Pubkey,
    pub fees_paid: u64,
}

#[event]
pub struct NftClaimed {
    pub pool: Pubkey,
    pub slot: u64,
    pub position: Pubkey,
    pub depositor: Pubkey,
    pub nft_mint: Pubkey,
}

#[event]
pub struct ValueRecorded {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub position: Pubkey,
    pub old: u64,
    pub new: u64,
    pub observed_at: i64,
}

#[event]
pub struct BelowFloorClosed {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub position: Pubkey,
    pub depositor: Pubkey,
}

#[event]
pub struct BelowFloorRetained {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub position: Pubkey,
    pub lock_until: i64,
}

#[event]
pub struct SweepBegun {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub sweep_epoch: u64,
}

#[event]
pub struct SweepEnded {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub sweep_epoch: u64,
    pub positions_updated: u32,
    pub last_sweep_at: i64,
}

/// `old_standards` is `0` on a creation, so one event type covers both admitting a collection and
/// re-standarding an admitted one. `reason` is `Some` **exactly** when the update cleared a bit,
/// which is what puts the motive on chain rather than only at the instruction boundary.
#[event]
pub struct CollectionAdmitted {
    pub pool: Pubkey,
    pub slot: u64,
    pub collection: Pubkey,
    pub standards: u8,
    pub old_standards: u8,
    pub reason: Option<u16>,
    pub authority: Pubkey,
}

/// Carries the withdrawn `standards` because nothing else survives the close: afterwards no
/// account records that this collection was ever admitted or what it permitted, so the admitted
/// set at any slot is reconstructible from events alone or not at all.
#[event]
pub struct CollectionWithdrawn {
    pub pool: Pubkey,
    pub slot: u64,
    pub collection: Pubkey,
    pub standards: u8,
    pub reason: u16,
    pub authority: Pubkey,
}

/// A third party ending custody — the only position closure no bye.fun instruction initiates, and
/// the reason it must be logged at all: an unlogged one is indistinguishable from a bug.
///
/// `source_state` is on the event because it is the only way an indexer can tell which of
/// `close_seized`'s three effect sets ran. `owner_observed` is what the asset's `owner` read as at
/// the time, which on a transfer-out names the destination and on a burn or freeze does not.
#[event]
pub struct PositionSeized {
    pub pool: Pubkey,
    pub slot: u64,
    pub position: Pubkey,
    pub kind: SeizureKind,
    pub owner_observed: Pubkey,
    pub source_state: PositionState,
    pub recorded_value: u64,
}

/// A batch's commitment, carrying the priced terms it will settle on.
///
/// The five `s_*` fields are the snapshot the batch settles against no matter what a later
/// `update_config` does, and they are published here because a third party auditing a settlement
/// must read the terms that priced it rather than the pool's current ones. The set is the priced
/// terms **only** — the tier accrual denominator and Treasury 04's ceiling are read live at
/// settlement, so neither is snapshotted and neither belongs here.
#[event]
pub struct BatchCommitted {
    pub pool: Pubkey,
    pub slot: u64,
    pub roller: Pubkey,
    pub batch: Pubkey,
    pub n: u8,
    pub s_price: u64,
    pub s_ticket: u64,
    pub s_fee: u64,
    pub s_alloc_bps: [u16; 3],
    pub s_blank_faces: [u64; 3],
}

/// Randomness delivered for a batch. Carries no result of its own — it is the anchor an indexer
/// replays a batch's settlements forward from, which is why it names the batch even though the
/// callback writes only `randomness` and the state transition.
#[event]
pub struct BatchResolved {
    pub pool: Pubkey,
    pub slot: u64,
    pub batch: Pubkey,
}

/// One roll's terminal draw.
///
/// **The draw inputs are published, not just persisted.** A third party must be able to
/// re-derive this roll's result from the event stream alone, and the pool state the draw reduced
/// over is not the state at commitment: the pipeline removes the drawn position and rebalances
/// between rolls, so every roll after the first sees a different `w_at_draw`, `n_real_at_draw`
/// and `blanks_at_draw`.
///
/// **The fee split is published as its four destinations rather than as the two aggregates the
/// vault sees.** `fee_protocol` carries the integer remainder of the basis-point division, so
/// only the per-destination form lets a reader check the four against `s_fee` and confirm the
/// remainder landed where it is supposed to; the two aggregates are recoverable from these four
/// and are therefore not carried.
#[event]
pub struct RollSettled {
    pub pool: Pubkey,
    pub slot: u64,
    pub batch: Pubkey,
    pub roll_index: u8,
    pub outcome: RollOutcome,
    /// `Pubkey::default()` when the roll drew a blank.
    pub selected_mint: Pubkey,
    /// Index into the batch's snapshotted `s_blank_faces`; meaningless on a real-card draw.
    pub blank_face_idx: u8,
    pub draw_value: u128,
    pub attempts: u8,
    pub w_at_draw: u128,
    pub n_real_at_draw: u32,
    pub blanks_at_draw: [u32; 3],
    /// Stays in the vault as owed fees.
    pub fee_equal: u64,
    /// Stays in the vault as owed fees.
    pub fee_tier: u64,
    /// Leaves the vault, and carries the basis-point division's integer remainder.
    pub fee_protocol: u64,
    /// Leaves the vault.
    pub fee_t04: u64,
}

/// One roll returned to its roller instead of settling. No fee is split on a refunded roll, which
/// is why this event carries no split: the full ticket price goes back.
///
/// `reason` is the non-zero refund branch, matching the code persisted on the roll's own record —
/// zero is reserved there for a settled roll and so never appears here.
#[event]
pub struct RollRefunded {
    pub pool: Pubkey,
    pub slot: u64,
    pub batch: Pubkey,
    pub roll_index: u8,
    pub reason: u16,
    pub wallet: Pubkey,
}

/// A batch reaching its last roll, releasing the actions gated on an open batch.
///
/// `open_batches_remaining` is the count **after** the decrement. Batches serialize, so it always
/// reads zero and a completion always reopens the pool; it is carried so a watcher reads the
/// reopening off the event rather than reconstructing the count across events.
#[event]
pub struct BatchCompleted {
    pub pool: Pubkey,
    pub slot: u64,
    pub batch: Pubkey,
    pub open_batches_remaining: u32,
}

/// A batch whose randomness never arrived, recovered after its timeout. `wallet` is what makes
/// per-wallet timeout rates computable, which is the anomaly signal that stands in for preventing
/// a withheld callback.
///
/// `open_batches_remaining` is the post-decrement count, carried for the same reason
/// [`BatchCompleted`] carries it — recovery is the other instruction that decrements it.
#[event]
pub struct BatchRecovered {
    pub pool: Pubkey,
    pub slot: u64,
    pub batch: Pubkey,
    pub wallet: Pubkey,
    pub open_batches_remaining: u32,
}

/// A settled or recovered batch's account closed and its rent returned. Nothing records the batch
/// after this, so the event is the only surviving statement that it existed and where its rent
/// went.
#[event]
pub struct BatchClosed {
    pub pool: Pubkey,
    pub slot: u64,
    pub batch: Pubkey,
    pub rent_recipient: Pubkey,
}

/// A treasury-acquired position entering the pool.
///
/// **Deliberately not a reuse of `DepositApproved`**: no attestation happens at activation. The
/// `value` and `observed_at` were attested when the card was bought and are only read back here,
/// so an approval event would record a value decision nobody made.
///
/// `observed_age_seconds` is `Clock.unix_timestamp − observed_at` in **seconds, not slots**, and
/// it is carried because the gap between attestation and activation is unbounded — this is the
/// field that makes that gap measurable rather than inferred.
#[event]
pub struct AcquiredActivated {
    pub pool: Pubkey,
    pub slot: u64,
    pub authority: Pubkey,
    pub position: Pubkey,
    pub value: u64,
    pub observed_at: i64,
    pub observed_age_seconds: i64,
}

/// A blank's equal-share credit that would push Treasury 04 past its ceiling, with the excess
/// routed to protocol revenue instead. `amount` is the excess that was rerouted, not the whole
/// credit.
#[event]
pub struct T04CeilingOverflow {
    pub pool: Pubkey,
    pub slot: u64,
    pub amount: u64,
}

/// The treasury buying a card off a holder at the live buyback rate.
///
/// **`observed_at` is carried because the attestation is not consumed here.** It is persisted and
/// read back later by the activation, so without the timestamp on this event there is no way to
/// audit after the fact how old the observation was that the pool eventually activated on;
/// `observed_age_seconds` at acceptance is carried too, so the acceptance-time and
/// activation-time ages can be compared directly rather than reconstructed.
///
/// `standard` is **appended last**, so no existing field's wire offset moves. This event and
/// `DepositPending` are the only two that carry the discriminant: those are the points where the
/// value is derived, nothing rewrites it afterwards, and they are the two an indexer sources it
/// from when it inserts the position row.
#[event]
pub struct InstantSold {
    pub pool: Pubkey,
    pub slot: u64,
    pub position: Pubkey,
    pub seller: Pubkey,
    pub value: u64,
    pub observed_at: i64,
    pub observed_age_seconds: i64,
    pub rate_bps: u16,
    pub paid: u64,
    pub standard: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Byte-cursor readers for the field-order pins at the end of this module. Read front-to-back
    // with a cursor rather than at hand-computed offsets, which are easy to get wrong at scale and
    // where a cursor cannot be off by one at field 15. Each pin ends on `is_empty()`, so a field
    // appended to an event without extending its pin reds rather than going unchecked.
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

    // `AuthorityRole`, `RotationPhase` and `ConfigValue` are all fieldless-or-not borsh
    // enums with no `#[msg]`-style explicit discriminant, so their *declaration index* is the
    // byte a client or another program reads off the wire — for `AuthorityRole` that byte is
    // `set_authorities`'s `role: AuthorityRole` instruction argument (`lib.rs`), and for
    // `ConfigValue` it is `ConfigChanged`'s payload tag (`Hours` sits at index 4). Nothing above
    // this module pinned that
    // index: `common/errors.rs` pins its wire numbers explicitly; these enums had neither an
    // explicit discriminant nor a test. Swapping two declarations compiles clean, passes every
    // other gate, and relocates whichever wire consumer reads the byte.
    //
    // Each `expected_*_byte` below is a hand-written match, independent of the enum's own
    // declaration order, over the enum's own variants with no wildcard arm — so a variant added
    // to the enum without a matching arm here fails the crate to compile (E0004) rather than
    // silently dropping out of the sweep. The test loop is what compares that independent
    // oracle against the enum's *actual* wire byte, taken from a real `try_to_vec()` round trip.

    fn expected_seizure_kind_byte(kind: SeizureKind) -> u8 {
        match kind {
            SeizureKind::Burned => 0,
            SeizureKind::Transferred => 1,
            SeizureKind::Frozen => 2,
            SeizureKind::Undecidable => 3,
        }
    }

    #[test]
    fn seizure_kind_variant_order_is_the_wire_discriminant() {
        // `ALL_SEIZURE_KINDS`, not a copy of it — the hand-listed array this loop used to carry
        // is the shape that read three variants for as long as there were three.
        for kind in ALL_SEIZURE_KINDS {
            let expected = expected_seizure_kind_byte(kind);
            assert_eq!(
                kind.try_to_vec().unwrap(),
                vec![expected],
                "SeizureKind's wire byte moved for the variant this pin expected at index \
                 {expected} — the cause named in PositionSeized is the only record of why a \
                 position closed, and a relocated byte reports a burn as a transfer"
            );
        }
    }

    #[test]
    fn seizure_kind_rejects_out_of_range_discriminant() {
        assert!(
            SeizureKind::try_from_slice(&[4u8]).is_err(),
            "byte 4 deserialized into a SeizureKind — a 5th variant was added; add it to \
             ALL_SEIZURE_KINDS, which is what every sweep over the cause iterates"
        );
        assert_eq!(
            ALL_SEIZURE_KINDS.len(),
            4,
            "and the shared list must hold every variant, or the sweeps that read it are the \
             hand-written arrays it replaced"
        );
    }

    /// **The lock gate's two sides, driven over every cause rather than over the ones that
    /// motivated it.** `close_seized` is permissionless, so a depositor can call it on their own
    /// position; the gate holds the `Active` row wherever that call could leave them holding the
    /// card, so the Season-0 lock cannot be stepped out of by arranging its own precondition. The
    /// split is asserted here, beside the enum, because it is a property of the cause and not of
    /// the handler.
    ///
    /// **`Transferred` is on the gated side, and it is not the absent route.** "The card is
    /// gone, so the depositor is a victim" does not hold under collusion: a collection carrying
    /// a `PermanentTransferDelegate` — which the census reads as
    /// `Clear`, correctly, because it grants transfers and never denies them — can put the card
    /// straight into the depositor's hands, and they then seize their own position with no
    /// `claim_nft_core` leg to run at all. Mainnet's Collector Crypt collection carries exactly
    /// that delegate, held by the collection's update authority.
    #[test]
    fn only_the_cause_that_destroys_the_card_escapes_the_lock() {
        assert!(!SeizureKind::Burned.depositor_may_keep_the_card());
        assert!(SeizureKind::Transferred.depositor_may_keep_the_card());
        assert!(SeizureKind::Frozen.depositor_may_keep_the_card());
        assert!(SeizureKind::Undecidable.depositor_may_keep_the_card());
        assert_eq!(
            ALL_SEIZURE_KINDS
                .iter()
                .filter(|kind| !kind.depositor_may_keep_the_card())
                .count(),
            1,
            "a second cause on the ungated side is a second way out of the Season-0 lock — it \
             is a decision, not a default"
        );
    }

    fn expected_authority_role_byte(role: AuthorityRole) -> u8 {
        match role {
            AuthorityRole::Administrator => 0,
            AuthorityRole::Operator => 1,
            AuthorityRole::T03 => 2,
            AuthorityRole::T02 => 3,
        }
    }

    #[test]
    fn authority_role_variant_order_is_the_wire_discriminant() {
        for role in [
            AuthorityRole::Administrator,
            AuthorityRole::Operator,
            AuthorityRole::T03,
            AuthorityRole::T02,
        ] {
            let expected = expected_authority_role_byte(role);
            assert_eq!(
                role.try_to_vec().unwrap(),
                vec![expected],
                "AuthorityRole's wire byte moved for the variant this pin expected at index \
                 {expected} — a declaration-order swap here relocates which pending-authority \
                 slot an accepted `set_authorities` rotation lands in"
            );
        }
    }

    fn expected_rotation_phase_byte(phase: RotationPhase) -> u8 {
        match phase {
            RotationPhase::Proposed => 0,
            RotationPhase::Accepted => 1,
        }
    }

    #[test]
    fn rotation_phase_variant_order_is_the_wire_discriminant() {
        for phase in [RotationPhase::Proposed, RotationPhase::Accepted] {
            let expected = expected_rotation_phase_byte(phase);
            assert_eq!(
                phase.try_to_vec().unwrap(),
                vec![expected],
                "RotationPhase's wire byte moved for the variant this pin expected at index \
                 {expected} — same class as AuthorityRole, indexer-only impact"
            );
        }
    }

    fn expected_config_value_byte(value: &ConfigValue) -> u8 {
        match value {
            ConfigValue::Amount(_) => 0,
            ConfigValue::Bps(_) => 1,
            ConfigValue::Count(_) => 2,
            ConfigValue::Faces(_) => 3,
            ConfigValue::Hours(_) => 4,
        }
    }

    #[test]
    fn config_value_variant_order_is_the_wire_discriminant() {
        for value in [
            ConfigValue::Amount(0),
            ConfigValue::Bps(0),
            ConfigValue::Count(0),
            ConfigValue::Faces([0, 0, 0]),
            ConfigValue::Hours(0),
        ] {
            let expected = expected_config_value_byte(&value);
            let bytes = value.try_to_vec().unwrap();
            assert_eq!(
                bytes[0], expected,
                "ConfigValue's leading wire byte moved for the variant this pin expected at \
                 index {expected} — a decoder depends on `Hours` staying at index 4, and would \
                 silently read the wrong field's new/old pair after a reorder"
            );
        }
    }

    // The pins above enumerate the *current* variants by hand; they can't fail for a variant
    // added without a corresponding array entry, since a compile-fail on the match arm doesn't
    // put the new variant into the loop. Asserting that the first byte past the current range is
    // still rejected closes that gap by construction: adding a variant makes the byte valid,
    // which reds this test until the iteration array above is updated to include it.

    #[test]
    fn authority_role_rejects_out_of_range_discriminant() {
        // `AuthorityRole` derives no `InitSpace` and nothing in the crate pins an `INIT_SPACE`
        // for it, so unlike `PositionState` — backstopped by `state/mod.rs`'s
        // `PositionState::INIT_SPACE == 1`, which reds if a payload grows the type — a variant
        // added here with a payload has no other guard. `is_err()` alone would still pass then,
        // on truncation rather than on an out-of-range tag. Assert borsh's own distinction
        // instead: an unknown enum tag fails with "Unexpected variant index", a truncated
        // payload fails some other way. A single tag byte is enough, since an invalid tag is
        // rejected before any payload is read.
        let mut buf: &[u8] = &[4u8];
        let err = match AuthorityRole::deserialize(&mut buf) {
            Ok(_) => panic!(
                "byte 4 deserialized into an AuthorityRole — a 5th variant was added; add it to \
                 authority_role_variant_order_is_the_wire_discriminant's iteration array"
            ),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("Unexpected variant index"),
            "byte 4 failed to deserialize for a reason other than an out-of-range tag — a 5th \
             variant may have been added with a payload that also fails on truncation, masking \
             this probe; add the variant to \
             authority_role_variant_order_is_the_wire_discriminant's iteration array \
             (borsh said: {err})"
        );
    }

    #[test]
    fn rotation_phase_rejects_out_of_range_discriminant() {
        // Same rationale as `authority_role_rejects_out_of_range_discriminant`: `RotationPhase`
        // derives no `InitSpace` either, so it carries the same zero backstop against a future
        // payload-carrying variant.
        let mut buf: &[u8] = &[2u8];
        let err = match RotationPhase::deserialize(&mut buf) {
            Ok(_) => panic!(
                "byte 2 deserialized into a RotationPhase — a 3rd variant was added; add it to \
                 rotation_phase_variant_order_is_the_wire_discriminant's iteration array"
            ),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("Unexpected variant index"),
            "byte 2 failed to deserialize for a reason other than an out-of-range tag — a 3rd \
             variant may have been added with a payload that also fails on truncation, masking \
             this probe; add the variant to \
             rotation_phase_variant_order_is_the_wire_discriminant's iteration array \
             (borsh said: {err})"
        );
    }

    #[test]
    fn config_value_rejects_out_of_range_discriminant() {
        // A padded buffer can't tell "tag out of range" apart from "payload truncated" — a
        // length-prefixed variant (`Vec`/`String`) reads its length out of the padding itself and
        // fails on that nonsense length regardless of how wide the padding is, so no padding
        // width closes this. Assert on borsh's own distinction instead: an unknown enum tag
        // fails with "Unexpected variant index", a truncated payload fails some other way. A
        // single tag byte is enough, since an invalid tag is rejected before any payload is read.
        let mut buf: &[u8] = &[5u8];
        let err = match ConfigValue::deserialize(&mut buf) {
            Ok(_) => panic!(
                "byte 5 deserialized into a ConfigValue — a 6th variant was added; add it to \
                 config_value_variant_order_is_the_wire_discriminant's iteration array"
            ),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("Unexpected variant index"),
            "byte 5 failed to deserialize for a reason other than an out-of-range tag — a 6th \
             variant may have been added with a payload that also fails on truncation, masking \
             this probe; add the variant to \
             config_value_variant_order_is_the_wire_discriminant's iteration array \
             (borsh said: {err})"
        );
    }

    // ── Round-two event field order ──────────────────────────────────────────────────────────
    //
    // One pin per event, each walking that event's own `try_to_vec()` bytes front to back. The
    // expected order is written out here by hand rather than read back off the struct, so a
    // reordered declaration disagrees with the pin instead of moving both together. Every value
    // in a fixture is distinct and every same-width neighbour pair differs, because a swap of two
    // fields of the same width is the reorder no length check can see.

    #[test]
    fn batch_committed_field_order() {
        let pool = Pubkey::new_unique();
        let roller = Pubkey::new_unique();
        let batch = Pubkey::new_unique();
        let event = BatchCommitted {
            pool,
            slot: 4_001,
            roller,
            batch,
            n: 9,
            s_price: 9_900_000,
            s_ticket: 9_000_000,
            s_fee: 900_000,
            s_alloc_bps: [7_500, 500, 2_000],
            s_blank_faces: [1_000_000, 2_000_000, 5_000_000],
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_001, "slot moved");
        assert_eq!(take_pubkey(&mut cursor), roller.to_bytes(), "roller moved");
        assert_eq!(take_pubkey(&mut cursor), batch.to_bytes(), "batch moved");
        assert_eq!(take_u8(&mut cursor), 9, "n moved");
        assert_eq!(take_u64(&mut cursor), 9_900_000, "s_price moved");
        assert_eq!(take_u64(&mut cursor), 9_000_000, "s_ticket moved");
        assert_eq!(take_u64(&mut cursor), 900_000, "s_fee moved");
        for (i, expected) in [7_500u16, 500, 2_000].iter().enumerate() {
            assert_eq!(take_u16(&mut cursor), *expected, "s_alloc_bps[{i}] moved");
        }
        for (i, expected) in [1_000_000u64, 2_000_000, 5_000_000].iter().enumerate() {
            assert_eq!(take_u64(&mut cursor), *expected, "s_blank_faces[{i}] moved");
        }
        assert!(cursor.is_empty(), "BatchCommitted grew a field");
    }

    #[test]
    fn batch_resolved_field_order() {
        let pool = Pubkey::new_unique();
        let batch = Pubkey::new_unique();
        let event = BatchResolved {
            pool,
            slot: 4_002,
            batch,
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_002, "slot moved");
        assert_eq!(take_pubkey(&mut cursor), batch.to_bytes(), "batch moved");
        assert!(cursor.is_empty(), "BatchResolved grew a field");
    }

    #[test]
    fn roll_settled_field_order() {
        let pool = Pubkey::new_unique();
        let batch = Pubkey::new_unique();
        let selected_mint = Pubkey::new_unique();
        // The four fee destinations are all u64 and adjacent, so they carry four distinct values:
        // a swapped pair inside the split is invisible to anything but a per-field comparison.
        let event = RollSettled {
            pool,
            slot: 4_003,
            batch,
            roll_index: 6,
            outcome: RollOutcome::Blank,
            selected_mint,
            blank_face_idx: 2,
            draw_value: 555_555,
            attempts: 3,
            w_at_draw: 666_666,
            n_real_at_draw: 12,
            blanks_at_draw: [21, 22, 23],
            fee_equal: 111_111,
            fee_tier: 222_222,
            fee_protocol: 333_333,
            fee_t04: 444_444,
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_003, "slot moved");
        assert_eq!(take_pubkey(&mut cursor), batch.to_bytes(), "batch moved");
        assert_eq!(take_u8(&mut cursor), 6, "roll_index moved");
        assert_eq!(take_u8(&mut cursor), 2, "outcome moved (Blank == 2)");
        assert_eq!(
            take_pubkey(&mut cursor),
            selected_mint.to_bytes(),
            "selected_mint moved"
        );
        assert_eq!(take_u8(&mut cursor), 2, "blank_face_idx moved");
        assert_eq!(take_u128(&mut cursor), 555_555, "draw_value moved");
        assert_eq!(take_u8(&mut cursor), 3, "attempts moved");
        assert_eq!(take_u128(&mut cursor), 666_666, "w_at_draw moved");
        assert_eq!(take_u32(&mut cursor), 12, "n_real_at_draw moved");
        for (i, expected) in [21u32, 22, 23].iter().enumerate() {
            assert_eq!(
                take_u32(&mut cursor),
                *expected,
                "blanks_at_draw[{i}] moved"
            );
        }
        assert_eq!(take_u64(&mut cursor), 111_111, "fee_equal moved");
        assert_eq!(take_u64(&mut cursor), 222_222, "fee_tier moved");
        assert_eq!(take_u64(&mut cursor), 333_333, "fee_protocol moved");
        assert_eq!(take_u64(&mut cursor), 444_444, "fee_t04 moved");
        assert!(cursor.is_empty(), "RollSettled grew a field");
    }

    #[test]
    fn roll_refunded_field_order() {
        let pool = Pubkey::new_unique();
        let batch = Pubkey::new_unique();
        let wallet = Pubkey::new_unique();
        let event = RollRefunded {
            pool,
            slot: 4_004,
            batch,
            roll_index: 4,
            reason: 6_204,
            wallet,
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_004, "slot moved");
        assert_eq!(take_pubkey(&mut cursor), batch.to_bytes(), "batch moved");
        assert_eq!(take_u8(&mut cursor), 4, "roll_index moved");
        assert_eq!(take_u16(&mut cursor), 6_204, "reason moved");
        assert_eq!(take_pubkey(&mut cursor), wallet.to_bytes(), "wallet moved");
        assert!(cursor.is_empty(), "RollRefunded grew a field");
    }

    #[test]
    fn batch_completed_field_order() {
        let pool = Pubkey::new_unique();
        let batch = Pubkey::new_unique();
        let event = BatchCompleted {
            pool,
            slot: 4_005,
            batch,
            open_batches_remaining: 0,
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_005, "slot moved");
        assert_eq!(take_pubkey(&mut cursor), batch.to_bytes(), "batch moved");
        assert_eq!(
            take_u32(&mut cursor),
            0,
            "open_batches_remaining moved — it is the post-decrement count"
        );
        assert!(cursor.is_empty(), "BatchCompleted grew a field");
    }

    #[test]
    fn batch_recovered_field_order() {
        let pool = Pubkey::new_unique();
        let batch = Pubkey::new_unique();
        let wallet = Pubkey::new_unique();
        let event = BatchRecovered {
            pool,
            slot: 4_006,
            batch,
            wallet,
            open_batches_remaining: 0,
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_006, "slot moved");
        assert_eq!(take_pubkey(&mut cursor), batch.to_bytes(), "batch moved");
        assert_eq!(take_pubkey(&mut cursor), wallet.to_bytes(), "wallet moved");
        assert_eq!(take_u32(&mut cursor), 0, "open_batches_remaining moved");
        assert!(cursor.is_empty(), "BatchRecovered grew a field");
    }

    #[test]
    fn batch_closed_field_order() {
        let pool = Pubkey::new_unique();
        let batch = Pubkey::new_unique();
        let rent_recipient = Pubkey::new_unique();
        let event = BatchClosed {
            pool,
            slot: 4_007,
            batch,
            rent_recipient,
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_007, "slot moved");
        assert_eq!(take_pubkey(&mut cursor), batch.to_bytes(), "batch moved");
        assert_eq!(
            take_pubkey(&mut cursor),
            rent_recipient.to_bytes(),
            "rent_recipient moved"
        );
        assert!(cursor.is_empty(), "BatchClosed grew a field");
    }

    #[test]
    fn acquired_activated_field_order() {
        let pool = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let position = Pubkey::new_unique();
        // `observed_at` and `observed_age_seconds` are both i64 and adjacent; the age is
        // deliberately not derivable from the slot, so a swap of the two is visible here.
        let event = AcquiredActivated {
            pool,
            slot: 4_008,
            authority,
            position,
            value: 36_000_000,
            observed_at: 1_760_000_000,
            observed_age_seconds: 3_600,
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_008, "slot moved");
        assert_eq!(
            take_pubkey(&mut cursor),
            authority.to_bytes(),
            "authority moved"
        );
        assert_eq!(
            take_pubkey(&mut cursor),
            position.to_bytes(),
            "position moved"
        );
        assert_eq!(take_u64(&mut cursor), 36_000_000, "value moved");
        assert_eq!(take_i64(&mut cursor), 1_760_000_000, "observed_at moved");
        assert_eq!(take_i64(&mut cursor), 3_600, "observed_age_seconds moved");
        assert!(cursor.is_empty(), "AcquiredActivated grew a field");
    }

    #[test]
    fn t04_ceiling_overflow_field_order() {
        let pool = Pubkey::new_unique();
        let event = T04CeilingOverflow {
            pool,
            slot: 4_009,
            amount: 1_250_000,
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_009, "slot moved");
        assert_eq!(
            take_u64(&mut cursor),
            1_250_000,
            "amount moved — it is the rerouted excess, not the whole credit"
        );
        assert!(cursor.is_empty(), "T04CeilingOverflow grew a field");
    }

    /// `standard` is asserted **last**, and that position is the contract: appending it there is
    /// what keeps every other field's offset where a shipped decoder already expects it. A pin
    /// that found it anywhere else would mean an earlier field had been displaced.
    #[test]
    fn instant_sold_field_order_ends_on_standard() {
        let pool = Pubkey::new_unique();
        let position = Pubkey::new_unique();
        let seller = Pubkey::new_unique();
        let event = InstantSold {
            pool,
            slot: 4_010,
            position,
            seller,
            value: 36_000_000,
            observed_at: 1_760_000_000,
            observed_age_seconds: 7_200,
            rate_bps: 8_500,
            paid: 30_600_000,
            standard: 2,
        };
        let bytes = event.try_to_vec().unwrap();
        let mut cursor = bytes.as_slice();

        assert_eq!(take_pubkey(&mut cursor), pool.to_bytes(), "pool moved");
        assert_eq!(take_u64(&mut cursor), 4_010, "slot moved");
        assert_eq!(
            take_pubkey(&mut cursor),
            position.to_bytes(),
            "position moved"
        );
        assert_eq!(take_pubkey(&mut cursor), seller.to_bytes(), "seller moved");
        assert_eq!(take_u64(&mut cursor), 36_000_000, "value moved");
        assert_eq!(take_i64(&mut cursor), 1_760_000_000, "observed_at moved");
        assert_eq!(take_i64(&mut cursor), 7_200, "observed_age_seconds moved");
        assert_eq!(take_u16(&mut cursor), 8_500, "rate_bps moved");
        assert_eq!(take_u64(&mut cursor), 30_600_000, "paid moved");
        assert_eq!(
            take_u8(&mut cursor),
            2,
            "standard is not the last field — appending it last is what holds every earlier \
             field's wire offset still"
        );
        assert!(cursor.is_empty(), "InstantSold grew a field after standard");
    }
}
