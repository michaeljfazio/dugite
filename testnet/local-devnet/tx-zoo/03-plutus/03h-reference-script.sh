#!/usr/bin/env bash
# 03h — Reference scripts (CIP-33). Step 1: pay to an addr with the V2
# always-true attached as a reference script. Step 2: lock funds at the
# V2 script addr. Step 3: spend, referring to the script via --tx-in-reference.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_skip "missing $SCRIPT"; zoo_record "$NAME" SKIP; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# Step 1: deposit the script.
DEP_UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
DEP_IN=${DEP_UTXO%% *}
DEP_RAW="$ZOO_BUILT/$NAME-deposit.raw"
DEP_SIGNED="$ZOO_BUILT/$NAME-deposit.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$DEP_IN" \
    --tx-out        "${ADDR}+30000000" \
    --tx-out-reference-script-file "$SCRIPT" \
    --change-address "$ADDR" \
    --out-file      "$DEP_RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$DEP_RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$DEP_SIGNED" >/dev/null
DEP_TXID=$(zoo_submit "$DEP_SIGNED") || { zoo_record "$NAME" FAIL "" "deposit"; exit 1; }
zoo_wait_inclusion "$DEP_TXID" 90 || { zoo_record "$NAME" FAIL "$DEP_TXID" "deposit-not-incl"; exit 1; }
# Find the ref-script output (the one with referenceScript set).
TMP=$(mktemp)
cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --address "$ADDR" --out-file "$TMP"
REF_TXIN=$(jq -r --arg t "$DEP_TXID" '
    to_entries
    | map(select(.key | startswith($t)))
    | map(select(.value.referenceScript != null))
    | .[0].key' "$TMP")
rm -f "$TMP"
[ -z "$REF_TXIN" ] && { zoo_fail "could not locate ref-script output"; zoo_record "$NAME" FAIL "$DEP_TXID" "no-ref-out"; exit 1; }

# Step 2: lock funds at the V2 script addr (inline datum).
PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}

# Step 3: spend using the reference script.
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$SCRIPT_TXIN" \
    --spending-tx-in-reference "$REF_TXIN" \
    --spending-plutus-script-v2 \
    --spending-reference-tx-in-inline-datum-present \
    --spending-reference-tx-in-redeemer-file "$REDEEMER" \
    --tx-in-collateral  "$COLLAT" \
    --tx-out        "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 90 && zoo_record "$NAME" PASS "$TXID" "ref_in=${REF_TXIN%#*}" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
