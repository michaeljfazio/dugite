#!/usr/bin/env bash
# Run a 30-min soak test (default), collect evidence into evidence/<ts>/.
# Usage: soak.sh [DURATION_SECONDS]   (default 1800)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/common.sh"

DURATION="${1:-1800}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
# Honour EVIDENCE_DIR if the caller (e.g. devnet-validate recipe) set it,
# so the soak's evidence lands alongside the other suites' output.
EVD="${EVIDENCE_DIR:-$LD_EVIDENCE/$TS}"
mkdir -p "$EVD/logs"

log_info "=== Soak starting (duration ${DURATION}s, evidence $EVD) ==="

# Verify devnet is running (sockets exist + tip queries succeed)
for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$sock" ] || die "Socket $sock not present - start the devnet first (./run.sh)"
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$sock" >/dev/null \
        || die "Tip query failed on $sock"
done

# Metadata snapshot
cat > "$EVD/metadata.json" <<EOF
{
  "timestamp": "$TS",
  "duration_seconds": $DURATION,
  "magic": $LD_MAGIC,
  "ports": { "relay": $LD_RELAY_PORT, "dugite_bp": $LD_DUGITE_BP_PORT, "cardano_bp": $LD_CARDANO_BP_PORT },
  "cardano_node_version": "$(cardano-node --version | awk 'NR==1 {print $2}')",
  "cardano_cli_version": "$(cardano-cli --version | awk 'NR==1 {print $2}')",
  "dugite_node_git": "$(cd "$LD_REPO_ROOT" && git rev-parse HEAD)",
  "genesis_hash_shelley": "$(cardano-cli hash genesis-file --genesis "$LD_GENESIS/shelley-genesis.json")",
  "genesis_hash_conway":  "$(cardano-cli hash genesis-file --genesis "$LD_GENESIS/conway-genesis.json")"
}
EOF

# Write CSV headers
echo "ts,node,slot,block_no,hash,era" > "$EVD/tip-samples.csv"
echo "ts,observer,event,slot,hash,issuer_vkey,body_size,n_txs" > "$EVD/blocks.csv"
echo "ts,target_socket,wave,txid,submit_rc" > "$EVD/tx-submissions.csv"
# tip-age sampler (Issue #508): record the `dugite_tip_age_seconds`
# Prometheus metric from both dugite processes so verify.sh can assert
# the chain is fresh post-catch-up — catches the 19d-stale class of bug
# that slipped through v1.5.0 BP soak (fixed in cb509ef91).
echo "ts,node,tip_age_seconds" > "$EVD/tip-age-samples.csv"

# Sampler PIDs collected for cleanup
SAMPLER_PIDS=()

