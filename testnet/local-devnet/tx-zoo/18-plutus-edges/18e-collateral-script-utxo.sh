#!/usr/bin/env bash
# 18e — a script-address UTxO declared as collateral.
#
# Upstream: test_same_collateral_txin.
#
# `cardano-cli conway transaction build` refuses this LOCALLY ("Expected key
# witnessed collateral") the moment it resolves the collateral UTxO's
# address, before it ever reaches a node — so this script goes straight to
# build-raw to force the tx onto the wire and observe the real ledger-level
# verdict, per the #1033 issue's instruction.
#
# PIN: verify against live cardano-node 11.0.1 during devnet verification.
# WANT is sourced from IntersectMBO/cardano-ledger (oracle-verified against
# `master`, 2026-08-06): this is a dedicated, explicit UTXO-level structural
# check, not something that merely falls out of a missing witness.
#   eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxo.hs
#     vKeyLocked txOut = ... isKeyHashAddr / isKeyHashCompactAddr
#     validateScriptsNotPaidUTxO utxoCollateral =
#       failureOnNonEmptyMap (Map.filter (not . vKeyLocked) utxoCollateral)
#         ScriptsNotPaidUTxO
#   -- Babbage's feesOK -> validateTotalCollateral reuses this verbatim;
#   -- Conway's conwayUtxoTransition delegates to Babbage.babbageUtxoValidation
#   -- unmodified. Constructor: ScriptsNotPaidUTxO (NonEmptyMap TxIn (TxOut era)).
#
# HIGH CONFIDENCE (unlike 18d): crates/dugite-ledger/src/validation/
# collateral.rs already implements this exact check
# (`ValidationError::ScriptLockedCollateral`, wired to wire tag 13 /
# `ScriptsNotPaidUTxOUTXO` in dugite-network). This PIN is a confirmatory
# live check, not a probe into unknown territory.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WANT="ScriptsNotPaidUTxO"   # PIN — see header
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# The "collateral candidate": a plain pure-ADA UTxO sitting at the SCRIPT
# address (never spent as a script input in this tx — only cited as
# collateral, so it needs no datum).
COLLAT_PAIR=$(plutus_lock "$SCRIPT" none 20000000) || { zoo_record "$NAME" FAIL "" "lock-collat"; exit 1; }
SCRIPT_COLLAT_TXIN=${COLLAT_PAIR%% *}

# The ACTUAL spend, at a separate, freshly-locked UTxO of the same script.
SPEND_PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock-spend"; exit 1; }
SPEND_TXIN=${SPEND_PAIR%% *}; SPEND_AMT=${SPEND_PAIR##* }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
EXUNITS="(1000000,1000000)"
FEE=2000000
REG_OUT=$((SPEND_AMT - FEE))
TIP=$(zoo_tip_slot)
TTL=$((TIP + 100))
PPARAMS=$(zoo_pparams_file)

# No --tx-total-collateral / --tx-out-return-collateral declared at all —
# keeps IncorrectTotalCollateralField structurally out of scope so only the
# script-locked-address check can fire.
RAW="$ZOO_BUILT/$NAME.raw"
cardano-cli conway transaction build-raw \
    --tx-in "$SPEND_TXIN" --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-execution-units "$EXUNITS" \
    --tx-in-collateral "$SCRIPT_COLLAT_TXIN" \
    --tx-out "${ADDR}+${REG_OUT}" \
    --fee "$FEE" \
    --ttl "$TTL" \
    --protocol-params-file "$PPARAMS" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build-raw: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null

# RED-PROOF: replace SCRIPT_COLLAT_TXIN with a genuine key-locked collateral
# UTxO (e.g. `plutus_collateral`) and this must FAIL — proves the assertion
# depends on the collateral's address, not on anything else about the tx.
expect_utxo_rejection "$NAME" "$SIGNED" "$WANT" "PIN"
