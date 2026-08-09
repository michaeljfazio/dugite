#!/usr/bin/env bash
# Give dugite its own writable copy of the mainnet ImmutableDB that cardano-node
# synced, at ~zero disk cost, so both sides of the byte-exactness comparison can
# replay the SAME chain to tip.
#
# ── Why this exists ──────────────────────────────────────────────────────────
#
# The comparison needs the full mainnet chain on BOTH sides: cstreamer replays
# cardano-node's ImmutableDB, dugite replays its own. A full mainnet chain is
# ~200 GB, and this machine had 264 GB free — two independent copies do not fit,
# which would have forced a sequential sync/dump/delete/re-sync dance.
#
# Two facts make that unnecessary:
#
#   1. dugite's ChainDB reads cardano-node's NATIVE chunk format directly. It is
#      not a conversion: `mithril-import` downloads cardano-node immutable chunk
#      files and merely MOVES them into `<db>/immutable`, and ChainDB serves
#      historical blocks straight out of them (crates/dugite-node/src/mithril.rs,
#      "Moving chunk files to permanent storage ... the directory is NOT deleted
#      after replay — it is the permanent immutable store").
#
#   2. The volume is APFS, so `cp -c` uses clonefile: copy-on-write, independent
#      files that share storage until written. MEASURED on a real chunk here,
#      not assumed — a 14,788,562-byte chunk cloned for 4 KB of free-space delta,
#      and overwriting byte 0 of the clone left the source hash unchanged.
#
# Independence is the property that matters, and it is why this uses `cp -c` and
# NOT `ln`. dugite's open path may legitimately REWRITE what it finds: #926-#929
# reconcile the secondary index, CRC-scan the tail chunk and truncate it to its
# verified prefix, and quarantine an index-less chunk as `.chunk.orphaned`. With
# hardlinks those repairs would land on the ORACLE's own database — silently
# corrupting the thing being compared against. A clone absorbs them.
#
# ── Usage ────────────────────────────────────────────────────────────────────
#   scripts/validation/clone-cn-immutable-for-dugite.sh [SRC_DB] [DST_DB]
#
# Defaults to db-cn-mainnet -> db-mainnet-tip under $DUGITE_ROOT.
set -uo pipefail

DUGITE_ROOT=${DUGITE_ROOT:-/Users/michaelfazio/Source/dugite}
SRC=${1:-$DUGITE_ROOT/db-cn-mainnet}
DST=${2:-$DUGITE_ROOT/db-mainnet-tip}

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

[ -d "$SRC/immutable" ] || { log "ERROR no $SRC/immutable"; exit 1; }

# The tail chunk of a LIVE node is mid-write. dugite would CRC-scan and truncate
# it to its verified prefix, which is safe on a clone but makes the copy's
# contents depend on when it ran — and a replay target whose end moves between
# runs is not a fixture. Drop the highest chunk while the node is up, and say so.
LIVE=0
if pgrep -f "cardano-node run .*$(basename "$SRC")" >/dev/null 2>&1; then
  LIVE=1
  log "WARN cardano-node is RUNNING against $SRC — excluding the tail chunk"
  log "     (re-run after a clean stop to capture the final chunk)"
fi

mkdir -p "$DST/immutable" || exit 1

# Highest chunk number present, so the tail can be excluded by NAME rather than
# by mtime — mtime would also exclude chunks merely touched by a reconciliation.
top=$(ls "$SRC/immutable" 2>/dev/null | sed -n 's/^\([0-9]\{5\}\)\.chunk$/\1/p' | sort -n | tail -1)
[ -n "$top" ] || { log "ERROR no chunk files in $SRC/immutable"; exit 1; }
log "source tail chunk: $top"

before=$(df -k "$DUGITE_ROOT" | awk 'NR==2{print $4}')
copied=0 skipped=0 failed=0
for f in "$SRC"/immutable/*; do
  b=$(basename "$f")
  if [ "$LIVE" = 1 ] && [ "${b%%.*}" = "$top" ]; then
    skipped=$((skipped + 1)); continue
  fi
  # -c clonefile, -p preserve times. Fall back is deliberately ABSENT: a plain
  # copy would silently consume ~200 GB, and a disk-full failure halfway through
  # is worse than refusing up front.
  if cp -c -p "$f" "$DST/immutable/$b" 2>/dev/null; then
    copied=$((copied + 1))
  else
    failed=$((failed + 1))
    log "ERROR clonefile failed for $b — is $DST on the same APFS volume as $SRC?"
    break
  fi
done
after=$(df -k "$DUGITE_ROOT" | awk 'NR==2{print $4}')

log "cloned=$copied skipped=$skipped failed=$failed"
log "free KB: $before -> $after (delta $((before - after)) KB)"

if [ "$failed" -gt 0 ]; then
  log "ERROR clone incomplete — refusing to leave a partial chain in place"
  exit 1
fi

# A clone that cost real disk means clonefile silently degraded to a full copy,
# which is the one outcome this script exists to prevent. 200 GB of chain would
# show up as tens of millions of KB; allow generous slack for concurrent writers.
delta=$((before - after))
if [ "$delta" -gt 2000000 ]; then
  log "WARN clone consumed ${delta} KB — that is not copy-on-write behaviour"
fi

log "dugite chain ready at $DST (replay with --no-include-ancillary semantics:"
log "  ledger state must come from chunk replay, never from Haskell's snapshot)"
