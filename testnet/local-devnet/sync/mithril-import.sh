#!/usr/bin/env bash
# sync/mithril-import.sh — invoke dugite-node mithril-import against the devnet
# aggregator (if running), measure throughput, and record to throughput.csv.
#
# On devnet scale we expect sub-1 minute import. On real testnets this may
# take longer; set MITHRIL_TIMEOUT_SEC to override the 60s default.
#
# If no Mithril aggregator is configured for the devnet, this script exits
# gracefully with a SKIP record (not a failure).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/../lib/common.sh"

EVIDENCE_DIR="${EVIDENCE_DIR:-$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$EVIDENCE_DIR"
THROUGHPUT_CSV="$EVIDENCE_DIR/throughput.csv"
[ -f "$THROUGHPUT_CSV" ] || echo "ts,scenario,blocks,seconds,blocks_per_sec,mb_per_sec" > "$THROUGHPUT_CSV"

MITHRIL_TIMEOUT="${MITHRIL_TIMEOUT_SEC:-60}"
MITHRIL_DB="$LD_STATE/mithril-test.db"

# Require devnet is running and relay socket is present
[ -S "$LD_RELAY_SOCK" ] || die "relay socket not present — run ./run.sh first"

# Check if mithril-import is supported by looking at the dugite-node binary
if ! "$DUGITE_BIN" mithril-import --help >/dev/null 2>&1; then
    log_warn "mithril-import subcommand not available — skipping"
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),mithril-import,0,0,0,0" >> "$THROUGHPUT_CSV"
    exit 0
fi

# The devnet config may not have a Mithril aggregator configured. Check for it.
MITHRIL_AGGREGATOR=$(jq -r '.mithrilAggregatorEndpoint // ""' \
    "$LD_CONFIG/dugite-bp.config.json" 2>/dev/null || echo "")

if [ -z "$MITHRIL_AGGREGATOR" ]; then
    log_warn "No Mithril aggregator in config — skipping mithril-import test"
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),mithril-import,0,0,0,0" >> "$THROUGHPUT_CSV"
    exit 0
fi

rm -rf "$MITHRIL_DB"
mkdir -p "$MITHRIL_DB"

log_info "Starting mithril-import (aggregator=$MITHRIL_AGGREGATOR timeout=${MITHRIL_TIMEOUT}s)..."
T_START=$(date +%s)

timeout "$MITHRIL_TIMEOUT" "$DUGITE_BIN" mithril-import \
    --network-magic  "$LD_MAGIC" \
    --database-path  "$MITHRIL_DB" \
    --aggregator-endpoint "$MITHRIL_AGGREGATOR" \
    > /tmp/mithril-import-test.log 2>&1
IMPORT_RC=$?

T_END=$(date +%s)
TOTAL_SEC=$(( T_END - T_START ))

if [ "$IMPORT_RC" -eq 124 ]; then
    log_error "mithril-import timed out after ${MITHRIL_TIMEOUT}s"
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),mithril-import,0,${TOTAL_SEC},0,0" >> "$THROUGHPUT_CSV"
    exit 1
fi

if [ "$IMPORT_RC" -ne 0 ]; then
    log_error "mithril-import failed (rc=$IMPORT_RC)"
    tail -20 /tmp/mithril-import-test.log >&2
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),mithril-import,0,${TOTAL_SEC},0,0" >> "$THROUGHPUT_CSV"
    exit 1
fi

# Measure how many blocks were imported
DB_SIZE_BYTES=$(du -sb "$MITHRIL_DB" 2>/dev/null | awk '{print $1}' || echo 0)

# Query the imported DB tip
IMPORTED_TIP=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.block // 0' || echo "?")

if [ "$TOTAL_SEC" -gt 0 ]; then
    BLOCKS_PER_SEC=$(echo "scale=2; ${IMPORTED_TIP:-0} / $TOTAL_SEC" | bc 2>/dev/null || echo "0")
    MB_PER_SEC=$(echo "scale=3; $DB_SIZE_BYTES / 1048576 / $TOTAL_SEC" | bc 2>/dev/null || echo "0")
else
    BLOCKS_PER_SEC="0"; MB_PER_SEC="0"
fi

# Verify import completed in under the SLA
if [ "$TOTAL_SEC" -gt "$MITHRIL_TIMEOUT" ]; then
    log_error "mithril-import exceeded timeout: ${TOTAL_SEC}s > ${MITHRIL_TIMEOUT}s"
    RESULT="FAIL"
else
    RESULT="PASS"
fi

echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),mithril-import,${IMPORTED_TIP:-0},${TOTAL_SEC},${BLOCKS_PER_SEC},${MB_PER_SEC}" \
    >> "$THROUGHPUT_CSV"

log_info "mithril-import $RESULT: blocks=${IMPORTED_TIP:-?} time=${TOTAL_SEC}s blocks/s=${BLOCKS_PER_SEC} MB/s=${MB_PER_SEC}"

rm -rf "$MITHRIL_DB" /tmp/mithril-import-test.log

[ "$RESULT" = "PASS" ] || exit 1
