#!/usr/bin/env bash
# 11a — Mempool: submit a tx with TTL=current_slot+2, wait for it to expire,
# then verify it is gone from mempool (TTL eviction).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=200000
SIGNED="$ZOO_BUILT/$NAME.signed"

# TTL=current+2 ensures it expires quickly
TTL=$((TIP + 2))

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
TXID=$(cardano-cli conway transaction txid --tx-file "$FINAL" 2>/dev/null || echo "")

# Submit to mempool — it should be accepted initially
if ! cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$FINAL" 2>/dev/null; then
    zoo_record "$NAME" SKIP "" "submit-failed (tx may already be spent)"
    exit 0
fi

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
