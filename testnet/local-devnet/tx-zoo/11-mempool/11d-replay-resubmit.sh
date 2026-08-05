#!/usr/bin/env bash
# 11d — replay/resubmit an already-included tx. Upstream: cardano-node-tests
# test_duplicated_tx (#1032, cardano-node-tests adoption P0.1).
#
# Submit a tx, wait for it to be confirmed on all 3 observers, then resubmit
# the IDENTICAL signed file to both the dugite relay socket AND the Haskell
# cardano-bp socket. Both must reject it — its inputs are already spent — and
# we record the exact wire text each side returned so a reason DIVERGENCE
# (not just "both said no") is visible, matching #979's "the wire text is the
# oracle, not just accept/reject" lesson (see 16-cert-negatives).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=200000

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build-raw \
    --tx-in     "$TXIN" \
    --tx-out    "${ADDR}+$((AMT - FEE))" \
    --fee       "$FEE" \
    --ttl       $((TIP + 600)) \
    --out-file  "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null

TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if ! zoo_wait_all_observers "$TXID" 120; then
    zoo_record "$NAME" FAIL "$TXID" "not-included-before-replay"
    exit 1
fi

# ── Resubmit the identical signed bytes to both a dugite socket and the
#    Haskell cardano-bp socket. Both must reject. ──────────────────────────
# RED-PROOF: change `rc -eq 0` handling below to treat acceptance as PASS (or
# drop the reason-keyword check) to hide a node that double-spends its own
# already-confirmed tx.
DETAIL=""
BAD=0
for label_sock in "relay:$ZOO_SOCKET" "cardano-bp:$LD_CARDANO_BP_SOCK"; do
    label=${label_sock%%:*}
    sock=${label_sock#*:}
    if [ ! -S "$sock" ]; then
        DETAIL="$DETAIL $label=socket-missing"
        continue
    fi
    OUT=$(cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
            --tx-file "$SIGNED" 2>&1) && RC=0 || RC=1
    SHORT=$(printf '%s' "$OUT" | head -c 160 | tr '\n' ' ')
    if [ "$RC" -eq 0 ]; then
        zoo_fail "$label: resubmit of already-included tx $TXID was ACCEPTED"
        DETAIL="$DETAIL $label=ACCEPTED(bad)"
        BAD=1
        continue
    fi
    # Haskell >=10.6 words this "All inputs are spent"; earlier releases and
    # dugite's own wire form use the BadInputsUTxO constructor name. Accept
    # either — the point is that BOTH sides reject the replay, for an
    # inputs-already-spent reason specifically.
    if printf '%s' "$OUT" | grep -qiE 'BadInputsUTxO|All inputs are spent|already spent|input.*not.*found'; then
        zoo_ok "$label: rejected replay ($SHORT)"
        DETAIL="$DETAIL $label=rejected[$SHORT]"
    else
        zoo_fail "$label: rejected, but not for spent inputs: $SHORT"
        DETAIL="$DETAIL $label=wrong-reason[$SHORT]"
        BAD=1
    fi
done

if [ "$BAD" -eq 0 ]; then
    zoo_record "$NAME" PASS "$TXID" "${DETAIL# }"
else
    zoo_record "$NAME" FAIL "$TXID" "${DETAIL# }"
    exit 1
fi
