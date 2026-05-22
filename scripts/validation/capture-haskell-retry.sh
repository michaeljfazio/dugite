#!/usr/bin/env bash
# capture-haskell-retry.sh — wrap capture-haskell-epoch-dumps.sh in a
# retry loop that survives the `FoldBlocksApplyBlockError ByronEraUnsupported`
# crash on networks (e.g. preprod) where the from-genesis sync goes through
# Byron era.  cardano-cli debug log-epoch-state cannot fold Byron blocks;
# the wrapper retries with backoff until the Haskell node has replayed past
# Byron → Shelley and the capture latches on.
#
# Usage: same args as capture-haskell-epoch-dumps.sh.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CAPTURE="$SCRIPT_DIR/capture-haskell-epoch-dumps.sh"
DELAY=30
MAX_DELAY=300

while true; do
    "$CAPTURE" "$@"
    rc=$?
    echo "[retry-wrapper] capture exited rc=$rc; retrying in ${DELAY}s"
    sleep "$DELAY"
    DELAY=$(( DELAY * 2 ))
    if [ "$DELAY" -gt "$MAX_DELAY" ]; then DELAY="$MAX_DELAY"; fi
done
