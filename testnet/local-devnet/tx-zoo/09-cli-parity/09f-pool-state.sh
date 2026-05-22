#!/usr/bin/env bash
# 09f — query pool-state
# Queries the registration parameters of pool1 (dugite-bp's pool).
# Epoch-stable; should match exactly.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

POOL_ID_FILE="$LD_KEYS/pool1/pool.id"
if [ ! -f "$POOL_ID_FILE" ]; then
    parity_record "pool-state" "SKIP" "skip" "skip" "pool1 id not found (run setup.sh first)"
    exit 0
fi

POOL_ID=$(cat "$POOL_ID_FILE")
parity_query_json "pool-state" "pool-state" "--stake-pool-id" "$POOL_ID"
