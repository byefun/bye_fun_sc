#!/usr/bin/env bash
# Integration test runner: boots a fresh solana-test-validator with the program loaded
# at its declared id, runs the integration suite, then tears the validator down.
#
# Why a wrapper (not bare `pnpm run test:integration`): the runtime tests need (a) a
# validator, and (b) a PRISTINE ledger each run, because program config accounts are
# expected to be init-once singletons. The .so is loaded at the DECLARED id (`--bpf-program
# $PROGRAM_ID`) so `declare_id!` matches; the keypair cargo generates under target/deploy
# would land the program at the wrong address.
set -euo pipefail
cd "$(dirname "$0")/.."

PROGRAM_ID="szKvUQJx6USMBe6J2yNrd16jEAF98Ys82GdeBP93yqG"
SO="target/deploy/bye_machine.so"
DEPLOYER="accounts/dev/deployer.json"
# The suite's three protocol authorities, distinct from DEPLOYER and from each other — see
# tests/helpers/protocol.ts's ensureProtocol and tests/helpers/env.ts's load{Operator,
# T02Authority,Unprivileged}. Fixed local keys, not bootstrap-rotated: rotation would make the live
# operator depend on runner file order under --test-concurrency=1.
OPERATOR="accounts/dev/operator.json"
T02="accounts/dev/t02.json"
UNPRIVILEGED="accounts/dev/unprivileged.json"
LEDGER_DIR="$(mktemp -d)"
LEDGER="$LEDGER_DIR/test-ledger"
# Assigned just before the suite runs, but declared here so the EXIT trap below can
# reference it unconditionally: the trap is installed before this file exists, and an
# early exit between the two would otherwise hit an unbound variable under `set -u`.
TAP_LOG=""

# Program fixtures the validator must load for the pNFT path. Dumped from mainnet-beta
# with `solana program dump <id> <path> --url mainnet-beta`, committed under
# tests/fixtures/programs/, and listed here as "<program_id>:<path>" pairs. No AMM: the
# blank leg's eligibility checks read the pool account and derive vault addresses from it
# without ever invoking the venue, so its code is not needed. The VRF program IS needed and
# is not optional — commit_rolls CPIs into it to place the randomness request, so without it
# no batch opens at all and every roll-pipeline test is unreachable, not merely unresolvable.
# MPL Core is on the list because the suite executes against it — a system-created account
# assigned to the program as a Core "fixture" proves an owner check and nothing about the
# program's own behaviour.
#   metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s  Token Metadata (pNFT)
#   auth9SigNpDKz4sJJ1DfCTuZrZNSAgh9sFD3rboVmgg  Token Auth Rules
FIXTURES=(
  "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s:tests/fixtures/programs/metadata.so"
  "auth9SigNpDKz4sJJ1DfCTuZrZNSAgh9sFD3rboVmgg:tests/fixtures/programs/auth-rules.so"
  "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d:tests/fixtures/programs/mpl-core.so"
  "Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz:tests/fixtures/programs/vrf.so"
)

# The rule-set ACCOUNT every eligible pNFT's programmable_config.rule_set points at —
# loaded as data, not code, via --account (not --bpf-program). The account itself, not just the Token Auth Rules program, must
# be present: it is what the escrow path's Validate CPI actually reads.
RULE_SET_ADDRESS="eBJLFYPxJmMGKuFwpDWkzxZeUrad92kZRC5BJLpzyT9"
RULE_SET_ACCOUNT="tests/fixtures/programs/rule-set.json"

# The blank settlement path's three accounts, loaded as data via --account exactly as the
# rule-set account above is. The venue program itself is never deployed: the pool's eligibility
# checks read the pool account and derive vault addresses from it, and none of them invokes the
# venue or reads a vault, so its code is not needed to exercise them.
#
# The pool is a real mainnet pool with its mints, vaults, price and activation point rewritten —
# see scripts/build-swap-fixtures.ts, which regenerates all three. Keeping a real pool underneath
# leaves the fee schedule and curve bounds at values a pool actually holds.
#
# The two mints carry accounts/dev/deployer.json as their mint authority, so the suite can mint
# against them exactly as it did against the runtime-created mint these replaced. $BYE and USDC
# are now distinct, which is what lets the pool name a pair at all.
SWAP_POOL_ADDRESS="BYEpoo1Fixture11111111111111111111111111111"
SWAP_POOL_ACCOUNT="tests/fixtures/swap/damm-v2-pool.json"
BYE_MINT_ADDRESS="BYEmint1Fixture1111111111111111111111111111"
BYE_MINT_ACCOUNT="tests/fixtures/swap/bye-mint.json"
USDC_MINT_ADDRESS="USDCmintFixture1111111111111111111111111111"
USDC_MINT_ACCOUNT="tests/fixtures/swap/usdc-mint.json"

