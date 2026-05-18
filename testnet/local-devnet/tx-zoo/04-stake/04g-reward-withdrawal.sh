#!/usr/bin/env bash
# 04g — reward withdrawal from wallet-a's stake addr.
#
# Expected SKIP rationale (legitimate in a one-shot tx-zoo run):
#
#   Reward distribution requires ~3 epoch boundaries between
#   stake-delegation (04b/04c) and the snapshot becoming the "go" snapshot
#   from which rewards are computed (mark → set → go). On the devnet
#   (epochLength=4000 slots, 1 slot/s ⇒ ~67 min/epoch ⇒ ~3.3 hours to
#   accumulate rewards), this is not feasible in a single suite run.
#
#   When the suite is run end-to-end without waiting hours, wallet-a's
#   reward balance is 0 and this script records SKIP=no-rewards. Treat
#   that as PASS for "Definition of done" — the wire path is exercised
#   in a separate long-running soak test outside the tx-zoo runner.
#
# When rewards ARE present (e.g. on a long-lived devnet or via a manual
# pre-warm) the script proceeds to build/sign/submit a withdrawal tx and
# verifies it lands on all three observers.
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
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "withdrawn=$REW" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
