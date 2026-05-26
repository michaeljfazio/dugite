#!/usr/bin/env bash
# 05g — Authorize CC hot keys under their cold keys. Carries TWO
# committee_hot_key_authorization certs in a single tx so that both cc-1
# and cc-2 (both seated at genesis by setup.sh) end this script with an
# authorised hot key. cc-1 then gets resigned by 05h while cc-2 stays
# authorised through 07f/07g, which would otherwise fail with
# `ConwayGovFailure (VotersDoNotExist ...)` once 05h retires cc-1.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
CC1="$ZOO_KEYS/cc-1"
CC2="$ZOO_KEYS/cc-2"
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
[ -s "$CC1/cc-cold.vkey" ] || die "cc-1 cold key missing — run setup"
[ -s "$CC2/cc-cold.vkey" ] || die "cc-2 cold key missing — run setup"

CERT1="$ZOO_BUILT/$NAME-cc1.cert"
CERT2="$ZOO_BUILT/$NAME-cc2.cert"
cardano-cli conway governance committee create-hot-key-authorization-certificate \
    --cold-verification-key-file "$CC1/cc-cold.vkey" \
    --hot-verification-key-file  "$CC1/cc-hot.vkey" \
    --out-file "$CERT1"
cardano-cli conway governance committee create-hot-key-authorization-certificate \
    --cold-verification-key-file "$CC2/cc-cold.vkey" \
    --hot-verification-key-file  "$CC2/cc-hot.vkey" \
    --out-file "$CERT2"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-in         "$TXIN" \
        --change-address "$ADDR" \
        --certificate-file "$CERT1" \
        --certificate-file "$CERT2" \
        --out-file      "$RAW" 2> "$ZOO_LOGS/$NAME.err" ; then
    zoo_skip "build rejected (devnet committee is empty): $(tail -1 "$ZOO_LOGS/$NAME.err")"
    zoo_record "$NAME" SKIP "" "empty-committee"
    exit 0
fi
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$CC1/cc-cold.skey" \
    --signing-key-file "$CC2/cc-cold.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "cc-1+cc-2" \
                              || { zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1; }
