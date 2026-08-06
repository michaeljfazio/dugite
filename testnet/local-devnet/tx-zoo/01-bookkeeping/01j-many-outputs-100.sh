#!/usr/bin/env bash
# 01j — one tx paying 100 fresh addresses.
#
# Upstream: cardano-node-tests test_transaction_to_100_addrs_from_1_addr
# (#1032, cardano-node-tests adoption P0.1).
#
# Assertion contract: derive 100 fresh single-address payment keys, build ONE
# tx with 100 --tx-out entries (1.5 ADA each) funded from the genesis wallet,
# assert it is accepted, then verify all 100 outputs landed. Full count is
# checked on ONE socket (100 round-trips is already the expensive part);
# a 5-address spot-check on a SECOND socket keeps the runtime sane while
# still proving the second implementation agrees on at least a sample.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

N=100
PER_ADDR=1500000
DIR="$ZOO_KEYS/$NAME"
mkdir -p "$DIR"

ADDRS=()
for i in $(seq 1 "$N"); do
    d="$DIR/addr-$i"
    mkdir -p "$d"
    if [ ! -s "$d/payment.addr" ]; then
        cardano-cli conway address key-gen \
            --verification-key-file "$d/payment.vkey" \
            --signing-key-file      "$d/payment.skey" >/dev/null
        cardano-cli conway address build \
            --payment-verification-key-file "$d/payment.vkey" \
            --testnet-magic "$LD_MAGIC" \
            --out-file "$d/payment.addr" >/dev/null
    fi
    ADDRS+=("$(cat "$d/payment.addr")")
done

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}

OUT_ARGS=()
for a in "${ADDRS[@]}"; do
    OUT_ARGS+=(--tx-out "${a}+${PER_ADDR}")
done

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    "${OUT_ARGS[@]}" \
    --change-address "$ADDR" \
    --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }

if ! zoo_wait_all_observers "$TXID" 120; then
    zoo_record "$NAME" FAIL "$TXID" "not-included"
    exit 1
fi

# ── Full count on ONE socket ────────────────────────────────────────────────
FOUND=0
MISSING=()
for a in "${ADDRS[@]}"; do
    HIT=$(cardano-cli conway query utxo \
            --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
            --address "$a" --output-json 2>/dev/null \
        | jq -r '[to_entries[] | select(.value.value.lovelace == '"$PER_ADDR"')] | length')
    if [ "${HIT:-0}" -ge 1 ]; then
        FOUND=$((FOUND + 1))
    else
        MISSING+=("$a")
    fi
done

# RED-PROOF: change `-eq $N` to `-ge 1` (or drop the check) to hide addresses
# that never received their output.
if [ "$FOUND" -ne "$N" ]; then
    zoo_fail "only $FOUND/$N addresses received their output on \$ZOO_SOCKET"
    zoo_record "$NAME" FAIL "$TXID" "found=$FOUND/$N missing=${#MISSING[@]}"
    exit 1
fi

# ── Spot-check 5 addresses on a SECOND socket ───────────────────────────────
SPOT_SOCK=""
for sock in "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$sock" ] && [ "$sock" != "$ZOO_SOCKET" ] && { SPOT_SOCK="$sock"; break; }
done

if [ -z "$SPOT_SOCK" ]; then
    zoo_record "$NAME" PASS "$TXID" "found=$FOUND/$N (no second socket for spot-check)"
    exit 0
fi

SPOT_OK=0
for idx in 0 19 39 59 79; do
    a="${ADDRS[$idx]}"
    HIT=$(cardano-cli conway query utxo \
            --testnet-magic "$LD_MAGIC" --socket-path "$SPOT_SOCK" \
            --address "$a" --output-json 2>/dev/null \
        | jq -r '[to_entries[] | select(.value.value.lovelace == '"$PER_ADDR"')] | length')
    [ "${HIT:-0}" -ge 1 ] && SPOT_OK=$((SPOT_OK + 1))
done

if [ "$SPOT_OK" -eq 5 ]; then
    zoo_record "$NAME" PASS "$TXID" "found=$FOUND/$N spot-check=5/5-on-$SPOT_SOCK"
else
    zoo_record "$NAME" FAIL "$TXID" "found=$FOUND/$N spot-check=$SPOT_OK/5-on-$SPOT_SOCK"
    exit 1
fi
