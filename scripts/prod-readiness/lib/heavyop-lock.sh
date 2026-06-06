#!/usr/bin/env bash
# heavyop-lock.sh {acquire <label> | bind <pid> | release | status}
# Enforces ONE heavy local op at a time. A lock is STALE (reclaimable) when its
# bound pid is dead OR its age exceeds HEAVYOP_TTL_SECS. Usage from the runbook:
#   acquire <label>            # gate before launching a replay
#   launch-replay.sh ...       # -> writes .jobs/<job>.pid
#   bind "$(cat .jobs/<job>.pid)"   # point the lock at the real bg job
#   ... (later wake observes job done) ...
#   release
# shellcheck source=scripts/prod-readiness/lib/common.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

cmd="${1:-status}"
case "$cmd" in
  acquire)
    label="${2:?label required}"
    if [ -f "$LOCK_FILE" ]; then
      hpid=$(awk -F= '/^pid=/{print $2}' "$LOCK_FILE")
      hstart=$(awk -F= '/^start=/{print $2}' "$LOCK_FILE")
      now=$(date +%s); age=$(( now - ${hstart:-0} ))
      if pid_alive "${hpid:-0}" && [ "$age" -lt "$HEAVYOP_TTL_SECS" ]; then
        log "heavy-op lock held by pid $hpid (age ${age}s); cannot acquire for '$label'"
        exit 1
      fi
      log "reclaiming stale lock (pid=$hpid age=${age}s)"
    fi
    # pid=0 == unbound; gated only by TTL until bind points it at the real job
    printf 'pid=0\nstart=%s\nlabel=%s\n' "$(date +%s)" "$label" > "$LOCK_FILE"
    log "heavy-op lock acquired for '$label'"
    ;;
  bind)
    pid="${2:?pid required}"
    [ -f "$LOCK_FILE" ] || die "no lock to bind"
    # rewrite only the pid line, preserve start+label
    awk -v p="$pid" 'BEGIN{FS=OFS="="} /^pid=/{$2=p} {print}' "$LOCK_FILE" > "$LOCK_FILE.tmp"
    mv "$LOCK_FILE.tmp" "$LOCK_FILE"
    log "heavy-op lock bound to pid $pid"
    ;;
  release) rm -f "$LOCK_FILE"; log "heavy-op lock released" ;;
  status)  [ -f "$LOCK_FILE" ] && cat "$LOCK_FILE" || echo "free" ;;
  *) die "usage: heavyop-lock.sh {acquire <label>|bind <pid>|release|status}" ;;
esac
