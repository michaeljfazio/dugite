#!/usr/bin/env bash
# One-shot health probe for the genesis-mode deferral soak. Exits 0 always;
# prints a compact line of the wedge-relevant signals. Metrics port 12796.
PID="$(pgrep -f 'target/release/dugite-node run' | head -1)"
TS="$(date -u +%H:%M:%SZ)"
if [ -z "$PID" ]; then echo "[$TS] NODE NOT RUNNING (no dugite-node run process)"; exit 0; fi
read -r RSS_KB PCPU < <(ps -o rss=,pcpu= -p "$PID" 2>/dev/null)
RSS_GB=$(awk "BEGIN{printf \"%.2f\", ${RSS_KB:-0}/1048576}")
M="$(curl -s --max-time 4 localhost:12796/metrics 2>/dev/null)"
g(){ echo "$M" | awk -v k="^$1 " '$0 ~ k {print $2; exit}'; }
echo "[$TS] pid=$PID rss=${RSS_GB}G cpu=${PCPU}% | mode=$(g dugite_consensus_mode) gsm=$(g dugite_gsm_state) | slot=$(g dugite_slot_number) blk=$(g dugite_block_number) ep=$(g dugite_epoch_number) era=$(g dugite_era) | peers=$(g dugite_peers_connected) hot=$(g dugite_peers_hot) | loe=$(g dugite_loe_tip_slot) gdd_disc=$(g dugite_gdd_disconnects_total) | defer_flush=$(g dugite_deferred_phase2_flushes_total) defer_blk=$(g dugite_deferred_phase2_blocks_total) | applyfail=$(g dugite_block_apply_failures_total) tipage=$(g dugite_tip_age_seconds) csidle=$(g dugite_chainsync_idle_seconds)"
