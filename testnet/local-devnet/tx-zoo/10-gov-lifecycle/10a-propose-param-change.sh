#!/usr/bin/env bash
# 10a — Gov lifecycle: propose a ParameterChange governance action.
# This starts the end-to-end governance test: propose → vote → ratify → enact.
# The proposal asks to set minFeeA to a new value (non-breaking change).
#
# On success, writes $ZOO_STATE/gov-lifecycle/proposal.actionid and
# expected-min-fee-a so downstream 10b/10c/10d/10e scripts can vote on the
# action and assert its enactment.
#
# prev_action_id resolution (lineal chain invariant):
#   Per Conway/CIP-1694 and Haskell `proposalsAddAction`, a ParameterChange
#   proposal's prev_action_id must reference the most recently enacted
#   ParameterChange on the current chain — NOT a stale ID from a previous
#   devnet boot.  We query the live gov-state from the chain to get the
#   canonical enacted pparam root, ignoring any persisted cross-round file.
#   If no ParameterChange has been enacted on this chain yet, no
#   --prev-governance-action-tx-id flag is needed (genesis root).
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

# Query the live chain for the currently enacted ParameterChange root.
# Haskell's `proposalsAddAction` checks `Map.member parentId graph` where the
# enacted root is the anchor of the purpose tree.  We extract it from
# `enactState.prevGovActionIds.PParamUpdate` in the gov-state JSON.
# This is robust against cross-round state pollution: if this is a fresh
# chain (new devnet boot), PParamUpdate will be null and we omit the flag.
PREV_ACTION_ARGS=()
GOV_STATE_JSON=$(cardano-cli conway query gov-state \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" 2>/dev/null || true)

if [ -n "$GOV_STATE_JSON" ]; then
    # Try to extract prevGovActionIds.PParamUpdate from enactState.
    # cardano-cli gov-state JSON shape: .enactState.prevGovActionIds.PParamUpdate
    # which is either null or {"txId": "...", "govActionIx": N}
    ENACTED_RAW=$(echo "$GOV_STATE_JSON" | \
        jq -r '(.enactState.prevGovActionIds.PParamUpdate // null)' 2>/dev/null || true)
    if [ -n "$ENACTED_RAW" ] && [ "$ENACTED_RAW" != "null" ]; then
        PREV_TX=$(echo "$ENACTED_RAW" | jq -r '.txId')
        PREV_IDX=$(echo "$ENACTED_RAW" | jq -r '.govActionIx')
        if [ -n "$PREV_TX" ] && [ "$PREV_TX" != "null" ]; then
            log_info "10a: chaining from enacted ParameterChange on this chain: ${PREV_TX}#${PREV_IDX}"
            PREV_ACTION_ARGS=(
                --prev-governance-action-tx-id "$PREV_TX"
                --prev-governance-action-index "$PREV_IDX"
            )
        fi
    else
        log_info "10a: no enacted ParameterChange on this chain — submitting genesis-root proposal"
    fi
else
    log_info "10a: gov-state query failed (node may be starting) — submitting without prev_action_id"
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
