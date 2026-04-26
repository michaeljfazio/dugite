#!/usr/bin/env bash
# Submit 5 self-to-self transactions to the dugite-node block producer.
# Uses keys at ./keys/ (payment.addr, payment.skey).
set -euo pipefail
cd "$(dirname "$0")/.."

CLI="./target/release/dugite-cli"
SOCKET="./node.sock"
MAGIC=2
KEY_DIR="./keys"
ADDR=$(cat "$KEY_DIR/payment.addr")
SKEY="$KEY_DIR/payment.skey"
FEE=200000
SEND=2000000
TX_COUNT="${TX_COUNT:-5}"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

# Get current slot from metrics, fallback to query tip
CURRENT_SLOT=$(curl -s --max-time 3 http://localhost:12798/metrics 2>/dev/null | grep '^dugite_slot_number ' | awk '{print int($2)}')
if [[ -z "$CURRENT_SLOT" || "$CURRENT_SLOT" == "0" ]]; then
    CURRENT_SLOT=$("$CLI" query tip --socket-path "$SOCKET" --testnet-magic $MAGIC 2>/dev/null | grep '"slot"' | head -1 | sed 's/[^0-9]//g')
fi
TTL=$((CURRENT_SLOT + 3600))

mapfile -t UTXO_LINES < <("$CLI" query utxo \
    --address "$ADDR" \
    --socket-path "$SOCKET" \
    --testnet-magic $MAGIC 2>/dev/null \
    | tail -n +3 \
    | awk '{print $1 "#" $2, $3}' \
    | sort -k2 -n -r)

AVAILABLE=${#UTXO_LINES[@]}
if (( AVAILABLE < TX_COUNT )); then
    TX_COUNT=$AVAILABLE
fi

echo "[submit-5-txs] slot=$CURRENT_SLOT ttl=$TTL utxos=$AVAILABLE submitting=$TX_COUNT"

ACCEPTED=0; REJECTED=0
for i in $(seq 1 "$TX_COUNT"); do
    IDX=$((i - 1))
    LINE="${UTXO_LINES[$IDX]}"
    UTXO_REF=$(echo "$LINE" | awk '{print $1}')
    AMOUNT=$(echo "$LINE" | awk '{print $2}')
    CHANGE=$((AMOUNT - SEND - FEE))
    if (( CHANGE < 1000000 )); then
        SEND_ALL=$((AMOUNT - FEE))
        if (( SEND_ALL < 1000000 )); then
            echo "[submit-5-txs] skip too-small UTxO ($AMOUNT)"
            continue
        fi
        TX_OUT_ARGS="--tx-out ${ADDR}+${SEND_ALL}"
    else
        TX_OUT_ARGS="--tx-out ${ADDR}+${SEND} --tx-out ${ADDR}+${CHANGE}"
    fi

    RAW="$TMP_DIR/tx-${i}.raw"
    SIGNED="$TMP_DIR/tx-${i}.signed"

    "$CLI" transaction build-raw \
        --tx-in "$UTXO_REF" \
        $TX_OUT_ARGS \
        --fee "$FEE" \
        --ttl "$TTL" \
        --out-file "$RAW" >/dev/null 2>&1 || { echo "[$i] build-raw FAIL"; ((REJECTED++)) || true; continue; }

    "$CLI" transaction sign \
        --tx-body-file "$RAW" \
        --signing-key-file "$SKEY" \
        --out-file "$SIGNED" >/dev/null 2>&1 || { echo "[$i] sign FAIL"; ((REJECTED++)) || true; continue; }

    if SUBMIT_OUT=$("$CLI" transaction submit \
            --tx-file "$SIGNED" \
            --socket-path "$SOCKET" \
            --testnet-magic $MAGIC 2>&1); then
        TXID=$("$CLI" transaction txid --tx-file "$SIGNED" 2>/dev/null || echo "?")
        echo "[$i] OK  ${TXID:0:16}... amount=$AMOUNT"
        ((ACCEPTED++)) || true
    else
        TXID=$("$CLI" transaction txid --tx-file "$SIGNED" 2>/dev/null || echo "?")
        echo "[$i] REJ ${TXID:0:16}... err=$SUBMIT_OUT"
        ((REJECTED++)) || true
    fi
done
echo "[submit-5-txs] done accepted=$ACCEPTED rejected=$REJECTED"
