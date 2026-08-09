#!/usr/bin/env bash
# Drive the mainnet dugite-vs-cardano-streamer exactness comparison (#1073).
#
#   1. wait for the cardano-node oracle to pass TARGET_EPOCH
#   2. stop it cleanly (SIGTERM — kill -9 corrupts an ImmutableDB)
#   3. replay the same chain with cardano-streamer, dumping per epoch
#   4. diff the two dumps, bisecting to the first divergent epoch
#
# The two dumps are taken at the SAME instant by construction: cardano-streamer
# fires at `siFinal` when `isFirstSlotOfNewEpoch` using the post-block state,
# and dugite writes post-`apply_block` when `current_epoch > last_epoch`. Both
# label with the NEW epoch. Verify that before trusting any output.
#
# This env inherits ERR_EXIT from the caller's zsh, which re-enables itself when
# lib/common.sh-style sourcing happens; be explicit rather than assuming.
set -uo pipefail

DUGITE_ROOT=${DUGITE_ROOT:-/Users/michaelfazio/Source/dugite}
CSTREAMER=${CSTREAMER:-/Users/michaelfazio/Source/cardano-streamer/dist-newstyle/build/aarch64-osx/ghc-9.6.7/cardano-streamer-0.1.0.0/x/cstreamer/build/cstreamer/cstreamer}
CN_DB=${CN_DB:-$DUGITE_ROOT/db-cn-mainnet}
CN_CFG=${CN_CFG:-$DUGITE_ROOT/cn-mainnet-config/config.json}
OUT=${OUT:-$DUGITE_ROOT/reports/mainnet-exactness}
TARGET_EPOCH=${TARGET_EPOCH:-273}
export CARDANO_NODE_SOCKET_PATH=${CARDANO_NODE_SOCKET_PATH:-/tmp/cn.sock}

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

# ── 1. wait ──────────────────────────────────────────────────────────────
log "waiting for oracle to reach epoch $TARGET_EPOCH"
while true; do
  e=$(timeout 30 cardano-cli query tip --mainnet 2>/dev/null \
        | python3 -c "import sys,json; print(json.load(sys.stdin).get('epoch',''))" 2>/dev/null)
  if [ -z "$e" ]; then
    # An unreadable tip is UNMEASURED, not "not there yet". Say which.
    log "WARN tip unreadable — node stopped, or socket gone"
    sleep 120
    continue
  fi
  log "oracle at epoch $e"
  [ "$e" -ge "$TARGET_EPOCH" ] 2>/dev/null && break
  sleep 120
done

# ── 2. stop the oracle ───────────────────────────────────────────────────
# cstreamer CAN read a live ImmutableDB (measured), but it is CPU-bound and
# would contend with a still-syncing node for the whole replay.
pid=$(pgrep -f "cardano-node run .*db-cn-mainnet" | head -1)
if [ -n "$pid" ]; then
  log "stopping cardano-node pid=$pid with SIGTERM (never -9: it corrupts the ImmutableDB)"
  kill -TERM "$pid"
  for _ in $(seq 1 60); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 2
  done
  # kill's exit 0 does NOT prove death — verify.
  if kill -0 "$pid" 2>/dev/null; then
    log "ERROR cardano-node still alive after 120s; refusing to proceed"
    exit 1
  fi
  log "cardano-node stopped"
else
  log "no cardano-node running against $CN_DB"
fi

# ── 3. cardano-streamer replay ───────────────────────────────────────────
mkdir -p "$OUT/cstreamer"
# `--validate re` REAPPLIES blocks: the same ledger state transition without
# re-verifying signatures and scripts. For a ledger-state comparison that is
# equivalent — every block here is already on mainnet and therefore valid, and
# `reapplyLedgerBlock` and `applyLedgerBlock` produce the identical
# `LedgerState` for a valid block. It also MATCHES what dugite's own side did:
# `run_dump_snapshot` replays with `BlockValidationMode::ApplyOnly`. Comparing
# a full-validation replay against an apply-only one would be the less
# like-for-like choice, not the more rigorous one.
#
# `none` would be wrong — it does not compute the ledger at all.
VALIDATE=${VALIDATE:-re}
log "running cardano-streamer dump-epoch-snapshots (--validate $VALIDATE)"
caffeinate -dimsu "$CSTREAMER" \
  --chain-dir "$CN_DB" \
  --config "$CN_CFG" \
  --out-dir "$OUT/cstreamer" \
  --validate "$VALIDATE" \
  dump-epoch-snapshots
rc=$?
n=$(ls "$OUT/cstreamer" 2>/dev/null | wc -l | tr -d ' ')
log "cstreamer exited rc=$rc, wrote $n epoch files"
if [ "$n" -eq 0 ]; then
  log "ERROR cstreamer produced no output — the diff would be vacuous, not clean"
  exit 1
fi

# ── 4. diff ──────────────────────────────────────────────────────────────
log "diffing"
python3 "$DUGITE_ROOT/.claude/worktrees/nonmyopic-1067/scripts/validation/diff-cstreamer-dumps.py" \
  --dugite "$OUT/dugite" \
  --cstreamer "$OUT/cstreamer" \
  --json "$OUT/report.json"
drc=$?
log "diff exit=$drc  (0 clean / 1 divergent / 2 schema gap / 3 VACUOUS)"
exit $drc
