#!/usr/bin/env bash
# launch-replay.sh <job-id> <db-dir> [extra dugite-node run args...]
# Forces a from-genesis replay over an existing (cloned) ImmutableDB and runs the
# node in the BACKGROUND under caffeinate, capturing PID + log under .jobs/.
#
# Instrumentation is inherited from the environment, so the runbook exports the
# right dump var before calling, e.g.:
#   DUGITE_EPOCH_STATE_DUMP=<dir>   (ledger byte-exactness items)
#   DUGITE_PHASE2_DUMP_DIR=<dir>    (phase-2 items)
# If neither is set, an epoch-state dump dir is defaulted so ledger replays
# always produce dumps.
#
# NOTE: the release binary must be built with the epoch-state-debug feature for
# DUGITE_EPOCH_STATE_DUMP to emit anything (see README / bootstrap preflight).
# shellcheck source=scripts/prod-readiness/lib/common.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

job="${1:?job id}"; db="${2:?db dir}"; shift 2 || true
[ -d "$db" ] || die "db dir not found: $db"
bin="$REPO_ROOT/target/release/dugite-node"
[ -x "$bin" ] || die "dugite-node binary not found at $bin (build it first)"

# Force from-genesis replay: remove snapshot(s) + utxo-store INSIDE the clone.
find "$db" -maxdepth 2 -name 'snapshot*' -prune -exec rm -rf {} + 2>/dev/null || true
rm -rf "$db/utxo-store" 2>/dev/null || true

# Default an epoch-state dump dir unless the runbook chose a phase-2 dump instead.
if [ -z "${DUGITE_EPOCH_STATE_DUMP:-}" ] && [ -z "${DUGITE_PHASE2_DUMP_DIR:-}" ]; then
  export DUGITE_EPOCH_STATE_DUMP="$DUMPS_DIR/$job"
  mkdir -p "$DUGITE_EPOCH_STATE_DUMP"
fi

logf="$JOBS_DIR/$job.log"; pidf="$JOBS_DIR/$job.pid"
caffeinate -dimsu "$bin" run --database-path "$db" "$@" >"$logf" 2>&1 &
echo $! > "$pidf"
log "launched replay job '$job' pid=$(cat "$pidf") db=$db log=$logf"
echo "$job"
