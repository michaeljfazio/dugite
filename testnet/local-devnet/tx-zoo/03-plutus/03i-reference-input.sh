#!/usr/bin/env bash
# 03i — Reference inputs (CIP-31). The tx reads a UTxO without consuming it.
# Demonstration: a normal payment tx that declares a reference input — the
# wire shape exercise is enough since the always-true validator doesn't use
# the ref input.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
PAY_UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
PAY_IN=${PAY_UTXO%% *}
# A second UTxO acts as the reference input — it stays at the addr.
REF_UTXO=$(zoo_utxo_at "$ADDR" 1) || { zoo_record "$NAME" FAIL "" "no-second-utxo"; exit 1; }
REF_IN=${REF_UTXO%% *}

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$PAY_IN" \
    --read-only-tx-in-reference "$REF_IN" \
    --tx-out        "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "ref=${REF_IN}" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
