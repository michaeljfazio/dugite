#!/usr/bin/env bash
# 6h autonomous soak: cardano-node Haskell relay + dugite-node BP on preview.
#
# Schedule:
#   - tx batch        every 30 min (varied: simple, multi-out, metadata, 3-tx chain)
#   - sync check      every  5 min (hang detector + tip-age + slot advance)
#   - health snapshot every  2 min (Prometheus from both nodes)
#   - log scan        every  1 min (forge events, errors, chain-selection flips)
#   - process check   every  5 min (restart dugite if dead — no scheduled restarts)
#
# Forge events on dugite are cross-validated against:
#   1) the Haskell relay log     (TraceDownloadedHeader, TraceAddedToCurrentChain)
#   2) Koios block_info          (canonical chain inclusion)
#
# Chain-selection hang detector watches for:
#   - Slot age > 600s without recovery
#   - >5 rollbacks in 60s (rollback storm)
#   - Block height stalled while sync is < 100%
#
# Soft-fork awareness:
#   - Track fork-switch events on both dugite + relay
#   - Compare tips: dugite must catch up with relay within 30s
#
# Designed to be FULLY AUTONOMOUS — restarts dugite if it dies, never the relay.
# Relay restarts are too disruptive (re-sync from disk) and would mask issues.

set -uo pipefail
cd "$(dirname "$0")/.."

