#!/usr/bin/env bash
# 16f — deregister a stake key that was never registered.
# Expect: StakeKeyNotRegisteredDELEG (DELEG tag 3), reached as
#         Ledger 2 -> CERTS 1 -> CERT 1 -> DELEG 3.
#
# Same constructor as 16b (delegating an unregistered key): Haskell has ONE
# predicate for "this stake credential is not in dsUnified", regardless of
# whether the certificate that hit it was a delegation or a deregistration.
# 04d (04-stake/04d-stake-deregister.sh) deregisters wallet-b's ALREADY
# registered key — a positive case. This needs a key the ledger has never
# seen at all, so it generates a fresh one rather than reusing any wallet key.
#
# Upstream: cardano-node-tests test_addr_registration.py::test_deregister_not_registered_addr
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/16-cert-negatives/_cert-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
[ -s "$WA/payment.skey" ] || { zoo_record_env_skip "$NAME" "wallet-a payment key missing"; exit 0; }

# A fresh, never-registered stake key.
FRESH="$ZOO_BUILT/$NAME-stake"
cardano-cli conway stake-address key-gen \
    --verification-key-file "$FRESH.vkey" --signing-key-file "$FRESH.skey" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "keygen"; exit 1; }

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit // .keyDeposit // 2000000' "$PPARAMS")

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address deregistration-certificate \
    --stake-verification-key-file "$FRESH.vkey" \
    --key-reg-deposit-amt "$DEPOSIT" \
    --out-file "$CERT" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "cert-create"; exit 1; }

SIGNED=$(cert_build_signed "$NAME" "$ADDR" "$WA/payment.skey" "$FRESH.skey" -- \
            --certificate-file "$CERT") || {
    # cardano-cli's `build` resolves the ledger state and may refuse locally.
    if grep -qiE "StakeKeyNotRegistered|not registered" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (stake key not registered)"
        zoo_record "$NAME" PASS "" "rejected-StakeKeyNotRegisteredDELEG-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}
# RED-PROOF: change the 3rd arg below to a different (wrong) constructor
# once — expect_cert_rejection's wrong-reason branch must then FAIL even
# though the tx is still correctly rejected on-chain.
expect_cert_rejection "$NAME" "$SIGNED" "StakeKeyNotRegisteredDELEG"
