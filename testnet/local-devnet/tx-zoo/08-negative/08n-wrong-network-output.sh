#!/usr/bin/env bash
# 08n — Rule 5c: output address with wrong network tag (WrongNetworkInOutput).
# Sends ADA to an address encoded for mainnet (network_id=1) on a testnet node.
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
MAINNET_VKEY="$ZOO_BUILT/$NAME.mainnet.vkey"
MAINNET_SKEY="$ZOO_BUILT/$NAME.mainnet.skey"

# Generate a fresh key pair
cardano-cli conway address key-gen \
    --signing-key-file  "$MAINNET_SKEY" \
    --verification-key-file "$MAINNET_VKEY" >/dev/null

# Derive a MAINNET address (--mainnet flag)
MAINNET_ADDR=$(cardano-cli conway address build \
    --payment-verification-key-file "$MAINNET_VKEY" \
    --mainnet 2>/dev/null || echo "")

if [ -z "$MAINNET_ADDR" ]; then
    zoo_record "$NAME" SKIP "" "could-not-derive-mainnet-addr"
    exit 0
fi

cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${MAINNET_ADDR}+2000000" \
    --tx-out   "${ADDR}+$((AMT - 2000000 - FEE))" \
    --fee      "$FEE" \
    --ttl      $((TIP + 600)) \
    --out-file "$RAW" 2>/dev/null || true

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "wrong-network-output submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-WrongNetworkInOutput" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
