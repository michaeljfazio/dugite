#!/usr/bin/env bash
# 03f — Plutus V3 mint policy.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v3.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }

POLICY_ID=$(cardano-cli conway transaction policyid --script-file "$SCRIPT")
ASSET_NAME_HEX="$(printf 'TXZOOPV3' | xxd -p | tr -d '\n')"
QUANTITY=55
ASSET="${POLICY_ID}.${ASSET_NAME_HEX}"
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-in-collateral "$COLLAT" \
    --tx-out        "${ADDR}+2000000 + ${QUANTITY} ${ASSET}" \
    --change-address "$ADDR" \
    --mint          "${QUANTITY} ${ASSET}" \
    --mint-script-file "$SCRIPT" \
    --mint-redeemer-file "$REDEEMER" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 90 && zoo_record "$NAME" PASS "$TXID" "policy=${POLICY_ID:0:16}" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
