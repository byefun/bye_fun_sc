//! Read-only parser for MagicBlock `ephemeral-vrf`'s `oracle_queue` account.
//!
//! **T-119 spike deliverable.** `recover_batch` (T-121) must prove that no queue item
//! still targets this program with a given batch key before it refunds a batch, because
//! the provider's fulfilment path enforces no deadline — a refund issued while the item
//! is live races a callback with unbounded lateness.
//!
//! The layout below was recovered from `magicblock-labs/solana-vrf`
//! `api/src/state/queue.rs` and then **validated byte-for-byte against two real captured
//! accounts** (see `tests/fixtures/vrf/PROVENANCE.md`). This is our own reader, not a
//! copy of the provider's BSL-1.1 source: it takes `&[u8]`, allocates nothing, bounds-checks
//! every offset, and never panics on hostile input.
//!
//! ## The hazard this module exists to prevent
//!
//! The account is **not** an array of items. It is a 12-byte header followed by a variable
//! region, and only the bytes below `header.cursor` are live. Items past the cursor are
//! stale — a real mainnet capture (`queue-mainnet-drained.bin`) reports `item_count = 0`
//! while still holding one parseable item whose `used` flag is 0, plus a block of garbage,
//! in the bytes above the cursor.
//!
//! Two guards keep stale and dead items out of the answer, and they do different jobs.
//! Deleting each in turn across the whole fixture set was measured out of band; what the
//! suite pins is the resulting behaviour, in the named tests:
//!
//! 1. stop at `header.cursor`, never at `acc.len()`. No fixture produces a false match
//!    without it — the stale bytes fail to tile the region, so the scan returns
//!    `TrailingBytes` or `ItemExceedsProviderLimits` instead of a clean verdict
//!    (`ignoring_the_cursor_leaves_the_drained_mainnet_queue_unparseable`).
//! 2. skip items whose `used` flag is 0. Without it `queue-byefun-hole.bin` *does* produce
//!    a false match (`both_guards_are_load_bearing_independently`).

/// `Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz` — same id on mainnet-beta and devnet,
/// with the same programData account, so integration ports between clusters unchanged.
pub const VRF_PROGRAM_ID: [u8; 32] = [
    7, 100, 104, 119, 200, 241, 253, 43, 158, 185, 243, 92, 234, 235, 93, 25, 63, 162, 158, 196,
    92, 7, 181, 101, 242, 27, 167, 177, 39, 172, 247, 127,
];

/// `Cuj97ggrhhidhbu39TijNVqE74xvKJ69gDervRUXAxGh` — the provider's default base-layer
/// queue, 30,000 bytes, shared by every base-layer consumer on both clusters. T-121 pins
/// the operative queue in `ProtocolConfig`; this is the value that pin should carry.
pub const DEFAULT_QUEUE: [u8; 32] = [
    176, 242, 103, 240, 220, 124, 68, 181, 2, 196, 54, 75, 234, 77, 57, 13, 125, 145, 34, 180, 223,
    225, 64, 202, 93, 91, 244, 232, 28, 11, 142, 242,
];

/// `DAS4r8ek6agjQwJdARmKcL21YBqHBTCr7s4uNkmE33fG` = `PDA([b"identity", bye_machine], VRF)`,
/// bump 255. **Signs the callback** — this is the address `vrf_callback` must constrain
/// its signer to. Distinct from [`REQUEST_IDENTITY`]; confusing the two is the provider's
/// documented top integration footgun.
pub const SCOPED_IDENTITY: [u8; 32] = [
    180, 182, 208, 31, 231, 250, 165, 134, 221, 57, 41, 231, 188, 189, 178, 249, 81, 209, 62, 192,
    46, 39, 142, 130, 208, 82, 180, 245, 168, 161, 11, 211,
];

/// `3pwcHhbFDeq1XKtSFudty36KcxzgB3tZsd4XPfqKmbGK` = `PDA([b"identity"], bye_machine)`,
/// bump 255. **Signs the request** CPI out of `commit_rolls`.
pub const REQUEST_IDENTITY: [u8; 32] = [
    41, 255, 106, 245, 121, 177, 196, 97, 199, 147, 225, 193, 13, 90, 209, 38, 60, 159, 255, 232,
    74, 231, 135, 93, 139, 32, 233, 227, 184, 91, 218, 192,
];

/// First 8 bytes of a queue account: `AccountDiscriminator::Queue` as a little-endian u64.
pub const QUEUE_DISCRIMINATOR: [u8; 8] = [3, 0, 0, 0, 0, 0, 0, 0];

/// `size_of::<Queue>()` — item_count u32, cursor u32, index u8, paused u8, 2 pad.
pub const HEADER_SIZE: usize = 12;

/// `align_up(HEADER_SIZE)` at `align_of::<QueueItem>() == 8`. Items begin here, measured
/// from the start of the body (i.e. `acc[8..]`), which is also how `cursor` is measured.
pub const ITEMS_START: usize = 16;

/// `size_of::<QueueItem>()`.
pub const ITEM_SIZE: usize = 96;

/// `size_of::<CompactAccountMeta>()` — a 32-byte pubkey plus one `is_writable` byte.
/// Note this is **not** the 34-byte `SerializableAccountMeta` used on the request wire:
/// the stored form drops `is_signer`, since a callback CPI can carry no extra signer.
pub const META_SIZE: usize = 33;

/// Provider cap on callback accounts, enforced at request time (`ArgumentSizeTooLarge`).
pub const MAX_CALLBACK_ACCOUNTS: usize = 25;

