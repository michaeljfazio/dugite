#!/usr/bin/env bash
# 2-min health watcher for bare BP. Emits one status line per check,
# plus event lines on tip stall / peer collapse / forge / process death.
set -uo pipefail
PID="${1:-}"
[ -z "$PID" ] && { echo "usage: $0 <pid>"; exit 1; }

prev_slot=""; stale_count=0
while sleep 120; do
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "[$(date -u +%H:%M:%SZ)] EVENT proc_dead pid=$PID"; break
  fi
  m=$(curl -fsS --max-time 10 http://127.0.0.1:12798/metrics 2>/dev/null) || { echo "[$(date -u +%H:%M:%SZ)] EVENT metrics_unreachable"; continue; }
  slot=$(awk '/^dugite_slot_number /{print int($2)}' <<<"$m")
  age=$(awk '/^dugite_tip_age_seconds /{print int($2)}' <<<"$m")
  conn=$(awk '/^dugite_peers_connected /{print int($2)}' <<<"$m")
  hot=$(awk '/^dugite_peers_hot /{print int($2)}' <<<"$m")
  ino=$(awk '/^dugite_peers_inbound /{print int($2)}' <<<"$m")
  forged=$(awk '/^dugite_blocks_forged_total /{print int($2)}' <<<"$m")
  announced=$(awk '/^dugite_blocks_announced_total /{print int($2)}' <<<"$m")
  fail=$(awk '/^dugite_forge_failures_total /{print int($2)}' <<<"$m")
  rss=$(ps -p "$PID" -o rss= | awk '{printf "%.1fGB",$1/1048576}')
  ts=$(date -u +%H:%M:%SZ)

  status=ok
  if [ -z "$slot" ] || [ "$slot" = "0" ]; then status=no_slot
  elif [ "$slot" = "$prev_slot" ]; then stale_count=$((stale_count+1)); [ "$stale_count" -ge 2 ] && status=tip_stalled
  else stale_count=0
  fi
  [ "${conn:-0}" -eq 0 ] && status=zero_peers
  [ "${hot:-0}" -lt 5 ] && [ "$status" = "ok" ] && status=low_hot

  echo "[$ts] STATUS=$status slot=$slot age=${age}s peers=$conn hot=$hot in=$ino forged=$forged announced=$announced fails=$fail rss=$rss"
  prev_slot="$slot"
done
