#!/usr/bin/env bash
# 13f — NEGATIVE twin: a Certifying-purpose script that always FAILS.
#
# The always-false validator is the subject of a delegation certificate. Both
# nodes must refuse to let this through as a valid certificate. The interesting
# part is WHERE it fails: an always-false script in a spend purpose fails in
# phase 2 (tx included, is_valid=false, collateral consumed). This asserts the
# same treatment for the Certifying purpose rather than assuming it — if
# dugite failed it in phase 1 while Haskell took the phase-2 path (or vice
# versa) that is a divergence the parity oracle would catch, and this script
# records which path was taken so the matrix has something to compare.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"
. "$ZOO_DIR/13-script-purposes/_purpose-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
W="script-stake-v3-false"
SCRIPT=$(script_file "$W")
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing false script-stake wallet — run run-all.sh --setup"; exit 0; }

ADDR=$(script_pay_addr "$W")
STAKE_ADDR=$(script_stake_addr "$W")
PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")

# Registration is not script-authorised, so it succeeds even for a false
# script. That is what gives us a registered credential to then FAIL to act on.
if [ "$(is_registered "$STAKE_ADDR")" != "yes" ]; then
    REG="$ZOO_BUILT/$NAME-reg.cert"
    cardano-cli conway stake-address registration-certificate \
        --stake-script-file "$SCRIPT" --key-reg-deposit-amt "$DEPOSIT" --out-file "$REG"
    U=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
    R0="$ZOO_BUILT/$NAME-reg.raw"; S0="$ZOO_BUILT/$NAME-reg.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${U%% *}" --change-address "$ADDR" \
        --certificate-file "$REG" --out-file "$R0" >/dev/null 2>&1 \
        || { zoo_record "$NAME" SKIP "" "reg-build-failed"; exit 0; }
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$R0" --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
        --out-file "$S0" >/dev/null
    T0=$(zoo_submit "$S0") && zoo_wait_inclusion "$T0" 120 >/dev/null 2>&1
fi

[ -s "$LD_KEYS/pool1/cold.vkey" ] || die "pool1 cold key missing"
POOL_ID=$(cardano-cli conway stake-pool id --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey")
CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address stake-delegation-certificate \
    --stake-script-file "$SCRIPT" --stake-pool-id "$POOL_ID" --out-file "$CERT"

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
write_redeemer "$REDEEMER"
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"

# `transaction build` runs the script client-side to compute ex-units, so an
# always-false script is refused right here. That IS a valid outcome for this
# test — record it as the phase-1/build-time rejection it is, and say so.
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-in         "${UTXO%% *}" \
        --tx-in-collateral "$COLLAT" \
        --change-address "$ADDR" \
        --certificate-file          "$CERT" \
        --certificate-script-file   "$SCRIPT" \
        --certificate-redeemer-file "$REDEEMER" \
        --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
    # Match on the WHOLE error file, not its tail: cardano-cli appends a
    # multi-line Haskell CallStack after the real message, and `tail -3` picked
    # up only that noise — which matched a generic /failed/ pattern and made a
    # correct rejection look like an unexpected one.
    #
    # The expected text is:
    #   Error: The following scripts have execution failures:
    #   the script for certificate 0 (in the list order of the certificates) failed with:
    #   Script hash: ...  /  Script language: PlutusV3
    if grep -qE "scripts have execution failures|script for certificate .* failed" \
            "$ZOO_LOGS/$NAME.err"; then
        DETAIL=$(grep -m1 -E "script for certificate" "$ZOO_LOGS/$NAME.err" | tr -d ',' | cut -c1-90)
        LANG=$(grep -m1 -E "Script language:" "$ZOO_LOGS/$NAME.err" | tr -d ',' | tr -s ' ')
        zoo_ok "always-false certifying script rejected at build: $DETAIL ($LANG)"
        zoo_record "$NAME" PASS "" "rejected-certifying-script-eval-failed-at-build"
        exit 0
    fi
    REASON=$(grep -m1 -E "^Error|Command failed" "$ZOO_LOGS/$NAME.err" | tr -d ',' | cut -c1-140)
    zoo_fail "build failed for an unexpected reason: $REASON"
    zoo_record "$NAME" FAIL "" "build-failed-wrong-reason"
    exit 1
fi

# If the build DID succeed, the node must reject it (or include it with
# is_valid=false and consume collateral). Either is a legitimate phase-2 path;
# what must not happen is silent acceptance as a valid delegation.
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" --tx-body-file "$RAW" \
    --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file "$SIGNED" >/dev/null
assert_purpose "$SIGNED" Certifying || { zoo_record "$NAME" FAIL "" "no-certifying-redeemer"; exit 1; }
if TXID=$(zoo_submit "$SIGNED" 2>/dev/null); then
    DELEG=$(cardano-cli conway query stake-address-info \
              --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
              --address "$STAKE_ADDR" 2>/dev/null | jq -r '.[0].stakeDelegation // "none"')
    if [ "$DELEG" = "none" ] || [ "$DELEG" = "null" ]; then
        zoo_record "$NAME" PASS "$TXID" "rejected-phase2-collateral-consumed"
    else
        zoo_fail "always-false certifying script produced a LIVE delegation to $DELEG"
        zoo_record "$NAME" FAIL "$TXID" "false-script-delegation-accepted"
        exit 1
    fi
else
    zoo_record "$NAME" PASS "" "rejected-at-submit"
fi
