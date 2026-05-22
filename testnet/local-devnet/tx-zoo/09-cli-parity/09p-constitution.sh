#!/usr/bin/env bash
# 09p — query constitution
# Returns the current Conway constitution (hash + URL).  Stable after enactment.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "constitution" "constitution"
