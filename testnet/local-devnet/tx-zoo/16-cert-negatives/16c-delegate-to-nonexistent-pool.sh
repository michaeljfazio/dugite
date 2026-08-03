#!/usr/bin/env bash
# 16c — delegate to a stake pool that does not exist.
# Expect: DelegateeStakePoolNotRegisteredDELEG (DELEG tag 6).
#
# Note the payload is a bare `KeyHash StakePool` — a bstr(28), NOT a
# `Credential` array. Getting that wrong yields a frame cardano-cli cannot
# decode (#979).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/16-cert-negatives/_cert-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
[ -s "$WA/stake.vkey" ] || { zoo_record_env_skip "$NAME" "wallet-a stake key missing"; exit 0; }

# 28 bytes of 0xAB — no pool can have this id.
FAKE_POOL="abababababababababababababababababababababababababababab"

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address stake-delegation-certificate \
    --stake-verification-key-file "$WA/stake.vkey" \
    --stake-pool-id "$FAKE_POOL" \
    --out-file "$CERT" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "cert-create"; exit 1; }

SIGNED=$(cert_build_signed "$NAME" "$ADDR" "$WA/payment.skey" "$WA/stake.skey" -- \
            --certificate-file "$CERT") || {
    if grep -qiE "DelegateeStakePoolNotRegistered|StakePoolNotRegistered|does not exist" \
            "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (pool not registered)"
        zoo_record "$NAME" PASS "" "rejected-DelegateeStakePoolNotRegisteredDELEG-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}
expect_cert_rejection "$NAME" "$SIGNED" "DelegateeStakePoolNotRegisteredDELEG"
