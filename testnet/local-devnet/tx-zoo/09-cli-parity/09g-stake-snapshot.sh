#!/usr/bin/env bash
# 09g — query stake-snapshot
# Returns mark/set/go stake snapshots for a pool.  Epoch-stable.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

POOL_ID_FILE="$LD_KEYS/pool1/pool.id"
if [ ! -f "$POOL_ID_FILE" ]; then
    parity_record "stake-snapshot" "SKIP" "skip" "skip" "pool1 id not found (run setup.sh first)"
    exit 0
fi

POOL_ID=$(cat "$POOL_ID_FILE")
parity_query_json "stake-snapshot" "stake-snapshot" "--stake-pool-id" "$POOL_ID"
