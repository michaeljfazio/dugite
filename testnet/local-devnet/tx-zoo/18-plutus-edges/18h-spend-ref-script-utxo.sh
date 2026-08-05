#!/usr/bin/env bash
# 18h — spend a UTxO that ITSELF carries a reference script (attached to the
# very output being spent), witnessed the ORDINARY way (--tx-in-script-file),
# not via --spending-tx-in-reference. ACCEPTED.
#
# Upstream: test_spend_reference_script.
#
# Different from 03h (which uses a SEPARATE output purely to carry the
# reference script for a DIFFERENT spend via --spending-tx-in-reference).
# Here the reference script sits on the exact output under spend, and the
# witness path used to authorize the spend deliberately ignores it — this
# proves dugite doesn't choke on an output whose fields include an
# (incidental, irrelevant-to-this-witness) referenceScript alongside an
# inline datum when that output is later consumed directly.
#
# Does not go through _lock-helper.sh's plutus_lock: that helper has no way
# to attach --tx-out-reference-script-file, so the lock step is written out
# here directly (same shape as 03h's step 1).
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
SCRIPT_ADDR_FILE="$ZOO_BUILT/$(basename "$SCRIPT" .plutus)-18h.addr"
cardano-cli conway address build \
    --payment-script-file "$SCRIPT" \
    --testnet-magic "$LD_MAGIC" \
    --out-file "$SCRIPT_ADDR_FILE"
SCRIPT_ADDR=$(cat "$SCRIPT_ADDR_FILE")

DATUM_FILE="$ZOO_BUILT/$NAME.datum.json"
echo '{"int": 42}' > "$DATUM_FILE"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }

# ---- Lock: SAME output carries BOTH the inline datum AND the reference
# script it will (incidentally) later be spent with a direct witness for. ----
LOCK_RAW="$ZOO_BUILT/$NAME-lock.raw"
LOCK_SIGNED="$ZOO_BUILT/$NAME-lock.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${UTXO%% *}" \
    --tx-out "${SCRIPT_ADDR}+5000000" \
    --tx-out-inline-datum-file "$DATUM_FILE" \
    --tx-out-reference-script-file "$SCRIPT" \
    --change-address "$ADDR" \
    --out-file "$LOCK_RAW" >/dev/null 2> "$ZOO_LOGS/$NAME-lock.err" \
    || { zoo_fail "lock build: $(tail -2 "$ZOO_LOGS/$NAME-lock.err")"; zoo_record "$NAME" FAIL "" "lock-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$LOCK_RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$LOCK_SIGNED" >/dev/null
LOCK_TXID=$(zoo_submit "$LOCK_SIGNED") || { zoo_record "$NAME" FAIL "" "lock-submit"; exit 1; }
zoo_wait_inclusion "$LOCK_TXID" 90 || { zoo_record "$NAME" FAIL "$LOCK_TXID" "lock-not-included"; exit 1; }

TMP=$(mktemp)
cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --address "$SCRIPT_ADDR" --out-file "$TMP"
SCRIPT_TXIN=$(jq -r --arg t "$LOCK_TXID" '
    to_entries
    | map(select(.key | startswith($t)))
    | map(select(.value.referenceScript != null and .value.inlineDatum != null))
    | .[0].key // empty' "$TMP")
rm -f "$TMP"
[ -z "$SCRIPT_TXIN" ] && {
    zoo_fail "could not locate the locked output carrying BOTH inlineDatum and referenceScript"
    zoo_record "$NAME" FAIL "$LOCK_TXID" "no-lock-output"; exit 1
}

# ---- Spend it directly (NOT via --spending-tx-in-reference). ----
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "$SCRIPT_TXIN" \
    --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present \
    --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-collateral "$COLLAT" \
    --tx-out "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "spend build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "spend-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null

# RED-PROOF: sabotage by breaking the reference-script hash (attach a
# DIFFERENT script at lock time than what's supplied as --tx-in-script-file)
# — must FAIL, proving the script hash IS being checked, not skipped.
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_record "$NAME" PASS "$TXID" "spent a UTxO carrying its own reference script"
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
