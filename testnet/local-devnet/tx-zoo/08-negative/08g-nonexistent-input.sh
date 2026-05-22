#!/usr/bin/env bash
# 08g — Rule 2: input referencing a non-existent UTxO must be rejected (InputNotFound).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
TIP=$(zoo_tip_slot)
FEE=200000
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"

# Fabricated txin: 32-byte all-zeros txid#0
FAKE_TXIN="0000000000000000000000000000000000000000000000000000000000000000#0"

cardano-cli conway transaction build-raw \
    --tx-in     "$FAKE_TXIN" \
    --tx-out    "${ADDR}+2000000" \
    --fee       "$FEE" \
    --ttl       $((TIP + 600)) \
    --out-file  "$RAW" 2>/dev/null || true

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "nonexistent-input submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-InputNotFound" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
