#!/usr/bin/env bash
# 01g — Conway tx with explicit treasury donation (new in PV9+).
# The `--treasury-donation` flag puts a value into the Conway-only tx body
# field that diverts ADA directly to the treasury at tx-apply time.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_fail "no UTxO"; zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}

DONATION=1000000   # 1 ADA donation

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
# `transaction build` will reject treasury-donation under PV9 if the field is
# unsupported; the script records that as FAIL so the limitation surfaces.
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-in         "$TXIN" \
        --tx-out        "${ADDR}+2000000" \
        --change-address "$ADDR" \
        --treasury-donation "$DONATION" \
        --out-file      "$RAW" 2> "$ZOO_LOGS/$NAME.err" ; then
    zoo_fail "build failed: $(tail -3 "$ZOO_LOGS/$NAME.err")"
    zoo_record "$NAME" FAIL "" "build-rejected"
    exit 1
fi
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" || zoo_record "$NAME" FAIL "$TXID" "not-included"
