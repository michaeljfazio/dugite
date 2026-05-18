#!/usr/bin/env bash
# 05g — Authorize a CC hot key under a cold key. This only succeeds if the
# cold key is an actual committee member (the local devnet genesis has an
# empty committee, so this is expected to FAIL build/submit on a vanilla
# devnet — the script records that outcome so the wire path stays exercised).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
CC="$ZOO_KEYS/cc-1"
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
[ -s "$CC/cc-cold.vkey" ] || die "cc-1 cold key missing — run setup"

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway governance committee create-hot-key-authorization-certificate \
    --cold-verification-key-file "$CC/cc-cold.vkey" \
    --hot-verification-key-file  "$CC/cc-hot.vkey" \
    --out-file "$CERT"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-in         "$TXIN" \
        --change-address "$ADDR" \
        --certificate-file "$CERT" \
        --out-file      "$RAW" 2> "$ZOO_LOGS/$NAME.err" ; then
    zoo_skip "build rejected (devnet committee is empty): $(tail -1 "$ZOO_LOGS/$NAME.err")"
    zoo_record "$NAME" SKIP "" "empty-committee"
    exit 0
fi
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$CC/cc-cold.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
