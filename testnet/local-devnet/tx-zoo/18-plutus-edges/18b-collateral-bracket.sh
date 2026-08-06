#!/usr/bin/env bash
# 18b — bracket the collateralPercentage boundary exactly, mirroring
# 17-context-inspecting/17f-exunits-bracket.sh's ExUnits bracket style but for
# the collateral sufficiency check instead of phase-2 CPU accounting.
#
# Haskell: `100 * effective_collateral >= collateralPercentage * fee`
# (Cardano.Ledger.Alonzo.Rules.Utxo, `feesOK`). Declaring
# total_collateral == ceil(fee * collateralPercentage / 100) must be
# ACCEPTED; one lovelace less must be REJECTED with InsufficientCollateral.
# `transaction build` auto-computes collateral, so the bracket needs
# build-raw on both arms (same reason 17f/03j do).
#
# Run the under-collateralised arm FIRST: it does not consume SCRIPT_TXIN
# (rejected transactions don't land), so the accepted arm can reuse the very
# same locked UTxO afterwards. Reordering would make the accept case pass by
# spending the ONLY locked UTxO, leaving nothing for the reject case (which
# would then fail with an unrelated "input not found").
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}; SCRIPT_AMT=${PAIR##* }

COLLAT_PAIR=$(plutus_collateral_pair) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
COLLAT=${COLLAT_PAIR%% *}; COLLAT_AMT=${COLLAT_PAIR##* }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
# (steps, memory) — cardano-cli's --tx-in-execution-units tuple order,
# confirmed live via dugite-relay's ScriptFailed budget-exhaustion log
# ("cpu_remaining" tracked the FIRST tuple element). always-true-v2 needs
# ~1,893,779 steps / ~5,894 mem in practice (CEK decode overhead, despite
# "trivial" logic) — 1,000,000 steps was under-provisioned and would make
# the EXACT (accept) arm below fail on budget exhaustion instead of actually
# succeeding, silently defeating this bracket's positive assertion.
EXUNITS="(2000000,1000000)"   # comfortably above the real ~1.89M-step need
FEE=2000000
REG_OUT=$((SCRIPT_AMT - FEE))
PPARAMS=$(zoo_pparams_file)
COLLAT_PCT=$(jq -r '.collateralPercentage // 150' "$PPARAMS")
NEEDED=$(( (FEE * COLLAT_PCT + 99) / 100 ))   # ceil(fee * pct / 100), matches collateral.rs

# RED-PROOF: flip WANT (below) to any other constructor and the under-arm must FAIL.
WANT="InsufficientCollateral"

build_at() {   # build_at <total_collateral> <suffix>
    local total="$1" sfx="$2"
    local return_amt=$((COLLAT_AMT - total))
    local raw="$ZOO_BUILT/$NAME.$sfx.raw" signed="$ZOO_BUILT/$NAME.$sfx.signed"
    cardano-cli conway transaction build-raw \
        --tx-in "$SCRIPT_TXIN" --tx-in-script-file "$SCRIPT" \
        --tx-in-inline-datum-present --tx-in-redeemer-file "$REDEEMER" \
        --tx-in-execution-units "$EXUNITS" \
        --tx-in-collateral "$COLLAT" \
        --tx-total-collateral "$total" \
        --tx-out-return-collateral "${ADDR}+${return_amt}" \
        --tx-out "${ADDR}+${REG_OUT}" \
        --fee "$FEE" \
        --protocol-params-file "$PPARAMS" \
        --out-file "$raw" >/dev/null 2> "$ZOO_LOGS/$NAME.$sfx.err" || return 1
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file "$signed" >/dev/null || return 1
    printf '%s' "$signed"
}

UNDER_RETURN=$((COLLAT_AMT - (NEEDED - 1)))
EXACT_RETURN=$((COLLAT_AMT - NEEDED))
if [ "$UNDER_RETURN" -lt 1000000 ] || [ "$EXACT_RETURN" -lt 1000000 ]; then
    zoo_skip "collateral utxo too small for the bracket (needed=$NEEDED, have=$COLLAT_AMT)"
    zoo_record "$NAME" SKIP "" "collateral-utxo-too-small"
    exit 0
fi

# ── Under-collateralised arm: NEEDED - 1, must be REJECTED ──
#
# Recorded as an internal assertion under the ONE script name (not a
# separate zoo_record row) — same convention 17f uses for its "reject the
# under-budget arm first" safety check: this whole script is ONE result row,
# matching the 12-scripts-per-12-rows bookkeeping the category was scoped
# with (#1033).
UNDER=$(build_at "$((NEEDED - 1))" under) || {
    zoo_fail "build-raw at needed-1 failed: $(tail -2 "$ZOO_LOGS/$NAME.under.err")"
    zoo_record "$NAME" FAIL "" "build-under"; exit 1; }
UNDER_OUT=$(cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-file "$UNDER" 2>&1) && UNDER_RC=0 || UNDER_RC=1
if [ "$UNDER_RC" -eq 0 ]; then
    zoo_fail "dugite ACCEPTED a tx collateralised one lovelace below ceil(fee*pct/100)"
    zoo_record "$NAME" FAIL "" "undercollateral-accepted-at-$((NEEDED - 1))"; exit 1
fi
if ! echo "$UNDER_OUT" | grep -q "$WANT"; then
    zoo_fail "under-collateralised tx rejected, but not with $WANT: $(echo "$UNDER_OUT" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "under-rejected-wrong-reason-want-$WANT"; exit 1
fi
zoo_ok "  under-collateralised arm rejected with $WANT as expected"

# ── Exactly-collateralised arm: ceil(fee*pct/100), must be ACCEPTED ──
EXACT=$(build_at "$NEEDED" exact) || {
    zoo_fail "build-raw at needed failed: $(tail -2 "$ZOO_LOGS/$NAME.exact.err")"
    zoo_record "$NAME" FAIL "" "build-exact"; exit 1; }
TXID=$(zoo_submit "$EXACT") || {
    zoo_fail "dugite REJECTED a tx collateralised at exactly ceil(fee*pct/100) — overly strict"
    zoo_record "$NAME" FAIL "" "overstrict-rejected-at-$NEEDED"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_record "$NAME" PASS "$TXID" "bracketed: under=$((NEEDED - 1))->$WANT, exact=$NEEDED->accepted"
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
