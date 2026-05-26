#!/usr/bin/env bash
# 10a — Gov lifecycle: propose a ParameterChange governance action.
# This starts the end-to-end governance test: propose → vote → ratify → enact.
# The proposal asks to set minFeeA to a new value (non-breaking change).
#
# On success, writes $ZOO_STATE/gov-lifecycle/proposal.actionid and
# expected-min-fee-a so downstream 10b/10c/10d/10e scripts can vote on the
# action and assert its enactment.
#
# On re-run (Round 2+): reads the previously enacted action ID from
# $ZOO_STATE/gov-lifecycle/enacted.actionid and supplies it as
# --prev-governance-action-tx-id.  Per Conway/CIP-1694, once a ParameterChange
# proposal has been enacted, all subsequent ParameterChange proposals must
# reference the most recently enacted one as their prev_action_id (the "lineal
# chain" invariant enforced by proposalsAddAction in cardano-ledger Proposals.hs).
# Without it the proposal is rejected at submission (InvalidPrevGovActionId).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")

PPARAMS=$(zoo_pparams_file)
GOV_DEPOSIT=$(jq -r '.govActionDeposit // 100000000000' "$PPARAMS")
CURRENT_MIN_FEE_A=$(jq -r '.txFeePerByte // .minFeeA // 44' "$PPARAMS")
NEW_MIN_FEE_A=$((CURRENT_MIN_FEE_A + 1))

GOV_STATE="$ZOO_STATE/gov-lifecycle"
mkdir -p "$GOV_STATE"

# Build prev-action-id args.  On first run there is no enacted action, so no
# --prev-governance-action-tx-id flag is needed.  On subsequent runs we must
# supply the previously enacted action ID so the new proposal correctly chains
# from the enacted root of the ParameterChange purpose tree.
PREV_ACTION_ARGS=()
ENACTED_ACTIONID_FILE="$GOV_STATE/enacted.actionid"
if [ -f "$ENACTED_ACTIONID_FILE" ]; then
    PREV_ACTIONID=$(cat "$ENACTED_ACTIONID_FILE")
    PREV_TX="${PREV_ACTIONID%%#*}"
    PREV_IDX="${PREV_ACTIONID##*#}"
    log_info "10a: chaining from previously enacted action ${PREV_TX}#${PREV_IDX}"
    PREV_ACTION_ARGS=(
        --prev-governance-action-tx-id "$PREV_TX"
        --prev-governance-action-index "$PREV_IDX"
    )
fi

ACTION="$ZOO_BUILT/$NAME.action"
cardano-cli conway governance action create-protocol-parameters-update \
    --testnet \
    --governance-action-deposit "$GOV_DEPOSIT" \
    --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
    --anchor-url  "$(zoo_anchor_url gov-proposal)" \
    --anchor-data-hash "$(zoo_anchor_hash gov-proposal)" \
    --min-fee-linear "$NEW_MIN_FEE_A" \
    "${PREV_ACTION_ARGS[@]}" \
    --out-file "$ACTION"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --change-address "$ADDR" \
    --proposal-file "$ACTION" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" \
    || { zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1; }

echo "${TXID}#0" > "$GOV_STATE/proposal.actionid"
echo "$NEW_MIN_FEE_A" > "$GOV_STATE/expected-min-fee-a"
zoo_record "$NAME" PASS "$TXID" "minFeeA=${CURRENT_MIN_FEE_A}->${NEW_MIN_FEE_A}"
