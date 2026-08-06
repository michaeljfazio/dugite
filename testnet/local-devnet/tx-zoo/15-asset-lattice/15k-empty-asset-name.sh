#!/usr/bin/env bash
# 15k — mint an asset with a ZERO-LENGTH asset name.
#
# CDDL `asset_name = bytes .size (0..32)` — zero bytes is a legal asset name,
# and cardano-cli's value grammar spells it as the bare policy id with no
# trailing `.<hex>` suffix. Every other positive mint in the zoo names its
# asset; this is the only one that omits the name entirely, so it is the
# only coverage of the empty-name arm of the asset-name length check on both
# the encode and decode side.
#
# Upstream: cardano-node-tests test_native_tokens.py — empty-asset-name minting.
#
# First live run recorded FAIL "mint-build" with the captured error reading
# only "runClientCommand, called at app/cardano-cli.hs:58:14 in
# cardano-cli-11.0.0.0-...:Main" — the tail of a Haskell CallStack, not the
# actual "Error: ..." line, because the old capture used `tail -2` (fixed
# below to print the first non-blank lines instead).
#
# That truncation left the real cause unprovable from the log alone, so both
# candidate syntaxes were tested directly against a live UTxO on this devnet:
# `--mint "5 <policyid>"` (bare, no suffix) and `--mint "5 <policyid>."`
# (trailing dot) both build successfully under cardano-cli 11.0.0.0 and both
# produce the IDENTICAL on-chain asset — `cardano-cli debug transaction view`
# shows `"policy <id>": {"default asset": 5}` either way, i.e. the CLI treats
# a trailing "." as an empty name suffix, same as omitting it outright. So
# this was never a syntax rejection. The one build error actually reproduced
# while investigating was "The UTxO is empty" against a since-spent --tx-in —
# a stale-UTxO race, the same "11c lesson, #918" class as 04i/11f: this script
# had no `zoo_wait_mempool_quiet` guard while its siblings in the same batch
# do, so a `--tx-in` picked right after another script's tx could already be
# gone by the time `transaction build` re-queries the node.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet

# Earlier scripts may still have transactions in flight against the shared
# genesis funder; building on a UTxO the ledger view reports but that a
# pending tx has already claimed (or a since-included tx has already spent)
# is an unavoidable build/submit failure (the 11c lesson, #918).
zoo_wait_mempool_quiet 90 || true

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
read -r POLICY POLICY_ID <<<"$(mint_policy "$NAME")"

# The empty-name asset: bare policy id, no `.<hex>` suffix.
ASSET="${POLICY_ID}"
QTY=5

# empty_asset_qty <sock> <addr> — sum of the empty-name asset's quantity
# across every UTxO at <addr>, on <sock>. `.value[$p][""]` reads the
# zero-length-key entry in the per-policy asset map; `// 0` covers both "no
# UTxO holds this policy" and "this UTxO holds the policy but not the empty
# name" (both come back null from jq, not a jq error).
empty_asset_qty() {
    local sock="$1" addr="$2"
    cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
        --address "$addr" --output-json 2>/dev/null \
      | jq --arg p "$POLICY_ID" '[.[].value[$p][""] // 0] | add // 0' 2>/dev/null || echo 0
}

# Baseline BEFORE this run's mint — reruns must not accumulate into a wrong
# assertion, so every check below is stated relative to this baseline rather
# than an absolute constant.
BASE=$(empty_asset_qty "$ZOO_SOCKET" "$ADDR")

# --- mint QTY of the empty-name asset ---
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.mint.raw"; SIGNED="$ZOO_BUILT/$NAME.mint.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${UTXO%% *}" \
    --tx-out "${ADDR}+3000000 + ${QTY} ${ASSET}" \
    --change-address "$ADDR" \
    --mint "${QTY} ${ASSET}" --mint-script-file "$POLICY" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.mint.err" \
    || { zoo_fail "mint build: $(grep -m2 -v '^[[:space:]]*$' "$ZOO_LOGS/$NAME.mint.err" | tr '\n' ' ')"; zoo_record "$NAME" FAIL "" "mint-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null
MINT_TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "mint-submit"; exit 1; }
zoo_wait_all_observers "$MINT_TXID" 120 "$ADDR" \
    || { zoo_record "$NAME" FAIL "$MINT_TXID" "mint-not-included"; exit 1; }

