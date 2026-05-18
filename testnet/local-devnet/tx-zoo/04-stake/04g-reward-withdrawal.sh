#!/usr/bin/env bash
# 04g — reward withdrawal from wallet-a's stake addr. The reward balance
# may be zero (depends on whether rewards have been distributed); the script
# attempts the withdrawal anyway to exercise the wire path.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr")
STAKE_ADDR=$(cat "$WA/stake.addr")

REW=$(cardano-cli conway query stake-address-info \
        --testnet-magic "$LD_MAGIC" \
        --socket-path "$ZOO_SOCKET" \
        --address "$STAKE_ADDR" 2>/dev/null \
        | jq -r '.[0].rewardAccountBalance // 0')
if [ "${REW:-0}" -le 0 ]; then
    zoo_skip "no rewards available at $STAKE_ADDR (balance=$REW)"
    zoo_record "$NAME" SKIP "" "no-rewards"
    exit 0
fi

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --change-address "$ADDR" \
    --withdrawal "${STAKE_ADDR}+${REW}" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$WA/stake.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "withdrawn=$REW" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
