#!/usr/bin/env bash
# Drive the mainnet dugite-vs-cardano-streamer exactness comparison (#1073).
#
#   1. wait for the cardano-node oracle to pass TARGET_EPOCH
#   2. stop it cleanly (SIGTERM — kill -9 corrupts an ImmutableDB)
#   3. clone its chain for dugite (APFS copy-on-write, ~0 disk)
#   4. replay with dugite, dumping per epoch
#   5. replay the same chain with cardano-streamer, dumping per epoch
#   6. diff the two dumps, bisecting to the first divergent epoch
#
# Steps 3-4 used to be manual, which is how a comparison ends up unable to say
# which binary produced which side. Each step can be skipped for a re-run:
# SKIP_WAIT / SKIP_CLONE / SKIP_DUGITE.
#
# TARGET_EPOCH defaults to 273 — the range validated so far. Set it to the
# chain tip's epoch for a full run.
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
WT=${WT:-$DUGITE_ROOT/.claude/worktrees/nonmyopic-1067}
# Use the PINNED oracle binary, copied out of dist-newstyle with a recorded
# sha256 and commit (see oracle-bin/PROVENANCE.txt). Two separate hazards make
# "resolve it from the build tree" wrong, and they pull in opposite directions:
#
#   - Pinning a GHC version in the path goes STALE. The tree was built with
#     9.6.7, a patch was rebuilt with ghcup's default 9.6.5, and the hard-coded
#     9.6.7 path would have re-run the unpatched binary while the patch looked
#     applied.
#   - "Newest wins" picks up the WRONG BRANCH. The 10.6.2 and 10.7.1 branches
#     both resolve to ghc-9.6.5, so they share one dist-newstyle output dir: a
#     10.7.1 build overwrites the validated oracle in place, and a FAILED one
#     leaves a binary whose provenance no longer matches the checked-out tree.
#
# A copy outside the build tree is immune to both. Fall back to the build tree
# only if the pin is missing, and say so loudly.
CSTREAMER=${CSTREAMER:-/Users/michaelfazio/Source/cardano-streamer/oracle-bin/cstreamer-10.6.2}
if [ ! -x "$CSTREAMER" ]; then
  echo "WARN pinned oracle missing at $CSTREAMER — falling back to newest build" >&2
  CSTREAMER=$(ls -t /Users/michaelfazio/Source/cardano-streamer/dist-newstyle/build/*/*/cardano-streamer-*/x/cstreamer/build/cstreamer/cstreamer 2>/dev/null | head -1)
fi
CN_DB=${CN_DB:-$DUGITE_ROOT/db-cn-mainnet}
CN_CFG=${CN_CFG:-$DUGITE_ROOT/cn-mainnet-config/config.json}
OUT=${OUT:-$DUGITE_ROOT/reports/mainnet-exactness}
TARGET_EPOCH=${TARGET_EPOCH:-273}
export CARDANO_NODE_SOCKET_PATH=${CARDANO_NODE_SOCKET_PATH:-/tmp/cn.sock}

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

if [ -z "$CSTREAMER" ] || [ ! -x "$CSTREAMER" ]; then
  log "ERROR no cstreamer binary found — build it with 'cabal build cstreamer'"
  exit 1
fi
# Print WHICH binary, how old it is, and its HASH. A stale or swapped binary is
# the failure mode this comparison is most likely to hit twice, and it is
# invisible from the output. The hash is what distinguishes two builds that
# share a path; the mtime alone does not.
log "cstreamer: $CSTREAMER"
log "cstreamer built: $(stat -f '%Sm' -t '%Y-%m-%d %H:%M' "$CSTREAMER")"
log "cstreamer sha256: $(shasum -a 256 "$CSTREAMER" | cut -d' ' -f1)"

# ── 1. wait ──────────────────────────────────────────────────────────────
#
# SKIP_WAIT=1 when the ImmutableDB already covers the range and the node is
# already stopped — a re-run after a cstreamer failure, typically. Without it
# the loop cannot be short-circuited by lowering TARGET_EPOCH, because an
# unreadable tip `continue`s (correctly: unmeasured is not "target reached").
if [ "${SKIP_WAIT:-0}" = "1" ]; then
  log "SKIP_WAIT=1 — not waiting on the oracle"
else
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
fi

# ── 2. stop the oracle ───────────────────────────────────────────────────
# cstreamer CAN read a live ImmutableDB (measured), but it is CPU-bound and
# would contend with a still-syncing node for the whole replay.
#
# SKIP_STOP=1 leaves the node alone. This step has no business firing during a
# dry run, and it HAS: setting SKIP_WAIT/SKIP_CLONE/SKIP_DUGITE to exercise the
# guards still reached step 2 and terminated a mainnet sync 48% of the way in.
# Nothing was lost — SIGTERM is the clean stop and it resumed at the same epoch
# — but "the steps I skipped" is not the same set as "the steps that act", and
# the only skip that was missing was the one with an external side effect.
if [ "${SKIP_STOP:-0}" = "1" ]; then
  log "SKIP_STOP=1 — leaving any running cardano-node alone"
  pid=""
