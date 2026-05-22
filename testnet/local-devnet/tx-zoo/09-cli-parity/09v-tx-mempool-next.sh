#!/usr/bin/env bash
# 09v — LocalTxMonitor: query tx-mempool-next
# Returns the next transaction from the mempool snapshot.  Values may differ
# between nodes; we verify only that the command succeeds on both sides and
# returns valid JSON (or the expected empty response).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

_mempool_next_ok() {
    local sock="$1"
    local out rc
    out=$(cardano-cli conway query tx-mempool \
        --testnet-magic "$LD_MAGIC" \
        --socket-path "$sock" \
        next-tx 2>&1) && rc=0 || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "error"
        return
    fi
    # Just verify it's valid JSON (or empty == no pending txs)
    if echo "$out" | jq . >/dev/null 2>&1; then
        echo "ok"
    else
        echo "invalid-json"
    fi
}

dugite_ok=$(_mempool_next_ok "$LD_DUGITE_BP_SOCK")
cardano_ok=$(_mempool_next_ok "$LD_CARDANO_BP_SOCK")

if [ "$dugite_ok" = "ok" ] && [ "$cardano_ok" = "ok" ]; then
    dsha=$(echo "ok" | sha256sum | awk '{print $1}')
    parity_record "tx-mempool-next/validity" "EQUAL" "$dsha" "$dsha" "both-ok"
elif [ "$dugite_ok" != "ok" ]; then
    parity_record "tx-mempool-next/validity" "ERROR" "error" "ok" \
        "dugite returned: $dugite_ok"
    exit 1
else
    parity_record "tx-mempool-next/validity" "ERROR" "ok" "error" \
        "cardano returned: $cardano_ok"
    exit 1
fi