DURATION_SEC=${DURATION_SEC:-21600}            # 6 hours
SNAPSHOT_INTERVAL=${SNAPSHOT_INTERVAL:-120}    # 2 min
SYNC_CHECK_INTERVAL=${SYNC_CHECK_INTERVAL:-300} # 5 min
TX_INTERVAL=${TX_INTERVAL:-1800}               # 30 min
LOG_TAIL_INTERVAL=${LOG_TAIL_INTERVAL:-60}     # 1 min
DUGITE_METRICS=${DUGITE_METRICS:-http://localhost:12798/metrics}
HASKELL_METRICS=${HASKELL_METRICS:-http://127.0.0.1:12797/metrics}
KOIOS_BASE=${KOIOS_BASE:-https://preview.koios.rest/api/v1}
BARE_BP=${BARE_BP:-0}   # set 1 to disable relay-side monitoring (bare-BP mode)

REPORT_DIR=./logs/soak-6h
mkdir -p "$REPORT_DIR"
TS=$(date +%Y%m%d-%H%M%S)
REPORT="$REPORT_DIR/orchestrator-$TS.log"
ln -sf "$(basename "$REPORT")" "$REPORT_DIR/orchestrator.current.log"

BP_LOG=$(readlink -f "./logs/bp-pair/bp.current.log" 2>/dev/null || true)
RELAY_LOG=$(readlink -f "./logs/bp-pair/relay.current.log" 2>/dev/null || true)
if [[ -z "$BP_LOG" || ! -f "$BP_LOG" ]]; then
    echo "FATAL: BP log not found at ./logs/bp-pair/bp.current.log — run launch-bp-pair.sh first"
    exit 1
fi
if [[ "$BARE_BP" != "1" ]] && [[ -z "$RELAY_LOG" || ! -f "$RELAY_LOG" ]]; then
    echo "FATAL: Relay log not found at ./logs/bp-pair/relay.current.log — run launch-bp-pair.sh first"
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

emit "SOAK START — duration=${DURATION_SEC}s tx=${TX_INTERVAL}s sync=${SYNC_CHECK_INTERVAL}s snap=${SNAPSHOT_INTERVAL}s"
emit "SOAK START — bp_log=$BP_LOG"
emit "SOAK START — relay_log=$RELAY_LOG"
emit "SOAK START — pool=Sandstone[SAND] σ≈0.0000247  P(0 forges in 6h) ≈ 97%"
emit "SOAK START — report=$REPORT"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
metric() {
    local url="$1" name="$2"
    curl -s --max-time 5 "$url" 2>/dev/null \
        | awk -v n="$name" '$1==n {print $2; exit}'
}

snap_dugite() {
    local raw
    raw=$(curl -s --max-time 5 "$DUGITE_METRICS" 2>/dev/null) \
        || { echo "metrics_unreachable"; return; }
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
    raw=$(curl -s --max-time 5 "$HASKELL_METRICS" 2>/dev/null) \
        || { echo "metrics_unreachable"; return; }
    local slot block epoch peers density
    slot=$(echo "$raw"    | awk '/^cardano_node_metrics_slotNum_int/ {print $2; exit}')
    block=$(echo "$raw"   | awk '/^cardano_node_metrics_blockNum_int/ {print $2; exit}')
    epoch=$(echo "$raw"   | awk '/^cardano_node_metrics_epoch_int/ {print $2; exit}')
    peers=$(echo "$raw"   | awk '/^cardano_node_metrics_connectedPeers_int/ {print $2; exit}')
    density=$(echo "$raw" | awk '/^cardano_node_metrics_density_real/ {print $2; exit}')
    printf "slot=%s blk=%s ep=%s peers=%s density=%s" \
        "${slot:-?}" "${block:-?}" "${epoch:-?}" "${peers:-?}" "${density:-?}"
}

# Koios block lookup with retry. Returns 0 on found, 1 on missing.
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
            emit "KOIOS OK — block ${hash:0:16}… accepted (epoch=${epoch_no} txs=${tx_count}) attempt=${attempt}"
            return 0
        fi
    done
    emit "KOIOS FAIL — block ${hash} (slot=${slot} block=${block_no}) NOT on canonical chain after 5×30s — likely orphaned"
    return 1
}

# Look for the dugite-forged block on the Haskell relay log.
# Records whether the relay (a) downloaded its header and (b) added it to its chain.
relay_saw_block() {
    local hash="$1" slot="$2"
    sleep 5  # give the relay a moment to apply
    local saw_hdr saw_added
    saw_hdr=$(grep -c "${hash}" "$RELAY_LOG" 2>/dev/null || echo 0)
    saw_added=$(grep -c "TraceAddedToCurrentChain.*${hash}\|AddedToCurrentChain.*${hash}" "$RELAY_LOG" 2>/dev/null || echo 0)
    emit "RELAY-SAW — block ${hash:0:16}… mentions=${saw_hdr} added_to_chain=${saw_added}"
    if (( saw_hdr == 0 )); then
        emit "RELAY-SAW WARN — relay log has no mention of forged block — diffusion failure suspected"
    fi
}

# Restart dugite-node (relay stays up). Used only on process death.
restart_dugite_node() {
    emit "RESTART — dugite-node process died, restarting"
    sleep 2
    rm -f ./node.sock ./db-preview/utxo-store/lock 2>/dev/null || true
    local new_log="./logs/bp-pair/bp-$(date +%Y%m%d-%H%M%S).log"
    emit "RESTART — new log → $new_log"
    nohup caffeinate -dimsu \
        ./target/release/dugite-node run \
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
        > "$new_log" 2>&1 &
    local new_pid=$!
    echo "$new_pid" > ./logs/bp-pair/bp.pid
    ln -sf "$(basename "$new_log")" ./logs/bp-pair/bp.current.log
    BP_LOG="$new_log"; BP_OFFSET=0
    local waited=0
    while [[ ! -S ./node.sock ]] && [[ $waited -lt 180 ]]; do
        sleep 1; waited=$((waited + 1))
        if ! kill -0 "$new_pid" 2>/dev/null; then
            emit "RESTART — dugite-node CRASHED during startup, last 30 lines:"
            tail -30 "$new_log" 2>/dev/null | while IFS= read -r ln; do
                emit "  BP: ${ln:0:240}"
            done
            return 1
        fi
    done
    [[ -S ./node.sock ]] && emit "RESTART — socket ready in ${waited}s pid=$new_pid"
    return 0
}

# Submit varied tx batch.
submit_varied_batch() {
    emit "TX BATCH — submitting varied batch (A/B/C/D=6 txs)"
    local out
    if out=$(./scripts/soak-varied-batch.sh 2>&1); then
        echo "$out" | while IFS= read -r ln; do
            emit "  TX: ${ln:0:240}"
        done
        local accepted rejected
        accepted=$(echo "$out" | grep -oE 'accepted=[0-9]+' | tail -1 | cut -d= -f2)
        rejected=$(echo "$out" | grep -oE 'rejected=[0-9]+' | tail -1 | cut -d= -f2)
        emit "TX BATCH — done accepted=${accepted:-?} rejected=${rejected:-?}"
    else
        emit "TX BATCH — script FAILED:"
        echo "$out" | tail -10 | while IFS= read -r ln; do
            emit "  TX-ERR: ${ln:0:240}"
        done
    fi
}

# ---------------------------------------------------------------------------
# Sync / hang detector
# ---------------------------------------------------------------------------
# Two consecutive 5-min sync checks with no slot advance AND non-empty tip_age
# triggers HANG diagnosis (we capture the running goroutine state via SIGUSR1
# if dugite supports it, otherwise we just record state).
PREV_SYNC_SLOT=""
PREV_SYNC_BLOCK=""
PREV_SYNC_PEERS=""
PREV_SYNC_ROLLS=""
PREV_SYNC_TIP_AGE=""
NO_PROGRESS_TICKS=0
BLOCK_STALL_TICKS=0
TIP_AGE_GROW_TICKS=0

sync_check() {
    local slot block peers tip_age sync rolls
    slot=$(metric "$DUGITE_METRICS" dugite_slot_number)
    block=$(metric "$DUGITE_METRICS" dugite_block_number)
    peers=$(metric "$DUGITE_METRICS" dugite_peers_connected)
    tip_age=$(metric "$DUGITE_METRICS" dugite_tip_age_seconds)
    sync=$(metric "$DUGITE_METRICS" dugite_sync_progress_percent)
    rolls=$(metric "$DUGITE_METRICS" dugite_rollback_count_total)
    slot=${slot:-0}; block=${block:-0}; peers=${peers:-0}
    tip_age=${tip_age:-0}; rolls=${rolls:-0}
    sync=${sync:-0}

    local relay_slot=0 relay_block=0
    if [[ "$BARE_BP" != "1" ]]; then
        relay_slot=$(metric "$HASKELL_METRICS" cardano_node_metrics_slotNum_int)
        relay_block=$(metric "$HASKELL_METRICS" cardano_node_metrics_blockNum_int)
        relay_slot=${relay_slot:-0}; relay_block=${relay_block:-0}
    fi

    local slot_diff block_diff
    slot_diff=$((${slot%.*} - ${PREV_SYNC_SLOT:-0}))
    block_diff=$((${block%.*} - ${PREV_SYNC_BLOCK:-0}))
    local roll_diff
    roll_diff=$((${rolls%.*} - ${PREV_SYNC_ROLLS:-0}))

    emit "SYNC — dugite slot=$slot blk=$block peers=$peers tip_age=${tip_age}s sync=${sync}/100 rolls=$rolls (Δslot=$slot_diff Δblk=$block_diff Δrolls=$roll_diff over ${SYNC_CHECK_INTERVAL}s)"
    if [[ "$BARE_BP" != "1" ]]; then
        emit "SYNC — relay  slot=$relay_slot blk=$relay_block  divergence: Δslot=$((relay_slot - ${slot%.*})) Δblk=$((relay_block - ${block%.*}))"
    fi

    # Hang detection — if BP exists, peers is non-zero, but slot didn't advance
    # over 5 min, that's suspicious. We tolerate one tick (could be rare slots).
    if [[ -n "$PREV_SYNC_SLOT" ]] && (( slot_diff == 0 )) && (( peers > 0 )); then
        NO_PROGRESS_TICKS=$((NO_PROGRESS_TICKS + 1))
        emit "SYNC WARN — no slot advance for ${NO_PROGRESS_TICKS} ticks ($((NO_PROGRESS_TICKS * SYNC_CHECK_INTERVAL))s)"
        if (( NO_PROGRESS_TICKS >= 2 )); then
            emit "HANG SUSPECTED — slot stalled $((NO_PROGRESS_TICKS * SYNC_CHECK_INTERVAL))s with $peers peer(s) — capturing diagnostic"
            emit "HANG DIAG — recent dugite log lines (last 60 since stall):"
            tail -60 "$BP_LOG" 2>/dev/null | while IFS= read -r ln; do
                emit "  STALL: ${ln:0:240}"
            done
            emit "HANG DIAG — relay log recent state:"
            tail -30 "$RELAY_LOG" 2>/dev/null | while IFS= read -r ln; do
                emit "  RELAY: ${ln:0:240}"
            done
            local pid; pid=$(pgrep -f "dugite-node run" || true)
            if [[ -n "$pid" ]]; then
                emit "HANG DIAG — sample stack via lldb (best-effort, may need sudo):"
                (timeout 10 lldb -p "$pid" --batch -o 'thread backtrace all' -o 'quit' 2>&1 | head -80 \
                    | while IFS= read -r ln; do emit "  LLDB: ${ln:0:240}"; done) || \
                emit "  LLDB: (failed — not available or no permission)"
            fi
        fi
    else
        if (( NO_PROGRESS_TICKS > 0 )); then
            emit "SYNC OK — slot resumed advancing (cleared ${NO_PROGRESS_TICKS} prior stall ticks)"
        fi
        NO_PROGRESS_TICKS=0
    fi

    # Rollback storm
    if (( roll_diff > 5 )); then
        emit "ROLLBACK STORM — ${roll_diff} rollbacks in last ${SYNC_CHECK_INTERVAL}s — fork volatility"
    fi

    # Block stall while slot advances — symptom of chain-selection wedging or
    # ledger apply hang on a single fork. Compare ints, not floats.
    if [[ -n "$PREV_SYNC_BLOCK" ]] && (( block_diff == 0 )) && (( slot_diff > 60 )); then
        BLOCK_STALL_TICKS=$((BLOCK_STALL_TICKS + 1))
        emit "BLOCK STALL — block height unchanged for ${BLOCK_STALL_TICKS} ticks while slot advanced ${slot_diff} slots — chain selection wedge suspected"
        if (( BLOCK_STALL_TICKS >= 2 )); then
            emit "BLOCK STALL DIAG — last 40 dugite log lines (ChainSync/ChainSel):"
            tail -40 "$BP_LOG" 2>/dev/null \
                | grep -E "ChainSync|ChainSel|switch_chain|VolatileDB|rollback|RollBackward|RollForward|TriggeredFork" \
                | tail -20 \
                | while IFS= read -r ln; do emit "  WEDGE: ${ln:0:240}"; done
        fi
    else
        BLOCK_STALL_TICKS=0
    fi

    # Tip-age growing — if we're disconnected but reporting peers > 0, tip_age
    # will grow unboundedly. Catch with delta > 200s sustained.
    local age_int=${tip_age%.*}
    local prev_age_int=${PREV_SYNC_TIP_AGE:-0}
    local age_diff=$((age_int - prev_age_int))
    if [[ -n "$PREV_SYNC_TIP_AGE" ]] && (( age_int > 600 )) && (( age_diff > 200 )); then
        TIP_AGE_GROW_TICKS=$((TIP_AGE_GROW_TICKS + 1))
        emit "TIP-AGE GROWING — tip_age=${age_int}s up ${age_diff}s since last tick (${TIP_AGE_GROW_TICKS} ticks)"
    else
        TIP_AGE_GROW_TICKS=0
    fi
    PREV_SYNC_TIP_AGE=$age_int

    # Divergence from relay
    local divergence
    divergence=$((relay_block - ${block%.*}))
    if (( divergence > 5 )); then
        emit "SYNC LAG — dugite is ${divergence} blocks behind relay"
    elif (( divergence < -2 )); then
        emit "SYNC AHEAD — dugite is $((-divergence)) blocks AHEAD of relay (unusual unless we just forged)"
    fi

    PREV_SYNC_SLOT=${slot%.*}
    PREV_SYNC_BLOCK=${block%.*}
    PREV_SYNC_PEERS=$peers
    PREV_SYNC_ROLLS=${rolls%.*}
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

    # Forge adoption
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        local hash slot blkno
        hash=$(echo "$line"  | grep -oE 'block_hash=[a-f0-9]{64}' | head -1 | cut -d= -f2)
        slot=$(echo "$line"  | grep -oE 'slot=[0-9]+' | head -1 | cut -d= -f2)
        blkno=$(echo "$line" | grep -oE 'block_no=[0-9]+' | head -1 | cut -d= -f2)
        emit "FORGE ADOPT — slot=${slot:-?} block=${blkno:-?} hash=${hash:-?}"
        if [[ -n "$hash" ]]; then
            (relay_saw_block "$hash" "${slot:-?}") &
            (koios_check_block "$hash" "${slot:-?}" "${blkno:-?}") &
        fi
    done < <(echo "$new" | grep "TraceAdoptedBlock" 2>/dev/null)

    # Forge-side errors
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        emit "FORGE ERROR — ${line:0:240}"
    done < <(echo "$new" | grep -E "TraceForgedInvalidBlock|TraceForgeStateUpdateError|Forged block announced but NO peers|Forged block has no announcement" 2>/dev/null)

    # Chain-selection / fork-switch events
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        emit "FORK SWITCH — ${line:0:240}"
    done < <(echo "$new" | grep -E "TriggeredFork|SwitchedToAFork|TraceSwitchedToChain|ChainSync.*switch|chain selection" 2>/dev/null)

    # Critical errors / panics
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

    # Dedupe RejectedTx
    local rejected_txids
    rejected_txids=$(echo "$new" | grep -oE 'TraceMempoolRejectedTx[^}]*"txid":"[a-f0-9]+"' \
        | grep -oE '"txid":"[a-f0-9]+"' \
        | sed 's/"txid":"\([a-f0-9]*\)"/\1/' \
        | sort -u | tr '\n' ',' | sed 's/,$//')
    if [[ -n "$rejected_txids" ]]; then
        local unique_count
        unique_count=$(echo "$rejected_txids" | tr ',' '\n' | wc -l | awk '{print $1}')
        emit "RELAY REJECT — ${unique_count} unique txids: ${rejected_txids:0:240}"
    fi

    # Real relay errors
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
LAST_SYNC=0
LAST_TX=$START   # don't tx-spam at T=0; first batch at T+30min
TX_BATCH_COUNT=0
RESTART_COUNT=0

while [[ $(date +%s) -lt $END ]]; do
    NOW=$(date +%s)

    # Process death check
    if ! pgrep -f "dugite-node run" > /dev/null 2>&1; then
        emit "FATAL — dugite-node process is DEAD — restarting"
        RESTART_COUNT=$((RESTART_COUNT + 1))
        restart_dugite_node
        sleep 30
    fi
    if [[ "$BARE_BP" != "1" ]] && ! pgrep -f "cardano-node run" > /dev/null 2>&1; then
        emit "FATAL — cardano-node relay process is DEAD — this is a soak-test invalidating event"
        emit "FATAL — relay log tail:"
        tail -30 "$RELAY_LOG" 2>/dev/null | while IFS= read -r ln; do
            emit "  RELAY-DEAD: ${ln:0:240}"
        done
        emit "FATAL — continuing soak without relay (cross-validation degraded)"
    fi

    scan_bp_log
    if [[ "$BARE_BP" != "1" ]]; then
        scan_relay_log
    fi

    if (( NOW - LAST_SNAP >= SNAPSHOT_INTERVAL )); then
        LAST_SNAP=$NOW
        emit "BP    — $(snap_dugite)"
        if [[ "$BARE_BP" != "1" ]]; then
            emit "RELAY — $(snap_haskell)"
        fi
    fi

    if (( NOW - LAST_SYNC >= SYNC_CHECK_INTERVAL )); then
        LAST_SYNC=$NOW
        sync_check
    fi

    if (( NOW - LAST_TX >= TX_INTERVAL )); then
        LAST_TX=$NOW
        TX_BATCH_COUNT=$((TX_BATCH_COUNT + 1))
        emit "TX BATCH #$TX_BATCH_COUNT — starting"
        submit_varied_batch
    fi

    sleep "$LOG_TAIL_INTERVAL"
done

emit "SOAK END — duration completed"
emit "===== FINAL SUMMARY ====="
emit "Final BP    — $(snap_dugite)"
emit "Final RELAY — $(snap_haskell)"
emit "Tx batches submitted:      $TX_BATCH_COUNT"
emit "Restarts performed:        $RESTART_COUNT (auto, on process death only)"
emit "Total FORGE ADOPT events:  $(grep -c 'FORGE ADOPT' "$REPORT")"
emit "Total KOIOS OK events:     $(grep -c 'KOIOS OK' "$REPORT")"
emit "Total KOIOS FAIL events:   $(grep -c 'KOIOS FAIL' "$REPORT")"
emit "Total FORK SWITCH events:  $(grep -c 'FORK SWITCH' "$REPORT")"
emit "Total HANG SUSPECTED:      $(grep -c 'HANG SUSPECTED' "$REPORT")"
emit "Total ROLLBACK STORM:      $(grep -c 'ROLLBACK STORM' "$REPORT")"
emit "Total BP CRIT events:      $(grep -c 'BP CRIT' "$REPORT")"
emit "Total FORGE ERROR events:  $(grep -c 'FORGE ERROR' "$REPORT")"
emit "Total RELAY ERR events:    $(grep -c 'RELAY ERR' "$REPORT")"
emit "Report file: $REPORT"
