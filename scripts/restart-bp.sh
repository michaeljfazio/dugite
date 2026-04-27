#!/usr/bin/env bash
# Restart only the dugite-node block producer (NOT the cardano-node relay).
# Used by the soak loop to verify recovery-after-restart on a regular cadence.
set -uo pipefail
cd "$(dirname "$0")/.."

LOG_DIR="./logs/bp-pair"
BP_PID_FILE="$LOG_DIR/bp.pid"
TS=$(date +%Y%m%d-%H%M%S)
BP_LOG="$LOG_DIR/bp-$TS.log"

# Stop existing dugite-node BP
if [[ -f "$BP_PID_FILE" ]]; then
    oldpid=$(cat "$BP_PID_FILE" 2>/dev/null || true)
    if [[ -n "$oldpid" ]] && kill -0 "$oldpid" 2>/dev/null; then
        echo "[restart-bp] stopping PID $oldpid"
        kill -TERM "$oldpid" 2>/dev/null || true
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            if ! kill -0 "$oldpid" 2>/dev/null; then break; fi
            sleep 1
        done
        kill -KILL "$oldpid" 2>/dev/null || true
    fi
    rm -f "$BP_PID_FILE"
fi
pkill -f "dugite-node run" 2>/dev/null || true
sleep 1
rm -f ./node.sock ./db-preview/utxo-store/lock 2>/dev/null || true

# Restart dugite-node with the same args as launch-bp-pair.sh
nohup ./target/release/dugite-node run \
    --config config/preview-config.json \
    --topology config/bp-single-relay-topology.json \
    --database-path ./db-preview \
    --socket-path ./node.sock \
    --host-addr 0.0.0.0 \
    --port 3001 \
    --shelley-kes-key ./keys/kes.skey \
    --shelley-vrf-key ./keys/vrf.skey \
    --shelley-operational-certificate ./keys/opcert.cert \
    --compat-metrics \
    > "$BP_LOG" 2>&1 &
BP_PID=$!
echo $BP_PID > "$BP_PID_FILE"
ln -sf "$(basename "$BP_LOG")" "$LOG_DIR/bp.current.log"
echo "[restart-bp] started PID $BP_PID, log $BP_LOG"

# Wait for socket so the next soak-tick has something to query
for i in $(seq 1 90); do
    if [[ -S ./node.sock ]]; then
        echo "[restart-bp] socket ready after ${i}s"
        exit 0
    fi
    sleep 1
    if ! kill -0 "$BP_PID" 2>/dev/null; then
        echo "[restart-bp] ERROR: dugite-node crashed during startup"
        tail -60 "$BP_LOG"
        exit 1
    fi
done

echo "[restart-bp] WARN: socket not ready after 90s (continuing — next tick will check)"
exit 0
