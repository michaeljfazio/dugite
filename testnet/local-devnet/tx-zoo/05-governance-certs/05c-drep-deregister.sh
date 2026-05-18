#!/usr/bin/env bash
# 05c — deregister drep-3 (a separate DRep so drep-1 stays around for voting).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
DREP="$ZOO_KEYS/drep-3"
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")

# Need to register drep-3 first if it isn't already.
PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.dRepDeposit // .drepDeposit // 500000000' "$PPARAMS")
DREP_KH=$(cardano-cli conway governance drep id \
    --drep-verification-key-file "$DREP/drep.vkey" --output-hex)
REG_LIST=$(cardano-cli conway query drep-state \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --drep-key-hash "$DREP_KH" 2>/dev/null || echo "[]")
if echo "$REG_LIST" | jq -e 'length>0' >/dev/null; then
    zoo_info "drep-3 already registered — proceeding to deregister"
else
    REG_CERT="$ZOO_BUILT/$NAME.reg.cert"
    cardano-cli conway governance drep registration-certificate \
        --drep-verification-key-file "$DREP/drep.vkey" \
        --key-reg-deposit-amt "$DEPOSIT" \
        --out-file "$REG_CERT"
    UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo-reg"; exit 1; }
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${UTXO%% *}" --change-address "$ADDR" \
        --certificate-file "$REG_CERT" \
        --out-file "$ZOO_BUILT/$NAME-reg.raw" >/dev/null
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$ZOO_BUILT/$NAME-reg.raw" \
        --signing-key-file "$WA/payment.skey" \
        --signing-key-file "$DREP/drep.skey" \
        --out-file "$ZOO_BUILT/$NAME-reg.signed" >/dev/null
    REG_TXID=$(zoo_submit "$ZOO_BUILT/$NAME-reg.signed") || { zoo_record "$NAME" FAIL "" "reg-submit"; exit 1; }
    zoo_wait_inclusion "$REG_TXID" 60 || { zoo_record "$NAME" FAIL "$REG_TXID" "reg-not-incl"; exit 1; }
fi

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway governance drep retirement-certificate \
    --drep-verification-key-file "$DREP/drep.vkey" \
    --deposit-amt "$DEPOSIT" \
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
    --signing-key-file "$DREP/drep.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "refunded=$DEPOSIT" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
