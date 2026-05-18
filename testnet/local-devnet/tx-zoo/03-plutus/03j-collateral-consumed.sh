#!/usr/bin/env bash
# 03j — Plutus Phase-2 failure path: tx with --script-invalid forces ledger
# to mark is_valid=false and consume collateral, returning any collateral-return
# output. This exercises the divergent code path Dugite has to match exactly.
#
# We use the V2 always-true script but flip --script-invalid; cardano-cli's
# `transaction build` requires Phase-2 validation to be skipped when the user
# is intentionally building an invalid tx, so we use `transaction build-raw`.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_skip "missing $SCRIPT"; zoo_record "$NAME" SKIP; exit 0; }

PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}
SCRIPT_AMT=${PAIR##* }

COLLAT_RAW=$(zoo_utxo_at "$(cat "$ZOO_PAY_ADDR_FILE")" 1) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
COLLAT=${COLLAT_RAW%% *}
COLLAT_AMT=${COLLAT_RAW##* }
RETURN_AMT=$((COLLAT_AMT - 2000000))
[ "$RETURN_AMT" -lt 1000000 ] && { zoo_skip "collateral utxo too small ($COLLAT_AMT)"; zoo_record "$NAME" SKIP; exit 0; }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# Build-raw with --script-invalid sets the tx body's is_valid flag to false.
# Phase-2 budget fields are not exercised; on apply, the ledger consumes
# COLLAT, emits COLLAT_AMT - <total_collateral>, and skips SCRIPT_TXIN.
TIP=$(zoo_tip_slot)
TTL=$((TIP + 600))
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
PPARAMS=$(zoo_pparams_file)
# Need exec-units for the redeemer. Use modest placeholders since the script
# is always-true and the tx is intentionally invalid.
cardano-cli conway transaction build-raw \
    --tx-in         "$SCRIPT_TXIN" \
    --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present \
    --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-execution-units "(1000000,1000000)" \
    --tx-in-collateral  "$COLLAT" \
    --tx-total-collateral "$((COLLAT_AMT - RETURN_AMT))" \
    --tx-out-return-collateral "${ADDR}+${RETURN_AMT}" \
    --tx-out        "${ADDR}+2000000" \
    --fee           500000 \
    --ttl           "$TTL" \
    --script-invalid \
    --protocol-params-file "$PPARAMS" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 90 && zoo_record "$NAME" PASS "$TXID" "is_valid=false consumed=$((COLLAT_AMT - RETURN_AMT))" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
