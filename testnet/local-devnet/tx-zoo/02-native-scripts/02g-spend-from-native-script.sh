#!/usr/bin/env bash
# 02g — spend a UTxO sitting at a native-script address. Requires 02f to have
# run first (or the script address to otherwise hold funds).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
if [ ! -s "$ZOO_BUILT/native-script-addr" ]; then
    zoo_skip "no native-script-addr — run 02f first"
    zoo_record "$NAME" SKIP "" "no-precondition"
    exit 0
fi
SCRIPT_ADDR=$(cat "$ZOO_BUILT/native-script-addr")
SCRIPT="$ZOO_BUILT/02f-pay-to-native-script.script.json"
[ -s "$SCRIPT" ] || { zoo_skip "policy file missing from 02f"; zoo_record "$NAME" SKIP; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
# Pick the UTxO at the script address.
UTXO=$(zoo_largest_utxo "$SCRIPT_ADDR") || {
    zoo_skip "no funds at script addr"
    zoo_record "$NAME" SKIP "" "no-script-utxo"
    exit 0
}
TXIN=${UTXO%% *}

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-in-script-file "$SCRIPT" \
    --tx-out        "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
