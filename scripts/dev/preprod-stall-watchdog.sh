#!/usr/bin/env bash
# Auto-sample-on-stall watchdog for the preprod resync (#sync-eval).
# Polls dugite_block_number; when it stops advancing for >= STALL_S, runs
# `sample` to capture the blocked thread stacks (the smoking gun), emits one
# line per stall-detect and per recovery. Low volume — safe as a Monitor.
#
# Usage: preprod-stall-watchdog.sh <pid> <metrics_port> <stall_secs>
set -uo pipefail
PID="$1"; PORT="$2"; STALL_S="${3:-25}"
OUTDIR=reports
last_blk=""; last_change=$(date +%s); sampled=0; stall_start=0
blkof() { curl -s --max-time 4 "http://localhost:$PORT/metrics" 2>/dev/null | grep -E '^dugite_block_number ' | awk '{print $2}'; }
while kill -0 "$PID" 2>/dev/null; do
  now=$(date +%s)
  blk=$(blkof)
  if [ -n "$blk" ]; then
    if [ "$blk" != "$last_blk" ]; then
      # progressed
      if [ "$sampled" = "1" ]; then
        echo "$(date -u +%H:%M:%SZ) RECOVERED block=$blk after $((now-stall_start))s stall"
      fi
      last_blk="$blk"; last_change="$now"; sampled=0
    else
      gap=$((now-last_change))
      if [ "$gap" -ge "$STALL_S" ] && [ "$sampled" = "0" ]; then
        stall_start="$last_change"
        TS=$(date -u +%Y%m%dT%H%M%SZ)
        F="$OUTDIR/preprod-stall-sample-blk${blk}-$TS.txt"
        echo "$(date -u +%H:%M:%SZ) STALL block=$blk gap=${gap}s — sampling 8s -> $F"
        sample "$PID" 8 -mayDie -f "$F" >/dev/null 2>&1
        echo "$(date -u +%H:%M:%SZ) STALL-SAMPLE-DONE $F"
        sampled=1
      fi
    fi
  fi
  sleep 5
done
echo "$(date -u +%H:%M:%SZ) WATCHDOG-EXIT pid $PID gone"
