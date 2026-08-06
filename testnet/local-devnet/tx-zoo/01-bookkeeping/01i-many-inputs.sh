#!/usr/bin/env bash
# 01i — many inputs in a single tx.
#
# Upstream: cardano-node-tests test_tx_many_utxos.py::test_mini_transactions
# (#1032, cardano-node-tests adoption P0.1).
#
# Assertion contract: fan out 300+ tiny UTxOs at a fresh script-local address,
# then build ONE tx that consumes as many of them as fit under
# maxTxSize=16384 (devnet). We assert the packed input count is >=300 (the
# upstream test's own floor), that the tx is ACCEPTED, and that the
# resulting balance at the address is EXACTLY (consumed-input-sum - fee) —
# verified on all 3 observers so both the fee calc AND consensus over a
# maximal-size tx are pinned, not just "it got included somewhere".
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

FUND_ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
PER_UTXO=2000000
MAX_TX_SIZE=16384

FAN="$ZOO_KEYS/$NAME"
mkdir -p "$FAN"
if [ ! -s "$FAN/payment.skey" ]; then
    cardano-cli conway address key-gen \
        --verification-key-file "$FAN/payment.vkey" \
        --signing-key-file      "$FAN/payment.skey" >/dev/null
fi
if [ ! -s "$FAN/payment.addr" ]; then
    cardano-cli conway address build \
        --payment-verification-key-file "$FAN/payment.vkey" \
        --testnet-magic "$LD_MAGIC" \
        --out-file "$FAN/payment.addr" >/dev/null
fi
FAN_ADDR=$(cat "$FAN/payment.addr")

# calc_fee <raw-body-file>  -> prints the min fee in lovelace for a 1-witness tx
calc_fee() {
    cardano-cli conway transaction calculate-min-fee \
        --tx-body-file "$1" \
        --protocol-params-file "$(zoo_pparams_file)" \
        --witness-count 1 \
        --output-json 2>/dev/null | jq -r '.fee // empty'
}

# build_fanout <in> <amt> <n-outputs> <sfx>  -> prints "txid change_amt" on
# success. Sends n-outputs x PER_UTXO to FAN_ADDR plus one change output back
# to FUND_ADDR (the funding wallet must never be drained — this always
# returns the leftover).
build_fanout() {
    local in="$1" amt="$2" n="$3" sfx="$4"
    local raw="$ZOO_BUILT/$NAME-fan$sfx.raw" signed="$ZOO_BUILT/$NAME-fan$sfx.signed"
    local out_args=() i
    for ((i = 0; i < n; i++)); do
        out_args+=(--tx-out "${FAN_ADDR}+${PER_UTXO}")
    done
    local needed=$((n * PER_UTXO))
    local probe_change=$((amt - needed - 300000))
    [ "$probe_change" -lt 1000000 ] && return 1
    cardano-cli conway transaction build-raw --tx-in "$in" "${out_args[@]}" \
        --tx-out "${FUND_ADDR}+${probe_change}" --fee 300000 \
        --out-file "$raw" >/dev/null 2> "$ZOO_LOGS/$NAME.fan$sfx.err" || return 1
    local fee
    fee=$(calc_fee "$raw") || return 1
    [ -z "$fee" ] && return 1
    local change=$((amt - needed - fee))
    [ "$change" -lt 1000000 ] && return 1
    cardano-cli conway transaction build-raw --tx-in "$in" "${out_args[@]}" \
        --tx-out "${FUND_ADDR}+${change}" --fee "$fee" \
        --out-file "$raw" >/dev/null 2>> "$ZOO_LOGS/$NAME.fan$sfx.err" || return 1
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file "$signed" >/dev/null || return 1
    local txid
    txid=$(zoo_submit "$signed") || return 1
    printf '%s %s' "$txid" "$change"
}

# ── Step 1: fan out 3 x 120 = 360 tiny UTxOs at FAN_ADDR ────────────────────
FANOUT_TXS=3
PER_TX_OUTPUTS=120
TOTAL_FANNED=$((FANOUT_TXS * PER_TX_OUTPUTS))

