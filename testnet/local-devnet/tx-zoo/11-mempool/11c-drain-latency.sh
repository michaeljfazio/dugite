#!/usr/bin/env bash
# 11c — Mempool: measure drain latency p50/p99.
# Submits a batch of N valid self-payments and measures how long each takes to
# disappear from the mempool (i.e. to be included in a block).
#
# Self-seeding (#918)
# -------------------
# This script used to record `no-txs-submitted` on every run. It was not that
# no transactions were built — it was that every one of them was rejected at
# submit time: 11a and 11b leave their own transactions IN FLIGHT, so the
# funding UTxO that `zoo_largest_utxo` reports from the ledger view has already
# been claimed by a pending mempool entry. The head of the chain conflicts, and
# because the batch is self-chained, every follower dies with it.
#
# So the script now seeds its own traffic instead of inheriting the residue of
# earlier scripts:
#   1. wait for the mempool to actually drain (`zoo_wait_mempool_quiet`),
#   2. submit ONE warm-up self-payment and wait for it to be included,
#   3. chain the measured batch off that confirmed output — an input nothing
#      else can be holding — so the measurement runs every round.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
FEE=200000

# Number of self-chained transactions to measure.
BATCH="${MEMPOOL_DRAIN_BATCH:-5}"
# How long to wait for earlier scripts' transactions to clear.
QUIET_WAIT="${MEMPOOL_QUIET_WAIT:-90}"
# How long to wait for the warm-up tx to be included.
WARMUP_WAIT="${MEMPOOL_WARMUP_WAIT:-90}"

build_sign() {
    # $1=out-file-stem  $2=txin  $3=out-lovelace  $4=ttl
    local stem="$1" txin="$2" amount="$3" ttl="$4"
    cardano-cli conway transaction build-raw \
        --tx-in    "$txin" \
        --tx-out   "${ADDR}+${amount}" \
        --fee      "$FEE" \
        --ttl      "$ttl" \
        --out-file "${stem}.body" 2>/dev/null || return 1
    cardano-cli conway transaction sign \
        --testnet-magic    "$LD_MAGIC" \
        --tx-body-file     "${stem}.body" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file         "$stem" 2>/dev/null || return 1
}

# ── Step 1+2: quiesce, then seed one confirmed self-payment ─────────────────
WARM_TXID=""
WARM_AMT=0
for attempt in 1 2; do
    zoo_wait_mempool_quiet "$QUIET_WAIT" || true

    UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
    WARM_IN="${UTXO%% *}"
    WARM_AVAIL="${UTXO##* }"
    WARM_AMT=$(( WARM_AVAIL - FEE ))
    if [ "$WARM_AMT" -lt $(( 2000000 * (BATCH + 1) )) ]; then
        zoo_record_env_skip "$NAME" "insufficient-funds-for-warmup avail=${WARM_AVAIL}"
        exit 0
    fi

    TIP=$(zoo_tip_slot)
    WARM_FILE="$ZOO_BUILT/${NAME}-warmup.signed"
    build_sign "$WARM_FILE" "$WARM_IN" "$WARM_AMT" $((TIP + 600)) || {
        zoo_record_env_skip "$NAME" "warmup-build-or-sign-failed"
        exit 0
    }
    WARM_TXID=$(cardano-cli conway transaction txid --tx-file "$WARM_FILE" --output-text 2>/dev/null || echo "")

    if WARM_ERR=$(cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" \
            --socket-path   "$ZOO_SOCKET" \
            --tx-file       "$WARM_FILE" 2>&1); then
        log_info "warm-up tx $WARM_TXID submitted (attempt $attempt); waiting for inclusion"
        if zoo_wait_inclusion "$WARM_TXID" "$WARMUP_WAIT" "$ADDR"; then
            break
        fi
        log_info "warm-up tx not included within ${WARMUP_WAIT}s; retrying"
    else
        log_info "warm-up submit failed (attempt $attempt): $(printf '%s' "$WARM_ERR" | head -c 160 | tr '\n' ' ')"
    fi
    WARM_TXID=""
done

if [ -z "$WARM_TXID" ]; then
    # Nothing was seeded, so nothing can be measured. Env-skip so this shows up
    # as a coverage gap instead of blending into the pass count.
    zoo_record_env_skip "$NAME" "warmup-tx-never-confirmed"
    exit 0
fi

# ── Step 3: build the measured batch off the confirmed warm-up output ───────
TIP=$(zoo_tip_slot)
PREV_TXIN="${WARM_TXID}#0"
PREV_AMT="$WARM_AMT"

log_info "Building mempool drain batch of $BATCH transactions from $PREV_TXIN..."

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

    build_sign "$TX_FILE" "$PREV_TXIN" "$OUT_AMT" $((TIP + 600)) || {
        zoo_record_env_skip "$NAME" "build-or-sign-failed-at-$i"
        exit 0
    }

    TXID=$(cardano-cli conway transaction txid --tx-file "$TX_FILE" --output-text 2>/dev/null || echo "")
    TXIDS+=("$TXID")
    TX_FILES+=("$TX_FILE")
    PREV_TXIN="${TXID}#0"
    PREV_AMT="$OUT_AMT"
done

if [ "$BATCH" -eq 0 ]; then
    zoo_record_env_skip "$NAME" "no-txs-built"
    exit 0
fi

# Submit all transactions, recording submit timestamp
SUBMITTED=0
for i in $(seq 0 $((BATCH - 1))); do
    # Use `if VAR=$(...)` so the assignment's exit status is consumed by the
    # if-statement context and does NOT trigger `set -e` from the script's
    # `set -euo pipefail`.
    if SUBMIT_ERR=$(cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" \
            --socket-path   "$ZOO_SOCKET" \
            --tx-file       "${TX_FILES[$i]}" 2>&1); then
        SUBMIT_TS+=("$(date +%s)")
        SUBMITTED=$(( SUBMITTED + 1 ))
    else
        SUBMIT_TS+=("0")
        SUBMIT_ERR_SHORT=$(printf '%s' "$SUBMIT_ERR" | head -c 160 | tr '\n' ' ')
        log_info "  tx $i submit failed: ${SUBMIT_ERR_SHORT}"
    fi
done

if [ "$SUBMITTED" -eq 0 ]; then
    zoo_record_env_skip "$NAME" "no-txs-submitted"
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

    PENDING=$(zoo_mempool_txcount)

    NOW=$(date +%s)
    # Iterate over the BATCH, not over SUBMITTED: those two differ as soon as
    # one submit fails, and indexing by the success count would silently drop
    # the tail of the batch from the measurement.
    for i in $(seq 0 $((BATCH - 1))); do
        TXID="${TXIDS[$i]}"
        [ -z "$TXID" ] && continue

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
