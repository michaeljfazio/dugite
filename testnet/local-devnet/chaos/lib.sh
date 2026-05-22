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
