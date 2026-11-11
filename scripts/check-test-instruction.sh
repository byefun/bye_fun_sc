#!/usr/bin/env bash
# Assert whether a built program carries the `testing`-gated resolution instruction.
#
# `test_resolve_batch` resolves a batch to caller-chosen randomness with no VRF signature. It is
# gated on the `testing` feature, and `lib.rs` makes `prod` + `testing` a compile error — but both
# of those constrain the *inputs* to a build. This checks the artifact that would actually be
# deployed, which is the only thing a deploy can be wrong about.
#
# **The signal is the instruction's context type name, not its discriminator.** Anchor dispatches
# on the first 8 bytes of `sha256("global:<name>")`, and searching for those bytes was the obvious
# check — it does not work. Measured on both artifacts: the discriminator appears in neither,
# because the generated dispatch compares it in pieces rather than holding it as a contiguous
# literal. What does appear, and only in a `testing` build, is `TestResolveBatch` — the name Anchor
# emits for the `msg!("Instruction: …")` log.
#
# That signal would disappear under the `no-log-ix-name` feature, and this script cannot tell that
# case from a clean artifact. The paired invocation below is what covers it: a `present` check over
# a build that was compiled WITH `testing` must succeed, or the search is not working and an
# `absent` result from it means nothing. Same shape as the checksum canary in
# run-integration-tests.sh — one control, both directions, nothing left permanently red.
#
# Usage:  scripts/check-test-instruction.sh <program.so> present|absent

set -euo pipefail

SO="${1:-}"
EXPECT="${2:-}"
# The `#[derive(Accounts)]` context type, which is what Anchor logs the instruction by.
MARKER="TestResolveBatch"

if [[ -z "$SO" || -z "$EXPECT" ]]; then
  echo "usage: $0 <program.so> present|absent" >&2
  exit 2
fi
if [[ "$EXPECT" != "present" && "$EXPECT" != "absent" ]]; then
  echo "expectation must be 'present' or 'absent', got '$EXPECT'" >&2
  exit 2
fi
if [[ ! -f "$SO" ]]; then
  echo "missing artifact $SO — nothing to check, which is not the same as a clean artifact" >&2
  exit 1
fi

FOUND=$(python3 - "$SO" "$MARKER" <<'PY'
import sys
so, marker = sys.argv[1], sys.argv[2]
with open(so, "rb") as f:
    print("yes" if marker.encode() in f.read() else "no")
PY
)

if [[ "$EXPECT" == "absent" && "$FOUND" == "yes" ]]; then
  echo "FAIL: $SO carries $MARKER." >&2
  echo "A shipped artifact with this instruction lets the Operator resolve a batch to randomness" >&2
  echo "they chose, with no VRF signature. Rebuild without the 'testing' feature." >&2
  exit 1
fi
if [[ "$EXPECT" == "present" && "$FOUND" == "no" ]]; then
  echo "FAIL: $SO does not carry $MARKER, but was built with the 'testing' feature." >&2
  echo "This is the control for the absence check: the search is not finding a marker that is" >&2
  echo "there, so an 'absent' verdict from it is worthless. Do not trust the paired check until" >&2
  echo "this passes — the marker may have moved, or 'no-log-ix-name' may be enabled." >&2
  exit 1
fi

echo "OK: $MARKER is $EXPECT in $SO."