/// Provider cap on `callback_args` bytes, enforced at request time.
pub const MAX_CALLBACK_ARGS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VrfQueueError {
    /// Fewer than 8 bytes, or the first 8 are not the queue discriminator.
    NotAQueueAccount,
    /// The body is too short to hold the fixed header.
    TruncatedHeader,
    /// `cursor` points outside the account, or below `ITEMS_START`.
    CursorOutOfBounds,
    /// An item's `*_offset` / `*_len` pair addresses bytes outside the account.
    ItemOffsetOutOfBounds,
    /// An item declares more callback accounts or args bytes than the provider permits,
    /// which the provider itself rejects at request time — so seeing one means the bytes
    /// are not a live item.
    ItemExceedsProviderLimits,
    /// Walking the items did not land exactly on `cursor`.
    TrailingBytes,
    /// `cursor == 0` is the provider's fresh-account encoding and is only consistent with
    /// an empty queue. Seen alongside a non-zero `item_count`, the header contradicts
    /// itself and the live region cannot be located.
    CursorInconsistentWithItemCount,
    /// An item's variable-length regions do not sit immediately after the fixed item, in
    /// `disc`/`metas`/`args` order. The provider writes them contiguously, so anything
    /// else would let an item address bytes outside its own slot.
    ItemRegionsNotContiguous,
    /// The walk found a different number of live items than `header.item_count` declares.
    /// The layout model and the provider's own bookkeeping disagree, so neither a hit nor
    /// a miss from this account is evidence.
    ItemCountMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueHeader {
    pub item_count: u32,
    pub cursor: u32,
    pub index: u8,
    /// 1 = the oracle has stopped accepting new requests so the queue can be drained and
    /// closed. A paused queue fails `commit_rolls`' request CPI, so the Operator needs to
    /// see this before it becomes a pool-wide stall.
    pub paused: u8,
}

/// One live queue item, with its variable-length regions already resolved and bounds-checked.
#[derive(Debug, Clone, Copy)]
pub struct QueueItemView<'a> {
    /// Byte offset of the item within the body (`acc[8..]`).
    pub pos: usize,
    /// Slot the request was enqueued at.
    pub slot: u64,
    /// The provider's request id.
    pub id: &'a [u8; 32],
    pub callback_program_id: &'a [u8; 32],
    pub callback_discriminator: &'a [u8],
    /// Raw `CompactAccountMeta` bytes: `META_SIZE` per account.
    pub callback_metas: &'a [u8],
    pub callback_args: &'a [u8],
    pub priority_request: u8,
    /// 0 = legacy global identity, 1 = scoped per-callback identity.
    pub identity_mode: u8,
    pub identity_bump: u8,
}

impl QueueItemView<'_> {
    pub fn callback_account_count(&self) -> usize {
        self.callback_metas.len() / META_SIZE
    }
}

/// What a completed scan consumed. Lets a caller assert bytes-consumed against the
/// source length rather than trusting a "no match" that a truncated read produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScanStats {
    /// Item slots stepped over, live and holes alike.
    pub items_visited: usize,
    /// Items with `used == 1`. Must equal [`ScanStats::item_count`] on a well-formed
    /// account; [`batch_request_is_live`] enforces that rather than leaving it to callers.
    pub live_items: usize,
    /// `header.item_count`, carried here so a caller holding only the stats can check the
    /// walk against the provider's own bookkeeping without re-parsing the header.
    pub item_count: u32,
    /// Body offset the walk finished at. Must equal `header.cursor`.
    pub bytes_consumed: usize,
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn read_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

#[inline]
fn align_up_8(x: usize) -> usize {
    (x + 7) & !7
}

/// Parse and validate the fixed header. `acc` is the whole account, discriminator included.
pub fn parse_header(acc: &[u8]) -> Result<QueueHeader, VrfQueueError> {
    if acc.len() < 8 || acc[..8] != QUEUE_DISCRIMINATOR {
        return Err(VrfQueueError::NotAQueueAccount);
    }
    let body = &acc[8..];
    if body.len() < HEADER_SIZE {
        return Err(VrfQueueError::TruncatedHeader);
    }
    let item_count = read_u32(body, 0);
    let cursor = read_u32(body, 4);
    // A freshly created account reads cursor == 0; the provider normalises it to
    // ITEMS_START on load. Anything else below ITEMS_START, or past the account, is junk.
    if cursor != 0 && ((cursor as usize) < ITEMS_START || cursor as usize > body.len()) {
        return Err(VrfQueueError::CursorOutOfBounds);
    }
    // A fresh account holds no items, so cursor == 0 and item_count > 0 cannot both be
    // true. Reading that pair as an empty live region would answer "absent" for items the
    // header itself calls live — the one false absence this module must never produce.
    if cursor == 0 && item_count != 0 {
        return Err(VrfQueueError::CursorInconsistentWithItemCount);
    }
    Ok(QueueHeader {
        item_count,
        cursor,
        index: body[8],
        paused: body[9],
    })
}

