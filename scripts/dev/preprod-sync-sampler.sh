#!/usr/bin/env bash
# Periodic sync-speed sampler for the preprod from-genesis resync (#sync-eval).
# Writes one CSV row per interval. No stdout flood — read the CSV to analyze.
#
# Usage: preprod-sync-sampler.sh <pid> <metrics_port> <csv_out> [interval_s]
set -uo pipefail
PID="$1"; PORT="$2"; OUT="$3"; INT="${4:-15}"

cputime_secs() {
  # ps cputime is [[DD-]HH:]MM:SS(.ss); normalize to float seconds.
  local t; t=$(ps -o cputime= -p "$1" 2>/dev/null | tr -d ' ')
  [ -z "$t" ] && { echo ""; return; }
  echo "$t" | awk -F'[:-]' '{
    n=NF; s=0;
    if (n==1){ s=$1 }
    else if (n==2){ s=$1*60+$2 }
    else if (n==3){ s=$1*3600+$2*60+$3 }
    else { s=$1*86400+$2*3600+$3*60+$4 }
    printf "%.2f", s
  }'
}

m() { # extract a single metric value by exact name from the cached metrics blob
  grep -E "^$1 " <<<"$METRICS" | head -1 | awk '{print $2}'
}

if [ ! -f "$OUT" ]; then
  echo "iso,epoch_s,block,slot,applied,received,rollback,not_connecting,hot,warm,cold,connected,fetch_ms_count,fetch_ms_sum,cputime_s,rss_kb" > "$OUT"
fi

while kill -0 "$PID" 2>/dev/null; do
  METRICS=$(curl -s --max-time 5 "http://localhost:$PORT/metrics" 2>/dev/null)
  ISO=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  EP=$(date +%s)
  BLK=$(m dugite_block_number); SLT=$(m dugite_slot_number)
  APP=$(m dugite_blocks_applied_total); RCV=$(m dugite_blocks_received_total)
  RBK=$(m dugite_rollback_count_total); NCN=$(m dugite_fetched_blocks_not_connecting_total)
  HOT=$(m dugite_peers_hot); WRM=$(m dugite_peers_warm); CLD=$(m dugite_peers_cold); CON=$(m dugite_peers_connected)
  FMC=$(m dugite_peer_block_fetch_ms_count); FMS=$(m dugite_peer_block_fetch_ms_sum)
  CPT=$(cputime_secs "$PID")
  RSS=$(ps -o rss= -p "$PID" 2>/dev/null | tr -d ' ')
  echo "$ISO,$EP,${BLK:-},${SLT:-},${APP:-},${RCV:-},${RBK:-},${NCN:-},${HOT:-},${WRM:-},${CLD:-},${CON:-},${FMC:-},${FMS:-},${CPT:-},${RSS:-}" >> "$OUT"
  sleep "$INT"
done
