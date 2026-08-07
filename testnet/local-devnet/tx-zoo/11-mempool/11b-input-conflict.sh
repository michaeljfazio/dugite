#!/usr/bin/env bash
# 11b — Mempool: submit two transactions spending the same input.
# The second must be rejected with an input-conflict error, never silently accepted.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
# Wait for the mempool to drain BEFORE selecting an input.
#
# `zoo_largest_utxo` queries the LEDGER's UTxO set, which is blind to mempool claims, and
# every script in this category selects from the SAME shared genesis address. So an
# earlier script's still-pending transaction leaves an input that looks spendable and is
# not: building on it yields an unavoidable input-conflict rejection, the tx is dropped,
# and the symptom is "tx on 0/3 observers after 120s" — a node-looking failure with a
# harness cause.
#
# This is #918's mechanism, which 11c already guards against for exactly this reason
# ("it inherited 11a/11b's in-flight transactions"). The remedy simply had not been
# applied to the rest of the category, so the failure stayed intermittent — it depends on
# whether the previous script's tx happened to be included in time. Measured: 11d failed
# this way in a single-batch zoo run after passing the two before it.
zoo_wait_mempool_quiet 60 || true

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=200000
TX1="$ZOO_BUILT/${NAME}-tx1.signed"
TX2="$ZOO_BUILT/${NAME}-tx2.signed"

# Tx1: spends TXIN, sends all-minus-fee back to self
cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - FEE))" \
    --fee      "$FEE" \
    --ttl      $((TIP + 600)) \
    --out-file "${TX1}.body" 2>/dev/null || { zoo_record "$NAME" SKIP "" "tx1-build-failed"; exit 0; }

cardano-cli conway transaction sign \
    --testnet-magic    "$LD_MAGIC" \
    --tx-body-file     "${TX1}.body" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file         "$TX1" 2>/dev/null || { zoo_record "$NAME" SKIP "" "tx1-sign-failed"; exit 0; }

# Tx2: same TXIN, different amount (still invalid spend of same input)
cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - FEE - 1000000))" \
    --fee      "$FEE" \
    --ttl      $((TIP + 600)) \
    --out-file "${TX2}.body" 2>/dev/null || { zoo_record "$NAME" SKIP "" "tx2-build-failed"; exit 0; }

cardano-cli conway transaction sign \
    --testnet-magic    "$LD_MAGIC" \
    --tx-body-file     "${TX2}.body" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file         "$TX2" 2>/dev/null || { zoo_record "$NAME" SKIP "" "tx2-sign-failed"; exit 0; }

TXID1=$(cardano-cli conway transaction txid --tx-file "$TX1" --output-text 2>/dev/null || echo "")
TXID2=$(cardano-cli conway transaction txid --tx-file "$TX2" --output-text 2>/dev/null || echo "")

# Submit tx1 first — should succeed
if ! cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$TX1" 2>/dev/null; then
    zoo_record "$NAME" SKIP "$TXID1" "tx1-submit-failed (utxo may be spent)"
    exit 0
fi

log_info "Tx1 submitted ($TXID1), now submitting conflicting tx2..."

# Submit tx2 — must be rejected
SUBMIT2_OUT=$(cardano-cli conway transaction submit \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-file       "$TX2" 2>&1) && SUBMIT2_RC=0 || SUBMIT2_RC=$?

if [ "$SUBMIT2_RC" -ne 0 ] || echo "$SUBMIT2_OUT" | grep -qiE 'conflict|input|utxo|invalid|error|reject'; then
    zoo_record "$NAME" PASS "$TXID2" "input-conflict-rejected rc=$SUBMIT2_RC"
else
    zoo_record "$NAME" FAIL "$TXID2" "conflicting-tx-silently-accepted: $SUBMIT2_OUT"
fi
