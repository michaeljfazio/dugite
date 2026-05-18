#!/usr/bin/env bash
# Launch cardano-node Haskell relay + dugite-node BP, but wrap the BP in
# `caffeinate -dimsu` to prevent macOS App Nap from freezing the process
# during long soaks (see memory: macOS App Nap froze BP 2026-05-08).
set -euo pipefail
cd "$(dirname "$0")/../.."

LOG_DIR="./logs/bp-pair"
mkdir -p "$LOG_DIR"
TS=$(date +%Y%m%d-%H%M%S)
RELAY_LOG="$LOG_DIR/relay-$TS.log"
BP_LOG="$LOG_DIR/bp-$TS.log"
RELAY_PID_FILE="$LOG_DIR/relay.pid"
BP_PID_FILE="$LOG_DIR/bp.pid"

# Clean up stale processes
for pidf in "$RELAY_PID_FILE" "$BP_PID_FILE"; do
    if [[ -f "$pidf" ]]; then
        oldpid=$(cat "$pidf" 2>/dev/null || true)
        if [[ -n "$oldpid" ]] && kill -0 "$oldpid" 2>/dev/null; then
            echo "Killing stale process $oldpid"
            kill -TERM "$oldpid" 2>/dev/null || true
            sleep 2
            kill -KILL "$oldpid" 2>/dev/null || true
        fi
        rm -f "$pidf"
    fi
done
pkill -f "dugite-node run" 2>/dev/null || true
pkill -f "cardano-node run" 2>/dev/null || true
sleep 2
rm -f ./node.sock ./haskell-node.sock ./db-preview/utxo-store/lock 2>/dev/null || true

echo "===== Starting cardano-node Haskell relay (port 3002) ====="
# Relay does NOT need caffeinate (Haskell node has its own scheduling that
# doesn't suffer from App Nap), but BP does because dugite is a thin Tokio
# runtime that macOS classifies as napable.
nohup cardano-node run \
    --config           config/bp-pair/haskell-relay.config.json \
    --topology         config/bp-pair/haskell-relay.topology.json \
    --database-path    ./db-haskell \
    --socket-path      ./haskell-node.sock \
    --host-addr        0.0.0.0 \
    --port             3002 \
    > "$RELAY_LOG" 2>&1 &
RELAY_PID=$!
echo $RELAY_PID > "$RELAY_PID_FILE"
echo "cardano-node relay started: PID $RELAY_PID, log $RELAY_LOG"

# Wait for cardano-node socket
echo "Waiting for cardano-node socket..."
for i in $(seq 1 90); do
    if [[ -S ./haskell-node.sock ]]; then
        echo "cardano-node socket ready after ${i}s"
        break
    fi
    sleep 1
    if ! kill -0 "$RELAY_PID" 2>/dev/null; then
        echo "ERROR: cardano-node crashed during startup"
        tail -40 "$RELAY_LOG"
        exit 1
    fi
done

echo "===== Starting dugite-node BP (port 3001, caffeinated) ====="
# caffeinate -dimsu:
#   -d  prevent display sleep
#   -i  prevent idle sleep
#   -m  prevent disk sleep
#   -s  prevent system sleep (AC power only)
#   -u  declare user is active (extra App Nap suppression for child PID)
nohup caffeinate -dimsu \
    ./target/release/dugite-node run \
    --config config/preview/config.json \
    --topology config/bp-pair/dugite-bp.topology.json \
    --database-path ./db-preview \
    --socket-path ./node.sock \
    --host-addr 0.0.0.0 \
    --port 3001 \
    --shelley-kes-key ./keys/kes.skey \
    --shelley-vrf-key ./keys/vrf.skey \
    --shelley-operational-certificate ./keys/opcert.cert \
    --compat-metrics \
    > "$BP_LOG" 2>&1 &
BP_WRAPPER_PID=$!
echo $BP_WRAPPER_PID > "$BP_PID_FILE"
echo "dugite-node BP started under caffeinate: wrapper PID $BP_WRAPPER_PID, log $BP_LOG"

echo "$RELAY_LOG" > "$LOG_DIR/relay.current.log.path"
echo "$BP_LOG" > "$LOG_DIR/bp.current.log.path"
ln -sf "$(basename "$RELAY_LOG")" "$LOG_DIR/relay.current.log"
ln -sf "$(basename "$BP_LOG")" "$LOG_DIR/bp.current.log"

# Wait for dugite socket
echo "Waiting for dugite socket..."
for i in $(seq 1 180); do
    if [[ -S ./node.sock ]]; then
        echo "dugite socket ready after ${i}s"
        break
    fi
    sleep 1
    if ! kill -0 "$BP_WRAPPER_PID" 2>/dev/null; then
        echo "ERROR: dugite-node crashed during startup"
        tail -60 "$BP_LOG"
        exit 1
    fi
done

echo
echo "===== Both nodes launched ====="
echo "Relay log: $RELAY_LOG (PID $RELAY_PID)"
echo "BP log:    $BP_LOG (wrapper PID $BP_WRAPPER_PID)"
echo "Dugite metrics: http://localhost:12798/metrics"
echo "Cardano metrics: http://127.0.0.1:12799/metrics"
