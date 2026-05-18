#!/usr/bin/env bash
# 04b — delegate wallet-a's stake to pool1 (the dugite-bp pool).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
[ -s "$LD_KEYS/pool1/cold.vkey" ] || die "pool1 cold key missing"
POOL_ID=$(cardano-cli conway stake-pool id --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey")

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address stake-delegation-certificate \
    --stake-verification-key-file "$WA/stake.vkey" \
    --stake-pool-id "$POOL_ID" \
    --out-file "$CERT"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --change-address "$ADDR" \
    --certificate-file "$CERT" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$WA/stake.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "pool=${POOL_ID:0:16}" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
