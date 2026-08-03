#!/usr/bin/env bash
# 09h — query protocol-state
#
# Two rows:
#   protocol-state/version      the protocol version
#   protocol-state/epoch-nonce  the EPOCH NONCE and last-epoch-block nonce
#
# Why the second row exists (#964)
# --------------------------------
# This script used to compare `{protocolVersion}` and nothing else, on the
# grounds that "nonce-related fields will differ if nodes are at different
# tips". Two things were wrong with that:
#
#   * The epoch nonce is NOT per-block. `epochNonce` is rotated once per epoch
#     by TICKN and is constant in between; only `evolvingNonce`,
#     `candidateNonce` and `labNonce` move with each block. Lumping it in with
#     those discarded a perfectly comparable field.
#   * The tip-instability worry is exactly what `parity_query_json` handles —
#     it samples both sockets, re-reads both tips, and retries until nothing
#     moved and both agree. This script hand-rolled its own comparison and so
#     opted out of that machinery rather than using it.
#
# The consequence: the epoch nonce was never compared against cardano-node by
# any suite. It is also one of exactly three node-supplied inputs to
# `cardano-cli query leadership-schedule`, which is computed CLIENT-side — the
# others being the pool distribution (#964's σ defect) and the protocol
# parameters. A wrong epoch nonce yields a leader schedule statistically
# INDEPENDENT of cardano-node's rather than merely mis-sized, which is what
# #964 observed and what a σ error alone cannot explain: a wrong σ only moves
# the threshold, so it can only ever produce a subset.
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

rc=0
if [ "$dsha" = "$csha" ]; then
    parity_record "protocol-state/version" "EQUAL" "$dsha" "$csha" "$dugite_val"
else
    parity_record "protocol-state/version" "DIVERGENT" "$dsha" "$csha" \
        "dugite=$dugite_val cardano=$cardano_val"
    rc=1
fi

# ── Row 2: the epoch nonce, at a pinned tip ────────────────────────────────
#
# Both nodes follow the same chain from the same genesis, so at an identical
# tip these two fields must be byte-identical. The per-block nonces
# (`evolvingNonce`, `candidateNonce`, `labNonce`) are deliberately excluded:
# they are legitimately mid-flight even at an agreed tip, since each node
# updates them as it applies the block rather than after.
PARITY_JQ_FILTER='{epochNonce: .epochNonce, lastEpochBlockNonce: .lastEpochBlockNonce}' \
    parity_query_json "protocol-state/epoch-nonce" "protocol-state" || rc=$?

exit "$rc"
