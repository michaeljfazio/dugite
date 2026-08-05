#!/usr/bin/env bash
# 11e — 100-tx dependent chain, submitted back-to-back without waiting for
# inclusion between steps (mempool dependency tracking at depth, extending
# 01h's 3-tx chain and 11a-c's mempool coverage). Upstream precedent:
# cardano-node-tests chained-tx coverage (#1032, cardano-node-tests adoption
# P0.1).
#
# Each tx spends output #0 of the previous one. Every tx is built, signed,
# and its txid computed BEFORE any submission happens (a signed tx's id is
# deterministic — `transaction txid` needs no network), so the whole chain
# can be fired at the mempool as fast as the socket allows instead of paying
# an inclusion-wait per step.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

# Earlier scripts may still have transactions in flight; building on a UTxO
# the ledger view reports but that a pending tx has already claimed is an
# unavoidable input-conflict at submit time (the 11c lesson, #918).
zoo_wait_mempool_quiet 90 || true

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
FEE=200000
DEPTH=100

cur_in="$TXIN"
cur_amt="$AMT"
TXIDS=()
FILES=()
for n in $(seq 1 "$DEPTH"); do
    out_amt=$((cur_amt - FEE))
    if [ "$out_amt" -lt 2000000 ]; then
        zoo_info "insufficient funds after $((n - 1)) chain steps, stopping there"
        break
    fi
    RAW="$ZOO_BUILT/$NAME-$n.raw"
    SIGNED="$ZOO_BUILT/$NAME-$n.signed"
    cardano-cli conway transaction build-raw \
        --tx-in     "$cur_in" \
        --tx-out    "${ADDR}+${out_amt}" \
        --fee       "$FEE" \
        --out-file  "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.build.err" || {
        zoo_info "build failed at step $n: $(tail -1 "$ZOO_LOGS/$NAME.build.err")"
        break
    }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file  "$RAW" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file      "$SIGNED" >/dev/null
    txid=$(cardano-cli conway transaction txid --tx-file "$SIGNED" --output-text 2>/dev/null)
    TXIDS+=("$txid")
    FILES+=("$SIGNED")
    cur_in="${txid}#0"
    cur_amt="$out_amt"
done

TOTAL=${#FILES[@]}
if [ "$TOTAL" -eq 0 ]; then
    zoo_record_env_skip "$NAME" "no-txs-built"
    exit 0
fi

# ── Fire the whole chain at the mempool, back-to-back ───────────────────────
SUBMITTED=0
for f in "${FILES[@]}"; do
    if cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
            --tx-file "$f" >/dev/null 2>> "$ZOO_LOGS/$NAME.submit.err"; then
        SUBMITTED=$((SUBMITTED + 1))
    else
        zoo_info "submit failed at chain position $SUBMITTED: $(tail -1 "$ZOO_LOGS/$NAME.submit.err")"
        break
    fi
done

# RED-PROOF: change `-ne "$TOTAL"` to a looser threshold to hide the mempool
# dropping a middle transaction while still accepting a later dependent one
# (which would itself indicate a correctness bug, not just a coverage gap).
if [ "$SUBMITTED" -ne "$TOTAL" ]; then
    zoo_record "$NAME" FAIL "" "only-$SUBMITTED-of-$TOTAL-submitted"
    exit 1
fi

LAST_TXID="${TXIDS[-1]}"
if ! zoo_wait_inclusion "$LAST_TXID" 90; then
    zoo_record "$NAME" FAIL "$LAST_TXID" "chain-not-included depth=$TOTAL"
    exit 1
fi

# ── Exact final value: initial amount minus (depth x fee) ──────────────────
ACTUAL=$(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$ADDR" --output-json 2>/dev/null \
    | jq -r --arg t "$LAST_TXID" '[to_entries[] | select(.key | startswith($t))][0].value.value.lovelace // empty')

# RED-PROOF: drop this equality (accepting any positive balance) to hide a
# fee miscounted somewhere along a 100-tx chain.
if [ "${ACTUAL:-}" = "$cur_amt" ]; then
    zoo_record "$NAME" PASS "$LAST_TXID" "chain=$TOTAL value=$ACTUAL"
else
    zoo_record "$NAME" FAIL "$LAST_TXID" "value-mismatch expected=$cur_amt actual=${ACTUAL:-none}"
    exit 1
fi
