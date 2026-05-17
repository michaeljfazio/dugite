#!/usr/bin/env bash
# Submit a varied transaction batch for the 6h soak.
#
# Tx mix per batch:
#   A. simple self-payment           (1 tx, 1 in, 2 out)
#   B. multi-output self-payment     (1 tx, 1 in, 4 out)
#   C. metadata-tagged self-payment  (1 tx, 1 in, 2 out, metadata label 674)
#   D. chained self-payment sequence (3 txs, each spends [0] of previous)
#
# Total: 6 transactions per batch.
#
# All paths are resolved relative to the repo root. Submission goes through
# the dugite N2C socket using dugite-cli (which we want to exercise).
#
# Output: one stdout line per tx and a summary line. Non-zero exit on a
# scripting failure, NOT on tx rejection — the orchestrator parses the
# accepted/rejected counters.

set -uo pipefail
cd "$(dirname "$0")/.."

CLI="./target/release/dugite-cli"
SOCKET="./node.sock"
MAGIC=2
KEY_DIR="./keys"
ADDR=$(cat "$KEY_DIR/payment.addr")
SKEY="$KEY_DIR/payment.skey"
FEE=200000
TMP_DIR=$(mktemp -d -t soak-varied-XXXXXX)
trap 'rm -rf "$TMP_DIR"' EXIT

emit() { printf "[soak-varied] %s\n" "$*"; }

# Current slot + TTL.
CURRENT_SLOT=$(curl -s --max-time 3 http://localhost:12798/metrics 2>/dev/null \
    | awk '$1=="dugite_slot_number" {print int($2); exit}')
if [[ -z "${CURRENT_SLOT:-}" || "$CURRENT_SLOT" == "0" ]]; then
    CURRENT_SLOT=$("$CLI" query tip --socket-path "$SOCKET" --testnet-magic $MAGIC 2>/dev/null \
        | python3 -c "import sys,json; print(json.load(sys.stdin)['slot'])" 2>/dev/null || echo 0)
fi
TTL=$((CURRENT_SLOT + 7200))

emit "slot=$CURRENT_SLOT ttl=$TTL addr=${ADDR:0:24}..."

# UTxO list, sorted by value desc.
mapfile -t UTXO_LINES < <("$CLI" query utxo \
    --address "$ADDR" \
    --socket-path "$SOCKET" \
    --testnet-magic $MAGIC 2>/dev/null \
    | tail -n +3 \
    | awk '{print $1 "#" $2, $3}' \
    | sort -k2 -n -r)

