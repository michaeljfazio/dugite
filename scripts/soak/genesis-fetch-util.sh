#!/usr/bin/env bash
# Fetch-path utilization probe. Samples the new instrumentation over a window and
# splits wall-time into: network-active / apply-backpressure / header-supply-idle.
# Answers "are we idle-wait-blocking, and why" with certainty. Metrics port 12796.
WIN="${1:-30}"
g(){ echo "$1" | awk -v k="^$2 " '$0 ~ k {print $2; exit}'; }
M1="$(curl -s --max-time 5 localhost:12796/metrics)"; t1=$(date +%s%N)
busy1=$(g "$M1" dugite_blockfetch_busy_us_total); sb1=$(g "$M1" dugite_blockfetch_send_blocked_us_total)
nh1=$(g "$M1" dugite_blockfetch_idle_no_headers_total); rx1=$(g "$M1" dugite_blockfetch_rx_bytes_total)
b1=$(g "$M1" dugite_block_number); s1=$(g "$M1" dugite_slot_number)
sleep "$WIN"
M2="$(curl -s --max-time 5 localhost:12796/metrics)"; t2=$(date +%s%N)
busy2=$(g "$M2" dugite_blockfetch_busy_us_total); sb2=$(g "$M2" dugite_blockfetch_send_blocked_us_total)
nh2=$(g "$M2" dugite_blockfetch_idle_no_headers_total); rx2=$(g "$M2" dugite_blockfetch_rx_bytes_total)
b2=$(g "$M2" dugite_block_number); s2=$(g "$M2" dugite_slot_number)
awk -v busy=$((busy2-busy1)) -v sb=$((sb2-sb1)) -v nh=$((nh2-nh1)) \
    -v rx=$((rx2-rx1)) -v db=$((b2-b1)) -v ds=$((s2-s1)) -v wns=$((t2-t1)) 'BEGIN{
  wall_us = wns/1000.0
  util = busy/wall_us
  net  = (busy-sb)/wall_us
  bp   = sb/wall_us
  freeidle = (wall_us-busy)/wall_us
  printf "window=%.1fs\n", wall_us/1e6
  printf "FETCHER UTILIZATION: %.0f%% busy (slot held), %.0f%% idle (slot free)\n", 100*util, 100*freeidle
  printf "  - network-active:      %.0f%% (downloading)\n", 100*net
  printf "  - apply-backpressure:  %.0f%% (blocked on fetched_blocks send)\n", 100*bp
  printf "  - slot-free idle:      %.0f%% (no peer fetching)\n", 100*freeidle
  printf "no-headers ticks: %d in window (header-supply-bound idle if high)\n", nh
  printf "throughput: %.2f MB/s, %.1f blk/s, %.0f slots/s\n", rx/1048576/(wall_us/1e6), db/(wall_us/1e6), ds/(wall_us/1e6)
}'
