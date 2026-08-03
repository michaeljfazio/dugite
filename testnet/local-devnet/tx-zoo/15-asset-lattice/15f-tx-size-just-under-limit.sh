#!/usr/bin/env bash
# 15f — a transaction padded to just UNDER maxTxSize must be ACCEPTED.
#
# Half of the size lattice (#961). 08i proves a 20 KB blob is refused, but a
# single far-over case cannot distinguish a correct limit from one that is
# wrong in the accepting direction: a node whose maxTxSize check was off by a
# kilobyte would pass 08i and still reject transactions cardano-node accepts.
# That is exactly the #930 shape — a one-byte over-count producing a FALSE
# reject — so the near side of the boundary is the interesting one.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/15-asset-lattice/_lattice-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
MAX=$(max_tx_size)

# Aim ~600 bytes under the cap: enough headroom that fee/change variation
# cannot push the built tx over, while still exercising a near-limit body.
TARGET=$(( MAX - 600 ))
META=$(metadata_blob "$NAME" "$TARGET")

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

SIZE=$(signed_tx_size "$SIGNED")
if [ "$SIZE" -ge "$MAX" ]; then
    # The padding overshot; this is an environment/pparams issue, not a node
    # verdict, so record it as such instead of failing on an untested claim.
    zoo_record_env_skip "$NAME" "built tx is ${SIZE}B >= maxTxSize ${MAX}B — padding overshot"
    exit 0
fi
zoo_info "$NAME: ${SIZE}B against maxTxSize ${MAX}B (margin $((MAX - SIZE))B)"

TXID=$(local_txid "$SIGNED")
zoo_submit "$SIGNED" || { zoo_record "$NAME" FAIL "" "submit-rejected-under-limit"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_record "$NAME" PASS "$TXID" "accepted-at-${SIZE}B-under-${MAX}B"
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
