#!/usr/bin/env bash
# Single iteration of the autonomous /loop monitoring task.
#
# Tracks state in logs/loop-state/. On each invocation:
#   - Reports 2-min health: tip slot/hash, peers, pipeline depth, forge metrics.
#   - If >= 10 min since last tx batch: submits 5 valid txs.
#   - If >= 30 min since last restart: restarts dugite BP cleanly.
#   - Detects newly forged blocks; queues them for Koios cross-validation.
#
# Stdout is the iteration's report — read by the agent to make decisions.
set -uo pipefail
cd "$(dirname "$0")/.."

STATE_DIR="logs/loop-state"
mkdir -p "$STATE_DIR"

now=$(date +%s)
loop_start=$(cat "$STATE_DIR/loop-start.epoch" 2>/dev/null || echo "$now")
last_tx=$(cat "$STATE_DIR/last-tx-batch.epoch" 2>/dev/null || echo "0")
last_restart=$(cat "$STATE_DIR/last-restart.epoch" 2>/dev/null || echo "0")
elapsed=$(( now - loop_start ))
elapsed_h=$(( elapsed / 3600 ))
elapsed_m=$(( (elapsed % 3600) / 60 ))

echo "=== iteration $(date '+%Y-%m-%dT%H:%M:%SZ') | elapsed ${elapsed_h}h${elapsed_m}m ==="

# ---- 1. Process health ----
bp_pid_file="logs/bp-pair/bp.pid"
relay_pid_file="logs/bp-pair/relay.pid"
bp_pid=$(cat "$bp_pid_file" 2>/dev/null || echo "")
relay_pid=$(cat "$relay_pid_file" 2>/dev/null || echo "")
bp_alive=0
relay_alive=0
[ -n "$bp_pid" ] && kill -0 "$bp_pid" 2>/dev/null && bp_alive=1
[ -n "$relay_pid" ] && kill -0 "$relay_pid" 2>/dev/null && relay_alive=1

echo "bp_pid=$bp_pid bp_alive=$bp_alive | relay_pid=$relay_pid relay_alive=$relay_alive"

if [ "$bp_alive" = "0" ] || [ "$relay_alive" = "0" ]; then
    echo "ACTION_REQUIRED: bp_or_relay_down"
fi

