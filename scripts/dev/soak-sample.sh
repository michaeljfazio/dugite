#!/usr/bin/env bash
# Mainnet relay soak with periodic sampling for the apply-pipeline fix.
# Runs the node for DURATION seconds and samples block_number + apply-rate +
# RSS + volatile-dir size + utxo_count + anomaly counts every SAMPLE seconds.
# Writes a tidy CSV-ish sample log + the full node log.
set -uo pipefail
cd "$(dirname "$0")/../.."

DURATION="${DURATION:-15000}"
SAMPLE="${SAMPLE:-240}"
LOG="${LOG:-/tmp/dugite-soak.log}"
SAMPLELOG="${SAMPLELOG:-/tmp/dugite-soak-samples.log}"
DB="${DB:-./db-mainnet}"
METRICS_PORT="${METRICS_PORT:-12810}"
BIN=./target/release-prof/dugite-node

rm -f "$LOG" "$SAMPLELOG" ./node-prof.sock

RUST_LOG="${RUST_LOG:-warn}" caffeinate -dimsu "$BIN" run \
  --config config/mainnet/config.json \
  --topology config/mainnet/topology.json \
  --database-path "$DB" \
  --socket-path ./node-prof.sock \
  --host-addr 0.0.0.0 --port 3201 \
  --metrics-port "$METRICS_PORT" > "$LOG" 2>&1 &
NODE_PID=$!
echo "node pid=$NODE_PID, soak ${DURATION}s, sample every ${SAMPLE}s -> $SAMPLELOG"
echo "ts elapsed block rate_blk_s rss_mb volatile_mb utxo errors warns fork_unreach" | tee "$SAMPLELOG"

START=$(date +%s)
PREV_BLOCK=""
PREV_T=""
while :; do
  NOW=$(date +%s)
  EL=$((NOW-START))
  [ "$EL" -ge "$DURATION" ] && break
  if ! kill -0 "$NODE_PID" 2>/dev/null; then echo "NODE EXITED at elapsed=${EL}s" | tee -a "$SAMPLELOG"; break; fi
  M=$(curl -s "http://localhost:${METRICS_PORT}/metrics" 2>/dev/null)
  BLOCK=$(printf '%s' "$M" | awk '/^dugite_block_number /{print $2}')
  UTXO=$(printf '%s' "$M" | awk '/^dugite_utxo_count /{print $2}')
  RSS=$(ps -o rss= -p "$NODE_PID" 2>/dev/null | awk '{printf "%.0f",$1/1024}')
  VOL=$(du -sm "$DB/volatile" 2>/dev/null | cut -f1)
  ERRS=$(grep -c "ERROR" "$LOG" 2>/dev/null)
  WARNS=$(grep -c "WARN" "$LOG" 2>/dev/null)
  FU=$(grep -c "fork unreachable" "$LOG" 2>/dev/null)
  RATE=""
  if [ -n "$PREV_BLOCK" ] && [ -n "$BLOCK" ]; then
    DT=$((NOW-PREV_T)); [ "$DT" -gt 0 ] && RATE=$(echo "scale=1; ($BLOCK-$PREV_BLOCK)/$DT" | bc)
  fi
  echo "$(date +%H:%M:%S) ${EL} ${BLOCK:-NA} ${RATE:-NA} ${RSS:-NA} ${VOL:-NA} ${UTXO:-NA} ${ERRS:-0} ${WARNS:-0} ${FU:-0}" | tee -a "$SAMPLELOG"
  PREV_BLOCK="$BLOCK"; PREV_T="$NOW"
  # sleep in the background-run context (allowed)
  S=0; while [ $S -lt "$SAMPLE" ]; do sleep 5; S=$((S+5)); kill -0 "$NODE_PID" 2>/dev/null || break; done
done

echo "stopping node pid=$NODE_PID..."
kill -TERM "$NODE_PID" 2>/dev/null
for i in $(seq 1 20); do kill -0 "$NODE_PID" 2>/dev/null || break; sleep 1; done
pkill -f "dugite-node run .*node-prof.sock" 2>/dev/null
echo "soak done. samples=$SAMPLELOG log=$LOG"
