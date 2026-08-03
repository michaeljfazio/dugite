#!/usr/bin/env bash
# 17f — assert dugite's phase-2 COST, not merely its verdict.
#
# Every other Plutus script in the zoo asserts acceptance. None asserts what
# the evaluation cost, and #772 was a CPU OVERCHARGE — a defect that does not
# change accept/reject on a devnet with generous limits, so it would still slip
# through today. `grep -r executionUnits tx-zoo/` finds nothing.
#
# Method: bracket dugite's accounting against cardano-api's Plutus evaluator,
# which is the reference implementation and is what `cardano-cli transaction
# build` uses to size a redeemer's budget.
#
#   1. Build the tx with `transaction build`; read the ExUnits it chose.
#   2. Re-build with `build-raw` declaring EXACTLY those units. dugite must
#      ACCEPT — so dugite's cost is <= the reference.
#   3. Re-build declaring one step FEWER. dugite must REJECT for budget — so
#      dugite's cost is > reference-1, i.e. >= the reference.
#
# Together those pin dugite's CPU accounting to the reference value exactly.
# Either bound alone is satisfiable by a wrong implementation: an undercharging
# node passes step 2 and fails step 3, an overcharging one the reverse.
#
# The script is `redeemerSameAsDatum`, a real plutus-tx program that actually
# executes (an always-true validator costs almost nothing and would make the
# bracket meaningless).
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
COLLAT=${COLLAT_PAIR%% *}

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 42}' > "$REDEEMER"

# ── Step 1: what does the reference evaluator charge? ───────────────────────
PROBE="$ZOO_BUILT/$NAME.probe.raw"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "$TXIN" --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-collateral "$COLLAT" \
    --tx-out "${ADDR}+2000000" --change-address "$ADDR" \
    --out-file "$PROBE" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "probe build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "probe-build"; exit 1; }

UNITS=$(cardano-cli debug transaction view --tx-body-file "$PROBE" 2>/dev/null \
    | jq -r '[.redeemers[0].redeemer."execution units".steps,
              .redeemers[0].redeemer."execution units".memory] | @tsv')
REF_STEPS=$(printf '%s' "$UNITS" | cut -f1)
REF_MEM=$(printf '%s' "$UNITS" | cut -f2)
if [ -z "$REF_STEPS" ] || [ "$REF_STEPS" = null ] || [ "$REF_STEPS" -le 0 ] 2>/dev/null; then
    zoo_fail "could not read reference ExUnits from the probe body"
    zoo_record "$NAME" FAIL "" "no-reference-exunits"; exit 1
fi
zoo_info "  reference ExUnits (cardano-api evaluator): steps=$REF_STEPS mem=$REF_MEM"

