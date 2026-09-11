#!/usr/bin/env bash
# Integration test runner: boots a fresh solana-test-validator with the program loaded
# at its declared id, runs the integration suite, then tears the validator down.
#
# Why a wrapper (not bare `pnpm run test:integration`): the runtime tests need (a) a
# validator, and (b) a PRISTINE ledger each run, because program config accounts are
# expected to be init-once singletons. The .so is loaded with the DECLARED-id keypair
# (accounts/dev/bye_machine-keypair.json) so `declare_id!` matches — the keypair cargo
# generates under target/deploy is a throwaway and would land the program at the wrong
# address.
#
# SCAFFOLD NOTE: programs/ is empty until the on-chain design lands, so this script
# exits with a clear message until `target/deploy/bye_machine.so` exists.
set -euo pipefail
cd "$(dirname "$0")/.."

PROGRAM_ID="4exRPJN7N8MVZh37efYAkwnGGxoJKMVUARQqf7awmYkJ"
SO="target/deploy/bye_machine.so"
DEPLOYER="accounts/dev/deployer.json"
# T-88a: the suite's four protocol authorities, distinct from DEPLOYER and from each other — see
# tests/helpers/protocol.ts's ensureProtocol and tests/helpers/env.ts's load{Operator,T03Authority,
# T02Authority,Unprivileged}. Committed, not bootstrap-rotated: rotation would make the live
# operator depend on runner file order under --test-concurrency=1.
OPERATOR="accounts/dev/operator.json"
T03="accounts/dev/t03.json"
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
# tests/fixtures/programs/, and listed here as "<program_id>:<path>" pairs. No MagicBlock
# VRF and no AMM (contract-test-plan.md's "Cloned programs" row: slice 1 issues no VRF
# request). MPL Core joined the list at T-105, which is the first task with anything to
# execute against it — every earlier Core "fixture" was a system-created account assigned
# to the program, which proves an owner check and nothing about the program's own behaviour.
#   metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s  Token Metadata (pNFT)
#   auth9SigNpDKz4sJJ1DfCTuZrZNSAgh9sFD3rboVmgg  Token Auth Rules
FIXTURES=(
  "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s:tests/fixtures/programs/metadata.so"
  "auth9SigNpDKz4sJJ1DfCTuZrZNSAgh9sFD3rboVmgg:tests/fixtures/programs/auth-rules.so"
  "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d:tests/fixtures/programs/mpl-core.so"
)

# The rule-set ACCOUNT every eligible pNFT's programmable_config.rule_set points at —
# loaded as data, not code, via --account (not --bpf-program). Entry criterion 3 is
# explicit that the account itself, not just the Token Auth Rules program, must be
# present: it is what the escrow path's Validate CPI actually reads.
RULE_SET_ADDRESS="eBJLFYPxJmMGKuFwpDWkzxZeUrad92kZRC5BJLpzyT9"
RULE_SET_ACCOUNT="tests/fixtures/programs/rule-set.json"

# T-48 (D-041 replay): 9 synthetic rule-set accounts, one per historical revision the live
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
CANARY_SHA256="d632df8de37679d3ad20afeb3ee50d424617f22a73fa707f4d46fe55ed8974db"

# --- Integration-suite liveness gate ------------------------------------------------
# `node --test` over a glob matching nothing prints "1..0 / # tests 0" and exits 0. That
# command is the last one in this script, so a renamed directory, a typo'd glob or a tsx
# resolution failure each read as a PASSING integration run. Exit status alone cannot
# tell "everything passed" from "nothing ran", and ci.yml runs only `typecheck` and the
# unit suite -- the one place the integration evidence is produced has no other backstop.
#
# Both are exact equalities, never >=. A >= floor accepts a suite that silently shrank,
# which is precisely the failure this gate exists to catch.
EXPECTED_INTEGRATION_TESTS=86  # T-47's 7 + T-48's 10 (rule-set-replay.test.ts) + G3's 4 + T-89's 5
                                # (pool-profiles.test.ts) + G1's 2 (return-rejected-custody.test.ts)
                                # + T-112's 1 + P9's 3 (collection-admission.test.ts)
                                # + T-102's 3 (legacy-standard-deposit.test.ts)
                                # + T-103's 5 (legacy-standard-exit.test.ts)
                                # + T-105's 3 (core-accountless-newowner.test.ts)
                                # + T-113's 5 (core-standard-deposit.test.ts)
                                # + T-109's 3 (core-standard-exit.test.ts)
                                # + T-109's 2 (core-seizure.test.ts)
                                # + T-110's 9 (core-seizure-matrix.test.ts)
                                # + T-110's 12 (standard-matrix.test.ts)
                                # + the collection-level freeze review fix's 4
                                #   (core-collection-freeze.test.ts)
                                # + the plugin-census review fix's 5
                                #   (core-transfer-refused.test.ts)
                                # + issue #7 / D-156's 3 (bootstrap-init-authority.test.ts —
                                #   two refusals plus the pinned-administrator positive control)
