#!/usr/bin/env bash
# 15c — mint 256 distinct asset names under one policy.
# At 256 entries a DEFINITE header needs 3 bytes (0xB9 xxxx) where the
# indefinite form needs 2 (0xBF + 0xFF). That 1-byte difference is exactly what
# over-counted maxValSize in #930 and produced a FALSE Phase-1 reject.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"; N=256
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
read -r POLICY POLICY_ID <<<"$(mint_policy "$NAME")"
ASSETS=$(asset_list "$POLICY_ID" "$N" "TZC")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${UTXO%% *}" \
    --tx-out "${ADDR}+20000000 + ${ASSETS}" \
    --change-address "$ADDR" \
    --mint "${ASSETS}" --mint-script-file "$POLICY" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    GOT=$(assets_at "$ZOO_SOCKET" "$ADDR" "$POLICY_ID")
    [ "${GOT:-0}" -ge "$N" ] \
        && zoo_record "$NAME" PASS "$TXID" "minted=$N observed=$GOT 256-entry-map-1-byte-boundary" \
        || { zoo_fail "expected >=$N assets, observed $GOT"; zoo_record "$NAME" FAIL "$TXID" "asset-count-$GOT"; exit 1; }
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
