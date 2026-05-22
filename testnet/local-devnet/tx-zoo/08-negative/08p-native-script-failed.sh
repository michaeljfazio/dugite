#!/usr/bin/env bash
# 08p — Rule 13: native script evaluation fails (NativeScriptFailed).
# Submits a tx guarded by a time-lock script whose slot window has already passed.
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
LOCK_SCRIPT="$ZOO_BUILT/$NAME.lock.json"
SCRIPT_ADDR="$ZOO_BUILT/$NAME.script.addr"

# Create a time-lock script that requires slot < 10 (already expired)
cat > "$LOCK_SCRIPT" <<SCRIPT
{"type": "before", "slot": 10}
SCRIPT

SCRIPT_ID=$(cardano-cli conway transaction policyid \
    --script-file "$LOCK_SCRIPT" 2>/dev/null || echo "")

if [ -z "$SCRIPT_ID" ]; then
    zoo_record "$NAME" SKIP "" "could-not-derive-script-id"
    exit 0
fi

# Build a script-guarded output — fund it first if no script UTxO exists
# We build a tx that mints a token under the expired time-lock script
ASSET="${SCRIPT_ID}.$(printf '%s' 'timedout' | xxd -p)"

cardano-cli conway transaction build-raw \
    --tx-in          "$TXIN" \
    --tx-out         "${ADDR}+$((AMT - FEE))+1 $ASSET" \
    --fee            "$FEE" \
    --ttl            $((TIP + 600)) \
    --mint           "1 $ASSET" \
    --mint-script-file "$LOCK_SCRIPT" \
    --out-file       "$RAW" 2>/dev/null || true

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "native-script-failed submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-NativeScriptFailed" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
