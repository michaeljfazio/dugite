#!/usr/bin/env bash
# 09g — query stake-snapshot
# Returns mark/set/go stake snapshots for a pool.  Epoch-stable.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

POOL_ID_FILE="$LD_KEYS/pool1/pool.id"
if [ ! -f "$POOL_ID_FILE" ]; then
    parity_record "stake-snapshot" "SKIP" "skip" "skip" "env-skip: pool1 id not found (run setup.sh first)"
    exit 0
fi

POOL_ID=$(cat "$POOL_ID_FILE")
parity_query_json "stake-snapshot" "stake-snapshot" "--stake-pool-id" "$POOL_ID"

# #963: see 09f. `stake-snapshot` leaked a *different* extra pool than
# `pool-state` did, because the two handlers fall back to different "all pools"
# collections — which is itself what pinned the diagnosis on the shared
# argument parser rather than on either handler.
if [ -f "$LD_KEYS/pool1/pool.id.hex" ]; then
    parity_assert_pool_filter "stake-snapshot-filter" "stake-snapshot" \
        "$(cat "$LD_KEYS/pool1/pool.id.hex")" "$POOL_ID" || true
fi
