#!/usr/bin/env bash
# 15m — an output carrying tokens but insufficient ADA is rejected by the
# TOKEN-BUNDLE minUTxO arm.
#
# Distinct from 08-negative/08a-min-utxo-violation.sh: 08a builds a plain
# ADA-only output (no multi-asset value at all) below the flat
# `minUTxOValue`/`(160+size) x coinsPerUTxOByte` floor. This script's output
# carries a real asset bundle, so the minimum lovelace it must satisfy is
# computed from the SERIALIZED SIZE of that bundled value (base overhead +
# per-policy + per-asset-name terms), which is always well above the flat
# floor 08a exercises — the two arms of the same rule, never the same code
# path. Getting the bundle-size term wrong is invisible to any ADA-only test.
#
# `transaction build` silently raises the lovelace on an under-valued output
# to the computed minimum instead of building the violating tx, so this uses
# build-raw (same reason 08a does).
#
# Upstream: cardano-node-tests test_native_tokens.py — "UTxO with tokens but
# no ADA must fail".
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# PIN: verify against live cardano-node during devnet verification. This is
# the Babbage-family token-bundle minUTxO predicate failure
# (ConwayUtxoPredFailure carrying the Babbage `OutputTooSmallUTxO` arm); the
# exact wire constructor name must be confirmed against a real cardano-node
# rejection and corrected here if it differs.
CONSTRUCTOR="BabbageOutputTooSmallUTxO"

read -r POLICY POLICY_ID <<<"$(mint_policy "$NAME")"
ASSET="${POLICY_ID}.$(printf 'TOKONLY' | xxd -p | tr -d '\n')"
TOKEN_QTY=5
TOKEN_UTXO_ADA=3000000   # lovelace co-located with the tokens by the setup tx

# expect_output_rejection <name> <signed-tx> <expected-constructor> — same
# three-outcome shape as _cert-neg-helper.sh's expect_cert_rejection
# (accepted=FAIL, named=PASS, wrong-reason=FAIL), generalised to UTXO-level
# constructor names instead of DELEG/POOL ones. Kept local to this script:
# _lattice-helper.sh is shared by every 15x script and this shape is
# specific to output-value rejections, not the asset-lattice category.
expect_output_rejection() {
    local name="$1" signed="$2" want="$3"
    local out rc
    out=$(cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
            --tx-file "$signed" 2>&1) && rc=0 || rc=1

    if [ "$rc" -eq 0 ]; then
        zoo_fail "$name: ACCEPTED — expected rejection with $want"
        zoo_record "$name" FAIL "" "accepted-expected-$want"
        return 1
    fi
    if echo "$out" | grep -q "$want"; then
        zoo_ok "$name: rejected with $want"
        zoo_record "$name" PASS "" "rejected-$want"
        return 0
    fi
    if echo "$out" | grep -q 'ConwayMempoolFailure'; then
        zoo_fail "$name: degraded to ConwayMempoolFailure instead of $want (#979)"
        zoo_record "$name" FAIL "" "degraded-to-mempool-failure-want-$want"
        return 1
    fi
    local got
    got=$(echo "$out" | grep -oE '\(Conway[A-Za-z]*Failure[^)]*|[A-Za-z]*OutputTooSmall[A-Za-z]*|[A-Za-z]*UTXO' \
          | head -1 | tr -d ',' | cut -c1-110)
    [ -z "$got" ] && got=$(echo "$out" | grep -m1 Error | tr -d ',' | cut -c1-110)
    zoo_fail "$name: rejected, but as '${got}' not $want"
    zoo_record "$name" FAIL "" "rejected-wrong-reason-want-$want"
    return 1
}

# --- setup: mint a token bundle to move around ---
U0=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${U0%% *}" --tx-out "${ADDR}+${TOKEN_UTXO_ADA} + ${TOKEN_QTY} ${ASSET}" --change-address "$ADDR" \
    --mint "${TOKEN_QTY} ${ASSET}" --mint-script-file "$POLICY" \
    --out-file "$ZOO_BUILT/$NAME-setup.raw" >/dev/null 2> "$ZOO_LOGS/$NAME-setup.err" \
    || { zoo_fail "setup build: $(tail -2 "$ZOO_LOGS/$NAME-setup.err")"; zoo_record "$NAME" FAIL "" "setup-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$ZOO_BUILT/$NAME-setup.raw" --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file "$ZOO_BUILT/$NAME-setup.signed" >/dev/null
T0=$(zoo_submit "$ZOO_BUILT/$NAME-setup.signed") || { zoo_record "$NAME" FAIL "" "setup-submit"; exit 1; }
zoo_wait_inclusion "$T0" 90 "$ADDR" >/dev/null 2>&1 || { zoo_record "$NAME" FAIL "$T0" "setup-not-included"; exit 1; }

# --- the test: move the token bundle into an output with only 1 lovelace ---
TOKEN_UTXO=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --address "$ADDR" --output-json 2>/dev/null \
  | jq -r --arg p "$POLICY_ID" 'to_entries | map(select(.value.value[$p] // {} | length > 0)) | .[0].key // empty')
[ -n "$TOKEN_UTXO" ] || { zoo_fail "minted token bundle not found"; zoo_record "$NAME" FAIL "$T0" "token-utxo-missing"; exit 1; }
FEE_UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-fee-utxo"; exit 1; }
if [ "$TOKEN_UTXO" = "${FEE_UTXO%% *}" ]; then
    FEE_UTXO=$(zoo_utxo_at "$ADDR" 1) || { zoo_record "$NAME" FAIL "" "no-second-utxo"; exit 1; }
fi
FEE_AMT=${FEE_UTXO##* }
TIP=$(zoo_tip_slot)
TTL=$((TIP + 600))
FEE=200000
TINY=1   # 1 lovelace against a token-bearing output — far below the
         # token-bundle minUTxO no matter how few assets are carried.
CHANGE=$(( TOKEN_UTXO_ADA + FEE_AMT - TINY - FEE ))

RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build-raw \
    --tx-in     "${TOKEN_UTXO}" \
    --tx-in     "${FEE_UTXO%% *}" \
    --tx-out    "${ADDR}+${TINY} + ${TOKEN_QTY} ${ASSET}" \
    --tx-out    "${ADDR}+${CHANGE}" \
    --fee       "$FEE" \
    --ttl       "$TTL" \
    --out-file  "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null

# RED-PROOF: change $CONSTRUCTOR to a different (wrong) constructor name once
# — expect_output_rejection's wrong-reason branch must then FAIL even though
# the tx is still correctly rejected on-chain.
expect_output_rejection "$NAME" "$SIGNED" "$CONSTRUCTOR"
