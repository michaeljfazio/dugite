#!/usr/bin/env bash
# sync/from-genesis.sh — boot dugite-bp with an empty database and measure
# the time + throughput required to sync from genesis to the relay's current tip.
#
# Prerequisites:
#   - devnet is already running (run.sh has been called)
#   - cardano-bp is at the relay's tip (i.e. devnet has had time to produce blocks)
#
# Output: evidence/<ts>/throughput.csv with a from-genesis row.
# Acceptance: completes in < FROM_GENESIS_TIMEOUT_SEC (default 600s)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/../lib/common.sh"

EVIDENCE_DIR="${EVIDENCE_DIR:-$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$EVIDENCE_DIR"
THROUGHPUT_CSV="$EVIDENCE_DIR/throughput.csv"
[ -f "$THROUGHPUT_CSV" ] || echo "ts,scenario,blocks,seconds,blocks_per_sec,mb_per_sec" > "$THROUGHPUT_CSV"

FROM_GENESIS_TIMEOUT="${FROM_GENESIS_TIMEOUT_SEC:-600}"
SYNC_SOCKET="/tmp/ld-$(id -u)/fg-test.sock"
SYNC_DB="$LD_STATE/fg-test.db"

# Require devnet is running
[ -S "$LD_RELAY_SOCK" ] || die "relay socket not present — run ./run.sh first"

TARGET_TIP=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.block // 0')
TARGET_SLOT=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.slot // 0')

log_info "Target tip: block=$TARGET_TIP slot=$TARGET_SLOT"
if [ "$TARGET_TIP" -lt 5 ]; then
    log_warn "Relay has fewer than 5 blocks — wait for devnet to produce more blocks"
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),from-genesis,0,0,0,0" >> "$THROUGHPUT_CSV"
    exit 0
fi

# Wipe any stale from-genesis test DB
rm -rf "$SYNC_DB"
mkdir -p "$SYNC_DB"

# Build a minimal relay topology pointing at the running dugite-relay
cat > /tmp/fg-test-topology.json <<EOF
{
  "bootstrapPeers": [{"address": "127.0.0.1", "port": $LD_RELAY_PORT}],
  "localRoots": [],
  "publicRoots": [],
  "useLedgerAfterSlot": 99999999
}
EOF

# Use the same config as dugite-bp but with no forging keys
# Pass --consensus-mode genesis to allow startup without VRF/KES/OpCert
T_START=$(date +%s)
log_info "Starting from-genesis sync (timeout=${FROM_GENESIS_TIMEOUT}s)..."

"$DUGITE_BIN" run \
    --config        "$LD_CONFIG/dugite-bp.config.json" \
    --topology      /tmp/fg-test-topology.json \
    --database-path "$SYNC_DB" \
    --socket-path   "$SYNC_SOCKET" \
    --host-addr     127.0.0.1 \
    --port          3099 \
    --consensus-mode genesis \
    > /tmp/fg-test.log 2>&1 &
FG_PID=$!
trap 'kill "$FG_PID" 2>/dev/null; rm -rf "$SYNC_DB" /tmp/fg-test.log /tmp/fg-test-topology.json' EXIT

log_info "from-genesis node PID=$FG_PID"

ELAPSED=0
POLL=5
CURRENT_BLOCK=0

while [ "$ELAPSED" -lt "$FROM_GENESIS_TIMEOUT" ]; do
    sleep "$POLL"
    ELAPSED=$(( ELAPSED + POLL ))

    if ! kill -0 "$FG_PID" 2>/dev/null; then
        log_error "from-genesis node exited prematurely"
        tail -20 /tmp/fg-test.log >&2
        break
    fi

    CURRENT_BLOCK=$(cardano-cli query tip \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$SYNC_SOCKET" 2>/dev/null | jq -r '.block // 0' || echo 0)
    log_info "  elapsed=${ELAPSED}s synced_blocks=${CURRENT_BLOCK}/${TARGET_TIP}"

    if [ "$CURRENT_BLOCK" -ge "$TARGET_TIP" ]; then
        log_info "Sync complete!"
        break
    fi
done

T_END=$(date +%s)
TOTAL_SEC=$(( T_END - T_START ))

kill "$FG_PID" 2>/dev/null || true

FINAL_BLOCK=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$SYNC_SOCKET" 2>/dev/null | jq -r '.block // 0' || echo "$CURRENT_BLOCK")

if [ "$FINAL_BLOCK" -ge "$TARGET_TIP" ]; then
    RESULT="PASS"
else
    RESULT="FAIL"
    log_warn "from-genesis sync did not reach target: $FINAL_BLOCK / $TARGET_TIP in ${TOTAL_SEC}s"
fi

# Compute throughput
if [ "$TOTAL_SEC" -gt 0 ]; then
    BLOCKS_PER_SEC=$(echo "scale=2; $FINAL_BLOCK / $TOTAL_SEC" | bc 2>/dev/null || echo "0")
else
    BLOCKS_PER_SEC="0"
fi

# Estimate MB/sec: devnet blocks ~1KB average
DB_SIZE_BYTES=$(du -sb "$SYNC_DB" 2>/dev/null | awk '{print $1}' || echo 0)
if [ "$TOTAL_SEC" -gt 0 ]; then
    MB_PER_SEC=$(echo "scale=3; $DB_SIZE_BYTES / 1048576 / $TOTAL_SEC" | bc 2>/dev/null || echo "0")
else
    MB_PER_SEC="0"
fi

echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),from-genesis,${FINAL_BLOCK},${TOTAL_SEC},${BLOCKS_PER_SEC},${MB_PER_SEC}" \
    >> "$THROUGHPUT_CSV"

log_info "from-genesis result: $RESULT blocks=$FINAL_BLOCK/$TARGET_TIP time=${TOTAL_SEC}s blocks/s=${BLOCKS_PER_SEC}"

[ "$RESULT" = "PASS" ] || exit 1
