#!/usr/bin/env bash
# Master entry for bare-BP soak: assumes ./db-preview exists; launches a
# caffeinated dugite-node alone (no local Haskell relay) and starts the
# orchestrator in BARE_BP=1 mode.
set -euo pipefail
cd "$(dirname "$0")/../.."

mkdir -p ./logs/soak-6h ./logs/bp-pair

if [[ ! -d ./db-preview/immutable ]]; then
    echo "FATAL: ./db-preview/immutable not present — run mithril-import or restore db first"
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

echo "== Launching bare BP (caffeinated)"
./scripts/soak/launch-bare-bp.sh

echo
echo "== Waiting 20s for node to settle"
sleep 20

if ! pgrep -f "dugite-node run" > /dev/null; then
    echo "FATAL: dugite-node not running after launch"
    exit 1
fi

echo "== Starting soak orchestrator (BARE_BP=1, background)"
BARE_BP=1 nohup ./scripts/soak/orchestrator-6h.sh > ./logs/soak-6h/orchestrator-stdout.log 2>&1 &
ORCH_PID=$!
echo "$ORCH_PID" > ./logs/soak-6h/orchestrator.pid
echo "Orchestrator PID $ORCH_PID, report at ./logs/soak-6h/orchestrator.current.log"
sleep 3
if ! kill -0 "$ORCH_PID" 2>/dev/null; then
    echo "FATAL: orchestrator died immediately"
    tail -40 ./logs/soak-6h/orchestrator-stdout.log
    exit 1
fi

echo "== All systems go (bare BP mode)"
echo "BP log:     $(readlink -f ./logs/bp-pair/bp.current.log)"
echo "Soak rpt:   $(readlink -f ./logs/soak-6h/orchestrator.current.log)"
echo "Status:     ./scripts/soak/status-6h.sh"
