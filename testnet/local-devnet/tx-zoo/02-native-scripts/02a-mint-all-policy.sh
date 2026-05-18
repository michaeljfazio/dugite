#!/usr/bin/env bash
# 02a — mint native tokens under an "all-of" policy with one signer key.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_fail "no UTxO"; zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}

KH=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_PAY_VKEY")
POLICY="$ZOO_BUILT/$NAME.policy.json"
cat > "$POLICY" <<EOF
{
  "type": "all",
  "scripts": [ { "type": "sig", "keyHash": "$KH" } ]
}
EOF
POLICY_ID=$(cardano-cli conway transaction policyid --script-file "$POLICY")
ASSET_NAME_HEX="$(printf 'TXZOOALL' | xxd -p | tr -d '\n')"
QUANTITY=100
ASSET="${POLICY_ID}.${ASSET_NAME_HEX}"

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-out        "${ADDR}+2000000 + ${QUANTITY} ${ASSET}" \
    --change-address "$ADDR" \
    --mint          "${QUANTITY} ${ASSET}" \
    --mint-script-file "$POLICY" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "policy=${POLICY_ID:0:16}" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
