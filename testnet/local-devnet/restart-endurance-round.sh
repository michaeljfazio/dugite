#!/usr/bin/env bash
# restart-endurance-round.sh — #1044 (#1031 P2-3).
#
# 30 SIGTERM/restart iterations of dugite-relay with per-iteration recovery
# assertions, adapted from upstream cardano-node-tests
# `test_reconnect.py::test_metrics_reconnect` (200 iterations upstream; adopted
# at 30 — at devnet scale the metric assertions saturate long before 200, which
# adds wall-time, not signal).
#
# WHY THIS ROUND EXISTS
#
# The gate has exactly ONE restart cycle (Round 3) plus the kill-9 chaos
# scenario. Leak-class defects need REPETITION to surface: fd/task leaks per
# reconnect (#924 — dropping a tokio JoinHandle DETACHES, leaking the socket for
# the process lifetime), peer-registry growth, and mux re-arm regressions
# (#980's class — a clean mini-protocol exit must be RESTARTED by the inbound
# governor, and dugite orphaned the responder task instead). One restart cannot
# distinguish "recovered" from "recovered while leaking a socket each time".
#
# The subject under test is therefore dugite-bp — the PEER of the node being
# restarted. A monotonic fd/thread climb on the peer across 30 reconnects is
# exactly the signal, and it is invisible in a single cycle.
#
# SIGTERM, NEVER SIGKILL: kill -9 corrupts the ImmutableDB. The chaos suite owns
# the kill-9 scenario deliberately and separately; this round is about clean
# restarts.
#
# Usage:
#   ./restart-endurance-round.sh                  # 30 iterations
#   RE_ITERATIONS=3 ./restart-endurance-round.sh  # short shakedown
#   RE_RED_CASE=down    ./restart-endurance-round.sh   # RED proof 1
#   RE_RED_CASE=peers   ./restart-endurance-round.sh   # RED proof 2
#
# RED-CASE PROOFS (runnable, not claimed — #916/#923/#945/#953/#959 were all
# checks that reported success while measuring nothing):
#   RE_RED_CASE=down  — assert recovery WITHOUT restarting the relay in
#                       iteration 1. The round MUST fail. Proves the recovery
#                       assertions actually observe a live node.
#   RE_RED_CASE=peers — require peers_connected > 999. The round MUST fail.
#                       Proves the metric is READ from the endpoint and not
#                       defaulted/ignored (the #987 shape: a verdict column that
#                       was always 0).
set +e
unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true

cd "$(dirname "${BASH_SOURCE[0]}")" || exit 2
. ./lib/common.sh
. ./lib/expect-log-errors.sh

ITERATIONS="${RE_ITERATIONS:-30}"
CATCHUP_TIMEOUT="${RE_CATCHUP_TIMEOUT:-120}"
STOP_TIMEOUT="${RE_STOP_TIMEOUT:-60}"
RED_CASE="${RE_RED_CASE:-}"

