#!/usr/bin/env bash
# 15h — a metadata-bearing tx must land under the txid computed locally.
#
# The zoo has always SUBMITTED metadata and never checked anything about it
# afterwards, so a metadata encoding that differed from Haskell's was invisible
# as long as the transaction was accepted.
#
# What this actually proves, stated precisely because no LSQ query returns a
# transaction's metadata:
#
#   txid = blake2b256(tx_body_cbor), and the body carries
#   auxiliary_data_hash = blake2b256(metadata_cbor)
#
# So the tx appearing under the LOCALLY computed txid on both dugite and
# cardano-node means both hashed the same metadata bytes to the same aux-data
# hash and the same body to the same id. A divergence surfaces as
# ConflictingMetadataHash at submission, or as the tx never appearing under
# this id — not as a silently different payload.
#
# The metadata deliberately mixes every Metadatum constructor, since #937 found
# three drifted copies of `read_metadatum` that each gated nested maps, lists
# and text on the definite form only.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

META="$ZOO_BUILT/$NAME.meta.json"
cat > "$META" <<'JSON'
{
  "674": {
    "int": 42,
    "negint": -17,
    "text": "dugite metadata round-trip",
    "bytes": "0xdeadbeef",
    "list": [1, "two", [3, 4], {"k": "v"}],
    "map": { "nested": { "deeper": [5, 6] } },
    "long_text": "0123456789012345678901234567890123456789012345678901234567890123456789"
  }
}
JSON

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
if ! cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "${UTXO%% *}" --change-address "$ADDR" \
        --metadata-json-file "$META" \
        --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
    zoo_fail "build failed: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
    zoo_record "$NAME" FAIL "" "build-failed"; exit 1
fi
cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
    --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" --out-file "$SIGNED" >/dev/null

TXID=$(local_txid "$SIGNED")
[ -n "$TXID" ] || { zoo_record "$NAME" FAIL "" "txid-compute-failed"; exit 1; }
zoo_info "$NAME: locally computed txid $TXID"

zoo_submit "$SIGNED" || { zoo_record "$NAME" FAIL "" "submit-rejected"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_ok "$NAME: aux-data hash and txid agree across implementations"
    zoo_record "$NAME" PASS "$TXID" "metadata-hash-round-trip"
else
    zoo_record "$NAME" FAIL "$TXID" "not-included-under-local-txid"; exit 1
fi