/// Walk every **live** item in the queue, calling `visit` for each until one yields
/// `Some(r)`, which is then returned. The walk itself always spans the whole live region,
/// so [`ScanStats`] describes it completely whether or not there was a hit.
///
/// Only the region `[ITEMS_START, header.cursor)` is walked, and only items with
/// `used == 1` are handed to `visit` — the two guards the module doc explains.
pub fn scan_live_items<'a, R>(
    acc: &'a [u8],
    mut visit: impl FnMut(&QueueItemView<'a>) -> Option<R>,
) -> Result<(Option<R>, ScanStats), VrfQueueError> {
    let header = parse_header(acc)?;
    let body = &acc[8..];
    let end = if header.cursor == 0 {
        ITEMS_START
    } else {
        header.cursor as usize
    };

    let mut stats = ScanStats {
        item_count: header.item_count,
        ..ScanStats::default()
    };
    let mut cur = ITEMS_START;
    let mut found = None;

    while cur + ITEM_SIZE <= end {
        let p = cur;
        let disc_off = read_u32(body, p + 72) as usize;
        let metas_off = read_u32(body, p + 76) as usize;
        let args_off = read_u32(body, p + 80) as usize;
        let disc_len = read_u16(body, p + 84) as usize;
        let metas_len = read_u16(body, p + 86) as usize;
        let args_len = read_u16(body, p + 88) as usize;
        let used = body[p + 91];

        // The provider refuses these at request time, so an item claiming more is not a
        // live item — refuse to address memory on its say-so.
        if metas_len > MAX_CALLBACK_ACCOUNTS || args_len > MAX_CALLBACK_ARGS {
            return Err(VrfQueueError::ItemExceedsProviderLimits);
        }
        let metas_bytes = metas_len * META_SIZE;
        let disc_end = disc_off.saturating_add(disc_len);
        let metas_end = metas_off.saturating_add(metas_bytes);
        let args_end = args_off.saturating_add(args_len);
        if disc_end > body.len() || metas_end > body.len() || args_end > body.len() {
            return Err(VrfQueueError::ItemOffsetOutOfBounds);
        }
        // The provider writes the three regions immediately after the fixed item, in this
        // order. Bounds alone would still let an item point at another item's bytes, or at
        // stale bytes above the cursor, so the layout is pinned rather than just bounded.
        if disc_off != p + ITEM_SIZE || metas_off != disc_end || args_off != metas_end {
            return Err(VrfQueueError::ItemRegionsNotContiguous);
        }

        let next = align_up_8(p + ITEM_SIZE + disc_len + metas_bytes + args_len);
        stats.items_visited += 1;

        if used == 1 {
            stats.live_items += 1;
            let item = QueueItemView {
                pos: p,
                slot: u64::from_le_bytes(body[p..p + 8].try_into().unwrap()),
                id: body[p + 8..p + 40].try_into().unwrap(),
                callback_program_id: body[p + 40..p + 72].try_into().unwrap(),
                callback_discriminator: &body[disc_off..disc_end],
                callback_metas: &body[metas_off..metas_end],
                callback_args: &body[args_off..args_end],
                priority_request: body[p + 90],
                identity_mode: body[p + 92],
                identity_bump: body[p + 93],
            };
            // `visit` stops being called once it has answered, but the walk continues:
            // the stats are the caller's evidence that the region was covered, so they
            // have to describe the whole live region on a hit as well as on a miss.
            if found.is_none() {
                found = visit(&item);
            }
        }

        if next <= cur {
            return Err(VrfQueueError::TrailingBytes);
        }
        cur = next;
    }

    stats.bytes_consumed = cur;
    // A well-formed queue's items tile the live region exactly. Landing anywhere else
    // means the layout assumption is wrong, and a "not found" from a mis-tiled walk is
    // not evidence of absence.
    if cur != end {
        return Err(VrfQueueError::TrailingBytes);
    }
    Ok((found, stats))
}

