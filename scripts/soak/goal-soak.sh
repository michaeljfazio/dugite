#!/usr/bin/env bash
# Single-network soak driver for the "30-min from fresh mithril" goal.
#
# Usage: ./scripts/soak/goal-soak.sh <preview|preprod|mainnet> [SOAK_MINUTES]
#
# Steps:
#   1) Wipe ./db-<network>/ for a fresh start.
#   2) Mithril import.
#   3) Launch relay in background (caffeinate on macOS).
#   4) Wait for "on tip" (dugite_slot_number within 5 of peer-reported tip).
#   5) Hold on tip for $SOAK_MINUTES (default 30).
#   6) Scan logs for anomalies (panic/fatal/decode errors/etc.).
#   7) Tear down. Exit 0 on clean run, non-zero otherwise.

set -eu
# Deliberately NOT using pipefail: many curls during sync probing can fail
# transiently (port not ready, koios hiccup) and we tolerate them.
cd "$(dirname "$0")/../.."

NETWORK="${1:?usage: $0 <preview|preprod|mainnet> [minutes]}"
SOAK_MINUTES="${2:-30}"

case "$NETWORK" in
    preview) MAGIC=2;          PORT=3001; METRICS=12796; KOIOS_TIP="https://preview.koios.rest/api/v1/tip" ;;
    preprod) MAGIC=1;          PORT=3001; METRICS=12799; KOIOS_TIP="https://preprod.koios.rest/api/v1/tip" ;;
    mainnet) MAGIC=764824073;  PORT=3001; METRICS=12800; KOIOS_TIP="https://api.koios.rest/api/v1/tip" ;;
    *) echo "Unknown network: $NETWORK"; exit 2 ;;
esac

DB="./db-$NETWORK"
SOCK="./node-$NETWORK.sock"
LOG_DIR="./logs/goal-soak"
mkdir -p "$LOG_DIR"
TS=$(date +%Y%m%d-%H%M%S)
LOG="$LOG_DIR/${NETWORK}-${TS}.log"
PIDFILE="$LOG_DIR/${NETWORK}.pid"
REPORT="$LOG_DIR/${NETWORK}-${TS}.report"

BIN=./target/release/dugite-node

