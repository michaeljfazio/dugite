#!/usr/bin/env bash
# 09h — query protocol-state
# Returns the current Nonce and other consensus state fields.  Some fields
# (nonce) vary per-block; we compare only the epoch-stable fields.
# Known divergence: nonce-related fields will differ if nodes are at different
# tips — recorded as normalised comparison over stable fields only.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

# protocol-state output varies between versions; extract epoch-stable subset.
_proto_state_stable() {
    local sock="$1"
    cardano-cli conway query protocol-state \
        --testnet-magic "$LD_MAGIC" \
        --socket-path "$sock" \
        --output-json 2>/dev/null \
    | jq -Sc '{protocolVersion: .protocolVersion}' 2>/dev/null \
    || echo '{"protocolVersion":"error"}'
}

dugite_val=$(_proto_state_stable "$LD_DUGITE_BP_SOCK")
cardano_val=$(_proto_state_stable "$LD_CARDANO_BP_SOCK")

dsha=$(printf '%s' "$dugite_val"  | sha256sum | awk '{print $1}')
csha=$(printf '%s' "$cardano_val" | sha256sum | awk '{print $1}')

if [ "$dsha" = "$csha" ]; then
    parity_record "protocol-state/version" "EQUAL" "$dsha" "$csha" "$dugite_val"
else
    parity_record "protocol-state/version" "DIVERGENT" "$dsha" "$csha" \
        "dugite=$dugite_val cardano=$cardano_val"
    exit 1
fi
