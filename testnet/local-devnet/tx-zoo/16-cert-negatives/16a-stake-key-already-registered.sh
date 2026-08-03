#!/usr/bin/env bash
# 16a — register a stake key that is already registered.
# Expect: StakeKeyRegisteredDELEG (ConwayDelegPredFailure tag 2), reached as
#         Ledger 2 -> CERTS 1 -> CERT 1 -> DELEG 2.
#
# Before #979 this reached cardano-cli as ConwayMempoolFailure.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/16-cert-negatives/_cert-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
[ -s "$WA/stake.vkey" ] || { zoo_record_env_skip "$NAME" "wallet-a stake key missing"; exit 0; }

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit // .keyDeposit // 2000000' "$PPARAMS")

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address registration-certificate \
    --stake-verification-key-file "$WA/stake.vkey" \
    --key-reg-deposit-amt "$DEPOSIT" \
    --out-file "$CERT" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "cert-create"; exit 1; }

SIGNED=$(cert_build_signed "$NAME" "$ADDR" "$WA/payment.skey" -- \
            --certificate-file "$CERT") || {
    # cardano-cli's `build` resolves the ledger state and may refuse locally.
    if grep -qiE "StakeKeyRegistered|already registered" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (stake key already registered)"
        zoo_record "$NAME" PASS "" "rejected-StakeKeyRegisteredDELEG-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}
expect_cert_rejection "$NAME" "$SIGNED" "StakeKeyRegisteredDELEG"
