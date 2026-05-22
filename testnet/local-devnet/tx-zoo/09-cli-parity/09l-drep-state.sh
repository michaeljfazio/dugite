#!/usr/bin/env bash
# 09l — query drep-state
# Returns state of all registered DReps.  Epoch-stable within a registration.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "drep-state" "drep-state" "--all-dreps"
