#!/usr/bin/env bash
# Self-test + RED proofs for lib/expect-log-errors.sh (#1041).
#
# Pins, in order:
#   1. expect_log_errors finds a declared fault diagnostic after the mark.
#   2. RED: expect_log_errors for a pattern that never fired FAILS.
#   3. assert_no_other_errors passes when every error-class line in the window
#      is allowlisted.
#   4. RED: a seeded un-allowlisted ERROR line makes assert_no_other_errors FAIL.
#   5. Marks bound the window: a pre-mark error is invisible to both assertions.
#   6. Agreement (#916 shape): the lib classifies exactly the lines
#      count_log_errors counts — same fixture, same number, by construction
#      from the shared LOG_ERROR_PATTERN, pinned here so a fork of the pattern
#      fails this test.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/expect-log-errors.sh"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

LOG="$TMP/dugite-bp.log"
ALLOW="$TMP/allow.errors"

FAIL=0
check() { # check <label> <got> <want>
    if [ "$2" != "$3" ]; then echo "FAIL: $1 — got $2, want $3"; FAIL=1; else echo "ok:   $1"; fi
}

# Pre-mark content: one real ERROR that must be OUTSIDE every window below.
cat > "$LOG" <<'EOF'
2026-01-01T00:00:00Z ERROR dugite_node: pre-mark error that no assertion may see
2026-01-01T00:00:01Z INFO dugite_node: boot complete error_count=0
EOF
MARK=$(log_mark "$LOG")
check "log_mark counts existing lines" "$MARK" "2"
check "log_mark of absent file is 0" "$(log_mark "$TMP/nope.log")" "0"

# Post-mark content: the declared fault diagnostic + an allowlisted teardown
# error + INFO decoys carrying the substring "error" (must not count, #916).
cat >> "$LOG" <<'EOF'
2026-01-01T00:10:00Z INFO dugite_node: forge loop tick error=none
2026-01-01T00:10:01Z ERROR dugite_consensus::forge: KES period exhausted, cannot evolve key
2026-01-01T00:10:02Z ERROR dugite_network: connection reset by peer during teardown
2026-01-01T00:10:03Z INFO dugite_node: peer mentioned an error in passing
EOF

# 1. Declared diagnostic is found.
if expect_log_errors "$LOG" "$MARK" 'KES period exhausted'; then
    echo "ok:   expect_log_errors finds the declared diagnostic"
else
    echo "FAIL: expect_log_errors missed a present pattern"; FAIL=1
fi

# 2. RED: a pattern that never fired must FAIL.
if expect_log_errors "$LOG" "$MARK" 'pattern that never fired' 2>/dev/null; then
    echo "FAIL: expect_log_errors passed for an absent pattern"; FAIL=1
else
    echo "ok:   RED — absent pattern fails expect_log_errors"
fi

# 3. Fully-allowlisted window passes.
cat > "$ALLOW" <<'EOF'
# teardown noise expected while the peer is being killed
reset by peer
# the fault this round injects on purpose
KES period exhausted
EOF
if assert_no_other_errors "$LOG" "$MARK" "$ALLOW"; then
    echo "ok:   assert_no_other_errors passes on a fully-allowlisted window"
else
    echo "FAIL: allowlisted window was rejected"; FAIL=1
fi

# 4. RED: seed an un-allowlisted ERROR → must FAIL.
echo '2026-01-01T00:20:00Z ERROR dugite_ledger: seeded bogus error for the RED proof' >> "$LOG"
if assert_no_other_errors "$LOG" "$MARK" "$ALLOW" 2>/dev/null; then
    echo "FAIL: seeded un-allowlisted ERROR was not detected"; FAIL=1
else
    echo "ok:   RED — seeded un-allowlisted ERROR fails assert_no_other_errors"
fi

# 5. The pre-mark ERROR is invisible: an empty allowlist over a mark taken NOW
#    (end of file) sees nothing, even though the file contains two un-allowlisted
#    ERRORs before it.
: > "$ALLOW.empty"
if assert_no_other_errors "$LOG" "$(log_mark "$LOG")" "$ALLOW.empty"; then
    echo "ok:   marks bound the window (pre-mark errors invisible)"
else
    echo "FAIL: pre-mark error leaked into a later window"; FAIL=1
fi

# 6. Agreement with the shared counter: with mark=0 the lib's window is the
#    whole file, so its error-class line count must equal count_log_errors.
LIB_COUNT=$(tail -n +1 "$LOG" | grep -cE "$LOG_ERROR_PATTERN" || true)
check "lib window count == count_log_errors" "$LIB_COUNT" "$(count_log_errors "$LOG")"

# 7. Loud failure on a missing allowlist path (fail, not fail-open).
if assert_no_other_errors "$LOG" 0 "$TMP/typo.errors" 2>/dev/null; then
    echo "FAIL: missing allowlist file passed silently"; FAIL=1
else
    echo "ok:   missing allowlist path fails loudly"
fi

[ "$FAIL" -eq 0 ] && echo "PASS: expect-log-errors contract holds" || echo "FAILURES present"
exit "$FAIL"
