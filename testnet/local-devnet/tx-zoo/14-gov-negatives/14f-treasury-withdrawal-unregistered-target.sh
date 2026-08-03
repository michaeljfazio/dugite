#!/usr/bin/env bash
# 14f — TreasuryWithdrawals targeting an UNREGISTERED reward account.
# Expect: TreasuryWithdrawalReturnAccountsDoNotExist (tag 17).
#
# The target must be registered at PROPOSAL SUBMISSION time, not merely at
# enactment. Contrast with the amount: a withdrawal EXCEEDING the treasury is
# NOT rejected at submission at all (it soft-fails each RATIFY pass until it
# expires), which is why there is no "over-withdraw" negative in this category.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/14-gov-negatives/_gov-neg-helper.sh"

# Anchors are a fixed seeded set (_zoo_anchor_seed); an unseeded name aborts
# with "anchor file missing". These negatives are about the deposit amount /
# target registration, so the anchor content is irrelevant — reuse a seeded one.
NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
PPARAMS=$(zoo_pparams_file)
GOV_DEPOSIT=$(jq -r '.govActionDeposit // 100000000000' "$PPARAMS")

# A stake key generated here and deliberately never registered.
UNREG="$ZOO_BUILT/$NAME-unreg"; mkdir -p "$UNREG"
[ -s "$UNREG/stake.vkey" ] || cardano-cli conway stake-address key-gen \
    --verification-key-file "$UNREG/stake.vkey" --signing-key-file "$UNREG/stake.skey" >/dev/null

ACTION="$ZOO_BUILT/$NAME.action"
cardano-cli conway governance action create-treasury-withdrawal \
    --testnet --governance-action-deposit "$GOV_DEPOSIT" \
    --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
    --anchor-url "$(zoo_anchor_url treasury)" \
    --anchor-data-hash "$(zoo_anchor_hash treasury)" \
    --funds-receiving-stake-verification-key-file "$UNREG/stake.vkey" \
    --transfer 1000000 \
    --out-file "$ACTION" 2>/dev/null || { zoo_record "$NAME" FAIL "" "action-create"; exit 1; }

SIGNED=$(gov_build_signed "$NAME" "$ADDR" "$WA/payment.skey" --proposal-file "$ACTION") || {
    if grep -qiE "TreasuryWithdrawalReturnAccountsDoNotExist|not registered" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (withdrawal target not registered)"
        zoo_record "$NAME" PASS "" "rejected-TreasuryWithdrawalReturnAccountsDoNotExist-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}
expect_gov_rejection "$NAME" "$SIGNED" "TreasuryWithdrawalReturnAccountsDoNotExist"
