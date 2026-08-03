#!/usr/bin/env bash
# 13a — register a stake credential whose credential is a PLUTUS SCRIPT.
#
# `cardano-cli stake-address registration-certificate --key-reg-deposit-amt`
# emits Conway `reg_deposit_cert` (cert index 7), NOT the deposit-less Shelley
# `reg_cert` (index 0) — cardano-cli 11.0.0.0 will not emit index 0 at all
# ("Create a stake address registration certificate" is refused without the
# deposit argument).
#
# That distinction is load-bearing. Haskell `getScriptWitnessConwayTxCert`:
#
#   ConwayRegCert _ SNothing     -> Nothing             -- idx 0: permissionless
#   ConwayRegCert cred (SJust _) -> credScriptHash cred -- idx 7: WITNESS REQUIRED
#
# so index 7 with a script credential is a Certifying purpose like any other.
# An earlier version of this script omitted the witness; dugite accepted it and
# cardano-node rejected it with MissingScriptWitnessesUTXOW — the divergence
# that led to the phase-1 fix in `cert_required_script_witness`. The negative
# twin 13i pins that rejection so the gap cannot reopen.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"
. "$ZOO_DIR/13-script-purposes/_purpose-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
W="script-stake-v3"
SCRIPT=$(script_file "$W")
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing script-stake wallet — run run-all.sh --setup"; exit 0; }

ADDR=$(script_pay_addr "$W")
STAKE_ADDR=$(script_stake_addr "$W")

# single-shot: on a fresh chain this is legitimately unregistered, and a
# 20s poll here would just add 20s to every run.
if [ "$(is_registered "$STAKE_ADDR" 1)" = "yes" ]; then
    zoo_skip "$STAKE_ADDR already registered"
    zoo_record "$NAME" SKIP "" "already-registered"
    exit 0
fi

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")
CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address registration-certificate \
    --stake-script-file "$SCRIPT" \
    --key-reg-deposit-amt "$DEPOSIT" \
    --out-file "$CERT"

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
write_redeemer "$REDEEMER"
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-in-collateral "$COLLAT" \
    --change-address "$ADDR" \
    --certificate-file          "$CERT" \
    --certificate-script-file   "$SCRIPT" \
    --certificate-redeemer-file "$REDEEMER" \
    --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null

assert_purpose "$SIGNED" Certifying || { zoo_record "$NAME" FAIL "" "no-certifying-redeemer"; exit 1; }

TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
wait_all_strict "$TXID" 150 "$ADDR" \
    && zoo_record "$NAME" PASS "$TXID" "certifying-purpose script-cred-registered deposit=$DEPOSIT" \
    || { zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1; }
