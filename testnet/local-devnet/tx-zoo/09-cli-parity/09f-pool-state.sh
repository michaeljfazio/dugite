#!/usr/bin/env bash
# 09f — query pool-state
# Queries the registration parameters of pool1 (dugite-bp's pool).
# Epoch-stable; should match exactly.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

POOL_ID_FILE="$LD_KEYS/pool1/pool.id"
if [ ! -f "$POOL_ID_FILE" ]; then
    parity_record "pool-state" "SKIP" "skip" "skip" "env-skip: pool1 id not found (run setup.sh first)"
    exit 0
fi

POOL_ID=$(cat "$POOL_ID_FILE")
parity_query_json "pool-state" "pool-state" "--stake-pool-id" "$POOL_ID"

# #963: the pool-id filter was inert — both sockets were asked about pool1 and
# dugite answered with pool1 AND pool2. The diff above catches that, but only
# comparatively; this asserts the property itself on each socket.
if [ -f "$LD_KEYS/pool1/pool.id.hex" ]; then
    parity_assert_pool_filter "pool-state-filter" "pool-state" \
        "$(cat "$LD_KEYS/pool1/pool.id.hex")" "$POOL_ID" || true
fi
