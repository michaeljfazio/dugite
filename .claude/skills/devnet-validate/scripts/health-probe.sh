#!/usr/bin/env bash
# health-probe.sh — one-shot runtime health verdict for a running dugite-node.
#
# Implements the 14-step evaluation procedure from references/health.md.
# Steps cover liveness (1-3), peers (4), tip (5), apply (6), forge (7),
# snapshot (8), network throughput (9), connection thrash (10), Haskell
# parity via cardano-bp Prometheus (11), Haskell adoption via cardano-bp.log
# (12), dugite log deltas (13), and Haskell log cross-validation (14).
# Exits 0 on healthy, non-zero on any anomaly. Always prints one line per
# check; suitable for inclusion in soak loops, CI gates, or human triage.
#
# Usage:
#   health-probe.sh [--port 12798]              # dugite-bp prometheus
#                   [--cardano-port 12800]      # cardano-bp prometheus
#                   [--log logs/dugite-bp.log]
#                   [--relay-log logs/dugite-relay.log]
#                   [--cardano-log logs/cardano-bp.log]
#                   [--baseline-dir /tmp/dugite-health-baseline]
#                   [--net-window 5]            # seconds between metric scrapes
#                   [--public]                  # relax thresholds for public testnets
#                   [--verbose]
#
# The baseline directory stores counter values from the previous run, so
# "no new errors since last probe" can be evaluated. First run establishes
# the baseline and treats all counters as OK. Default location is
# $TMPDIR/dugite-health-baseline. Reset between unrelated soaks.
#
# Exit codes:
#   0 — HEALTHY
#   1 — SICK (one or more anomalies; printed before verdict)
#   2 — usage error
set -euo pipefail

PORT=12798
CARDANO_PORT=12800
LOG=""
RELAY_LOG=""
CARDANO_LOG=""
BASELINE_DIR="${TMPDIR:-/tmp}/dugite-health-baseline"
PUBLIC=0
VERBOSE=0
NET_WINDOW=5         # seconds between the two metric scrapes

while [ $# -gt 0 ]; do
    case "$1" in
        --port)            PORT="$2"; shift 2 ;;
        --cardano-port)    CARDANO_PORT="$2"; shift 2 ;;
        --log)             LOG="$2"; shift 2 ;;
        --relay-log)       RELAY_LOG="$2"; shift 2 ;;
        --cardano-log)     CARDANO_LOG="$2"; shift 2 ;;
        --baseline-dir)    BASELINE_DIR="$2"; shift 2 ;;
        --net-window)      NET_WINDOW="$2"; shift 2 ;;
        --public)          PUBLIC=1; shift ;;
        --verbose)         VERBOSE=1; shift ;;
        -h|--help)         sed -n '2,/^set -e/p' "$0" | sed -e 's/^# \{0,1\}//' -e '$d'; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

mkdir -p "$BASELINE_DIR"

# Auto-locate logs if not provided. Try both possible cwds (repo root and
# the devnet directory) so the probe works from either.
auto_locate() {
    local name="$1"
    for cand in "testnet/local-devnet/logs/$name" "logs/$name" "../../testnet/local-devnet/logs/$name"; do
        if [ -f "$cand" ]; then printf '%s' "$cand"; return; fi
    done
}
[ -z "$LOG" ]         && LOG="$(auto_locate dugite-bp.log)"
[ -z "$RELAY_LOG" ]   && RELAY_LOG="$(auto_locate dugite-relay.log)"
[ -z "$CARDANO_LOG" ] && CARDANO_LOG="$(auto_locate cardano-bp.log)"

# Thresholds — tighter on devnet, looser on public testnets.
TIP_AGE_MAX=5
PEERS_MIN=1
SLOT_PARITY=5
if [ $PUBLIC -eq 1 ]; then
    TIP_AGE_MAX=60
    PEERS_MIN=2
    SLOT_PARITY=20
fi

ANOMALIES=()
SUMMARY=()

ok()    { SUMMARY+=("  ✓ $1"); }
fail()  { SUMMARY+=("  ✗ $1"); ANOMALIES+=("$1"); }
info()  { [ $VERBOSE -eq 1 ] && SUMMARY+=("  · $1") || true; }

# Safe integer coercion: empty / non-numeric → 0. Never trips set -e.
toi() {
    awk -v v="${1:-}" 'BEGIN{ if (v ~ /^-?[0-9]+(\.[0-9]+)?$/) printf "%.0f", v; else print 0 }'
}

