#!/usr/bin/env bash
# 08l — Rule 10: MissingRequiredSigner.
# Build a tx that declares a required signer but doesn't include its witness.
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
REQ_VKEY="$ZOO_BUILT/$NAME.req.vkey"
REQ_SKEY="$ZOO_BUILT/$NAME.req.skey"

# Generate a fresh key whose vkey hash we'll declare as required signer
cardano-cli conway address key-gen \
    --signing-key-file  "$REQ_SKEY" \
    --verification-key-file "$REQ_VKEY" >/dev/null

# Get vkey hash
REQ_HASH=$(cardano-cli conway address key-hash \
    --payment-verification-key-file "$REQ_VKEY" 2>/dev/null || echo "")

if [ -z "$REQ_HASH" ]; then
    zoo_record "$NAME" SKIP "" "could-not-derive-req-signer-hash"
    exit 0
fi

cardano-cli conway transaction build-raw \
    --tx-in              "$TXIN" \
    --tx-out             "${ADDR}+$((AMT - FEE))" \
    --fee                "$FEE" \
    --ttl                $((TIP + 600)) \
    --required-signer-hash "$REQ_HASH" \
    --out-file           "$RAW" 2>/dev/null || true

# Sign ONLY with the payment key (NOT the required signer key)
cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "missing-required-signer submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-MissingRequiredSigner" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