# Two pools at lengths Pool no longer has, for the migrate_pool suite: the 342-byte layout with
# three blank slots, which the migration grows, and the 276-byte layout from before escrow_bid_bps
# and reserved were appended, which it must refuse. No instruction can create either any more, and
# only this program can write Pool's discriminator, so they are loaded at the address init_pool
# gives their pool id — see scripts/build-legacy-pool-fixtures.ts, which regenerates both.
LEGACY_POOL_ADDRESS="B8rHjfjXGAg9K1DfBKpzWtJZbNhtg4XMVMSjRNPWeGsm"
LEGACY_POOL_ACCOUNT="tests/fixtures/legacy-pool.json"
MALFORMED_POOL_ADDRESS="AWkr7RbX3UdBP1NrrHa7CYzfrRLUEnvVXojSJrMWbHsJ"
MALFORMED_POOL_ACCOUNT="tests/fixtures/malformed-pool.json"

# A finished batch in the 1,100-byte layout RollBatch had with three blank slots, for the
# close_batch suite: commit_rolls now writes the 1,360-byte layout, so no instruction can create
# one. Regenerated by scripts/build-legacy-pool-fixtures.ts.
LEGACY_BATCH_ADDRESS="Gem9yjNhravXtfoWyAuwjmki4Kw4cMAXvzKMQPqp1e5c"
LEGACY_BATCH_ACCOUNT="tests/fixtures/legacy-batch.json"
# Three more 1,100-byte batches naming a roller whose key the suite holds: one finished, for the
# roller's own close; one still Committed; and one finished but sitting at another batch's PDA.
LEGACY_ROLLER_BATCH_ADDRESS="BFMNTu6A89BNmTdJixw2jys23kGVprt5fbX5ghf4fNLc"
LEGACY_ROLLER_BATCH_ACCOUNT="tests/fixtures/legacy-batch-roller.json"
LEGACY_COMMITTED_BATCH_ADDRESS="6Q7y4Ezq36T56Vo9ynMmeRUXKsUfgw6YjEDby5KqR7A2"
LEGACY_COMMITTED_BATCH_ACCOUNT="tests/fixtures/legacy-batch-committed.json"
MISPLACED_BATCH_ADDRESS="7Vc1qXdb1Jt39wECs6cYTkX6zNpPWww8fKyZxvfttH3Y"
MISPLACED_BATCH_ACCOUNT="tests/fixtures/legacy-batch-misplaced.json"

