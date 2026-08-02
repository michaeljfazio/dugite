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
