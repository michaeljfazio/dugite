#!/usr/bin/env bash
# 02f — pay funds to a native-script address (so 02g can spend from it).
# The script is the same "all"-policy used elsewhere.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_fail "no UTxO"; zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}

KH=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_PAY_VKEY")
SCRIPT="$ZOO_BUILT/$NAME.script.json"
cat > "$SCRIPT" <<EOF
{ "type": "all", "scripts": [ { "type": "sig", "keyHash": "$KH" } ] }
EOF
SCRIPT_ADDR_FILE="$ZOO_BUILT/$NAME.script.addr"
cardano-cli conway address build \
    --payment-script-file "$SCRIPT" \
    --testnet-magic "$LD_MAGIC" \
    --out-file      "$SCRIPT_ADDR_FILE"
SCRIPT_ADDR=$(cat "$SCRIPT_ADDR_FILE")

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-out        "${SCRIPT_ADDR}+5000000" \
    --change-address "$ADDR" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
echo "$SCRIPT_ADDR" > "$ZOO_BUILT/native-script-addr"  # share with 02g
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "addr=${SCRIPT_ADDR:0:24}" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
