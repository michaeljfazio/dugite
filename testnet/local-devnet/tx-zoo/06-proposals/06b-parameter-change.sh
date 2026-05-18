#!/usr/bin/env bash
# 06b — ParameterChange proposal: trivial bump of maxBlockBodySize.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
PPARAMS=$(zoo_pparams_file)
GOV_DEPOSIT=$(jq -r '.govActionDeposit // 100000000000' "$PPARAMS")
CURRENT_MAX_BODY=$(jq -r '.maxBlockBodySize' "$PPARAMS")
NEW_MAX_BODY=$((CURRENT_MAX_BODY + 1))

ACTION="$ZOO_BUILT/$NAME.action"
cardano-cli conway governance action create-protocol-parameters-update \
    --testnet \
    --governance-action-deposit "$GOV_DEPOSIT" \
    --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
    --anchor-url  "https://example.com/pparam-change.json" \
    --anchor-data-hash "0000000000000000000000000000000000000000000000000000000000000000" \
    --max-block-body-size "$NEW_MAX_BODY" \
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
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "maxBlockBodySize ${CURRENT_MAX_BODY}->${NEW_MAX_BODY}" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