# Compute b - a with empty/non-numeric treated as 0.
delta() {
    awk -v a="${1:-}" -v b="${2:-}" 'BEGIN{
        if (a !~ /^-?[0-9]+(\.[0-9]+)?$/) a=0
        if (b !~ /^-?[0-9]+(\.[0-9]+)?$/) b=0
        printf "%.0f", b-a
    }'
}

abs() { v=$(toi "${1:-0}"); [ "$v" -lt 0 ] && echo $((-v)) || echo "$v"; }

metric() {
    awk -v k="$1" '$1==k {print $2; found=1; exit} END{ if(!found) print ""}' "$SCRAPE"
}

# --- 1. Prometheus port responding ------------------------------------------
SCRAPE="$(mktemp)"
CARDANO_SCRAPE="$(mktemp)"
SCRAPE2="$(mktemp)"
trap 'rm -f "$SCRAPE" "$SCRAPE2" "$CARDANO_SCRAPE"' EXIT

if ! curl -fs --max-time 3 "localhost:${PORT}/metrics" > "$SCRAPE"; then
    fail "prometheus :${PORT} not responding"
    printf '%s\n' "${SUMMARY[@]}"
    echo "verdict: FAIL (cannot reach :${PORT})"
    exit 1
fi
ok "prometheus :${PORT} responding"

# --- 2. Identity / role -----------------------------------------------------
IS_BP=$(metric dugite_is_block_producer)
NET_MAGIC=$(metric dugite_network_magic)
DIFF_MODE=$(metric dugite_diffusion_mode)
info "role: is_bp=${IS_BP:-?} network_magic=${NET_MAGIC:-?} diffusion=${DIFF_MODE:-?}"

# --- 3. Wall-clock / liveness + network deltas -----------------------------
# Snapshot the counters that we'll diff against after NET_WINDOW seconds.
SLOT_A=$(metric dugite_slot_number)
BLOCKS_RX_A=$(metric dugite_blocks_received_total)
BLOCKS_APPLY_A=$(metric dugite_blocks_applied_total)
TXS_RX_A=$(metric dugite_transactions_received_total)
CONN_TOTAL_A=$(metric dugite_n2n_connections_total)
PEERS_A=$(metric dugite_peers_connected)

sleep "$NET_WINDOW"

if ! curl -fs --max-time 3 "localhost:${PORT}/metrics" > "$SCRAPE2"; then
    fail "prometheus :${PORT} stopped responding mid-probe"
fi
SLOT_B=$(awk '$1=="dugite_slot_number" {print $2; exit}' "$SCRAPE2")

SLOT_A_I=$(toi "$SLOT_A")
SLOT_B_I=$(toi "$SLOT_B")
if [ -n "$SLOT_A" ] && [ -n "$SLOT_B" ] && [ "$SLOT_B_I" -gt "$SLOT_A_I" ]; then
    ok "wall-clock advancing (slot $SLOT_A_I → $SLOT_B_I)"
else
    fail "wall-clock NOT advancing (slot stayed at ${SLOT_A:-?}); node frozen?"
fi

# Compute deltas now while we still have both scrapes.
DELTA_BLOCKS_RX=$(delta "$BLOCKS_RX_A"   "$(awk '$1=="dugite_blocks_received_total"     {print $2; exit}' "$SCRAPE2")")
DELTA_BLOCKS_APPLY=$(delta "$BLOCKS_APPLY_A" "$(awk '$1=="dugite_blocks_applied_total"      {print $2; exit}' "$SCRAPE2")")
DELTA_TXS_RX=$(delta "$TXS_RX_A"        "$(awk '$1=="dugite_transactions_received_total" {print $2; exit}' "$SCRAPE2")")
DELTA_CONN_TOTAL=$(delta "$CONN_TOTAL_A" "$(awk '$1=="dugite_n2n_connections_total"      {print $2; exit}' "$SCRAPE2")")
DELTA_PEERS=$(delta "$PEERS_A"          "$(awk '$1=="dugite_peers_connected"             {print $2; exit}' "$SCRAPE2")")
SLOT_DELTA=$(delta "$SLOT_A" "$SLOT_B")

# Subsequent checks read the freshest scrape.
mv "$SCRAPE2" "$SCRAPE"

