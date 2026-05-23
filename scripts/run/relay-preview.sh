#!/usr/bin/env bash
# Run Dugite as a relay node on the Cardano preview testnet.
#
# Usage: ./scripts/run/relay-preview.sh [--log FILE]
#
# Prerequisites:
#   - Build: cargo build --release
#   - Database in ./db-preview/ (use mithril-import first if empty)

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

if [[ ! -x "$BIN" ]]; then
    echo "Binary not found. Building..."
    cargo build --release
fi

# Import Mithril snapshot if database is empty
if [[ ! -d "./db-preview/immutable" ]]; then
    echo "Database empty. Importing Mithril snapshot..."
    # On failure, retry once — but refuse if the disk is full (ENOSPC), and
    # preserve the snapshot archive so the retry can reuse it.
    IMPORT_CMD=("$BIN" mithril-import --network-magic 2 --database-path ./db-preview)
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
    --config config/preview/config.json
    --topology config/preview/topology.json
    --database-path ./db-preview
    --socket-path ./node.sock
    --host-addr 0.0.0.0
    --port 3001
)

echo "Starting Dugite relay (preview testnet)..."
echo "Database:  ./db-preview"
echo "Socket:    ./node.sock"
echo "Metrics:   http://localhost:12796/metrics"

if [[ -n "$LOGFILE" ]]; then
    echo "Logging to: $LOGFILE"
    "${CMD[@]}" 2>&1 | tee "$LOGFILE"
else
    "${CMD[@]}"
fi
