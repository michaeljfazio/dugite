#!/usr/bin/env bash
# 06f — UpdateCommittee proposal: add cc-1 as a committee member with a
# 10-epoch term, and set the threshold to 1/2.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
CC="$ZOO_KEYS/cc-1"
ADDR=$(cat "$WA/payment-stake.addr")
PPARAMS=$(zoo_pparams_file)
GOV_DEPOSIT=$(jq -r '.govActionDeposit // 100000000000' "$PPARAMS")
CURRENT_EPOCH=$(zoo_tip_epoch)
EXPIRES_AT=$((CURRENT_EPOCH + 10))

ACTION="$ZOO_BUILT/$NAME.action"
cardano-cli conway governance action update-committee \
    --testnet \
    --governance-action-deposit "$GOV_DEPOSIT" \
    --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
    --anchor-url  "$(zoo_anchor_url update-committee)" \
    --anchor-data-hash "$(zoo_anchor_hash update-committee)" \
    --add-cc-cold-verification-key-file "$CC/cc-cold.vkey" \
    --epoch "$EXPIRES_AT" \
    --threshold "1/2" \
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
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "expires_at=$EXPIRES_AT threshold=1/2" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
