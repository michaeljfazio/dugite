#!/usr/bin/env bash
# 01e — tx with both lower and upper validity bounds (--invalid-before / --invalid-hereafter).
# The interval is chosen tight around the current tip slot so the tx is
# accepted but the validity-window logic is exercised.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_fail "no UTxO"; zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}

TIP=$(zoo_tip_slot)
LOWER=$((TIP > 5 ? TIP - 5 : 0))
UPPER=$((TIP + 3600))   # 1h window
zoo_info "validity-interval: tip=$TIP lower=$LOWER upper=$UPPER"

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-out        "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --invalid-before    "$LOWER" \
    --invalid-hereafter "$UPPER" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" || zoo_record "$NAME" FAIL "$TXID" "not-included"
