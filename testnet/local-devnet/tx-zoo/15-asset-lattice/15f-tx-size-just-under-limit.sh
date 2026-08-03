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

# Converge on a payload that lands just under the cap.
#
# A fixed margin does not work: the metadata payload is chunked into 64-byte
# CBOR strings, each carrying its own header, so the serialized tx is ~5%
# larger than the character count plus a few hundred bytes of body, witness and
# change. The first attempt at MAX-600 overshot by 103 bytes and the guard
# below turned the whole case into an ENV_SKIP — a skip measures nothing, which
# is the failure mode this suite exists to avoid. So: build, MEASURE, and shrink
# by the observed excess until it fits.
TARGET=$(( MAX - 1200 ))
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"; SIGNED="$ZOO_BUILT/$NAME.signed"
SIZE=0
for attempt in 1 2 3 4; do
    META=$(metadata_blob "$NAME" "$TARGET")
    if ! cardano-cli conway transaction build \
            --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
            --tx-in "${UTXO%% *}" --change-address "$ADDR" \
            --metadata-json-file "$META" \
            --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err"; then
        zoo_fail "build failed: $(grep -m1 Error "$ZOO_LOGS/$NAME.err" | cut -c1-140)"
        zoo_record "$NAME" FAIL "" "build-failed"; exit 1
    fi
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$RAW" --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file "$SIGNED" >/dev/null
    SIZE=$(signed_tx_size "$SIGNED")
    [ "$SIZE" -lt "$MAX" ] && break
    # Shrink by the excess plus a 10% cushion for the chunk headers we shed.
    EXCESS=$(( SIZE - MAX ))
    TARGET=$(( TARGET - EXCESS - (EXCESS / 10) - 128 ))
    zoo_info "$NAME: attempt $attempt built ${SIZE}B (cap ${MAX}B) — retrying at payload ${TARGET}"
done
if [ "$SIZE" -ge "$MAX" ]; then
    # Could not converge; this is a fixture problem, not a node verdict.
    zoo_record_env_skip "$NAME" "could not converge under maxTxSize ${MAX}B (last ${SIZE}B)"
    exit 0
fi
# A tx far below the cap would pass trivially and prove nothing about the
# boundary, so require it to be genuinely near-limit.
if [ "$SIZE" -lt $(( MAX - 3000 )) ]; then
    zoo_record_env_skip "$NAME" "converged to ${SIZE}B, too far under maxTxSize ${MAX}B to test the boundary"
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
