#!/usr/bin/env bash
# Shared helpers for 15-asset-lattice.
#
# WHY THIS CATEGORY EXISTS (#961)
# -------------------------------
# Every POSITIVE mint in the zoo was one asset name under one policy. The only
# large asset map anywhere was the NEGATIVE 08m (100 names, expected-reject via
# OutputValueTooLarge). So no positive test ever built a complex value.
#
# That matters because #930 was a false Phase-1 REJECT found on a 324-entry
# asset map: Haskell's `encodeMap` switches from a definite to an indefinite
# CBOR map header above 23 entries, and dugite emitted definite headers
# everywhere, over-counting the serialised size by one byte at >=256 entries and
# tripping maxValSize. The unit layer now pins 23/24/255/256, but the devnet
# gate could not have caught a recurrence on the wire because it never built a
# map that big.
#
# The sizes here are the switch points, not round numbers:
#   23  last definite-length map header  (0xB7)
#   24  first indefinite-length map      (0xBF … 0xFF)
#   256 where the definite header would need 3 bytes (0xB9 xxxx) against the
#       indefinite form's 2 (0xBF + 0xFF) — the exact 1-byte over-count.

# mint_policy <name> — write a single-sig policy for the zoo payment key and
# echo "<policy_file> <policy_id>".
mint_policy() {
    local out="$ZOO_BUILT/$1.policy.json"
    local kh; kh=$(cardano-cli conway address key-hash --payment-verification-key-file "$ZOO_PAY_VKEY")
    cat > "$out" <<JSON
{ "type": "all", "scripts": [ { "type": "sig", "keyHash": "$kh" } ] }
JSON
    printf '%s %s\n' "$out" "$(cardano-cli conway transaction policyid --script-file "$out")"
}

# asset_list <policy_id> <count> <prefix> — echo "N pid.hex + N pid.hex + ..."
# for `count` distinct asset names under one policy.
asset_list() {
    local pid="$1" n="$2" prefix="${3:-TZ}" i out=""
    for i in $(seq 1 "$n"); do
        local hex; hex=$(printf '%s%04d' "$prefix" "$i" | xxd -p | tr -d '\n')
        out+="${out:+ + }1 ${pid}.${hex}"
    done
    printf '%s' "$out"
}

# Count the distinct assets an address holds for a policy, on a given socket.
assets_at() {
    local sock="$1" addr="$2" pid="$3"
    cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
        --address "$addr" --output-json 2>/dev/null \
      | jq --arg p "$pid" '[.[].value[$p] // {} | keys[]] | unique | length' 2>/dev/null || echo 0
}

# ── Tx-size lattice + metadata helpers (#961) ─────────────────────────
#
# 08i already proves a 20 KB metadata blob trips TxTooLarge, but a single
# far-over-the-line case is a weak bound: it cannot tell a correct limit from
# one that is wrong in the ACCEPTING direction. The lattice pairs a tx just
# UNDER maxTxSize (must be accepted and land on every observer) with one just
# OVER (must be refused), so the limit is bracketed rather than merely
# exceeded.

# max_tx_size — the live maxTxSize protocol parameter.
max_tx_size() {
    jq -r '.maxTxSize // 16384' "$(zoo_pparams_file)"
}

# metadata_blob <name> <bytes> — write a metadata JSON whose payload is
# roughly <bytes> long; echo the file path.
#
# Built as a list of 64-byte strings rather than one enormous string: CBOR text
# above 64 bytes is chunked, and the chunked form is what the node actually
# encodes. One huge string would measure a different shape from real metadata.
metadata_blob() {
    local out="$ZOO_BUILT/$1.meta.json" bytes="$2"
    python3 -c '
import json, sys
out, n = sys.argv[1], int(sys.argv[2])
chunks = ["A" * 64 for _ in range(max(1, n // 64))]
json.dump({"674": {"msg": chunks}}, open(out, "w"))
' "$out" "$bytes"
    printf '%s' "$out"
}

# signed_tx_size <signed-file> — serialized size in bytes of a signed tx.
signed_tx_size() {
    python3 -c '
import json, sys
print(len(bytes.fromhex(json.load(open(sys.argv[1]))["cborHex"])))
' "$1"
}

# local_txid <signed-file> — the txid cardano-cli computes locally.
#
# WHAT A METADATA ROUND-TRIP CAN AND CANNOT CHECK HERE
# ----------------------------------------------------
# No LSQ query returns a transaction's metadata, so "re-query the metadata
# bytes" is not reachable through cardano-cli. What IS checkable is the part
# that can actually diverge:
#
#   txid = blake2b256(tx_body_cbor), and the body carries
#   auxiliary_data_hash = blake2b256(metadata_cbor)
#
# So a metadata-bearing tx appearing under this locally-computed txid on BOTH
# dugite and cardano-node proves both hashed the same metadata bytes to the
# same aux-data hash, and the same body to the same id. A divergence in
# metadata encoding surfaces either as ConflictingMetadataHash at submission or
# as the tx never appearing under this id — never as a silently different
# payload. The check is stated in those terms rather than claimed as a full
# byte round-trip.
local_txid() {
    cardano-cli conway transaction txid --tx-file "$1" 2>/dev/null
}
