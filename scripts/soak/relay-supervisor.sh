#!/usr/bin/env bash
# Keep the cardano-node preprod relay running (auto-restart on crash), so it
# stays available to independently validate dugite's forged block. cardano-node
# treats a transient host-network outage as a fatal `DiffusionError
# BadConfiguration` and exits; this supervisor brings it straight back.
cd "$(dirname "$0")/../.."
LOG_DIR=./logs/bp-pair-preprod
mkdir -p "$LOG_DIR"
echo $$ > "$LOG_DIR/relay-supervisor.pid"
while true; do
  # Reap any stale relay before (re)starting.
  pkill -TERM -f "cardano-node run" 2>/dev/null || true
  sleep 2
  rm -f ./haskell-node.sock 2>/dev/null || true
  TS=$(date +%Y%m%d-%H%M%S)
  RELAY_LOG="$LOG_DIR/relay-$TS.log"
  ln -sf "$(basename "$RELAY_LOG")" "$LOG_DIR/relay.current.log"
  echo "$(date -u +%FT%TZ) relay-supervisor: starting cardano-node relay -> $RELAY_LOG"
  cardano-node run \
    --config        config/bp-pair/preprod/haskell-relay.config.json \
    --topology      config/bp-pair/preprod/haskell-relay.topology.json \
    --database-path ./db-haskell-dl/db \
    --socket-path   ./haskell-node.sock \
    --host-addr     0.0.0.0 \
    --port          3002 \
    >> "$RELAY_LOG" 2>&1
  code=$?
  echo "$(date -u +%FT%TZ) relay-supervisor: cardano-node exited (code $code) — restarting in 8s" >> "$RELAY_LOG"
  sleep 8
done
