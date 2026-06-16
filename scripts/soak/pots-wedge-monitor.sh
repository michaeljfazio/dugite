#!/usr/bin/env bash
# Soak monitor for the from-genesis mainnet #763 validation run.
# Emits ONE event line per actionable signal only:
#   - POTS DIVERGENCE ep{N}  (reserves/treasury Δ != 0 vs Koios mainnet /totals)
#   - WEDGE #767             (block did not advance for 2 consecutive 60s checks; auto-samples)
#   - ERA {name}             (era boundary crossed)
#   - HEARTBEAT              (once/hour: epoch, block, era, pots status, blk/s)
#   - SOAK STOPPED           (process gone -> exit)
# Everything else (per-epoch byte-exact OK) is tracked silently and folded into HEARTBEAT.
set -uo pipefail

METRICS="http://127.0.0.1:12801/metrics"
# Match by command line, not a fixed PID, so the monitor survives a node
# restart (e.g. #767 wedge recovery).
NODE_PAT="release-prof/dugite-node.*db-mainnet-genesis"
# Read the active log path from the pointer file (updated on each relaunch) so
# the monitor follows the node across restarts.
LOG=$(cat /tmp/dugite-mainnet-soak-current.log 2>/dev/null || echo /private/tmp/dugite-mainnet-soak-20260615-154625.log)
REPORTS=/Users/michaelfazio/Source/dugite/reports
WEDGE_STALL=30  # consecutive 60s ticks with no block advance before sampling
                # (>=30min). Raised from 5: the KNOWN #767 residual (peer-Slow-
                # cascade) self-recovers within ~5-7min, so a 5min threshold spams
                # samples; only a genuine >30min hard wedge (beyond the residual)
                # should alert now.
# Match ERROR/FATAL *log-level* lines (anchored on the tracing timestamp so we
# don't match "ERROR" inside a message), plus panics and REAL divergence
# signals. Deliberately NOT a bare "diverg" — that matches the benign
# `chain_diverged=false` field present on every routine ChainSync INFO line.
ERR_PAT='Z +ERROR |Z +FATAL |panicked|chain_diverged=true|reserves mismatch|treasury mismatch|unable to store'

era_for() { # epoch -> era name (mainnet)
  local e=$1
  if   [ "$e" -lt 208 ]; then echo Byron
  elif [ "$e" -lt 236 ]; then echo Shelley
  elif [ "$e" -lt 251 ]; then echo Allegra
  elif [ "$e" -lt 290 ]; then echo Mary
  elif [ "$e" -lt 365 ]; then echo Alonzo
  elif [ "$e" -lt 507 ]; then echo Babbage
  else echo Conway; fi
}

metric() { curl -s --max-time 8 "$METRICS" 2>/dev/null | grep -E "^$1 " | awk '{print $2}'; }

koios_pots() { # epoch -> "reserves treasury" (mainnet ground truth)
  curl -s --max-time 20 "https://api.koios.rest/api/v1/totals?_epoch_no=$1" -H "accept: application/json" 2>/dev/null \
    | python3 -c "import sys,json;d=json.load(sys.stdin);r=d[0] if d else {};print(r.get('reserves','?'),r.get('treasury','?'))" 2>/dev/null
}

last_epoch=-1
last_block=-1
last_era=""
stall=0
pots_status="unknown"
pots_epoch="?"
hb_count=0          # heartbeat once every 60 loops (~60 min)
block_hr_start=-1   # block number at start of the heartbeat hour
# Baseline the error scan at the current EOF so pre-existing/resolved errors
# (e.g. an already-handled disk-space warning) are never re-reported.
last_size=$(stat -f%z "$LOG" 2>/dev/null || echo 0)

