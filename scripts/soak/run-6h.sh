#!/usr/bin/env bash
# Master entry: assumes Mithril import is complete; launches pair, waits
# for socket, starts orchestrator in the background.
#
# Idempotent: tears down stale nodes first.
set -euo pipefail
cd "$(dirname "$0")/../.."

mkdir -p ./logs/soak-6h ./logs/bp-pair

if [[ ! -d ./db-preview/immutable ]]; then
    echo "FATAL: ./db-preview/immutable not present — run mithril-import first"
    exit 1
fi
if [[ ! -f ./keys/kes.skey || ! -f ./keys/vrf.skey || ! -f ./keys/opcert.cert ]]; then
    echo "FATAL: BP keys missing under ./keys/"
    exit 1
fi
if [[ ! -x ./target/release/dugite-node || ! -x ./target/release/dugite-cli ]]; then
    echo "FATAL: dugite binaries missing — build first"
    exit 1
fi

echo "== Launching BP+relay pair (caffeinated)"
./scripts/soak/run-bp-pair.sh

echo
echo "== Waiting 20s for nodes to settle"
sleep 20

# Sanity: both processes alive
if ! pgrep -f "dugite-node run" > /dev/null; then
    echo "FATAL: dugite-node not running after launch"
    exit 1
fi
if ! pgrep -f "cardano-node run" > /dev/null; then
    echo "FATAL: cardano-node not running after launch"
    exit 1
fi

echo "== Starting soak orchestrator (background)"
nohup ./scripts/soak/orchestrator-6h.sh > ./logs/soak-6h/orchestrator-stdout.log 2>&1 &
ORCH_PID=$!
echo "$ORCH_PID" > ./logs/soak-6h/orchestrator.pid
echo "Orchestrator PID $ORCH_PID, report at ./logs/soak-6h/orchestrator.current.log"
sleep 3
if ! kill -0 "$ORCH_PID" 2>/dev/null; then
    echo "FATAL: orchestrator died immediately"
    tail -40 ./logs/soak-6h/orchestrator-stdout.log
    exit 1
fi

echo "== All systems go"
echo "BP log:     $(readlink -f ./logs/bp-pair/bp.current.log)"
echo "Relay log:  $(readlink -f ./logs/bp-pair/relay.current.log)"
echo "Soak rpt:   $(readlink -f ./logs/soak-6h/orchestrator.current.log)"
echo "Status:     ./scripts/soak/status-6h.sh"
