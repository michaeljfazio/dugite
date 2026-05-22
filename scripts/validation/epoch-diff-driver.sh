#!/usr/bin/env bash
# epoch-diff-driver.sh
#
# Documents the end-to-end run order for cross-validating dugite-node
# vs cardano-haskell-node on preview / preprod via the per-epoch
# ledger-state dump + diff harness (tasks #21/#22/#23).
#
# This script does NOT start the nodes for you — node lifecycle is too
# environment-specific for a single one-shot harness.  It does:
#   1. Sanity-check the requested epoch range
#   2. Print the exact commands you should run in parallel terminals
#   3. (Optional, with --diff) invoke the diff tool once the dumps exist
#
# Usage:
#   epoch-diff-driver.sh \
#       --network preview \
#       --haskell-dir ./epoch-dumps-haskell \
#       --dugite-dir  ./epoch-dumps-dugite \
#       --from-epoch 1 --to-epoch 5 \
#       [--diff]            # actually run diff-epoch-dumps.py at the end
#       [--report-md OUT.md] [--report-json OUT.json]

set -euo pipefail

NETWORK=""
HASKELL_DIR=""
DUGITE_DIR=""
FROM_EPOCH=""
TO_EPOCH=""
DO_DIFF=0
REPORT_MD=""
REPORT_JSON=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --network)      NETWORK="$2"; shift 2 ;;
        --haskell-dir)  HASKELL_DIR="$2"; shift 2 ;;
        --dugite-dir)   DUGITE_DIR="$2"; shift 2 ;;
        --from-epoch)   FROM_EPOCH="$2"; shift 2 ;;
        --to-epoch)     TO_EPOCH="$2"; shift 2 ;;
        --diff)         DO_DIFF=1; shift ;;
        --report-md)    REPORT_MD="$2"; shift 2 ;;
        --report-json)  REPORT_JSON="$2"; shift 2 ;;
        -h|--help)
            sed -n '1,25p' "$0"
            exit 0
            ;;
        *)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
    esac
done

for v in NETWORK HASKELL_DIR DUGITE_DIR FROM_EPOCH TO_EPOCH; do
    if [ -z "${!v}" ]; then
        echo "missing --${v,,/_/-}; see --help" >&2
        exit 2
    fi
done

case "$NETWORK" in
    preview)   MAGIC=2 ;;
    preprod)   MAGIC=1 ;;
    mainnet)   MAGIC=764824073 ;;
    *)
        echo "unknown network: $NETWORK" >&2
        exit 2
        ;;
esac

mkdir -p "$HASKELL_DIR" "$DUGITE_DIR"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

cat <<RECIPE
[driver] cross-validation recipe for $NETWORK (magic=$MAGIC), epochs $FROM_EPOCH..$TO_EPOCH

Both nodes sync from genesis with the dumper attached, capturing EVERY
epoch boundary from epoch 0 to current tip.  Mithril snapshots cannot
satisfy 'every epoch up to current tip' coverage — they only provide
forward-going state from the snapshot point.  Wall time on preview is
~12-24h Haskell + ~6-12h dugite running in parallel.

Step 1 — start cardano-haskell-node from genesis (background, terminal A):
    rm -rf ./db-$NETWORK-haskell
    cardano-node run \\
        --config         config/$NETWORK/config.json \\
        --topology       config/$NETWORK/topology.json \\
        --database-path  ./db-$NETWORK-haskell \\
        --socket-path    ./node-haskell.sock \\
        --port 3002 \\
        +RTS -N -A64m -RTS

Step 2 — start dugite-node from genesis with the dumper feature (terminal B):
    cargo build --release -p dugite-node --features dugite-ledger/epoch-state-debug
    rm -rf ./db-$NETWORK-dugite
    DUGITE_EPOCH_STATE_DUMP=$DUGITE_DIR \\
    DUGITE_EPOCH_STATE_DUMP_SKIP_ASSETS=1 \\
    ./target/release/dugite-node run \\
        --config        config/$NETWORK/config.json \\
        --topology      config/$NETWORK/topology.json \\
        --database-path ./db-$NETWORK-dugite \\
        --socket-path   ./node-dugite.sock \\
        --port 3001

Step 3 — start Haskell-side capture immediately after node startup (terminal C):
    $SCRIPT_DIR/capture-haskell-epoch-dumps.sh \\
        --socket ./node-haskell.sock \\
        --magic  $MAGIC \\
        --out-dir $HASKELL_DIR

Step 4 — wait for both sides to cross epoch $TO_EPOCH then diff:
    $SCRIPT_DIR/diff-epoch-dumps.py \\
        --haskell-dir $HASKELL_DIR \\
        --dugite-dir  $DUGITE_DIR \\
        --from-epoch $FROM_EPOCH --to-epoch $TO_EPOCH \\
        ${REPORT_MD:+--report-md $REPORT_MD} \\
        ${REPORT_JSON:+--report-json $REPORT_JSON}

Step 1 — start cardano-haskell-node (foreground, terminal A):
    cardano-node run \\
        --config         config/$NETWORK/config.json \\
        --topology       config/$NETWORK/topology.json \\
        --database-path  ./db-$NETWORK-haskell \\
        --socket-path    ./node-haskell.sock \\
        --port 3002

Step 2 — start dugite-node with the dumper feature (foreground, terminal B):
    cargo build --release -p dugite-node --features dugite-ledger/epoch-state-debug
    DUGITE_EPOCH_STATE_DUMP=$DUGITE_DIR \\
    DUGITE_EPOCH_STATE_DUMP_SKIP_ASSETS=1 \\
    ./target/release/dugite-node run \\
        --config        config/$NETWORK/config.json \\
        --topology      config/$NETWORK/topology.json \\
        --database-path ./db-$NETWORK \\
        --socket-path   ./node-dugite.sock \\
        --port 3001

Step 3 — start the Haskell-side capture (foreground, terminal C):
    $SCRIPT_DIR/capture-haskell-epoch-dumps.sh \\
        --socket ./node-haskell.sock \\
        --magic  $MAGIC \\
        --out-dir $HASKELL_DIR

Step 4 — wait for both sides to cross epoch $TO_EPOCH then run the diff:
    $SCRIPT_DIR/diff-epoch-dumps.py \\
        --haskell-dir $HASKELL_DIR \\
        --dugite-dir  $DUGITE_DIR \\
        --from-epoch $FROM_EPOCH --to-epoch $TO_EPOCH \\
        ${REPORT_MD:+--report-md $REPORT_MD} \\
        ${REPORT_JSON:+--report-json $REPORT_JSON}

RECIPE

if [ "$DO_DIFF" = "1" ]; then
    echo "[driver] --diff requested; invoking diff tool now..."
    EXTRA=()
    if [ -n "$REPORT_MD" ];   then EXTRA+=(--report-md "$REPORT_MD"); fi
    if [ -n "$REPORT_JSON" ]; then EXTRA+=(--report-json "$REPORT_JSON"); fi
    python3 "$SCRIPT_DIR/diff-epoch-dumps.py" \
        --haskell-dir "$HASKELL_DIR" \
        --dugite-dir  "$DUGITE_DIR" \
        --from-epoch "$FROM_EPOCH" --to-epoch "$TO_EPOCH" \
        "${EXTRA[@]}"
fi
