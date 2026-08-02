#!/usr/bin/env bash
# 13g — register a DRep whose credential is a SCRIPT, then cast a vote with it.
#
# THE VOTING PURPOSE (redeemer tag 4). A vote cast by a script-credentialed
# DRep makes the script the subject of the vote, so the ledger builds a
# `Voting` ScriptPurpose. Every vote in the zoo before this used a key
# credential, so tag 4 had never been constructed.
#
# Two transactions:
#   1. DRep registration with --drep-script-hash (script is the credential)
#   2. a vote on 06a's InfoAction with --vote-script-file + --vote-redeemer-file
#
# The InfoAction is reused deliberately: it can never ratify (its voting
# threshold is NoVotingThreshold, so acceptance short-circuits False), which
# means casting a vote on it cannot perturb any other test's governance
# assertions.
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
SCRIPT_HASH=$(cat "$ZOO_KEYS/$W/stake-script.hash")

# The action to vote on: 06a's InfoAction.
ACTION_FILE="$ZOO_BUILT/gov-action-info.id"
if [ ! -s "$ACTION_FILE" ]; then
    zoo_skip "no InfoAction on file (06a must run first)"
    zoo_record "$NAME" SKIP "" "no-gov-action"
    exit 0
fi
ACTION_ID=$(cat "$ACTION_FILE")
ACTION_TX="${ACTION_ID%#*}"
ACTION_IX="${ACTION_ID#*#}"

PPARAMS=$(zoo_pparams_file)
DREP_DEPOSIT=$(jq -r '.dRepDeposit // 500000000' "$PPARAMS")

# ---- 1. register the script-credentialed DRep --------------------------------
REG_CERT="$ZOO_BUILT/$NAME-drep.cert"
cardano-cli conway governance drep registration-certificate \
    --drep-script-hash "$SCRIPT_HASH" \
    --key-reg-deposit-amt "$DREP_DEPOSIT" \
    --out-file "$REG_CERT" 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "drep cert: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "drep-cert"; exit 1; }

ALREADY=$(cardano-cli conway query drep-state --testnet-magic "$LD_MAGIC" \
            --socket-path "$ZOO_SOCKET" --drep-script-hash "$SCRIPT_HASH" 2>/dev/null \
            | jq -r 'if length>0 then "yes" else "no" end' 2>/dev/null || echo "no")
if [ "$ALREADY" != "yes" ]; then
    U=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
    R0="$ZOO_BUILT/$NAME-reg.raw"; S0="$ZOO_BUILT/$NAME-reg.signed"
    # Registering a DRep whose credential is a script DOES require the script
    # to authorise it (unlike a stake-credential registration, which is
    # permissionless) — so this first tx already carries a Certifying redeemer.
    RD="$ZOO_BUILT/$NAME-reg.redeemer.json"; write_redeemer "$RD"
    COL=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
    if ! cardano-cli conway transaction build \
            --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
            --tx-in "${U%% *}" --tx-in-collateral "$COL" --change-address "$ADDR" \
            --certificate-file "$REG_CERT" \
            --certificate-script-file "$SCRIPT" \
            --certificate-redeemer-file "$RD" \
            --out-file "$R0" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
        zoo_fail "drep reg build: $(tail -2 "$ZOO_LOGS/$NAME.err")"
        zoo_record "$NAME" FAIL "" "drep-reg-build"
        exit 1
    fi
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$R0" \
        --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file "$S0" >/dev/null
    T0=$(zoo_submit "$S0") || { zoo_record "$NAME" FAIL "" "drep-reg-submit"; exit 1; }
    # Pass $ADDR explicitly: zoo_wait_inclusion defaults to the GENESIS funder
    # address, but this transaction's change goes to the script-stake wallet, so
    # the default look-up can never find it and the tx looks lost when it landed
    # fine.
    zoo_wait_inclusion "$T0" 120 "$ADDR" >/dev/null 2>&1 \
        || { zoo_record "$NAME" FAIL "$T0" "drep-reg-not-included"; exit 1; }
    zoo_ok "script-credentialed DRep registered ($T0)"
fi

# ---- 2. cast the vote --------------------------------------------------------
VOTE="$ZOO_BUILT/$NAME.vote"
cardano-cli conway governance vote create \
    --yes \
    --governance-action-tx-id "$ACTION_TX" \
    --governance-action-index "$ACTION_IX" \
    --drep-script-hash "$SCRIPT_HASH" \
    --out-file "$VOTE" 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "vote create: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "vote-create"; exit 1; }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
write_redeemer "$REDEEMER"
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "${UTXO%% *}" \
    --tx-in-collateral "$COLLAT" \
    --change-address "$ADDR" \
    --vote-file          "$VOTE" \
    --vote-script-file   "$SCRIPT" \
    --vote-redeemer-file "$REDEEMER" \
    --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "vote build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "vote-build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null

assert_purpose "$SIGNED" Voting || { zoo_record "$NAME" FAIL "" "no-voting-redeemer"; exit 1; }

TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
wait_all_strict "$TXID" 150 "$ADDR" \
    && zoo_record "$NAME" PASS "$TXID" "voting-purpose drep-script=${SCRIPT_HASH:0:16}" \
    || { zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1; }
