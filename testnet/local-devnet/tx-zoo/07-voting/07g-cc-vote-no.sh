#!/usr/bin/env bash
# 07g — CC hot-key (cc-1) votes NO on the InfoAction. Same auth caveat as 07f.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/07-voting/_vote-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ACTION=$(zoo_gov_action_id) || { zoo_skip "no action"; zoo_record "$NAME" SKIP; exit 0; }
CC="$ZOO_KEYS/cc-1"
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")

VOTE="$ZOO_BUILT/$NAME.vote"
zoo_vote_file cc-hot no "$CC/cc-hot.vkey" "$VOTE" || {
    zoo_skip "vote-file failed"
    zoo_record "$NAME" SKIP "" "vote-file"
    exit 0
}

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-in         "$TXIN" \
        --change-address "$ADDR" \
        --vote-file     "$VOTE" \
        --out-file      "$RAW" 2> "$ZOO_LOGS/$NAME.err" ; then
    zoo_skip "build rejected: $(tail -1 "$ZOO_LOGS/$NAME.err")"
    zoo_record "$NAME" SKIP "" "cc-not-authorized"
    exit 0
fi
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$CC/cc-hot.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "cc-hot NO" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
