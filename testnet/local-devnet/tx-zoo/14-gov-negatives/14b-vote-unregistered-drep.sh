#!/usr/bin/env bash
# 14b — vote from a DRep credential that was never registered.
# Expect: VotersDoNotExist (ConwayGovPredFailure tag 14).
#
# `internVoter` does a Map.lookup against vsDReps and misses. Note this is the
# SAME constructor for a bogus StakePool or CommitteeVoter.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/14-gov-negatives/_gov-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")

ACTION_FILE="$ZOO_BUILT/gov-action-info.id"
[ -s "$ACTION_FILE" ] || { zoo_skip "no InfoAction (06a first)"; zoo_record "$NAME" SKIP "" "no-gov-action"; exit 0; }
ACTION_ID=$(cat "$ACTION_FILE"); ACTION_TX="${ACTION_ID%#*}"; ACTION_IX="${ACTION_ID#*#}"

# A fresh DRep key that is deliberately never registered on-chain.
UNREG="$ZOO_BUILT/$NAME-unreg"
mkdir -p "$UNREG"
[ -s "$UNREG/drep.vkey" ] || cardano-cli conway governance drep key-gen \
    --verification-key-file "$UNREG/drep.vkey" --signing-key-file "$UNREG/drep.skey" >/dev/null

VOTE="$ZOO_BUILT/$NAME.vote"
cardano-cli conway governance vote create --yes \
    --governance-action-tx-id "$ACTION_TX" --governance-action-index "$ACTION_IX" \
    --drep-verification-key-file "$UNREG/drep.vkey" \
    --out-file "$VOTE" 2>/dev/null || { zoo_record "$NAME" FAIL "" "vote-create"; exit 1; }

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${UTXO%% *}" --change-address "$ADDR" \
        --vote-file "$VOTE" --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
    if grep -qiE "VotersDoNotExist|not registered|does not exist" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (unregistered DRep)"
        zoo_record "$NAME" PASS "" "rejected-VotersDoNotExist-at-build"
        exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
fi
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$UNREG/drep.skey" --out-file "$SIGNED" >/dev/null
expect_gov_rejection "$NAME" "$SIGNED" "VotersDoNotExist"
