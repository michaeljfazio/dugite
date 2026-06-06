#!/usr/bin/env bash
# common.sh — shared paths, config, logging, and JSON helpers for the engine.
# Source this from every lib/*.sh:  . "$(dirname "${BASH_SOURCE[0]}")/common.sh"
# Vars below are consumed by sourcing scripts; SC2034 (unused) is expected here.
# shellcheck disable=SC2034
set -euo pipefail

# --- canonical paths (repo-root relative) ---
ENGINE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$ENGINE_DIR/../.." && pwd)"
STATE_FILE="$ENGINE_DIR/engine-state.md"
RUNBOOK="$ENGINE_DIR/engine-runbook.md"
LOCK_FILE="$ENGINE_DIR/.engine-heavyop.lock"
HALT_FILE="$ENGINE_DIR/.engine-halt"
JOBS_DIR="$ENGINE_DIR/.jobs"          # per-background-job PID+log metadata
CLONES_DIR="$REPO_ROOT/db-clones"     # APFS clones live here
DUMPS_DIR="$REPO_ROOT/epoch-dumps-engine"

# --- tunables (overridable via env, defaults match the spec) ---
HEAVYOP_TTL_SECS="${HEAVYOP_TTL_SECS:-21600}"   # 6h: a replay should finish inside this
KEEP_CLONES="${KEEP_CLONES:-2}"                 # keep last N db-clones per net
KEEP_DUMPS="${KEEP_DUMPS:-4}"                   # keep last N dump dirs per net
MIN_FREE_GB="${MIN_FREE_GB:-40}"                # refuse a clone if it would drop below this

mkdir -p "$JOBS_DIR" "$CLONES_DIR" "$DUMPS_DIR"

log()  { printf '[engine %s] %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
die()  { log "FATAL: $*"; exit 1; }

# free disk in GB on the volume holding the repo
free_disk_gb() { df -g "$REPO_ROOT" | awk 'NR==2 {print $4}'; }
# available RAM in GB (macOS: free + inactive + speculative pages are all reclaimable)
free_ram_gb()  {
  vm_stat | awk '
    /Pages free/        {gsub(/\./,"",$3); f=$3}
    /Pages inactive/    {gsub(/\./,"",$3); i=$3}
    /Pages speculative/ {gsub(/\./,"",$3); s=$3}
    END {print int((f+i+s)*4096/1024/1024/1024)}'
}
# is a PID alive?
pid_alive()    { kill -0 "$1" 2>/dev/null; }
