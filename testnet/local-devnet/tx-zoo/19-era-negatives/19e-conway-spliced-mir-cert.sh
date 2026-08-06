#!/usr/bin/env bash
# 19e — Conway tx with a certificate tag SPLICED to 6 (MIR shape) (#1023,
# #1034). This is the permanent on-the-wire regression pin for #1023.
#
# Unlike 19a/19b (a genuine Shelley-era tx, wire era_id=1), this builds a
# VALID Conway transaction (one stake-registration cert, tag 7
# `reg_deposit_cert`), then overwrites JUST that certificate's own tag byte
# from 7 to 6 with tx-cbor-tool.py's `splice-cert-tag` — everything else
# about the certificate (its remaining fields, its arity) is left untouched
# on purpose. cardano-ledger's Conway certificate decoder dispatches on the
# tag integer FIRST and hard-fails immediately for tag 6
# ("MIR certificates are no longer supported") before it would ever look at
# the rest of the array, so a 3-element donor spliced onto a tag whose real
# shape is 2-element `[6, [pot, target]]` is exactly the point: the decoder
# must reject at the TAG, before arity is ever examined
# (crates/dugite-serialization/src/decode/era_conway.rs, #1023).
#
# Confirmed empirically (2026-08, during authoring): `cardano-cli
# conway transaction txid --tx-file <spliced>` itself hard-rejects with the
# EXACT cardano-ledger message "MIR certificates are no longer supported"
# BEFORE ever opening a socket — cardano-cli 11.0.0.0 uses the real ledger
# decoder to read a Conway-tagged tx file. So submission here goes through
# `dugite-cli transaction submit`, which forwards cborHex to the node
# untouched (same precedent as 08-negative/08f-double-spend.sh) — we want
# the rejection to come from the NODE's decoder, not cardano-cli's.
#
# Both observers are expected to refuse the tx, but NOT necessarily with the
# same wire shape: dugite answers a structured MsgRejectTx
# (crates/dugite-network/src/protocol/local_tx_submission/), while a real
# cardano-node may instead DROP THE CONNECTION on certain decode failures —
# the #925 class (`ouroboros-network` codec-level failures do not always
# have a corresponding ApplyTxErr to report). Both are recorded verbatim in
# the detail field rather than pattern-matched against one expected string.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/19-era-negatives/_era-neg-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
era_neg_require_dugite_cli "$NAME" || exit 0

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
BASE_LINE=$(era_neg_conway_stake_reg_base "$NAME" "$ADDR") || exit 0
read -r BASE_BODY BASE_SIGNED <<< "$BASE_LINE"

BEFORE=$(python3 "$ZOO_PY_TX_CBOR" show-certs --in "$BASE_BODY" 2>/dev/null || echo '{}')
BEFORE_TAG=$(printf '%s' "$BEFORE" | jq -r '.cert_tags[0] // "null"')
if [ "$BEFORE_TAG" != "7" ]; then
    zoo_record "$NAME" FAIL "" "base-tx-unexpected-cert-tag=$BEFORE_TAG (wanted 7)"
    exit 1
fi

SPLICED_BODY="$ZOO_BUILT/$NAME-spliced.body"
python3 "$ZOO_PY_TX_CBOR" splice-cert-tag \
    --in "$BASE_BODY" --out "$SPLICED_BODY" --index 0 --tag 6 >/dev/null \
    || { zoo_record "$NAME" FAIL "" "splice-cert-tag-failed"; exit 1; }

AFTER=$(python3 "$ZOO_PY_TX_CBOR" show-certs --in "$SPLICED_BODY" 2>/dev/null || echo '{}')
AFTER_TAG=$(printf '%s' "$AFTER" | jq -r '.cert_tags[0] // "null"')
if [ "$AFTER_TAG" != "6" ]; then
    zoo_record "$NAME" FAIL "" "splice-did-not-land tag=$AFTER_TAG (wanted 6)"
    exit 1
fi
log_info "$NAME: spliced cert tag 7 -> 6 (MIR shape), arity intentionally unchanged"

SIGNED_SPLICED="$ZOO_BUILT/$NAME-spliced.signed"
TXID=$(python3 "$ZOO_PY_TX_CBOR" sign \
        --in "$SPLICED_BODY" --out "$SIGNED_SPLICED" \
        --signing-key-file "$ZOO_PAY_SKEY" 2>/dev/null) \
    || { zoo_record "$NAME" FAIL "" "vendored-signer-failed-on-spliced-body"; exit 1; }
log_info "$NAME: spliced+resigned tx $TXID"

# RED-PROOF: change `--tag 6` above (and the two assertions before it) to
# `--tag 7` — i.e. splice the cert onto ITS OWN current-era tag, a no-op —
# once. That produces an ordinary accepted Conway tx, and
# era_neg_assert_rejected_both must then FAIL with an "accepted where
# rejection was expected" detail line.
era_neg_assert_rejected_both "$NAME" "$SIGNED_SPLICED" "$TXID" era_neg_submit_dugite
