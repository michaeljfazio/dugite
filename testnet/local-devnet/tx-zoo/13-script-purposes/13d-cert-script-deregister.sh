#!/usr/bin/env bash
# 13d — deregister the SCRIPT stake credential.
#
# THE CERTIFYING PURPOSE, second form. Deregistration reclaims the deposit and
# therefore must be authorised by the credential — so a script credential means
# a `Certifying` ScriptPurpose and a redeemer, exactly as for delegation.
#
# Ordered after 13c (withdrawal) on purpose: deregistering first would remove
# the reward account the Rewarding-purpose test needs.
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
if [ "$(is_registered "$STAKE_ADDR")" != "yes" ]; then
    zoo_skip "script stake credential not registered"
    zoo_record "$NAME" SKIP "" "not-registered"
    exit 0
fi

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")
CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address deregistration-certificate \
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
if wait_all_strict "$TXID" 150 "$ADDR"; then
    # The deposit must come back and the account must be gone.
    STILL=$(is_registered "$STAKE_ADDR")
    if [ "$STILL" = "no" ]; then
        zoo_record "$NAME" PASS "$TXID" "certifying-purpose deregistered refund=$DEPOSIT"
    else
        zoo_record "$NAME" FAIL "$TXID" "still-registered-after-dereg"
        exit 1
    fi
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"
    exit 1
fi