EXPECTED_FIXTURES=3            # Token Metadata + Token Auth Rules + MPL Core — raised together with FIXTURES above

if [[ ! -f "$SO" ]]; then
  echo "missing $SO — no program has been built yet." >&2
  echo "This repo is a scaffold: implement programs/bye_machine first, then run:" >&2
  echo "  docker compose run --rm build   # or: anchor build" >&2
  exit 1
fi
if [[ ! -f "$DEPLOYER" ]]; then
  echo "missing $DEPLOYER — required committed suite admin key" >&2
  exit 1
fi
for kp in "$OPERATOR" "$T03" "$T02" "$UNPRIVILEGED"; do
  if [[ ! -f "$kp" ]]; then
    echo "missing $kp — required committed suite authority key (T-88a)" >&2
    exit 1
  fi
done

# --- Fixture checksum verification -------------------------------------------------
# Runs before the liveness gate below (and before FIXTURES is even read) on purpose:
# with EXPECTED_INTEGRATION_TESTS still 0 the gate refuses every run, so this is the
# one piece of T-46 that must execute — and be provably exercised, both directions —
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
verify_fixture "$RULE_SET_ACCOUNT"

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
for f in tests/fixtures/programs/*; do
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
  echo "constant with the first one (T-46+). Refusing to report success on zero evidence." >&2
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

# Pre-fund the committed suite admin (accounts/dev/deployer.json). Program config is
# expected to be init-once with `config.admin == signer` enforced on every admin
# instruction, so the whole suite shares this one committed key.
DEPLOYER_ADDR="$(solana-keygen pubkey "$DEPLOYER")"
echo "Airdropping suite admin (deployer $DEPLOYER_ADDR)…"
solana airdrop 100 "$DEPLOYER_ADDR" -u http://127.0.0.1:8899 >/dev/null 2>&1

# T-88a: the four protocol authorities, each its own committed key. They sign transactions in
# both the operator's own case (approve_deposit/reject_deposit) and the wrong-signer matrix's
# negative cases (T02/T03/unprivileged signing an instruction they do not authorize), so each
# needs fee-paying SOL, not just an on-chain identity.
OPERATOR_ADDR="$(solana-keygen pubkey "$OPERATOR")"
T03_ADDR="$(solana-keygen pubkey "$T03")"
T02_ADDR="$(solana-keygen pubkey "$T02")"
UNPRIVILEGED_ADDR="$(solana-keygen pubkey "$UNPRIVILEGED")"
echo "Airdropping suite authorities (operator $OPERATOR_ADDR, t03 $T03_ADDR, t02 $T02_ADDR, unprivileged $UNPRIVILEGED_ADDR)…"
solana airdrop 100 "$OPERATOR_ADDR" -u http://127.0.0.1:8899 >/dev/null 2>&1
solana airdrop 100 "$T03_ADDR" -u http://127.0.0.1:8899 >/dev/null 2>&1
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
# # fail 0 / # cancelled 5`. Measured on this tree, 2026-09-07, from a fixture whose hook threw.
# Both existing checks pass in that state: `cancelled` tests are counted in `# tests`, so the
# count pin agrees, and `# fail` is genuinely 0 — so the gate printed its success line over a file
# that executed no assertions at all, and only `exit "$RUNNER_STATUS"` on the last line caught it.
# That is the gate deferring to the exit code, which is the one instrument this script exists
# because it could not trust (T-81: "assert a test-count floor separately from the exit code").
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
