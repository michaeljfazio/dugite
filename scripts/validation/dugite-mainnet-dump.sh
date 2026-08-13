#!/usr/bin/env bash
# Replay mainnet from genesis with dugite and dump per-epoch ledger state.
#
# The oracle half of this comparison already had a driver
# (`mainnet-exactness-run.sh`); dugite's half was run by hand, which is how a
# comparison ends up unable to say which binary produced which side. This makes
# it repeatable and records the binary's identity next to its output.
#
# DUGITE_DUMP_DIGEST=1 reduces `stake`/`delegations` to
# {__count__, __sum__, __digest__}. At mainnet scale the raw maps dominate the
# dump; the digest is the same canonical form the comparator and cstreamer
# reducer use, verified against a shared test vector, so a mismatch is still
# caught — it just does not cost tens of GB to store.
#
# It is DEFAULTED here rather than only documented. This comment described the
# variable as if the script set it while only `mainnet-exactness-run.sh` did, so
# every hand-rolled invocation of this driver silently paid the undigested cost
# — measured at 3.1 GB against 0.4 GB for 306 preprod epochs, and mainnet is far
# worse. Export `DUGITE_DUMP_DIGEST=0` to opt out when drilling into the raw
# maps.
set -uo pipefail
export DUGITE_DUMP_DIGEST=${DUGITE_DUMP_DIGEST:-1}

DUGITE_ROOT=${DUGITE_ROOT:-/Users/michaelfazio/Source/dugite}
WT=${WT:-$DUGITE_ROOT/.claude/worktrees/nonmyopic-1067}
BIN=${BIN:-$WT/target/release/dugite-node}
# The config comes from the SAME TREE as the binary.
#
# This defaulted to `$DUGITE_ROOT/config/mainnet/config.json` — the main
# checkout — while the binary came from `$WT`. Those are different trees at
# different commits, and the main checkout's config was stale: it pinned
# `ByronGenesisHash: dbbdaeab…` where mainnet's is `5f20df93…`, and carried no
# Alonzo or Conway genesis pin at all. A tip run that had waited 16 hours for
# the oracle to reach its target epoch then died instantly on
#
#   Byron genesis hash mismatch … refusing to start rather than build a ledger
#   from an unverified genesis
#
# which is the guard behaving exactly as intended — the failure was pointing it
# at a config the code under test does not ship. Sourcing both from `$WT` makes
# the pairing structural instead of coincidental.
CFG=${CFG:-$WT/config/mainnet/config.json}
DB=${DB:-$DUGITE_ROOT/db-mainnet-avvm}
OUT=${OUT:-$DUGITE_ROOT/reports/mainnet-exactness/dugite-digest}
STOP_SLOT=${STOP_SLOT:-}

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

[ -x "$BIN" ] || { log "ERROR no dugite-node at $BIN (cargo build --release -p dugite-node)"; exit 1; }
[ -f "$CFG" ] || { log "ERROR no config at $CFG"; exit 1; }
[ -d "$DB/immutable" ] || { log "ERROR no chain at $DB/immutable"; exit 1; }

# Say WHICH binary produced this, and hash it. A stale binary is the failure
# mode this whole comparison is most likely to hit, and it is invisible in the
# output — the dumps look perfectly well-formed either way.
log "binary:  $BIN"
log "built:   $(stat -f '%Sm' -t '%Y-%m-%d %H:%M' "$BIN")"
log "sha256:  $(shasum -a 256 "$BIN" | cut -d' ' -f1)"
log "commit:  $(cd "$WT" && git rev-parse --short HEAD) $(cd "$WT" && git diff --quiet || echo '(DIRTY)')"
log "chain:   $DB"
log "out:     $OUT"

# Never merge a new run into an old one. Leftover files from a previous binary
# are indistinguishable from this run's, and the epochs the new run does not
# reach would silently keep their stale contents — a diff would then compare
# two binaries' output and call it one.
if [ -d "$OUT" ]; then
  ts=$(date -u +%Y%m%dT%H%M%SZ)
  log "moving existing $OUT aside to ${OUT}.pre-$ts"
  mv "$OUT" "${OUT}.pre-$ts" || exit 1
fi
mkdir -p "$OUT" || exit 1

args=(dump-snapshot --config "$CFG" --database-path "$DB" --output-dir "$OUT")
[ -n "$STOP_SLOT" ] && args+=(--stop-slot "$STOP_SLOT")

# caffeinate: a sleeping Mac stalls the replay while ps, the socket and the
# process all still look alive.
start=$(date +%s)
caffeinate -dimsu "$BIN" "${args[@]}"
rc=$?
elapsed=$(( $(date +%s) - start ))

n=$(ls "$OUT" 2>/dev/null | wc -l | tr -d ' ')
log "dump-snapshot exited rc=$rc after ${elapsed}s, wrote $n epoch files"
[ "$n" -gt 0 ] || { log "ERROR no epoch files — a diff against this would be vacuous, not clean"; exit 1; }
exit $rc
