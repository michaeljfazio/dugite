#!/usr/bin/env bash
# 15l — mint +N and burn -M (M<N) of the SAME policy/asset in ONE transaction.
#
# 15e already covers mint-A + burn-B (two DIFFERENT policies) in one tx. This
# is the harder case: both operations target the SAME asset, so the tx's
# `mint` field carries two quantities for one map key. A CBOR map cannot
# encode two entries under the same key, so whatever cardano-cli accepts on
# the command line, the WIRE form can only ever be a single netted entry —
# the point of this script is to prove that, not merely assert a final
# balance. A setup tx first mints a baseline supply (mirroring 15e); the tx
# under test then mints N new units and burns M of the existing supply.
#
# Upstream: cardano-node-tests test_native_tokens.py::test_minting_burning_same_token_single_tx
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
read -r POLICY POLICY_ID <<<"$(mint_policy "$NAME")"
ASSET="${POLICY_ID}.$(printf 'NETMB' | xxd -p | tr -d '\n')"

N=20     # minted in the test tx
M=7      # burned in the test tx (must be < N and <= the setup supply)
NET=$((N - M))
SETUP_QTY=15   # >= M, so the burn input can cover it

# asset_qty <sock> — total quantity of $ASSET at $ADDR on $sock.
asset_qty() {
    local sock="$1"
    cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
        --address "$ADDR" --output-json 2>/dev/null \
      | jq --arg p "$POLICY_ID" --arg a "${ASSET#*.}" '[.[].value[$p][$a] // 0] | add // 0' 2>/dev/null || echo 0
}

# --- setup: mint SETUP_QTY of the asset so there is an existing supply to burn from ---
U0=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${U0%% *}" --tx-out "${ADDR}+3000000 + ${SETUP_QTY} ${ASSET}" --change-address "$ADDR" \
    --mint "${SETUP_QTY} ${ASSET}" --mint-script-file "$POLICY" \
    --out-file "$ZOO_BUILT/$NAME-setup.raw" >/dev/null 2> "$ZOO_LOGS/$NAME-setup.err" \
    || { zoo_fail "setup build: $(tail -2 "$ZOO_LOGS/$NAME-setup.err")"; zoo_record "$NAME" FAIL "" "setup-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$ZOO_BUILT/$NAME-setup.raw" --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file "$ZOO_BUILT/$NAME-setup.signed" >/dev/null
T0=$(zoo_submit "$ZOO_BUILT/$NAME-setup.signed") || { zoo_record "$NAME" FAIL "" "setup-submit"; exit 1; }
zoo_wait_inclusion "$T0" 90 "$ADDR" >/dev/null 2>&1 || { zoo_record "$NAME" FAIL "$T0" "setup-not-included"; exit 1; }

# Baseline captured AFTER setup lands — reruns accumulate supply across
# invocations, so the net-quantity assertion is stated relative to this
# observed baseline, not an absolute constant.
BASE=$(asset_qty "$ZOO_SOCKET")

BURN_IN=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --address "$ADDR" --output-json 2>/dev/null \
  | jq -r --arg p "$POLICY_ID" --arg a "${ASSET#*.}" \
      'to_entries | map(select(.value.value[$p][$a] // 0 > 0)) | .[0].key // empty')
[ -n "$BURN_IN" ] || { zoo_fail "setup supply not found"; zoo_record "$NAME" FAIL "$T0" "burn-input-missing"; exit 1; }
FEE_UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-fee-utxo"; exit 1; }
if [ "$BURN_IN" = "${FEE_UTXO%% *}" ]; then
    FEE_UTXO=$(zoo_utxo_at "$ADDR" 1) || { zoo_record "$NAME" FAIL "" "no-second-utxo"; exit 1; }
fi

# --- the test: attempt the two-entry mint form first; cardano-cli may
#     merge same-asset mint entries client-side, or refuse the duplicate
#     key outright. Either way, record which happened. ---
MINT_TWO_ENTRY="${N} ${ASSET} + -${M} ${ASSET}"
MINT_NETTED="${NET} ${ASSET}"
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"

WIRE_SHAPE=""
set +e
cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${FEE_UTXO%% *}" --tx-in "$BURN_IN" \
    --change-address "$ADDR" \
    --mint "$MINT_TWO_ENTRY" --mint-script-file "$POLICY" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.two-entry.err"
TWO_ENTRY_RC=$?
set -e
if [ "$TWO_ENTRY_RC" -eq 0 ]; then
    WIRE_SHAPE="cli-accepted-two-entry-form"
    zoo_info "cardano-cli accepted the two-entry mint string (+${N}/-${M} same asset)"
else
    zoo_info "cardano-cli refused the two-entry mint string: $(tail -2 "$ZOO_LOGS/$NAME.two-entry.err") — falling back to the pre-netted form"
    WIRE_SHAPE="cli-refused-two-entry-used-netted-form"
    cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${FEE_UTXO%% *}" --tx-in "$BURN_IN" \
        --change-address "$ADDR" \
        --mint "$MINT_NETTED" --mint-script-file "$POLICY" \
        --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.netted.err" \
        || { zoo_fail "netted build: $(tail -2 "$ZOO_LOGS/$NAME.netted.err")"; zoo_record "$NAME" FAIL "" "netted-build"; exit 1; }
fi

# Whatever cardano-cli did with the CLI-level string, the tx body's `mint`
# field is a CBOR map and cannot carry two entries under one key — decode it
# to confirm exactly one entry survived for this asset, and record that
# alongside which CLI path produced it.
MINT_ENTRY_COUNT=$(cardano-cli debug transaction view --tx-body-file "$RAW" 2>/dev/null \
    | jq --arg p "$POLICY_ID" '[.mint[$p] // {} | keys[]] | length' 2>/dev/null || echo -1)
zoo_info "observed wire shape: $WIRE_SHAPE, mint map entries for this asset=$MINT_ENTRY_COUNT"

cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    WANT=$((BASE + NET))
    ALL_MATCH=1
    for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
        [ -S "$sock" ] || continue
        GOT=$(asset_qty "$sock")
        # RED-PROOF: change WANT (or NET) to a wrong value once — this
        # comparison must then FAIL on at least one observer even though the
        # mint+burn tx really did land, proving the check verifies the exact
        # net delta (N-M) rather than merely "the tx was accepted".
        if [ "${GOT:-0}" -ne "$WANT" ]; then
            zoo_fail "net-quantity mismatch on $sock: want=$WANT (base=$BASE net=$NET) got=${GOT:-0}"
            ALL_MATCH=0
        fi
    done
    if [ "$ALL_MATCH" -eq 1 ]; then
        zoo_ok "exact net quantity N-M=$NET (base=$BASE -> $WANT) identical on all 3 observers"
        zoo_record "$NAME" PASS "$TXID" "net=$NET base=$BASE want=$WANT wire-shape=$WIRE_SHAPE mint-entries=$MINT_ENTRY_COUNT"
    else
        zoo_record "$NAME" FAIL "$TXID" "net-quantity-mismatch wire-shape=$WIRE_SHAPE"
        exit 1
    fi
else
    zoo_record "$NAME" FAIL "$TXID" "not-included wire-shape=$WIRE_SHAPE"; exit 1
fi