else
  pid=$(pgrep -f "cardano-node run .*db-cn-mainnet" | head -1)
fi
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

# ── 3. clone the chain for dugite ────────────────────────────────────────
#
# AFTER the node is stopped, so the tail chunk is complete. The clone script
# excludes the tail while a node is live, because a chunk mid-write makes the
# replay target move between runs.
#
# APFS copy-on-write, so a ~200 GB chain costs ~0 disk (measured: 13 GB cloned
# with free space unchanged). dugite needs its OWN copy rather than reading
# db-cn-mainnet directly: its open path may reconcile the secondary index,
# truncate the tail to its verified prefix or quarantine a chunk (#926-#929),
# and under a shared directory those repairs would land on the ORACLE's
# database — silently corrupting the thing being compared against.
DUGITE_DB=${DUGITE_DB:-$DUGITE_ROOT/db-mainnet-tip}
if [ "${SKIP_CLONE:-0}" = "1" ]; then
  log "SKIP_CLONE=1 — reusing $DUGITE_DB as-is"
else
  log "cloning $CN_DB -> $DUGITE_DB"
  bash "$WT/scripts/validation/clone-cn-immutable-for-dugite.sh" "$CN_DB" "$DUGITE_DB" || {
    log "ERROR clone failed"; exit 1; }
fi

# ── 4. dugite replay ─────────────────────────────────────────────────────
#
# dugite FIRST, even though it is the shorter of the two. It takes well under
# an hour where cstreamer takes many, so running it first means a broken
# binary, a missing config or a bad clone surfaces in minutes instead of after
# a whole oracle replay has been spent.
if [ "${SKIP_DUGITE:-0}" = "1" ]; then
  log "SKIP_DUGITE=1 — reusing existing dugite dumps"
else
  log "running dugite dump-snapshot"
  DUGITE_DUMP_DIGEST=1 DB="$DUGITE_DB" OUT="$OUT/dugite" \
    bash "$WT/scripts/validation/dugite-mainnet-dump.sh" || {
    log "ERROR dugite dump failed"; exit 1; }
fi

# ── 5. cardano-streamer replay ───────────────────────────────────────────
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

# Reduce oracle dumps AS THEY ARE WRITTEN, or the run dies on a full disk.
# Measured on the 208-273 range: 0.0 MB at epoch 208, 42 MB at 227, 108 MB at
# 247, 374 MB at 273 — superlinear in the delegator count, 8.3 GB for 66
# epochs. Extrapolated past Conway that is several hundred GB of oracle output
# for a full run, against ~224 GB free: the replay would have died somewhere
# past epoch 400, hours in, with nothing to show.
#
# `stake` and `delegations` are ~98% of each file and the comparator digests
# them anyway, so storing them raw buys nothing. Reducing in flight took the
# existing dumps from 8.3 GB to 630 MB with a BYTE-IDENTICAL verdict — 66
# paired epochs, 12,408 comparisons, 61 divergent, same field breakdown before
# and after.
log "starting the dump reducer (watch mode) — see reduce-cstreamer-dumps.py"
python3 "$WT/scripts/validation/reduce-cstreamer-dumps.py" \
  "$OUT/cstreamer" --watch --quiet &
reducer=$!
# Always reap it, including on a failed or interrupted replay: a watcher left
# running against a directory the next run recreates is a confusing way to lose
# an afternoon.
trap 'kill "$reducer" 2>/dev/null' EXIT

log "running cardano-streamer dump-epoch-snapshots (--validate $VALIDATE)"
caffeinate -dimsu "$CSTREAMER" \
  --chain-dir "$CN_DB" \
  --config "$CN_CFG" \
  --out-dir "$OUT/cstreamer" \
  --validate "$VALIDATE" \
  dump-epoch-snapshots
rc=$?

# Stop watching, then make a FINAL pass: the watcher never touches the newest
# file (cstreamer may still be writing it), so the last epoch is always left raw
# until the replay is over.
kill "$reducer" 2>/dev/null
trap - EXIT
python3 "$WT/scripts/validation/reduce-cstreamer-dumps.py" "$OUT/cstreamer" --all --quiet
n=$(ls "$OUT/cstreamer" 2>/dev/null | wc -l | tr -d ' ')
log "cstreamer exited rc=$rc, wrote $n epoch files"
if [ "$n" -eq 0 ]; then
  log "ERROR cstreamer produced no output — the diff would be vacuous, not clean"
  exit 1
fi

# ── 6. diff ──────────────────────────────────────────────────────────────
log "diffing"
python3 "$WT/scripts/validation/diff-cstreamer-dumps.py" \
  --dugite "$OUT/dugite" \
  --cstreamer "$OUT/cstreamer" \
  --json "$OUT/report.json"
drc=$?
log "diff exit=$drc  (0 clean / 1 divergent / 2 schema gap / 3 VACUOUS)"
exit $drc
