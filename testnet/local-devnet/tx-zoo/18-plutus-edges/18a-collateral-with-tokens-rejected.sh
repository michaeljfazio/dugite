#!/usr/bin/env bash
# 18a — collateral input carrying native tokens.
#
# Upstream: tests_plutus/test_spend_negative_build.py::test_collateral_w_tokens
#
# Cardano's Alonzo collateral rule requires the collateral UTxO's NET value
# (inputs minus collateral_return) to be pure ADA — `isAdaOnly` in Haskell's
# `feesOK`. A collateral candidate that also carries a native token must be
# rejected with CollateralContainsNonADA, independent of whether the ADA
# amount itself would otherwise be sufficient.
#
# Mechanism: mint one token to our own address (like 03d), locate the
# resulting ADA+token UTxO, then use THAT as --tx-in-collateral for an
# unrelated Plutus spend.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# RED-PROOF: flip WANT to any other constructor name (e.g. "InsufficientCollateral")
# and this must FAIL — proves the assertion is not vacuously true.
WANT="CollateralContainsNonADA"

# ---- Step 1: mint a token into our own wallet (NoDatum V1 mint policy,
# same variant 03d uses — minting has no datum). ----
MINT_SCRIPT="$ZOO_DIR/lib/plutus/always-true-v1-mint.plutus"
[ -s "$MINT_SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$MINT_SCRIPT")"; exit 0; }

MINT_UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
MINT_COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
POLICY_ID=$(cardano-cli conway transaction policyid --script-file "$MINT_SCRIPT")
ASSET_NAME_HEX="$(printf 'TXZOOCOLTOK' | xxd -p | tr -d '\n')"
ASSET="${POLICY_ID}.${ASSET_NAME_HEX}"
MINT_REDEEMER="$ZOO_BUILT/$NAME.mint-redeemer.json"
echo '{"int": 0}' > "$MINT_REDEEMER"

MINT_RAW="$ZOO_BUILT/$NAME-mint.raw"
MINT_SIGNED="$ZOO_BUILT/$NAME-mint.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "${MINT_UTXO%% *}" \
    --tx-in-collateral "$MINT_COLLAT" \
    --tx-out        "${ADDR}+3000000 + 1 ${ASSET}" \
    --change-address "$ADDR" \
    --mint          "1 ${ASSET}" \
    --mint-script-file "$MINT_SCRIPT" \
    --mint-redeemer-file "$MINT_REDEEMER" \
    --out-file      "$MINT_RAW" >/dev/null 2> "$ZOO_LOGS/$NAME-mint.err" \
    || { zoo_fail "mint build: $(tail -2 "$ZOO_LOGS/$NAME-mint.err")"; zoo_record "$NAME" FAIL "" "mint-build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" --tx-body-file "$MINT_RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" --out-file "$MINT_SIGNED" >/dev/null
MINT_TXID=$(zoo_submit "$MINT_SIGNED") || { zoo_record "$NAME" FAIL "" "mint-submit"; exit 1; }
zoo_wait_inclusion "$MINT_TXID" 90 || { zoo_record "$NAME" FAIL "$MINT_TXID" "mint-not-included"; exit 1; }

TMP=$(mktemp)
cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --address "$ADDR" --out-file "$TMP"
TOKEN_TXIN=$(jq -r --arg t "$MINT_TXID" --arg p "$POLICY_ID" '
    to_entries
    | map(select(.key | startswith($t)))
    | map(select(.value.value[$p] != null))
    | .[0].key // empty' "$TMP")
rm -f "$TMP"
[ -z "$TOKEN_TXIN" ] && { zoo_fail "could not locate the token-bearing UTxO"; zoo_record "$NAME" FAIL "$MINT_TXID" "no-token-utxo"; exit 1; }

# ---- Step 2: an unrelated Plutus spend, collateralised by the token UTxO. ----
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }
PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
RAW="$ZOO_BUILT/$NAME.raw"

if cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$SCRIPT_TXIN" \
        --tx-in-script-file "$SCRIPT" \
        --tx-in-inline-datum-present \
        --tx-in-redeemer-file "$REDEEMER" \
        --tx-in-collateral "$TOKEN_TXIN" \
        --tx-out "${ADDR}+2000000" \
        --change-address "$ADDR" \
        --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
    SIGNED="$ZOO_BUILT/$NAME.signed"
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null
    expect_utxo_rejection "$NAME" "$SIGNED" "$WANT"
    exit $?
fi

# cardano-cli's `transaction build` can refuse locally once it resolves the
# collateral UTxO's composition — that is still the rule firing, just at the
# client instead of the node.
if grep -qi "$WANT\|non-ada\|non ada\|only.*ada\|ada.*only" "$ZOO_LOGS/$NAME.err"; then
    zoo_ok "$NAME: refused at build ($WANT)"
    zoo_record "$NAME" PASS "" "rejected-$WANT-at-build"
    exit 0
fi
zoo_fail "$NAME: build failed for a reason unrelated to $WANT: $(tail -2 "$ZOO_LOGS/$NAME.err")"
zoo_record "$NAME" FAIL "" "build-failed-not-$WANT"
exit 1
