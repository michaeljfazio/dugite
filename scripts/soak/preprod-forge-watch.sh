#!/usr/bin/env bash
# Poll-loop watcher for the preprod forge soak. Emits a concise heartbeat every
# ~15 min and exits immediately when the dugite BP forges (or falls off tip /
# crashes), so the orchestrator is notified at the decisive moment.
LEADER=${1:-127750519}
LOG=logs/bp-pair-preprod/bp.current.log
BP_PID=$(cat logs/bp-pair-preprod/bp.pid 2>/dev/null)
iter=0
while true; do
  iter=$((iter+1))
  slot=$(curl -s http://localhost:12799/metrics 2>/dev/null | grep '^dugite_slot_number' | awk '{print $2}')
  net=$(grep -oE 'at slot [0-9]+' logs/bp-pair-preprod/relay.current.log 2>/dev/null | tail -1 | awk '{print $3}')
  # Only a forge AT this leader slot counts (slot=$LEADER also matches current_slot=$LEADER).
  forge=$(grep -hE "TraceNodeIsLeader|TraceForgedBlock|TraceAdoptedBlock|TraceForgedInvalid|TraceDidntAdopt|TraceNodeCannotForge" "$LOG" 2>/dev/null | grep "slot=$LEADER" | tail -1)
  # ValidateAll withdrawal rejection (tip-following failure) uses the "...: withdrawal N !=" format
  err=$(grep -hE "WithdrawalAmountMismatch: withdrawal|Block does not connect|thread '.*' panicked|FATAL" "$LOG" 2>/dev/null | tail -1)
  behind=$(( ${net:-0} - ${slot:-0} ))
  to_leader=$(( LEADER - ${slot:-0} ))
  hrs=$(awk "BEGIN{printf \"%.1f\", $to_leader/3600}")
  alive="up"; { [ -n "$BP_PID" ] && kill -0 "$BP_PID" 2>/dev/null; } || alive="DEAD"
  # Decisive exits (checked every 10-min poll)
  if echo "$forge" | grep -q "TraceForgedBlock\|TraceAdoptedBlock\|TraceForgedInvalid"; then echo "EXIT: FORGE EVENT -> $forge"; break; fi
  if [ "$alive" = "DEAD" ]; then echo "EXIT: BP PROCESS DIED"; break; fi
  if [ -n "$err" ]; then echo "EXIT: TIP-FOLLOWING/CRITICAL ERROR -> $err"; break; fi
  if [ "${slot:-0}" -gt "$((LEADER+240))" ]; then echo "EXIT: LEADER SLOT PASSED (check forge outcome)"; break; fi
  # Emit a heartbeat notification only every 3rd poll (~30 min) to avoid noise
  if [ $((iter % 3)) -eq 1 ]; then
    echo "HEARTBEAT slot=${slot:-?} net=${net:-?} behind=${behind} to_leader=${to_leader}(~${hrs}h) proc=${alive}"
  fi
  sleep 600
done
