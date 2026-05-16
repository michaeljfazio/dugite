#!/usr/bin/env bash
# Shared helpers for local-devnet scripts.
# Source: . "$(dirname "$0")/lib/common.sh"
set -euo pipefail

# ---- Paths ----
LD_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LD_STATE="$LD_ROOT/state"
LD_LOGS="$LD_ROOT/logs"
LD_KEYS="$LD_ROOT/keys"
LD_GENESIS="$LD_ROOT/genesis"
LD_CONFIG="$LD_ROOT/config"
LD_EVIDENCE="$LD_ROOT/evidence"

# ---- Constants ----
LD_MAGIC=42
LD_RELAY_PORT=30000
LD_DUGITE_BP_PORT=30001
LD_CARDANO_BP_PORT=30003
LD_RELAY_SOCK="$LD_STATE/dugite-relay.sock"
LD_DUGITE_BP_SOCK="$LD_STATE/dugite-bp.sock"
LD_CARDANO_BP_SOCK="$LD_STATE/cardano-bp.sock"

# Dugite binary discovered relative to repo root (LD_ROOT/../..)
LD_REPO_ROOT="$(cd "$LD_ROOT/../.." && pwd)"
DUGITE_BIN="$LD_REPO_ROOT/target/release/dugite-node"

# ---- Logging ----
log_info() { printf '\033[0;32m[INFO]\033[0m  %s\n' "$*" >&2; }
log_warn() { printf '\033[0;33m[WARN]\033[0m  %s\n' "$*" >&2; }
log_error() { printf '\033[0;31m[ERROR]\033[0m %s\n' "$*" >&2; }
die() { log_error "$*"; exit 1; }

# ---- Version comparison: returns 0 if $1 >= $2 ----
version_ge() {
    [ "$(printf '%s\n%s' "$2" "$1" | sort -V | head -n1)" = "$2" ]
}

# ---- Prereq checks ----
check_prereqs() {
    command -v jq >/dev/null || die "jq not installed"
    command -v cardano-cli >/dev/null || die "cardano-cli not installed"
    command -v cardano-node >/dev/null || die "cardano-node not installed"
    [ -x "$DUGITE_BIN" ] || die "dugite-node binary not found at $DUGITE_BIN — run 'cargo build --release'"

    local cli_ver
    cli_ver="$(cardano-cli --version | awk 'NR==1 {print $2}')"
    version_ge "$cli_ver" "11.0.0" || die "cardano-cli $cli_ver < 11.0.0 required"

    local node_ver
    node_ver="$(cardano-node --version | awk 'NR==1 {print $2}')"
    version_ge "$node_ver" "11.0.1" || die "cardano-node $node_ver < 11.0.1 required"

    log_info "Prereqs OK (cardano-cli $cli_ver, cardano-node $node_ver, dugite-node at $DUGITE_BIN)"
}

# ---- Port availability ----
port_free() {
    local port="$1"
    ! lsof -iTCP:"$port" -sTCP:LISTEN -t >/dev/null 2>&1
}

assert_ports_free() {
    for p in "$LD_RELAY_PORT" "$LD_DUGITE_BP_PORT" "$LD_CARDANO_BP_PORT"; do
        port_free "$p" || die "Port $p is in use — stop the conflicting process first"
    done
}

# ---- Wait for a unix socket to appear and be queryable ----
wait_for_socket() {
    local sock="$1" timeout="${2:-90}"
    local i=0
    while [ $i -lt "$timeout" ]; do
        if [ -S "$sock" ]; then
            # Try a tip query — confirms the N2C handler is alive
            if cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$sock" >/dev/null 2>&1; then
                log_info "Socket $sock ready after ${i}s"
                return 0
            fi
        fi
        sleep 1
        i=$((i + 1))
    done
    die "Socket $sock did not become ready within ${timeout}s"
}

# ---- Query tip helpers ----
# Returns: "slot block_no hash era" on one line
query_tip_oneline() {
    local sock="$1"
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
        | jq -r '[.slot, .block, .hash, .era] | @tsv'
}

# Returns just the slot
query_slot() {
    local sock="$1"
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
        | jq -r '.slot'
}

# ---- Caffeinate wrapper (no-op on non-macOS) ----
caffeinate_if_macos() {
    if [ "$(uname)" = "Darwin" ]; then
        caffeinate -dimsu "$@"
    else
        "$@"
    fi
}

# Mark sourcing successful
LD_COMMON_LOADED=1
