#!/usr/bin/env bash
# Run Dugite as a block producer on Cardano mainnet.
#
# Usage: ./scripts/run/bp-mainnet.sh [--log FILE]
#
# Prerequisites:
#   - Build: cargo build --release
#   - Keys in ./keys/mainnet/pool/ (kes.skey, vrf.skey, opcert.cert, cold.skey)
#   - Database in ./db-mainnet/ (use mithril-import first if empty)

set -euo pipefail
cd "$(dirname "$0")/../.."

LOGFILE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --log) LOGFILE="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

BIN=./target/release/dugite-node
KEY_DIR=./keys/mainnet/pool

if [[ ! -x "$BIN" ]]; then
    echo "Binary not found. Building..."
    cargo build --release
fi

for f in kes.skey vrf.skey opcert.cert; do
    if [[ ! -f "$KEY_DIR/$f" ]]; then
        echo "Missing key: $KEY_DIR/$f"
        exit 1
    fi
done

# Import Mithril snapshot if database is empty
if [[ ! -d "./db-mainnet/immutable" ]]; then
    echo "Database empty. Importing Mithril snapshot (~35 GB, may take 30+ minutes)..."
    # On failure, retry once — but refuse if the disk is full (ENOSPC), and
    # preserve the snapshot archive so the retry can reuse it.
    IMPORT_CMD=("$BIN" mithril-import --network-magic 764824073 --database-path ./db-mainnet)
    if ! IMPORT_OUTPUT=$("${IMPORT_CMD[@]}" 2>&1); then
        printf '%s\n' "$IMPORT_OUTPUT" >&2
        if printf '%s\n' "$IMPORT_OUTPUT" | grep -qi "no space left on device\|ENOSPC"; then
            echo "ERROR: mithril-import failed: disk full (ENOSPC). Free up space and retry." >&2
            exit 1
        fi
        echo "mithril-import failed; retrying once (archive cache preserved)..."
        "${IMPORT_CMD[@]}"
    fi
fi

CMD=(
    "$BIN" run
    --config config/mainnet/config.json
    --topology config/mainnet/topology.json
    --database-path ./db-mainnet
    --socket-path ./node.sock
    --host-addr 0.0.0.0
    --port 3001
    --shelley-kes-key "$KEY_DIR/kes.skey"
    --shelley-vrf-key "$KEY_DIR/vrf.skey"
    --shelley-operational-certificate "$KEY_DIR/opcert.cert"
)

echo "Starting Dugite block producer (mainnet)..."
echo "Pool keys: $KEY_DIR"
echo "Database:  ./db-mainnet"
echo "Socket:    ./node.sock"
echo "Metrics:   http://localhost:12800/metrics"

if [[ -n "$LOGFILE" ]]; then
    echo "Logging to: $LOGFILE"
    "${CMD[@]}" 2>&1 | tee "$LOGFILE"
else
    "${CMD[@]}"
fi
