#!/usr/bin/env bash
# 09o — query treasury
# Returns the current treasury balance.  Epoch-stable between withdrawals.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "treasury" "treasury"