FAILURES=0
step() { echo; echo "########## $* ##########"; date -u +%H:%M:%SZ; }
ok()   { printf '\033[0;32m[PASS]\033[0m %s\n' "$*"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
note() { printf '\033[0;36m[NOTE]\033[0m %s\n' "$*"; }

DUGITE_BIN="${DUGITE_BIN:-$(cd ../.. && pwd)/target/release/dugite-node}"
[ -x "$DUGITE_BIN" ] || { echo "REFUSING TO RUN: dugite-node binary not found at $DUGITE_BIN"; exit 2; }

# The devnet must already be up: this round restarts a node, it does not create
# a devnet. Running it against a dead devnet would make every assertion below
# vacuous in the same direction (all "recovered" checks fail), which is noise,
# not signal.
[ -S "$LD_RELAY_SOCK" ] || {
    echo "REFUSING TO RUN: $LD_RELAY_SOCK absent — start the devnet first (setup.sh && run.sh)"
    exit 2
}

EVIDENCE_DIR="$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$EVIDENCE_DIR"
CSV="$EVIDENCE_DIR/restart-endurance.csv"
echo "ts,iteration,outcome,tip_slot,peers_connected,bp_fds,bp_threads,bp_rss_kb,detail" > "$CSV"

RELAY_METRICS="http://127.0.0.1:${LD_DUGITE_RELAY_METRICS_PORT}/metrics"

# ── helpers ────────────────────────────────────────────────────────────────

# Read one Prometheus gauge from the relay's metrics endpoint. Prints the value,
# or nothing (and returns 1) when the endpoint or the metric is unavailable —
# NEVER a default, so a missing metric fails the assertion instead of silently
# satisfying it.
relay_metric() {
    local name="$1" out
    out=$(curl -fsS --max-time 5 "$RELAY_METRICS" 2>/dev/null) || return 1
    printf '%s' "$out" \
        | awk -v n="$name" '$1 == n { print $2; found=1 } END { exit !found }'
}

relay_tip_slot() {
    cardano-cli query tip --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.slot // empty'
}

bp_pid() { cat "$LD_STATE/dugite-bp.pid" 2>/dev/null; }

# fd / thread / rss for dugite-bp (the PEER — see the header).
bp_resources() {
    local pid fds threads rss
    pid="$(bp_pid)"
    if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
        echo "0 0 0"
        return 1
    fi
    case "$(uname -s)" in
        Darwin)
            fds=$(lsof -p "$pid" 2>/dev/null | wc -l | tr -d ' ')
            threads=$(ps -M "$pid" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
            ;;
        *)
            fds=$(ls "/proc/$pid/fd" 2>/dev/null | wc -l | tr -d ' ')
            threads=$(awk '/Threads:/{print $2}' "/proc/$pid/status" 2>/dev/null)
            ;;
    esac
    rss=$(ps -p "$pid" -o rss= 2>/dev/null | tr -d ' ')
    echo "${fds:-0} ${threads:-0} ${rss:-0}"
}

stop_relay() {
    local pid deadline
    pid="$(cat "$LD_STATE/dugite-relay.pid" 2>/dev/null)"
    if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
        # Resolve by command line rather than trusting a stale pidfile (#944's
        # lesson: a path/pid that does not exist makes the criterion never
        # evaluate, and the round reports the same result either way).
        pid=$(pgrep -f "dugite-node run .*dugite-relay.config.json" 2>/dev/null | head -1)
    fi
    [ -n "$pid" ] || return 1
    kill -TERM "$pid" 2>/dev/null || true
    deadline=$(( $(date +%s) + STOP_TIMEOUT ))
    while kill -0 "$pid" 2>/dev/null; do
        [ "$(date +%s)" -ge "$deadline" ] && return 1
        sleep 1
    done
    # `kill` returning 0 does NOT prove death, and a sandboxed kill can silently
    # no-op — so exit is confirmed by polling, above, not by kill's status.
    return 0
}

start_relay() {
    caffeinate_if_macos "$DUGITE_BIN" run \
        --config        "$LD_CONFIG/dugite-relay.config.json" \
        --topology      "$LD_CONFIG/dugite-relay.topology.json" \
        --database-path "$LD_STATE/dugite-relay.db" \
        --socket-path   "$LD_RELAY_SOCK" \
        --host-addr     127.0.0.1 \
        --port          "$LD_RELAY_PORT" \
        --metrics-port  "$LD_DUGITE_RELAY_METRICS_PORT" \
        --rpc-host      127.0.0.1 \
        --rpc-port      "$LD_DUGITE_RELAY_RPC_PORT" \
        >> "$LD_LOGS/dugite-relay.log" 2>&1 &
    write_node_pidfile "$LD_STATE/dugite-relay.db" "$LD_STATE/dugite-relay.pid" \
        || echo $! > "$LD_STATE/dugite-relay.pid"
}