# Two current-layout pools still holding their own Treasury 03 and Treasury 04, with those two token
# accounts each, for the consolidate_treasuries suite: init_pool now creates only the shared
# treasuries, so no instruction can produce a pool like these. The second has a batch open.
# Regenerated by scripts/build-legacy-pool-fixtures.ts.
UNCONSOLIDATED_POOL_ADDRESS="9DkwUbPphjri77TXq7GH2eRSqyEH3rR6z4gcDaF2L73N"
UNCONSOLIDATED_POOL_ACCOUNT="tests/fixtures/unconsolidated-pool.json"
UNCONSOLIDATED_T03_ADDRESS="CCVerWT76EY4X5HiGo1zhKSqmTipV9QBbjLziUcnWBnE"
UNCONSOLIDATED_T03_ACCOUNT="tests/fixtures/unconsolidated-pool-t03.json"
UNCONSOLIDATED_T04_ADDRESS="8nJD5UJZGYTqMx9q8JK9tihJE9edXsVn3qMS9zukuX1y"
UNCONSOLIDATED_T04_ACCOUNT="tests/fixtures/unconsolidated-pool-t04.json"
BUSY_UNCONSOLIDATED_POOL_ADDRESS="4vBHtPS8oRLTPecEtZhcSBnLbizxgyujgR9MNjNKCAd7"
BUSY_UNCONSOLIDATED_POOL_ACCOUNT="tests/fixtures/busy-unconsolidated-pool.json"
BUSY_UNCONSOLIDATED_T03_ADDRESS="2HAGNWZ1UNaYQaoeLvfwapxWUap1hA2GUQVgeVaWJfm2"
BUSY_UNCONSOLIDATED_T03_ACCOUNT="tests/fixtures/busy-unconsolidated-pool-t03.json"
BUSY_UNCONSOLIDATED_T04_ADDRESS="B2pXYPN23GNX2vBsYLtU1bGxUm26SAXFRw3Dd15PuHYf"
BUSY_UNCONSOLIDATED_T04_ACCOUNT="tests/fixtures/busy-unconsolidated-pool-t04.json"
# A third, no batch open, that names a Treasury 03 other than its own: consolidation refuses it.
MISNAMED_UNCONSOLIDATED_POOL_ADDRESS="4KVhS4voiCvGaNLB4YYkqQhkMPKsujcpaV4tJLBnG6rG"
MISNAMED_UNCONSOLIDATED_POOL_ACCOUNT="tests/fixtures/misnamed-unconsolidated-pool.json"
MISNAMED_UNCONSOLIDATED_T03_ADDRESS="4cpSerJSccYiXu1wBQ2yekSF8XSG5rnNxDyHi9rQ4kpt"
MISNAMED_UNCONSOLIDATED_T03_ACCOUNT="tests/fixtures/misnamed-unconsolidated-pool-t03.json"
MISNAMED_UNCONSOLIDATED_T04_ADDRESS="F1sSjQFL2hGy3mqcMd4unomJCG6em7rN6ngjQ3e88dHm"
MISNAMED_UNCONSOLIDATED_T04_ACCOUNT="tests/fixtures/misnamed-unconsolidated-pool-t04.json"

# The VRF oracle queue commit_rolls places its randomness request in, loaded as data for the
# same reason the accounts above are. A queue cannot be created from this suite — the provider's
# InitializeOracleQueue reads an oracle record that only its own upgrade authority can register —
# and creating one would buy nothing, since serving it needs an ECVRF proof verified on chain.
# The request handler requires only that this account be writable and VRF-owned, so a real
# captured queue is one it genuinely appends to.
#
# Bytes: the mainnet capture reporting item_count = 0, cursor = 16 — empty, with 30,000 bytes of
# room, so an appended request never makes the handler resize it.
#
# The address is not arbitrary, unlike the --account fixtures above it: it is
# PDA([b"queue", BYEvrforac1e…, 0]) under the VRF program. PurgeExpiredRequests checks exactly
# those seeds, and is permissionless and reads no oracle record, so loading the queue at its
# derived address is what makes the provider's own expiry path drivable here — and with it the
# VRF-timeout recovery, whose precondition is a request that is no longer live.
VRF_QUEUE_ADDRESS="FZBg5ns3gbHVPBEMUHjaMC419ybCrP1JdZv6rjRM3V8s"
VRF_QUEUE_ACCOUNT="tests/fixtures/vrf/queue-localnet.json"

# The key the queue PDA is seeded with, loaded rent-exempt and system-owned. It is not decoration:
# PurgeExpiredRequests credits the reclaimed request fee to this account, and a bare 0-lamport
# account cannot receive it — the transfer leaves it below the rent-exempt minimum and the runtime
# rejects the whole transaction after the program has already reported success.
VRF_ORACLE_ADDRESS="BYEvrforac1eFixture111111111111111111111111"
VRF_ORACLE_ACCOUNT="tests/fixtures/vrf/oracle-localnet.json"

