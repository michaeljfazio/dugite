#!/usr/bin/env bash
# One-shot health snapshot of the BP+relay pair.
# Output is single-line key=value pairs for easy parsing.
set -uo pipefail
cd "$(dirname "$0")/../.."

CLI="./target/release/dugite-cli"
DUGITE_METRICS="http://localhost:12798/metrics"
RELAY_METRICS="http://127.0.0.1:12797/metrics"
LOG_DIR="./logs/bp-pair"
BP_PID_FILE="$LOG_DIR/bp.pid"
RELAY_PID_FILE="$LOG_DIR/relay.pid"

ts=$(date '+%Y-%m-%dT%H:%M:%S')

# Process status
bp_pid=$(cat "$BP_PID_FILE" 2>/dev/null || echo "")
relay_pid=$(cat "$RELAY_PID_FILE" 2>/dev/null || echo "")
bp_alive=NO; relay_alive=NO
[[ -n "$bp_pid" ]] && kill -0 "$bp_pid" 2>/dev/null && bp_alive=YES
[[ -n "$relay_pid" ]] && kill -0 "$relay_pid" 2>/dev/null && relay_alive=YES

# Dugite metrics
metrics=$(curl -s --max-time 4 "$DUGITE_METRICS" 2>/dev/null || echo "")
_m() { echo "$metrics" | awk -v k="^$1 " '$0 ~ k { print $2; exit }'; }
sync=$(_m dugite_sync_progress_percent)
height=$(_m dugite_block_number)
slot=$(_m dugite_slot_number)
epoch=$(_m dugite_epoch_number)
peers=$(_m dugite_peers_connected)
mempool_n=$(_m dugite_mempool_tx_count)
mempool_b=$(_m dugite_mempool_bytes)
forged=$(_m dugite_blocks_forged_total)
applied=$(_m dugite_blocks_applied_total)
rollbacks=$(_m dugite_chain_rollbacks_total)
density=$(_m dugite_chain_density)

# Relay metrics (cardano-node prometheus)
rmetrics=$(curl -s --max-time 4 "$RELAY_METRICS" 2>/dev/null || echo "")
relay_slot=$(echo "$rmetrics" | awk '/^cardano_node_metrics_slotNum_int / {print $2; exit}')
relay_height=$(echo "$rmetrics" | awk '/^cardano_node_metrics_blockNum_int / {print $2; exit}')
relay_density=$(echo "$rmetrics" | awk '/^cardano_node_metrics_density_real / {print $2; exit}')
relay_peers=$(echo "$rmetrics" | awk '/^cardano_node_metrics_connectedPeers_int / {print $2; exit}')

# Compute slot lag
[[ -n "$slot" && -n "$relay_slot" ]] && lag=$((relay_slot - slot)) || lag=""

# Wallclock vs slot freshness (preview shelley start = 2022-10-25T00:00:00 UTC = 1666656000, slot length 1s)
now_epoch=$(date +%s)
shelley_start=1666656000
expected_slot=$((now_epoch - shelley_start))
[[ -n "$slot" ]] && wall_lag=$((expected_slot - slot)) || wall_lag=""

echo "ts=$ts bp=$bp_alive[$bp_pid] relay=$relay_alive[$relay_pid] sync=$sync height=$height slot=$slot epoch=$epoch peers=$peers mempool=$mempool_n forged=$forged applied=$applied rollbacks=$rollbacks density=$density relay_slot=$relay_slot relay_height=$relay_height relay_density=$relay_density relay_peers=$relay_peers lag_vs_relay=$lag wall_lag=$wall_lag"