# --- 4. Peers ---------------------------------------------------------------
PEERS=$(metric dugite_peers_connected)
PEERS_HOT=$(metric dugite_peers_hot)
PEERS_INT=$(toi "${PEERS:-0}")
HOT_INT=$(toi "${PEERS_HOT:-0}")
if [ "$PEERS_INT" -ge "$PEERS_MIN" ]; then
    ok "peers connected = $PEERS_INT (≥$PEERS_MIN); hot = $HOT_INT"
else
    fail "peers connected = $PEERS_INT (<$PEERS_MIN)"
fi
if [ "$HOT_INT" -lt 1 ] && [ "$PEERS_INT" -ge 1 ]; then
    fail "peers connected ($PEERS_INT) but zero hot — peer stuck in warm state"
fi

# --- 5. Tip age / chain progress -------------------------------------------
TIP_AGE=$(metric dugite_tip_age_seconds)
CSY_IDLE=$(metric dugite_chainsync_idle_seconds)
TIP_AGE_INT=$(toi "${TIP_AGE:-0}")
CSY_INT=$(toi "${CSY_IDLE:-0}")
if [ "$TIP_AGE_INT" -le "$TIP_AGE_MAX" ]; then
    ok "tip_age=${TIP_AGE_INT}s (≤${TIP_AGE_MAX}s); chainsync_idle=${CSY_INT}s"
else
    fail "tip_age=${TIP_AGE_INT}s exceeds ${TIP_AGE_MAX}s — node may be stalled"
fi

SYNC_PCT=$(metric dugite_sync_progress_percent)
if [ -n "$SYNC_PCT" ]; then
    info "sync_progress=$(awk -v v="$SYNC_PCT" 'BEGIN{printf "%.2f%%", v/100}')"
fi

# --- 6. Apply pipeline ------------------------------------------------------
APPLY_FAIL=$(metric dugite_block_apply_failures_total)
ROLLBACK=$(metric dugite_rollback_count_total)
APPLY_FAIL_INT=$(toi "${APPLY_FAIL:-0}")
if [ "$APPLY_FAIL_INT" -eq 0 ]; then
    ok "block_apply_failures = 0; rollbacks = $(toi "${ROLLBACK:-0}")"
else
    fail "block_apply_failures = ${APPLY_FAIL_INT} (must be 0)"
fi

# --- 7. Forge pipeline (BP only) -------------------------------------------
IS_BP_INT=$(toi "${IS_BP:-0}")
if [ "$IS_BP_INT" -eq 1 ]; then
    FORGE_FAIL=$(metric dugite_forge_failures_total)
    FORGED=$(metric dugite_blocks_forged_total)
    LEADER_CHECKS=$(metric dugite_leader_checks_total)
    FORGE_FAIL_INT=$(toi "${FORGE_FAIL:-0}")
    if [ "$FORGE_FAIL_INT" -eq 0 ]; then
        ok "forge_failures = 0; forged = $(toi "${FORGED:-0}"); leader_checks = $(toi "${LEADER_CHECKS:-0}")"
    else
        fail "forge_failures = ${FORGE_FAIL_INT} (must be 0)"
    fi
    # Leader-check delta from a per-port baseline file.
    PREV_LC_FILE="$BASELINE_DIR/leader_checks-${PORT}"
    LC_NOW=$(toi "${LEADER_CHECKS:-0}")
    if [ -f "$PREV_LC_FILE" ]; then
        PREV_LC=$(toi "$(cat "$PREV_LC_FILE")")
        if [ "$LC_NOW" -le "$PREV_LC" ]; then
            fail "leader_checks did not advance since last probe ($PREV_LC → $LC_NOW) — forge scheduler dead"
        else
            info "leader_checks advanced $PREV_LC → $LC_NOW"
        fi
    fi
    echo "$LC_NOW" > "$PREV_LC_FILE"
else
    info "relay role — skipping forge checks"
fi

# --- 8. Snapshot worker -----------------------------------------------------
SNAP_ALIVE=$(metric dugite_snapshot_worker_alive)
SNAP_FAIL=$(metric dugite_snapshot_failed_total)
UTXO_FLUSH_FAIL=$(metric dugite_utxo_flush_failed_total)
SNAP_ALIVE_INT=$(toi "${SNAP_ALIVE:-1}")
SNAP_FAIL_INT=$(toi "${SNAP_FAIL:-0}")
UTXO_FLUSH_INT=$(toi "${UTXO_FLUSH_FAIL:-0}")
if [ "$SNAP_ALIVE_INT" -eq 1 ] && [ "$SNAP_FAIL_INT" -eq 0 ] && [ "$UTXO_FLUSH_INT" -eq 0 ]; then
    ok "snapshot worker alive; failed=0; utxo_flush_failed=0"
