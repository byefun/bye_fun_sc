// The MagicBlock `ephemeral-vrf` queue this suite's roll pipeline requests against, and the
// provider's own expiry instruction over it.
//
// `commit_rolls` CPIs into the VRF program to place its randomness request, and that handler
// requires the `oracle_queue` account to be writable and owned by the VRF program — nothing more.
// So a captured queue account, loaded at a fixture address, is a queue the real request handler
// genuinely appends to.
//
// **A queue is not creatable from this suite, which is why one is loaded rather than made.**
// `InitializeOracleQueue` reads the oracle's record with `as_account_mut::<Oracle>`, so it needs
// an oracle already registered, and registration is checked against the VRF program's own upgrade
// authority. Serving a queue needs more still — an ECVRF proof the provider verifies on chain.
//
// **The queue is loaded at its derived PDA rather than an arbitrary address, and that is what
// makes the timeout path drivable.** `PurgeExpiredRequests` is permissionless and reads no oracle
// record; it checks only that the account carries seeds `[b"queue", oracle, index]`. Loading the
// capture at that address therefore lets the provider's own code remove an expired request, which
// is the precondition `recover_batch` scans for. The alternative — pointing the protocol config at
// a second queue that never held the request — would reach the same refund through a cause that
// never occurs in production.
//
// The bytes are a real mainnet capture reporting `item_count = 0, cursor = 16` — empty, with
// 30,000 bytes of room, so a request appends without the handler needing to resize it. Stale item
// bytes sit above the cursor in that capture, which is a property of the real account and is left
// intact: the queue reader under test is the one that has to stop at the cursor.

import { address, type Address, type Instruction } from "@solana/kit";
import { VRF_PROGRAM } from "../../scripts/constants.js";
import { writable } from "../../scripts/lib/anchor.js";

/**
 * The key the queue PDA is seeded with. Never signed for and never registered — it exists only so
 * the queue has a derivable address.
 */
export const VRF_ORACLE_FIXTURE: Address = address("BYEvrforac1eFixture111111111111111111111111");

/** `PDA([b"queue", VRF_ORACLE_FIXTURE, 0])` under the VRF program, bump 255. */
export const VRF_QUEUE_FIXTURE: Address = address("FZBg5ns3gbHVPBEMUHjaMC419ybCrP1JdZv6rjRM3V8s");

/** The index byte in the queue's seeds, and in the purge instruction's one argument. */
export const VRF_QUEUE_INDEX = 0;

/** Requests expire this many seconds after creation — the provider's `QUEUE_TTL_SECONDS`. */
export const VRF_QUEUE_TTL_SECONDS = 120;

const PURGE_EXPIRED_REQUESTS_DISCRIMINANT = 9;

/**
 * The provider's `PurgeExpiredRequests` — permissionless, two accounts, one `u8` argument. Drops
 * every queue item older than the TTL, which is how a request stops being live without an oracle.
 *
 * Wire format is the provider's `instruction8!` macro: the enum discriminant in byte 0, seven zero
 * bytes, then the `#[repr(C, packed)]` body — here a single `index` byte.
 */
export function purgeExpiredRequests(): Instruction {
  const data = new Uint8Array(9);
  data[0] = PURGE_EXPIRED_REQUESTS_DISCRIMINANT;
  data[8] = VRF_QUEUE_INDEX;

  return {
    programAddress: VRF_PROGRAM,
    accounts: [writable(VRF_ORACLE_FIXTURE), writable(VRF_QUEUE_FIXTURE)],
    data,
  };
}
