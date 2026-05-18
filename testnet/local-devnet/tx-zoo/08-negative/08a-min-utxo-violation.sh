#!/usr/bin/env bash
# 08a — payment tx with an output below the minUTxO threshold. The node must
# reject. We use `build-raw` to bypass cardano-cli's client-side checks.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
TTL=$((TIP + 600))
FEE=200000
TINY=1   # 1 lovelace — guaranteed below minUTxO

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build-raw \
    --tx-in     "$TXIN" \
    --tx-out    "${ADDR}+${TINY}" \
    --tx-out    "${ADDR}+$((AMT - TINY - FEE))" \
    --fee       "$FEE" \
    --ttl       "$TTL" \
    --out-file  "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
# Expect submit FAILURE (BadInputsUTxO / OutputTooSmall).
zoo_expect_failure "min-utxo-violation submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-as-expected" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
