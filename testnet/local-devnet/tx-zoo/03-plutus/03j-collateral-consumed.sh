#!/usr/bin/env bash
# 03j — Plutus Phase-2 failure path: tx with `is_valid=false` whose Plutus
# script actually FAILS Phase-2.  This is the LEGITIMATE collateral-consumed
# path: the ledger consumes collateral and skips regular inputs/outputs.
#
# Script used: always-false-v3.plutus — an Aiken-compiled validator whose
# spend handler is `fail`. The is_valid=false + script-fails path is
# era-agnostic; we use V3 because Aiken supports only V3 (the previous
# vendored always-false-v2 cborHex was malformed UPLC).
#
# The tx body declares is_valid=false (--script-invalid) AND evaluation
# agrees (scripts fail), so admission is accepted and collateral consumed.
#
# Note: using always-true with --script-invalid is the DoS ATTACKER pattern
# (#522) — that case is now rejected at mempool admission with IsValidTagMismatch.
# This test uses the CORRECT always-false script to exercise the legitimate path.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
# Spending WITH an inline datum, and this test needs the script to FAIL.
# `alwaysFailsNoDatum` returns TRUE for spending-with-datum — the inverse of
# what is wanted — so the failure has to come from `alwaysFailsWithDatum`.
# With the wrong one the test still passed, but only because the execution
# budget was exhausted; a pass for the wrong reason is not a pass.
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

# The fee must cover the DECLARED execution units — Cardano prices the budget a
# tx RESERVES, not what it consumes — so raising EXUNITS raised the minimum fee
# with it, and the old 500000 became FeeTooSmallUTxO against an expected
# 1474393. Kept clear of that so a pricing tweak does not turn this into a
# flake.
FEE=2000000

SCRIPT="$ZOO_DIR/lib/plutus/always-false-v3-spend.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}
SCRIPT_AMT=${PAIR##* }

COLLAT_PAIR=$(plutus_collateral_pair) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
COLLAT=${COLLAT_PAIR%% *}
COLLAT_AMT=${COLLAT_PAIR##* }
# Collateral must be DERIVED from the fee, not pinned.
#
# The ledger requires total_collateral >= ceil(fee * collateralPercentage/100).
# This used to hardcode a 2000000 margin, which happened to satisfy a 500000
# fee — so raising the fee for the bigger upstream script (#969/#970) turned it
# into `InsufficientCollateral`, a failure with nothing to do with what the
# test is about. Read the percentage from the chain and add 25% slack.
COLLAT_PCT=$(jq -r '.collateralPercentage // 150' "$(zoo_pparams_file)")
COLLAT_NEEDED=$(( (FEE * COLLAT_PCT + 99) / 100 ))
COLLAT_MARGIN=$(( COLLAT_NEEDED + COLLAT_NEEDED / 4 ))
RETURN_AMT=$((COLLAT_AMT - COLLAT_MARGIN))
[ "$RETURN_AMT" -lt 1000000 ] && { zoo_skip "collateral utxo too small ($COLLAT_AMT for margin $COLLAT_MARGIN)"; zoo_record "$NAME" SKIP "" "collateral-utxo-too-small=$COLLAT_AMT"; exit 0; }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# Build-raw with --script-invalid + always-false script:
#   declared is_valid=false  (--script-invalid)
#   evaluated is_valid=false (scripts actually fail Phase-2)
# → LEGITIMATE path: collateral consumed, SCRIPT_TXIN skipped.
# Phase-1 value-conservation holds on the regular-input/output sub-balance:
#   SCRIPT_TXIN_AMT = REG_OUT + FEE
# Collateral sub-balance:
#   COLLAT_AMT - total_collateral = RETURN_AMT
TIP=$(zoo_tip_slot)
# Keep the validity upper bound inside the time-translation horizon —
# tip+600 trips TimeTranslationPastHorizon on the Haskell BP for some
# tip positions (issue #733), which rejects the tx for the wrong reason.
TTL=$((TIP + 100))
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
PPARAMS=$(zoo_pparams_file)
REG_OUT=$((SCRIPT_AMT - FEE))
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
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 90 && zoo_record "$NAME" PASS "$TXID" "is_valid=false always-false-v3 consumed=$((COLLAT_AMT - RETURN_AMT))" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
