#!/usr/bin/env bash
# 16a — register a stake key that is already registered.
# Expect: StakeKeyRegisteredDELEG (ConwayDelegPredFailure tag 2), reached as
#         Ledger 2 -> CERTS 1 -> CERT 1 -> DELEG 2.
#
# Before #979 this reached cardano-cli as ConwayMempoolFailure.
#
# #1060 — TWO FIXTURE DEFECTS, both of which made this case assert something it
# was not testing. Neither was a node bug: dugite's reported reason was correct
# and cardano-node reports the same one.
#
# 1. THE TX WAS NOT WITNESSED. `--key-reg-deposit-amt` makes cardano-cli emit the
#    Conway `reg_cert` (index 7), and that variant REQUIRES a vkey witness from
#    the stake credential. Oracle-verified against cardano-ledger
#    `Conway/TxCert.hs::getVKeyWitnessConwayTxCert`:
#
#      ConwayRegCert _ (SJust _)  -> credKeyHashWitness  -- index 7, WITNESS REQUIRED
#      ConwayRegCert _ SNothing   -> Nothing             -- index 0, permissionless
#
#    with the source comment noting the exemption applies "only during the
#    transitional period of Conway era and only for staking credential
#    registration certificates without a deposit". The script signed with
#    `payment.skey` alone, so the tx was genuinely missing a required witness and
#    `MissingVKeyWitnessesUTXOW` was the RIGHT answer — cardano-node would say the
#    same. See also #965: only the deposit-less reg_cert is permissionless.
#
# 2. IT BORROWED ITS PRECONDITION FROM ANOTHER CATEGORY. "Already registered"
#    depended on 05-governance-certs having run first, which is why
#    `run-all.sh`'s ALL_CATEGORIES carries an ordering comment. A negative case
#    whose precondition is established elsewhere silently stops testing its own
#    predicate the moment that ordering changes — and cannot be run standalone at
#    all. It now establishes its own state: register, wait for inclusion, THEN
#    re-register.
#
# The generalisable half: when a negative case reports the wrong REASON, check
# whether the transaction is well-formed for the case before suspecting the node.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/16-cert-negatives/_cert-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
[ -s "$WA/stake.vkey" ] || { zoo_record_env_skip "$NAME" "wallet-a stake key missing"; exit 0; }
[ -s "$WA/stake.skey" ] || {
    # The index-7 cert cannot be witnessed without it, so the case would only
    # ever be able to observe a missing-witness failure.
    zoo_record_env_skip "$NAME" "wallet-a stake SIGNING key missing (index-7 reg_cert needs its witness)"
    exit 0
}

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit // .keyDeposit // 2000000' "$PPARAMS")

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address registration-certificate \
    --stake-verification-key-file "$WA/stake.vkey" \
    --key-reg-deposit-amt "$DEPOSIT" \
    --out-file "$CERT" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "cert-create"; exit 1; }

# ── establish the precondition ourselves ──────────────────────────────────
#
# If the key is not registered yet, register it and wait for the tx to land.
# `stake-address-info` returns an empty array for an unregistered credential.
REG_INFO=$(cardano-cli conway query stake-address-info \
             --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
             --address "$(cat "$WA/stake.addr")" --output-json 2>/dev/null || echo '[]')
ALREADY=$(echo "$REG_INFO" | jq 'length' 2>/dev/null || echo 0)

if [ "${ALREADY:-0}" -lt 1 ]; then
    zoo_info "$NAME: stake key not registered yet — registering it first so the re-registration below is the thing under test"
    SETUP=$(cert_build_signed "$NAME-setup" "$ADDR" "$WA/payment.skey" "$WA/stake.skey" -- \
                --certificate-file "$CERT") || {
        zoo_fail "$NAME: could not build the first-time registration: $(grep -m1 Error "$ZOO_LOGS/$NAME-setup.err" 2>/dev/null | cut -c1-140)"
        zoo_record "$NAME" FAIL "" "setup-build-failed"; exit 1
    }
    TXID=$(zoo_submit "$SETUP") || {
        zoo_fail "$NAME: the FIRST registration was rejected — the precondition cannot be established"
        zoo_record "$NAME" FAIL "" "setup-submit-rejected"; exit 1
    }
    zoo_wait_inclusion "$TXID" 90 "$ADDR" || {
        zoo_fail "$NAME: the first registration never landed, so 'already registered' is unproven"
        zoo_record "$NAME" FAIL "" "setup-not-included"; exit 1
    }
fi

# ── the case: re-register the same key, correctly witnessed ────────────────
#
# Signed with BOTH keys, so a missing witness cannot mask the DELEG failure.
SIGNED=$(cert_build_signed "$NAME" "$ADDR" "$WA/payment.skey" "$WA/stake.skey" -- \
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