# Rule-set replay: 9 synthetic rule-set accounts, one per historical revision the live
# eBJLFY…zyT9 account's own revision map records (parsed from $RULE_SET_ACCOUNT — see
# tests/integration/rule-set-replay.test.ts's own revision-map parser, which re-derives the same
# 9 offsets at test time rather than trusting this list). Each file is that one revision's exact
# on-chain bytes, repackaged as a standalone rule-set account (its own 9-byte header + a
# single-entry revision map) so it becomes the *sole, and therefore operative*, revision when
# loaded at its own address — i.e. "what would happen if Metaplex republished this revision as
# current". Addresses are arbitrary (never signed for; --account loads state, not a keypair).
REVISION_RULE_SETS=(
  "y1UDsgq3ff1tXtEjJNQ9Vv9BViK1oRLS8i1U2MMZCkJ:tests/fixtures/programs/rule-set-revision-0.json"
  "4akQWcyASwc5ucfgMbmE9Ck1s34JcU6eFmjTNbMquz9V:tests/fixtures/programs/rule-set-revision-1.json"
  "6ztH99b9a7WtBN1Rx6tV2ardn6jAHF3ZAANxM5jBk7sk:tests/fixtures/programs/rule-set-revision-2.json"
  "7cCzkSMttJWLDVhgWQEUytB3ksMc73YQrkHjHB6sxZmF:tests/fixtures/programs/rule-set-revision-3.json"
  "Gz6qVdriqDecVqhSZmfmMYsPgRgh8HGgvQ287qmztBPq:tests/fixtures/programs/rule-set-revision-4.json"
  "85LJQktqJRwiPCD3RutHaBG9kPspnMbf5H9BXQNkZTKb:tests/fixtures/programs/rule-set-revision-5.json"
  "8yYVLWJJLsmbWG6uxYqvkebSTEPizAUZbRj1aBadim85:tests/fixtures/programs/rule-set-revision-6.json"
  "F8sPtXnhnrxaa4ghj1YY8MQNS6nNh2ez25fbikMr7AXB:tests/fixtures/programs/rule-set-revision-7.json"
  "Hd4z5EdEk6UTLZY85ZkW1rRpeFHcfsZ4whbHfqhGrcBt:tests/fixtures/programs/rule-set-revision-8.json"
)
EXPECTED_REVISION_RULE_SETS=9

# Checksum manifest + tamper-control canary for the fixture blobs above. See the
# verification block below (runs before validator boot, fails closed).
FIXTURE_MANIFEST="tests/fixtures/programs/SHA256SUMS"
CANARY="tests/fixtures/canary.bin"
CANARY_SHA256="31847232876a3cef8db9a8744c3543c7e87693561f38d1a9b2e1247d428f2ed7"

# --- Integration-suite liveness gate ------------------------------------------------
# `node --test` over a glob matching nothing prints "1..0 / # tests 0" and exits 0. That
# command is the last one in this script, so a renamed directory, a typo'd glob or a tsx
# resolution failure each read as a PASSING integration run. Exit status alone cannot
# tell "everything passed" from "nothing ran", and ci.yml runs only `typecheck` and the
# unit suite -- the one place the integration evidence is produced has no other backstop.
#
# Both are exact equalities, never >=. A >= floor accepts a suite that silently shrank,
# which is precisely the failure this gate exists to catch.
EXPECTED_INTEGRATION_TESTS=291 # The count from a run, not an itemized sum: re-measure a run
# rather than hand-adding contributions.
EXPECTED_FIXTURES=4            # Token Metadata + Token Auth Rules + MPL Core + MagicBlock VRF — raised together with FIXTURES above

if [[ ! -f "$SO" ]]; then
  echo "missing $SO — no program has been built yet." >&2
  echo "Build it first:" >&2
  echo "  docker compose run --rm build   # or: anchor build" >&2
  exit 1
fi
if [[ ! -f "$DEPLOYER" ]]; then
  echo "missing $DEPLOYER — required local suite admin key (see README, Integration suite keypairs)" >&2
  exit 1
fi
for kp in "$OPERATOR" "$T02" "$UNPRIVILEGED"; do
  if [[ ! -f "$kp" ]]; then
    echo "missing $kp — required suite authority key" >&2
    exit 1
  fi
done

