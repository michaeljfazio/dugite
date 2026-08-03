#!/usr/bin/env bash
# 15j — a NESTED native-script combinator: all[ any[sig,sig], atLeast 2 of 3 ].
#
# The zoo's native scripts (02a-02c) are all FLAT: one `all`, one `any`, one
# `atLeast`, each containing only `sig` leaves. Nothing ever nested a
# combinator inside another, so the recursive evaluation path was untested
# (#961) — a validator that handled only depth-1 scripts would pass every
# existing case.
#
# Two directions, because a script that always passes is indistinguishable from
# an unevaluated one:
#   positive — sign with keys that satisfy BOTH branches: must mint
#   negative — satisfy the `any` but only 1 of the 3 in `atLeast 2`: must fail
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
KEYDIR="$ZOO_BUILT/$NAME-keys"; mkdir -p "$KEYDIR"

# Four keys: A (the zoo payment key, always signs), plus B, C, D.
KH_A=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_PAY_VKEY")
for k in b c d; do
    [ -s "$KEYDIR/$k.skey" ] || cardano-cli conway address key-gen \
        --verification-key-file "$KEYDIR/$k.vkey" --signing-key-file "$KEYDIR/$k.skey" 2>/dev/null
done
KH_B=$(cardano-cli conway address key-hash --payment-verification-key-file "$KEYDIR/b.vkey")
KH_C=$(cardano-cli conway address key-hash --payment-verification-key-file "$KEYDIR/c.vkey")
KH_D=$(cardano-cli conway address key-hash --payment-verification-key-file "$KEYDIR/d.vkey")

POLICY="$ZOO_BUILT/$NAME.policy.json"
cat > "$POLICY" <<JSON
{ "type": "all", "scripts": [
    { "type": "any", "scripts": [
        { "type": "sig", "keyHash": "$KH_A" },
        { "type": "sig", "keyHash": "$KH_B" } ] },
    { "type": "atLeast", "required": 2, "scripts": [
        { "type": "sig", "keyHash": "$KH_B" },
        { "type": "sig", "keyHash": "$KH_C" },
        { "type": "sig", "keyHash": "$KH_D" } ] } ] }
JSON
PID=$(cardano-cli conway transaction policyid --script-file "$POLICY")
ASSET=$(printf 'NESTED' | xxd -p | tr -d '\n')

mint_with() {   # mint_with <suffix> <signing-key-file...>
    local suffix="$1"; shift
    local utxo raw signed args=()
    utxo=$(zoo_largest_utxo "$ADDR") || return 2
    raw="$ZOO_BUILT/$NAME-$suffix.raw"; signed="$ZOO_BUILT/$NAME-$suffix.signed"
    # --witness-override is load-bearing: `transaction build` sizes the fee for
    # the witnesses it can infer, and this script signs with EXTRA keys
    # afterwards to satisfy the nested script. Without it the built fee covers
    # a smaller tx than the one submitted and the node correctly answers
    # FeeTooSmallUTxO — a fixture error that looks like a script failure.
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${utxo%% *}" --change-address "$ADDR" \
        --mint "1 ${PID}.${ASSET}" --mint-script-file "$POLICY" \
        --witness-override $(( $# + 1 )) \
        --out-file "$raw" >/dev/null 2> "$ZOO_LOGS/$NAME-$suffix.err" || return 2
    args=(--testnet-magic "$LD_MAGIC" --tx-body-file "$raw" --signing-key-file "$ZOO_PAY_SKEY")
    local k; for k in "$@"; do args+=(--signing-key-file "$k"); done
    cardano-cli conway transaction sign "${args[@]}" --out-file "$signed" >/dev/null || return 2
    printf '%s' "$signed"
}

# ── positive: A satisfies the `any`; C and D satisfy `atLeast 2 of 3` ──
SIGNED=$(mint_with ok "$KEYDIR/c.skey" "$KEYDIR/d.skey") || {
    zoo_fail "positive build/sign failed: $(grep -m1 Error "$ZOO_LOGS/$NAME-ok.err" 2>/dev/null | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "positive-build-failed"; exit 1
}
TXID=$(local_txid "$SIGNED")
zoo_submit "$SIGNED" || { zoo_record "$NAME" FAIL "" "positive-rejected"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" || { zoo_record "$NAME" FAIL "$TXID" "positive-not-included"; exit 1; }
zoo_ok "$NAME: nested all[any,atLeast] minted with A + C + D"

# ── negative: A satisfies the `any`, but only C signs the atLeast (1 of 2) ──
SIGNED2=$(mint_with bad "$KEYDIR/c.skey") || {
    zoo_ok "$NAME: under-signed nested script refused at build"
    zoo_record "$NAME" PASS "$TXID" "nested-combinator-both-directions-build-refused"; exit 0
}
if cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
        --socket-path "$ZOO_SOCKET" --tx-file "$SIGNED2" >/dev/null 2>&1; then
    zoo_fail "$NAME: ACCEPTED a mint satisfying only 1 of the atLeast-2 branch"
    zoo_record "$NAME" FAIL "" "negative-accepted"; exit 1
fi
zoo_ok "$NAME: under-signed nested script rejected at submit"
zoo_record "$NAME" PASS "$TXID" "nested-combinator-both-directions"
