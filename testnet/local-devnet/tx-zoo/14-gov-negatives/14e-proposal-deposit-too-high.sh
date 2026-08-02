#!/usr/bin/env bash
# 14e — proposal whose deposit is ABOVE govActionDeposit.
# Expect: ProposalDepositIncorrect — the SAME constructor as 14d.
#
# This is the counter-intuitive one and the reason it is a separate test: the
# check is `pProcDeposit == expectedDeposit`, exact equality
# (`Mismatch RelEQ Coin`), not a floor. Over-depositing is rejected just as
# firmly as under-depositing. A node that treated the parameter as a minimum
# would pass 14d and fail only here.
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
BAD=$(( GOV_DEPOSIT + 1000000 ))

ACTION="$ZOO_BUILT/$NAME.action"
cardano-cli conway governance action create-info \
    --testnet --governance-action-deposit "$BAD" \
    --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
    --anchor-url "$(zoo_anchor_url gov-proposal)" \
    --anchor-data-hash "$(zoo_anchor_hash gov-proposal)" \
    --out-file "$ACTION" 2>/dev/null || { zoo_record "$NAME" FAIL "" "action-create"; exit 1; }

SIGNED=$(gov_build_signed "$NAME" "$ADDR" "$WA/payment.skey" --proposal-file "$ACTION") || {
    if grep -qiE "ProposalDepositIncorrect|deposit" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (deposit above govActionDeposit — equality, not a floor)"
        zoo_record "$NAME" PASS "" "rejected-ProposalDepositIncorrect-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}
expect_gov_rejection "$NAME" "$SIGNED" "ProposalDepositIncorrect" "over-deposit=$BAD want=$GOV_DEPOSIT"