FEE=2000000
COLLAT_PCT=$(jq -r '.collateralPercentage // 150' "$(zoo_pparams_file)")
COLLAT_AMT=${COLLAT_PAIR##* }
COLLAT_NEEDED=$(( (FEE * COLLAT_PCT + 99) / 100 ))
RETURN_AMT=$(( COLLAT_AMT - COLLAT_NEEDED - COLLAT_NEEDED / 4 ))
[ "$RETURN_AMT" -lt 1000000 ] && { zoo_skip "collateral utxo too small"; zoo_record "$NAME" SKIP "" "collateral-utxo-too-small"; exit 0; }
OUT_AMT=$((SCRIPT_AMT - FEE))

# build_at <steps> <mem> <suffix>  -> signed tx path on stdout
build_at() {
    local steps="$1" mem="$2" sfx="$3"
    local raw="$ZOO_BUILT/$NAME.$sfx.raw" signed="$ZOO_BUILT/$NAME.$sfx.signed"
    cardano-cli conway transaction build-raw \
        --tx-in "$TXIN" --tx-in-script-file "$SCRIPT" \
        --tx-in-inline-datum-present --tx-in-redeemer-file "$REDEEMER" \
        --tx-in-execution-units "($steps,$mem)" \
        --tx-in-collateral "$COLLAT" \
        --tx-total-collateral "$((COLLAT_AMT - RETURN_AMT))" \
        --tx-out-return-collateral "${ADDR}+${RETURN_AMT}" \
        --tx-out "${ADDR}+${OUT_AMT}" \
        --fee "$FEE" \
        --protocol-params-file "$(zoo_pparams_file)" \
        --out-file "$raw" >/dev/null 2> "$ZOO_LOGS/$NAME.$sfx.err" || return 1
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file "$signed" >/dev/null || return 1
    printf '%s' "$signed"
}

# ── Step 3 FIRST: the under-budget tx must be rejected ──────────────────────
# Run the negative before the positive. The positive SPENDS the locked UTxO, so
# afterwards the negative could only fail with `InputNotFound` — a rejection
# for entirely the wrong reason that would still look like a pass.
UNDER=$(build_at "$((REF_STEPS - 1))" "$REF_MEM" under) || {
    zoo_fail "build-raw at reference-1 steps failed"; zoo_record "$NAME" FAIL "" "build-under"; exit 1; }
UNDER_ERR="$ZOO_LOGS/$NAME.under.submit.err"
if cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
        --socket-path "$ZOO_SOCKET" --tx-file "$UNDER" >/dev/null 2>"$UNDER_ERR"; then
    zoo_fail "dugite ACCEPTED a tx budgeted one step below the reference cost — \
it is undercharging phase-2 CPU relative to cardano-api"
    zoo_record "$NAME" FAIL "" "undercharge-accepted-at-$((REF_STEPS - 1))"
    exit 1
fi
# It must be rejected for BUDGET, not for anything else, or this proves
# nothing — "the node said no" is satisfied by a missing input just as well.
#
# The wire error is not enough to tell: dugite's `ScriptFailed` still degrades
# to a generic `ConwayMempoolFailure "transaction validation failed"` on the
# N2C wire (#979's remaining set), so the reason has to be read from the node
# log. That is a weaker source than the wire and it is used deliberately rather
# than by omission — when #979 gives `ScriptFailed` a typed encoding, prefer
# the wire and delete the fallback.
UNDER_REASON=""
if grep -qiE 'budget|ExUnits|exceed' "$UNDER_ERR"; then
    UNDER_REASON="wire"
else
    for lg in "$LD_ROOT/logs/dugite-relay.log" "$LD_ROOT/logs/dugite-bp.log"; do
        [ -f "$lg" ] || continue
        if tail -400 "$lg" | grep -qi 'budget exhausted'; then UNDER_REASON="node-log"; break; fi
    done
fi
if [ -z "$UNDER_REASON" ]; then
    zoo_fail "under-budget tx was rejected, but nothing shows it was for the BUDGET: $(tail -2 "$UNDER_ERR")"
    zoo_record "$NAME" FAIL "" "under-rejected-wrong-reason"
    exit 1
fi
zoo_ok "  reference-1 steps rejected for budget (via $UNDER_REASON) — dugite charges >= $REF_STEPS"

# ── Step 2: the exactly-budgeted tx must be accepted ────────────────────────
EXACT=$(build_at "$REF_STEPS" "$REF_MEM" exact) || {
    zoo_fail "build-raw at reference steps failed"; zoo_record "$NAME" FAIL "" "build-exact"; exit 1; }
TXID=$(zoo_submit "$EXACT") || {
    zoo_fail "dugite REJECTED a tx budgeted at exactly the reference cost — \
it is overcharging phase-2 CPU relative to cardano-api (the #772 defect class)"
    zoo_record "$NAME" FAIL "" "overcharge-rejected-at-$REF_STEPS"; exit 1; }

if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_record "$NAME" PASS "$TXID" "exunits bracketed exactly: steps=$REF_STEPS mem=$REF_MEM"
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
