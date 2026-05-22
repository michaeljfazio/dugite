#!/usr/bin/env bash
# 09a — query tip
# Both nodes track the same chain; at any given instant the tips may differ by
# 0-2 blocks.  We normalise out the volatile fields (slot, block, hash) and
# compare only the structural shape + era.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

# Run query against both sides, extract just the era (stable field).
_tip_era() {
    local sock="$1"
    cardano-cli conway query tip \
        --testnet-magic "$LD_MAGIC" \
        --socket-path "$sock" \
        --output-json 2>/dev/null \
    | jq -Sc '{era: .era}' 2>/dev/null || echo '{"era":"error"}'
}

dugite_era=$(_tip_era "$LD_DUGITE_BP_SOCK")
cardano_era=$(_tip_era "$LD_CARDANO_BP_SOCK")

dsha=$(printf '%s' "$dugite_era"  | sha256sum | awk '{print $1}')
csha=$(printf '%s' "$cardano_era" | sha256sum | awk '{print $1}')

if [ "$dsha" = "$csha" ]; then
    parity_record "tip/era" "EQUAL" "$dsha" "$csha" "era=$dugite_era"
else
    parity_record "tip/era" "DIVERGENT" "$dsha" "$csha" "dugite=$dugite_era cardano=$cardano_era"
    exit 1
fi
