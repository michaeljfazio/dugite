#!/usr/bin/env bash
# Run Dugite as a relay node on the Cardano preprod testnet.
#
# Usage: ./scripts/run/relay-preprod.sh [--log FILE]

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

if [[ ! -d "./db-preprod/immutable" ]]; then
    echo "Database empty. Importing Mithril snapshot..."
    # On failure, retry once — but refuse if the disk is full (ENOSPC), and
    # preserve the snapshot archive so the retry can reuse it.
    IMPORT_CMD=("$BIN" mithril-import --network-magic 1 --database-path ./db-preprod)
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
    --config config/preprod/config.json
    --topology config/preprod/topology.json
    --database-path ./db-preprod
    --socket-path ./node.sock
    --host-addr 0.0.0.0
    --port 3001
)

echo "Starting Dugite relay (preprod testnet)..."
echo "Database:  ./db-preprod"
echo "Socket:    ./node.sock"
echo "Metrics:   http://localhost:12798/metrics"

if [[ -n "$LOGFILE" ]]; then
    echo "Logging to: $LOGFILE"
    "${CMD[@]}" 2>&1 | tee "$LOGFILE"
else
    "${CMD[@]}"
fi
