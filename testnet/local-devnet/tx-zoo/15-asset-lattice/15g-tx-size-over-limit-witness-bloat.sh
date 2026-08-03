#!/usr/bin/env bash
# 15g — exceed maxTxSize through WITNESS bloat rather than metadata.
# Expect: TxTooLarge (ConwayUtxoPredFailure tag 3, MaxTxSizeUTxO).
#
# 08i pads the body with metadata. A witness-bloated transaction crosses the
# same limit through a different part of the serialization — the witness set
# rather than the auxiliary data — which is worth separating because the two
# are measured by different code: the body size is computed over the body's own
# bytes, while the tx size is over the whole signed envelope. A node that
# measured only the body would pass 08i and accept this.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
MAX=$(max_tx_size)

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}; AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=300000
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"

# build-raw, so the witness count is ours to choose rather than the CLI's.
cardano-cli conway transaction build-raw \
    --tx-in "$TXIN" --tx-out "${ADDR}+$((AMT - FEE))" \
    --fee "$FEE" --ttl $((TIP + 600)) \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" || {
    zoo_fail "build-raw failed"; zoo_record "$NAME" FAIL "" "build-failed"; exit 1
}

# Each vkey witness is ~100 bytes on the wire, so ~200 keys clears a 16 KB cap
# comfortably. Generate them once and sign with all of them.
KEYDIR="$ZOO_BUILT/$NAME-keys"; mkdir -p "$KEYDIR"
SIGN_ARGS=(--signing-key-file "$ZOO_PAY_SKEY")
N=$(( (MAX / 100) + 40 ))
for i in $(seq 1 "$N"); do
    if [ ! -s "$KEYDIR/k$i.skey" ]; then
        cardano-cli conway address key-gen \
            --verification-key-file "$KEYDIR/k$i.vkey" \
            --signing-key-file "$KEYDIR/k$i.skey" 2>/dev/null || break
    fi
    SIGN_ARGS+=(--signing-key-file "$KEYDIR/k$i.skey")
done

cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" "${SIGN_ARGS[@]}" --out-file "$SIGNED" >/dev/null 2>&1 || {
    zoo_fail "sign failed with $N extra witnesses"
    zoo_record "$NAME" FAIL "" "sign-failed"; exit 1
}

SIZE=$(signed_tx_size "$SIGNED")
if [ "$SIZE" -le "$MAX" ]; then
    zoo_record_env_skip "$NAME" "witness bloat only reached ${SIZE}B, not over maxTxSize ${MAX}B"
    exit 0
fi
zoo_info "$NAME: ${SIZE}B against maxTxSize ${MAX}B"

OUT=$(cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
        --socket-path "$ZOO_SOCKET" --tx-file "$SIGNED" 2>&1) && {
    zoo_fail "$NAME: ACCEPTED a ${SIZE}B tx over maxTxSize ${MAX}B"
    zoo_record "$NAME" FAIL "" "accepted-over-limit"; exit 1
}
if echo "$OUT" | grep -qiE 'MaxTxSizeUTxO|TxTooLarge|too large'; then
    zoo_ok "$NAME: rejected with a size failure at ${SIZE}B"
    zoo_record "$NAME" PASS "" "rejected-MaxTxSizeUTxO-witness-bloat"
else
    got=$(echo "$OUT" | grep -m1 -oE '\(Conway[A-Za-z]*Failure[^)]*' | cut -c1-110)
    zoo_fail "$NAME: rejected, but as '${got:-unknown}' not a size failure"
    zoo_record "$NAME" FAIL "" "rejected-wrong-reason-want-MaxTxSizeUTxO"; exit 1
fi
