#!/usr/bin/env bash
# 02c — mint native tokens under a k-of-n "atLeast" policy. Three keys
# listed, two required. We sign with two.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
[ -s "$ZOO_KEYS/wallet-a/payment.vkey" ] || die "wallet-a missing — run setup"
[ -s "$ZOO_KEYS/wallet-b/payment.vkey" ] || die "wallet-b missing — run setup"
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_fail "no UTxO"; zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}

KH0=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_PAY_VKEY")
KHA=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_KEYS/wallet-a/payment.vkey")
KHB=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_KEYS/wallet-b/payment.vkey")
POLICY="$ZOO_BUILT/$NAME.policy.json"
cat > "$POLICY" <<EOF
{
  "type": "atLeast",
  "required": 2,
  "scripts": [
    { "type": "sig", "keyHash": "$KH0" },
    { "type": "sig", "keyHash": "$KHA" },
    { "type": "sig", "keyHash": "$KHB" }
  ]
}
EOF
POLICY_ID=$(cardano-cli conway transaction policyid --script-file "$POLICY")
ASSET_NAME_HEX="$(printf 'TXZOOKOFN' | xxd -p | tr -d '\n')"
QUANTITY=7
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
# Sign with the genesis utxo key + wallet-a — 2 of 3.
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --signing-key-file "$ZOO_KEYS/wallet-a/payment.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "policy=${POLICY_ID:0:16}" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
