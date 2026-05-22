#!/usr/bin/env bash
# 09m — query drep-stake-distribution
# Returns the stake delegated to each DRep.  Epoch-stable.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "drep-stake-distribution" "drep-stake-distribution" "--all-dreps"
