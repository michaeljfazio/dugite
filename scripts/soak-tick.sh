#!/usr/bin/env bash
# Soak-cycle tick: always run health check; submit 5 txs if ≥10 min since last
# batch; restart dugite-node BP every ≥30 minutes to verify recovery.
set -uo pipefail
cd "$(dirname "$0")/.."

JOURNAL="logs/bp-pair/journal.log"
LAST_TX_FILE="logs/bp-pair/last-tx-batch.epoch"
LAST_RESTART_FILE="logs/bp-pair/last-bp-restart.epoch"

now_epoch=$(date +%s)
last_tx=$(cat "$LAST_TX_FILE" 2>/dev/null || echo 0)
elapsed=$((now_epoch - last_tx))
last_restart=$(cat "$LAST_RESTART_FILE" 2>/dev/null || echo 0)
restart_elapsed=$((now_epoch - last_restart))

# Always health check
./scripts/health-check.sh | tee -a "$JOURNAL"

# Mempool snapshot
dugite_mp=$(curl -sf --max-time 4 http://localhost:12798/metrics 2>/dev/null | awk '/^dugite_mempool_tx_count/ {print $2; exit}')
relay_mp=$(cardano-cli query tx-mempool info --testnet-magic 2 --socket-path ./haskell-node.sock 2>/dev/null | grep numberOfTxs | grep -oE '[0-9]+' | head -1)
echo "[$(date '+%Y-%m-%dT%H:%M:%SZ')] mempool dugite=$dugite_mp relay=$relay_mp elapsed_since_tx=${elapsed}s" | tee -a "$JOURNAL"

if [ "$elapsed" -ge 600 ]; then
    echo "[$(date '+%Y-%m-%dT%H:%M:%SZ')] === Submitting 5 txs (cycle elapsed=${elapsed}s) ===" | tee -a "$JOURNAL"
    if ./scripts/submit-5-txs.sh 2>&1 | tee -a "$JOURNAL"; then
        echo "$now_epoch" > "$LAST_TX_FILE"
    fi
fi

# Restart dugite-node BP every ≥30 minutes (1800s) to exercise the
# recovery-after-restart path. The cardano-node relay is left untouched.
# First-ever tick (last_restart==0) seeds the timer instead of restarting.
if [ "$last_restart" -eq 0 ]; then
    echo "$now_epoch" > "$LAST_RESTART_FILE"
elif [ "$restart_elapsed" -ge 1800 ]; then
    echo "[$(date '+%Y-%m-%dT%H:%M:%SZ')] === Restarting dugite-node BP (cycle elapsed=${restart_elapsed}s) ===" | tee -a "$JOURNAL"
    if ./scripts/restart-bp.sh 2>&1 | tee -a "$JOURNAL"; then
        echo "$now_epoch" > "$LAST_RESTART_FILE"
    fi
fi
