#!/usr/bin/env bash
# 03l — DoS regression test for #522: a tx with is_valid=false but a script
# that evaluates to True (is_valid tag mismatch) MUST be rejected at mempool
# admission with IsValidTagMismatch — it must NOT be included in a block.
#
# Attacker pattern: use always-true-v2 with --script-invalid.
# Expected: zoo_submit fails (node rejects at admission) OR the tx is submitted
#           but never included (if relay propagated it — this path should not
#           occur after the #522 fix).
#
# The test PASSES if the tx is REJECTED at submission (exit 1 from zoo_submit).
# The test FAILS if the tx ends up included in a block.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
# Execution budget for a REAL plutus-tx program (#969/#970).
#
# This was (1000000,1000000), tuned for a trivial always-true validator.
# Upstream's compiled output exceeds that, and running out of budget IS a
# phase-2 failure — which silently changed what these two tests measure:
# 03l stopped asserting "is_valid=false over a SUCCEEDING script is rejected"
# (the script no longer succeeded), and 03j started passing for the wrong
# reason (budget exhaustion rather than the script's own verdict).
#
# Devnet maxTxExecutionUnits = (steps 10000000000, memory 140000000).
EXUNITS="(2000000000,20000000)"

SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}
SCRIPT_AMT=${PAIR##* }

COLLAT_RAW=$(zoo_utxo_at "$(cat "$ZOO_PAY_ADDR_FILE")" 1) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
COLLAT=${COLLAT_RAW%% *}
COLLAT_AMT=${COLLAT_RAW##* }
RETURN_AMT=$((COLLAT_AMT - 2000000))
[ "$RETURN_AMT" -lt 1000000 ] && { zoo_skip "collateral utxo too small ($COLLAT_AMT)"; zoo_record "$NAME" SKIP "" "collateral-utxo-too-small=$COLLAT_AMT"; exit 0; }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

TIP=$(zoo_tip_slot)
# Keep the validity upper bound inside the time-translation horizon
# (devnet safe zone is 240 slots; horizon distance ranges ~241-640 slots
# depending on epoch position).  tip+600 made this test trip
# TimeTranslationPastHorizon on the Haskell BP for some tip positions
# (issue #733) instead of exercising the tag-mismatch path.
TTL=$((TIP + 100))
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
PPARAMS=$(zoo_pparams_file)
FEE=500000
REG_OUT=$((SCRIPT_AMT - FEE))

# Build a tx that uses always-true-v2 but claims is_valid=false.
# This is the IsValidTagMismatch attack pattern (#522):
#   declared: is_valid=false  (--script-invalid)
#   evaluated: is_valid=true  (always-true script passes Phase-2)
cardano-cli conway transaction build-raw \
    --tx-in         "$SCRIPT_TXIN" \
    --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present \
    --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-execution-units "$EXUNITS" \
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

# The submission MUST be rejected by dugite (IsValidTagMismatch at admission).
# zoo_submit returns non-zero when the node rejects the tx.
if TXID=$(zoo_submit "$SIGNED" 2>/dev/null); then
    # If submission somehow succeeded, the tx must not be included.
    if zoo_wait_inclusion "$TXID" 30 2>/dev/null; then
        zoo_record "$NAME" FAIL "$TXID" "tag-mismatch tx was included — #522 regression"
        exit 1
    else
        # Admission must reject the tag-mismatch tx (IsValidTagMismatch);
        # accepting it into the mempool at all is the #522/#734 bug even
        # if it never reaches a block.
        zoo_record "$NAME" FAIL "$TXID" "admitted at submission — must reject IsValidTagMismatch (#734)"
        exit 1
    fi
else
    # Rejected at submission — this is the expected #522 fix behaviour.
    zoo_record "$NAME" PASS "" "rejected-at-admission IsValidTagMismatch"
fi
