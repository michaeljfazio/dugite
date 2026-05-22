#!/usr/bin/env bash
# 09n — query committee-state
# Returns the current Constitutional Committee member states.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "committee-state" "committee-state"
