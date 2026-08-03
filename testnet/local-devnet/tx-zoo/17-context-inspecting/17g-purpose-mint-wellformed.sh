#!/usr/bin/env bash
# 17g — the SAME validator family, driven through a SECOND purpose.
#
# `purposeIsWellformedNoDatum` reads the MintingScript arm:
#
#   MintingScript cs -> member cs $ getValue $ mintValueMinted (txInfoMint txInfo)
#
# i.e. the policy id in the ScriptInfo must actually appear as a key in the
# transaction's mint value. That is a different ScriptContext construction path
# and a different redeemer-pointer tag from 17a-17f's spend, and it is the one
# place a wrong `mintValueMinted` (Conway split minted/burned) would show up.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/17-context-inspecting/_ctx-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$(ctx_script purpose-is-wellformed-no-datum)"
[ -s "$SCRIPT" ] || { zoo_record "$NAME" FAIL "" "missing-script"; exit 1; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
POLICY_ID=$(cardano-cli conway transaction policyid --script-file "$SCRIPT")
ASSET="${POLICY_ID}.$(printf '%s' "17g" | xxd -p)"
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
COLLAT_PAIR=$(plutus_collateral_pair) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"; echo '{"int": 0}' > "$REDEEMER"
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"

cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${UTXO%% *}" \
    --tx-in-collateral "${COLLAT_PAIR%% *}" \
    --tx-out "${ADDR}+5000000 + 1 ${ASSET}" \
    --mint "1 ${ASSET}" \
    --mint-script-file "$SCRIPT" \
    --mint-redeemer-file "$REDEEMER" \
    --change-address "$ADDR" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -3 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_record "$NAME" PASS "$TXID" "mint purpose: policy id found in mintValueMinted"
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
