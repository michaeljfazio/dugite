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
# Named pipe (FIFO) instead of a regular file: the cli writes per-block
# records into the pipe, the splitter reads them out and emits one
# epoch_NNNNNN.json per epoch.  Nothing accumulates on disk.  Without
# this, cn 11.0.1's per-block emission writes ~10 GB/min and saturates
# the filesystem within minutes (observed 284 GB in 30 min).
FIFO="$OUT_DIR/epoch-states.fifo"
rm -f "$FIFO"
mkfifo "$FIFO"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SPLITTER="$SCRIPT_DIR/split-haskell-jsonl.py"

if [ ! -x "$SPLITTER" ] && [ ! -r "$SPLITTER" ]; then
    echo "splitter not found at $SPLITTER" >&2
    exit 3
fi

echo "[capture] cli=$CLI socket=$SOCKET magic=$MAGIC out=$OUT_DIR fifo=$FIFO"

# Start the splitter FIRST, reading from the FIFO (blocks until writer opens).
python3 "$SPLITTER" --out-dir "$OUT_DIR" < "$FIFO" &
SPLITTER_PID=$!

# Now start the cli writer — opens the FIFO writer end, splitter unblocks.
"$CLI" debug log-epoch-state \
    --socket-path "$SOCKET" \
    --node-configuration-file "$NODE_CONFIG" \
    --out-file "$FIFO" &
CLI_PID=$!

trap 'kill $CLI_PID $SPLITTER_PID 2>/dev/null || true; rm -f "$FIFO"' EXIT INT TERM

wait "$CLI_PID"
rc=$?
echo "[capture] cli exited rc=$rc"
exit "$rc"
