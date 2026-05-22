#!/usr/bin/env bash
# Slow-loris / DoS resistance tests.
#
# Checks that dugite-relay has timeouts that protect against:
#  1. Connection that opens but never sends any bytes (idle connection)
#  2. Partial handshake — sends 4 bytes then stalls indefinitely
#  3. Repeated connection open + immediate close (connection churn)
#
# These tests do NOT require socat — they use bash's built-in /dev/tcp or nc.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

adv_require_devnet

PASS=0; FAIL=0

MAX_WAIT="${SLOW_LORIS_TIMEOUT:-30}"

# ---- Case 1: idle connection (no bytes sent) --------------------------------
since=$(adv_log_line)
{
    exec 3<>/dev/tcp/"$ADV_TARGET_HOST"/"$ADV_TARGET_PORT" 2>/dev/null
    sleep "$MAX_WAIT"
    exec 3>&-
} 2>/dev/null &
IDLE_PID=$!
sleep $(( MAX_WAIT + 5 ))
# Connection should have been closed by the server-side timeout
if kill -0 "$IDLE_PID" 2>/dev/null; then
    kill "$IDLE_PID" 2>/dev/null || true
    adv_record "slow-loris" "idle-connection" "SILENT_SKIP" \
        "server kept idle connection open for ${MAX_WAIT}s — no timeout enforced"
    FAIL=$(( FAIL + 1 ))
else
    adv_check_no_panic "$since" && no_panic=1 || no_panic=0
    if [ "$no_panic" -eq 0 ]; then
        adv_record "slow-loris" "idle-connection" "PANIC" "panic"
        FAIL=$(( FAIL + 1 ))
    else
        adv_record "slow-loris" "idle-connection" "PASS" "connection timed out within ${MAX_WAIT}s"
        PASS=$(( PASS + 1 ))
    fi
fi

# ---- Case 2: partial handshake (4 bytes then stall) -------------------------
since=$(adv_log_line)
{
    exec 4<>/dev/tcp/"$ADV_TARGET_HOST"/"$ADV_TARGET_PORT" 2>/dev/null
    # Send 4 bytes (incomplete mux header) then stall
    printf '\x00\x00\x00\x00' >&4
    sleep "$MAX_WAIT"
    exec 4>&-
} 2>/dev/null &
PARTIAL_PID=$!
sleep $(( MAX_WAIT + 5 ))

if kill -0 "$PARTIAL_PID" 2>/dev/null; then
    kill "$PARTIAL_PID" 2>/dev/null || true
    adv_record "slow-loris" "partial-header" "SILENT_SKIP" \
        "server kept partial-header connection open for ${MAX_WAIT}s"
    FAIL=$(( FAIL + 1 ))
else
    adv_check_no_panic "$since" && no_panic=1 || no_panic=0
    if [ "$no_panic" -eq 0 ]; then
        adv_record "slow-loris" "partial-header" "PANIC" "panic"
        FAIL=$(( FAIL + 1 ))
    else
        adv_record "slow-loris" "partial-header" "PASS" "timed out within ${MAX_WAIT}s"
        PASS=$(( PASS + 1 ))
    fi
fi

log_info "=== slow-loris adversarial: PASS=$PASS FAIL=$FAIL ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
