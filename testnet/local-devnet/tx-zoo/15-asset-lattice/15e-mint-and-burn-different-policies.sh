#!/usr/bin/env bash
# 15e — mint policy A and BURN policy B in the same transaction.
#
# The mint field then carries both a positive and a NEGATIVE quantity, which is
# a different wire shape from any existing zoo mint (all of which are
# positive-only). Burning requires the tokens to exist first, so this mints B in
# a setup transaction and burns it in the transaction under test.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
KH=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_PAY_VKEY")
PA="$ZOO_BUILT/$NAME.pa.json"; PB="$ZOO_BUILT/$NAME.pb.json"
cat > "$PA" <<JSON
{ "type": "all", "scripts": [ { "type": "sig", "keyHash": "$KH" } ] }
JSON
cat > "$PB" <<JSON
{ "type": "atLeast", "required": 1, "scripts": [ { "type": "sig", "keyHash": "$KH" } ] }
JSON
PIDA=$(cardano-cli conway transaction policyid --script-file "$PA")
PIDB=$(cardano-cli conway transaction policyid --script-file "$PB")
NB="${PIDB}.$(printf 'BURNME' | xxd -p | tr -d '\n')"
NA="${PIDA}.$(printf 'MINTME' | xxd -p | tr -d '\n')"

# --- setup: mint 10 of policy B so there is something to burn ---
U=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${U%% *}" --tx-out "${ADDR}+3000000 + 10 ${NB}" --change-address "$ADDR" \
    --mint "10 ${NB}" --mint-script-file "$PB" \
    --out-file "$ZOO_BUILT/$NAME-setup.raw" >/dev/null 2>&1 \
    || { zoo_record "$NAME" FAIL "" "setup-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$ZOO_BUILT/$NAME-setup.raw" --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file "$ZOO_BUILT/$NAME-setup.signed" >/dev/null
T0=$(zoo_submit "$ZOO_BUILT/$NAME-setup.signed") || { zoo_record "$NAME" FAIL "" "setup-submit"; exit 1; }
zoo_wait_inclusion "$T0" 90 "$ADDR" >/dev/null 2>&1 || { zoo_record "$NAME" FAIL "$T0" "setup-not-included"; exit 1; }

# --- the test: mint A (+5) and burn B (-10) in ONE transaction ---
BURN_IN=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --address "$ADDR" --output-json 2>/dev/null \
  | jq -r --arg a "$NB" 'to_entries | map(select(.value.value[($a|split(".")[0])][($a|split(".")[1])] // 0 > 0)) | .[0].key // empty')
[ -n "$BURN_IN" ] || { zoo_fail "minted B not found"; zoo_record "$NAME" FAIL "" "burn-input-missing"; exit 1; }
U2=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo2"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${U2%% *}" --tx-in "$BURN_IN" \
    --tx-out "${ADDR}+3000000 + 5 ${NA}" --change-address "$ADDR" \
    --mint "5 ${NA} + -10 ${NB}" \
    --mint-script-file "$PA" --mint-script-file "$PB" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    LEFT=$(assets_at "$ZOO_SOCKET" "$ADDR" "$PIDB")
    MINTED=$(assets_at "$ZOO_SOCKET" "$ADDR" "$PIDA")
    [ "${MINTED:-0}" -ge 1 ] \
        && zoo_record "$NAME" PASS "$TXID" "mint-A+burn-B one-tx A=$MINTED B-remaining=$LEFT" \
        || { zoo_fail "policy A not observed"; zoo_record "$NAME" FAIL "$TXID" "mint-missing"; exit 1; }
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
