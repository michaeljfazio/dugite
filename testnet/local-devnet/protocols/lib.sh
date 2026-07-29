#!/usr/bin/env bash
# Shared library for adversarial N2N protocol tests (protocols/).
#
# Strategy: all tests speak raw Cardano N2N. CBOR frames are constructed as
# hex strings and written to the dugite-relay's N2N port by the vendored
# stdlib-only writer `tx-zoo/lib/raw-socket-send.py` (#923 — the previous
# socat dependency silently PASSED every case on hosts without socat).
# We then assert:
#   1. The connection is terminated (peer demoted / disconnected)
#   2. No panic in the logs
#   3. No ERROR-level log lines that are NOT in the allowlist
#
# Source: . "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
set -euo pipefail

_LIB_SELF="${BASH_SOURCE[0]:-$0}"
PROTO_DIR="$(cd "$(dirname "$_LIB_SELF")" && pwd)"
LD_ROOT="$(cd "$PROTO_DIR/.." && pwd)"
. "$LD_ROOT/lib/common.sh"

# ---- Configuration -----------------------------------------------------------
# Target: dugite-relay's N2N port (adversarial frames arrive via the relay)
ADV_TARGET_HOST="127.0.0.1"
ADV_TARGET_PORT="$LD_RELAY_PORT"

# Timeout in seconds for the connection to be terminated after sending bad data.
#
# Must EXCEED the node's handshake timeout (10s), not equal it. At 10s the
# probe raced the close and lost by ~3ms for the empty-payload case: dugite and
# cardano-node 11.0.1 both close at 10.003s (measured on the same host), so the
# old value failed a case where the two implementations are byte-identical —
# a harness bug, not a node gap, exactly like the cli-parity both-sides-failed
# rule. 15s leaves margin without masking a real leak (#924 kept the socket
# open indefinitely, which no timeout value would have hidden).
ADV_EXPECT_CLOSE_SEC="${ADV_EXPECT_CLOSE_SEC:-15}"

# Vendored raw-socket writer (shared with tx-zoo 08r, see #918/#923).
ADV_RAW_SEND="${ADV_RAW_SEND:-$LD_ROOT/tx-zoo/lib/raw-socket-send.py}"

# Output CSV for adversarial test results
ADV_CSV="${ADV_CSV:-$LD_ROOT/evidence/current/n2n-trace.csv}"

# Log file to monitor for panics / errors during adversarial tests
ADV_LOG="$LD_LOGS/dugite-relay.log"

# ---- Known-allowlisted ERROR patterns (not bugs) ----------------------------
# These are ERROR lines that dugite emits legitimately when rejecting bad peers.
ALLOWLISTED_ERRORS=(
    "connection reset by peer"
    "broken pipe"
    "unexpected end of input"
    "decode error"
    "handshake error"
    "refused to decode"
    "malformed"
    "oversized"
    "peer demoted"
    "connection closed"
    "refused connection"
)

# ---- Helpers -----------------------------------------------------------------

_adv_ensure_csv() {
    local dir; dir="$(dirname "$ADV_CSV")"
    mkdir -p "$dir"
    if [ ! -f "$ADV_CSV" ]; then
        echo "ts,protocol,msg_type,peer,dir,size_bytes,outcome,notes" > "$ADV_CSV"
    fi
}

adv_record() {
    local protocol="$1" msg_type="$2" outcome="$3" notes="${4:-}"
    _adv_ensure_csv
    printf '%s,%s,%s,%s,out,%s,%s,%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$protocol" "$msg_type" "${ADV_TARGET_HOST}:${ADV_TARGET_PORT}" \
        "${#notes}" "$outcome" "${notes//,/;}" \
        >> "$ADV_CSV"
    local icon
    case "$outcome" in
        REJECTED|CLOSED) icon="✓" ;;
        PANIC)           icon="💥" ;;
        SILENT_SKIP)     icon="✗" ;;
        ERROR)           icon="!" ;;
        PASS)            icon="✓" ;;
        *)               icon="?" ;;
    esac
    printf '[adversarial] %s %-20s %-30s %s\n' \
        "$icon" "$protocol" "$msg_type" "${notes:-}" >&2
}

# Check for panics in dugite-relay log since $since_line
adv_check_no_panic() {
    local since_line="$1"
    local log="$ADV_LOG"
    [ -f "$log" ] || return 0
    local total; total=$(wc -l < "$log")
    if [ "$total" -gt "$since_line" ]; then
        if awk "NR>$since_line" "$log" | grep -qiE 'panicked|PANIC|thread.*panicked'; then
            return 1
        fi
    fi
    return 0
}

# Get current line count of relay log (for before/after comparison)
adv_log_line() {
    [ -f "$ADV_LOG" ] && wc -l < "$ADV_LOG" || echo 0
}

# Write hex CBOR to the target and expect the peer to close within ADV_EXPECT_CLOSE_SEC
# Returns 0 if connection closed (as expected), 1 if it stayed open (bad)
adv_send_expect_close() {
    local hex_payload="$1"
    local timeout="${2:-$ADV_EXPECT_CLOSE_SEC}"

    # #923: the old socat path returned 0 (a PASS) when socat was missing, so
    # every adversarial case in 01-07 recorded REJECTED without sending a
    # byte. Now uses the vendored stdlib-only writer (python3 is a hard
    # harness dependency); a missing writer is a loud failure, never a pass.
    if [ ! -f "$ADV_RAW_SEND" ]; then
        log_error "raw-socket-send.py not found at $ADV_RAW_SEND — adversarial send cannot run"
        return 2
    fi

    local rc=0
    python3 "$ADV_RAW_SEND" --tcp "${ADV_TARGET_HOST}:${ADV_TARGET_PORT}" \
        --hex "$hex_payload" --read-timeout "$timeout" --expect-close \
        > /dev/null 2>&1 || rc=$?
    # exit 0 = sent, peer closed within timeout (expected reaction)
    # exit 3 = peer left the connection open → adversarial frame tolerated
    # exit 2 = could not connect/send → infrastructure failure, not a pass
    [ "$rc" -eq 0 ]
}

# Check that no new non-allowlisted ERROR lines appeared since $since_line
adv_check_no_new_errors() {
    local since_line="$1"
    local log="$ADV_LOG"
    [ -f "$log" ] || return 0
    local new_errors
    new_errors=$(awk "NR>$since_line" "$log" | grep -iE 'ERROR' | grep -viE "$(IFS='|'; echo "${ALLOWLISTED_ERRORS[*]}")" || true)
    [ -z "$new_errors" ]
}

# Assert devnet is up and relay socket is reachable
adv_require_devnet() {
    if ! lsof -iTCP:"$ADV_TARGET_PORT" -sTCP:LISTEN -P -n 2>/dev/null | grep -q LISTEN; then
        log_error "dugite-relay N2N port $ADV_TARGET_PORT not listening — is the devnet running?"
        exit 2
    fi
}
