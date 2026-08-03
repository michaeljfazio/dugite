#!/usr/bin/env bash
# 15i — the `after` (lower-bound) native timelock, both directions.
#
# The zoo covered `before` (02d) and nothing else: a grep for `after` across
# the whole tree returned zero matches (#961). `after` is the mirror rule and
# is evaluated against the validity interval's LOWER bound, which is a
# different field and a different comparison from `before`'s.
#
# Two cases, because a policy that always passes and a policy that always fails
# are both consistent with an unimplemented check:
#   positive — `after <past slot>`, with invalid-before set: must mint
#   negative — `after <future slot>`: must be refused
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
KH=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_PAY_VKEY")
TIP=$(zoo_tip_slot)

mk_after_policy() {   # mk_after_policy <file> <slot>
    cat > "$1" <<JSON
{ "type": "all", "scripts": [
    { "type": "sig",   "keyHash": "$KH" },
    { "type": "after", "slot": $2 } ] }
JSON
}

# ── positive: lower bound already passed ──────────────────────────────
POL_OK="$ZOO_BUILT/$NAME-ok.policy.json"
PAST=$(( TIP > 20 ? TIP - 20 : 0 ))
mk_after_policy "$POL_OK" "$PAST"
PID_OK=$(cardano-cli conway transaction policyid --script-file "$POL_OK")
ASSET=$(printf 'AFTEROK' | xxd -p | tr -d '\n')

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME-ok.raw"; SIGNED="$ZOO_BUILT/$NAME-ok.signed"
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${UTXO%% *}" --change-address "$ADDR" \
        --mint "1 ${PID_OK}.${ASSET}" --mint-script-file "$POL_OK" \
        --invalid-before "$PAST" \
        --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
    zoo_fail "positive build failed: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "positive-build-failed"; exit 1
fi
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null
TXID=$(local_txid "$SIGNED")
zoo_submit "$SIGNED" || { zoo_record "$NAME" FAIL "" "positive-rejected"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" || { zoo_record "$NAME" FAIL "$TXID" "positive-not-included"; exit 1; }
zoo_ok "$NAME: 'after' past-slot policy minted"

# ── negative: lower bound not yet reached ─────────────────────────────
POL_BAD="$ZOO_BUILT/$NAME-bad.policy.json"
FUTURE=$(( TIP + 100000 ))
mk_after_policy "$POL_BAD" "$FUTURE"
PID_BAD=$(cardano-cli conway transaction policyid --script-file "$POL_BAD")

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo-2"; exit 1; }
RAW2="$ZOO_BUILT/$NAME-bad.raw"; SIGNED2="$ZOO_BUILT/$NAME-bad.signed"
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${UTXO%% *}" --change-address "$ADDR" \
        --mint "1 ${PID_BAD}.${ASSET}" --mint-script-file "$POL_BAD" \
        --out-file "$RAW2" >/dev/null 2> "$ZOO_LOGS/$NAME-bad.err"; then
    zoo_ok "$NAME: future-slot 'after' policy refused at build"
    zoo_record "$NAME" PASS "$TXID" "after-timelock-both-directions-build-refused"
    exit 0
fi
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW2" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED2" >/dev/null
if cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
        --socket-path "$ZOO_SOCKET" --tx-file "$SIGNED2" >/dev/null 2>&1; then
    zoo_fail "$NAME: ACCEPTED a mint whose 'after' lower bound is in the future"
    zoo_record "$NAME" FAIL "" "negative-accepted"; exit 1
fi
zoo_ok "$NAME: future-slot 'after' policy rejected at submit"
zoo_record "$NAME" PASS "$TXID" "after-timelock-both-directions"
