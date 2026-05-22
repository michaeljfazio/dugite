#!/usr/bin/env bash
# 08o — Rule 3c: mint field present but no matching policy script (InvalidMint).
# Attempts to mint a native token without providing the minting script.
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

# Use a fabricated policy ID (32 zero bytes in hex) — no witness will be provided
FAKE_POLICY="0000000000000000000000000000000000000000000000000000000000000000"
ASSET="${FAKE_POLICY}.$(printf '%s' 'token01' | xxd -p)"

cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - FEE))+1 $ASSET" \
    --fee      "$FEE" \
    --ttl      $((TIP + 600)) \
    --mint     "1 $ASSET" \
    --out-file "$RAW" 2>/dev/null || true

# Sign with only the payment key (no policy script/witness)
cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "mint-without-policy submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-InvalidMint" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
