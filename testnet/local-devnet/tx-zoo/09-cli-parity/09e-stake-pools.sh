#!/usr/bin/env bash
# 09e — query stake-pools
# Returns the set of registered pool IDs.  Epoch-stable; should match exactly.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "stake-pools" "stake-pools"
