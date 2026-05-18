#!/usr/bin/env bash
# 01h — 3-tx chain. Each tx spends index-0 of the previous. Mempool must accept
# the chain in order; tests both mempool dependency tracking and that the
# block producer schedules them in arrival order.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_fail "no UTxO"; zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }

FEE=200000
TXIDS=()
cur_in="$TXIN"
cur_amt="$AMT"
for n in 1 2 3; do
    out_amt=$((cur_amt - FEE))
    if [ "$out_amt" -lt 2000000 ]; then
        zoo_fail "chain step $n: out_amt=$out_amt too small"
        zoo_record "$NAME" FAIL "" "chain-step-$n-too-small"
        exit 1
    fi
    RAW="$ZOO_BUILT/$NAME-$n.raw"
    SIGNED="$ZOO_BUILT/$NAME-$n.signed"
    cardano-cli conway transaction build-raw \
        --tx-in     "$cur_in" \
        --tx-out    "${ADDR}+${out_amt}" \
        --fee       "$FEE" \
        --out-file  "$RAW" >/dev/null
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file  "$RAW" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file      "$SIGNED" >/dev/null
    TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit-$n"; exit 1; }
    TXIDS+=("$TXID")
    cur_in="${TXID}#0"
    cur_amt="$out_amt"
done

# Wait for the LAST tx in the chain to land — implies all predecessors did too.
LAST="${TXIDS[-1]}"
zoo_wait_inclusion "$LAST" 90 && zoo_record "$NAME" PASS "$LAST" "chain=${#TXIDS[@]}" \
                              || zoo_record "$NAME" FAIL "$LAST" "chain-not-included"
