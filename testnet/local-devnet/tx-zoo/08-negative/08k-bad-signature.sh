#!/usr/bin/env bash
# 08k — Rule 14: wrong signing key (InvalidWitnessSignature).
# Build a valid tx body but sign with a freshly generated throwaway key
# (not the key that owns the UTxO).
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
WRONG_SKEY="$ZOO_BUILT/$NAME.wrong.skey"
WRONG_VKEY="$ZOO_BUILT/$NAME.wrong.vkey"

# Generate a throwaway payment key pair
cardano-cli conway key gen-payment \
    --signing-key-file  "$WRONG_SKEY" \
    --verification-key-file "$WRONG_VKEY" >/dev/null

cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - FEE))" \
    --fee      "$FEE" \
    --ttl      $((TIP + 600)) \
    --out-file "$RAW" >/dev/null

# Sign with the wrong key
cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$WRONG_SKEY" \
    --out-file       "$SIGNED" >/dev/null

zoo_expect_failure "bad-signature submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-InvalidWitnessSignature" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
