#!/usr/bin/env bash
# 18l — a PlutusV2 script exercising byteStringToInteger, a V3-era builtin
# retroactively added to the PlutusV2 cost model.
#
# Upstream: tests_conway/test_update_plutusv2_builtins.py::test_update_in_pv9
# (the PV10-live arm), via tests_plutus_v2/mint_raw.py::check_missing_builtin.
#
# UNLIKE every other script in this category, the correct outcome is NOT
# fixed — it branches on the LIVE chain's cost model, exactly like upstream's
# own `cost_model_len < 185` branch:
#
#   costModels.PlutusV2 length >= 185  -> builtin priced -> tx SUCCEEDS
#   costModels.PlutusV2 length <  185  -> builtin unpriced -> tx FAILS
#     (Haskell: "overspending the budget" — a missing cost-model parameter,
#     NOT an "unknown builtin" error; the CEK machine still recognises the
#     opcode, it just cannot price it)
#
# The assertion is PARITY, not a fixed verdict: whichever branch fires, BOTH
# implementations must agree. `transaction build`'s Plutus evaluation runs
# CLIENT-SIDE in cardano-cli itself (the socket is only used to fetch
# UTxOs/protocol-parameters/cost-models, never to delegate evaluation) — so
# re-running the identical build against a DIFFERENT node's socket is a
# direct probe of whether that node's LSQ cost-model answer agrees with the
# first, independent of anything server-side.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
edge_materialize_builtins_script || { zoo_record "$NAME" FAIL "" "vendor-materialize"; exit 1; }
[ -s "$EDGE_BUILTINS_SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$EDGE_BUILTINS_SCRIPT")"; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
PPARAMS=$(zoo_pparams_file)
COST_LEN=$(jq -r '.costModels.PlutusV2 | length // 0' "$PPARAMS")
zoo_info "  costModels.PlutusV2 length = $COST_LEN (>=185 => byteStringToInteger priced)"

POLICY_ID=$(cardano-cli conway transaction policyid --script-file "$EDGE_BUILTINS_SCRIPT")
ASSET_NAME_HEX="$(printf 'TXZOOV2V3BI' | xxd -p | tr -d '\n')"
ASSET="${POLICY_ID}.${ASSET_NAME_HEX}"
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }

# build_against <sock> <suffix> -> 0 on local build success, 1 on failure
# (stderr captured to $ZOO_LOGS/$NAME.$suffix.err either way).
build_against() {
    local sock="$1" sfx="$2"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
        --tx-in "${UTXO%% *}" \
        --tx-in-collateral "$COLLAT" \
        --tx-out "${ADDR}+3000000 + 1 ${ASSET}" \
        --change-address "$ADDR" \
        --mint "1 ${ASSET}" \
        --mint-script-file "$EDGE_BUILTINS_SCRIPT" \
        --mint-redeemer-file "$REDEEMER" \
        --out-file "$ZOO_BUILT/$NAME.$sfx.raw" >/dev/null 2> "$ZOO_LOGS/$NAME.$sfx.err"
}

if [ "$COST_LEN" -ge 185 ]; then
    # RED-PROOF: change this branch's condition to `-lt 185` (asserting the
    # WRONG expectation for the detected cost-model length) — must FAIL.
    build_against "$ZOO_SOCKET" primary || {
        zoo_fail "costModels.PlutusV2 has $COST_LEN entries (>=185) but the mint build FAILED: $(tail -3 "$ZOO_LOGS/$NAME.primary.err")"
        zoo_record "$NAME" FAIL "" "expected-accept-cost-len-$COST_LEN-got-build-fail"
        exit 1
    }
    SIGNED="$ZOO_BUILT/$NAME.signed"
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$ZOO_BUILT/$NAME.primary.raw" --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file "$SIGNED" >/dev/null
    TXID=$(zoo_submit "$SIGNED") || {
        zoo_fail "local build succeeded but the node REJECTED the tx — dugite disagrees with its own served cost model"
        zoo_record "$NAME" FAIL "" "node-rejected-after-local-accept-cost-len-$COST_LEN"; exit 1; }
    if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
        zoo_record "$NAME" PASS "$TXID" "byteStringToInteger priced (costModels.PlutusV2 len=$COST_LEN), all observers agree"
    else
        zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
    fi
else
    if build_against "$ZOO_SOCKET" primary; then
        zoo_fail "costModels.PlutusV2 has only $COST_LEN entries (<185) but the mint build SUCCEEDED — \
byteStringToInteger should be unpriced and this should have failed budget evaluation"
        zoo_record "$NAME" FAIL "" "expected-reject-cost-len-$COST_LEN-got-build-success"
        exit 1
    fi
    if ! grep -qiE 'budget|overspend|exceed|cost' "$ZOO_LOGS/$NAME.primary.err"; then
        zoo_fail "build failed, but not for a budget/cost-model reason: $(tail -3 "$ZOO_LOGS/$NAME.primary.err")"
        zoo_record "$NAME" FAIL "" "build-failed-wrong-reason"; exit 1
    fi
    zoo_ok "  primary node (\$ZOO_SOCKET): local build failed for budget/cost-model reasons as expected"

    # Parity: the SAME probe against cardano-bp's own socket, if present.
    if [ -n "${LD_CARDANO_BP_SOCK:-}" ] && [ -S "$LD_CARDANO_BP_SOCK" ]; then
        if build_against "$LD_CARDANO_BP_SOCK" cbp; then
            zoo_fail "cardano-bp's served cost model DISAGREES with \$ZOO_SOCKET's — the same local build \
succeeded against cardano-bp but failed against \$ZOO_SOCKET, a real LSQ cost-model divergence"
            zoo_record "$NAME" FAIL "" "cost-model-parity-mismatch-cbp-accepted"
            exit 1
        fi
        if ! grep -qiE 'budget|overspend|exceed|cost' "$ZOO_LOGS/$NAME.cbp.err"; then
            zoo_fail "cardano-bp probe failed, but not for a budget/cost-model reason: $(tail -3 "$ZOO_LOGS/$NAME.cbp.err")"
            zoo_record "$NAME" FAIL "" "cbp-build-failed-wrong-reason"
            exit 1
        fi
        zoo_ok "  cardano-bp socket agrees: same budget/cost-model rejection"
    else
        zoo_info "  cardano-bp socket unavailable — skipping the cross-node cost-model parity probe"
    fi
    zoo_record "$NAME" PASS "" "byteStringToInteger unpriced (costModels.PlutusV2 len=$COST_LEN), rejected for budget as expected"
fi
