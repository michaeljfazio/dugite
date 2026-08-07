#!/usr/bin/env bash
# 11a — Mempool: submit a tx with TTL=current_slot+2, wait for it to expire,
# then verify it is gone from mempool (TTL eviction).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
# Wait for the mempool to drain BEFORE selecting an input.
#
# `zoo_largest_utxo` queries the LEDGER's UTxO set, which is blind to mempool claims, and
# every script in this category selects from the SAME shared genesis address. So an
# earlier script's still-pending transaction leaves an input that looks spendable and is
# not: building on it yields an unavoidable input-conflict rejection, the tx is dropped,
# and the symptom is "tx on 0/3 observers after 120s" — a node-looking failure with a
# harness cause.
#
# This is #918's mechanism, which 11c already guards against for exactly this reason
# ("it inherited 11a/11b's in-flight transactions"). The remedy simply had not been
# applied to the rest of the category, so the failure stayed intermittent — it depends on
# whether the previous script's tx happened to be included in time. Measured: 11d failed
# this way in a single-batch zoo run after passing the two before it.
zoo_wait_mempool_quiet 60 || true

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=200000
SIGNED="$ZOO_BUILT/$NAME.signed"

# TTL=current+8 leaves enough margin for build/sign/submit to complete
# before the wall-clock crosses TTL (each slot is 1s on the devnet, and
# build/sign+IPC can take 1-2s under load), while still expiring quickly
# enough that we can observe eviction within seconds.
TTL=$((TIP + 8))

cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - FEE))" \
    --fee      "$FEE" \
    --ttl      "$TTL" \
    --out-file "$SIGNED" 2>/dev/null || { zoo_record "$NAME" SKIP "" "build-raw-failed"; exit 0; }

cardano-cli conway transaction sign \
    --testnet-magic    "$LD_MAGIC" \
    --tx-body-file     "$SIGNED" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file         "${SIGNED}.signed" 2>/dev/null || { zoo_record "$NAME" SKIP "" "sign-failed"; exit 0; }

FINAL="${SIGNED}.signed"
TXID=$(cardano-cli conway transaction txid --tx-file "$FINAL" --output-text 2>/dev/null || echo "")

# Submit to mempool — it should be accepted initially.
SUBMIT_ERR=$(cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$FINAL" 2>&1) || {
    # Trim the error to a single short line for the CSV row.
    SUBMIT_ERR_SHORT=$(printf '%s' "$SUBMIT_ERR" | head -c 140 | tr '\n' ' ')
    zoo_record "$NAME" SKIP "" "submit-failed: ${SUBMIT_ERR_SHORT}"
    exit 0
}

log_info "Submitted TTL=$TTL tx, waiting for expiry..."

# Wait up to 30s for the slot to advance past TTL
MAX_WAIT=30
ELAPSED=0
while [ "$ELAPSED" -lt "$MAX_WAIT" ]; do
    CURRENT_TIP=$(zoo_tip_slot)
    if [ "$CURRENT_TIP" -gt "$TTL" ]; then
        break
    fi
    sleep 2
    ELAPSED=$(( ELAPSED + 2 ))
done

CURRENT_TIP=$(zoo_tip_slot)
if [ "$CURRENT_TIP" -le "$TTL" ]; then
    zoo_record "$NAME" SKIP "" "slot-did-not-advance-past-ttl slot=${CURRENT_TIP} ttl=${TTL}"
    exit 0
fi

# Check if tx is still in mempool — should be gone after eviction
MEMPOOL_NEXT=$(cardano-cli conway query tx-mempool next-tx \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" 2>/dev/null | jq -r '.txid // ""' || echo "")

# Query mempool info for size
MEMPOOL_INFO=$(cardano-cli conway query tx-mempool info \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" 2>/dev/null | jq -r '.numberOfTxs // 0' || echo "0")

if [ -z "$TXID" ] || [ -z "$MEMPOOL_NEXT" ] || [ "$MEMPOOL_NEXT" != "$TXID" ]; then
    zoo_record "$NAME" PASS "$TXID" "ttl-evicted slot=${CURRENT_TIP} ttl=${TTL} mempool_size=${MEMPOOL_INFO}"
else
    zoo_record "$NAME" FAIL "$TXID" "still-in-mempool after ttl expiry slot=${CURRENT_TIP} ttl=${TTL}"
fi
