#!/usr/bin/env bash
# Assert that no function in this program overflows the SBF stack frame.
#
# **`cargo build-sbf` exits 0 over this diagnostic.** It is emitted by the SBF backend before the
# link step and is worded "Error:", but it does not fail the build — `cargo build-sbf && echo ok`
# prints ok over a program the runtime may execute incorrectly. No exit code anywhere surfaces it,
# which is why this reads the build's own output instead of its status.
#
# A frame overflow is a runtime hazard, not a build-time nit: the backend's own wording is "may
# cause undefined behavior during execution". The instruction still builds, deploys, and usually
# runs — until the overflowing path is taken with the wrong argument sizes.
#
# ── The allowlist and why it is checked in both directions ───────────────────────────────────────
#
# `mpl_core`'s plugin-registry decoder overflows by 128 bytes. It is a dependency: this repo cannot
# fix it, and dropping MPL Core support to avoid it is not a trade anyone would make. It is
# accepted, by name, below.
#
# The allowlist is also the control. Every accepted symbol MUST appear in the log, or this script
# fails. Without that direction a typo in the pattern, a reworded diagnostic, or a log that never
# captured stderr would make every build look clean — a "no overflows found" verdict from a search
# that finds nothing is worth nothing. If an accepted overflow genuinely goes away (the dependency
# fixed it), delete its entry here; do not leave it listed.
#
# ── Measure with the right toolchain or the answer is about the toolchain ────────────────────────
#
# Which functions overflow depends on the platform-tools version. `SettleRoll::try_accounts`
# overflows by 576 bytes under v1.53 and fits under v1.54, on identical source — and a bare
# `cargo build-sbf` silently resolves to v1.53. Always pass `--tools-version v1.54`, as CI does;
# an overflow measured without it is a fact about v1.53, not about this program.
#
# Usage:  scripts/check-frame-overflow.sh <build-log>

set -euo pipefail

LOG="${1:-}"

# Mangled symbols carry a trailing hash that changes between builds, so each entry is matched as a
# substring of the symbol rather than compared whole.
ACCEPTED=(
  "mpl_core6hooked6plugin31registry_records_to_plugin_list"
)

if [[ -z "$LOG" ]]; then
  echo "usage: $0 <build-log>" >&2
  exit 2
fi
if [[ ! -f "$LOG" ]]; then
  echo "missing build log $LOG — nothing to read, which is not the same as a clean build" >&2
  exit 1
fi

MARKER="overflows the maximum allowed frame space"

# Every overflow line the build emitted, reduced to its mangled symbol. Built with a read loop
# rather than `mapfile`, which macOS's bash 3.2 does not have.
SYMBOLS=()
while IFS= read -r sym; do
  [[ -n "$sym" ]] && SYMBOLS+=("$sym")
done < <(grep -F "$MARKER" "$LOG" | sed -E 's/^.*Function ([^ ]+) overflows.*/\1/')

FAILED=0

# ── Direction 1: nothing overflows except what is accepted ───────────────────────────────────────
for sym in ${SYMBOLS[@]+"${SYMBOLS[@]}"}; do
  ok=0
  for allowed in "${ACCEPTED[@]}"; do
    [[ "$sym" == *"$allowed"* ]] && ok=1 && break
  done
  if [[ "$ok" -eq 0 ]]; then
    echo "FAIL: $sym overflows its SBF stack frame and is not an accepted overflow." >&2
    grep -F "$MARKER" "$LOG" | grep -F "$sym" >&2
    echo "" >&2
    echo "The build exited 0 over this. The backend's own wording is 'may cause undefined" >&2
    echo "behavior during execution' — treat it as a runtime defect on that instruction, not" >&2
    echo "as a warning. Shrink the frame (box large locals, split the function), or add the" >&2
    echo "symbol to ACCEPTED in this script with the reasoning for accepting it." >&2
    FAILED=1
  fi
done

# ── Direction 2: every accepted overflow is still there ──────────────────────────────────────────
for allowed in "${ACCEPTED[@]}"; do
  found=0
  for sym in ${SYMBOLS[@]+"${SYMBOLS[@]}"}; do
    [[ "$sym" == *"$allowed"* ]] && found=1 && break
  done
  if [[ "$found" -eq 0 ]]; then
    echo "FAIL: accepted overflow '$allowed' did not appear in $LOG." >&2
    echo "" >&2
    echo "This is the control for the check above. Either the diagnostic's wording changed and" >&2
    echo "the search now matches nothing — in which case a clean verdict from it is worthless —" >&2
    echo "or the dependency fixed its frame, in which case delete the entry from ACCEPTED." >&2
    echo "Do not trust a passing run until this is resolved." >&2
    FAILED=1
  fi
done

if [[ "$FAILED" -ne 0 ]]; then
  exit 1
fi

echo "OK: ${#SYMBOLS[@]} frame overflow(s) in $LOG, all accepted, and every accepted one present."
