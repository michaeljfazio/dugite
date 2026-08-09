#!/usr/bin/env bash
# Attach `cardano-cli debug log-epoch-state` to a syncing node and reduce its
# output to per-epoch digest files as it arrives.
#
# ┌──────────────────────────────────────────────────────────────────────────┐
# │ DOES NOT WORK ON MAINNET. MEASURED, not assumed:                         │
# │                                                                          │
# │     Error: FoldBlocksApplyBlockError ByronEraUnsupported                 │
# │                                                                          │
# │ `log-epoch-state` is built on cardano-api's `foldBlocks`, which replays  │
# │ from GENESIS and rejects Byron blocks. There is no start-point flag —    │
# │ the whole CLI surface is --socket-path/--node-configuration-file/        │
# │ --out-file — so a chain containing Byron cannot be folded at all. It     │
# │ dies within seconds of attaching.                                        │
# │                                                                          │
# │ USE IT ON preview / preprod, which genesis POST-Byron. That is where it  │
# │ is actually valuable: both run Conway, and cardano-streamer's 10.6.2 pin │
# │ is exactly what cannot be trusted for Conway rules. So this covers the   │
# │ era cstreamer cannot, on the networks where it runs.                     │
# │                                                                          │
# │ For MAINNET Conway coverage the path is porting cardano-streamer's       │
# │ dump-epoch-snapshots commits onto lehins 10.7.1 instead.                 │
# └──────────────────────────────────────────────────────────────────────────┘
#
# The node reports each epoch state as it CROSSES that boundary, so this must
# be running while the node syncs — which is free when a tip sync is happening
# anyway. Attaching it to a node already at tip captures nothing until the next
# boundary.
#
# Why this oracle: cardano-streamer is pinned to cardano-node 10.6.2
# dependencies. Provably fine for historical epochs (any version that syncs
# mainnet computes identical historical state, or it forks) but NOT for Conway,
# whose rules are recent. This is cardano-node 11.0.1 itself.
#
# Why a FIFO: `log-epoch-state` writes the WHOLE epoch state per line and never
# terminates. At mainnet scale that is hundreds of MB per epoch — the same
# volume wall that made the dumps 1-2 TB by tip. Reducing in-flight keeps the
# raw state off disk entirely.
set -uo pipefail

ROOT=${ROOT:-/Users/michaelfazio/Source/dugite}
WT=${WT:-$ROOT/.claude/worktrees/nonmyopic-1067}
SOCK=${SOCK:-/tmp/cn.sock}
CFG=${CFG:-$ROOT/cn-mainnet-config/config.json}
OUT=${OUT:-$ROOT/reports/mainnet-exactness/cn-logepochstate}
FIFO=${FIFO:-/tmp/cn-epochstate.fifo}

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

mkdir -p "$OUT"
rm -f "$FIFO"
mkfifo "$FIFO"

# Wait for the node to actually serve queries. A socket FILE existing proves
# nothing — a dead node leaves one behind, which already misled this campaign
# once.
log "waiting for the node to answer queries on $SOCK"
until CARDANO_NODE_SOCKET_PATH=$SOCK timeout 30 cardano-cli query tip --mainnet >/dev/null 2>&1; do
  if ! pgrep -f "cardano-node run .*db-cn-mainnet" >/dev/null 2>&1; then
    log "ERROR node is not running; nothing to attach to"
    exit 1
  fi
  sleep 30
done
log "node is answering; attaching"

# Pin the FIFO open read-write for this script's lifetime BEFORE either end
# starts. Without this the writer races the reader's open() and dies:
#
#   cardano-cli: /tmp/cn-epochstate.fifo: withBinaryFile: does not exist
#                (Device not configured)
#
# which is ENXIO — opening a FIFO for write when no reader is attached yet.
# Backgrounding the reader first is NOT sufficient; cardano-cli starts faster
# than Python reaches its open(). An O_RDWR handle never blocks and guarantees
# a reader exists for as long as this script runs. Nothing ever reads from fd 3,
# so all data still reaches the reducer.
exec 3<>"$FIFO"

python3 "$WT/scripts/validation/reduce-epoch-state-log.py" \
  --fifo "$FIFO" --out-dir "$OUT" &
READER=$!

CARDANO_NODE_SOCKET_PATH=$SOCK cardano-cli debug log-epoch-state \
  --socket-path "$SOCK" \
  --node-configuration-file "$CFG" \
  --out-file "$FIFO" &
WRITER=$!

# An immediate writer death means no oracle at all, and the run would otherwise
# sit silently for days producing nothing.
sleep 5
if ! kill -0 $WRITER 2>/dev/null; then
  log "ERROR log-epoch-state died within 5s — no epochs will be captured"
  kill $READER 2>/dev/null
  exit 1
fi
log "writer alive after 5s"

log "reader=$READER writer=$WRITER out=$OUT"
trap 'kill $READER $WRITER 2>/dev/null; rm -f "$FIFO"' EXIT
wait $WRITER
log "log-epoch-state exited rc=$?"
