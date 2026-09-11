# VRF `oracle_queue` fixtures — provenance

Captured 2026-09-10 for **T-119**. Consumed by
`programs/bye_machine/src/common/vrf_queue.rs`'s test module and, from T-121, by
`recover_batch`'s queue scan.

Two fixtures are **verbatim chain captures**. The other four are derived from the devnet
capture by a writer that is first proved able to reproduce that capture byte-for-byte, so
the synthetic cases inherit the real layout instead of a hand-guessed one.

## Source

| | |
|---|---|
| Queue account | `Cuj97ggrhhidhbu39TijNVqE74xvKJ69gDervRUXAxGh` (`DEFAULT_QUEUE`) |
| Owner program | `Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz` — same id, same programData, both clusters |
| Method | `getAccountInfo`, `encoding: base64`, public RPC |
| Mainnet capture | slot 445,893,204 · `item_count = 0`, `cursor = 16` |
| Devnet capture | slot ~496,165,000 · `item_count = 7`, `cursor = 1024` |
| Account size | 30,000 bytes on both clusters |
| Layout source | `magicblock-labs/solana-vrf` `api/src/state/queue.rs` @ `4de8b0f2`, then validated against the captures above |

## Files

| File | Kind | `item_count` / `cursor` | Expected verdict for `BATCH_KEY` |
|---|---|---|---|
| `queue-devnet-live-7.bin` | real capture | 7 / 1024 | no match — 7 live items, none bye.fun's |
| `queue-mainnet-drained.bin` | real capture | 0 / 16 | no match — **and stale item bytes sit above the cursor** |
| `queue-byefun-present.bin` | derived | 8 / 1200 | **match** — 8th item, `callback_program = bye_machine`, `callback_args[..32] = BATCH_KEY` |
| `queue-byefun-absent.bin` | derived | 8 / 1200 | no match — `callback_args` carries a *different* batch key |
| `queue-byefun-removed.bin` | derived | 7 / 1024 | no match — well-formed item, `used = 0`, cursor trimmed below it |
| `queue-byefun-hole.bin` | derived | 7 / 1200 | no match — `used = 0` hole **inside** the cursor, at slot 0 |

`BATCH_KEY` = `BWgXgB48LW9Rb85Ljo79jDEe8Nvyr4NUraCM2wVbXuHX`
= `9c2f687e8aaf31b0b37d1c04cccb1bf253a6933cecfe1243f8944491bc86435e`.

An opaque off-curve key owned by `bye_machine`. T-116 fixes the real `RollBatch` seeds; the
scan matches these 32 bytes wherever they come from, so this fixture set does not pre-empt
that decision.

## Why each derived case exists

Each one fails a *different* wrong implementation. Together they are the discriminating
set; drop any one and a specific defect passes.

- **`present`** — the positive control. Without it the whole set is satisfied by a scanner
  that always answers "no", and every absence below would be vacuous.
- **`absent`** — field discrimination. It is byte-identical to `present` except in
  `callback_args`, and `BATCH_KEY` still appears verbatim **inside the item's account-meta
  list**. A scanner that searches the raw account, or reads the meta list instead of the
  args, passes `present` and fails here.
- **`removed`** — what a fulfilled or purged request leaves behind: the matching item is
  fully formed and still in the file, marked dead by `used = 0` *and* a trimmed cursor.
  Because both marks are present it does not isolate either guard; the isolation is done
  in `both_guards_are_load_bearing_independently`, which restores them one at a time.
- **`hole`** — the `used` guard, isolated. `remove_items_matching` clears `used` in place
  and trims only *trailing* holes, so an item removed while live items still follow it
  leaves a dead slot **below** the cursor, where the cursor guard is inert.

  This fixture was added *after* an injection round: deleting the `used` guard passed the
  entire suite without it, because every other dead item in the set sat above the cursor
  and the cursor guard was masking the second guard completely.

## The hazard the real captures document

`queue-mainnet-drained.bin` is an account the provider considers **empty** — and it still
contains a parseable item plus a block of garbage above the cursor:

```
cursor-honoring       : 0 items
naive (whole account) : 1 slot stepped, 0 live, then Err
  [0] pos=16  slot=445890215  callback=4dArXGeDHu6bPc2Y4W89KNhriqLkQduTwhPfHnn42Nbp  used=0
  [1] pos=160 slot=7342615587296536643  metas_len=52416  args_len=46931  <- garbage,
      refused on the provider's caps and its offsets before it can be reported
```

Neither is reportable as a live item: `[0]` carries `used = 0`, and `[1]` exceeds the
provider's caps. So on this fixture the cursor guard is what separates a clean empty
verdict from an `Err`, and it is the `used` guard that stops a dead item from matching.

## Reproducing

The captures are point-in-time and cannot be re-fetched identically — the mainnet queue
drains and refills continuously, and the devnet items will eventually be purged. Verify
integrity against `SHA256SUMS` instead:

```sh
cd tests/fixtures/vrf && shasum -a 256 -c SHA256SUMS
```

To re-capture from scratch, `getAccountInfo` the queue account and check the result parses
with `vrf_queue::scan_live_items` to its own `item_count` and `cursor` — that self-consistency
is what qualifies a capture as a fixture.

## Integrity check that matters

Every fixture is a **complete 30,000-byte account**. A truncated capture would make every
"no match" above a false absence rather than evidence, so
`every_fixture_is_a_complete_30000_byte_account` pins the length and the discriminator
before any other test runs.
