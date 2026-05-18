#!/usr/bin/env bash
# Launch a bare dugite-node block producer (no local Haskell relay) on
# preview testnet. Peers come from the bp-bare-topology.json bootstrap +
# peer-snapshot cache + on-chain SPO relays after the ledger crosses
# useLedgerAfterSlot.
#
# dugite-node is wrapped in caffeinate -dimsu on macOS to prevent App Nap
# from freezing the process during long soaks (see project memory note
# project_macos_appnap_freeze_2026_05_08).
set -euo pipefail
cd "$(dirname "$0")/../.."

LOG_DIR="./logs/bp-pair"
mkdir -p "$LOG_DIR"
TS=$(date +%Y%m%d-%H%M%S)
BP_LOG="$LOG_DIR/bp-$TS.log"
BP_PID_FILE="$LOG_DIR/bp.pid"

# Stop any prior runs (relay too, in case prior soak left it around).
if [[ -f "$BP_PID_FILE" ]]; then
    oldpid=$(cat "$BP_PID_FILE" 2>/dev/null || true)
    if [[ -n "$oldpid" ]] && kill -0 "$oldpid" 2>/dev/null; then
        echo "Killing stale BP wrapper PID $oldpid"
        kill -TERM "$oldpid" 2>/dev/null || true
        sleep 2
        kill -KILL "$oldpid" 2>/dev/null || true
    fi
    rm -f "$BP_PID_FILE"
fi
pkill -f "dugite-node run" 2>/dev/null || true
pkill -f "caffeinate -dimsu" 2>/dev/null || true
pkill -f "cardano-node run" 2>/dev/null || true
sleep 2
rm -f ./node.sock ./haskell-node.sock ./db-preview/utxo-store/lock 2>/dev/null || true
# Wipe stale relay log symlinks so the orchestrator's BARE_BP=1 mode picks up
# a clean state.
rm -f "$LOG_DIR/relay.current.log" 2>/dev/null || true

echo "===== Starting dugite-node BARE BP (port 3001, caffeinated) ====="
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
ln -sf "$(basename "$BP_LOG")" "$LOG_DIR/bp.current.log"
echo "dugite-node BARE BP started under caffeinate: wrapper PID $BP_WRAPPER_PID"
echo "log: $BP_LOG"

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
echo "===== Bare BP launched ====="
echo "BP log:      $BP_LOG (wrapper PID $BP_WRAPPER_PID)"
echo "Dugite metrics: http://localhost:12798/metrics"
echo "Topology:    bp-bare-topology.json"
echo "Peers:       public preview bootstrap + on-chain ledger peers"
