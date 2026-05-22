#!/usr/bin/env bash
# 09d — query stake-distribution
# The stake snapshot is epoch-stable.  Both nodes should agree on the
# stake distribution for the current epoch.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "stake-distribution" "stake-distribution"