/// The predicate `recover_batch` (T-121) needs: is there a live item whose callback
/// targets `callback_program_id` and whose `callback_args` **begin with** `batch_key`?
///
/// Matching on `callback_args` rather than on the account-meta list is deliberate. The
/// batch PDA is globally unique across pools, so two pools can never false-positive each
/// other's recovery; and a substring search over the raw account would match the same 32
/// bytes appearing in an unrelated item's meta list. The `queue-byefun-absent.bin` fixture
/// is built to fail exactly that way if the field is not respected.
pub fn batch_request_is_live(
    acc: &[u8],
    callback_program_id: &[u8; 32],
    batch_key: &[u8; 32],
) -> Result<(bool, ScanStats), VrfQueueError> {
    let (hit, stats) = scan_live_items(acc, |item| {
        let targets_us = item.callback_program_id == callback_program_id;
        let carries_batch =
            item.callback_args.len() >= 32 && &item.callback_args[..32] == batch_key;
        (targets_us && carries_batch).then_some(())
    })?;
    // The walk and the provider's own count must agree. If they do not, the layout model
    // is wrong about this account, and neither the hit nor the miss above is evidence.
    if stats.live_items as u64 != stats.item_count as u64 {
        return Err(VrfQueueError::ItemCountMismatch);
    }
    Ok((hit.is_some(), stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real captures and the synthetic cases derived from them. See
    // `tests/fixtures/vrf/PROVENANCE.md` for how each was produced.
    const DEVNET_LIVE_7: &[u8] =
        include_bytes!("../../../../tests/fixtures/vrf/queue-devnet-live-7.bin");
    const MAINNET_DRAINED: &[u8] =
        include_bytes!("../../../../tests/fixtures/vrf/queue-mainnet-drained.bin");
    const BYEFUN_PRESENT: &[u8] =
        include_bytes!("../../../../tests/fixtures/vrf/queue-byefun-present.bin");
    const BYEFUN_ABSENT: &[u8] =
        include_bytes!("../../../../tests/fixtures/vrf/queue-byefun-absent.bin");
    const BYEFUN_REMOVED: &[u8] =
        include_bytes!("../../../../tests/fixtures/vrf/queue-byefun-removed.bin");
    const BYEFUN_HOLE: &[u8] =
        include_bytes!("../../../../tests/fixtures/vrf/queue-byefun-hole.bin");

    /// `4exRPJN7N8MVZh37efYAkwnGGxoJKMVUARQqf7awmYkJ`, this program.
    const BYE_MACHINE: [u8; 32] = [
        54, 76, 54, 252, 49, 226, 204, 175, 166, 157, 81, 120, 114, 48, 36, 171, 165, 254, 215,
        110, 69, 63, 93, 233, 51, 2, 2, 144, 142, 169, 93, 139,
    ];

    /// The batch key present in `queue-byefun-present.bin`'s `callback_args`.
    const BATCH_KEY: [u8; 32] = [
        0x9c, 0x2f, 0x68, 0x7e, 0x8a, 0xaf, 0x31, 0xb0, 0xb3, 0x7d, 0x1c, 0x04, 0xcc, 0xcb, 0x1b,
        0xf2, 0x53, 0xa6, 0x93, 0x3c, 0xec, 0xfe, 0x12, 0x43, 0xf8, 0x94, 0x44, 0x91, 0xbc, 0x86,
        0x43, 0x5e,
    ];

    /// Loads a fixture and pins its length and discriminator **at the point of use**.
    /// libtest gives no ordering guarantee and does not stop the suite on a failure, so a
    /// separate whole-suite length check cannot protect an absence assertion in another
    /// test: every absence proof re-establishes its own source here.
    fn complete(name: &str, f: &'static [u8]) -> &'static [u8] {
        assert_eq!(f.len(), 30_000, "{name} is not a full account capture");
        assert_eq!(f[..8], QUEUE_DISCRIMINATOR, "{name} lost its discriminator");
        f
    }

    /// Every fixture is a full-size account. A short read would make every absence
    /// assertion vacuous, so each fixture's length and discriminator are pinned — here
    /// over the whole set, and again inside each absence test via [`complete`].
    #[test]
    fn every_fixture_is_a_complete_30000_byte_account() {
        for (name, f) in [
            ("devnet-live-7", DEVNET_LIVE_7),
            ("mainnet-drained", MAINNET_DRAINED),
            ("byefun-present", BYEFUN_PRESENT),
            ("byefun-absent", BYEFUN_ABSENT),
            ("byefun-removed", BYEFUN_REMOVED),
            ("byefun-hole", BYEFUN_HOLE),
        ] {
            assert_eq!(f.len(), 30_000, "{name} is not a full account capture");
            assert_eq!(f[..8], QUEUE_DISCRIMINATOR, "{name} lost its discriminator");
        }
    }

    /// POSITIVE CONTROL for the parser itself. Seven items captured from devnet, and the
    /// walk must reproduce the header's own bookkeeping: exactly `item_count` live items,
    /// finishing exactly on `cursor`. A parser that cannot do this on real chain bytes has
    /// no standing to report an absence on any other fixture.
    #[test]
    fn the_real_devnet_capture_parses_to_its_own_declared_bookkeeping() {
        let src = complete("devnet-live-7", DEVNET_LIVE_7);
        let header = parse_header(src).unwrap();
        assert_eq!(header.item_count, 7);
        assert_eq!(header.cursor, 1024);
        assert_eq!(header.paused, 0);

        let (hit, stats) = scan_live_items::<()>(src, |_| None).unwrap();
        assert!(hit.is_none());
        assert_eq!(stats.live_items, 7, "live items must match item_count");
        assert_eq!(stats.items_visited, 7);
        assert_eq!(
            stats.bytes_consumed, header.cursor as usize,
            "the walk must consume the live region exactly — a short walk's \
             'not found' is a false absence"
        );
    }

    /// THE LAYOUT CONTROL. Reading a capture successfully only proves the reader is
    /// self-consistent — a reader with the wrong field offsets can still walk a queue and
    /// land on the cursor, because the *step* is computed from lengths it may also be
    /// misreading. So: re-emit each parsed item from its decoded parts and require the
    /// result to be byte-identical to 30,000 bytes of real chain data.
    ///
    /// **What this pins, and what it cannot.** Reproduction constrains only the fields that
    /// are non-zero in the source. Here that is `slot`, `id`, `callback_program_id`, the
    /// three offsets, `disc_len`, `metas_len` and `identity_mode`/`identity_bump` — and,
    /// through `metas_off == disc_off + disc_len` and `args_off == metas_off + metas_len *
    /// META_SIZE`, it pins `META_SIZE` at exactly 33.
    ///
    /// It does **not** pin `args_len` (p+88), `priority_request` (p+90) or the p+94..96
    /// padding: all are 0 in every one of the 7 captured items, so a model that placed them
    /// elsewhere would reproduce the source just as exactly. `used` (p+91) is written as a
    /// literal 1 below and only live items are visited, so its offset is not pinned here
    /// either. `args_len` is the one the matcher depends on, and the only evidence for its
    /// position comes from the derived fixtures — which this same writer produced.
    #[test]
    fn re_emitting_the_real_capture_reproduces_it_byte_for_byte() {
        let src = DEVNET_LIVE_7;
        let header = parse_header(src).unwrap();
        let mut out = vec![0u8; src.len()];
        out[..8].copy_from_slice(&QUEUE_DISCRIMINATOR);
        let body = &mut out[8..];

        let mut cur = ITEMS_START;
        let mut live = 0u32;
        scan_live_items::<()>(src, |item| {
            let p = item.pos;
            assert_eq!(p, cur, "items must tile the live region without gaps");
            let disc_off = p + ITEM_SIZE;
            let metas_off = disc_off + item.callback_discriminator.len();
            let args_off = metas_off + item.callback_metas.len();
            let args_end = args_off + item.callback_args.len();

            body[p..p + 8].copy_from_slice(&item.slot.to_le_bytes());
            body[p + 8..p + 40].copy_from_slice(item.id);
            body[p + 40..p + 72].copy_from_slice(item.callback_program_id);
            body[p + 72..p + 76].copy_from_slice(&(disc_off as u32).to_le_bytes());
            body[p + 76..p + 80].copy_from_slice(&(metas_off as u32).to_le_bytes());
            body[p + 80..p + 84].copy_from_slice(&(args_off as u32).to_le_bytes());
            body[p + 84..p + 86]
                .copy_from_slice(&(item.callback_discriminator.len() as u16).to_le_bytes());
            body[p + 86..p + 88]
                .copy_from_slice(&(item.callback_account_count() as u16).to_le_bytes());
            body[p + 88..p + 90].copy_from_slice(&(item.callback_args.len() as u16).to_le_bytes());
            body[p + 90] = item.priority_request;
            body[p + 91] = 1; // used
            body[p + 92] = item.identity_mode;
            body[p + 93] = item.identity_bump;
            body[disc_off..metas_off].copy_from_slice(item.callback_discriminator);
            body[metas_off..args_off].copy_from_slice(item.callback_metas);
            body[args_off..args_end].copy_from_slice(item.callback_args);

            live += 1;
            cur = align_up_8(args_end);
            None
        })
        .unwrap();

        body[0..4].copy_from_slice(&live.to_le_bytes());
        body[4..8].copy_from_slice(&(cur as u32).to_le_bytes());
        body[8] = header.index;
        body[9] = header.paused;

        // The capture holds stale bytes above the cursor (see the mainnet fixture for the
        // hazard); copy them across so the comparison is over the whole account and the
        // live region is the only thing this test reconstructed.
        let tail = header.cursor as usize;
        out[8 + tail..].copy_from_slice(&src[8 + tail..]);

        assert_eq!(live, header.item_count);
        assert_eq!(cur, header.cursor as usize);
        assert_eq!(
            out.len(),
            src.len(),
            "re-emission changed the account length"
        );
        let first_diff = out.iter().zip(src).position(|(a, b)| a != b);
        assert_eq!(
            first_diff, None,
            "re-emission diverges from the real capture at byte {first_diff:?} — \
             the layout model is wrong"
        );
    }

    /// Every item in the real capture uses the scoped identity and stays inside the
    /// provider's caps — the shape bye.fun's own request must also produce.
    #[test]
    fn real_devnet_items_are_scoped_and_within_the_provider_caps() {
        let (_, stats) = scan_live_items::<()>(DEVNET_LIVE_7, |item| {
            assert_eq!(item.identity_mode, 1, "expected the scoped identity path");
            assert_eq!(item.callback_discriminator.len(), 8);
            assert!(item.callback_account_count() <= MAX_CALLBACK_ACCOUNTS);
            assert!(item.callback_args.len() <= MAX_CALLBACK_ARGS);
            None
        })
        .unwrap();
        assert_eq!(stats.live_items, 7);
    }

    /// THE HAZARD, on real bytes. Mainnet reports an empty queue, and the correct scan
    /// agrees — while the account demonstrably still holds parseable item bytes above the
    /// cursor. This is the fixture that makes the cursor guard load-bearing.
    #[test]
    fn the_drained_mainnet_capture_yields_no_items_despite_holding_stale_item_bytes() {
        let drained = complete("mainnet-drained", MAINNET_DRAINED);
        let header = parse_header(drained).unwrap();
        assert_eq!(header.item_count, 0);
        assert_eq!(
            header.cursor, 16,
            "cursor is at ITEMS_START — queue is drained"
        );

        let (hit, stats) = scan_live_items::<()>(drained, |_| None).unwrap();
        assert!(hit.is_none());
        assert_eq!(stats.items_visited, 0);
        assert_eq!(stats.live_items, 0);

        // The absence above is only meaningful because the bytes are really there: the
        // stale item's callback program id sits at body offset 40+16, past the cursor.
        let body = &MAINNET_DRAINED[8..];
        let stale_callback_program = &body[ITEMS_START + 40..ITEMS_START + 72];
        assert_ne!(
            stale_callback_program, &[0u8; 32],
            "fixture no longer carries the stale item — the guard it proves is untested"
        );
    }

    /// A scan that ignored the cursor would walk that stale region. Pinned as an explicit
    /// counter-measurement so the guard cannot be removed silently: with the cursor
    /// clamped to the account length instead, the stale bytes fail to parse as items.
    ///
    /// Note what this does *not* show: the stale region yields no live item even to a
    /// cursor-blind walk, because `[0]` has `used == 0` and `[1]` is garbage. On this
    /// account the cursor guard buys a clean verdict rather than a correct one; the
    /// false-match case belongs to the `used` guard and lives in `queue-byefun-hole.bin`.
    #[test]
    fn ignoring_the_cursor_leaves_the_drained_mainnet_queue_unparseable() {
        let mut naive = MAINNET_DRAINED.to_vec();
        // Rewrite only the cursor, to the full body length — the single change that
        // distinguishes a correct scan from the naive one.
        let body_len = (naive.len() - 8) as u32;
        naive[12..16].copy_from_slice(&body_len.to_le_bytes());

        let naive_result = scan_live_items::<()>(&naive, |_| None);
        // The stale region is not well-formed: the walk either trips a bounds/limit check
        // or fails to tile the region. Either way it is *not* the clean empty result the
        // correct scan returns — which is the whole point.
        assert_eq!(
            naive_result,
            Err(VrfQueueError::ItemExceedsProviderLimits),
            "the naive scan must not quietly agree with the correct one"
        );
    }

    /// POSITIVE CONTROL for the matcher: a queue that really does hold this batch's
    /// request must be reported live.
    #[test]
    fn a_live_byefun_request_is_found() {
        let (live, stats) =
            batch_request_is_live(BYEFUN_PRESENT, &BYE_MACHINE, &BATCH_KEY).unwrap();
        assert!(live, "the batch's own queue item must be found");
        assert_eq!(stats.live_items, 8, "7 unrelated items plus ours");
    }

    /// NEGATIVE CONTROL, and a field-discrimination test. This fixture is byte-identical
    /// to the one above except that `callback_args` carries a *different* batch key —
    /// while `BATCH_KEY` still appears verbatim inside the item's account-meta list. A
    /// matcher that searched the raw account, or read the wrong field, passes the test
    /// above and fails here.
    #[test]
    fn a_different_batch_is_not_matched_even_though_the_key_bytes_are_in_the_account() {
        let (live, stats) = batch_request_is_live(
            complete("byefun-absent", BYEFUN_ABSENT),
            &BYE_MACHINE,
            &BATCH_KEY,
        )
        .unwrap();
        assert!(!live, "matched a batch whose key is only in the meta list");
        assert_eq!(stats.live_items, 8, "the walk still covered every item");

        // Prove the bytes really are present, so the non-match above is discriminating
        // rather than vacuous.
        assert!(
            BYEFUN_ABSENT.windows(32).any(|w| w == BATCH_KEY),
            "fixture no longer contains the key — this test proves nothing"
        );
    }

    /// NEGATIVE CONTROL for the cursor guard, on the matcher. The batch's item is still
    /// in the file, fully formed, but it has been logically removed and the cursor
    /// trimmed below it — the state a fulfilled or purged request leaves behind. Reporting
    /// it live here is what would freeze the pool forever.
    #[test]
    fn a_removed_byefun_request_is_not_matched_though_its_bytes_remain() {
        assert!(
            BYEFUN_REMOVED.windows(32).any(|w| w == BATCH_KEY),
            "fixture no longer contains the key — this test proves nothing"
        );

        let (live, stats) = batch_request_is_live(
            complete("byefun-removed", BYEFUN_REMOVED),
            &BYE_MACHINE,
            &BATCH_KEY,
        )
        .unwrap();
        assert!(!live, "a removed request must not block recovery");
        assert_eq!(stats.live_items, 7, "only the 7 unrelated items are live");
        assert_eq!(stats.bytes_consumed, 1024);
    }

    /// NEGATIVE CONTROL for the `used` guard, and the reason it is not redundant with the
    /// cursor guard. `remove_items_matching` clears `used` in place and trims only
    /// *trailing* holes, so an item removed while live items still follow it leaves a
    /// `used == 0` slot **below** the cursor. The cursor guard cannot reject this one.
    ///
    /// This fixture was added because an injection round removing the `used` guard passed
    /// the whole suite without it: every other `used == 0` item in the set sits past the
    /// cursor, so the cursor guard was masking the second guard entirely.
    #[test]
    fn a_hole_inside_the_live_region_is_not_matched() {
        let hole = complete("byefun-hole", BYEFUN_HOLE);
        let header = parse_header(hole).unwrap();
        let (live, stats) = batch_request_is_live(hole, &BYE_MACHINE, &BATCH_KEY).unwrap();

        assert!(!live, "a logically-removed item must not block recovery");
        assert_eq!(
            stats.items_visited, 8,
            "the hole is stepped over, not skipped"
        );
        assert_eq!(stats.live_items, 7, "but it is not counted live");
        assert_eq!(stats.live_items as u32, header.item_count);
        assert_eq!(
            stats.bytes_consumed, header.cursor as usize,
            "the hole is inside the live region, so the walk still spans it"
        );
        // The hole really is inside the cursor — otherwise the cursor guard, not the
        // `used` guard, is what this test is exercising.
        let hole_pos = ITEMS_START;
        assert!((hole_pos as u32) < header.cursor);
        assert_eq!(BYEFUN_HOLE[8 + hole_pos + 91], 0, "slot 0 must be the hole");
    }

    /// The two guards are independent. Each is perturbed on its own and the verdict must
    /// flip, so neither can be deleted on the grounds that the other covers it.
    #[test]
    fn both_guards_are_load_bearing_independently() {
        // --- the cursor guard, alone ---------------------------------------------
        // In `queue-byefun-removed.bin` the matching item is well-formed and `used` is 0,
        // but it also sits past the cursor. Restoring *both* — extending the cursor over
        // it and setting `used` — must resurrect it; if it does not, the fixture is not
        // exercising the cursor guard at all.
        let (live, _) = batch_request_is_live(BYEFUN_REMOVED, &BYE_MACHINE, &BATCH_KEY).unwrap();
        assert!(!live);

        let mut resurrected = BYEFUN_REMOVED.to_vec();
        resurrected[8..12].copy_from_slice(&8u32.to_le_bytes());
        resurrected[12..16].copy_from_slice(&1200u32.to_le_bytes());
        resurrected[8 + 1024 + 91] = 1;
        let (live_again, _) =
            batch_request_is_live(&resurrected, &BYE_MACHINE, &BATCH_KEY).unwrap();
        assert!(
            live_again,
            "the trimmed item must be reachable once restored"
        );

        // Extending the cursor *without* clearing the hole must still not match — that
        // isolates the cursor as the sole difference above.
        let mut cursor_only = BYEFUN_REMOVED.to_vec();
        // 7, not 8: with the hole still set this account really does hold 7 live items,
        // and a header that disagreed with its own contents would now be refused for the
        // inconsistency instead of exercising the `used` guard.
        cursor_only[8..12].copy_from_slice(&7u32.to_le_bytes());
        cursor_only[12..16].copy_from_slice(&1200u32.to_le_bytes());
        let (still_dead, _) =
            batch_request_is_live(&cursor_only, &BYE_MACHINE, &BATCH_KEY).unwrap();
        assert!(
            !still_dead,
            "with the cursor extended, only the `used` flag is still rejecting it — \
             so the `used` guard carries this case on its own"
        );

        // --- the `used` guard, alone ---------------------------------------------
        // `queue-byefun-hole.bin` puts the hole below the cursor, where the cursor guard
        // is inert. Setting `used` there must resurrect it.
        let (hole_live, _) = batch_request_is_live(BYEFUN_HOLE, &BYE_MACHINE, &BATCH_KEY).unwrap();
        assert!(!hole_live);

        let mut hole_used = BYEFUN_HOLE.to_vec();
        hole_used[8 + ITEMS_START + 91] = 1;
        hole_used[8..12].copy_from_slice(&8u32.to_le_bytes()); // the header agrees: 8 live
        let (hole_live_again, _) =
            batch_request_is_live(&hole_used, &BYE_MACHINE, &BATCH_KEY).unwrap();
        assert!(
            hole_live_again,
            "clearing the hole must resurrect the item with the cursor untouched — \
             if it does not, the `used` guard is untested"
        );
    }

    /// A different program's identical batch key must not match. Two pools' recoveries
    /// must never be able to false-positive each other.
    #[test]
    fn an_item_from_another_callback_program_is_never_matched() {
        let mut other_program = BYE_MACHINE;
        other_program[0] ^= 0xff;
        let (live, _) = batch_request_is_live(
            complete("byefun-present", BYEFUN_PRESENT),
            &other_program,
            &BATCH_KEY,
        )
        .unwrap();
        assert!(!live);
    }

    /// Hostile input must be refused, not panic. A `recover_batch` that panics on a
    /// malformed account is a denial of service on the recovery path itself.
    #[test]
    fn malformed_accounts_are_refused_rather_than_panicking() {
        assert_eq!(parse_header(&[]), Err(VrfQueueError::NotAQueueAccount));
        assert_eq!(
            parse_header(&[0u8; 64]),
            Err(VrfQueueError::NotAQueueAccount)
        );

        let mut short = QUEUE_DISCRIMINATOR.to_vec();
        short.extend_from_slice(&[0u8; 4]);
        assert_eq!(parse_header(&short), Err(VrfQueueError::TruncatedHeader));

        let mut bad_cursor = DEVNET_LIVE_7.to_vec();
        bad_cursor[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            parse_header(&bad_cursor),
            Err(VrfQueueError::CursorOutOfBounds)
        );

        // An item claiming 60,000 callback accounts must be refused on the provider's own
        // cap rather than used to index memory.
        let mut absurd = DEVNET_LIVE_7.to_vec();
        absurd[8 + ITEMS_START + 86..8 + ITEMS_START + 88]
            .copy_from_slice(&60_000u16.to_le_bytes());
        assert_eq!(
            scan_live_items::<()>(&absurd, |_| None),
            Err(VrfQueueError::ItemExceedsProviderLimits)
        );
    }

    /// The two identity PDAs are **re-derived here** rather than trusted as literals, and
    /// checked against this crate's own `PROGRAM_ID` rather than a second copy of it. The
    /// request-side and callback-side PDAs share the `b"identity"` seed but differ in both
    /// the seed list and the deriving program; the provider's docs call confusing them the
    /// top integration footgun, so the distinctness is asserted too.
    #[test]
    fn the_identity_pdas_rederive_from_this_crates_own_program_id() {
        use anchor_lang::prelude::Pubkey;

        let ours = crate::program_id::PROGRAM_ID;
        assert_eq!(
            ours.to_bytes(),
            BYE_MACHINE,
            "the fixtures were built against a different program id"
        );
        let vrf = Pubkey::new_from_array(VRF_PROGRAM_ID);

        let (scoped, scoped_bump) =
            Pubkey::find_program_address(&[b"identity", ours.as_ref()], &vrf);
        assert_eq!(scoped.to_bytes(), SCOPED_IDENTITY);
        assert_eq!(scoped_bump, 255);

        let (request, request_bump) = Pubkey::find_program_address(&[b"identity"], &ours);
        assert_eq!(request.to_bytes(), REQUEST_IDENTITY);
        assert_eq!(request_bump, 255);

        assert_ne!(
            scoped, request,
            "request-side and callback-side identities must never collapse"
        );
    }

    /// POSITIVE CONTROL for the derivation above. The provider hardcodes its legacy global
    /// identity as `9irBy75QS2BN81FUgXuHcjqceJJRuc9oDkAe8TKVvvAw` at bump 254; reproducing
    /// a value we did not choose proves the derivation is real and not self-confirming.
    #[test]
    fn the_pda_derivation_reproduces_the_providers_own_published_constant() {
        use anchor_lang::prelude::{pubkey, Pubkey};

        let vrf = Pubkey::new_from_array(VRF_PROGRAM_ID);
        let (legacy, bump) = Pubkey::find_program_address(&[b"identity"], &vrf);
        assert_eq!(
            legacy,
            pubkey!("9irBy75QS2BN81FUgXuHcjqceJJRuc9oDkAe8TKVvvAw")
        );
        assert_eq!(bump, 254);
    }

    /// Our literal pins are checked against the pinned SDK's own constants, so a version
    /// bump that moved the program id, the default queue, or the scoped-identity
    /// derivation fails here instead of at the first devnet request.
    #[test]
    fn our_pins_agree_with_the_pinned_sdks_own_constants() {
        use anchor_lang::prelude::Pubkey;

        assert_eq!(
            ephemeral_vrf_sdk::consts::VRF_PROGRAM_ID.to_bytes(),
            VRF_PROGRAM_ID
        );
        assert_eq!(
            ephemeral_vrf_sdk::consts::DEFAULT_QUEUE.to_bytes(),
            DEFAULT_QUEUE
        );

        let ours = crate::program_id::PROGRAM_ID;
        assert_eq!(
            ephemeral_vrf_sdk::consts::scoped_vrf_identity(&ours).to_bytes(),
            SCOPED_IDENTITY,
            "the SDK's scoped-identity derivation no longer agrees with our pin"
        );

        // The legacy global identity is deprecated; assert we are not accidentally
        // pinned to it, since the two differ only in which path signs the callback.
        let legacy: Pubkey = ephemeral_vrf_sdk::consts::VRF_PROGRAM_IDENTITY;
        assert_ne!(legacy.to_bytes(), SCOPED_IDENTITY);
    }

    /// The layout constants are pinned to their measured values. These are not arbitrary:
    /// each was confirmed against the two real captures, and a silent change to any of
    /// them re-points every offset in the scan.
    #[test]
    fn layout_constants_are_pinned_to_their_measured_values() {
        assert_eq!(HEADER_SIZE, 12);
        assert_eq!(ITEMS_START, 16);
        assert_eq!(ITEM_SIZE, 96);
        assert_eq!(META_SIZE, 33);
        assert_eq!(MAX_CALLBACK_ACCOUNTS, 25);
        assert_eq!(MAX_CALLBACK_ARGS, 512);
        assert_eq!(QUEUE_DISCRIMINATOR, [3, 0, 0, 0, 0, 0, 0, 0]);
        // ITEMS_START is align_up(HEADER_SIZE) at align_of::<QueueItem>() == 8.
        assert_eq!(ITEMS_START, align_up_8(HEADER_SIZE));
    }

    /// REGRESSION for the `item_count` cross-check. A header that disagrees with its own
    /// contents means the layout model is wrong about this account, so neither the hit nor
    /// the miss is evidence — and the caller must not be handed a verdict either way.
    #[test]
    fn a_header_that_disagrees_with_the_walk_is_refused() {
        let mut miscounted = BYEFUN_PRESENT.to_vec();
        miscounted[8..12].copy_from_slice(&7u32.to_le_bytes()); // 8 items are really live
        // The region still tiles and every item still parses: the count is the only fault.
        let (_, stats) = scan_live_items::<()>(&miscounted, |_| None).unwrap();
        assert_eq!(stats.live_items, 8);
        assert_eq!(stats.item_count, 7);

        assert_eq!(
            batch_request_is_live(&miscounted, &BYE_MACHINE, &BATCH_KEY),
            Err(VrfQueueError::ItemCountMismatch),
            "a self-contradicting header must not yield a verdict"
        );
    }

    /// REGRESSION for the cursor guard's blind spot. The guard stops the *walk* at
    /// `header.cursor`, but each item addresses its regions by absolute offset, so an item
    /// inside the live region can still point `callback_args` somewhere else entirely.
    ///
    /// `queue-byefun-absent.bin` carries a different batch key in its args while
    /// `BATCH_KEY` sits verbatim in the same item's meta list at body 1128. Repointing
    /// `args_off` from 1161 to 1128 — without touching any length, so the items still tile
    /// the region exactly — makes the matcher read the meta list and report a batch that
    /// this queue does not hold.
    #[test]
    fn an_item_whose_regions_are_not_contiguous_is_refused() {
        // Baseline: unmodified, this fixture is a clean miss.
        let (before, _) = batch_request_is_live(BYEFUN_ABSENT, &BYE_MACHINE, &BATCH_KEY).unwrap();
        assert!(!before);

        let mut repointed = BYEFUN_ABSENT.to_vec();
        let item = 1024usize; // the 8th item, body-relative
        assert_eq!(read_u32(&repointed[8..], item + 80), 1161, "args_off moved");
        repointed[8 + item + 80..8 + item + 84].copy_from_slice(&1128u32.to_le_bytes());
        // The key really is readable at the new offset, so the guard is what rejects this
        // rather than the bytes simply not matching.
        assert_eq!(&repointed[8 + 1128..8 + 1160], &BATCH_KEY[..]);

        assert_eq!(
            batch_request_is_live(&repointed, &BYE_MACHINE, &BATCH_KEY),
            Err(VrfQueueError::ItemRegionsNotContiguous),
            "an item must not be able to address regions outside its own slot"
        );
    }

    /// REGRESSION. `cursor == 0` is the provider's fresh-account encoding, and it is only
    /// consistent with an empty queue. A header that reads `cursor == 0` while claiming
    /// live items is not a fresh account, and answering "absent" for it is a false absence
    /// of exactly the kind that lets a refund race a live callback.
    ///
    /// Driven through `batch_request_is_live` — the production entry point — so the guard
    /// under test is the shipped one and not a copy owned by the test.
    #[test]
    fn a_zero_cursor_with_live_items_is_refused_rather_than_answered_absent() {
        // `queue-byefun-present.bin` really does hold this batch, so a "no" here is wrong
        // rather than merely unproven.
        let (present, _) = batch_request_is_live(BYEFUN_PRESENT, &BYE_MACHINE, &BATCH_KEY).unwrap();
        assert!(present, "fixture no longer holds the batch — this test proves nothing");

        let mut zero_cursor = BYEFUN_PRESENT.to_vec();
        zero_cursor[12..16].copy_from_slice(&0u32.to_le_bytes());
        // item_count is left at 8: that inconsistency is the whole point.
        assert_eq!(read_u32(&zero_cursor[8..], 0), 8);

        assert_eq!(
            batch_request_is_live(&zero_cursor, &BYE_MACHINE, &BATCH_KEY),
            Err(VrfQueueError::CursorInconsistentWithItemCount),
            "a zero cursor over a non-empty header must fail closed, never answer absent"
        );
    }

    /// REGRESSION for the `ScanStats` contract on the hit path. The matching item is put
    /// **first** here, so a scan that stops at the first hit reports `live_items = 1` and
    /// `bytes_consumed = 192` instead of the header's own 8 and 1200 — and a caller using
    /// those fields to reject a mis-tiled walk would reject a genuine hit.
    ///
    /// `queue-byefun-hole.bin` already carries the bye.fun item at slot 0; clearing its
    /// hole makes it a live first match without moving any other item.
    #[test]
    fn stats_are_complete_when_the_matching_item_is_first() {
        let mut match_first = BYEFUN_HOLE.to_vec();
        match_first[8 + ITEMS_START + 91] = 1; // clear the hole -> 8 live items
        match_first[8..12].copy_from_slice(&8u32.to_le_bytes()); // header agrees: 8 live
        let header = parse_header(&match_first).unwrap();
        assert_eq!(header.item_count, 8);
        assert_eq!(header.cursor, 1200);

        let (live, stats) =
            batch_request_is_live(&match_first, &BYE_MACHINE, &BATCH_KEY).unwrap();
        assert!(live, "the first item is the match");
        assert_eq!(
            stats.items_visited, 8,
            "the walk must span the live region even after it has its answer"
        );
        assert_eq!(stats.live_items as u32, header.item_count);
        assert_eq!(
            stats.bytes_consumed, header.cursor as usize,
            "bytes_consumed must be the end of the live region, not the end of the match"
        );
    }
}
