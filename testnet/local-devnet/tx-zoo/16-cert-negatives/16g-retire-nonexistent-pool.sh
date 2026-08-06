#!/usr/bin/env bash
# 16g — retire a stake pool whose cold key was never registered.
# Expect: StakePoolNotRegisteredOnKeyPOOL, reached as
#         Ledger 2 -> CERTS 1 -> CERT 2 -> POOL 1 (retirement of an unknown pool).
#
# Distinct from DELEG's DelegateeStakePoolNotRegisteredDELEG (16c): a
# PoolRetirement certificate never touches a stake credential, so it cannot
# reach the DELEG rule at all — it is validated entirely by POOL. Haskell
# keeps these as two separate predicate-failure families under two different
# rules even though the plain-English reason ("that pool doesn't exist")
# reads the same in both.
#
# Uses a freshly generated cold key (same `node key-gen` pattern as
# 08-negative/08s-pool-cost-too-low.sh) rather than any of the zoo's
# provisioned pools (pool1/pool2/pool3), all of which ARE registered.
#
# Upstream: cardano-node-tests test_pools.py — deregister an unregistered pool.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/16-cert-negatives/_cert-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# A fresh cold key that has never been the subject of a PoolRegistration cert.
COLD_VKEY="$ZOO_BUILT/$NAME.cold.vkey"
COLD_SKEY="$ZOO_BUILT/$NAME.cold.skey"
COUNTER="$ZOO_BUILT/$NAME.counter"
cardano-cli conway node key-gen \
    --cold-verification-key-file "$COLD_VKEY" \
    --cold-signing-key-file      "$COLD_SKEY" \
    --operational-certificate-issue-counter-file "$COUNTER" >/dev/null 2>&1 \
    || { zoo_record "$NAME" SKIP "" "node-key-gen-not-available"; exit 0; }

RETIRE_EPOCH=$(( $(zoo_tip_epoch) + 2 ))   # earliest valid retire epoch
CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-pool deregistration-certificate \
    --cold-verification-key-file "$COLD_VKEY" \
    --epoch "$RETIRE_EPOCH" \
    --out-file "$CERT" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "cert-create"; exit 1; }

SIGNED=$(cert_build_signed "$NAME" "$ADDR" "$ZOO_PAY_SKEY" "$COLD_SKEY" -- \
            --certificate-file "$CERT") || {
    # cardano-cli's `build` resolves the ledger state and may refuse locally.
    if grep -qiE "StakePoolNotRegistered|does not exist|pool.*not.*regist" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (pool not registered)"
        zoo_record "$NAME" PASS "" "rejected-StakePoolNotRegisteredOnKeyPOOL-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}
# RED-PROOF: change the 3rd arg below to a different (wrong) constructor
# once — expect_cert_rejection's wrong-reason branch must then FAIL even
# though the tx is still correctly rejected on-chain.
expect_cert_rejection "$NAME" "$SIGNED" "StakePoolNotRegisteredOnKeyPOOL"
