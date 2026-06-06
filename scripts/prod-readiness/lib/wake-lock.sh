#!/usr/bin/env bash
# wake-lock.sh {acquire|release|status} — at most ONE engine wake runs at a time.
# Needed for unattended cron operation: if a cron fire lands while a previous wake
# is still mid-flight (e.g. a long fix+nextest wake), the new wake must NOT start a
# second concurrent pass (which would double-spawn Workflows / replays). TTL-based so
# a crashed wake can never wedge the loop permanently.
# shellcheck source=scripts/prod-readiness/lib/common.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WAKE_LOCK="${WAKE_LOCK:-$ENGINE_DIR/.engine-wake.lock}"
WAKE_TTL="${WAKE_TTL:-1320}"   # 22 min — longer than any healthy single wake, < cron period

case "${1:-status}" in
  acquire)
    if [ -f "$WAKE_LOCK" ]; then
      start=$(cat "$WAKE_LOCK" 2>/dev/null || echo 0)
      now=$(date +%s); age=$(( now - ${start:-0} ))
      if [ "$age" -lt "$WAKE_TTL" ]; then
        echo "busy age=${age}s"; exit 1
      fi
      log "reclaiming stale wake-lock (age=${age}s)"
    fi
    date +%s > "$WAKE_LOCK"; echo "acquired"
    ;;
  release) rm -f "$WAKE_LOCK"; echo "released" ;;
  status)  [ -f "$WAKE_LOCK" ] && echo "held" || echo "free" ;;
  *) die "usage: wake-lock.sh {acquire|release|status}" ;;
esac
