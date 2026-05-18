#!/usr/bin/env bash
# 07b — DRep (drep-2) votes NO on the InfoAction.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/07-voting/_vote-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ACTION=$(zoo_gov_action_id) || { zoo_skip "no action"; zoo_record "$NAME" SKIP; exit 0; }
DREP="$ZOO_KEYS/drep-2"
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")

# Register drep-2 first if needed (drep-1 was reg'd by 05a; we want a second voter).
PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.dRepDeposit // .drepDeposit // 500000000' "$PPARAMS")
DREP_KH=$(cardano-cli conway governance drep id --drep-verification-key-file "$DREP/drep.vkey" --output-format hex)
REG_LIST=$(cardano-cli conway query drep-state \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --drep-key-hash "$DREP_KH" 2>/dev/null || echo "[]")
if ! echo "$REG_LIST" | jq -e 'length>0' >/dev/null; then
    REG_CERT="$ZOO_BUILT/$NAME-reg.cert"
    cardano-cli conway governance drep registration-certificate \
        --drep-verification-key-file "$DREP/drep.vkey" \
        --key-reg-deposit-amt "$DEPOSIT" \
        --out-file "$REG_CERT"
    U=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo-reg"; exit 1; }
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${U%% *}" --change-address "$ADDR" \
        --certificate-file "$REG_CERT" \
        --out-file "$ZOO_BUILT/$NAME-reg.raw" >/dev/null
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$ZOO_BUILT/$NAME-reg.raw" \
        --signing-key-file "$WA/payment.skey" \
        --signing-key-file "$DREP/drep.skey" \
        --out-file "$ZOO_BUILT/$NAME-reg.signed" >/dev/null
    RT=$(zoo_submit "$ZOO_BUILT/$NAME-reg.signed") || { zoo_record "$NAME" FAIL "" "reg-submit"; exit 1; }
    zoo_wait_inclusion "$RT" 60 || { zoo_record "$NAME" FAIL "$RT" "reg-not-incl"; exit 1; }
fi

VOTE="$ZOO_BUILT/$NAME.vote"
zoo_vote_file drep no "$DREP/drep.vkey" "$VOTE" || die "vote-file failed"

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
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "drep NO" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
