#!/usr/bin/env bash
# Shared helpers for chaos engineering scenarios.
# Source: . "$(dirname "$0")/lib.sh"
set -euo pipefail

_CHAOS_SELF="${BASH_SOURCE[0]:-$0}"
CHAOS_DIR="$(cd "$(dirname "$_CHAOS_SELF")" && pwd)"
. "$CHAOS_DIR/../lib/common.sh"

EVIDENCE_DIR="${EVIDENCE_DIR:-$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$EVIDENCE_DIR"
CHAOS_CSV="$EVIDENCE_DIR/chaos-events.csv"
[ -f "$CHAOS_CSV" ] || echo "ts,scenario,action,recovery_seconds,result,detail" > "$CHAOS_CSV"

# Detect OS for network manipulation tool selection
CHAOS_OS="$(uname -s)"
case "$CHAOS_OS" in
    Darwin) CHAOS_NET_TOOL="pfctl" ;;
    Linux)  CHAOS_NET_TOOL="iptables" ;;
    *)      CHAOS_NET_TOOL="unknown" ;;
esac

# count_matching <extended-regex> <file> [tail_lines] -> a single integer, always
#
# Replaces the `... | grep -c RE || echo 0` idiom, which is BROKEN: grep -c
# prints its count (`0`) AND exits 1 when there are no matches, so the `|| echo
# 0` appends a SECOND line and the variable becomes the two-line string "0\n0".
# Every arithmetic test on it then dies with "integer expected" and the script
# falls through — which is how clock-skew and inbound-syn-flood came to report
# PASS from a comparison that never evaluated.
#
# This is the same defect the release-report generator carried (#953 fix), and
# it recurs because the idiom reads correct. awk cannot fail this way: it has
# no match-based exit status.
count_matching() {
    local re="$1" file="$2" tail_n="${3:-}"
    [ -f "$file" ] || { echo 0; return 0; }
    if [ -n "$tail_n" ]; then
        tail -n "$tail_n" "$file" 2>/dev/null | awk -v re="$re" 'BEGIN{c=0} $0 ~ re {c++} END{print c+0}'
    else
        awk -v re="$re" 'BEGIN{c=0} $0 ~ re {c++} END{print c+0}' "$file" 2>/dev/null || echo 0
    fi
}

# line_count <file> -> a single integer, always (0 when the file is absent)
line_count() {
    [ -f "$1" ] || { echo 0; return 0; }
    awk 'END{print NR+0}' "$1" 2>/dev/null || echo 0
}

chaos_record() {
    local scenario="$1" action="$2" recovery="${3:-0}" result="${4:-UNKNOWN}" detail="${5:-}"
    printf '%s,%s,%s,%s,%s,%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$scenario" "$action" "$recovery" "$result" "${detail//,/;}" \
        >> "$CHAOS_CSV"
}

# Verify ChainDB integrity by checking that the running node's tip is sane
chaos_verify_chaindb() {
    local socket="${1:-$LD_DUGITE_BP_SOCK}"
    local tip
    tip=$(cardano-cli query tip \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$socket" 2>/dev/null | jq -r '.block // "?"' || echo "?")
    if [ "$tip" = "?" ] || [ "$tip" = "0" ]; then
        log_warn "chaindb-verify: tip query returned $tip"
        return 1
    fi
    log_info "chaindb-verify OK: tip_block=$tip"
    return 0
}

# Wait for dugite-bp socket to be responsive, up to $2 seconds
chaos_wait_for_socket() {
    local socket="${1:-$LD_DUGITE_BP_SOCK}"
    local timeout="${2:-60}"
    local elapsed=0
    while [ "$elapsed" -lt "$timeout" ]; do
        if cardano-cli query tip \
                --testnet-magic "$LD_MAGIC" \
                --socket-path   "$socket" >/dev/null 2>&1; then
            return 0
        fi
        sleep 2
        elapsed=$(( elapsed + 2 ))
    done
    return 1
}

# Require the net tool is present, skip-with-warning if not
chaos_require_net_tool() {
    if ! command -v "$CHAOS_NET_TOOL" >/dev/null 2>&1; then
        log_warn "Network chaos tool '$CHAOS_NET_TOOL' not available on $CHAOS_OS — skipping network partition tests"
        return 1
    fi
    return 0
}

# Require caffeinate is present on macOS
chaos_require_caffeinate() {
    if [ "$CHAOS_OS" = "Darwin" ] && ! command -v caffeinate >/dev/null 2>&1; then
        log_error "caffeinate not found on macOS — this is required to prevent App Nap"
        return 1
    fi
    return 0
}