else
    fail "snapshot worker: alive=${SNAP_ALIVE_INT} failed=${SNAP_FAIL_INT} utxo_flush_failed=${UTXO_FLUSH_INT}"
fi

# --- 9. Network throughput (delta-based) -----------------------------------
# Net-stall semantics depend on role. A relay receives blocks from peers, so
# `blocks_received_total` should advance with the chain. A BP forges blocks, so
# its own `blocks_received_total` is 0 by design; use `blocks_forged_total`
# advance (from a per-probe baseline file) as the BP liveness signal instead.
FORGE_BASE="$BASELINE_DIR/blocks_forged-${PORT}"
FORGE_NOW=$(toi "$(metric dugite_blocks_forged_total)")
FORGE_PREV=0
[ -f "$FORGE_BASE" ] && FORGE_PREV=$(toi "$(cat "$FORGE_BASE")")
DELTA_BLOCKS_FORGE=$((FORGE_NOW - FORGE_PREV))
echo "$FORGE_NOW" > "$FORGE_BASE"

NET_OK=1
if [ "$IS_BP_INT" -eq 1 ]; then
    # BP role: liveness = forging OR receiving (relay-side traffic if multi-stream).
    if [ "$SLOT_DELTA" -ge 1 ] && [ "$DELTA_BLOCKS_FORGE" -le 0 ] && [ "$DELTA_BLOCKS_RX" -le 0 ] && [ "$HOT_INT" -ge 1 ]; then
        if [ "$PUBLIC" -eq 0 ]; then
            fail "BP forge-stall: slot advanced by ${SLOT_DELTA} but blocks_forged delta=0 AND blocks_received delta=0 with hot peer"
            NET_OK=0
        fi
    fi
else
    # Relay/follower: liveness = receiving blocks from upstream.
    if [ "$SLOT_DELTA" -ge 1 ] && [ "$DELTA_BLOCKS_RX" -le 0 ] && [ "$HOT_INT" -ge 1 ]; then
        if [ "$PUBLIC" -eq 0 ]; then
            fail "net-stall: slot advanced by ${SLOT_DELTA} but blocks_received delta=0 with hot peer"
            NET_OK=0
        elif [ "$CSY_INT" -gt 30 ]; then
            fail "net-stall: chainsync idle=${CSY_INT}s with no blocks received over ${NET_WINDOW}s"
            NET_OK=0
        fi
    fi
fi
# Apply pipeline lag: if blocks arrive but aren't applied, the ledger thread is stuck.
APPLY_LAG=$((DELTA_BLOCKS_RX - DELTA_BLOCKS_APPLY))
if [ "$APPLY_LAG" -gt 5 ]; then
    fail "apply pipeline lagging: received_delta=${DELTA_BLOCKS_RX} applied_delta=${DELTA_BLOCKS_APPLY} (lag=${APPLY_LAG})"
    NET_OK=0
fi
[ "$NET_OK" -eq 1 ] && ok "network throughput OK (blocks_rx_delta=${DELTA_BLOCKS_RX} applied_delta=${DELTA_BLOCKS_APPLY} txs_rx_delta=${DELTA_TXS_RX} over ${NET_WINDOW}s)"

# --- 10. Connection thrash --------------------------------------------------
PEERS_DELTA_ABS=$(abs "$DELTA_PEERS")
THRASH_BUDGET=$((2 * PEERS_DELTA_ABS + 1))
if [ "$DELTA_CONN_TOTAL" -gt "$THRASH_BUDGET" ]; then
    fail "connection thrash: conn_total_delta=${DELTA_CONN_TOTAL} > budget=${THRASH_BUDGET} (peers_delta=${DELTA_PEERS})"
else
    info "no conn thrash (conn_total_delta=${DELTA_CONN_TOTAL}, peers_delta=${DELTA_PEERS})"
fi