while true; do
  ep=$(metric dugite_epoch_number)
  blk=$(metric dugite_block_number)

  # ---- liveness / stop detection ----
  node_pid=$(pgrep -f "$NODE_PAT" | head -1)
  if [ -z "$ep" ] || [ -z "$blk" ]; then
    if [ -z "$node_pid" ]; then
      echo "SOAK STOPPED: node ($NODE_PAT) gone; last seen ep${last_epoch} block${last_block}"
      exit 0
    fi
    # metrics briefly unreachable but process alive -> skip this tick quietly
    sleep 60; continue
  fi

  [ "$block_hr_start" = "-1" ] && block_hr_start=$blk

  # ---- wedge detection ----
  if [ "$blk" = "$last_block" ]; then
    stall=$((stall+1))
    if [ "$stall" -ge "$WEDGE_STALL" ]; then
      ts=$(date -u +%Y%m%dT%H%M%SZ)
      out="$REPORTS/767-wedge-sample-$ts.txt"
      [ -n "$node_pid" ] && sample "$node_pid" 5 -mayDie -f "$out" >/dev/null 2>&1 &
      echo "WEDGE #767 at ep${ep} block${blk} slot$(metric dugite_slot_number): no block advance across ${WEDGE_STALL}x60s; sampling -> $out"
      stall=0   # don't re-fire every tick; wait for next genuine stall run
    fi
  else
    stall=0
  fi

  # ---- new-error scan (since-offset: only bytes appended since last tick, so
  #      a resolved error never re-alerts; reports each new occurrence once) ----
  cur_size=$(stat -f%z "$LOG" 2>/dev/null || echo "$last_size")
  if [ "$cur_size" -gt "$last_size" ]; then
    new_err=$(tail -c "+$((last_size + 1))" "$LOG" 2>/dev/null | grep -acE "$ERR_PAT")
    if [ "${new_err:-0}" -gt 0 ]; then
      first=$(tail -c "+$((last_size + 1))" "$LOG" 2>/dev/null |
        grep -aE "$ERR_PAT" | head -1 | cut -c1-200)
      echo "NEW ERRORS (${new_err}) at ep${ep}: ${first}"
    fi
  fi
  # If the log was rotated/truncated (cur < last), re-baseline.
  last_size=$cur_size

  # ---- era boundary ----
  era=$(era_for "$ep")
  if [ -n "$last_era" ] && [ "$era" != "$last_era" ]; then
    echo "ERA $era reached at ep${ep} block${blk} (was $last_era)"
  fi
  last_era=$era

  # ---- epoch advance -> pots byte-exactness check ----
  if [ "$ep" -gt "$last_epoch" ] && [ "$last_epoch" -ge 0 ]; then
    sleep 8   # let boundary pot update settle
    d_res=$(metric dugite_reserves_lovelace)
    d_tre=$(metric dugite_treasury_lovelace)
    read -r k_res k_tre <<<"$(koios_pots "$ep")"
    if [ -n "$d_res" ] && [ -n "$k_res" ] && [ "$k_res" != "?" ]; then
      if [ "$d_res" = "$k_res" ] && [ "$d_tre" = "$k_tre" ]; then
        pots_status="byte-exact"; pots_epoch=$ep
      else
        pots_status="DIVERGED"; pots_epoch=$ep
        dr=$(( ${d_res:-0} - ${k_res:-0} )); dt=$(( ${d_tre:-0} - ${k_tre:-0} ))
        echo "POTS DIVERGENCE ep${ep}: reserves dugite=$d_res koios=$k_res Δ=$dr | treasury dugite=$d_tre koios=$k_tre Δ=$dt"
      fi
    fi
    # Conway close-out signal for #763
    if [ "$ep" -ge 524 ] && [ "$pots_status" = "byte-exact" ]; then
      echo "POTS #763 byte-exact at ep${ep} (>=524, Conway): reserves=$d_res treasury=$d_tre match Koios"
    fi
  fi
  last_epoch=$ep
  last_block=$blk

  # ---- hourly heartbeat ----
  hb_count=$((hb_count+1))
  if [ "$hb_count" -ge 60 ]; then
    bps=$(( (blk - block_hr_start) / 3600 ))
    echo "HEARTBEAT ep${ep} $era block${blk} (~${bps} blk/s last hr) pots=${pots_status}@ep${pots_epoch}"
    hb_count=0; block_hr_start=$blk
  fi

  sleep 60
done
