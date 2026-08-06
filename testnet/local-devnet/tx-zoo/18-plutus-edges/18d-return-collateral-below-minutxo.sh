#!/usr/bin/env bash
# 18d — the return-collateral output is below the minimum UTxO value.
#
# Upstream: tests_plutus_v2 collateral-output suite (CLI-text-only assertion
# upstream — the constructor itself is not named in the Python test, only a
# CLIError substring check). Declared --tx-total-collateral is kept
# CORRECT (matching the actual balance), and the collateral input is sized
# with generous headroom above collateralPercentage*fee, so this isolates
# the min-UTxO-on-return-collateral check specifically from
# IncorrectTotalCollateralField/InsufficientCollateral.
#
# PIN: verify against live cardano-node 11.0.1 during devnet verification.
# WANT is sourced from IntersectMBO/cardano-ledger (oracle-verified against
# `master`, 2026-08-06): Babbage's `allSizedOutputsBabbageTxBodyF` appends
# `collateralReturn` onto the regular output list and runs ONE
# `validateOutputTooSmallUTxO` pass over the combined sequence — Conway
# reuses `babbageUtxoValidation` verbatim, no Conway-specific override. The
# firing constructor is the Babbage one, NOT the generic Alonzo
# `OutputTooSmallUTxO`:
#   eras/babbage/impl/src/Cardano/Ledger/Babbage/Rules/Utxo.hs
#     data BabbageUtxoPredFailure era = ... | BabbageOutputTooSmallUTxO
#       (NonEmpty (TxOut era, Coin))
#
# OPEN RISK (found while writing this test, not fixed here — see the #1033
# PR report): crates/dugite-ledger/src/validation/phase1.rs Rule 5 (~line
# 1353, `for output in &body.outputs`) iterates ONLY `body.outputs`. It never
# folds `body.collateral_return` into that loop the way Haskell's
# `allSizedOutputsBabbageTxBodyF` does, and
# crates/dugite-ledger/src/validation/collateral.rs has no separate check
# either. dugite currently has NO min-UTxO enforcement on collateral_return
# at all, so this script may find dugite ACCEPTS what Haskell rejects — that
# would be a genuine finding, not a test bug.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WANT="BabbageOutputTooSmallUTxO"   # PIN — see header
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
# Generous lock amount: the collateral input needs enough headroom that a
# TINY return-collateral output still leaves a sufficient effective balance,
# so InsufficientCollateral cannot also fire and cloud the assertion.
PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}; SCRIPT_AMT=${PAIR##* }

COLLAT_PAIR=$(plutus_collateral_pair) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
COLLAT=${COLLAT_PAIR%% *}; COLLAT_AMT=${COLLAT_PAIR##* }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
# (steps, memory) — cardano-cli's --tx-in-execution-units tuple order,
# confirmed live via dugite-relay's ScriptFailed budget-exhaustion log
# ("cpu_remaining" tracked the FIRST tuple element). always-true-v2 needs
# ~1,893,779 steps / ~5,894 mem in practice; 1,000,000 was under-provisioned.
EXUNITS="(2000000,1000000)"
FEE=2000000
REG_OUT=$((SCRIPT_AMT - FEE))
PPARAMS=$(zoo_pparams_file)
COLLAT_PCT=$(jq -r '.collateralPercentage // 150' "$PPARAMS")

# A deliberately-tiny return-collateral output — well below any realistic
# minUTxO for a plain ada-only output (Conway floors are ~0.9-1.1M lovelace;
# see reference_alonzo_ada_only_val_size_is_two-class formulas).
RETURN_AMT=200000
TOTAL_COLLATERAL=$((COLLAT_AMT - RETURN_AMT))
NEEDED=$(( (FEE * COLLAT_PCT + 99) / 100 ))
if [ "$TOTAL_COLLATERAL" -lt "$NEEDED" ]; then
    zoo_skip "collateral utxo too small to isolate the minUTxO check (needed=$NEEDED, have=$TOTAL_COLLATERAL)"
    zoo_record "$NAME" SKIP "" "collateral-utxo-too-small"; exit 0
fi

# RED-PROOF: raise RETURN_AMT above the real minUTxO floor and this must
# FAIL — proves the assertion depends on the return-collateral output being
# genuinely too small, not on collateral presence alone.

RAW="$ZOO_BUILT/$NAME.raw"
cardano-cli conway transaction build-raw \
    --tx-in "$SCRIPT_TXIN" --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-execution-units "$EXUNITS" \
    --tx-in-collateral "$COLLAT" \
    --tx-total-collateral "$TOTAL_COLLATERAL" \
    --tx-out-return-collateral "${ADDR}+${RETURN_AMT}" \
    --tx-out "${ADDR}+${REG_OUT}" \
    --fee "$FEE" \
    --protocol-params-file "$PPARAMS" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build-raw: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null

expect_utxo_rejection "$NAME" "$SIGNED" "$WANT" "return_collateral=$RETURN_AMT (PIN)"