if (( ${#UTXO_LINES[@]} < 4 )); then
    emit "FATAL not enough UTxOs (need 4, have ${#UTXO_LINES[@]})"
    echo "[soak-varied] done accepted=0 rejected=0"
    exit 0
fi
emit "utxos available=${#UTXO_LINES[@]}"

ACCEPTED=0
REJECTED=0
SUBMITTED_TXIDS=()

# ------------------------------------------------------------------
# submit_signed FILE LABEL
# ------------------------------------------------------------------
submit_signed() {
    local file="$1" label="$2"
    local txid
    # dugite-cli emits "Transaction hash: <hash>\n<hash>" — take the bare hash on the last line.
    txid=$("$CLI" transaction txid --tx-file "$file" 2>/dev/null \
        | awk '/^[0-9a-f]{64}$/ {print; exit}')
    [[ -z "$txid" ]] && txid="?"
    local out
    if out=$("$CLI" transaction submit \
            --tx-file "$file" \
            --socket-path "$SOCKET" \
            --testnet-magic $MAGIC 2>&1); then
        emit "  ${label}: OK   ${txid}"
        SUBMITTED_TXIDS+=("$txid")
        ACCEPTED=$((ACCEPTED + 1))
    else
        local errsum="${out//$'\n'/ }"
        emit "  ${label}: REJ  ${txid} err=${errsum:0:200}"
        REJECTED=$((REJECTED + 1))
    fi
}

# ------------------------------------------------------------------
# A. Simple self-payment
# ------------------------------------------------------------------
{
    local_line="${UTXO_LINES[0]}"
    UTXO_REF=$(echo "$local_line" | awk '{print $1}')
    AMOUNT=$(echo  "$local_line" | awk '{print $2}')
    SEND=2000000
    CHANGE=$((AMOUNT - SEND - FEE))
    if (( CHANGE < 1000000 )); then
        emit "A simple-pay: SKIP utxo too small (amount=$AMOUNT)"
    else
        RAW="$TMP_DIR/a.raw"; SIGNED="$TMP_DIR/a.signed"
        "$CLI" transaction build-raw \
            --tx-in "$UTXO_REF" \
            --tx-out "${ADDR}+${SEND}" \
            --tx-out "${ADDR}+${CHANGE}" \
            --fee "$FEE" --ttl "$TTL" \
            --out-file "$RAW" >/dev/null 2>&1 \
            || { emit "A simple-pay: build FAIL"; REJECTED=$((REJECTED+1)); }
        if [[ -s "$RAW" ]]; then
            "$CLI" transaction sign \
                --tx-body-file "$RAW" \
                --signing-key-file "$SKEY" \
                --out-file "$SIGNED" >/dev/null 2>&1 \
                || { emit "A simple-pay: sign FAIL"; REJECTED=$((REJECTED+1)); }
            [[ -s "$SIGNED" ]] && submit_signed "$SIGNED" "A simple-pay"
        fi
    fi
}

# ------------------------------------------------------------------
# B. Multi-output self-payment (4 outputs)
# ------------------------------------------------------------------
{
    local_line="${UTXO_LINES[1]}"
    UTXO_REF=$(echo "$local_line" | awk '{print $1}')
    AMOUNT=$(echo  "$local_line" | awk '{print $2}')
    OUT_EACH=1500000
    CHANGE=$((AMOUNT - OUT_EACH * 3 - FEE))
    if (( CHANGE < 1000000 )); then
        emit "B multi-out: SKIP utxo too small (amount=$AMOUNT)"
    else
        RAW="$TMP_DIR/b.raw"; SIGNED="$TMP_DIR/b.signed"
        "$CLI" transaction build-raw \
            --tx-in "$UTXO_REF" \
            --tx-out "${ADDR}+${OUT_EACH}" \
            --tx-out "${ADDR}+${OUT_EACH}" \
            --tx-out "${ADDR}+${OUT_EACH}" \
            --tx-out "${ADDR}+${CHANGE}" \
            --fee "$FEE" --ttl "$TTL" \
            --out-file "$RAW" >/dev/null 2>&1 \
            || { emit "B multi-out: build FAIL"; REJECTED=$((REJECTED+1)); }
        if [[ -s "$RAW" ]]; then
            "$CLI" transaction sign \
                --tx-body-file "$RAW" \
                --signing-key-file "$SKEY" \
                --out-file "$SIGNED" >/dev/null 2>&1 \
                || { emit "B multi-out: sign FAIL"; REJECTED=$((REJECTED+1)); }
            [[ -s "$SIGNED" ]] && submit_signed "$SIGNED" "B multi-out"
        fi
    fi
}

# ------------------------------------------------------------------
# C. Metadata-tagged self-payment (CIP-20-style label 674)
# ------------------------------------------------------------------
{
    META_FILE="$TMP_DIR/meta.json"
    NOW_ISO=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    cat > "$META_FILE" <<EOF
{
  "674": {
    "msg": ["soak-6h", "varied-batch", "$NOW_ISO"]
  }
}
EOF
    local_line="${UTXO_LINES[2]}"
    UTXO_REF=$(echo "$local_line" | awk '{print $1}')
    AMOUNT=$(echo  "$local_line" | awk '{print $2}')
    SEND=1500000
    CHANGE=$((AMOUNT - SEND - FEE))
    if (( CHANGE < 1000000 )); then
        emit "C metadata: SKIP utxo too small (amount=$AMOUNT)"
    else
        RAW="$TMP_DIR/c.raw"; SIGNED="$TMP_DIR/c.signed"
        "$CLI" transaction build-raw \
            --tx-in "$UTXO_REF" \
            --tx-out "${ADDR}+${SEND}" \
            --tx-out "${ADDR}+${CHANGE}" \
            --metadata-json-file "$META_FILE" \
            --fee "$FEE" --ttl "$TTL" \
            --out-file "$RAW" >/dev/null 2>&1 \
            || { emit "C metadata: build FAIL"; REJECTED=$((REJECTED+1)); }
        if [[ -s "$RAW" ]]; then
            "$CLI" transaction sign \
                --tx-body-file "$RAW" \
                --signing-key-file "$SKEY" \
                --out-file "$SIGNED" >/dev/null 2>&1 \
                || { emit "C metadata: sign FAIL"; REJECTED=$((REJECTED+1)); }
            [[ -s "$SIGNED" ]] && submit_signed "$SIGNED" "C metadata "
        fi
    fi
}

# ------------------------------------------------------------------
# D. 3-tx chain — each spends index-0 of previous
# ------------------------------------------------------------------
{
    local_line="${UTXO_LINES[3]}"
    UTXO_REF=$(echo "$local_line" | awk '{print $1}')
    AMOUNT=$(echo  "$local_line" | awk '{print $2}')
    NEED=$((FEE * 3 + 1500000))
    if (( AMOUNT < NEED )); then
        emit "D chain: SKIP utxo too small (have=$AMOUNT need=$NEED)"
    else
        cur_ref="$UTXO_REF"
        cur_amt="$AMOUNT"
        for n in 1 2 3; do
            out_amt=$((cur_amt - FEE))
            if (( out_amt < 1000000 )); then
                emit "D chain step $n: SKIP exhausted (out=$out_amt)"
                break
            fi
            RAW="$TMP_DIR/d${n}.raw"
            SIGNED="$TMP_DIR/d${n}.signed"
            "$CLI" transaction build-raw \
                --tx-in "$cur_ref" \
                --tx-out "${ADDR}+${out_amt}" \
                --fee "$FEE" --ttl "$TTL" \
                --out-file "$RAW" >/dev/null 2>&1 \
                || { emit "D chain $n: build FAIL"; REJECTED=$((REJECTED+1)); break; }
            "$CLI" transaction sign \
                --tx-body-file "$RAW" \
                --signing-key-file "$SKEY" \
                --out-file "$SIGNED" >/dev/null 2>&1 \
                || { emit "D chain $n: sign FAIL"; REJECTED=$((REJECTED+1)); break; }
            next_txid=$("$CLI" transaction txid --tx-file "$SIGNED" 2>/dev/null \
                | awk '/^[0-9a-f]{64}$/ {print; exit}')
            [[ -z "$next_txid" ]] && { emit "D chain $n: txid extract FAIL"; REJECTED=$((REJECTED+1)); break; }
            submit_signed "$SIGNED" "D chain   $n/3"
            cur_ref="${next_txid}#0"
            cur_amt="$out_amt"
        done
    fi
}

# ------------------------------------------------------------------
# Mempool peek for any of the submitted txids (best-effort).
# ------------------------------------------------------------------
sleep 2
MEMPOOL_CNT=$(curl -s --max-time 3 http://localhost:12798/metrics 2>/dev/null \
    | awk '$1=="dugite_mempool_tx_count" {print $2; exit}')
emit "mempool tx_count after submit=${MEMPOOL_CNT:-?}  submitted_ids=${#SUBMITTED_TXIDS[@]}"

emit "done accepted=$ACCEPTED rejected=$REJECTED"