cleanup() {
    log_info "Stopping samplers"
    # Kill each sampler PID *and* its descendants. The samplers run
    # `tail -F <log> | while read`; the recorded $! is the subshell
    # holding the pipeline, but `tail -F` is a sibling child that
    # may survive a SIGTERM on the subshell alone. Walk descendants
    # to clean up tail/jq/grep processes too.
    kill_tree() {
        local parent="$1" child
        for child in $(pgrep -P "$parent" 2>/dev/null); do
            kill_tree "$child"
        done
        kill -TERM "$parent" 2>/dev/null || true
    }
    for pid in "${SAMPLER_PIDS[@]}"; do
        kill_tree "$pid"
    done
    sleep 1
    for pid in "${SAMPLER_PIDS[@]}"; do
        for descendant in $(pgrep -P "$pid" 2>/dev/null); do
            kill -KILL "$descendant" 2>/dev/null || true
        done
        kill -0 "$pid" 2>/dev/null && kill -KILL "$pid" 2>/dev/null || true
    done
    # Snapshot logs to evidence dir
    cp "$LD_LOGS"/*.log "$EVD/logs/" 2>/dev/null || true
    # Snapshot the tx-zoo results as this round's slice.
    #
    # tx-zoo/state/ lives outside $LD_STATE, so results.csv accumulates across
    # every round of a multi-round run. Without a per-round copy here,
    # generate-release-report.sh falls back to that shared file for EVERY round
    # and then sums it, reporting ~3x the real tx count (and identical per-round
    # figures). Copying it per round gives the report a real per-round slice.
    cp "$LD_ROOT/tx-zoo/state/results.csv" "$EVD/tx-results.csv" 2>/dev/null || true
    log_info "Soak evidence saved to $EVD"
}
trap cleanup EXIT INT TERM

# ---- Tip sampler ----
sample_tips() {
    local out="$1"
    while true; do
        local now
        now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        for entry in "relay:$LD_RELAY_SOCK" "dugite-bp:$LD_DUGITE_BP_SOCK" "cardano-bp:$LD_CARDANO_BP_SOCK"; do
            name="${entry%%:*}"
            sock="${entry##*:}"
            line="$(query_tip_oneline "$sock" 2>/dev/null || printf '?\t?\t?\t?')"
            slot="$(echo "$line" | awk -F'\t' '{print ($1=="" ? "?" : $1)}')"
            blk="$(echo "$line"  | awk -F'\t' '{print ($2=="" ? "?" : $2)}')"
            hash="$(echo "$line" | awk -F'\t' '{print ($3=="" ? "?" : $3)}')"
            era="$(echo "$line"  | awk -F'\t' '{print ($4=="" ? "?" : $4)}')"
            printf '%s,%s,%s,%s,%s,%s\n' "$now" "$name" "$slot" "$blk" "$hash" "$era" >> "$out"
        done
        sleep 5
    done
}
sample_tips "$EVD/tip-samples.csv" &
SAMPLER_PIDS+=($!)
log_info "tip-sampler PID $!"

# ---- Tip-age (Prometheus) sampler ----
sample_tip_age() {
    local out="$1"
    while true; do
        local now
        now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        for entry in "dugite-bp:$LD_DUGITE_BP_METRICS_PORT" "dugite-relay:$LD_DUGITE_RELAY_METRICS_PORT"; do
            name="${entry%%:*}"
            port="${entry##*:}"
            age=$(curl -s --max-time 3 "http://127.0.0.1:${port}/metrics" 2>/dev/null \
                  | awk '/^dugite_tip_age_seconds / {print $2; exit}')
            [ -z "$age" ] && age="?"
            printf '%s,%s,%s\n' "$now" "$name" "$age" >> "$out"
        done
        sleep 5
    done
}
sample_tip_age "$EVD/tip-age-samples.csv" &
SAMPLER_PIDS+=($!)
log_info "tip-age-sampler PID $!"

# ---- Block recorder ----
# Tails the three node logs and records forge + receive events.
#
# Observed log lines (real soak, 2026-05-16):
#
#   dugite (relay or bp) — text format with `forge:` target for forge events:
#     2026-05-16T06:07:47.684Z  INFO forge: TraceForgedBlock slot=21 block_no=0 block_hash=bba2... txs=0
#     2026-05-16T06:07:47.695Z  INFO forge: TraceAdoptedBlock block_no=0 slot=21 block_hash=bba2... txs=0
#     2026-05-16T06:07:29.692Z  INFO dugite_node::node: Chain extended era=Conway slot=2 block=0 txs=0 hash=a36d...
#
#   cardano-bp — MachineFormat JSON (one object per line):
#     {"ns":"Forge.Loop.AdoptedBlock","data":{"slot":2,"blockHash":"a36d...","blockSize":828,...}}
#     {"ns":"ChainDB.AddBlockEvent.AddedToCurrentChain","data":{"newtip":"a36d...@2","newSuffixSelectView":{"slotNo":2,...}}}
record_dugite() {
    local observer="$1" log="$2" out="$3"
    tail -n 0 -F "$log" 2>/dev/null | while IFS= read -r line; do
        local now slot hash issuer
        now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        if echo "$line" | grep -q 'TraceForgedBlock'; then
            slot=$(echo "$line"  | grep -oE 'slot=[0-9]+'         | head -1 | cut -d= -f2)
            hash=$(echo "$line"  | grep -oE 'block_hash=[a-f0-9]+'| head -1 | cut -d= -f2)
            printf '%s,%s,forge,%s,%s,,,\n' "$now" "$observer" "${slot:-?}" "${hash:-?}" >> "$out"
        elif echo "$line" | grep -q 'Chain extended'; then
            slot=$(echo "$line"  | grep -oE 'slot=[0-9]+'  | head -1 | cut -d= -f2)
            hash=$(echo "$line"  | grep -oE 'hash=[a-f0-9]+'| head -1 | cut -d= -f2)
            printf '%s,%s,recv,%s,%s,,,\n' "$now" "$observer" "${slot:-?}" "${hash:-?}" >> "$out"
        fi
    done
}

record_cardano() {
    local observer="$1" log="$2" out="$3"
    tail -n 0 -F "$log" 2>/dev/null | while IFS= read -r line; do
        # cardano-node MachineFormat — one JSON object per line. Use jq for parsing.
        ns=$(echo "$line" | jq -r '.ns // empty' 2>/dev/null) || continue
        case "$ns" in
            "Forge.Loop.AdoptedBlock"|"Forge.Loop.ForgedBlock")
                ts=$(  echo "$line" | jq -r '.at // empty'                            2>/dev/null)
                slot=$(echo "$line" | jq -r '.data.slot // empty'                     2>/dev/null)
                # AdoptedBlock uses .data.blockHash; ForgedBlock uses .data.block
                hash=$(echo "$line" | jq -r '.data.blockHash // .data.block // empty' 2>/dev/null)
                printf '%s,%s,forge,%s,%s,,,\n' "${ts:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}" "$observer" "${slot:-?}" "${hash:-?}" >> "$out"
                ;;
            "ChainDB.AddBlockEvent.AddedToCurrentChain")
                ts=$(  echo "$line" | jq -r '.at // empty'                              2>/dev/null)
                slot=$(echo "$line" | jq -r '.data.newSuffixSelectView.slotNo // empty' 2>/dev/null)
                # newtip is "hash@slot" — strip the @slot suffix
                hash=$(echo "$line" | jq -r '.data.newtip // empty'                     2>/dev/null | cut -d@ -f1)
                printf '%s,%s,recv,%s,%s,,,\n' "${ts:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}" "$observer" "${slot:-?}" "${hash:-?}" >> "$out"
                ;;
        esac
    done
}

record_dugite  "dugite-bp"    "$LD_LOGS/dugite-bp.log"    "$EVD/blocks.csv" &
SAMPLER_PIDS+=($!)
record_dugite  "dugite-relay" "$LD_LOGS/dugite-relay.log" "$EVD/blocks.csv" &
SAMPLER_PIDS+=($!)
record_cardano "cardano-bp"   "$LD_LOGS/cardano-bp.log"   "$EVD/blocks.csv" &
SAMPLER_PIDS+=($!)
log_info "block-recorder pids: ${SAMPLER_PIDS[*]: -3}"

# ---- Tx injector ----
# Submits 5 txs to each of the 3 sockets at T+120s, T+600s, T+1200s.
inject_wave() {
    local wave="$1"
    local out="$2"
    log_info "tx-injector: wave $wave starting"
    for entry in "relay:$LD_RELAY_SOCK" "dugite-bp:$LD_DUGITE_BP_SOCK" "cardano-bp:$LD_CARDANO_BP_SOCK"; do
        name="${entry%%:*}"
        sock="${entry##*:}"
        # Capture both txids and a per-tx return code.
        while IFS= read -r txid; do
            rc=$?
            ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
            printf '%s,%s,%s,%s,%s\n' "$ts" "$sock" "$wave" "$txid" "$rc" >> "$out"
        done < <("$SCRIPT_DIR/submit-txs.sh" "$sock" 5 "wave${wave}-${name}" 2>/dev/null || true)
    done
    log_info "tx-injector: wave $wave done"
}

inject_runner() {
    local out="$1"
    local start
    start="$(date +%s)"
    local wave=0
    # Wave triggers (seconds from soak start)
    for w in 120 600 1200; do
        while [ $(( $(date +%s) - start )) -lt "$w" ]; do
            sleep 5
            # If soak duration is shorter than the wave trigger, exit early
            if [ $(( $(date +%s) - start )) -ge "$DURATION" ]; then
                return
            fi
        done
        wave=$((wave + 1))
        inject_wave "$wave" "$out"
    done
}
inject_runner "$EVD/tx-submissions.csv" &
SAMPLER_PIDS+=($!)
log_info "tx-injector PID $!"

END_EPOCH=$(($(date +%s) + DURATION))
log_info "Soak end at epoch $END_EPOCH ($(date -u -r $END_EPOCH 2>/dev/null || date -u -d @$END_EPOCH))"

# Main loop: print a heartbeat every 30s
while [ "$(date +%s)" -lt "$END_EPOCH" ]; do
    REMAINING=$((END_EPOCH - $(date +%s)))
    RELAY_TIP="$(query_slot "$LD_RELAY_SOCK" 2>/dev/null || echo ?)"
    DBP_TIP="$(query_slot "$LD_DUGITE_BP_SOCK" 2>/dev/null || echo ?)"
    CBP_TIP="$(query_slot "$LD_CARDANO_BP_SOCK" 2>/dev/null || echo ?)"
    log_info "[+$((DURATION-REMAINING))s / ${DURATION}s] tips: relay=$RELAY_TIP dugite-bp=$DBP_TIP cardano-bp=$CBP_TIP"
    sleep 30
done

log_info "Soak duration reached. Cleanup running."
