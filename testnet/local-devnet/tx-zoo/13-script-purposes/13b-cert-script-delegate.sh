#!/usr/bin/env bash
# 13b — delegate a SCRIPT stake credential to pool1.
#
# THE CERTIFYING PURPOSE. Delegating a script-held credential makes the script
# the subject of the certificate, so the ledger builds a `Certifying`
# ScriptPurpose and demands a redeemer for it. This is the first transaction in
# the entire zoo to carry redeemer tag 2.
#
# The script witness is supplied with --certificate-script-file +
# --certificate-redeemer-file, and collateral is required exactly as for a
# Plutus spend (phase-2 can fail, so the ledger needs something to collect).
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
    zoo_skip "script stake credential not registered (13a must run first)"
    zoo_record "$NAME" SKIP "" "not-registered"
    exit 0
fi

[ -s "$LD_KEYS/pool1/cold.vkey" ] || die "pool1 cold key missing"
POOL_ID=$(cardano-cli conway stake-pool id --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey")

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address stake-delegation-certificate \
    --stake-script-file "$SCRIPT" \
    --stake-pool-id "$POOL_ID" \
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
# TWO payment keys: the wallet's own key spends $TXIN, and the zoo payment key
# owns the collateral UTxO (plutus_collateral draws from the pre-split pool at
# the GENESIS address). Omitting the second one submits fine as far as the CLI
# is concerned and is rejected by the node with
# MissingVKeyWitnessesUTXOW — which reads like a dugite bug but is not.
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null

# The point of the test: prove tag 2 is on the wire before submitting.
assert_purpose "$SIGNED" Certifying || { zoo_record "$NAME" FAIL "" "no-certifying-redeemer"; exit 1; }

TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
wait_all_strict "$TXID" 150 "$ADDR" \
    && zoo_record "$NAME" PASS "$TXID" "certifying-purpose pool=${POOL_ID:0:16}" \
    || { zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1; }
