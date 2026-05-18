#!/usr/bin/env bash
# 03j — Plutus Phase-2 failure path (SKIPPED).
#
# This test should exercise the collateral-consumed path: a tx that
# *actually fails* Phase-2 evaluation, signed with --script-invalid so
# the ledger applies it with is_valid=false (consuming collateral
# instead of regular inputs, emitting only the collateral-return).
#
# The vendored script `always-true-v2.plutus` is the wrong vehicle:
# Phase-2 succeeds for any input, so combining it with --script-invalid
# produces a tx whose declared `is_valid=false` disagrees with the
# evaluator's `is_valid=true`. Per ConwayUtxowFailure rules, the block
# carrying that tx fails the IsValid check, and the BP gets stuck
# re-forging it indefinitely.
#
# To run this test properly we need to vendor an `always-false-v2`
# (or v3) validator under `lib/plutus/`. Filed as a separate harness
# follow-up; mark SKIP for now so the rest of the zoo can progress.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
zoo_skip "needs an always-false validator (always-true + --script-invalid is invalid block)"
zoo_record "$NAME" SKIP "" "needs-always-false-validator"
exit 0

# (Original implementation below kept for reference; see header.)
. "$ZOO_DIR/03-plutus/_lock-helper.sh"

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
# Phase-1 value-conservation is enforced for ALL txs, including
# is_valid=false. The regular-path balance must hold:
#   sum(inputs)  ==  sum(outputs)  +  fee
# So the single regular tx-out must drain SCRIPT_TXIN minus the fee.
# (Collateral inputs / collateral-return are a separate sub-balance:
#  collat_in - total_collateral == collat_return.)
FEE=500000
REG_OUT=$((SCRIPT_AMT - FEE))
cardano-cli conway transaction build-raw \
    --tx-in         "$SCRIPT_TXIN" \
    --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present \
    --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-execution-units "(1000000,1000000)" \
    --tx-in-collateral  "$COLLAT" \
    --tx-total-collateral "$((COLLAT_AMT - RETURN_AMT))" \
    --tx-out-return-collateral "${ADDR}+${RETURN_AMT}" \
    --tx-out        "${ADDR}+${REG_OUT}" \
    --fee           "$FEE" \
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
