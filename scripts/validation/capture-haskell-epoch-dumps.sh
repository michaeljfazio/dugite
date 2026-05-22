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

while [ "$#" -gt 0 ]; do
    case "$1" in
        --socket)  SOCKET="$2"; shift 2 ;;
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        --magic)   MAGIC="$2";   shift 2 ;;
        --cli)     CLI="$2";     shift 2 ;;
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

if [ -z "$SOCKET" ] || [ -z "$OUT_DIR" ]; then
    echo "usage: $0 --socket <path> --out-dir <dir> [--magic N] [--cli <path>]" >&2
    exit 2
fi

mkdir -p "$OUT_DIR"
JSONL="$OUT_DIR/epoch-states.jsonl"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SPLITTER="$SCRIPT_DIR/split-haskell-jsonl.py"

if [ ! -x "$SPLITTER" ] && [ ! -r "$SPLITTER" ]; then
    echo "splitter not found at $SPLITTER" >&2
    exit 3
fi

echo "[capture] cli=$CLI socket=$SOCKET magic=$MAGIC out=$OUT_DIR jsonl=$JSONL"

# Run the cli in the foreground; user is expected to background this
# whole script.  Tail the jsonl and feed each line into the splitter
# so per-epoch files appear as the chain advances.
(
    "$CLI" debug log-epoch-state \
        --socket-path "$SOCKET" \
        --testnet-magic "$MAGIC" \
        --out-file "$JSONL"
) &
CLI_PID=$!

trap 'kill $CLI_PID 2>/dev/null || true' EXIT INT TERM

# Wait for the jsonl to appear (cli takes a few seconds to attach).
TRIES=0
while [ ! -e "$JSONL" ]; do
    sleep 1
    TRIES=$((TRIES + 1))
    if [ "$TRIES" -gt 60 ]; then
        echo "[capture] timed out waiting for $JSONL" >&2
        exit 4
    fi
done

# Tail and split.  `tail -F` follows even if the file is rotated.
tail -n +1 -F "$JSONL" | python3 "$SPLITTER" --out-dir "$OUT_DIR"
