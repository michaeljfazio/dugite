#!/usr/bin/env bash
# sync/bulk-sync-throughput.sh — measure ChainSync bulk-sync throughput while
# the devnet is running. Samples the relay's tip every SAMPLE_INTERVAL_SEC,
# then computes blocks/sec, headers/sec, and estimated MB/sec over the
# measurement window. Emits results to evidence/<ts>/throughput.csv.
#
# Intended to be called either standalone or as part of verify.sh/devnet-validate.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/../lib/common.sh"

EVIDENCE_DIR="${EVIDENCE_DIR:-$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$EVIDENCE_DIR"
THROUGHPUT_CSV="$EVIDENCE_DIR/throughput.csv"
[ -f "$THROUGHPUT_CSV" ] || echo "ts,scenario,blocks,seconds,blocks_per_sec,mb_per_sec" > "$THROUGHPUT_CSV"

# Duration over which to measure; default 30s (long enough to cover a few blocks
# at 1-slot epochs, short enough for smoke runs)
MEASURE_SEC="${BULK_SYNC_MEASURE_SEC:-30}"
SAMPLE_INTERVAL=5

[ -S "$LD_RELAY_SOCK" ] || die "relay socket not present — run ./run.sh first"
[ -S "$LD_DUGITE_BP_SOCK" ] || die "dugite-bp socket not present — run ./run.sh first"

log_info "Measuring bulk-sync throughput over ${MEASURE_SEC}s..."

# Sample dugite-bp tip at start
T_START=$(date +%s)
TIP_START=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.block // 0' || echo 0)
SLOT_START=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.slot // 0' || echo 0)
DB_START=$(du -sb "$LD_STATE/dugite-bp.db" 2>/dev/null | awk '{print $1}' || echo 0)

ELAPSED=0
while [ "$ELAPSED" -lt "$MEASURE_SEC" ]; do
    sleep "$SAMPLE_INTERVAL"
    ELAPSED=$(( ELAPSED + SAMPLE_INTERVAL ))
    CUR=$(cardano-cli query tip \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.block // 0' || echo 0)
    log_info "  elapsed=${ELAPSED}s blocks=${CUR}"
done

T_END=$(date +%s)
TIP_END=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.block // 0' || echo "$TIP_START")
SLOT_END=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.slot // 0' || echo "$SLOT_START")
DB_END=$(du -sb "$LD_STATE/dugite-bp.db" 2>/dev/null | awk '{print $1}' || echo "$DB_START")

TOTAL_SEC=$(( T_END - T_START ))
DELTA_BLOCKS=$(( TIP_END - TIP_START ))
DELTA_BYTES=$(( DB_END - DB_START ))

if [ "$TOTAL_SEC" -gt 0 ] && [ "$DELTA_BLOCKS" -gt 0 ]; then
    BLOCKS_PER_SEC=$(echo "scale=3; $DELTA_BLOCKS / $TOTAL_SEC" | bc 2>/dev/null || echo "0")
    MB_PER_SEC=$(echo "scale=3; $DELTA_BYTES / 1048576 / $TOTAL_SEC" | bc 2>/dev/null || echo "0")
    HEADERS_PER_SEC=$(echo "scale=3; $DELTA_BLOCKS / $TOTAL_SEC" | bc 2>/dev/null || echo "0")
else
    BLOCKS_PER_SEC="0"; MB_PER_SEC="0"; HEADERS_PER_SEC="0"
fi

echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),bulk-sync,${DELTA_BLOCKS},${TOTAL_SEC},${BLOCKS_PER_SEC},${MB_PER_SEC}" \
    >> "$THROUGHPUT_CSV"

log_info "Bulk-sync throughput: blocks/s=${BLOCKS_PER_SEC} MB/s=${MB_PER_SEC} headers/s=${HEADERS_PER_SEC} delta_blocks=${DELTA_BLOCKS} over ${TOTAL_SEC}s"

# Cross-validate with relay tip to confirm no stall
RELAY_TIP=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.block // 0' || echo 0)
GAP=$(( RELAY_TIP - TIP_END ))
log_info "Relay tip=$RELAY_TIP dugite-bp tip=$TIP_END gap=${GAP} blocks"

if [ "$GAP" -gt 20 ]; then
    log_warn "dugite-bp is >20 blocks behind relay — possible sync stall"
    exit 1
fi

exit 0
