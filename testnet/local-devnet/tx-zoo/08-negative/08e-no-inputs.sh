#!/usr/bin/env bash
# 08e — Rule 1: transaction with no inputs must be rejected (NoInputs).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
TIP=$(zoo_tip_slot)
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"

# Build a tx with no inputs — just one output
cardano-cli conway transaction build-raw \
    --tx-out "${ADDR}+2000000" \
    --fee    200000 \
    --ttl    $((TIP + 600)) \
    --out-file "$RAW" 2>/dev/null || true

# Sign (even if build warned, sign the raw body)
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file "$SIGNED" 2>/dev/null || true

zoo_expect_failure "no-inputs submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-NoInputs" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
