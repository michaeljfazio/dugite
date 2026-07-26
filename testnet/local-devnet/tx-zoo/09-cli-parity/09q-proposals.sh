#!/usr/bin/env bash
# 09q — query proposals
# Returns the list of live governance action proposals.  Stable within an epoch.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

# cardano-cli 11.0.0.0 requires an explicit selector: either --all-proposals or
# the (--governance-action-tx-id, --governance-action-index) pair. Omitting it
# is a parse error ("Missing:"), not an empty-result query.
parity_query_json "proposals" "proposals" "--all-proposals"
