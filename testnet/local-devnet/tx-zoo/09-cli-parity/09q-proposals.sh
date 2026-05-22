#!/usr/bin/env bash
# 09q — query proposals
# Returns the list of live governance action proposals.  Stable within an epoch.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "proposals" "proposals"
