#!/usr/bin/env bash
# 03m — #733 regression test: a Plutus tx whose validity upper bound (TTL)
# lies past the safe-zone time-translation horizon MUST be rejected at
# mempool admission, for BOTH is_valid polarities.
#
# Haskell semantics: building the script context translates the validity
# interval to POSIX time via the horizon-limited EpochInfo; a past-horizon
# bound fails with TimeTranslationPastHorizon, which is a CollectError
# (UtxosFailure CollectErrors BadTranslation) — a hard rejection raised
# BEFORE script evaluation regardless of the is_valid tag. Pre-fix, dugite
# admitted such txs, forged them, and the Haskell BP rejected the block and
# wedged (devnet QA failure, 2026-06-10).
#
# TTL = tip + 2000: the devnet horizon distance ranges ~241-640 slots
# (safe zone 240, epoch 400, rounded up to the epoch boundary), so +2000 is
# deterministically past-horizon at every tip position.
#
# The test PASSES only if BOTH submissions are rejected at admission.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_skip "missing $SCRIPT"; zoo_record "$NAME" SKIP; exit 0; }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
PPARAMS=$(zoo_pparams_file)
FEE=500000

# One locked UTxO per polarity — each submission attempt needs its own,
# since a rejected spend leaves the UTxO intact but a (buggy) accepted one
# would consume it.
build_and_submit_expect_reject() {
    local polarity="$1"  # "valid" (plain) or "invalid" (--script-invalid)
    local pair script_txin script_amt collat_raw collat collat_amt return_amt
    pair=$(plutus_lock "$SCRIPT" inline 5000000) || return 2
    script_txin=${pair%% *}
    script_amt=${pair##* }

    collat_raw=$(zoo_utxo_at "$ADDR" 1) || return 2
    collat=${collat_raw%% *}
    collat_amt=${collat_raw##* }
    return_amt=$((collat_amt - 2000000))
    [ "$return_amt" -lt 1000000 ] && return 3

    local tip ttl raw signed reg_out
    tip=$(zoo_tip_slot)
    ttl=$((tip + 2000))
    raw="$ZOO_BUILT/$NAME.$polarity.raw"
    signed="$ZOO_BUILT/$NAME.$polarity.signed"
    reg_out=$((script_amt - FEE))

    local extra=()
    [ "$polarity" = "invalid" ] && extra+=(--script-invalid)

    cardano-cli conway transaction build-raw \
        --tx-in         "$script_txin" \
        --tx-in-script-file "$SCRIPT" \
        --tx-in-inline-datum-present \
        --tx-in-redeemer-file "$REDEEMER" \
        --tx-in-execution-units "(1000000,1000000)" \
        --tx-in-collateral  "$collat" \
        --tx-total-collateral "$((collat_amt - return_amt))" \
        --tx-out-return-collateral "${ADDR}+${return_amt}" \
        --tx-out        "${ADDR}+${reg_out}" \
        --fee           "$FEE" \
        --ttl           "$ttl" \
        "${extra[@]}" \
        --protocol-params-file "$PPARAMS" \
        --out-file      "$raw" >/dev/null
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file  "$raw" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file      "$signed" >/dev/null

    # Past-horizon TTL must be rejected at admission (CollectError /
    # TimeTranslationPastHorizon) — for BOTH polarities.
    if TXID=$(zoo_submit "$signed" 2>/dev/null); then
        zoo_record "$NAME" FAIL "$TXID" \
            "past-horizon TTL ($polarity polarity) admitted — must reject (#733)"
        return 1
    fi
    return 0
}

rc=0
build_and_submit_expect_reject "valid" || rc=$?
case "$rc" in
    2) zoo_record "$NAME" FAIL "" "setup (valid polarity)"; exit 1 ;;
    3) zoo_skip "collateral utxo too small"; zoo_record "$NAME" SKIP; exit 0 ;;
    1) exit 1 ;;
esac

rc=0
build_and_submit_expect_reject "invalid" || rc=$?
case "$rc" in
    2) zoo_record "$NAME" FAIL "" "setup (invalid polarity)"; exit 1 ;;
    3) zoo_skip "collateral utxo too small"; zoo_record "$NAME" SKIP; exit 0 ;;
    1) exit 1 ;;
esac

zoo_record "$NAME" PASS "" "past-horizon TTL rejected at admission, both polarities (#733)"
