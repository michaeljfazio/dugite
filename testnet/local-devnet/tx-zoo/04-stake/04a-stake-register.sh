#!/usr/bin/env bash
# 04a — register wallet-a's stake key (Conway: with deposit from pparams).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
[ -s "$WA/stake.vkey" ] || die "wallet-a stake key missing — run setup"

# Skip if already registered.
ADDR=$(cat "$WA/payment-stake.addr")
STAKE_ADDR=$(cat "$WA/stake.addr")
REG=$(cardano-cli conway query stake-address-info \
        --testnet-magic "$LD_MAGIC" \
        --socket-path "$ZOO_SOCKET" \
        --address "$STAKE_ADDR" 2>/dev/null \
        | jq -r 'if length>0 then "yes" else "no" end')
if [ "$REG" = "yes" ]; then
    zoo_skip "$STAKE_ADDR already registered"
    zoo_record "$NAME" SKIP "" "already-registered"
    exit 0
fi

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")
CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address registration-certificate \
    --stake-verification-key-file "$WA/stake.vkey" \
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
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$WA/stake.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "deposit=$DEPOSIT" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