# --- Fixture checksum verification -------------------------------------------------
# Runs before the liveness gate below (and before FIXTURES is even read) on purpose:
# with EXPECTED_INTEGRATION_TESTS still 0 the gate refuses every run, so this is the
# one piece of this runner that must execute — and be provably exercised, both directions —
# on a run that otherwise fails closed for an unrelated, expected reason.
#
# A checksum step that silently no-ops reads exactly like one that passes: a
# `command -v … || true` guard, an empty manifest, or a hashing tool that resolves to
# something else on another machine all produce a script that "passed" without ever
# comparing a byte. `sha256sum` is /sbin-only on recent macOS and absent on macOS ≤ 14
# — machine-dependent, not portable — so it is tried first where present and
# `shasum -a 256` (ships with every macOS, and with most Linux images via perl) is the
# fallback; neither present is a hard failure, not a skip.
sha256_of() {
  local f="$1" out
  if command -v sha256sum >/dev/null 2>&1; then
    out="$(sha256sum "$f")"
  elif command -v shasum >/dev/null 2>&1; then
    out="$(shasum -a 256 "$f")"
  else
    echo "FATAL: no SHA-256 tool found (need 'sha256sum' or 'shasum -a 256')" >&2
    exit 1
  fi
  printf '%s' "${out%%  *}"
}

verify_blob() {
  local file="$1" expected="$2"
  [[ "$(sha256_of "$file")" == "$expected" ]]
}

# One committed byte flipped in the digest, never in the file: proves the comparison
# itself discriminates, not just that hashing ran.
tamper_digest() {
  local d="$1" first="${d:0:1}" flipped
  if [[ "$first" == "0" ]]; then flipped="1"; else flipped="0"; fi
  printf '%s%s' "$flipped" "${d:1}"
}

# The tamper control, run before any real blob is trusted: the canary must verify
# against its correct digest (exit 0) and must NOT verify against that same digest
# with one nibble altered (non-zero). Both directions, one committed artifact, nothing
# left permanently red.
if [[ ! -f "$CANARY" ]]; then
  echo "missing checksum canary $CANARY — the tamper control cannot run" >&2
  exit 1
fi
if ! verify_blob "$CANARY" "$CANARY_SHA256"; then
  echo "CHECKSUM CONTROL FAILED: canary did not verify against its own pinned digest" >&2
  echo "— the checker is broken, or $CANARY was modified without repinning it." >&2
  exit 1
fi
if verify_blob "$CANARY" "$(tamper_digest "$CANARY_SHA256")"; then
  echo "CHECKSUM CONTROL FAILED: canary verified against a deliberately altered digest" >&2
  echo "— the checker does not discriminate a mismatch and cannot be trusted." >&2
  exit 1
fi
echo "Checksum control: canary passed the correct digest and failed the tampered one."

if [[ ! -f "$FIXTURE_MANIFEST" ]]; then
  echo "missing fixture manifest $FIXTURE_MANIFEST" >&2
  exit 1
fi

manifest_digest_for() {
  local target="$1" line hash path
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    hash="${line%%  *}"
    path="${line#*  }"
    if [[ "$path" == "$target" ]]; then
      printf '%s' "$hash"
      return 0
    fi
  done < "$FIXTURE_MANIFEST"
  return 1
}

verify_fixture() {
  local path="$1" expected
  if ! expected="$(manifest_digest_for "$path")"; then
    echo "fixture $path is not listed in $FIXTURE_MANIFEST" >&2
    exit 1
  fi
  if [[ ! -f "$path" ]]; then
    echo "missing fixture $path — dump it with 'solana program dump' / 'solana account'" >&2
    exit 1
  fi
  if ! verify_blob "$path" "$expected"; then
    echo "CHECKSUM MISMATCH: $path does not match $FIXTURE_MANIFEST" >&2
    echo "  expected $expected" >&2
    echo "  got      $(sha256_of "$path")" >&2
    exit 1
  fi
}

verify_fixture "tests/fixtures/programs/metadata.so"
verify_fixture "tests/fixtures/programs/auth-rules.so"
verify_fixture "tests/fixtures/programs/mpl-core.so"
verify_fixture "tests/fixtures/programs/vrf.so"
verify_fixture "$RULE_SET_ACCOUNT"
verify_fixture "$SWAP_POOL_ACCOUNT"
verify_fixture "$BYE_MINT_ACCOUNT"
verify_fixture "$USDC_MINT_ACCOUNT"
verify_fixture "$VRF_QUEUE_ACCOUNT"
verify_fixture "$VRF_ORACLE_ACCOUNT"