# Assert the minted value is visible IDENTICALLY on all 3 observers before
# proceeding to burn — a divergence caught here is a real encode/decode bug,
# not a propagation race (zoo_wait_all_observers already blocked on all 3).
WANT_AFTER_MINT=$((BASE + QTY))
for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$sock" ] || continue
    GOT=$(empty_asset_qty "$sock" "$ADDR")
    # RED-PROOF: change WANT_AFTER_MINT (or GOT) to an off-by-one value once
    # — this comparison must then FAIL even though the mint tx really did
    # land, proving the check reads the actual empty-name quantity and not
    # merely "some asset exists under this policy".
    if [ "${GOT:-0}" -ne "$WANT_AFTER_MINT" ]; then
        zoo_fail "empty-asset-name qty mismatch on $sock: want=$WANT_AFTER_MINT got=${GOT:-0}"
        zoo_record "$NAME" FAIL "$MINT_TXID" "mint-qty-mismatch-$sock"
        exit 1
    fi
done
zoo_ok "empty-name asset qty=$WANT_AFTER_MINT identical on all 3 observers"

# --- burn QTY of the same empty-name asset (second tx) ---
BURN_IN=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --address "$ADDR" --output-json 2>/dev/null \
  | jq -r --arg p "$POLICY_ID" 'to_entries | map(select(.value.value[$p][""] // 0 > 0)) | .[0].key // empty')
[ -n "$BURN_IN" ] || { zoo_fail "minted empty-name asset not found for burn"; zoo_record "$NAME" FAIL "$MINT_TXID" "burn-input-missing"; exit 1; }
FEE_UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-fee-utxo"; exit 1; }

BRAW="$ZOO_BUILT/$NAME.burn.raw"; BSIGNED="$ZOO_BUILT/$NAME.burn.signed"
cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
    --tx-in "${FEE_UTXO%% *}" --tx-in "$BURN_IN" \
    --change-address "$ADDR" \
    --mint "-${QTY} ${ASSET}" --mint-script-file "$POLICY" \
    --out-file "$BRAW" >/dev/null 2> "$ZOO_LOGS/$NAME.burn.err" \
    || { zoo_fail "burn build: $(grep -m2 -v '^[[:space:]]*$' "$ZOO_LOGS/$NAME.burn.err" | tr '\n' ' ')"; zoo_record "$NAME" FAIL "$MINT_TXID" "burn-build"; exit 1; }
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$BRAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$BSIGNED" >/dev/null
BURN_TXID=$(zoo_submit "$BSIGNED") || { zoo_record "$NAME" FAIL "$MINT_TXID" "burn-submit"; exit 1; }
if zoo_wait_all_observers "$BURN_TXID" 120 "$ADDR"; then
    GOT=$(empty_asset_qty "$ZOO_SOCKET" "$ADDR")
    # RED-PROOF: change BASE (or GOT) to a nonzero constant once — this
    # comparison must then FAIL even though the burn tx really did land.
    if [ "${GOT:-0}" -eq "$BASE" ]; then
        zoo_ok "empty-name asset back to baseline=$BASE after burn (gone)"
        zoo_record "$NAME" PASS "$BURN_TXID" "mint=$QTY burn=$QTY empty-name-asset baseline-restored"
    else
        zoo_fail "empty-name asset not fully burned: want=$BASE got=${GOT:-0}"
        zoo_record "$NAME" FAIL "$BURN_TXID" "burn-incomplete-got-${GOT:-0}"
        exit 1
    fi
else
    zoo_record "$NAME" FAIL "$BURN_TXID" "burn-not-included"; exit 1
fi
