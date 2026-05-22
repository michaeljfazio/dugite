#!/usr/bin/env bash
# 09k — query gov-state
# Returns Conway governance state: DRep votes, committee, proposals.
# Epoch-stable between proposals; should match exactly.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "gov-state" "gov-state"
