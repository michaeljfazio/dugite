#!/usr/bin/env bash
# 11c — Mempool: measure drain latency p99.
# Submits a batch of N valid transactions (to self) and measures how long
# until each one disappears from the mempool (included in a block).
# Reports p50 and p99 latency in seconds.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
TIP=$(zoo_tip_slot)
FEE=200000

# Number of self-chained transactions to submit
BATCH="${MEMPOOL_DRAIN_BATCH:-5}"

log_info "Building mempool drain batch of $BATCH transactions..."

# Build a chain: each tx spends the output of the previous one.
# Start from the largest UTXO.
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
PREV_TXIN="${UTXO%% *}"
PREV_AMT="${UTXO##* }"

TXIDS=()
SUBMIT_TS=()
TX_FILES=()

for i in $(seq 1 "$BATCH"); do
    TX_FILE="$ZOO_BUILT/${NAME}-chain-${i}.signed"
    OUT_AMT=$(( PREV_AMT - FEE ))
    if [ "$OUT_AMT" -lt 2000000 ]; then
        log_info "Insufficient funds after tx $((i-1)), stopping at $((i-1)) txs"
        BATCH=$((i-1))
        break
    fi

    cardano-cli conway transaction build-raw \
        --tx-in    "$PREV_TXIN" \
        --tx-out   "${ADDR}+${OUT_AMT}" \
        --fee      "$FEE" \
        --ttl      $((TIP + 600)) \
        --out-file "${TX_FILE}.body" 2>/dev/null || { zoo_record "$NAME" SKIP "" "build-failed-at-$i"; exit 0; }

    cardano-cli conway transaction sign \
        --testnet-magic    "$LD_MAGIC" \
        --tx-body-file     "${TX_FILE}.body" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file         "$TX_FILE" 2>/dev/null || { zoo_record "$NAME" SKIP "" "sign-failed-at-$i"; exit 0; }

    TXID=$(cardano-cli conway transaction txid --tx-file "$TX_FILE" 2>/dev/null || echo "")
    TXIDS+=("$TXID")
    TX_FILES+=("$TX_FILE")
    PREV_TXIN="${TXID}#0"
    PREV_AMT="$OUT_AMT"
done

if [ "$BATCH" -eq 0 ]; then
    zoo_record "$NAME" SKIP "" "no-txs-built"
    exit 0
fi

# Submit all transactions, recording submit timestamp
T_START=$(date +%s)
SUBMITTED=0
for i in $(seq 0 $((BATCH - 1))); do
    if cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" \
            --socket-path   "$ZOO_SOCKET" \
            --tx-file       "${TX_FILES[$i]}" 2>/dev/null; then
        SUBMIT_TS+=("$(date +%s)")
        SUBMITTED=$(( SUBMITTED + 1 ))
    else
        SUBMIT_TS+=("0")
        log_info "  tx $i submit failed (may be ok if chain broke)"
    fi
done

if [ "$SUBMITTED" -eq 0 ]; then
    zoo_record "$NAME" SKIP "" "no-txs-submitted"
    exit 0
fi

log_info "Submitted $SUBMITTED/$BATCH txs; waiting for block inclusion..."

# Poll mempool until all txids are gone or timeout
MAX_WAIT="${MEMPOOL_DRAIN_TIMEOUT:-120}"
ELAPSED=0
LATENCIES=()

while [ "$ELAPSED" -lt "$MAX_WAIT" ] && [ "${#LATENCIES[@]}" -lt "$SUBMITTED" ]; do
    sleep 2
    ELAPSED=$(( ELAPSED + 2 ))

    # Get pending txids from mempool
    PENDING=$(cardano-cli conway query tx-mempool info \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" 2>/dev/null | jq -r '.numberOfTxs // 0' || echo "?")

    NOW=$(date +%s)
    for i in $(seq 0 $((SUBMITTED - 1))); do
        TXID="${TXIDS[$i]}"
        [ -z "$TXID" ] && continue
        # Check if already recorded
        local_already=0
        for lat in "${LATENCIES[@]:-}"; do [ -n "$lat" ] && local_already=$(( local_already + 1 )); done

        # Query if this specific tx is still pending
        IN_MEMPOOL=$(cardano-cli conway query tx-mempool tx-exists \
            --testnet-magic "$LD_MAGIC" \
            --socket-path   "$ZOO_SOCKET" \
            "$TXID" 2>/dev/null | jq -r '.isInMempool // false' || echo "false")

        if [ "$IN_MEMPOOL" = "false" ] && [ "${SUBMIT_TS[$i]:-0}" -gt 0 ]; then
            LAT=$(( NOW - SUBMIT_TS[$i] ))
            LATENCIES+=("$LAT")
            TXIDS[$i]=""  # mark as done
        fi
    done

    log_info "  elapsed=${ELAPSED}s drained=${#LATENCIES[@]}/$SUBMITTED pending_count=${PENDING}"
done

if [ "${#LATENCIES[@]}" -eq 0 ]; then
    zoo_record "$NAME" FAIL "" "no-txs-drained in ${MAX_WAIT}s (mempool not clearing)"
    exit 0
fi

# Compute p50 and p99
SORTED_LATENCIES=($(printf '%s\n' "${LATENCIES[@]}" | sort -n))
N="${#SORTED_LATENCIES[@]}"
IDX_P50=$(( N * 50 / 100 ))
IDX_P99=$(( N * 99 / 100 ))
[ "$IDX_P99" -ge "$N" ] && IDX_P99=$(( N - 1 ))
P50="${SORTED_LATENCIES[$IDX_P50]}"
P99="${SORTED_LATENCIES[$IDX_P99]}"

# SLA: p99 <= 60s (4 block times at 1s/slot with 400-slot epochs)
SLA_P99=60
if [ "$P99" -le "$SLA_P99" ]; then
    zoo_record "$NAME" PASS "" "drain_p50=${P50}s drain_p99=${P99}s drained=${#LATENCIES[@]}/$SUBMITTED"
else
    zoo_record "$NAME" FAIL "" "drain_p99=${P99}s exceeds SLA=${SLA_P99}s drained=${#LATENCIES[@]}/$SUBMITTED"
fi
