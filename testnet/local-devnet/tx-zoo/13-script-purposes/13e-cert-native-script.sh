#!/usr/bin/env bash
# 13e — certificate guarded by a NATIVE script credential (no Plutus at all).
#
# Registers and then delegates a stake credential whose credential is a native
# script. Native scripts do not use redeemers or collateral — the witness is
# the script itself in the witness set — so this exercises a genuinely
# different code path from 13b, and one the zoo had no coverage of: every
# certificate in the zoo used a key credential.
#
# The vendored native script is `{"type":"all","scripts":[]}`, which is
# vacuously true and needs no signatures, so acceptance depends only on the
# node's handling of a native-script credential on a certificate.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/13-script-purposes/_purpose-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
W="script-stake-native"
SCRIPT=$(script_file "$W")
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing native script-stake wallet — run run-all.sh --setup"; exit 0; }

ADDR=$(script_pay_addr "$W")
STAKE_ADDR=$(script_stake_addr "$W")
PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")
[ -s "$LD_KEYS/pool1/cold.vkey" ] || die "pool1 cold key missing"
POOL_ID=$(cardano-cli conway stake-pool id --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey")

CERTS=()
if [ "$(is_registered "$STAKE_ADDR")" != "yes" ]; then
    REG="$ZOO_BUILT/$NAME-reg.cert"
    cardano-cli conway stake-address registration-certificate \
        --stake-script-file "$SCRIPT" --key-reg-deposit-amt "$DEPOSIT" --out-file "$REG"
    CERTS+=(--certificate-file "$REG")
fi
DEL="$ZOO_BUILT/$NAME-deleg.cert"
cardano-cli conway stake-address stake-delegation-certificate \
    --stake-script-file "$SCRIPT" --stake-pool-id "$POOL_ID" --out-file "$DEL"
CERTS+=(--certificate-file "$DEL" --certificate-script-file "$SCRIPT")

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --change-address "$ADDR" \
    "${CERTS[@]}" \
    --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
    --out-file      "$SIGNED" >/dev/null

# A native-script witness carries NO redeemer — assert that explicitly, so a
# future change that silently turns this into a Plutus path is caught.
if python3 "$ZOO_LIB/tx-cbor-tool.py" redeemers --in "$SIGNED" 2>/dev/null | grep -q .; then
    zoo_fail "native-script certificate unexpectedly carries a redeemer"
    zoo_record "$NAME" FAIL "" "unexpected-redeemer"
    exit 1
fi

TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
wait_all_strict "$TXID" 150 "$ADDR" \
    && zoo_record "$NAME" PASS "$TXID" "native-script-cred-cert no-redeemer" \
    || { zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1; }
