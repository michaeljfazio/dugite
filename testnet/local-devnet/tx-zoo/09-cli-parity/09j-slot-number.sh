#!/usr/bin/env bash
# 09j — query slot-number
# Returns the slot number of a given UTxO time.  We query for an absolute
# UTC timestamp and compare — both should return the same slot since they
# share genesis.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

# Use a fixed past timestamp so the answer is deterministic
# (2020-07-29T21:44:51Z is the Shelley start on mainnet — arbitrary
# but stable; on our devnet genesis starts from SystemStart in genesis)
FIXED_TIME=$(cat "$LD_GENESIS/shelley-genesis.json" \
    | jq -r '.systemStart' 2>/dev/null || echo "2020-01-01T00:00:00Z")

# Advance by 1000 slots worth of time
SLOT_LEN=$(cat "$LD_GENESIS/shelley-genesis.json" | jq -r '.slotLength' 2>/dev/null || echo 1)
QUERY_TIME=$(date -u -d "@$(($(date -u -d "$FIXED_TIME" +%s) + 1000 * ${SLOT_LEN%.*}))" \
    +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null \
    || python3 -c "
import datetime, sys
t = datetime.datetime.fromisoformat('${FIXED_TIME%Z}').replace(tzinfo=datetime.timezone.utc)
t += datetime.timedelta(seconds=1000 * ${SLOT_LEN%.*})
print(t.strftime('%Y-%m-%dT%H:%M:%SZ'))")

parity_query_json "slot-number" "slot-number" "--utc-time" "$QUERY_TIME"
