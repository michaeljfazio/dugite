#!/usr/bin/env bash
# Launch cardano-node Haskell relay + dugite-node BP on PREPROD.
#
# Topology (hidden block producer behind a Haskell relay):
#   dugite-BP (:3001) <--localRoot--> cardano-node relay (:3002) <---> public preprod
#
# The dugite BP forges for the Sandstone [SAND] preprod pool and its only peer
# is the local Haskell relay. The relay diffuses forged blocks to the public
# preprod network, so acceptance can be observed in the relay logs
# (ChainDB.AddedToCurrentChain) and via Koios.
#
# The BP is wrapped in `caffeinate -dimsu` to prevent macOS App Nap from
# freezing the Tokio runtime during long soaks.
set -euo pipefail
cd "$(dirname "$0")/../.."

DB_HASKELL="${DB_HASKELL:-./db-haskell-dl/db}"
DB_DUGITE="${DB_DUGITE:-./db-preprod}"

LOG_DIR="./logs/bp-pair-preprod"
mkdir -p "$LOG_DIR"
TS=$(date +%Y%m%d-%H%M%S)
RELAY_LOG="$LOG_DIR/relay-$TS.log"
BP_LOG="$LOG_DIR/bp-$TS.log"
RELAY_PID_FILE="$LOG_DIR/relay.pid"
BP_PID_FILE="$LOG_DIR/bp.pid"

# Clean up stale processes (SIGTERM only — SIGKILL corrupts the append-only DBs)
for pidf in "$RELAY_PID_FILE" "$BP_PID_FILE"; do
    if [[ -f "$pidf" ]]; then
        oldpid=$(cat "$pidf" 2>/dev/null || true)
        if [[ -n "$oldpid" ]] && kill -0 "$oldpid" 2>/dev/null; then
            echo "Killing stale process $oldpid"
            kill -TERM "$oldpid" 2>/dev/null || true
            sleep 3
        fi
        rm -f "$pidf"
    fi
done
pkill -TERM -f "dugite-node run" 2>/dev/null || true
pkill -TERM -f "cardano-node run" 2>/dev/null || true
sleep 3
rm -f ./node.sock ./haskell-node.sock "$DB_DUGITE/utxo-store/lock" 2>/dev/null || true

echo "===== Starting cardano-node Haskell relay (port 3002) ====="
nohup cardano-node run \
    --config           config/bp-pair/preprod/haskell-relay.config.json \
    --topology         config/bp-pair/preprod/haskell-relay.topology.json \
    --database-path    "$DB_HASKELL" \
    --socket-path      ./haskell-node.sock \
    --host-addr        0.0.0.0 \
    --port             3002 \
    > "$RELAY_LOG" 2>&1 &
RELAY_PID=$!
echo $RELAY_PID > "$RELAY_PID_FILE"
echo "cardano-node relay started: PID $RELAY_PID, log $RELAY_LOG"

echo "Waiting for cardano-node socket..."
for i in $(seq 1 300); do
    if [[ -S ./haskell-node.sock ]]; then
        echo "cardano-node socket ready after ${i}s"
        break
    fi
    sleep 1
    if ! kill -0 "$RELAY_PID" 2>/dev/null; then
        echo "ERROR: cardano-node crashed during startup"
        tail -60 "$RELAY_LOG"
        exit 1
    fi
done

echo "===== Starting dugite-node BP (port 3001, caffeinated) ====="
nohup caffeinate -dimsu \
    ./target/release/dugite-node run \
    --config config/preprod/config.json \
    --topology config/bp-pair/preprod/dugite-bp.topology.json \
    --database-path "$DB_DUGITE" \
    --socket-path ./node.sock \
    --host-addr 0.0.0.0 \
    --port 3001 \
    --shelley-kes-key ./keys/preprod/pool/kes.skey \
    --shelley-vrf-key ./keys/preprod/pool/vrf.skey \
    --shelley-operational-certificate ./keys/preprod/pool/opcert.cert \
    --storage-profile ultra-memory \
    --utxo-backend in-memory \
    --compat-metrics \
    > "$BP_LOG" 2>&1 &
BP_WRAPPER_PID=$!
echo $BP_WRAPPER_PID > "$BP_PID_FILE"
echo "dugite-node BP started under caffeinate: wrapper PID $BP_WRAPPER_PID, log $BP_LOG"

ln -sf "$(basename "$RELAY_LOG")" "$LOG_DIR/relay.current.log"
ln -sf "$(basename "$BP_LOG")" "$LOG_DIR/bp.current.log"

echo "Waiting for dugite socket..."
for i in $(seq 1 300); do
    if [[ -S ./node.sock ]]; then
        echo "dugite socket ready after ${i}s"
        break
    fi
    sleep 1
    if ! kill -0 "$BP_WRAPPER_PID" 2>/dev/null; then
        echo "ERROR: dugite-node crashed during startup"
        tail -80 "$BP_LOG"
        exit 1
    fi
done

echo
echo "===== Both nodes launched (PREPROD) ====="
echo "Relay log: $RELAY_LOG (PID $RELAY_PID)"
echo "BP log:    $BP_LOG (wrapper PID $BP_WRAPPER_PID)"
echo "Dugite metrics:  http://localhost:12799/metrics"
echo "Cardano metrics: http://127.0.0.1:12798/metrics"
