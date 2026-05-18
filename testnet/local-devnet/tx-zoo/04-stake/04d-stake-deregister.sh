#!/usr/bin/env bash
# 04d — deregister a stake address (refunds deposit). Run last so the rest
# of the zoo can use the registered stake address.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
# Use wallet-b, which 04c just registered. Run AFTER 06/07 so its
# governance functions are exercised first.
WB="$ZOO_KEYS/wallet-b"
ADDR=$(cat "$WB/payment-stake.addr")
STAKE_ADDR=$(cat "$WB/stake.addr")

REG=$(cardano-cli conway query stake-address-info \
        --testnet-magic "$LD_MAGIC" \
        --socket-path "$ZOO_SOCKET" \
        --address "$STAKE_ADDR" 2>/dev/null \
        | jq -r 'if length>0 then "yes" else "no" end')
if [ "$REG" != "yes" ]; then
    zoo_skip "$STAKE_ADDR not registered — nothing to deregister"
    zoo_record "$NAME" SKIP "" "not-registered"
    exit 0
fi

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")
CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address deregistration-certificate \
    --stake-verification-key-file "$WB/stake.vkey" \
    --key-reg-deposit-amt "$DEPOSIT" \
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
    --signing-key-file "$WB/payment.skey" \
    --signing-key-file "$WB/stake.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "refunded=$DEPOSIT" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