# Wait until the relay serves a tip AND that tip is advancing. A socket that
# accepts a connection is not the same as a node that has rejoined the chain.
wait_relay_recovered() {
    local deadline first last
    deadline=$(( $(date +%s) + CATCHUP_TIMEOUT ))
    first=""
    while [ "$(date +%s)" -lt "$deadline" ]; do
        last="$(relay_tip_slot)"
        if [ -n "$last" ]; then
            if [ -z "$first" ]; then
                first="$last"
            elif [ "$last" -gt "$first" ]; then
                echo "$last"
                return 0
            fi
        fi
        sleep 2
    done
    [ -n "$last" ] && echo "$last"
    return 1
}

# ── round ──────────────────────────────────────────────────────────────────

step "restart-endurance: $ITERATIONS iterations of dugite-relay SIGTERM/restart"
[ -n "$RED_CASE" ] && note "RED-CASE MODE '$RED_CASE' — this run is EXPECTED to fail"

MARK_BP=$(log_mark "$LD_LOGS/dugite-bp.log")
MARK_RELAY=$(log_mark "$LD_LOGS/dugite-relay.log")
note "log marks: dugite-bp=$MARK_BP dugite-relay=$MARK_RELAY"

read -r FDS0 THREADS0 RSS0 <<< "$(bp_resources)"
note "dugite-bp baseline: fds=$FDS0 threads=$THREADS0 rss_kb=$RSS0"
if [ "${FDS0:-0}" -eq 0 ]; then
    bad "could not sample dugite-bp resources at baseline — the leak assertions would be vacuous"
fi

RECOVERED=0
for i in $(seq 1 "$ITERATIONS"); do
    if [ "$RED_CASE" = "down" ] && [ "$i" -eq 1 ]; then
        # RED proof 1: stop the relay and DO NOT restart it, then run the same
        # recovery assertions. They must fail.
        note "[iter $i] RED-CASE 'down': stopping relay and skipping the restart"
        stop_relay
    else
        if ! stop_relay; then
            bad "[iter $i] dugite-relay did not exit within ${STOP_TIMEOUT}s of SIGTERM"
            printf '%s,%s,FAIL,,,,,,%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$i" "stop-timeout" >> "$CSV"
            continue
        fi
        start_relay
    fi

    TIP=$(wait_relay_recovered)
    RC=$?

    PEERS=$(relay_metric dugite_peers_connected) || PEERS=""
    read -r FDS THREADS RSS <<< "$(bp_resources)"

    PEER_FLOOR=1
    [ "$RED_CASE" = "peers" ] && PEER_FLOOR=999

    DETAIL=""
    ITER_OK=1
    if [ "$RC" -ne 0 ] || [ -z "$TIP" ]; then
        ITER_OK=0
        DETAIL="tip-not-advancing"
    fi
    if [ -z "$PEERS" ]; then
        ITER_OK=0
        DETAIL="$DETAIL;peers-metric-unavailable"
    elif [ "$PEERS" -lt "$PEER_FLOOR" ]; then
        ITER_OK=0
        DETAIL="$DETAIL;peers=$PEERS<$PEER_FLOOR"
    fi

    if [ "$ITER_OK" -eq 1 ]; then
        RECOVERED=$((RECOVERED + 1))
        printf '%s,%s,PASS,%s,%s,%s,%s,%s,%s\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$i" "$TIP" "$PEERS" "$FDS" "$THREADS" "$RSS" "ok" >> "$CSV"
        note "[iter $i/$ITERATIONS] recovered: tip=$TIP peers=$PEERS bp_fds=$FDS bp_threads=$THREADS"
    else
        printf '%s,%s,FAIL,%s,%s,%s,%s,%s,%s\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$i" "${TIP:-}" "${PEERS:-}" "$FDS" "$THREADS" "$RSS" "${DETAIL#;}" >> "$CSV"
        bad "[iter $i/$ITERATIONS] did not recover: ${DETAIL#;}"
    fi
