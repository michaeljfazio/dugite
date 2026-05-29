#!/usr/bin/env bash
# Run mainnet sync from db-mainnet with per-batch apply diagnostics for DURATION
# seconds.  Emits APPLY_DIAG warn lines (target apply_diag) we post-process to
# find which structure grows / which phase's time grows as throughput declines.
set -uo pipefail
cd "$(dirname "$0")/../.."

DURATION="${DURATION:-2700}"
LOG="${LOG:-/tmp/dugite-diag.log}"
DB="${DB:-./db-mainnet}"
BIN=./target/release-prof/dugite-node

rm -f "$LOG" ./node-prof.sock

RUST_LOG="warn" DUGITE_APPLY_DIAG=1 caffeinate -dimsu "$BIN" run \
  --config config/mainnet/config.json \
  --topology config/mainnet/topology.json \
  --database-path "$DB" \
  --socket-path ./node-prof.sock \
  --host-addr 0.0.0.0 --port 3201 \
  --metrics-port 12810 > "$LOG" 2>&1 &
NODE_PID=$!
echo "node pid=$NODE_PID, diag run for ${DURATION}s -> $LOG"

sleep "$DURATION"

echo "stopping node..."
kill -TERM "$NODE_PID" 2>/dev/null
for i in $(seq 1 20); do kill -0 "$NODE_PID" 2>/dev/null || break; sleep 1; done
pkill -f "dugite-node run .*node-prof.sock" 2>/dev/null
echo "done. log=$LOG"
