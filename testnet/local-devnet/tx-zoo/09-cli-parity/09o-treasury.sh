#!/usr/bin/env bash
# 09o — query treasury
# Returns the current treasury balance.  Epoch-stable between withdrawals.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

# cardano-cli 11.0.0.0: `query treasury` has no --output-json/--output-yaml;
# it prints the bare lovelace value and offers only --out-file.
PARITY_OUTPUT_JSON=0 parity_query_json "treasury" "treasury"
