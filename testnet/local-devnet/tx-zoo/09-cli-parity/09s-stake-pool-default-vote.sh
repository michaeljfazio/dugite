#!/usr/bin/env bash
# 09s — query stake-pool-default-vote
# Returns the default vote an SPO would cast (abstain or no-confidence).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

POOL_ID_FILE="$LD_KEYS/pool1/pool.id"
if [ ! -f "$POOL_ID_FILE" ]; then
    parity_record "stake-pool-default-vote" "SKIP" "skip" "skip" "pool1 id not found"
    exit 0
fi
POOL_ID=$(cat "$POOL_ID_FILE")
parity_query_json "stake-pool-default-vote" "stake-pool-default-vote" \
    "--stake-pool-id" "$POOL_ID"
