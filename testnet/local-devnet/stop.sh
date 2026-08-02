#!/usr/bin/env bash
# Stop dugite-relay, dugite-bp, cardano-bp. Preserves DBs and logs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/common.sh"

log_info "=== Local devnet stop ==="

# Collect a PID and all of its descendants, in stop order (children first).
# Needed because dugite-node is wrapped in `caffeinate -dimsu` on macOS:
# signalling caffeinate alone does NOT propagate to the child dugite-node,
# which would survive and orphan itself to PID 1.
collect_descendants() {
    local root="$1"
    local descendants
    # pgrep -P lists immediate children; recurse.
    descendants="$(pgrep -P "$root" 2>/dev/null || true)"
    local child
    for child in $descendants; do
        collect_descendants "$child"
    done
    echo "$root"
}

stop_one() {
    local name="$1"
    local pidfile="$LD_STATE/$name.pid"
    if [ ! -f "$pidfile" ]; then
        log_warn "$name: no pidfile at $pidfile — already stopped?"
        return 0
    fi
    local root_pid
    root_pid="$(cat "$pidfile")"
    if ! kill -0 "$root_pid" 2>/dev/null; then
        log_warn "$name: PID $root_pid not alive — already stopped"
        rm -f "$pidfile"
        return 0
    fi

    # Snapshot the whole tree before signalling — children may detach once
    # their parent dies, and we want to kill them anyway.
    local pids
    pids="$(collect_descendants "$root_pid")"
    log_info "$name: SIGTERM tree from $root_pid (pids: $(echo $pids | tr '\n' ' '))"
    local p
    for p in $pids; do
        kill -TERM "$p" 2>/dev/null || true
    done

    # Wait up to 5s for the root PID to exit.
    for _ in 1 2 3 4 5; do
        kill -0 "$root_pid" 2>/dev/null || break
        sleep 1
    done

    # Then SIGKILL any survivor across the original snapshot.
    for p in $pids; do
        if kill -0 "$p" 2>/dev/null; then
            log_warn "$name: PID $p did not exit after SIGTERM — SIGKILL"
            kill -KILL "$p" 2>/dev/null || true
        fi
    done

    rm -f "$pidfile"
    log_info "$name stopped"
}

stop_one dugite-bp
stop_one dugite-relay
stop_one cardano-bp
# Only present in two-forger mode (#957); stop_one is a no-op without a pidfile.
stop_one cardano-arbiter

rm -f "$LD_SOCK_DIR"/*.sock

log_info "Done. State preserved at $LD_STATE; logs at $LD_LOGS."
