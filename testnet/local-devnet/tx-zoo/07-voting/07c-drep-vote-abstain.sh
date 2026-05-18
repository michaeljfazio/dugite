#!/usr/bin/env bash
# 07c — DRep (drep-1) votes ABSTAIN on the InfoAction (overwrites 07a's YES).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/07-voting/_vote-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ACTION=$(zoo_gov_action_id) || { zoo_skip "no action"; zoo_record "$NAME" SKIP; exit 0; }
DREP="$ZOO_KEYS/drep-1"
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")

VOTE="$ZOO_BUILT/$NAME.vote"
zoo_vote_file drep abstain "$DREP/drep.vkey" "$VOTE" || die "vote-file failed"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --change-address "$ADDR" \
    --vote-file     "$VOTE" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$DREP/drep.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "drep ABSTAIN" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
