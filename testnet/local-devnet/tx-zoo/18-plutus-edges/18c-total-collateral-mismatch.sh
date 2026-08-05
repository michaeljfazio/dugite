#!/usr/bin/env bash
# 18c — declared --tx-total-collateral disagrees with the actual collateral
# balance (inputs minus collateral_return).
#
# Upstream: node #4744 / tests_plutus/test_mint_build.py (~line 1074).
#
# Isolated from InsufficientCollateral on purpose: the ACTUAL effective
# collateral (COLLAT_AMT - RETURN_AMT) is set to exactly the sufficiency
# threshold (so the sufficiency check alone would pass), while the DECLARED
# `total_collateral` field is a different number. Haskell's
# `IncorrectTotalCollateralField` is a straight equality check
# (`declared == effective`, dugite: crates/dugite-ledger/src/validation/
# collateral.rs "If total_collateral is declared, it must match the effective
# collateral"), independent of whether that effective amount happens to be
# sufficient — mixing the two would leave ambiguous which predicate actually
# fired.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}; SCRIPT_AMT=${PAIR##* }

COLLAT_PAIR=$(plutus_collateral_pair) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
COLLAT=${COLLAT_PAIR%% *}; COLLAT_AMT=${COLLAT_PAIR##* }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
# (steps, memory) — cardano-cli's --tx-in-execution-units tuple order,
# confirmed live via dugite-relay's ScriptFailed budget-exhaustion log
# ("cpu_remaining" tracked the FIRST tuple element). always-true-v2 needs
# ~1,893,779 steps / ~5,894 mem in practice (CEK decode overhead, despite
# "trivial" logic) — 1,000,000 steps was under-provisioned. This mattered
# concretely here: with the declared/effective collateral EQUAL (no
# mismatch), the tx reaches Phase-2 and a too-low step budget produces a
# ScriptFailed rejection that is indistinguishable, at the wire, from this
# category's #979-shape "degraded to ConwayMempoolFailure" signal — masking
# whether IncorrectTotalCollateralField's own encoder is exercised at all.
EXUNITS="(2000000,1000000)"
FEE=2000000
REG_OUT=$((SCRIPT_AMT - FEE))
PPARAMS=$(zoo_pparams_file)
COLLAT_PCT=$(jq -r '.collateralPercentage // 150' "$PPARAMS")
NEEDED=$(( (FEE * COLLAT_PCT + 99) / 100 ))
ACTUAL_RETURN=$((COLLAT_AMT - NEEDED))       # effective collateral == NEEDED (sufficient)
DECLARED_TOTAL=$((NEEDED + 500000))          # WRONG on purpose — mismatched from the actual delta

if [ "$ACTUAL_RETURN" -lt 1000000 ]; then
    zoo_skip "collateral utxo too small (needed=$NEEDED, have=$COLLAT_AMT)"
    zoo_record "$NAME" SKIP "" "collateral-utxo-too-small"; exit 0
fi

# RED-PROOF: set DECLARED_TOTAL=$NEEDED (i.e. correct) and this must FAIL —
# proves the assertion actually depends on the mismatch, not on collateral
# presence alone.
WANT="IncorrectTotalCollateralField"

RAW="$ZOO_BUILT/$NAME.raw"
cardano-cli conway transaction build-raw \
    --tx-in "$SCRIPT_TXIN" --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-execution-units "$EXUNITS" \
    --tx-in-collateral "$COLLAT" \
    --tx-total-collateral "$DECLARED_TOTAL" \
    --tx-out-return-collateral "${ADDR}+${ACTUAL_RETURN}" \
    --tx-out "${ADDR}+${REG_OUT}" \
    --fee "$FEE" \
    --protocol-params-file "$PPARAMS" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build-raw: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null

expect_utxo_rejection "$NAME" "$SIGNED" "$WANT" "declared=$DECLARED_TOTAL actual=$NEEDED"
