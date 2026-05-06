#!/usr/bin/env bash
# Watchdog: keeps cardano-node relay alive. If it exits unexpectedly,
# rebuilds it with the same topology and continues. Logs each restart cycle.
#
# Use this for long unattended soak runs where the relay's BadConfiguration
# crash (cardano-node 10.6.2 + ledger peers + connection-timeout cascade)
# might fire without immediate human attention.
set -uo pipefail
cd "$(dirname "$0")/.."

LOG_DIR=./logs/bp-pair
mkdir -p "$LOG_DIR"
WATCHDOG_LOG="$LOG_DIR/watchdog-$(date +%Y%m%d-%H%M%S).log"
ln -sf "$(basename "$WATCHDOG_LOG")" "$LOG_DIR/watchdog.current.log"

emit() {
    local ts; ts=$(date '+%Y-%m-%d %H:%M:%S')
    echo "[$ts] $*" | tee -a "$WATCHDOG_LOG"
}

emit "WATCHDOG START — supervising cardano-node relay"

CYCLE=0
while true; do
    CYCLE=$((CYCLE + 1))

    if pgrep -f "cardano-node run" > /dev/null 2>&1; then
        emit "CYCLE $CYCLE — relay already running, skipping spawn"
    else
        ts=$(date +%Y%m%d-%H%M%S)
        new_relay_log="$LOG_DIR/relay-${ts}.log"
        emit "CYCLE $CYCLE — launching cardano-node relay → $new_relay_log"
        rm -f ./haskell-node.sock 2>/dev/null || true
        nohup cardano-node run \
            --config           config/haskell-preview-config.json \
            --topology         config/haskell-relay-single-bp-topology.json \
            --database-path    ./db-haskell \
            --socket-path      ./haskell-node.sock \
            --host-addr        0.0.0.0 \
            --port             3002 \
            > "$new_relay_log" 2>&1 &
        local_pid=$!
        echo "$local_pid" > "$LOG_DIR/relay.pid"
        ln -sf "$(basename "$new_relay_log")" "$LOG_DIR/relay.current.log"
        emit "CYCLE $CYCLE — relay PID $local_pid"
    fi

    # Wait for the relay to die
    relay_pid=$(pgrep -f "cardano-node run" || true)
    if [[ -z "$relay_pid" ]]; then
        emit "CYCLE $CYCLE — could not find relay PID, retrying in 10s"
        sleep 10
        continue
    fi

    # Block until process exits
    while kill -0 "$relay_pid" 2>/dev/null; do
        sleep 30
    done

    emit "CYCLE $CYCLE — relay PID $relay_pid exited; will restart in 30s"
    # Show why it died
    last_log=$(readlink -f "$LOG_DIR/relay.current.log" 2>/dev/null)
    if [[ -n "$last_log" && -f "$last_log" ]]; then
        tail -5 "$last_log" | while IFS= read -r ln; do
            emit "  exit-context: ${ln:0:200}"
        done
    fi

    sleep 30
done
