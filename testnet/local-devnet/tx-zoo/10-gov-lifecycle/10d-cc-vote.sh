#!/usr/bin/env bash
# 10d — Gov lifecycle: Constitutional Committee (cc-2) votes YES on
# the proposal from 10a. cc-2's hot key was authorized by
# 05g-cc-hot-key-authorization.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

GOV_STATE="$ZOO_STATE/gov-lifecycle"
ACTION_ID_FILE="$GOV_STATE/proposal.actionid"
[ -f "$ACTION_ID_FILE" ] || {
    zoo_record "$NAME" SKIP "" "no-proposal-actionid (run 10a first)"
    exit 0
}
ACTION_ID=$(cat "$ACTION_ID_FILE")
ACTION_TXID="${ACTION_ID%#*}"
ACTION_IDX="${ACTION_ID##*#}"

CC_HOT_SKEY="$LD_KEYS/cc-2/cc-hot.skey"
CC_HOT_VKEY="$LD_KEYS/cc-2/cc-hot.vkey"
[ -f "$CC_HOT_SKEY" ] || {
    zoo_record "$NAME" SKIP "" "no-cc-2-hot-key"
    exit 0
}

WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")

VOTE_FILE="$ZOO_BUILT/$NAME.vote"
cardano-cli conway governance vote create \
    --yes \
    --governance-action-tx-id      "$ACTION_TXID" \
    --governance-action-index      "$ACTION_IDX" \
    --cc-hot-verification-key-file "$CC_HOT_VKEY" \
    --anchor-url                   "$(zoo_anchor_url cc-vote)" \
    --anchor-data-hash             "$(zoo_anchor_hash cc-vote)" \
    --out-file                     "$VOTE_FILE"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic  "$LD_MAGIC" \
    --socket-path    "$ZOO_SOCKET" \
    --tx-in          "$TXIN" \
    --change-address "$ADDR" \
    --vote-file      "$VOTE_FILE" \
    --out-file       "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic    "$LD_MAGIC" \
    --tx-body-file     "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$CC_HOT_SKEY" \
    --out-file         "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" \
    || { zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1; }

touch "$GOV_STATE/cc-voted"
zoo_record "$NAME" PASS "$TXID" "cc-2 YES on $ACTION_TXID#$ACTION_IDX"