# REVISION_RULE_SETS and EXPECTED_REVISION_RULE_SETS move together, same reasoning as
# FIXTURES/EXPECTED_FIXTURES below: a half-landed list would boot a validator missing revision
# accounts the replay test needs, surfacing later as an unrelated-looking account-not-found error.
if [[ "${#REVISION_RULE_SETS[@]}" -ne "$EXPECTED_REVISION_RULE_SETS" ]]; then
  echo "revision rule-set list holds ${#REVISION_RULE_SETS[@]}, expected exactly $EXPECTED_REVISION_RULE_SETS" >&2
  exit 1
fi
for pair in "${REVISION_RULE_SETS[@]:-}"; do
  [[ -z "$pair" ]] && continue
  verify_fixture "${pair##*:}"
done

# The reverse direction: a blob committed on disk but missing from the manifest would
# load into the validator unverified. Every file under tests/fixtures/programs/ other
# than the manifest itself must be one the manifest names.
for f in tests/fixtures/programs/* tests/fixtures/swap/*; do
  [[ -e "$f" ]] || continue
  [[ "$(basename "$f")" == "$(basename "$FIXTURE_MANIFEST")" ]] && continue
  if ! manifest_digest_for "$f" >/dev/null; then
    echo "unlisted fixture on disk: $f is not recorded in $FIXTURE_MANIFEST" >&2
    exit 1
  fi
done

if [[ "$EXPECTED_INTEGRATION_TESTS" -lt 1 ]]; then
  echo "liveness gate is vacuous: EXPECTED_INTEGRATION_TESTS=$EXPECTED_INTEGRATION_TESTS, so a run" >&2
  echo "that executed nothing would satisfy it. tests/integration/ has no cases yet; raise this" >&2
  echo "constant with the first one. Refusing to report success on zero evidence." >&2
  exit 1
fi

# FIXTURES and EXPECTED_FIXTURES move together, asserted before the loop reads them: the
# loop below iterates "${FIXTURES[@]:-}", which passes over an empty array without
# complaint, so a half-landed fixture set would boot a validator missing programs the
# suite needs and fail later as an unrelated-looking runtime error.
if [[ "${#FIXTURES[@]}" -ne "$EXPECTED_FIXTURES" ]]; then
  echo "fixture list holds ${#FIXTURES[@]}, expected exactly $EXPECTED_FIXTURES" >&2
  exit 1
fi

BPF_ARGS=(--bpf-program "$PROGRAM_ID" "$SO")
for pair in "${FIXTURES[@]:-}"; do
  [[ -z "$pair" ]] && continue
  fixture_id="${pair%%:*}"; fixture_so="${pair##*:}"
  if [[ ! -f "$fixture_so" ]]; then
    echo "missing fixture $fixture_so — dump it with 'solana program dump'" >&2
    exit 1
  fi
  BPF_ARGS+=(--bpf-program "$fixture_id" "$fixture_so")
done
BPF_ARGS+=(--account "$RULE_SET_ADDRESS" "$RULE_SET_ACCOUNT")
BPF_ARGS+=(--account "$SWAP_POOL_ADDRESS" "$SWAP_POOL_ACCOUNT")
BPF_ARGS+=(--account "$BYE_MINT_ADDRESS" "$BYE_MINT_ACCOUNT")
BPF_ARGS+=(--account "$USDC_MINT_ADDRESS" "$USDC_MINT_ACCOUNT")
BPF_ARGS+=(--account "$LEGACY_POOL_ADDRESS" "$LEGACY_POOL_ACCOUNT")
BPF_ARGS+=(--account "$MALFORMED_POOL_ADDRESS" "$MALFORMED_POOL_ACCOUNT")
BPF_ARGS+=(--account "$LEGACY_BATCH_ADDRESS" "$LEGACY_BATCH_ACCOUNT")
BPF_ARGS+=(--account "$LEGACY_ROLLER_BATCH_ADDRESS" "$LEGACY_ROLLER_BATCH_ACCOUNT")
BPF_ARGS+=(--account "$LEGACY_COMMITTED_BATCH_ADDRESS" "$LEGACY_COMMITTED_BATCH_ACCOUNT")
BPF_ARGS+=(--account "$MISPLACED_BATCH_ADDRESS" "$MISPLACED_BATCH_ACCOUNT")
BPF_ARGS+=(--account "$UNCONSOLIDATED_POOL_ADDRESS" "$UNCONSOLIDATED_POOL_ACCOUNT")
BPF_ARGS+=(--account "$UNCONSOLIDATED_T03_ADDRESS" "$UNCONSOLIDATED_T03_ACCOUNT")
BPF_ARGS+=(--account "$UNCONSOLIDATED_T04_ADDRESS" "$UNCONSOLIDATED_T04_ACCOUNT")
BPF_ARGS+=(--account "$BUSY_UNCONSOLIDATED_POOL_ADDRESS" "$BUSY_UNCONSOLIDATED_POOL_ACCOUNT")
BPF_ARGS+=(--account "$BUSY_UNCONSOLIDATED_T03_ADDRESS" "$BUSY_UNCONSOLIDATED_T03_ACCOUNT")
BPF_ARGS+=(--account "$BUSY_UNCONSOLIDATED_T04_ADDRESS" "$BUSY_UNCONSOLIDATED_T04_ACCOUNT")
BPF_ARGS+=(--account "$MISNAMED_UNCONSOLIDATED_POOL_ADDRESS" "$MISNAMED_UNCONSOLIDATED_POOL_ACCOUNT")
BPF_ARGS+=(--account "$MISNAMED_UNCONSOLIDATED_T03_ADDRESS" "$MISNAMED_UNCONSOLIDATED_T03_ACCOUNT")
BPF_ARGS+=(--account "$MISNAMED_UNCONSOLIDATED_T04_ADDRESS" "$MISNAMED_UNCONSOLIDATED_T04_ACCOUNT")
BPF_ARGS+=(--account "$VRF_QUEUE_ADDRESS" "$VRF_QUEUE_ACCOUNT")
BPF_ARGS+=(--account "$VRF_ORACLE_ADDRESS" "$VRF_ORACLE_ACCOUNT")
for pair in "${REVISION_RULE_SETS[@]:-}"; do
  [[ -z "$pair" ]] && continue
  revision_addr="${pair%%:*}"; revision_account="${pair##*:}"
  BPF_ARGS+=(--account "$revision_addr" "$revision_account")
done

echo "Starting validator (ledger: $LEDGER)…"
solana-test-validator --ledger "$LEDGER" --reset --quiet "${BPF_ARGS[@]}" \
  >/tmp/bye-fun-validator.log 2>&1 &
VALIDATOR_PID=$!
# Tear down the validator AND delete its ledger dir on exit — the ledger is a fresh
# mktemp dir each run (hundreds of MB), so without cleanup repeated runs pile up stale
# ledgers and eventually exhaust the disk (which degrades the validator mid-run).
trap 'kill "$VALIDATOR_PID" 2>/dev/null || true; rm -rf "$LEDGER_DIR" 2>/dev/null || true; rm -f "$TAP_LOG" 2>/dev/null || true' EXIT

# Wait for the validator to be FULLY ready — not just answering RPC. The RPC replies to
# cluster-version while the faucet is still "waiting for fees to stabilize", so
# airdrop-heavy tests race a not-ready validator and flake. A successful probe airdrop
# is the reliable readiness signal (faucet up + fees stable + slots advancing).
echo "Waiting for validator readiness (probe airdrop)…"
PROBE_KP="$(mktemp)"
solana-keygen new --no-bip39-passphrase --force -s -o "$PROBE_KP" >/dev/null 2>&1
PROBE_ADDR="$(solana-keygen pubkey "$PROBE_KP")"
READY=0
for _ in $(seq 1 90); do
  if solana airdrop 1 "$PROBE_ADDR" -u http://127.0.0.1:8899 >/dev/null 2>&1; then READY=1; break; fi
  sleep 1
done
rm -f "$PROBE_KP"
if [[ "$READY" != "1" ]]; then echo "validator not ready after 90s — aborting" >&2; exit 1; fi

# The RPC (8899) is up, but solana-kite awaits tx confirmation over the WebSocket
# subscription endpoint (8900), which stabilizes a few seconds later. Without this settle
# delay the before() hooks flake with "WebSocket connection closed".
echo "RPC ready; letting WebSocket (8900) stabilize…"
sleep 10

# Pre-fund the local suite admin (accounts/dev/deployer.json). Program config is
# expected to be init-once with `config.admin == signer` enforced on every admin
# instruction, so the whole suite shares this one local key.
DEPLOYER_ADDR="$(solana-keygen pubkey "$DEPLOYER")"
echo "Airdropping suite admin (deployer $DEPLOYER_ADDR)…"
solana airdrop 100 "$DEPLOYER_ADDR" -u http://127.0.0.1:8899 >/dev/null 2>&1

# The three protocol authorities, each its own key. They sign transactions in
# both the operator's own case (approve_deposit/reject_deposit) and the wrong-signer matrix's
# negative cases (T02/unprivileged signing an instruction they do not authorize), so each needs
# fee-paying SOL, not just an on-chain identity.
OPERATOR_ADDR="$(solana-keygen pubkey "$OPERATOR")"
T02_ADDR="$(solana-keygen pubkey "$T02")"
UNPRIVILEGED_ADDR="$(solana-keygen pubkey "$UNPRIVILEGED")"
echo "Airdropping suite authorities (operator $OPERATOR_ADDR, t02 $T02_ADDR, unprivileged $UNPRIVILEGED_ADDR)…"
solana airdrop 100 "$OPERATOR_ADDR" -u http://127.0.0.1:8899 >/dev/null 2>&1
solana airdrop 100 "$T02_ADDR" -u http://127.0.0.1:8899 >/dev/null 2>&1
solana airdrop 100 "$UNPRIVILEGED_ADDR" -u http://127.0.0.1:8899 >/dev/null 2>&1
echo "Validator ready."

echo "Running integration suite…"
TAP_LOG="$(mktemp)"
set +e
node --import tsx/esm --test --test-concurrency=1 'tests/integration/**/*.test.ts' 2>&1 | tee "$TAP_LOG"
RUNNER_STATUS=${PIPESTATUS[0]}
set -e