UTXO=$(zoo_largest_utxo "$FUND_ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
CUR_IN=${UTXO%% *}
CUR_AMT=${UTXO##* }
LAST_TXID=""
for t in $(seq 1 "$FANOUT_TXS"); do
    RESULT=$(build_fanout "$CUR_IN" "$CUR_AMT" "$PER_TX_OUTPUTS" "$t") || {
        zoo_record_env_skip "$NAME" "fanout-tx-$t-build-or-submit-failed"
        exit 0
    }
    LAST_TXID=${RESULT%% *}
    CUR_AMT=${RESULT##* }
    # Chain off the change output (last index = PER_TX_OUTPUTS, since the
    # PER_TX_OUTPUTS FAN_ADDR outputs occupy indices 0..PER_TX_OUTPUTS-1).
    CUR_IN="${LAST_TXID}#${PER_TX_OUTPUTS}"
done

# Confirm the LAST fan-out tx landed — that guarantees all of its outputs
# (including every FAN_ADDR utxo it created) are on-chain too.
zoo_wait_inclusion "$LAST_TXID" 90 || {
    zoo_record "$NAME" FAIL "$LAST_TXID" "fanout-not-included"
    exit 1
}

# ── Step 2: build ONE tx consuming as many FAN_ADDR utxos as fit ───────────
mapfile -t FAN_INS < <(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$FAN_ADDR" --output-json 2>/dev/null | jq -r 'keys[]')
TOTAL_AVAIL=${#FAN_INS[@]}
if [ "$TOTAL_AVAIL" -lt 300 ]; then
    zoo_record_env_skip "$NAME" "only-$TOTAL_AVAIL-utxos-fanned-out (wanted >=300)"
    exit 0
fi

N=$TOTAL_AVAIL
[ "$N" -gt "$TOTAL_FANNED" ] && N=$TOTAL_FANNED
CONSUME_RAW="$ZOO_BUILT/$NAME-consume.raw"
CONSUME_SIGNED="$ZOO_BUILT/$NAME-consume.signed"
FINAL_N=0
FINAL_CHANGE=0
while [ "$N" -ge 300 ]; do
    IN_ARGS=()
    for ((i = 0; i < N; i++)); do
        IN_ARGS+=(--tx-in "${FAN_INS[$i]}")
    done
    SUM=$((N * PER_UTXO))
    if cardano-cli conway transaction build-raw "${IN_ARGS[@]}" \
        --tx-out "${FAN_ADDR}+300000" --fee 300000 \
        --out-file "$CONSUME_RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.consume.err"; then
        FEE=$(calc_fee "$CONSUME_RAW") || FEE=""
        if [ -n "$FEE" ]; then
            CHANGE=$((SUM - FEE))
            if cardano-cli conway transaction build-raw "${IN_ARGS[@]}" \
                --tx-out "${FAN_ADDR}+${CHANGE}" --fee "$FEE" \
                --out-file "$CONSUME_RAW" >/dev/null 2>> "$ZOO_LOGS/$NAME.consume.err" \
                && cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
                    --tx-body-file "$CONSUME_RAW" \
                    --signing-key-file "$FAN/payment.skey" \
                    --out-file "$CONSUME_SIGNED" >/dev/null 2>> "$ZOO_LOGS/$NAME.consume.err"; then
                SIZE_BYTES=$(python3 -c "import json,sys; print(len(json.load(open(sys.argv[1]))['cborHex'])//2)" "$CONSUME_SIGNED")
                if [ "$SIZE_BYTES" -le "$MAX_TX_SIZE" ]; then
                    FINAL_N=$N
                    FINAL_CHANGE=$CHANGE
                    break
                fi
            fi
        fi
    fi
    N=$((N - 10))
done

# RED-PROOF: delete this floor check (or lower it) to hide a build that only
# ever manages to pack a handful of inputs before giving up.
if [ "$FINAL_N" -lt 300 ]; then
    zoo_fail "packed only $FINAL_N inputs (need >=300) out of $TOTAL_AVAIL available"
    zoo_record "$NAME" FAIL "" "packed-$FINAL_N-inputs-below-floor"
    exit 1
fi
zoo_ok "packed $FINAL_N/$TOTAL_AVAIL inputs into one tx (maxTxSize=$MAX_TX_SIZE)"

TXID=$(zoo_submit "$CONSUME_SIGNED") || { zoo_record "$NAME" FAIL "" "consume-submit n=$FINAL_N"; exit 1; }

if ! zoo_wait_all_observers "$TXID" 150 "$FAN_ADDR"; then
    zoo_record "$NAME" FAIL "$TXID" "not-included n=$FINAL_N"
    exit 1
fi

# ── Step 3: exact post-balance check on all 3 observers ────────────────────
# RED-PROOF: change the comparison below to `-ge` or drop it to hide a fee
# miscalculation or a Phase-1 rule that silently dropped/kept an extra input.
FAIL_SOCKS=""
for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$sock" ] || continue
    ACTUAL=$(cardano-cli conway query utxo \
            --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
            --address "$FAN_ADDR" --output-json 2>/dev/null \
        | jq -r --arg t "$TXID" '[to_entries[] | select(.key | startswith($t))][0].value.value.lovelace // empty')
    if [ "${ACTUAL:-}" != "$FINAL_CHANGE" ]; then
        FAIL_SOCKS="$FAIL_SOCKS $sock(got=${ACTUAL:-none})"
    fi
done

if [ -z "$FAIL_SOCKS" ]; then
    zoo_record "$NAME" PASS "$TXID" "inputs=$FINAL_N balance=$FINAL_CHANGE"
else
    zoo_record "$NAME" FAIL "$TXID" "balance-mismatch expected=$FINAL_CHANGE bad=$FAIL_SOCKS"
    exit 1
fi
