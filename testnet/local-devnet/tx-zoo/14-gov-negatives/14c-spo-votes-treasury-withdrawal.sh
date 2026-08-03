#!/usr/bin/env bash
# 14c — an SPO votes on a TreasuryWithdrawals action.
# Expect: DisallowedVoters (ConwayGovPredFailure tag 5).
#
# This is a HARD phase-1 rejection (checkVotersAreValid), not merely a vote
# that goes uncounted at ratification — a distinction that matters for how the
# test is written. Per Governance/Internal.hs the SPO column is:
#   NoConfidence yes | UpdateCommittee yes | NewConstitution NO
#   HardForkInitiation yes | ParameterChange only-if-SecurityGroup
#   TreasuryWithdrawals NO | InfoAction yes
# DReps are never disallowed by action type, so "DRep votes NoConfidence" would
# be a BAD test case — it is legal.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/14-gov-negatives/_gov-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
[ -s "$LD_KEYS/pool1/cold.vkey" ] || { zoo_record_env_skip "$NAME" "pool1 cold key missing"; exit 0; }

# Find a live TreasuryWithdrawals action to vote on.
ACTION_ID=$(cardano-cli conway query gov-state --testnet-magic "$LD_MAGIC" \
              --socket-path "$ZOO_SOCKET" 2>/dev/null \
            | jq -r '[.proposals[]? | select(.proposalProcedure.govAction.tag=="TreasuryWithdrawals")][0]
                     | if . == null then empty
                       else "\(.actionId.txId)#\(.actionId.govActionIx)" end' 2>/dev/null)
if [ -z "$ACTION_ID" ]; then
    zoo_skip "no live TreasuryWithdrawals action (06d must run first and still be pending)"
    zoo_record "$NAME" SKIP "" "no-treasury-action"
    exit 0
fi
ACTION_TX="${ACTION_ID%#*}"; ACTION_IX="${ACTION_ID#*#}"

VOTE="$ZOO_BUILT/$NAME.vote"
cardano-cli conway governance vote create --yes \
    --governance-action-tx-id "$ACTION_TX" --governance-action-index "$ACTION_IX" \
    --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey" \
    --out-file "$VOTE" 2>/dev/null || { zoo_record "$NAME" FAIL "" "vote-create"; exit 1; }

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${UTXO%% *}" --change-address "$ADDR" \
        --vote-file "$VOTE" --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
    if grep -qiE "DisallowedVoters|not allowed to vote" "$ZOO_LOGS/$NAME.err"; then
        zoo_ok "refused at build (SPO may not vote on TreasuryWithdrawals)"
        zoo_record "$NAME" PASS "" "rejected-DisallowedVoters-at-build"; exit 0
    fi
    zoo_fail "build failed unexpectedly: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
fi
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$LD_KEYS/pool1/cold.skey" --out-file "$SIGNED" >/dev/null
expect_gov_rejection "$NAME" "$SIGNED" "DisallowedVoters" "SPO on TreasuryWithdrawals"
