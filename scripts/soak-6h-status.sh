#!/usr/bin/env bash
# Compact status snapshot for the 6h soak run. Designed to be called from
# Claude's wakeup loop — output is intentionally terse to minimise tokens.
#
# Sections:
#   1. Process state              (dugite pid, cardano-node pid, uptime)
#   2. Health metrics             (slot, block, peers, tip_age, mempool)
#   3. Critical events since boot (KOIOS FAIL, HANG SUSPECTED, BP CRIT, FORGE ADOPT)
#   4. Last 8 lines from orchestrator report

set -uo pipefail
cd "$(dirname "$0")/.."

REPORT_LINK="./logs/soak-6h/orchestrator.current.log"
DUGITE_METRICS=${DUGITE_METRICS:-http://localhost:12798/metrics}
HASKELL_METRICS=${HASKELL_METRICS:-http://127.0.0.1:12799/metrics}

echo "=== SOAK 6H STATUS ($(date '+%Y-%m-%d %H:%M:%S')) ==="

# pgrep can return multiple PIDs when caffeinate wraps dugite-node; take last
# (the actual dugite-node process is the deeper child of caffeinate).
dugite_pid=$(pgrep -f "dugite-node run" 2>/dev/null | tail -1)
relay_pid=$(pgrep -f "cardano-node run" 2>/dev/null | tail -1)

if [[ -n "$dugite_pid" ]]; then
    dugite_age=$(ps -o etime= -p "$dugite_pid" 2>/dev/null | tr -d ' ')
    echo "PROC dugite-node:  pid=$dugite_pid uptime=$dugite_age"
else
    echo "PROC dugite-node:  NOT RUNNING"
fi
if [[ -n "$relay_pid" ]]; then
    relay_age=$(ps -o etime= -p "$relay_pid" 2>/dev/null | tr -d ' ')
    echo "PROC cardano-node: pid=$relay_pid uptime=$relay_age"
else
    echo "PROC cardano-node: NOT RUNNING"
fi

# Dugite metrics
echo "---"
echo "DUGITE METRICS:"
raw=$(curl -s --max-time 5 "$DUGITE_METRICS" 2>/dev/null || true)
if [[ -n "$raw" ]]; then
    for m in \
        dugite_slot_number dugite_block_number dugite_epoch_number \
        dugite_peers_connected dugite_peers_duplex dugite_tip_age_seconds \
        dugite_sync_progress_percent dugite_blocks_forged_total \
        dugite_blocks_applied_total dugite_blocks_announced_total \
        dugite_leader_checks_total dugite_forge_failures_total \
        dugite_rollback_count_total dugite_mempool_tx_count; do
        val=$(echo "$raw" | awk -v n="$m" '$1==n {print $2; exit}')
        echo "  $m=${val:-?}"
    done
else
    echo "  (metrics unreachable)"
fi

echo "---"
echo "RELAY METRICS:"
raw=$(curl -s --max-time 5 "$HASKELL_METRICS" 2>/dev/null || true)
if [[ -n "$raw" ]]; then
    for m in \
        cardano_node_metrics_slotNum_int cardano_node_metrics_blockNum_int \
        cardano_node_metrics_epoch_int  cardano_node_metrics_connectedPeers_int \
        cardano_node_metrics_density_real cardano_node_metrics_RTS_gcLiveBytes_int; do
        val=$(echo "$raw" | awk -v n="$m" '$1==n {print $2; exit}')
        echo "  $m=${val:-?}"
    done
else
    echo "  (metrics unreachable)"
fi

if [[ -e "$REPORT_LINK" ]]; then
    rpt=$(readlink -f "$REPORT_LINK" 2>/dev/null || echo "$REPORT_LINK")
    echo "---"
    echo "EVENT COUNTERS (since soak start):"
    for ev in "FORGE ADOPT" "FORGE ERROR" "FORK SWITCH" "KOIOS OK" "KOIOS FAIL" \
              "RELAY-SAW WARN" "HANG SUSPECTED" "ROLLBACK STORM" "BP CRIT" \
              "RELAY ERR" "FATAL" "RESTART"; do
        n=$(grep -c "$ev" "$rpt" 2>/dev/null | head -1)
        echo "  $ev: ${n:-0}"
    done
    echo "---"
    echo "LAST 12 LINES of orchestrator report:"
    tail -12 "$rpt" 2>/dev/null
else
    echo "(orchestrator not running yet — no report file)"
fi

echo "==="
