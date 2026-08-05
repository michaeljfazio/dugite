#!/usr/bin/env bash
# 08t — bracket dugite's fee enforcement exactly at the boundary, mirroring
# 17f's two-arm ExUnits bracket but for the base tx fee.
#
#   Arm B (minFee - 1) FIRST: must be REJECTED as FeeTooSmallUTxO, and the
#   rejection text must carry the exact required amount (minFee) — proving
#   dugite reports the SAME number cardano-cli's own calculate-min-fee
#   computed, not just "some" fee error.
#   Arm A (minFee) SECOND: must be ACCEPTED and included.
#
# Order matters for the same reason as 17f: arm A spends the funding UTxO, so
# it must run after arm B or a "rejected" arm B could just mean "no such
# input" instead of "fee too low".
#
# Upstream precedent: cardano-node-tests fee-boundary coverage (#1032,
# cardano-node-tests adoption P0.1).
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
TTL=$((TIP + 600))

# ── Discover the exact min fee via cardano-cli's own evaluator ─────────────
PLACEHOLDER=300000
PROBE="$ZOO_BUILT/$NAME.probe.raw"
cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - PLACEHOLDER))" \
    --fee      "$PLACEHOLDER" \
    --ttl      "$TTL" \
    --out-file "$PROBE" >/dev/null 2> "$ZOO_LOGS/$NAME.probe.err" \
    || { zoo_fail "probe build: $(tail -2 "$ZOO_LOGS/$NAME.probe.err")"; zoo_record "$NAME" FAIL "" "probe-build"; exit 1; }

MINFEE=$(cardano-cli conway transaction calculate-min-fee \
    --tx-body-file "$PROBE" \
    --protocol-params-file "$(zoo_pparams_file)" \
    --witness-count 1 \
    --output-json 2>/dev/null | jq -r '.fee // empty')
if [ -z "$MINFEE" ] || [ "$MINFEE" -le 0 ] 2>/dev/null; then
    zoo_fail "could not compute reference min fee"
    zoo_record "$NAME" FAIL "" "no-reference-minfee"
    exit 1
fi
zoo_info "  reference min fee (cardano-cli calculate-min-fee): $MINFEE"

# expect_fee_rejection <desc> <signed-file> <want-amount>
# Local mirror of 16-cert-negatives/_cert-neg-helper.sh's expect_cert_rejection
# — kept local rather than sourced across categories since that helper's
# ConwayMempoolFailure-degradation message is specific to certificate
# rejections and does not apply to a fee-too-small rejection.
expect_fee_rejection() {
    local desc="$1" signed="$2" want="$3"
    local out rc
    out=$(cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
            --tx-file "$signed" 2>&1) && rc=0 || rc=1
    if [ "$rc" -eq 0 ]; then
        zoo_fail "$desc: ACCEPTED — expected FeeTooSmallUTxO rejection"
        return 1
    fi
    if ! printf '%s' "$out" | grep -qiE 'fee|FeeTooSmall'; then
        zoo_fail "$desc: rejected, but not for fee: $(printf '%s' "$out" | head -c 160)"
        return 1
    fi
    # RED-PROOF: drop this grep to accept ANY fee-flavoured rejection text,
    # hiding a node that reports the wrong required amount.
    if ! printf '%s' "$out" | grep -q "$want"; then
        zoo_fail "$desc: rejected for fee, but required amount $want not present in: $(printf '%s' "$out" | head -c 200)"
        return 1
    fi
    zoo_ok "$desc: rejected FeeTooSmallUTxO, required=$want confirmed in wire text"
    return 0
}

# ── Arm B: minFee - 1 — MUST be rejected, naming the exact required fee ────
UNDERFEE=$((MINFEE - 1))
RAW_B="$ZOO_BUILT/$NAME.under.raw"
SIGNED_B="$ZOO_BUILT/$NAME.under.signed"
cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - UNDERFEE))" \
    --fee      "$UNDERFEE" \
    --ttl      "$TTL" \
    --out-file "$RAW_B" >/dev/null 2> "$ZOO_LOGS/$NAME.under.err" \
    || { zoo_fail "under-fee build: $(tail -2 "$ZOO_LOGS/$NAME.under.err")"; zoo_record "$NAME" FAIL "" "build-under"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW_B" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED_B" >/dev/null

if ! expect_fee_rejection "arm-B(minFee-1=$UNDERFEE)" "$SIGNED_B" "$MINFEE"; then
    zoo_record "$NAME" FAIL "" "under-fee-arm-failed required=$MINFEE"
    exit 1
fi

# ── Arm A: exactly minFee — MUST be accepted and included ──────────────────
RAW_A="$ZOO_BUILT/$NAME.exact.raw"
SIGNED_A="$ZOO_BUILT/$NAME.exact.signed"
cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - MINFEE))" \
    --fee      "$MINFEE" \
    --ttl      "$TTL" \
    --out-file "$RAW_A" >/dev/null 2> "$ZOO_LOGS/$NAME.exact.err" \
    || { zoo_fail "exact-fee build: $(tail -2 "$ZOO_LOGS/$NAME.exact.err")"; zoo_record "$NAME" FAIL "" "build-exact"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW_A" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED_A" >/dev/null
TXID=$(zoo_submit "$SIGNED_A") || {
    zoo_fail "dugite REJECTED a tx paying exactly the reference min fee ($MINFEE)"
    zoo_record "$NAME" FAIL "" "exact-fee-rejected=$MINFEE"
    exit 1
}

if zoo_wait_inclusion "$TXID" 60; then
    zoo_record "$NAME" PASS "$TXID" "fee-bracketed-exactly minFee=$MINFEE"
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"
    exit 1
fi