done

# ── end-of-round assertions ────────────────────────────────────────────────

step "end-of-round assertions"

if [ "$RECOVERED" -eq "$ITERATIONS" ]; then
    ok "all $ITERATIONS iterations recovered"
else
    bad "only $RECOVERED/$ITERATIONS iterations recovered"
fi

read -r FDS1 THREADS1 RSS1 <<< "$(bp_resources)"
note "dugite-bp final: fds=$FDS1 threads=$THREADS1 rss_kb=$RSS1 (baseline fds=$FDS0 threads=$THREADS0)"

# Leak assertion. A per-reconnect fd/thread leak shows up as growth roughly
# proportional to ITERATIONS, so the bound is generous in absolute terms while
# still catching one-per-restart: a leak of 1 fd per iteration over 30 iterations
# is +30, well past this.
FD_BUDGET="${RE_FD_BUDGET:-$(( ITERATIONS / 3 + 10 ))}"
THREAD_BUDGET="${RE_THREAD_BUDGET:-$(( ITERATIONS / 3 + 10 ))}"
if [ "${FDS0:-0}" -gt 0 ] && [ "${FDS1:-0}" -gt 0 ]; then
    FD_GROWTH=$(( FDS1 - FDS0 ))
    THREAD_GROWTH=$(( THREADS1 - THREADS0 ))
    if [ "$FD_GROWTH" -le "$FD_BUDGET" ]; then
        ok "dugite-bp fd count flat across $ITERATIONS reconnects (growth=$FD_GROWTH, budget=$FD_BUDGET)"
    else
        bad "dugite-bp fd count GREW by $FD_GROWTH across $ITERATIONS reconnects (budget=$FD_BUDGET) — per-reconnect fd leak (#924 class)"
    fi
    if [ "$THREAD_GROWTH" -le "$THREAD_BUDGET" ]; then
        ok "dugite-bp thread count flat (growth=$THREAD_GROWTH, budget=$THREAD_BUDGET)"
    else
        bad "dugite-bp thread count GREW by $THREAD_GROWTH (budget=$THREAD_BUDGET) — per-reconnect task leak (#980 class)"
    fi
else
    bad "could not sample dugite-bp resources — leak assertions are INCONCLUSIVE, not passing"
fi

# Log oracle: expected reconnect churn is allowed, anything else is not.
ALLOWLIST="$(pwd)/restart-endurance-round.allowed-errors"
if [ -f "$ALLOWLIST" ]; then
    if assert_no_other_errors "$LD_LOGS/dugite-bp.log" "$MARK_BP" "$ALLOWLIST"; then
        ok "no unexpected error-class lines in dugite-bp.log"
    else
        bad "unexpected error-class line(s) in dugite-bp.log — see above"
    fi
    if assert_no_other_errors "$LD_LOGS/dugite-relay.log" "$MARK_RELAY" "$ALLOWLIST"; then
        ok "no unexpected error-class lines in dugite-relay.log"
    else
        bad "unexpected error-class line(s) in dugite-relay.log — see above"
    fi
else
    bad "missing restart-endurance-round.allowed-errors next to this script"
fi

echo
echo "evidence: $CSV"
if [ "$FAILURES" -eq 0 ]; then
    echo "RESTART-ENDURANCE ROUND: PASS ($RECOVERED/$ITERATIONS iterations)"
    [ -n "$RED_CASE" ] && {
        echo "RED-CASE '$RED_CASE' PASSED — that is itself a FAILURE: the assertions did not detect the injected fault"
        exit 1
    }
    exit 0
else
    echo "RESTART-ENDURANCE ROUND: FAIL ($FAILURES assertion(s))"
    [ -n "$RED_CASE" ] && {
        echo "RED-CASE '$RED_CASE' correctly detected — the assertions are discriminating"
        exit 0
    }
    exit 1
fi
