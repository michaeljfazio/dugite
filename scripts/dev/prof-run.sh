#!/usr/bin/env bash
# One-shot profiling run: resume mainnet sync from db-mainnet (slow regime) under
# samply for $DURATION seconds, then SIGINT samply so it finalizes the profile.
set -uo pipefail
cd "$(dirname "$0")/../.."

DURATION="${DURATION:-150}"
OUT="${OUT:-/tmp/dugite-prof.json}"
LOG="${LOG:-/tmp/dugite-prof.log}"
DB="${DB:-./db-mainnet}"
BIN=./target/release-prof/dugite-node

rm -f "$OUT" "$LOG" ./node-prof.sock

caffeinate -dimsu samply record --save-only -o "$OUT" -- \
  "$BIN" run \
  --config config/mainnet/config.json \
  --topology config/mainnet/topology.json \
  --database-path "$DB" \
  --socket-path ./node-prof.sock \
  --host-addr 0.0.0.0 --port 3201 \
  --metrics-port 12810 > "$LOG" 2>&1 &
SAMPLY_PID=$!
echo "samply pid=$SAMPLY_PID, profiling for ${DURATION}s -> $OUT"

sleep "$DURATION"

echo "sending SIGINT to samply to finalize profile..."
kill -INT "$SAMPLY_PID" 2>/dev/null
# Give samply time to write the profile and reap the child.
for i in $(seq 1 30); do
  if ! kill -0 "$SAMPLY_PID" 2>/dev/null; then break; fi
  sleep 1
done
# Belt-and-suspenders: ensure node is gone.
pkill -f "dugite-node run .*node-prof.sock" 2>/dev/null
echo "done. profile=$OUT log=$LOG"
ls -la "$OUT" 2>/dev/null
