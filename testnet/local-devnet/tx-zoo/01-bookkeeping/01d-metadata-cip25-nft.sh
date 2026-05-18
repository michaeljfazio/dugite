#!/usr/bin/env bash
# 01d — CIP-25 NFT metadata at label 721. No minting in this script; for that
# see 02-native-scripts (CIP-25 metadata is just a documentation convention,
# not a separate ledger op). This verifies the label is preserved.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_fail "no UTxO"; zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}

# Per CIP-25: { 721: { <policy_id>: { <asset_name>: { name, image, ... } } } }
# We use a placeholder policy_id since no minting is happening — the metadata
# is just attached to a payment tx so the wire shape is exercised.
META="$ZOO_BUILT/$NAME.meta.json"
cat > "$META" <<EOF
{
  "721": {
    "0000000000000000000000000000000000000000000000000000000000": {
      "tx-zoo-nft": {
        "name": "tx-zoo demo NFT",
        "image": "ipfs://QmExampleHashGoesHereExampleHashGoesHereEx",
        "description": "Reference CIP-25 metadata bundled in a payment tx.",
        "mediaType": "image/png"
      }
    },
    "version": "1.0"
  }
}
EOF

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-out        "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --metadata-json-file "$META" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" || zoo_record "$NAME" FAIL "$TXID" "not-included"
