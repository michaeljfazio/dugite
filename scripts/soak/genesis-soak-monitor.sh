#!/usr/bin/env bash
# Persistent watcher for the genesis-mode deferral soak. Polls every 5 min.
# Emits a line (=> a notification) ONLY on: anomaly/wedge, milestone, or a
# ~30-min heartbeat. Silence between = healthy. Covers failure modes explicitly.
cd "$(dirname "$0")/../.."
prev_slot=-1; prev_era=-1; prev_gsm=-1; frozen=0; mfail=0; iter=0
while true; do
  iter=$((iter+1))
  PID="$(pgrep -f 'target/release/dugite-node run' | head -1)"
  if [ -z "$PID" ]; then
    echo "[$(date -u +%H:%MZ)] NODE EXITED — dugite-node process gone (crash or stopped). Check the soak log."
    break
  fi
  RSS_KB="$(ps -o rss= -p "$PID" 2>/dev/null | tr -d ' ')"
  PCPU="$(ps -o pcpu= -p "$PID" 2>/dev/null | tr -d ' ')"
  RSS_GB="$(awk "BEGIN{printf \"%.2f\", ${RSS_KB:-0}/1048576}")"
  M="$(curl -s --max-time 6 localhost:12796/metrics 2>/dev/null)"
  if [ -z "$M" ]; then
    mfail=$((mfail+1))
    [ "$mfail" -ge 2 ] && echo "[$(date -u +%H:%MZ)] ALERT metrics endpoint unreachable ${mfail}x (node up pid=$PID rss=${RSS_GB}G cpu=${PCPU}%) — possible hung metrics/apply"
    sleep 300; continue
  fi
  mfail=0
  gv(){ echo "$M" | awk -v k="^$1 " '$0 ~ k {print $2; exit}'; }
  slot="$(gv dugite_slot_number)"; era="$(gv dugite_era)"; gsm="$(gv dugite_gsm_state)"
  peers="$(gv dugite_peers_connected)"; hot="$(gv dugite_peers_hot)"
  dflush="$(gv dugite_deferred_phase2_flushes_total)"; dblk="$(gv dugite_deferred_phase2_blocks_total)"
  applyfail="$(gv dugite_block_apply_failures_total)"; loe="$(gv dugite_loe_tip_slot)"; gdd="$(gv dugite_gdd_disconnects_total)"
  busy="$(gv dugite_blockfetch_busy_us_total)"; sblk="$(gv dugite_blockfetch_send_blocked_us_total)"; nohdr="$(gv dugite_blockfetch_idle_no_headers_total)"
  : "${slot:=0}" "${era:=0}" "${gsm:=0}" "${peers:=0}" "${applyfail:=0}" "${busy:=0}" "${sblk:=0}" "${nohdr:=0}"
  fatals="$(cat reports/genesis-preview-resync-*.log 2>/dev/null | grep -cE 'found a block-fatal error|flush cancelled by shutdown|fatal at window head|does not connect to tip|panicked')"
  : "${fatals:=0}"
  # Fetch utilization over the poll interval: util% = busy_us delta / wall delta;
  # bp% = send_blocked (apply backpressure) delta / wall. now_ns from date.
  now_ns=$(date +%s%N)
  fetch="util=? bp=? nohdr=?"
  if [ "${prev_busy:-x}" != "x" ] && [ "${prev_ns:-0}" -gt 0 ]; then
    fetch="$(awk -v b=$busy -v pb=$prev_busy -v s=$sblk -v ps=$prev_sblk -v nh=$nohdr -v pnh=$prev_nohdr -v dn=$((now_ns-prev_ns)) 'BEGIN{
      w=dn/1000.0; if(w<=0){print "util=? bp=? nohdr=?"; exit}
      printf "util=%.0f%% bp=%.0f%% nohdr+=%d", 100*(b-pb)/w, 100*(s-ps)/w, (nh-pnh)}')"
  fi
  prev_busy=$busy; prev_sblk=$sblk; prev_nohdr=$nohdr; prev_ns=$now_ns
  stats="rss=${RSS_GB}G cpu=${PCPU}% slot=$slot era=$era gsm=$gsm peers=$peers/$hot loe=$loe defer=$dflush/$dblk gdd=$gdd applyfail=$applyfail fatals=$fatals fetch[$fetch]"

  alert=""
  # RSS: legitimate growth is bounded by the LSM cache (8G) + UTxO working set,
  # with LOW cpu while the tip advances. The deferral wedge was ~13.5G WITH cpu
  # pegged on all cores AND apply frozen. So: hard-alert only well above the
  # cache ceiling; otherwise require the runaway co-signature (frozen/pegged).
  if [ "$slot" = "$prev_slot" ] && [ "$gsm" != "2" ]; then frozen=$((frozen+1)); else frozen=0; fi
  cpu_int=${PCPU%.*}
  awk "BEGIN{exit !(${RSS_GB} > 11.0)}" && alert="WEDGE? RSS >11G (runaway)"
  [ "${cpu_int:-0}" -gt 900 ] 2>/dev/null && [ "$frozen" -ge 1 ] && alert="$alert WEDGE? cpu ${PCPU}% pegged + tip frozen"
  [ "$frozen" -ge 1 ] && awk "BEGIN{exit !(${RSS_GB} > 6.0)}" && alert="$alert WEDGE? frozen+RSS>6G (runaway forming)"
  [ "$frozen" -ge 2 ] && alert="$alert WEDGE? tip frozen ~$((frozen*5))min (slot=$slot)"
  [ "$peers" -lt 3 ] 2>/dev/null && alert="$alert ALERT peers<3"
  # HAA regression: GSM dropped from Syncing(1)/CaughtUp(2) back to PreSyncing(0)
  # — the faithful fix should NOT regress; if it does, condition-2 (all
  # established outbound ⊆ trusted) broke (e.g. peer-sharing added an untrusted peer).
  [ "$gsm" = "0" ] && [ "$prev_gsm" = "1" ] && alert="$alert ALERT HAA regression gsm 1->0"
  [ "$applyfail" -gt 0 ] 2>/dev/null && alert="$alert ALERT applyfail=$applyfail"
  [ "$fatals" -gt 0 ] 2>/dev/null && alert="$alert ALERT deferral-fatal/panic-in-log=$fatals"

  milestone=""
  [ "$era" != "$prev_era" ] && [ "$prev_era" != "-1" ] && milestone="era ${prev_era}->${era}"
  [ "$gsm" != "$prev_gsm" ] && [ "$prev_gsm" != "-1" ] && milestone="$milestone gsm ${prev_gsm}->${gsm}"
  if [ "$prev_slot" -lt 52000000 ] 2>/dev/null && [ "$slot" -ge 52000000 ] 2>/dev/null; then
    milestone="$milestone ENTERED-DENSE-PLUTUS-REGION(~slot52.27M where Praos wedged)"
  fi

  if [ -n "$alert" ]; then echo "[$(date -u +%H:%MZ)] $alert | $stats"
  elif [ -n "$milestone" ]; then echo "[$(date -u +%H:%MZ)] MILESTONE $milestone | $stats"
  elif [ $((iter % 6)) -eq 1 ]; then echo "[$(date -u +%H:%MZ)] OK | $stats"
  fi
  prev_slot="$slot"; prev_era="$era"; prev_gsm="$gsm"
  sleep 300
done
