#!/usr/bin/env bash
# capture-haskell-epoch-dumps.sh
#
# Stream `cardano-cli debug log-epoch-state` from a running
# cardano-haskell-node into per-epoch JSON dumps that conform to the
# canonical schema (see scripts/validation/EPOCH_DIFF.md).
#
# Usage:
#   capture-haskell-epoch-dumps.sh \
#     --socket ./node.sock \
#     --out-dir ./epoch-dumps-haskell \
#     [--magic 2] \
#     [--cli /usr/local/bin/cardano-cli]
#
# The cli streams line-delimited JSON, one record per epoch boundary,
# to a single .jsonl file.  A companion python splitter is invoked to
# expand that .jsonl into per-epoch files in <out-dir>.
#
# Run this in the background while dugite-node runs separately with
# DUGITE_EPOCH_STATE_DUMP=<dir> set.

set -euo pipefail

SOCKET=""
OUT_DIR=""
MAGIC="2"
CLI="cardano-cli"
NODE_CONFIG=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --socket)        SOCKET="$2"; shift 2 ;;
        --out-dir)       OUT_DIR="$2"; shift 2 ;;
        --magic)         MAGIC="$2";   shift 2 ;;
        --cli)           CLI="$2";     shift 2 ;;
        --node-config)   NODE_CONFIG="$2"; shift 2 ;;
        -h|--help)
            sed -n '1,30p' "$0"
            exit 0
            ;;
        *)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
    esac
done

if [ -z "$SOCKET" ] || [ -z "$OUT_DIR" ] || [ -z "$NODE_CONFIG" ]; then
    echo "usage: $0 --socket <path> --out-dir <dir> --node-config <path> [--magic N] [--cli <path>]" >&2
    exit 2
fi

mkdir -p "$OUT_DIR"
JSONL="$OUT_DIR/epoch-states.jsonl"
rm -f "$JSONL"
touch "$JSONL"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SPLITTER="$SCRIPT_DIR/split-haskell-jsonl.py"

if [ ! -x "$SPLITTER" ] && [ ! -r "$SPLITTER" ]; then
    echo "splitter not found at $SPLITTER" >&2
    exit 3
fi

echo "[capture] cli=$CLI socket=$SOCKET magic=$MAGIC out=$OUT_DIR jsonl=$JSONL"

# cn 11.0.1's --out-file does NOT support FIFOs on macOS (withBinaryFile
# fails with Device not configured) and emits per-block-applied records
# (~10 GB/min).  Workaround: regular file + a truncation watcher that
# resets the file every 60s once the splitter has consumed the buffered
# tail.  `tail -F` handles truncation gracefully (continues from offset
# 0 on the next append).
"$CLI" debug log-epoch-state \
    --socket-path "$SOCKET" \
    --node-configuration-file "$NODE_CONFIG" \
    --out-file "$JSONL" &
CLI_PID=$!

# Truncation watcher — periodically resets the JSONL to 0 bytes so it
# never grows unbounded.  The splitter's tail -F sees the truncation
# and continues following.
(
    while kill -0 "$CLI_PID" 2>/dev/null; do
        sleep 60
        : > "$JSONL"
    done
) &
TRUNC_PID=$!

trap 'kill $CLI_PID $TRUNC_PID 2>/dev/null || true' EXIT INT TERM

# Wait for the jsonl to have content (cli takes a few seconds to attach).
TRIES=0
while [ ! -s "$JSONL" ]; do
    sleep 1
    TRIES=$((TRIES + 1))
    if [ "$TRIES" -gt 120 ]; then
        echo "[capture] timed out waiting for $JSONL to receive data" >&2
        kill "$CLI_PID" 2>/dev/null || true
        exit 4
    fi
    if ! kill -0 "$CLI_PID" 2>/dev/null; then
        echo "[capture] cli exited before any output" >&2
        exit 5
    fi
done

# Tail + split.  `tail -F` follows truncation cleanly.
tail -n +1 -F "$JSONL" | python3 "$SPLITTER" --out-dir "$OUT_DIR"
