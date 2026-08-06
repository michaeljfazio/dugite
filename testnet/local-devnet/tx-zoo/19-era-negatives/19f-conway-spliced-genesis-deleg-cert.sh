#!/usr/bin/env bash
# 19f — Conway tx with a certificate tag SPLICED to 5 (GenesisKeyDelegation
# shape) (#1023, #1034). The permanent on-the-wire regression pin for
# #1023's OTHER hard-rejected tag.
#
# Identical mechanism to 19e — see its header and the category README.md —
# splicing tag 7 (`reg_deposit_cert`) to 5 instead of 6. cardano-ledger's
# Conway certificate decoder fails immediately on tag 5 with "Genesis
# delegation certificates are no longer supported"
# (crates/dugite-serialization/src/decode/era_conway.rs, #1023), before ever
# looking at the rest of the array — so the 3-element donor certificate
# spliced onto tag 5 (whose real shape is 4-element `[5, genesis_hash,
# genesis_delegate_hash, vrf_hash]`) again proves the decoder rejects at the
# TAG, not the arity.
#
# Confirmed empirically (2026-08, during authoring): `cardano-cli conway
# transaction txid --tx-file <spliced>` itself hard-rejects with the exact
# cardano-ledger message BEFORE ever opening a socket, so — same as 19e —
# submission goes through `dugite-cli transaction submit` (raw-forwarding),
# not cardano-cli.
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
    --in "$BASE_BODY" --out "$SPLICED_BODY" --index 0 --tag 5 >/dev/null \
    || { zoo_record "$NAME" FAIL "" "splice-cert-tag-failed"; exit 1; }

AFTER=$(python3 "$ZOO_PY_TX_CBOR" show-certs --in "$SPLICED_BODY" 2>/dev/null || echo '{}')
AFTER_TAG=$(printf '%s' "$AFTER" | jq -r '.cert_tags[0] // "null"')
if [ "$AFTER_TAG" != "5" ]; then
    zoo_record "$NAME" FAIL "" "splice-did-not-land tag=$AFTER_TAG (wanted 5)"
    exit 1
fi
log_info "$NAME: spliced cert tag 7 -> 5 (GenesisKeyDelegation shape), arity intentionally unchanged"

SIGNED_SPLICED="$ZOO_BUILT/$NAME-spliced.signed"
TXID=$(python3 "$ZOO_PY_TX_CBOR" sign \
        --in "$SPLICED_BODY" --out "$SIGNED_SPLICED" \
        --signing-key-file "$ZOO_PAY_SKEY" 2>/dev/null) \
    || { zoo_record "$NAME" FAIL "" "vendored-signer-failed-on-spliced-body"; exit 1; }
log_info "$NAME: spliced+resigned tx $TXID"

# RED-PROOF: change `--tag 5` above (and the two assertions before it) to
# `--tag 7` — i.e. splice the cert onto ITS OWN current-era tag, a no-op —
# once. That produces an ordinary accepted Conway tx, and
# era_neg_assert_rejected_both must then FAIL with an "accepted where
# rejection was expected" detail line.
era_neg_assert_rejected_both "$NAME" "$SIGNED_SPLICED" "$TXID" era_neg_submit_dugite
