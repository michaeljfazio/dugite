#!/usr/bin/env bash
# 02e — burn the tokens minted by 02a (or any "all"-policy asset present at
# the genesis addr). Reads the latest TXZOOALL holding and burns half of it.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

# Re-derive the 02a policy id (same key, same script shape).
KH=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_PAY_VKEY")
POLICY="$ZOO_BUILT/02a-mint-all-policy.policy.json"
if [ ! -s "$POLICY" ]; then
    POLICY="$ZOO_BUILT/$NAME.policy.json"
    cat > "$POLICY" <<EOF
{
  "type": "all",
  "scripts": [ { "type": "sig", "keyHash": "$KH" } ]
}
EOF
fi
POLICY_ID=$(cardano-cli conway transaction policyid --script-file "$POLICY")
ASSET_NAME_HEX="$(printf 'TXZOOALL' | xxd -p | tr -d '\n')"
ASSET="${POLICY_ID}.${ASSET_NAME_HEX}"

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
TMP=$(mktemp)
cardano-cli conway query utxo \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --address       "$ADDR" \
    --out-file      "$TMP"
# Find a UTxO that has the TXZOOALL asset; pick the largest balance.
TXIN=$(jq -r --arg p "$POLICY_ID" --arg a "$ASSET_NAME_HEX" '
    to_entries
    | map(select(.value.value[$p][$a] != null))
    | sort_by(-.value.value[$p][$a])
    | .[0].key' "$TMP")
QTY=$(jq -r --arg p "$POLICY_ID" --arg a "$ASSET_NAME_HEX" --arg k "$TXIN" '
    .[$k].value[$p][$a] // 0' "$TMP")
rm -f "$TMP"
if [ -z "$TXIN" ] || [ "$TXIN" = "null" ] || [ "${QTY:-0}" -le 0 ]; then
    zoo_skip "no TXZOOALL holdings to burn (run 02a first)"
    zoo_record "$NAME" SKIP "" "no-asset"
    exit 0
fi
BURN=$((QTY / 2))
[ "$BURN" -lt 1 ] && BURN=1
REMAIN=$((QTY - BURN))

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
TX_OUT="${ADDR}+2000000"
[ "$REMAIN" -gt 0 ] && TX_OUT="${TX_OUT} + ${REMAIN} ${ASSET}"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-out        "$TX_OUT" \
    --change-address "$ADDR" \
    --mint          "-${BURN} ${ASSET}" \
    --mint-script-file "$POLICY" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "burned=${BURN}" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
