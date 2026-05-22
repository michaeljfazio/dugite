#!/usr/bin/env bash
# 08j — Rule 8: validity start slot is in the far future (NotYetValid).
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
SIGNED="$ZOO_BUILT/$NAME.signed"

# Validity interval starts 10000 slots in the future
FUTURE_START=$((TIP + 10000))

cardano-cli conway transaction build-raw \
    --tx-in                "$TXIN" \
    --tx-out               "${ADDR}+$((AMT - FEE))" \
    --fee                  "$FEE" \
    --invalid-before       "$FUTURE_START" \
    --invalid-hereafter    $((FUTURE_START + 600)) \
    --out-file             "$RAW" 2>/dev/null || true

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "not-yet-valid submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-NotYetValid" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
