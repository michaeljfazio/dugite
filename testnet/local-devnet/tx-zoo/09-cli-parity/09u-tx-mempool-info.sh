#!/usr/bin/env bash
# 09u — LocalTxMonitor: query tx-mempool-info
# Returns mempool capacity + current tx count + byte size.
# Values may differ between nodes (each has its own mempool), but the
# SHAPE (field presence + types) must match.  We compare structure only.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

_mempool_shape() {
    local sock="$1"
    cardano-cli conway query tx-mempool \
        --testnet-magic "$LD_MAGIC" \
        --socket-path "$sock" \
        info 2>/dev/null \
    | jq -Sc 'keys | sort' 2>/dev/null \
    || echo '["error"]'
}

dugite_shape=$(_mempool_shape "$LD_DUGITE_BP_SOCK")
cardano_shape=$(_mempool_shape "$LD_CARDANO_BP_SOCK")

dsha=$(printf '%s' "$dugite_shape"  | sha256sum | awk '{print $1}')
csha=$(printf '%s' "$cardano_shape" | sha256sum | awk '{print $1}')

if [ "$dsha" = "$csha" ]; then
    parity_record "tx-mempool-info/shape" "EQUAL" "$dsha" "$csha" "keys=$dugite_shape"
else
    parity_record "tx-mempool-info/shape" "DIVERGENT" "$dsha" "$csha" \
        "dugite=$dugite_shape cardano=$cardano_shape"
    exit 1
fi
