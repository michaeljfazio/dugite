#!/usr/bin/env bash
# 18g — a PlutusV1 spend in a tx that ALSO carries an unrelated reference
# input. ACCEPTED — the Conway-era inversion.
#
# Upstream: test_v1_script_with_reference_input (re-raises if it fails in
# Conway, i.e. upstream itself expects ACCEPT on a Conway-era node).
#
# Pre-Conway (Babbage) Haskell's `transTxOutV1`/`checkV1Restrictions` had a
# BLANKET rule: any reference input at all on a tx invalidates a V1 script
# (`ReferenceInputsNotSupported`), because PV1.TxInfo has no field to carry
# them. Conway defines its OWN module-local `transTxOutV1` that keeps the
# inline-datum and reference-script checks but DROPS the blanket
# reference-inputs rule — a V1 script simply cannot SEE the reference input
# (translated then discarded), but its mere presence no longer fails the tx.
#
# dugite already implements this era gate — see
# crates/dugite-uplc/src/tx_info_populate.rs `check_v1_output_restrictions`:
#   if !conway_or_later && !tx.body.reference_inputs.is_empty() { ... reject ... }
# The pre-Conway `ReferenceInputsNotSupported`-class reject arm is DEAD CODE
# on this devnet (Conway PV10) — documented here rather than exercised,
# since there is no PV/era knob on this devnet to flip it back on.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v1.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

PAIR=$(plutus_lock "$SCRIPT" hash 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}

COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
DATUM="$ZOO_BUILT/always-true-v1.datum.json"
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# An unrelated, disjoint UTxO acts as the reference input (mirrors 03i).
REF_UTXO=$(zoo_utxo_at "$ADDR" 1) || { zoo_record "$NAME" FAIL "" "no-second-utxo"; exit 1; }
REF_IN=${REF_UTXO%% *}

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$SCRIPT_TXIN" \
    --tx-in-script-file "$SCRIPT" \
    --tx-in-datum-file  "$DATUM" \
    --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-collateral  "$COLLAT" \
    --read-only-tx-in-reference "$REF_IN" \
    --tx-out        "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
# RED-PROOF: submit without --read-only-tx-in-reference (a plain 03a) still
# passes on its own, so the meaningful sabotage here is deleting the
# `conway_or_later` gate in tx_info_populate.rs, which is out of this
# script's reach — the live-run signal is dugite REJECTING this tx at all.
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_record "$NAME" PASS "$TXID" "V1 spend + unrelated ref_in=${REF_IN}"
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
