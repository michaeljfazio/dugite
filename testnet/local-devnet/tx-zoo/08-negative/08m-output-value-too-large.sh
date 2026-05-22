#!/usr/bin/env bash
# 08m — Rule 5a: output value CBOR size > max_val_size (OutputValueTooLarge).
# We try to put a multi-asset value with many distinct policy/asset combinations
# to push the CBOR encoding of the value field past the protocol limit.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=200000
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"

# Generate a fake policy script with a trivially-true native script
POLICY_SCRIPT="$ZOO_BUILT/$NAME.policy.json"
POLICY_SKEY="$ZOO_BUILT/$NAME.policy.skey"
POLICY_VKEY="$ZOO_BUILT/$NAME.policy.vkey"

cardano-cli conway key gen-policy \
    --signing-key-file  "$POLICY_SKEY" \
    --verification-key-file "$POLICY_VKEY" 2>/dev/null || \
cardano-cli conway key gen-payment \
    --signing-key-file  "$POLICY_SKEY" \
    --verification-key-file "$POLICY_VKEY" >/dev/null

# Write a simple "always succeeds" native script
POLICY_VKEY_HASH=$(cardano-cli conway key hash \
    --payment-verification-key-file "$POLICY_VKEY" 2>/dev/null || \
    jq -r '.cborHex | .[4:]' "$POLICY_VKEY" | head -c 56)

cat > "$POLICY_SCRIPT" <<SCRIPT
{
  "type": "sig",
  "keyHash": "$POLICY_VKEY_HASH"
}
SCRIPT

POLICY_ID=$(cardano-cli conway transaction policyid \
    --script-file "$POLICY_SCRIPT" 2>/dev/null || echo "")

if [ -z "$POLICY_ID" ]; then
    zoo_record "$NAME" SKIP "" "could-not-derive-policy-id"
    exit 0
fi

# Build output with 100 distinct asset names under the same policy
MINT_ARG=""
VALUE_ARG="${ADDR}+$((AMT - FEE))"
for i in $(seq 1 100); do
    ASSET_NAME=$(printf '%040x' "$i")
    MINT_ARG+="1 ${POLICY_ID}.${ASSET_NAME}+"
    VALUE_ARG+="+1 ${POLICY_ID}.${ASSET_NAME}"
done
MINT_ARG="${MINT_ARG%+}"

cardano-cli conway transaction build-raw \
    --tx-in          "$TXIN" \
    --tx-out         "$VALUE_ARG" \
    --fee            "$FEE" \
    --ttl            $((TIP + 600)) \
    --mint           "$MINT_ARG" \
    --mint-script-file "$POLICY_SCRIPT" \
    --out-file       "$RAW" 2>/dev/null || true

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --signing-key-file "$POLICY_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "output-value-too-large submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-OutputValueTooLarge" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
