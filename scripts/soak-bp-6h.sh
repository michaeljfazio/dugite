#!/usr/bin/env bash
# 6-hour BP soak monitor: dugite-node BP behind a cardano-node relay.
# - Health snapshots every 2 min from both nodes' Prometheus endpoints.
# - Forge-event detection on the dugite log (TraceAdoptedBlock, etc.).
# - Cross-validates every forged block via Koios block_info.
# - Surfaces ERROR/PANIC/WARN lines from both dugite and cardano-node logs.
# - Restarts dugite-node every 45 min (relay stays up).
# - Submits 5 valid txs every 25 min via scripts/submit-5-txs.sh.
#
# Each significant event is one stdout line, also appended to a flat report.
set -uo pipefail
cd "$(dirname "$0")/.."

DURATION_SEC=${DURATION_SEC:-21600}            # 6 hours
SNAPSHOT_INTERVAL=${SNAPSHOT_INTERVAL:-120}    # health snapshot every 2 min
LOG_TAIL_INTERVAL=${LOG_TAIL_INTERVAL:-15}     # log diff scan every 15s
RESTART_INTERVAL=${RESTART_INTERVAL:-2700}     # restart dugite every 45 min
TX_INTERVAL=${TX_INTERVAL:-1500}               # 5 valid txs every 25 min
DUGITE_METRICS=${DUGITE_METRICS:-http://localhost:12798/metrics}
HASKELL_METRICS=${HASKELL_METRICS:-http://127.0.0.1:12799/metrics}
KOIOS_BASE=${KOIOS_BASE:-https://preview.koios.rest/api/v1}

REPORT_DIR=./logs/soak-bp-6h
mkdir -p "$REPORT_DIR"
TS=$(date +%Y%m%d-%H%M%S)
REPORT="$REPORT_DIR/report-$TS.log"
ln -sf "$(basename "$REPORT")" "$REPORT_DIR/report.current.log"

# Resolve current log files written by launch-bp-pair.sh.
BP_LOG=$(readlink -f "./logs/bp-pair/bp.current.log" 2>/dev/null || true)
RELAY_LOG=$(readlink -f "./logs/bp-pair/relay.current.log" 2>/dev/null || true)
if [[ -z "$BP_LOG" || ! -f "$BP_LOG" ]]; then
    echo "FATAL: BP log not found (looked at ./logs/bp-pair/bp.current.log)"
    exit 1
fi
if [[ -z "$RELAY_LOG" || ! -f "$RELAY_LOG" ]]; then
    echo "FATAL: relay log not found (looked at ./logs/bp-pair/relay.current.log)"
    exit 1
fi

START=$(date +%s)
END=$((START + DURATION_SEC))

emit() {
    local now ts elapsed h m line
    now=$(date +%s)
    ts=$(date '+%Y-%m-%d %H:%M:%S')
    elapsed=$((now - START))
    h=$((elapsed / 3600))
    m=$(((elapsed % 3600) / 60))
    line=$(printf "[%s] [+%02d:%02d] %s" "$ts" "$h" "$m" "$*")
    echo "$line"
    echo "$line" >> "$REPORT"
}

emit "SOAK START — duration=${DURATION_SEC}s snapshot=${SNAPSHOT_INTERVAL}s restart=${RESTART_INTERVAL}s tx=${TX_INTERVAL}s"
emit "SOAK START — bp_log=$BP_LOG relay_log=$RELAY_LOG"
emit "SOAK START — pool=Sandstone[SAND] pool_id=6954ec11cf7097a693721104139b96c54e7f3e2a8f9e7577630f7856 magic=2"
emit "SOAK START — note: σ≈0.0000247 → P(0 forges in 6h) ≳ 97%; absence of forging is expected"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
metric() {
    # metric URL NAME  →  value (or empty)
    local url="$1" name="$2"
    curl -s --max-time 5 "$url" 2>/dev/null | awk -v n="$name" '$1==n {print $2; exit}'
}

snap_dugite() {
    local raw
    raw=$(curl -s --max-time 5 "$DUGITE_METRICS" 2>/dev/null) || { echo "metrics_unreachable"; return; }
    local sync slot block epoch peers out in_p dup tip_age forged forge_fail leaders applied rolls mem_b mempool announced
    sync=$(echo "$raw"      | awk '$1=="dugite_sync_progress_percent" {printf "%.2f", $2/100; exit}')
    slot=$(echo "$raw"      | awk '$1=="dugite_slot_number" {print $2; exit}')
    block=$(echo "$raw"     | awk '$1=="dugite_block_number" {print $2; exit}')
    epoch=$(echo "$raw"     | awk '$1=="dugite_epoch_number" {print $2; exit}')
    peers=$(echo "$raw"     | awk '$1=="dugite_peers_connected" {print $2; exit}')
    out=$(echo "$raw"       | awk '$1=="dugite_peers_outbound" {print $2; exit}')
    in_p=$(echo "$raw"      | awk '$1=="dugite_peers_inbound" {print $2; exit}')
    dup=$(echo "$raw"       | awk '$1=="dugite_peers_duplex" {print $2; exit}')
    tip_age=$(echo "$raw"   | awk '$1=="dugite_tip_age_seconds" {print $2; exit}')
    forged=$(echo "$raw"    | awk '$1=="dugite_blocks_forged_total" {print $2; exit}')
    forge_fail=$(echo "$raw"| awk '$1=="dugite_forge_failures_total" {print $2; exit}')
    leaders=$(echo "$raw"   | awk '$1=="dugite_leader_checks_total" {print $2; exit}')
    applied=$(echo "$raw"   | awk '$1=="dugite_blocks_applied_total" {print $2; exit}')
    announced=$(echo "$raw" | awk '$1=="dugite_blocks_announced_total" {print $2; exit}')
    rolls=$(echo "$raw"     | awk '$1=="dugite_rollback_count_total" {print $2; exit}')
    mempool=$(echo "$raw"   | awk '$1=="dugite_mempool_tx_count" {print $2; exit}')
    mem_b=$(echo "$raw"     | awk '$1=="dugite_mem_resident_bytes" {printf "%.1f", $2/1073741824; exit}')
    printf "sync=%s%% slot=%s blk=%s ep=%s peers=%s/o%s/i%s/d%s tip_age=%ss applied=%s forged=%s leaders=%s announced=%s fails=%s rb=%s mp=%s rss=%sGB" \
        "${sync:-?}" "${slot:-?}" "${block:-?}" "${epoch:-?}" \
        "${peers:-?}" "${out:-?}" "${in_p:-?}" "${dup:-?}" \
        "${tip_age:-?}" "${applied:-?}" "${forged:-?}" "${leaders:-?}" \
        "${announced:-?}" "${forge_fail:-?}" "${rolls:-?}" "${mempool:-?}" "${mem_b:-?}"
}

snap_haskell() {
    local raw
    raw=$(curl -s --max-time 5 "$HASKELL_METRICS" 2>/dev/null) || { echo "metrics_unreachable"; return; }
    local slot block epoch peers
    slot=$(echo "$raw"  | awk '/^cardano_node_metrics_slotNum_int/ {print $2; exit}')
    block=$(echo "$raw" | awk '/^cardano_node_metrics_blockNum_int/ {print $2; exit}')
    epoch=$(echo "$raw" | awk '/^cardano_node_metrics_epoch_int/ {print $2; exit}')
    peers=$(echo "$raw" | awk '/^cardano_node_metrics_connectedPeers_int/ {print $2; exit}')
    printf "slot=%s blk=%s ep=%s peers=%s" "${slot:-?}" "${block:-?}" "${epoch:-?}" "${peers:-?}"
}

# Cross-validate one forged block via Koios.
koios_check_block() {
    local hash="$1" slot="$2" block_no="$3"
    local resp
    for attempt in 1 2 3 4 5; do
        sleep 30
        resp=$(curl -s --max-time 10 -X POST "${KOIOS_BASE}/block_info" \
            -H "Content-Type: application/json" \
            -d "{\"_block_hashes\":[\"${hash}\"]}" 2>/dev/null)
        if echo "$resp" | grep -q "\"hash\".*${hash}"; then
            local epoch_no tx_count
            epoch_no=$(echo "$resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d[0].get('epoch_no','?')) if d else print('?')" 2>/dev/null || echo "?")
            tx_count=$(echo "$resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d[0].get('tx_count','?')) if d else print('?')" 2>/dev/null || echo "?")
            emit "KOIOS OK — forged block ${hash} accepted on canonical chain (epoch=${epoch_no} txs=${tx_count}) after ${attempt}×30s"
            return 0
        fi
    done
    emit "KOIOS FAIL — forged block ${hash} (slot=${slot} block_no=${block_no}) NOT visible on Koios after 5×30s — likely orphaned / not adopted"
    emit "KOIOS FAIL — pulling cardano-node log lines mentioning slot=${slot}:"
    grep -B1 -A1 "${slot}" "$RELAY_LOG" 2>/dev/null | tail -20 | while IFS= read -r ln; do
        emit "  RELAY: ${ln:0:240}"
    done
    return 1
}

# ---------------------------------------------------------------------------
# Restart dugite-node only (leave relay running)
# ---------------------------------------------------------------------------
restart_dugite_node() {
    local kind="${1:-graceful}"
    emit "RESTART — ${kind} restart of dugite-node beginning"

    local pid
    pid=$(pgrep -f "dugite-node run" || true)
    if [[ -z "$pid" ]]; then
        emit "RESTART — no dugite-node process found, starting fresh"
    else
        if [[ "$kind" == "hard" ]]; then
            emit "RESTART — sending SIGKILL to PID $pid"
            kill -KILL "$pid" 2>/dev/null || true
        else
            emit "RESTART — sending SIGTERM to PID $pid"
            kill -TERM "$pid" 2>/dev/null || true
            local waited=0
            while kill -0 "$pid" 2>/dev/null && [[ $waited -lt 45 ]]; do
                sleep 1
                waited=$((waited + 1))
            done
            if kill -0 "$pid" 2>/dev/null; then
                emit "RESTART — graceful shutdown timed out after ${waited}s, escalating to SIGKILL"
                kill -KILL "$pid" 2>/dev/null || true
            else
                emit "RESTART — graceful shutdown completed in ${waited}s"
            fi
        fi
    fi

    # Wait for socket + lock cleanup
    sleep 3
    rm -f ./node.sock ./db-preview/utxo-store/lock 2>/dev/null || true
    # Wait for metrics port to release
    local port_wait=0
    while lsof -i :12798 >/dev/null 2>&1 && [[ $port_wait -lt 15 ]]; do
        sleep 1
        port_wait=$((port_wait + 1))
    done

    # Rotate the BP log via launch-bp-pair.sh-style filename
    local new_bp_log
    new_bp_log="./logs/bp-pair/bp-$(date +%Y%m%d-%H%M%S).log"
    emit "RESTART — starting dugite-node, log → $new_bp_log"
    nohup ./target/release/dugite-node run \
        --config config/preview-config.json \
        --topology config/bp-single-relay-topology.json \
        --database-path ./db-preview \
        --socket-path ./node.sock \
        --host-addr 0.0.0.0 \
        --port 3001 \
        --shelley-kes-key ./keys/kes.skey \
        --shelley-vrf-key ./keys/vrf.skey \
        --shelley-operational-certificate ./keys/opcert.cert \
        --compat-metrics \
        > "$new_bp_log" 2>&1 &
    local new_pid=$!
    echo "$new_pid" > ./logs/bp-pair/bp.pid
    ln -sf "$(basename "$new_bp_log")" ./logs/bp-pair/bp.current.log
    BP_LOG="$new_bp_log"
    BP_OFFSET=0

    # Wait for socket
    local waited=0
    while [[ ! -S ./node.sock ]] && [[ $waited -lt 180 ]]; do
        sleep 1
        waited=$((waited + 1))
        if ! kill -0 "$new_pid" 2>/dev/null; then
            emit "RESTART — dugite-node CRASHED during startup (last 30 lines):"
            tail -30 "$new_bp_log" 2>/dev/null | while IFS= read -r ln; do
                emit "  BP: ${ln:0:240}"
            done
            return 1
        fi
    done
    if [[ -S ./node.sock ]]; then
        emit "RESTART — dugite-node socket ready in ${waited}s (PID $new_pid)"
    else
        emit "RESTART — WARN socket not ready after ${waited}s"
    fi
    return 0
}

# ---------------------------------------------------------------------------
# Submit 5 valid txs
# ---------------------------------------------------------------------------
submit_tx_batch() {
    emit "TX BATCH — submitting 5 valid txs"
    local out
    if out=$(./scripts/submit-5-txs.sh 2>&1); then
        echo "$out" | while IFS= read -r ln; do
            emit "  TX: ${ln:0:240}"
        done
        local accepted rejected
        accepted=$(echo "$out" | grep -oE 'accepted=[0-9]+' | tail -1 | cut -d= -f2)
        rejected=$(echo "$out" | grep -oE 'rejected=[0-9]+' | tail -1 | cut -d= -f2)
        emit "TX BATCH — done accepted=${accepted:-?} rejected=${rejected:-?}"
    else
        emit "TX BATCH — script FAILED (exit=$?):"
        echo "$out" | tail -10 | while IFS= read -r ln; do
            emit "  TX-ERR: ${ln:0:240}"
        done
    fi
}

# ---------------------------------------------------------------------------
# Log tailers (offset-tracked)
# ---------------------------------------------------------------------------
BP_OFFSET=$(wc -c < "$BP_LOG" 2>/dev/null | awk '{print $1}')
RELAY_OFFSET=$(wc -c < "$RELAY_LOG" 2>/dev/null | awk '{print $1}')
: "${BP_OFFSET:=0}"
: "${RELAY_OFFSET:=0}"

scan_bp_log() {
    [[ ! -f "$BP_LOG" ]] && return 0
    local cur
    cur=$(wc -c < "$BP_LOG" 2>/dev/null | awk '{print $1}')
    : "${cur:=0}"
    if (( cur <= BP_OFFSET )); then return 0; fi
    local new
    new=$(tail -c +$((BP_OFFSET + 1)) "$BP_LOG" 2>/dev/null | head -c $((cur - BP_OFFSET)))
    BP_OFFSET=$cur

    # ── Forge-success events ─────────────────────────────────────────────
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        local hash slot blkno
        hash=$(echo "$line"  | grep -oE 'block_hash=[a-f0-9]{64}' | head -1 | cut -d= -f2)
        slot=$(echo "$line"  | grep -oE 'slot=[0-9]+' | head -1 | cut -d= -f2)
        blkno=$(echo "$line" | grep -oE 'block_no=[0-9]+' | head -1 | cut -d= -f2)
        emit "FORGE ADOPT — slot=${slot:-?} block=${blkno:-?} hash=${hash:-?}"
        if [[ -n "$hash" ]]; then
            (koios_check_block "$hash" "${slot:-?}" "${blkno:-?}") &
        fi
    done < <(echo "$new" | grep "TraceAdoptedBlock" 2>/dev/null)

    # ── Forge-side errors ────────────────────────────────────────────────
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        emit "FORGE ERROR — ${line:0:240}"
    done < <(echo "$new" | grep -E "TraceForgedInvalidBlock|TraceForgeStateUpdateError|Forged block announced but NO peers|Forged block has no announcement" 2>/dev/null)

    # ── Critical errors / panics ─────────────────────────────────────────
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        emit "BP CRIT — ${line:0:240}"
    done < <(echo "$new" | grep -iE "panic|fatal|stack overflow|out of memory|segfault|assertion failed" 2>/dev/null)
}

scan_relay_log() {
    [[ ! -f "$RELAY_LOG" ]] && return 0
    local cur
    cur=$(wc -c < "$RELAY_LOG" 2>/dev/null | awk '{print $1}')
    : "${cur:=0}"
    if (( cur <= RELAY_OFFSET )); then return 0; fi
    local new
    new=$(tail -c +$((RELAY_OFFSET + 1)) "$RELAY_LOG" 2>/dev/null | head -c $((cur - RELAY_OFFSET)))
    RELAY_OFFSET=$cur

    # ── RejectedTx is logged repeatedly by TxSubmission2 — dedupe by txid
    # and emit one summary line per scan window.
    local rejected_txids
    rejected_txids=$(echo "$new" | grep -oE 'TraceMempoolRejectedTx[^}]*"txid":"[a-f0-9]+"' \
        | grep -oE '"txid":"[a-f0-9]+"' \
        | sed 's/"txid":"\([a-f0-9]*\)"/\1/' \
        | sort -u | tr '\n' ',' | sed 's/,$//')
    if [[ -n "$rejected_txids" ]]; then
        local unique_count
        unique_count=$(echo "$rejected_txids" | tr ',' '\n' | wc -l | awk '{print $1}')
        emit "RELAY REJECTED-TX — ${unique_count} unique txids: ${rejected_txids:0:240}"
    fi

    # ── Non-mempool relay errors: real ChainDB / consensus / network errors.
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        emit "RELAY ERR — ${line:0:240}"
    done < <(echo "$new" \
        | grep -v "TraceMempoolRejectedTx" \
        | grep -v "TraceMempoolRemoveTxs" \
        | grep -iE "invalidBlock|ChainSyncProtocolError|BlockFetchProtocolError|exception|panic|InvalidHeader|ChainSyncBadTip|HeaderEnvelopeError|ChainDB.*Error|ApplyBlockError|ledger.*[Ee]rror" 2>/dev/null)
}

# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------
LAST_SNAP=0
LAST_RESTART=$START
LAST_TX=$START
PREV_FORGED=0
PREV_LEADERS=0
PREV_APPLIED=0
RESTART_COUNT=0
TX_BATCH_COUNT=0

while [[ $(date +%s) -lt $END ]]; do
    NOW=$(date +%s)

    # Fast-cadence log scanners
    scan_bp_log
    scan_relay_log

    # Health snapshot every SNAPSHOT_INTERVAL
    if (( NOW - LAST_SNAP >= SNAPSHOT_INTERVAL )); then
        LAST_SNAP=$NOW
        bp_snap=$(snap_dugite)
        relay_snap=$(snap_haskell)
        emit "BP    — $bp_snap"
        emit "RELAY — $relay_snap"

        cur_forged=$(metric "$DUGITE_METRICS" dugite_blocks_forged_total)
        cur_leaders=$(metric "$DUGITE_METRICS" dugite_leader_checks_total)
        cur_applied=$(metric "$DUGITE_METRICS" dugite_blocks_applied_total)
        cur_forged=${cur_forged:-0}
        cur_leaders=${cur_leaders:-0}
        cur_applied=${cur_applied:-0}
        d_forged=$((${cur_forged%.*} - PREV_FORGED))
        d_leaders=$((${cur_leaders%.*} - PREV_LEADERS))
        d_applied=$((${cur_applied%.*} - PREV_APPLIED))
        emit "DELTA — Δapplied=$d_applied Δleaders=$d_leaders Δforged=$d_forged (interval ${SNAPSHOT_INTERVAL}s)"
        PREV_FORGED=${cur_forged%.*}
        PREV_LEADERS=${cur_leaders%.*}
        PREV_APPLIED=${cur_applied%.*}

        if ! pgrep -f "dugite-node run" > /dev/null 2>&1; then
            emit "FATAL — dugite-node process is DEAD (will be restarted on next cycle)"
        fi
        # cardano-node is optional: only required when dugite is behind a
        # relay. In bare-BP mode we run dugite directly on the public
        # network and this check would spam FATAL forever. Set
        # SOAK_REQUIRE_CARDANO_NODE=1 to re-enable.
        if [[ "${SOAK_REQUIRE_CARDANO_NODE:-0}" == "1" ]] \
            && ! pgrep -f "cardano-node run" > /dev/null 2>&1; then
            emit "FATAL — cardano-node process is DEAD"
        fi
    fi

    # Tx submission every TX_INTERVAL
    if (( NOW - LAST_TX >= TX_INTERVAL )); then
        LAST_TX=$NOW
        TX_BATCH_COUNT=$((TX_BATCH_COUNT + 1))
        emit "TX BATCH — #$TX_BATCH_COUNT starting"
        submit_tx_batch
    fi

    # Restart every RESTART_INTERVAL — alternate graceful / hard
    if (( NOW - LAST_RESTART >= RESTART_INTERVAL )); then
        LAST_RESTART=$NOW
        RESTART_COUNT=$((RESTART_COUNT + 1))
        if (( RESTART_COUNT % 3 == 0 )); then
            restart_dugite_node hard
        else
            restart_dugite_node graceful
        fi
        sleep 30  # let node settle
    fi

    sleep "$LOG_TAIL_INTERVAL"
done

emit "SOAK END — duration completed"
emit "===== FINAL SUMMARY ====="
emit "Final BP    — $(snap_dugite)"
emit "Final RELAY — $(snap_haskell)"
emit "Restarts performed:        $RESTART_COUNT"
emit "Tx batches submitted:      $TX_BATCH_COUNT"
emit "Total FORGE ADOPT events:  $(grep -c 'FORGE ADOPT' "$REPORT")"
emit "Total KOIOS OK events:     $(grep -c 'KOIOS OK' "$REPORT")"
emit "Total KOIOS FAIL events:   $(grep -c 'KOIOS FAIL' "$REPORT")"
emit "Total BP CRIT events:      $(grep -c 'BP CRIT' "$REPORT")"
emit "Total FORGE ERROR events:  $(grep -c 'FORGE ERROR' "$REPORT")"
emit "Total RELAY ERR events:    $(grep -c 'RELAY ERR' "$REPORT")"
emit "Report file: $REPORT"
