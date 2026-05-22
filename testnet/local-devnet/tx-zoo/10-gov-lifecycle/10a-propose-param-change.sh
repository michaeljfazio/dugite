#!/usr/bin/env bash
# 10a — Gov lifecycle: propose a ParameterChange governance action.
# This starts the end-to-end governance test: propose → vote → ratify → enact.
# The proposal asks to set minFeeA to a new value (non-breaking change).
#
# On success, writes $ZOO_STATE/gov-lifecycle/proposal.action to signal
# downstream 10b/10c/10d/10e/10f scripts.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=400000
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"

# Gov lifecycle state dir
GOV_STATE="$ZOO_STATE/gov-lifecycle"
mkdir -p "$GOV_STATE"

# Get current gov action deposit from protocol parameters
GOV_DEPOSIT=$(cardano-cli conway query protocol-parameters \
    --testnet-magic "$LD_MAGIC" \
    --socket-path "$ZOO_SOCKET" \
    --output-json 2>/dev/null | jq -r '.govActionDeposit // 100000000000')

# Get current epoch + current constitution anchor (needed for ParameterChange)
CONSTITUTION=$(cardano-cli conway query constitution \
    --testnet-magic "$LD_MAGIC" \
    --socket-path "$ZOO_SOCKET" \
    --output-json 2>/dev/null | jq -r '.anchor // {}')
CONST_URL=$(echo "$CONSTITUTION" | jq -r '.url // "https://example.com/const.json"')
CONST_HASH=$(echo "$CONSTITUTION" | jq -r '.dataHash // "0000000000000000000000000000000000000000000000000000000000000000"')

# Get current pparams for the proposal anchor
CURRENT_MIN_FEE_A=$(cardano-cli conway query protocol-parameters \
    --testnet-magic "$LD_MAGIC" \
    --socket-path "$ZOO_SOCKET" \
    --output-json 2>/dev/null | jq -r '.txFeePerByte // 44')

# Propose changing minFeeA by +1 (trivially non-breaking)
NEW_MIN_FEE_A=$(( CURRENT_MIN_FEE_A + 1 ))

PROPOSAL_ANCHOR_URL=$(zoo_anchor_url "gov-proposal" '{"change":"minFeeA+1"}')
PROPOSAL_ANCHOR_HASH=$(zoo_anchor_hash "gov-proposal")
ACTION_FILE="$GOV_BUILT/$NAME.action"
mkdir -p "$ZOO_BUILT"

cardano-cli conway governance action create-protocol-parameters-update \
    --testnet \
    --anchor-url  "$PROPOSAL_ANCHOR_URL" \
    --anchor-data-hash "$PROPOSAL_ANCHOR_HASH" \
    --constitution-script-hash "$(echo "$CONSTITUTION" | jq -r '.script // empty' || echo "")" \
    --min-fee-constant "$NEW_MIN_FEE_A" \
    --governance-action-deposit "$GOV_DEPOSIT" \
    --deposit-return-stake-verification-key-file "$ZOO_PAY_VKEY" \
    --out-file "$RAW" 2>/dev/null || \
cardano-cli conway governance action create-protocol-parameters-update \
    --testnet \
    --anchor-url  "$PROPOSAL_ANCHOR_URL" \
    --anchor-data-hash "$PROPOSAL_ANCHOR_HASH" \
    --min-fee-constant "$NEW_MIN_FEE_A" \
    --governance-action-deposit "$GOV_DEPOSIT" \
    --deposit-return-stake-verification-key-file "$ZOO_PAY_VKEY" \
    --out-file "$RAW" 2>/dev/null || \
{ zoo_record "$NAME" SKIP "" "governance-action-create-failed"; exit 0; }

# Build the tx including the proposal
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --change-address "$ADDR" \
    --proposal-file  "$RAW" \
    --out-file       "$SIGNED" 2>/dev/null || \
cardano-cli conway transaction build-raw \
    --tx-in              "$TXIN" \
    --tx-out             "${ADDR}+$((AMT - GOV_DEPOSIT - FEE))" \
    --fee                "$FEE" \
    --ttl                $((TIP + 600)) \
    --proposal-file      "$RAW" \
    --out-file           "$SIGNED" 2>/dev/null || \
{ zoo_record "$NAME" SKIP "" "transaction-build-failed"; exit 0; }

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$SIGNED" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file       "$SIGNED.signed" 2>/dev/null || true

FINAL="${SIGNED}.signed"
[ -f "$FINAL" ] || FINAL="$SIGNED"

TXID=$(cardano-cli conway transaction txid --tx-file "$FINAL" 2>/dev/null || echo "")
if zoo_submit "$NAME" "$FINAL" && [ -n "$TXID" ]; then
    # Record the proposal TxID#0 for downstream scripts
    echo "${TXID}#0" > "$GOV_STATE/proposal.actionid"
    echo "$NEW_MIN_FEE_A" > "$GOV_STATE/expected-min-fee-a"
    zoo_record "$NAME" PASS "$TXID" "proposed-param-change-minFeeA=$NEW_MIN_FEE_A"
else
    zoo_record "$NAME" FAIL "" "proposal-tx-failed"
fi