# --- 11. Haskell-tip parity (cardano-bp Prometheus) ------------------------
HASKELL_UP=0
if curl -fs --max-time 3 "localhost:${CARDANO_PORT}/metrics" > "$CARDANO_SCRAPE" 2>/dev/null; then
    HASKELL_UP=1
    H_SLOT=$(awk '$1=="cardano_node_metrics_slotNum_int"   {print $2; exit}' "$CARDANO_SCRAPE")
    H_BLOCK=$(awk '$1=="cardano_node_metrics_blockNum_int" {print $2; exit}' "$CARDANO_SCRAPE")
    H_EPOCH=$(awk '$1=="cardano_node_metrics_epoch_int"    {print $2; exit}' "$CARDANO_SCRAPE")
    H_PEERS=$(awk '$1=="cardano_node_metrics_connectedPeers_int" {print $2; exit}' "$CARDANO_SCRAPE")
    D_SLOT=$(toi "$SLOT_B")
    D_BLOCK=$(toi "$(metric dugite_block_number)")
    D_EPOCH=$(toi "$(metric dugite_epoch_number)")
    H_SLOT_I=$(toi "$H_SLOT")
    H_BLOCK_I=$(toi "$H_BLOCK")
    H_EPOCH_I=$(toi "$H_EPOCH")
    if [ -n "$H_SLOT" ] && [ -n "$H_BLOCK" ]; then
        SLOT_GAP=$(( D_SLOT > H_SLOT_I ? D_SLOT - H_SLOT_I : H_SLOT_I - D_SLOT ))
        BLOCK_GAP=$(( D_BLOCK > H_BLOCK_I ? D_BLOCK - H_BLOCK_I : H_BLOCK_I - D_BLOCK ))
        if [ "$SLOT_GAP" -le "$SLOT_PARITY" ] && [ "$BLOCK_GAP" -le 1 ] && [ "$D_EPOCH" -eq "$H_EPOCH_I" ]; then
            ok "Haskell parity: dugite(slot=$D_SLOT block=$D_BLOCK epoch=$D_EPOCH) ≈ cardano-bp(slot=$H_SLOT_I block=$H_BLOCK_I epoch=$H_EPOCH_I)"
        else
            fail "Haskell parity drift: slot_gap=$SLOT_GAP (≤$SLOT_PARITY) block_gap=$BLOCK_GAP (≤1) epoch_match=$([ "$D_EPOCH" -eq "$H_EPOCH_I" ] && echo yes || echo no)"
        fi
        H_PEERS_INT=$(toi "${H_PEERS:-0}")
        [ "$H_PEERS_INT" -ge 1 ] || fail "cardano-bp reports $H_PEERS_INT peers — not connected to dugite-relay"
    else
        info "cardano-bp :${CARDANO_PORT} returned no slot/block metrics (Haskell EKG not yet up?)"
    fi
else
    info "cardano-bp prometheus :${CARDANO_PORT} unreachable — skipping Haskell parity check"
fi

# --- 12. Recent Haskell adoption (devnet only — BP role) -------------------
# On devnet (σ=1.0, f=0.5) dugite-bp produces blocks every ~2s. cardano-bp
# should log an adoption event for each. cardano-node 11.0.1+ uses the new
# tracer namespace `ChainDB.AddBlockEvent.AddedToCurrentChain`; legacy
# `TraceAdoptedBlock` predates 10.x. Match both for forward-compat.
if [ "$IS_BP_INT" -eq 1 ] && [ "$PUBLIC" -eq 0 ] && [ -n "$CARDANO_LOG" ] && [ -f "$CARDANO_LOG" ]; then
    ADOPT_NOW=$(grep -cE 'TraceAdoptedBlock|AddedToCurrentChain' "$CARDANO_LOG" || true)
    ADOPT_BASE="$BASELINE_DIR/adoptions-cardano-bp"
    if [ -f "$ADOPT_BASE" ]; then
        PREV=$(toi "$(cat "$ADOPT_BASE")")
        DELTA=$((ADOPT_NOW - PREV))
        if [ "$DELTA" -ge 1 ]; then
            ok "cardano-bp adopted $DELTA new block(s) since last probe (total=$ADOPT_NOW)"
        else
            fail "cardano-bp adopted 0 new blocks since last probe (total stayed $ADOPT_NOW) — diffusion or forge broken"
        fi
    else
        info "cardano-bp adoptions baseline established: $ADOPT_NOW"
    fi
    echo "$ADOPT_NOW" > "$ADOPT_BASE"
fi

