#!/usr/bin/env bash
# 14a — vote on a governance action that does not exist.
# Expect: GovActionsDoNotExist (ConwayGovPredFailure tag 0).
#
# Note the same constructor also covers voting on an action that has already
# been PRUNED from the proposals map (expired and swept at a boundary), so a
# rejection here is indistinguishable from "never proposed" — which is why 14h
# tests the still-present-but-expired case separately.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/14-gov-negatives/_gov-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
DREP="$ZOO_KEYS/drep-1"
[ -s "$DREP/drep.vkey" ] || { zoo_record_env_skip "$NAME" "drep-1 missing — run --setup"; exit 0; }

FAKE_TX="0000000000000000000000000000000000000000000000000000000000000001"
VOTE="$ZOO_BUILT/$NAME.vote"
cardano-cli conway governance vote create --yes \
    --governance-action-tx-id "$FAKE_TX" --governance-action-index 0 \
    --drep-verification-key-file "$DREP/drep.vkey" \
    --out-file "$VOTE" 2>/dev/null \
    || { zoo_record "$NAME" FAIL "" "vote-create"; exit 1; }

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${UTXO%% *}" --change-address "$ADDR" \
        --vote-file "$VOTE" --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
    if grep -qiE "GovActionsDoNotExist|does not exist|unknown governance action" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (action does not exist)"
        zoo_record "$NAME" PASS "" "rejected-GovActionsDoNotExist-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
fi
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$DREP/drep.skey" --out-file "$SIGNED" >/dev/null
expect_gov_rejection "$NAME" "$SIGNED" "GovActionsDoNotExist"
