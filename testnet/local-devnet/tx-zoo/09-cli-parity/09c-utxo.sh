#!/usr/bin/env bash
# 09c — query utxo (whole address)
# Queries the genesis payment address that tx-zoo uses.  At the time this
# script runs the nodes should be at the same tip, so the UTxO sets should
# match.  If they diverge by at most 1-2 UTxOs due to a recent unconfirmed
# tx, that is a known transient condition; we record it but treat it as a
# soft warning rather than a hard failure.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

ADDR_FILE="$LD_KEYS/utxo/payment.addr"
if [ ! -f "$ADDR_FILE" ]; then
    parity_record "utxo" "SKIP" "skip" "skip" "payment addr not found (run tx-zoo --setup first)"
    exit 0
fi

ADDR=$(cat "$ADDR_FILE")
parity_query_json "utxo" "utxo" "--address" "$ADDR"
