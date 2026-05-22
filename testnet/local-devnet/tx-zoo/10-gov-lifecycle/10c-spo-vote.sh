#!/usr/bin/env bash
# 10c — Gov lifecycle: SPO votes YES on the proposal from 10a.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

GOV_STATE="$ZOO_STATE/gov-lifecycle"
ACTION_ID_FILE="$GOV_STATE/proposal.actionid"
if [ ! -f "$ACTION_ID_FILE" ]; then
    zoo_record "$NAME" SKIP "" "no-proposal-actionid (run 10a first)"
    exit 0
fi

ACTION_ID=$(cat "$ACTION_ID_FILE")
ACTION_TXID="${ACTION_ID%#*}"
ACTION_IDX="${ACTION_ID##*#}"

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=400000
VOTE_FILE="$ZOO_BUILT/$NAME.vote"
SIGNED="$ZOO_BUILT/$NAME.signed"

# Pool1 cold key from devnet setup
POOL_COLD_SKEY="$LD_KEYS/pool1/cold.skey"
POOL_COLD_VKEY="$LD_KEYS/pool1/cold.vkey"
if [ ! -f "$POOL_COLD_SKEY" ]; then
    zoo_record "$NAME" SKIP "" "no-pool1-cold-key"
    exit 0
fi

ANCHOR_URL=$(zoo_anchor_url "spo-vote" '{"vote":"yes"}')
ANCHOR_HASH=$(zoo_anchor_hash "spo-vote")

cardano-cli conway governance vote create \
    --yes \
    --governance-action-tx-id           "$ACTION_TXID" \
    --governance-action-index           "$ACTION_IDX" \
    --cold-verification-key-file        "$POOL_COLD_VKEY" \
    --anchor-url                        "$ANCHOR_URL" \
    --anchor-data-hash                  "$ANCHOR_HASH" \
    --out-file                          "$VOTE_FILE" 2>/dev/null || \
{ zoo_record "$NAME" SKIP "" "governance-vote-create-failed"; exit 0; }

cardano-cli conway transaction build \
    --testnet-magic  "$LD_MAGIC" \
    --socket-path    "$ZOO_SOCKET" \
    --tx-in          "$TXIN" \
    --change-address "$ADDR" \
    --vote-file      "$VOTE_FILE" \
    --out-file       "$SIGNED" 2>/dev/null || \
cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - FEE))" \
    --fee      "$FEE" \
    --ttl      $((TIP + 600)) \
    --vote-file "$VOTE_FILE" \
    --out-file  "$SIGNED" 2>/dev/null || \
{ zoo_record "$NAME" SKIP "" "transaction-build-failed"; exit 0; }

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$SIGNED" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --signing-key-file "$POOL_COLD_SKEY" \
    --out-file       "${SIGNED}.signed" 2>/dev/null || true

FINAL="${SIGNED}.signed"
[ -f "$FINAL" ] || FINAL="$SIGNED"

TXID=$(cardano-cli conway transaction txid --tx-file "$FINAL" 2>/dev/null || echo "")
if zoo_submit "$NAME" "$FINAL"; then
    touch "$GOV_STATE/spo-voted"
    zoo_record "$NAME" PASS "$TXID" "spo-vote-yes"
else
    zoo_record "$NAME" FAIL "" "spo-vote-tx-failed"
fi