# --- 13. New ERROR / panic / stale / KES since last probe -------------------
ERR_TOTAL=0
LOG_TARGETS=""
[ -n "$LOG" ]       && LOG_TARGETS="$LOG_TARGETS $LOG"
[ -n "$RELAY_LOG" ] && LOG_TARGETS="$LOG_TARGETS $RELAY_LOG"
for f in $LOG_TARGETS; do
    [ -f "$f" ] || continue
    # Case-sensitive on the level keywords (tracing emits uppercase) so the
    # case-insensitive substring `error=...` inside benign WARN/INFO lines
    # doesn't match. Match leading whitespace to anchor the log level.
    ec=$(grep -cE ' ERROR | panicked| stale intersection|KES sign failure' "$f" || true)
    base_file="$BASELINE_DIR/errors-$(basename "$f")"
    prev=0
    [ -f "$base_file" ] && prev=$(toi "$(cat "$base_file")")
    new=$((ec - prev))
    if [ "$new" -gt 0 ]; then
        fail "$(basename "$f"): $new new ERROR/panic/stale/KES lines since last probe"
        if [ "$VERBOSE" -eq 1 ]; then
            grep -nE ' ERROR | panicked| stale intersection|KES sign failure' "$f" | tail -n 3 | sed 's/^/      /' >&2 || true
        fi
    else
        info "$(basename "$f"): ${ec} cumulative ERROR/panic/stale (delta=0)"
    fi
    echo "$ec" > "$base_file"
    ERR_TOTAL=$((ERR_TOTAL + new))
done
[ "$ERR_TOTAL" -eq 0 ] && ok "no new ERROR/panic/stale/KES lines in dugite logs"

# Fork-switch instability detector.
for f in $LOG_TARGETS; do
    [ -f "$f" ] || continue
    fork_now=$(grep -c 'Switched to fork' "$f" || true)
    base_file="$BASELINE_DIR/forks-$(basename "$f")"
    prev=0
    [ -f "$base_file" ] && prev=$(toi "$(cat "$base_file")")
    d=$((fork_now - prev))
    if [ "$d" -gt 2 ]; then
        fail "$(basename "$f"): $d 'Switched to fork' events since last probe — fork instability"
    elif [ "$d" -gt 0 ]; then
        info "$(basename "$f"): $d fork switch(es) since last probe (within budget)"
    fi
    echo "$fork_now" > "$base_file"
done

# --- 14. Cross-validation (cardano-bp.log) ---------------------------------
if [ -n "$CARDANO_LOG" ] && [ -f "$CARDANO_LOG" ]; then
    # cardano-node 11.0.1 uses new-tracer namespaces; match BOTH legacy and new
    # names so this check works against any node version >=10.
    IFB=$(grep -cE 'TraceForgedInvalidBlock|AddBlockValidation\.InvalidBlock|Forge\.Loop\.ForgedInvalidBlock' "$CARDANO_LOG" || true)
    if [ "$IFB" -eq 0 ]; then
        ok "cardano-bp.log: zero invalid-block events (legacy + new tracer)"
    else
        fail "CRITICAL: cardano-bp.log has $IFB invalid-block events — Haskell rejected a dugite-forged block"
    fi
    # Mismatch / fetch-timeout / disconnect patterns — early-warning indicators.
    for pat in 'mismatched' 'BlockFetchClient.*timeout' 'ConnectionLost'; do
        n_now=$(grep -cE "$pat" "$CARDANO_LOG" || true)
        slug=$(echo "$pat" | tr -c '[:alnum:]' '_')
        base_file="$BASELINE_DIR/cardano-${slug}"
        prev=0
        [ -f "$base_file" ] && prev=$(toi "$(cat "$base_file")")
        d=$((n_now - prev))
        if [ "$d" -gt 0 ]; then
            fail "cardano-bp.log: $d new '${pat}' events since last probe"
        fi
        echo "$n_now" > "$base_file"
    done
else
    info "cardano-bp.log not provided — skipping cross-validation grep"
fi

# --- Emit verdict -----------------------------------------------------------
echo
echo "dugite-node health probe — port :$PORT$([ "$HASKELL_UP" -eq 1 ] && echo " + cardano-bp :$CARDANO_PORT")"
[ -n "$LOG" ]         && echo "  dugite log:  $LOG"
[ -n "$RELAY_LOG" ]   && echo "  relay log:   $RELAY_LOG"
[ -n "$CARDANO_LOG" ] && echo "  haskell log: $CARDANO_LOG"
printf '%s\n' "${SUMMARY[@]}"

if [ ${#ANOMALIES[@]} -eq 0 ]; then
    echo "verdict: HEALTHY"
    exit 0
else
    echo "verdict: SICK (${#ANOMALIES[@]} anomaly/anomalies)"
    for a in "${ANOMALIES[@]}"; do echo "  - $a"; done
    exit 1
fi
