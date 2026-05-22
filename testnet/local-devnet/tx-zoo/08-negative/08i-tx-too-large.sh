#!/usr/bin/env bash
# 08i — Rule 6: transaction exceeding max_tx_size must be rejected (TxTooLarge).
# We pad the tx with a large metadata blob to exceed the default 16384-byte limit.
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
RAW="$ZOO_BUILT/$NAME.raw"
META="$ZOO_BUILT/$NAME.meta.json"
SIGNED="$ZOO_BUILT/$NAME.signed"

# Generate a metadata JSON with a 20KB string to exceed max_tx_size
python3 -c "
import json, sys
big = 'A' * 20000
print(json.dumps({'0': {'msg': [big]}}))
" > "$META"

cardano-cli conway transaction build-raw \
    --tx-in       "$TXIN" \
    --tx-out      "${ADDR}+$((AMT - FEE))" \
    --fee         "$FEE" \
    --ttl         $((TIP + 600)) \
    --metadata-json-file "$META" \
    --out-file    "$RAW" 2>/dev/null || true

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "tx-too-large submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-TxTooLarge" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
