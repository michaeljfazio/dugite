#!/usr/bin/env bash
# gc-disk.sh {gc-clones <net> | gc-dumps <net> | fits <gb>}
# Autonomous disk hygiene: keep the last N db-clones/dumps per net, and a
# predicate that says whether a clone of <gb> would keep free space >= MIN_FREE_GB.
# shellcheck source=scripts/prod-readiness/lib/common.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

cmd="${1:?need a subcommand: gc-clones, gc-dumps, or fits}"; arg="${2:-}"

keep_last() {  # <dir> <glob> <keepN> : remove all but the newest <keepN> matches
  local dir="$1" glob="$2" keep="$3"
  [ -d "$dir" ] || return 0
  # newest-first; drop the first <keep>, delete the rest
  find "$dir" -maxdepth 1 -name "$glob" -print0 2>/dev/null \
    | xargs -0 ls -1dt 2>/dev/null \
    | tail -n +"$((keep+1))" \
    | while IFS= read -r p; do log "GC removing $p"; rm -rf "$p"; done
}

case "$cmd" in
  gc-clones) keep_last "$CLONES_DIR" "${arg:?net}-*" "$KEEP_CLONES" ;;
  gc-dumps)  keep_last "$DUMPS_DIR"  "${arg:?net}-*" "$KEEP_DUMPS" ;;
  fits)      need="${arg:?gb}"; have=$(free_disk_gb); [ "$(( have - need ))" -ge "$MIN_FREE_GB" ] ;;
  *) die "usage: gc-disk.sh {gc-clones <net>|gc-dumps <net>|fits <gb>}" ;;
esac
