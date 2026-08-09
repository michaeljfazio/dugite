#!/usr/bin/env bash
# Attach `cardano-cli debug log-epoch-state` to a syncing node and reduce its
# output to per-epoch digest files as it arrives.
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

# Reader first — opening a FIFO for write blocks until a reader exists.
python3 "$WT/scripts/validation/reduce-epoch-state-log.py" \
  --fifo "$FIFO" --out-dir "$OUT" &
READER=$!

CARDANO_NODE_SOCKET_PATH=$SOCK cardano-cli debug log-epoch-state \
  --socket-path "$SOCK" \
  --node-configuration-file "$CFG" \
  --out-file "$FIFO" &
WRITER=$!

log "reader=$READER writer=$WRITER out=$OUT"
trap 'kill $READER $WRITER 2>/dev/null; rm -f "$FIFO"' EXIT
wait $WRITER
log "log-epoch-state exited rc=$?"
