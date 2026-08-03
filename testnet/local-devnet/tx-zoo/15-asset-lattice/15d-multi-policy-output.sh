#!/usr/bin/env bash
# 15d — a single output carrying assets from TWO different policies.
#
# The zoo's positive mints were all single-policy, so the OUTER level of the
# multi-asset map (policy -> asset-map) always had exactly one entry. Only the
# inner level was ever exercised, and #930's encodeMap switch applies to BOTH.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
KH=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_PAY_VKEY")

# Two DIFFERENT policies: same signer, different script shapes, so the policy
# ids differ and the outer map genuinely has two entries.
P1="$ZOO_BUILT/$NAME.p1.json"; P2="$ZOO_BUILT/$NAME.p2.json"
cat > "$P1" <<JSON
{ "type": "all", "scripts": [ { "type": "sig", "keyHash": "$KH" } ] }
JSON
cat > "$P2" <<JSON
{ "type": "any", "scripts": [ { "type": "sig", "keyHash": "$KH" } ] }
JSON
PID1=$(cardano-cli conway transaction policyid --script-file "$P1")
PID2=$(cardano-cli conway transaction policyid --script-file "$P2")
[ "$PID1" != "$PID2" ] || { zoo_fail "policies collided"; zoo_record "$NAME" FAIL "" "policy-collision"; exit 1; }

A1="5 ${PID1}.$(printf 'MPA' | xxd -p | tr -d '\n')"
A2="7 ${PID2}.$(printf 'MPB' | xxd -p | tr -d '\n')"
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${UTXO%% *}" \
    --tx-out "${ADDR}+5000000 + ${A1} + ${A2}" \
    --change-address "$ADDR" \
    --mint "${A1} + ${A2}" \
    --mint-script-file "$P1" --mint-script-file "$P2" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    N1=$(assets_at "$ZOO_SOCKET" "$ADDR" "$PID1"); N2=$(assets_at "$ZOO_SOCKET" "$ADDR" "$PID2")
    [ "${N1:-0}" -ge 1 ] && [ "${N2:-0}" -ge 1 ] \
        && zoo_record "$NAME" PASS "$TXID" "two-policies-one-output p1=$N1 p2=$N2" \
        || { zoo_fail "expected both policies present, got p1=$N1 p2=$N2"; zoo_record "$NAME" FAIL "$TXID" "missing-policy"; exit 1; }
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
