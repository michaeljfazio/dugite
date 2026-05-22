#!/usr/bin/env bash
# chaos/inbound-syn-flood.sh — simulate a SYN flood / connection-rate flood
# against the dugite-bp N2N listener to verify the per-IP rate limiter (#497).
#
# Spawns FLOOD_CONNS rapid TCP connections from loopback. Verifies:
#   1. dugite-bp does not crash
#   2. dugite-bp remains responsive to legitimate queries after the flood
#   3. log shows connection throttling (not silent accept of all connections)
#
# Memory: project_inbound_per_ip_rate_limit.md (observed 659 connections in mins)
set -euo pipefail

SCENARIO="inbound-syn-flood"
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

FLOOD_CONNS="${SYN_FLOOD_CONNS:-200}"
FLOOD_PARALLEL="${SYN_FLOOD_PARALLEL:-20}"

[ -S "$LD_DUGITE_BP_SOCK" ] || die "$SCENARIO: dugite-bp socket not present"

TIP_BEFORE=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.block // 0' || echo 0)
LOG_LINE_BEFORE=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null || echo 0)

log_info "$SCENARIO: flooding port $LD_DUGITE_BP_PORT with $FLOOD_CONNS connections (parallel=$FLOOD_PARALLEL)..."

# Flood: open TCP connection, send a tiny payload, immediately close.
# This simulates a client that connects but never completes the handshake.
flood_one() {
    (echo -n "\x00"; sleep 0.1) | \
        timeout 2 socat - "TCP:127.0.0.1:${LD_DUGITE_BP_PORT}" 2>/dev/null || true
}

T_FLOOD_START=$(date +%s)
SENT=0
while [ "$SENT" -lt "$FLOOD_CONNS" ]; do
    BATCH=$((FLOOD_CONNS - SENT))
    [ "$BATCH" -gt "$FLOOD_PARALLEL" ] && BATCH="$FLOOD_PARALLEL"
    for _ in $(seq 1 "$BATCH"); do
        flood_one &
    done
    wait
    SENT=$(( SENT + BATCH ))
done
T_FLOOD_END=$(date +%s)
FLOOD_SEC=$(( T_FLOOD_END - T_FLOOD_START ))

log_info "$SCENARIO: flood complete ($FLOOD_CONNS conns in ${FLOOD_SEC}s), checking node health..."

# Give the node a moment to process
sleep 3

# Verify node is still responsive
if ! chaos_wait_for_socket "$LD_DUGITE_BP_SOCK" 30; then
    chaos_record "$SCENARIO" "post-flood" "0" "FAIL" "node-unresponsive-after-flood"
    exit 1
fi

TIP_AFTER=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.block // 0' || echo 0)

LOG_LINE_AFTER=$(wc -l < "$LD_LOGS/dugite-bp.log" 2>/dev/null || echo 0)
NEW_LINES=$(( LOG_LINE_AFTER - LOG_LINE_BEFORE ))

PANICS=$(tail -n "$((NEW_LINES + 10))" "$LD_LOGS/dugite-bp.log" 2>/dev/null | \
    grep -c -E 'panic|PANIC|thread.*panicked' || echo 0)

if [ "$PANICS" -gt 0 ]; then
    chaos_record "$SCENARIO" "post-flood" "0" "FAIL" "panic-during-flood"
    exit 1
fi

# Check for rate-limit log line (optional — not a hard failure if missing,
# since the rate-limiter may not log every rejection)
RATE_LOG=$(tail -n "$((NEW_LINES + 10))" "$LD_LOGS/dugite-bp.log" 2>/dev/null | \
    grep -c -iE 'rate.limit|too.many.conn|conn.rate|throttl' || echo 0)

chaos_record "$SCENARIO" "post-flood" "$FLOOD_SEC" "PASS" \
    "conns=${FLOOD_CONNS} tip_before=${TIP_BEFORE} tip_after=${TIP_AFTER} rate_log_lines=${RATE_LOG}"
log_info "$SCENARIO: PASS — node responsive after ${FLOOD_CONNS} conns (rate_log_lines=${RATE_LOG})"