# ---- 2. Metrics health (only if BP alive) ----
if [ "$bp_alive" = "1" ]; then
    metrics=$(curl -s --max-time 5 http://localhost:12798/metrics 2>/dev/null || echo "")
    if [ -z "$metrics" ]; then
        echo "ACTION_REQUIRED: metrics_endpoint_unreachable"
    else
        slot=$(echo "$metrics" | awk '/^cardano_node_metrics_slotNum_int / {print $2}')
        block=$(echo "$metrics" | awk '/^cardano_node_metrics_blockNum_int / {print $2}')
        density=$(echo "$metrics" | awk '/^cardano_node_metrics_density_real / {print $2}')
        peers_conn=$(echo "$metrics" | awk '/^dugite_peers_connected / {print $2}')
        peers_hot=$(echo "$metrics" | awk '/^dugite_peers_hot / {print $2}')
        peers_duplex=$(echo "$metrics" | awk '/^dugite_peers_duplex / {print $2}')
        forged=$(echo "$metrics" | awk '/^dugite_blocks_forged_total / {print $2}')
        announced=$(echo "$metrics" | awk '/^dugite_blocks_announced_total / {print $2}')
        leadership=$(echo "$metrics" | awk '/^dugite_leader_checks_total / {print $2}')
        not_elected=$(echo "$metrics" | awk '/^dugite_leader_checks_not_elected_total / {print $2}')
        elected=$(( ${leadership:-0} - ${not_elected:-0} ))
        echo "metrics: slot=${slot:-?} block=${block:-?} density=${density:-?} peers_conn=${peers_conn:-?} peers_hot=${peers_hot:-?} peers_duplex=${peers_duplex:-?} forged=${forged:-?} announced=${announced:-?} leader_checks=${leadership:-?} elected=${elected:-?}"

        # Estimate tip staleness: real time vs slot time. Preview Shelley
        # systemStart = 2022-10-25T00:00:00Z = 1666656000; 1 slot = 1s.
        if [ -n "${slot:-}" ] && [ "${slot:-0}" -gt 0 ]; then
            preview_epoch_start=1666656000
            slot_real=$(( preview_epoch_start + slot ))
            stale=$(( now - slot_real ))
            stale_min=$(( stale / 60 ))
            echo "tip_staleness_seconds=$stale (=${stale_min}m)"
            if [ "$stale" -gt 180 ]; then
                echo "ACTION_REQUIRED: tip_stale_over_3min"
            fi
        fi
    fi

    # Check log for recently forged blocks across ALL bp-*.log files (not
    # just current — past restart cycles may have produced blocks too).
    forge_events_file="$STATE_DIR/forge-events.tsv"
    : > "$forge_events_file"
    grep -hE "Forged block|forged block hash=|forge_block_at success" logs/bp-pair/bp-*.log 2>/dev/null \
        | grep -oE "[0-9a-f]{64}" | sort -u > "$STATE_DIR/forged-hashes.tmp"
    forged_hash_count=$(wc -l < "$STATE_DIR/forged-hashes.tmp" | tr -d ' ')

    if [ "${forged_hash_count:-0}" -gt 0 ]; then
        echo "DUGITE_FORGED_HASHES: $forged_hash_count unique hash(es) detected across all BP runs"
        cat "$STATE_DIR/forged-hashes.tmp" | head -5

        # Cross-validate each forged hash against the cardano-node relay log.
        # Possible relay verdicts (in order of strength):
        #   1. AddedBlockToVolatileDB / AddedBlockToQueue (hash match) → ACCEPTED
        #   2. AddBlockValidation.ValidCandidate              → VALIDATED
        #   3. ChainSyncClientEvent.TraceDownloadedHeader     → SEEN
        #   4. ChainDB.AddBlockValidation.InvalidBlock        → INVALID (with reason)
        #   5. (no mention)                                    → DROPPED before relay saw it
        relay_log_link="logs/bp-pair/relay.current.log"
        relay_log_path=""
        if [ -L "$relay_log_link" ]; then
            relay_log_path="logs/bp-pair/$(readlink "$relay_log_link")"
        fi

        echo "=== relay verdicts on dugite-forged blocks ==="
        while read -r forged_hash; do
            [ -z "$forged_hash" ] && continue
            short="${forged_hash:0:12}"
            verdict="DROPPED_BEFORE_RELAY"
            relay_excerpt=""

            if [ -n "$relay_log_path" ] && [ -f "$relay_log_path" ]; then
                # Search by full hash first (relay logs typically print full).
                relay_excerpt=$(grep -F "$forged_hash" "$relay_log_path" 2>/dev/null | head -3)
                if [ -n "$relay_excerpt" ]; then
                    if echo "$relay_excerpt" | grep -qiE "InvalidBlock|InvalidCandidate|ValidationError|RejectedHeader"; then
                        verdict="INVALID"
                    elif echo "$relay_excerpt" | grep -qiE "AddedBlockToVolatileDB|AddBlockValidation.*Valid|TryAddToCurrentChain|SwitchedToAFork.*$short"; then
                        verdict="ACCEPTED"
                    elif echo "$relay_excerpt" | grep -qiE "TraceDownloadedHeader|TraceFoundIntersection|ChainSync.*$short"; then
                        verdict="SEEN_BUT_NOT_ADOPTED"
                    else
                        verdict="MENTIONED"
                    fi
                fi
            fi

            echo "  hash=${short}... verdict=$verdict"
            if [ -n "$relay_excerpt" ]; then
                echo "$relay_excerpt" | head -2 | sed 's/^/    relay: /'
            fi

            # Track newly-confirmed accepted blocks for the success criterion.
            if [ "$verdict" = "ACCEPTED" ]; then
                if ! grep -qF "$forged_hash" "$STATE_DIR/forged-accepted.list" 2>/dev/null; then
                    echo "$forged_hash" >> "$STATE_DIR/forged-accepted.list"
                    echo "  *** NEW ACCEPTED FORGED BLOCK: $forged_hash ***"
                fi
            fi
        done < "$STATE_DIR/forged-hashes.tmp"

        accepted_count=$(wc -l < "$STATE_DIR/forged-accepted.list" 2>/dev/null | tr -d ' ' || echo 0)
        echo "forged_accepted_total=${accepted_count:-0}"
        if [ "${accepted_count:-0}" -gt 0 ]; then
            echo "SUCCESS_CRITERION_MET: at least 1 forged block accepted by relay"
        fi
        echo "$forged_hash_count" > "$forged_count_file"
    fi
fi

# ---- 3. Tx submission cadence ----
ten_min_ago=$(( now - 600 ))
if [ "$bp_alive" = "1" ] && [ "$last_tx" -lt "$ten_min_ago" ]; then
    echo "ACTION_DUE: submit_5_txs (last_batch=$(date -r $last_tx '+%H:%M:%S' 2>/dev/null || echo never))"
fi

# ---- 4. Restart cadence ----
thirty_min_ago=$(( now - 1800 ))
if [ "$bp_alive" = "1" ] && [ "$last_restart" -lt "$thirty_min_ago" ]; then
    if [ "$last_restart" = "0" ]; then
        # First iteration after launch — count launch as the restart.
        echo "$now" > "$STATE_DIR/last-restart.epoch"
    else
        echo "ACTION_DUE: restart_bp (last_restart=$(date -r $last_restart '+%H:%M:%S' 2>/dev/null || echo never))"
    fi
fi

# ---- 5. Loop horizon ----
# The loop now runs until the success criterion is met (1 forged block
# accepted by the relay). Time is informational only.

# ---- 6. Success criterion summary ----
accepted_count=$(wc -l < "$STATE_DIR/forged-accepted.list" 2>/dev/null | tr -d ' ' || echo 0)
echo "success_criterion: forged_accepted=${accepted_count:-0} (need >=1)"

echo "=== end iteration ==="
