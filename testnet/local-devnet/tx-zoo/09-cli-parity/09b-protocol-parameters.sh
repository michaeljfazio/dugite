#!/usr/bin/env bash
# 09b — query protocol-parameters
# Protocol parameters are static within an era.  Both nodes should return
# byte-identical JSON (after jq-normalisation) at any point in the same epoch.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

parity_query_json "protocol-parameters" "protocol-parameters"