emit() { printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$1" | tee -a "$REPORT"; }

# Extract a single Prometheus metric value by name (returns "" if absent).
metric_value() {
    local name="$1"
    curl -fsS --max-time 5 "http://127.0.0.1:$METRICS/metrics" 2>/dev/null \
        | awk -v n="^${name} " '$0 ~ n {print $2; exit}'
}

cleanup() {
    if [[ -f "$PIDFILE" ]]; then
        local pid; pid=$(cat "$PIDFILE" 2>/dev/null || true)
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            emit "stopping dugite-node pid=$pid"
            kill -TERM "$pid" 2>/dev/null || true
            for _ in {1..30}; do
                kill -0 "$pid" 2>/dev/null || break
                sleep 1
            done
            kill -KILL "$pid" 2>/dev/null || true
        fi
        rm -f "$PIDFILE"
    fi
    rm -f "$SOCK" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

pkill -f "dugite-node run" 2>/dev/null || true
sleep 1

emit "=== Goal soak: $NETWORK | $SOAK_MINUTES minutes ==="
emit "DB:     $DB"
emit "Log:    $LOG"
emit "Report: $REPORT"

# 1) Wipe DB (skipped when SKIP_MITHRIL=1 — for fast retry after script-only
#    failures where the previous fresh import is still valid on disk).
if [[ "${SKIP_MITHRIL:-0}" != "1" ]] && [[ -d "$DB" ]]; then
    emit "Wiping existing $DB"
    rm -rf "$DB"
elif [[ "${SKIP_MITHRIL:-0}" == "1" ]]; then
    emit "SKIP_MITHRIL=1 — reusing existing $DB"
fi

# 2) Mithril import (skipped when SKIP_MITHRIL=1)
if [[ "${SKIP_MITHRIL:-0}" == "1" ]] && [[ -d "$DB/immutable" ]]; then
    emit "SKIP_MITHRIL=1 — skipping mithril-import"
else
    emit "Importing mithril snapshot (network=$NETWORK magic=$MAGIC)"
    IMPORT_LOG="$LOG_DIR/${NETWORK}-${TS}.mithril.log"
    if ! "$BIN" mithril-import --network-magic "$MAGIC" --database-path "$DB" >"$IMPORT_LOG" 2>&1; then
        if grep -qi "no space left on device" "$IMPORT_LOG"; then
            emit "FAIL: mithril-import out of space"
            tail -40 "$IMPORT_LOG" >>"$REPORT"
            exit 3
        fi
        emit "mithril-import failed; retrying once"
        if ! "$BIN" mithril-import --network-magic "$MAGIC" --database-path "$DB" >>"$IMPORT_LOG" 2>&1; then
            emit "FAIL: mithril-import failed twice"
            tail -80 "$IMPORT_LOG" >>"$REPORT"
            exit 3
        fi
    fi
    emit "Mithril import OK"
fi

# 3) Launch relay (background)
CONFIG="config/$NETWORK/config.json"
TOPOLOGY="config/$NETWORK/topology.json"

CMD=(
    "$BIN" run
    --config "$CONFIG"
    --topology "$TOPOLOGY"
    --database-path "$DB"
    --socket-path "$SOCK"
    --host-addr 0.0.0.0
    --port "$PORT"
)

if [[ "$(uname -s)" == "Darwin" ]] && command -v caffeinate >/dev/null 2>&1; then
    LAUNCH=("caffeinate" "-dimsu" "${CMD[@]}")
else
    LAUNCH=("${CMD[@]}")
fi

emit "Launching: ${LAUNCH[*]}"
nohup "${LAUNCH[@]}" >"$LOG" 2>&1 &
PID=$!
echo "$PID" >"$PIDFILE"
emit "Dugite started: pid=$PID log=$LOG"

# 4) Wait for on-tip.
#    "on tip" := dugite_slot_number within 5 slots of dugite_max_peer_tip_slot
#    (i.e. as close as one of our peers thinks the chain has reached).
#    We also cross-check vs Koios when available, but don't gate on it.
emit "Waiting for dugite to reach tip…"
on_tip_at=""
last_slot_seen=0
warmup_stall=0   # ticks observed before metric reports any slot at all
sync_stall=0    # ticks observed where slot didn't grow but is > 0
START_WAIT=$(date +%s)
MAX_WAIT=$((90 * 60))   # 90 minutes to reach tip after mithril import
while true; do
    if ! kill -0 "$PID" 2>/dev/null; then
        emit "FAIL: dugite-node crashed during sync"
        tail -120 "$LOG" >>"$REPORT"
        exit 4
    fi
    now=$(date +%s); elapsed=$((now - START_WAIT))
    if (( elapsed > MAX_WAIT )); then
        emit "FAIL: did not reach tip within $((MAX_WAIT/60)) min"
        tail -120 "$LOG" >>"$REPORT"
        exit 4
    fi

    DUGITE_SLOT=$(metric_value dugite_slot_number); DUGITE_SLOT=${DUGITE_SLOT:-0}
    PEER_SLOT=$(metric_value dugite_max_peer_tip_slot);   PEER_SLOT=${PEER_SLOT:-0}

    # Koios cross-check (informational only)
    KOIOS_SLOT=$(curl -fsS --max-time 8 "$KOIOS_TIP" 2>/dev/null | python3 -c 'import sys,json
try:
    j=json.load(sys.stdin)
    if isinstance(j,list) and j: print(int(j[0].get("abs_slot",0)))
    elif isinstance(j,dict): print(int(j.get("abs_slot",0)))
    else: print(0)
except Exception:
    print(0)' 2>/dev/null || echo 0)
    KOIOS_SLOT=${KOIOS_SLOT:-0}

    if (( DUGITE_SLOT == 0 )); then
        warmup_stall=$((warmup_stall + 1))
    elif (( DUGITE_SLOT > last_slot_seen )); then
        last_slot_seen=$DUGITE_SLOT
        sync_stall=0
    else
        sync_stall=$((sync_stall + 1))
    fi

    emit "sync: dugite=$DUGITE_SLOT peer=$PEER_SLOT koios=$KOIOS_SLOT elapsed=${elapsed}s"

    # On-tip condition: peer reported a tip, and we're within 5 slots of it.
    if (( PEER_SLOT > 0 )) && (( DUGITE_SLOT > 0 )); then
        diff=$((PEER_SLOT - DUGITE_SLOT))
        (( diff < 0 )) && diff=$((-diff))
        if (( diff <= 5 )); then
            on_tip_at=$(date +%s)
            emit "REACHED TIP — diff=$diff (dugite=$DUGITE_SLOT, peer=$PEER_SLOT, koios=$KOIOS_SLOT)"
            break
        fi
    fi

    # Failure thresholds (only after we'd reasonably expect progress).
    if (( warmup_stall >= 60 )); then
        emit "FAIL: dugite_slot_number never advanced past 0 in 10 minutes (metric never populated)"
        tail -200 "$LOG" >>"$REPORT"
        exit 5
    fi
    if (( sync_stall >= 60 )); then
        emit "FAIL: dugite slot stuck at $last_slot_seen for ~10 min"
        tail -200 "$LOG" >>"$REPORT"
        exit 5
    fi
    sleep 10
done

# 5) Hold on tip for SOAK_MINUTES.
#
#    "Anomalous behaviour" semantics:
#      - PEER-DRIFT FAIL: dugite_slot_number is behind dugite_max_peer_tip_slot
#                   by >60 slots AND that condition persists for 2+ ticks.
#                   This is the genuine "dugite is failing to ingest blocks
#                   its peers already told it about" signal.
#      - KOIOS-DRIFT FAIL: dugite_slot_number is behind Koios's reported tip
#                   by >180 slots AND that condition persists for 3+ ticks.
#                   Mainnet block-propagation jitter, koios upstream-relay
#                   bursts, and our own peer-selection topology can produce
#                   transient 60–120 slot lags vs Koios that recover within a
#                   couple of ticks; failing on those gives false positives.
#      - SOLO STALL FAIL: dugite_slot_number has not advanced for >5 min WHILE
#                   the network tip *has* advanced. Pure-network quiet (both
#                   dugite and koios stuck at the same slot) is not a fail —
#                   that's just slot drought.
#    The peer-tip metric is allowed to lag dugite_slot_number (dugite may have
#    adopted a block ahead of the next MsgRollForward observed peer-tip
#    update); that is NOT a drift event.
emit "=== Soaking on tip for $SOAK_MINUTES minutes ==="
SOAK_END=$((on_tip_at + SOAK_MINUTES * 60))
last_check_slot=$last_slot_seen
last_network_slot=0
joint_stall=0
peer_drift_ticks=0
koios_drift_ticks=0
while (( $(date +%s) < SOAK_END )); do
    if ! kill -0 "$PID" 2>/dev/null; then
        emit "FAIL: dugite-node crashed during on-tip soak"
        tail -200 "$LOG" >>"$REPORT"
        exit 6
    fi
    sleep 30

    DUGITE_SLOT=$(metric_value dugite_slot_number); DUGITE_SLOT=${DUGITE_SLOT:-0}
    PEER_SLOT=$(metric_value dugite_max_peer_tip_slot); PEER_SLOT=${PEER_SLOT:-0}
    KOIOS_SLOT=$(curl -fsS --max-time 8 "$KOIOS_TIP" 2>/dev/null | python3 -c 'import sys,json
try:
    j=json.load(sys.stdin)
    if isinstance(j,list) and j: print(int(j[0].get("abs_slot",0)))
    elif isinstance(j,dict): print(int(j.get("abs_slot",0)))
    else: print(0)
except Exception:
    print(0)' 2>/dev/null || echo 0)
    KOIOS_SLOT=${KOIOS_SLOT:-0}

    # Reference tip for SOLO-STALL detection (advance signal).
    if (( KOIOS_SLOT > 0 )); then
        NET_TIP=$KOIOS_SLOT
    elif (( PEER_SLOT > DUGITE_SLOT )); then
        NET_TIP=$PEER_SLOT
    else
        NET_TIP=$DUGITE_SLOT
    fi

    peer_behind=$((PEER_SLOT - DUGITE_SLOT))
    (( peer_behind < 0 )) && peer_behind=0
    koios_behind=$((KOIOS_SLOT - DUGITE_SLOT))
    (( koios_behind < 0 )) && koios_behind=0

    rem=$((SOAK_END - $(date +%s)))
    emit "soak: dugite=$DUGITE_SLOT peer=$PEER_SLOT koios=$KOIOS_SLOT peer_behind=$peer_behind koios_behind=$koios_behind remaining=${rem}s"

    # Joint stall: tick where dugite didn't advance but network DID.
    if (( DUGITE_SLOT == last_check_slot )) && (( NET_TIP > last_network_slot )); then
        joint_stall=$((joint_stall + 1))
    else
        joint_stall=0
    fi
    last_check_slot=$DUGITE_SLOT
    last_network_slot=$NET_TIP

    # PEER-DRIFT: dugite genuinely lagging what its OWN peers reported.
    if (( peer_behind > 60 )); then
        peer_drift_ticks=$((peer_drift_ticks + 1))
    else
        peer_drift_ticks=0
    fi
    # KOIOS-DRIFT: dugite far behind external reference. More lenient because
    # mainnet block bursts + peer-selection topology cause transient lag.
    if (( KOIOS_SLOT > 0 )) && (( koios_behind > 180 )); then
        koios_drift_ticks=$((koios_drift_ticks + 1))
    else
        koios_drift_ticks=0
    fi

    if (( joint_stall >= 10 )); then
        emit "FAIL: dugite stuck while network advanced — solo stall for ~5 min"
        tail -200 "$LOG" >>"$REPORT"
        exit 7
    fi
    if (( peer_drift_ticks >= 2 )); then
        emit "FAIL: dugite lags its peers by >60 slots for 2+ ticks (peer_behind=$peer_behind)"
        tail -200 "$LOG" >>"$REPORT"
        exit 8
    fi
    if (( koios_drift_ticks >= 3 )); then
        emit "FAIL: dugite lags Koios by >180 slots for 3+ ticks (koios_behind=$koios_behind)"
        tail -200 "$LOG" >>"$REPORT"
        exit 9
    fi
done

emit "=== Soak complete; scanning logs for anomalies ==="

# 6) Anomaly scan.
#    These patterns are deliberately strict: anything that hits one of them is
#    treated as a soak fail. Add to the suppression second-stage `grep -ivE`
#    chain if a known-benign noise pattern appears.
BAD_PATTERNS='panic|stack overflow|out of memory|segfault|assertion failed|fatal error|unwrap.*on.*None|unwrap.*on.*Err|decoder error|malformed|InvalidHeader|ApplyBlockError|ChainSyncProtocolError|BlockFetchProtocolError|ConsensusError|Failed to decode|RuntimeError|thread .* panicked|protocol violation'
ANOM_COUNT=$(grep -iE "$BAD_PATTERNS" "$LOG" 2>/dev/null | grep -ivE 'mempool_rejected|TraceMempoolRejectedTx' | wc -l | awk '{print $1}')
emit "Anomaly lines found: $ANOM_COUNT"
if (( ANOM_COUNT > 0 )); then
    emit "ANOMALIES — sample (first 30):"
    grep -iE "$BAD_PATTERNS" "$LOG" | grep -ivE 'mempool_rejected|TraceMempoolRejectedTx' | head -30 | sed 's/^/    /' | tee -a "$REPORT"
    exit 9
fi

emit "PASS: $NETWORK soaked clean for $SOAK_MINUTES minutes on tip"
exit 0
