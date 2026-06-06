#!/usr/bin/env bash
# health-sample.sh — emit a one-line JSON snapshot of node + host health.
# Degrades gracefully when nothing is running. The runbook parses this in ASSESS.
# Optional env to sample a live node's tip:  HS_SOCKET=<path> HS_MAGIC=<n>
# shellcheck source=scripts/prod-readiness/lib/common.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

node_pids=$(pgrep -f 'dugite-node run' || true)
rss_mb=0
for p in $node_pids; do
  r=$(ps -o rss= -p "$p" 2>/dev/null | awk '{print int($1/1024)}')
  rss_mb=$(( rss_mb + ${r:-0} ))
done

# tip slot via dugite-cli if a socket+magic are given and the socket exists
tip_slot="null"
if [ -n "${HS_SOCKET:-}" ] && [ -n "${HS_MAGIC:-}" ] && [ -S "$HS_SOCKET" ]; then
  raw=$("$REPO_ROOT/target/release/dugite-cli" query tip \
        --socket-path "$HS_SOCKET" --testnet-magic "$HS_MAGIC" 2>/dev/null || true)
  if [ -n "$raw" ]; then
    printf '%s' "$raw" > "$JOBS_DIR/last-tip.json"
    s=$(printf '%s' "$raw" | grep -oE '"slot"[[:space:]]*:[[:space:]]*[0-9]+' | grep -oE '[0-9]+' | head -1)
    [ -n "$s" ] && tip_slot="$s"
  fi
fi

jobs_running=$(find "$JOBS_DIR" -maxdepth 1 -name '*.pid' 2>/dev/null | wc -l | tr -d ' ')
halt=$( { [ -f "$HALT_FILE" ] || grep -qi '^- HALT: true' "$STATE_FILE" 2>/dev/null; } && echo true || echo false )

printf '{"node_pids":"%s","rss_mb":%s,"free_disk_gb":%s,"free_ram_gb":%s,"jobs_running":%s,"halt":%s,"tip_slot":%s}\n' \
  "$(echo "$node_pids" | tr '\n' ' ' | sed 's/ *$//')" \
  "$rss_mb" "$(free_disk_gb)" "$(free_ram_gb)" "$jobs_running" "$halt" "$tip_slot"
