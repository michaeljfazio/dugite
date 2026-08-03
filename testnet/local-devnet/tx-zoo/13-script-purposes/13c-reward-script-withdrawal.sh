#!/usr/bin/env bash
# 13c — withdraw rewards from a reward account whose credential is a SCRIPT.
#
# THE REWARDING PURPOSE. A withdrawal must be authorised by the stake
# credential, so a script credential means a `Rewarding` ScriptPurpose and
# redeemer tag 3.
#
# Like 04g this needs rewards to exist, which on the devnet means the full
# mark/set/go pipeline after delegation (~4-5 epoch boundaries). Outside the
# rewards-maturity round it records a STATE skip (not an ENV skip): the surface
# is exercisable, the chain simply has not got there yet. See #958.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"
. "$ZOO_DIR/13-script-purposes/_purpose-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
W="script-stake-v3"
SCRIPT=$(script_file "$W")
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing script-stake wallet — run run-all.sh --setup"; exit 0; }

ADDR=$(script_pay_addr "$W")
STAKE_ADDR=$(script_stake_addr "$W")
INFO=$(cardano-cli conway query stake-address-info \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$STAKE_ADDR" 2>/dev/null)
REWARD=$(echo "$INFO" | jq -r '.[0].rewardAccountBalance // 0')
if [ "${REWARD:-0}" -le 0 ]; then
    zoo_skip "no rewards on $STAKE_ADDR yet (needs ~5 epoch boundaries post-delegation)"
    zoo_record "$NAME" SKIP "" "no-rewards"
    exit 0
fi

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
write_redeemer "$REDEEMER"
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-in-collateral "$COLLAT" \
    --change-address "$ADDR" \
    --withdrawal              "${STAKE_ADDR}+${REWARD}" \
    --withdrawal-script-file   "$SCRIPT" \
    --withdrawal-redeemer-file "$REDEEMER" \
    --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null

assert_purpose "$SIGNED" Rewarding || { zoo_record "$NAME" FAIL "" "no-rewarding-redeemer"; exit 1; }

TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if wait_all_strict "$TXID" 150 "$ADDR"; then
    AFTER=$(cardano-cli conway query stake-address-info \
              --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
              --address "$STAKE_ADDR" 2>/dev/null | jq -r '.[0].rewardAccountBalance // 0')
    if [ "${AFTER:-0}" -eq 0 ]; then
        zoo_record "$NAME" PASS "$TXID" "rewarding-purpose withdrew=$REWARD balance-now=0"
    else
        zoo_record "$NAME" FAIL "$TXID" "reward-balance-not-zeroed after=$AFTER"
        exit 1
    fi
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"
    exit 1
fi
