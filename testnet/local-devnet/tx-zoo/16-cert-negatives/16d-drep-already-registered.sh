#!/usr/bin/env bash
# 16d — register a DRep that is already registered.
# Expect: ConwayDRepAlreadyRegistered (ConwayGovCertPredFailure tag 0),
#         reached as Ledger 2 -> CERTS 1 -> CERT 3 -> GOVCERT 0.
#
# GOVCERT tags are 0-based; DELEG's are 1-based. The two rules sit side by side
# under CERT and do NOT share a numbering convention (#979).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/16-cert-negatives/_cert-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
DREP="$ZOO_KEYS/drep-1"
[ -s "$DREP/drep.vkey" ] || { zoo_record_env_skip "$NAME" "drep-1 missing — run --setup"; exit 0; }

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.dRepDeposit // .drepDeposit // 500000000' "$PPARAMS")

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway governance drep registration-certificate \
    --drep-verification-key-file "$DREP/drep.vkey" \
    --key-reg-deposit-amt "$DEPOSIT" \
    --out-file "$CERT" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "cert-create"; exit 1; }

SIGNED=$(cert_build_signed "$NAME" "$ADDR" "$WA/payment.skey" "$DREP/drep.skey" -- \
            --certificate-file "$CERT") || {
    if grep -qiE "DRepAlreadyRegistered|already registered" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (DRep already registered)"
        zoo_record "$NAME" PASS "" "rejected-ConwayDRepAlreadyRegistered-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}
expect_cert_rejection "$NAME" "$SIGNED" "ConwayDRepAlreadyRegistered"
