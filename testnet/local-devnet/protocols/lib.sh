#!/usr/bin/env bash
# Shared library for adversarial N2N protocol tests (protocols/).
#
# Strategy: all tests speak raw Cardano N2N via socat or nc.
# CBOR frames are constructed as hex strings and piped via socat to the
# dugite-relay's N2N port.  We then assert:
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

# Timeout in seconds for a socat connection to be terminated after sending bad data
ADV_EXPECT_CLOSE_SEC="${ADV_EXPECT_CLOSE_SEC:-10}"

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

# Send hex CBOR via socat and expect the connection to close within ADV_EXPECT_CLOSE_SEC
# Returns 0 if connection closed (as expected), 1 if it stayed open (bad)
adv_send_expect_close() {
    local hex_payload="$1"
    local timeout="${2:-$ADV_EXPECT_CLOSE_SEC}"

    if ! command -v socat >/dev/null 2>&1; then
        log_warn "socat not found — skipping adversarial frame send test"
        return 0  # skip, not fail
    fi

    local tmp; tmp=$(mktemp)
    printf '%s' "$hex_payload" | xxd -r -p > "$tmp" 2>/dev/null || {
        printf '%s' "$hex_payload" | python3 -c "import sys,binascii; sys.stdout.buffer.write(binascii.unhexlify(sys.stdin.read().strip()))" > "$tmp"
    }

    local rc=0
    # socat: send binary payload, wait up to $timeout for close
    timeout "$timeout" socat - "TCP:${ADV_TARGET_HOST}:${ADV_TARGET_PORT}" < "$tmp" > /dev/null 2>&1 \
        && rc=0 || rc=$?
    rm -f "$tmp"

    # rc=124 = timeout (connection NOT closed) → bad
    # rc=0 or rc=1 = connection closed (expected) → good
    [ "$rc" -ne 124 ] && return 0 || return 1
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
