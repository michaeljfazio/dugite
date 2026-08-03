#!/usr/bin/env bash
# 16e — register a stake key declaring the WRONG deposit.
#
# This is the #979 PV-inversion case, and the expected constructor depends on
# the protocol version:
#
#   PV <= 10  IncorrectDepositDELEG   (DELEG tag 1, one field, no Mismatch)
#   PV >= 11  DepositIncorrectDELEG   (DELEG tag 7, carrying a full Mismatch)
#
# `hardforkConwayDELEGIncorrectDepositsAndRefunds pv = pvMajor pv > 10`. Every
# real network runs PV 10 today, so an implementation carrying only the PV>=11
# arm would degrade the ONLY reachable case while its typed arm sat dead —
# which is exactly what #978 found in the withdrawal path.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/16-cert-negatives/_cert-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")

PPARAMS=$(zoo_pparams_file)
PV=$(jq -r '.protocolVersion.major // 10' "$PPARAMS")
DEPOSIT=$(jq -r '.stakeAddressDeposit // .keyDeposit // 2000000' "$PPARAMS")
BAD=$(( DEPOSIT + 1 ))

FRESH="$ZOO_BUILT/$NAME-stake"
cardano-cli conway stake-address key-gen \
    --verification-key-file "$FRESH.vkey" --signing-key-file "$FRESH.skey" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "keygen"; exit 1; }

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address registration-certificate \
    --stake-verification-key-file "$FRESH.vkey" \
    --key-reg-deposit-amt "$BAD" \
    --out-file "$CERT" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "cert-create"; exit 1; }

if [ "$PV" -ge 11 ]; then WANT="DepositIncorrectDELEG"; else WANT="IncorrectDepositDELEG"; fi

SIGNED=$(cert_build_signed "$NAME" "$ADDR" "$WA/payment.skey" "$FRESH.skey" -- \
            --certificate-file "$CERT") || {
    if grep -qiE "IncorrectDeposit|DepositIncorrect" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (wrong stake-key deposit)"
        zoo_record "$NAME" PASS "" "rejected-$WANT-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}
expect_cert_rejection "$NAME" "$SIGNED" "$WANT" "PV$PV"
