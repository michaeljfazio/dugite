#!/usr/bin/env bash
# poll-job.sh <job-id>  — report a background job's status for the runbook.
# Prints one of: unknown | running | wedged | done  followed by the last log line.
# "wedged" = process alive but log file unmodified for > 5 minutes.
# shellcheck source=scripts/prod-readiness/lib/common.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

job="${1:?job id}"
pidf="$JOBS_DIR/$job.pid"; logf="$JOBS_DIR/$job.log"
[ -f "$pidf" ] || { echo "unknown"; exit 0; }
pid=$(cat "$pidf")
last=$(tail -n 1 "$logf" 2>/dev/null | tr -d '\r\n' | cut -c1-160)

if pid_alive "$pid"; then
  if [ -n "$(find "$logf" -mmin +5 2>/dev/null)" ]; then
    printf 'wedged pid=%s | %s\n' "$pid" "$last"
  else
    printf 'running pid=%s | %s\n' "$pid" "$last"
  fi
else
  printf 'done | %s\n' "$last"
fi
