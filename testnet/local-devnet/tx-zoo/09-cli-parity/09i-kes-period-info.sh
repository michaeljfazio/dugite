#!/usr/bin/env bash
# 09i — query kes-period-info
# Validates the operational certificate against the current KES period.
# Both nodes should agree on the current KES period since genesis is shared.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

OPCERT="$LD_KEYS/pool1/opcert.cert"
if [ ! -f "$OPCERT" ]; then
    parity_record "kes-period-info" "SKIP" "skip" "skip" "env-skip: opcert not found (run setup.sh first)"
    exit 0
fi

parity_query_json "kes-period-info" "kes-period-info" "--op-cert-file" "$OPCERT"
