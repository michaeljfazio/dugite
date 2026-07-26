#!/usr/bin/env bash
# 09j — query slot-number
# Returns the slot number of a given UTxO time.  We query for an absolute
# UTC timestamp and compare — both should return the same slot since they
# share genesis.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

# Anchor on the devnet's own SystemStart so the expected answer is fixed.
FIXED_TIME=$(jq -r '.systemStart' "$LD_GENESIS/shelley-genesis.json" 2>/dev/null \
    || echo "2020-01-01T00:00:00Z")
SLOT_LEN=$(jq -r '.slotLength' "$LD_GENESIS/shelley-genesis.json" 2>/dev/null || echo 1)

# Offset must land in the PAST relative to the chain tip. cardano-node will
# only convert a timestamp inside its forecast horizon; asking for one beyond
# the tip fails with "Command failed: query slot-number" on the Haskell side
# while dugite answers happily, which reads as a parity divergence but is
# purely a function of when the suite happened to run.
#
# This used to offset by 1000 slots — 1000s after genesis on a devnet whose
# slotLength is 1s — so the query only succeeded if the suite ran more than
# ~16 minutes into the round. Round 1 reaches here in about 6.
#
# 100 slots is comfortably behind the tip by the time 09-cli-parity runs
# (setup + 30s settle + the full tx-zoo) and is still a fixed, genesis-derived
# instant, so the expected slot number is deterministic.
SLOT_OFFSET=100
QUERY_TIME=$(python3 -c "
import datetime
t = datetime.datetime.fromisoformat('${FIXED_TIME%Z}').replace(tzinfo=datetime.timezone.utc)
t += datetime.timedelta(seconds=${SLOT_OFFSET} * ${SLOT_LEN%.*})
print(t.strftime('%Y-%m-%dT%H:%M:%SZ'))")

# cardano-cli 11.0.0.0: `query slot-number` takes the timestamp POSITIONALLY
# (usage: ... --socket-path SOCKET_PATH [--volatile-tip|--immutable-tip] TIMESTAMP)
# and has no output-format flag, so --output-json must not be sent either.
PARITY_OUTPUT_JSON=0 parity_query_json "slot-number" "slot-number" "$QUERY_TIME"
