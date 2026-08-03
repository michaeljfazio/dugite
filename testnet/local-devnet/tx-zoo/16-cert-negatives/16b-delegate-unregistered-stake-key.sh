#!/usr/bin/env bash
# 16b — delegate a stake key that was never registered.
# Expect: StakeKeyNotRegisteredDELEG (DELEG tag 3).
#
# Upstream has ONE constructor for this; dugite distinguishes the delegation
# and deregistration cases internally and deliberately drops that extra
# precision on the wire rather than inventing a tag (#979).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/16-cert-negatives/_cert-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
POOL_ID_FILE="$LD_KEYS/pool1/pool.id"
[ -s "$POOL_ID_FILE" ] || { zoo_record_env_skip "$NAME" "pool1 id missing — run setup.sh"; exit 0; }

# A fresh, never-registered stake key.
FRESH="$ZOO_BUILT/$NAME-stake"
cardano-cli conway stake-address key-gen \
    --verification-key-file "$FRESH.vkey" --signing-key-file "$FRESH.skey" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "keygen"; exit 1; }

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address stake-delegation-certificate \
    --stake-verification-key-file "$FRESH.vkey" \
    --stake-pool-id "$(cat "$POOL_ID_FILE")" \
    --out-file "$CERT" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "cert-create"; exit 1; }

SIGNED=$(cert_build_signed "$NAME" "$ADDR" "$WA/payment.skey" "$FRESH.skey" -- \
            --certificate-file "$CERT") || {
    if grep -qiE "StakeKeyNotRegistered|not registered" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (stake key not registered)"
        zoo_record "$NAME" PASS "" "rejected-StakeKeyNotRegisteredDELEG-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}
expect_cert_rejection "$NAME" "$SIGNED" "StakeKeyNotRegisteredDELEG"
