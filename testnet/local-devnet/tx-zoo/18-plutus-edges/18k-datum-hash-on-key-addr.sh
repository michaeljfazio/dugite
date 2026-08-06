#!/usr/bin/env bash
# 18k — a datum hash attached to a KEY-credential (non-script) output, then
# spent normally. ACCEPTED for both txs.
#
# Upstream: test_tx_basic.py::test_utxo_with_datum_hash +
# test_datum_on_key_credential_address.
#
# A datum hash is legal metadata on ANY output per the Alonzo CDDL
# (`datum_option = [0, $hash32] / [1, data]`) — it does not require the
# output's payment credential to be a script. Spending it later needs no
# Plutus witnessing at all: datum hashes are informational for key-locked
# outputs, not an authorization requirement.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

DATUM_FILE="$ZOO_BUILT/$NAME.datum.json"
echo '{"int": 7}' > "$DATUM_FILE"
DHASH=$(cardano-cli conway transaction hash-script-data --script-data-file "$DATUM_FILE")

# ---- Tx1: pay to our OWN key address, attaching a datum hash. ----
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW1="$ZOO_BUILT/$NAME-1.raw"
SIGNED1="$ZOO_BUILT/$NAME-1.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${UTXO%% *}" \
    --tx-out "${ADDR}+3000000" \
    --tx-out-datum-hash "$DHASH" \
    --change-address "$ADDR" \
    --out-file "$RAW1" >/dev/null 2> "$ZOO_LOGS/$NAME-1.err" \
    || { zoo_fail "tx1 build: $(tail -2 "$ZOO_LOGS/$NAME-1.err")"; zoo_record "$NAME" FAIL "" "tx1-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW1" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED1" >/dev/null
TXID1=$(zoo_submit "$SIGNED1") || { zoo_record "$NAME" FAIL "" "tx1-submit"; exit 1; }
zoo_wait_inclusion "$TXID1" 90 || { zoo_record "$NAME" FAIL "$TXID1" "tx1-not-included"; exit 1; }

# Locate the specific output carrying the datum hash.
TMP=$(mktemp)
cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --address "$ADDR" --out-file "$TMP"
DATUM_TXIN=$(jq -r --arg t "$TXID1" --arg h "$DHASH" '
    to_entries
    | map(select(.key | startswith($t)))
    | map(select(.value.datumhash == $h))
    | .[0].key // empty' "$TMP")
rm -f "$TMP"
[ -z "$DATUM_TXIN" ] && {
    zoo_fail "could not locate the datum-hash-bearing output at the key address"
    zoo_record "$NAME" FAIL "$TXID1" "no-datum-hash-output"; exit 1
}

# ---- Tx2: spend it back like any plain payment — no script, no datum
# witness needed; the hash is purely informational for a key credential. ----
RAW2="$ZOO_BUILT/$NAME-2.raw"
SIGNED2="$ZOO_BUILT/$NAME-2.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "$DATUM_TXIN" \
    --tx-out "${ADDR}+1000000" \
    --change-address "$ADDR" \
    --out-file "$RAW2" >/dev/null 2> "$ZOO_LOGS/$NAME-2.err" \
    || { zoo_fail "tx2 build: $(tail -2 "$ZOO_LOGS/$NAME-2.err")"; zoo_record "$NAME" FAIL "" "tx2-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW2" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED2" >/dev/null
# RED-PROOF: swap ADDR for a SCRIPT address at tx1 time without also
# providing the script witness at tx2 — must FAIL, proving tx2's success
# genuinely depends on the credential being a KEY, not on any datum leniency.
TXID2=$(zoo_submit "$SIGNED2") || { zoo_record "$NAME" FAIL "" "tx2-submit"; exit 1; }
if zoo_wait_all_observers "$TXID2" 120 "$ADDR"; then
    zoo_record "$NAME" PASS "$TXID2" "datum-hash on key addr, spent normally (lock=$TXID1)"
else
    zoo_record "$NAME" FAIL "$TXID2" "not-included"; exit 1
fi
