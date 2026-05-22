#!/usr/bin/env bash
# 09r — query future-pparams (ratified but not yet enacted protocol parameter changes)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "future-pparams" "future-pparams"
