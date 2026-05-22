#!/usr/bin/env bash
# 09t — query leadership-schedule
# Returns the leader schedule for the CURRENT epoch for pool1.
# Both nodes share the same genesis + VRF keys, so the schedule should match.
# This query requires the VRF signing key — skip gracefully if not available.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

VRF_SKEY="$LD_KEYS/pool1/vrf.skey"
POOL_ID_FILE="$LD_KEYS/pool1/pool.id"

if [ ! -f "$VRF_SKEY" ] || [ ! -f "$POOL_ID_FILE" ]; then
    parity_record "leadership-schedule" "SKIP" "skip" "skip" "VRF key or pool id not found"
    exit 0
fi

POOL_ID=$(cat "$POOL_ID_FILE")
parity_query_json "leadership-schedule" "leadership-schedule" \
    "--stake-pool-id" "$POOL_ID" \
    "--vrf-signing-key-file" "$VRF_SKEY" \
    "--current-epoch"