# Read the TAP summary with bash builtins only. `grep` and `head` are deliberately not
# used: both are shadowed on at least one machine that runs this (ugrep rejects patterns
# GNU grep accepts; `head` resolves to LWP's HTTP client), and a parser that silently
# matched nothing would turn this gate into the exact false-clean it exists to prevent.
tap_summary_field() {
  local field="$1" line value=""
  while IFS= read -r line; do
    case "$line" in
      "# $field "*) value="${line#"# $field "}" ;;
    esac
  done < "$TAP_LOG"
  printf '%s' "$value"
}

RAN="$(tap_summary_field tests)"
FAILED="$(tap_summary_field fail)"
# `cancelled` is read for the reason the other two are: a failed `before()` hook cancels every
# test in its file and node reports them as **cancelled, not failed** — `# tests 28 / # pass 23 /
# # fail 0 / # cancelled 5` from a fixture whose hook threw.
# Both existing checks pass in that state: `cancelled` tests are counted in `# tests`, so the
# count pin agrees, and `# fail` is genuinely 0 — so the gate printed its success line over a file
# that executed no assertions at all, and only `exit "$RUNNER_STATUS"` on the last line caught it.
# That is the gate deferring to the exit code, which is the one instrument this script exists
# because it could not trust: assert a test-count floor separately from the exit code.
CANCELLED="$(tap_summary_field cancelled)"

if [[ -z "$RAN" || -z "$FAILED" || -z "$CANCELLED" ]]; then
  echo "GATE: the runner emitted no TAP summary, so the suite reported no result at all." >&2
  exit 1
fi
if [[ "$RAN" -ne "$EXPECTED_INTEGRATION_TESTS" ]]; then
  echo "GATE: integration suite ran $RAN test(s), expected exactly $EXPECTED_INTEGRATION_TESTS." >&2
  exit 1
fi
if [[ "$FAILED" -ne 0 ]]; then
  echo "GATE: integration suite reported $FAILED failing test(s)." >&2
  exit 1
fi
if [[ "$CANCELLED" -ne 0 ]]; then
  echo "GATE: integration suite reported $CANCELLED cancelled test(s) — a before()/after() hook" >&2
  echo "threw, so those tests never ran. They are counted in \"# tests\" and are NOT counted in" >&2
  echo "\"# fail\", so the count and failure checks above both pass over them." >&2
  exit 1
fi

echo "Integration suite: $RAN test(s), 0 failing, 0 cancelled — matches the declared expectation."
exit "$RUNNER_STATUS"
