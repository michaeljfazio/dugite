#!/usr/bin/env bash
# 17h — the phase-2 negative that both IMPLEMENTATIONS have to judge.
#
# 17e's mismatch is rejected by `cardano-cli transaction build`, which
# evaluates the script locally — so the transaction never reaches a node and
# neither ledger is exercised. This one uses `build-raw` with a declared budget
# instead, so evaluation happens where it matters, and submits the SAME BYTES
# to dugite and to cardano-node.
#
# The assertion is agreement: both must reject. A one-sided result is the
# off-diagonal cell the bidirectional parity oracle exists to catch, and it is
# a P0 whichever way it falls — dugite accepting what cardano-node rejects is a
# consensus split, and the reverse is a false reject.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/17-context-inspecting/_ctx-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$(ctx_script redeemer-same-as-datum)"
[ -s "$SCRIPT" ] || { zoo_record "$NAME" FAIL "" "missing-script"; exit 1; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
TXIN=${PAIR%% *}; SCRIPT_AMT=${PAIR##* }
COLLAT_PAIR=$(plutus_collateral_pair) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
COLLAT=${COLLAT_PAIR%% *}; COLLAT_AMT=${COLLAT_PAIR##* }

# The datum is {"int": 42} (see _lock-helper.sh); 43 makes the script's
# `datum == redeemer` false.
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"; echo '{"int": 43}' > "$REDEEMER"

FEE=2000000
COLLAT_PCT=$(jq -r '.collateralPercentage // 150' "$(zoo_pparams_file)")
COLLAT_NEEDED=$(( (FEE * COLLAT_PCT + 99) / 100 ))
RETURN_AMT=$(( COLLAT_AMT - COLLAT_NEEDED - COLLAT_NEEDED / 4 ))
[ "$RETURN_AMT" -lt 1000000 ] && { zoo_skip "collateral utxo too small"; zoo_record "$NAME" SKIP "" "collateral-utxo-too-small"; exit 0; }

RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build-raw \
    --tx-in "$TXIN" --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-execution-units "$CTX_EXUNITS" \
    --tx-in-collateral "$COLLAT" \
    --tx-total-collateral "$((COLLAT_AMT - RETURN_AMT))" \
    --tx-out-return-collateral "${ADDR}+${RETURN_AMT}" \
    --tx-out "${ADDR}+$((SCRIPT_AMT - FEE))" \
    --fee "$FEE" \
    --protocol-params-file "$(zoo_pparams_file)" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build-raw: $(tail -3 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build-raw"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null

# Submit the same bytes to both implementations.
verdict_for() {
    local sock="$1" errf="$2"
    if cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
            --socket-path "$sock" --tx-file "$SIGNED" >/dev/null 2>"$errf"; then
        printf 'ACCEPT'
    else
        printf 'REJECT'
    fi
}
D_ERR="$ZOO_LOGS/$NAME.dugite.err"; C_ERR="$ZOO_LOGS/$NAME.cardano.err"
D_VERDICT=$(verdict_for "${LD_DUGITE_BP_SOCK:-/tmp/ld-$UID/dbp.sock}" "$D_ERR")
C_VERDICT=$(verdict_for "${LD_CARDANO_BP_SOCK:-/tmp/ld-$UID/cbp.sock}" "$C_ERR")

zoo_info "  dugite=$D_VERDICT cardano-node=$C_VERDICT"
if [ "$D_VERDICT" != "$C_VERDICT" ]; then
    zoo_fail "OFF-DIAGONAL: dugite=$D_VERDICT cardano-node=$C_VERDICT — one implementation \
accepts a phase-2 failure the other rejects"
    zoo_record "$NAME" FAIL "" "offdiag-dugite=$D_VERDICT-cardano=$C_VERDICT"
    exit 1
fi
if [ "$D_VERDICT" != REJECT ]; then
    zoo_fail "both implementations ACCEPTED a tx whose script must fail — the \
validator is not really running on either side"
    zoo_record "$NAME" FAIL "" "both-accepted"
    exit 1
fi
zoo_record "$NAME" PASS "" "phase2 mismatch REJECTED by dugite and cardano-node alike"
