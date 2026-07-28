#!/usr/bin/env bash
# Shared helpers for local-devnet scripts.
# Source: . "$(dirname "$0")/lib/common.sh"
set -euo pipefail

# ---- Paths ----
# BASH_SOURCE may be unset under zsh or when interactively sourced; fall back
# to $0 (caller's script path) in that case. Both targets resolve to .../lib/common.sh.
_LD_SELF="${BASH_SOURCE[0]:-$0}"
LD_ROOT="$(cd "$(dirname "$_LD_SELF")/.." && pwd)"
LD_STATE="$LD_ROOT/state"
LD_LOGS="$LD_ROOT/logs"
LD_KEYS="$LD_ROOT/keys"
LD_GENESIS="$LD_ROOT/genesis"
LD_CONFIG="$LD_ROOT/config"
LD_EVIDENCE="$LD_ROOT/evidence"

# ---- Constants ----
LD_MAGIC=42
# N2N ports — single-digit-incremented from the standard Cardano BP port so
# they are easy to remember. dugite-bp keeps 3001 (Cardano default) so that
# `dugite-monitor` and `cardano-cli` connect without overrides.
LD_DUGITE_BP_PORT=3001
LD_RELAY_PORT=3002
LD_CARDANO_BP_PORT=3003
# Prometheus metrics ports — single-digit-incremented from 12798 (the
# dugite-monitor default). dugite-bp keeps 12798. The relay gets 12799 to
# avoid the two dugite processes fighting for the same listener.
LD_DUGITE_BP_METRICS_PORT=12798
LD_DUGITE_RELAY_METRICS_PORT=12799
# Unix-domain socket paths.
#
# macOS imposes a 104-byte SUN_LEN limit on sun_path; both the worktree path
# and the default $TMPDIR (a long `/var/folders/...` path on macOS) blow past
# that. Place sockets under /tmp/ld-<uid>/ which is short and per-user.
LD_SOCK_DIR="/tmp/ld-$(id -u)"
mkdir -p "$LD_SOCK_DIR" 2>/dev/null || true
LD_RELAY_SOCK="$LD_SOCK_DIR/relay.sock"
LD_DUGITE_BP_SOCK="$LD_SOCK_DIR/dbp.sock"
LD_CARDANO_BP_SOCK="$LD_SOCK_DIR/cbp.sock"

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
        exec caffeinate -dimsu "$@"
    else
        exec "$@"
    fi
}

# Resolve the REAL dugite-node pid for a given --database-path and write it to
# a pidfile.
#
# Why this exists: `caffeinate_if_macos ... &` backgrounds a shell FUNCTION, so
# `$!` is the pid of the subshell running that function — not caffeinate and not
# the node. On macOS the recorded pid was therefore stale within moments, and
# `kill "$(cat state/dugite-bp.pid)"` — the documented Round 3 restart step —
# silently killed nothing. Round 3 then "passed" while the node had never gone
# down, i.e. it never tested restart resilience at all.
#
# `exec` above makes the subshell BECOME caffeinate (or the node on Linux), and
# this helper additionally resolves the actual `dugite-node run` process so the
# pidfile always names something a SIGTERM will stop. Never SIGKILL a dugite
# node: it corrupts the append-only ImmutableDB.
write_node_pidfile() {
    local db_path="$1" pidfile="$2" tries=0 pid=""
    while [ "$tries" -lt 50 ]; do
        # Prefer the bare node process over the caffeinate wrapper.
        pid=$(pgrep -f "dugite-node run .*--database-path $db_path" 2>/dev/null \
              | while read -r p; do
                    case "$(ps -o command= -p "$p" 2>/dev/null)" in
                        caffeinate*) ;;
                        *) echo "$p" ;;
                    esac
                done | head -n 1)
        [ -n "$pid" ] && { echo "$pid" > "$pidfile"; return 0; }
        tries=$((tries + 1))
        sleep 0.2
    done
    return 1
}

# Mark sourcing successful
LD_COMMON_LOADED=1
