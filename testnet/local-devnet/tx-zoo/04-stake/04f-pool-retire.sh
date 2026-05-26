#!/usr/bin/env bash
# 04f — retire pool3 at a future epoch.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
POOL="$ZOO_KEYS/pool3"
[ -s "$POOL/cold.skey" ] || die "pool3 cold key missing"
ADDR=$(cat "$WA/payment-stake.addr")

CURRENT_EPOCH=$(zoo_tip_epoch)
RETIRE_EPOCH=$((CURRENT_EPOCH + 2))   # earliest valid retire epoch

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-pool deregistration-certificate \
    --cold-verification-key-file "$POOL/cold.vkey" \
    --epoch "$RETIRE_EPOCH" \
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
    --signing-key-file "$POOL/cold.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "retire_at=$RETIRE_EPOCH" \
                              || { zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1; }
