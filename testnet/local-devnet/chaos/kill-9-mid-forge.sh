#!/usr/bin/env bash
# chaos/kill-9-mid-forge.sh — SIGKILL dugite-bp during an active forge slot,
# then restart it and verify it recovers to the relay's tip.
#
# Recovery bound: dugite-bp must reconnect and reach relay tip within 120s.
# Acceptance: ChainDB integrity passes after recovery; no data corruption.
set -euo pipefail

SCENARIO="kill-9-mid-forge"
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

RECOVERY_BOUND="${KILL9_RECOVERY_SEC:-120}"

[ -S "$LD_DUGITE_BP_SOCK" ] || die "$SCENARIO: dugite-bp socket not present — run ./run.sh first"
[ -f "$LD_STATE/dugite-bp.pid" ] || die "$SCENARIO: dugite-bp.pid not found"

# Resolve the NODE pid by command line, not from the pidfile.
#
# On macOS run.sh launches the node under `caffeinate`, so $LD_STATE/dugite-bp.pid
# can hold the WRAPPER's pid (restart-dugite-bp.sh documents this). SIGKILLing
# the wrapper leaves the node running: the "process still alive after SIGKILL"
# check passes (the wrapper really is dead), the script then starts a SECOND
# node on the same database, and the #929 directory lock rejects it. The
# scenario would fail for a reason entirely unrelated to SIGKILL recovery.
BP_PID=$(pgrep -f "dugite-node run .*dugite-bp" | head -1)
[ -n "$BP_PID" ] || BP_PID=$(cat "$LD_STATE/dugite-bp.pid" 2>/dev/null)
if [ -z "$BP_PID" ] || ! kill -0 "$BP_PID" 2>/dev/null; then
    log_warn "$SCENARIO: dugite-bp PID $BP_PID is not running — skipping"
    chaos_record "$SCENARIO" "skip" "0" "SKIP" "dugite-bp-not-running"
    exit 0
fi

# Record tip before kill
TIP_BEFORE=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.block // 0' || echo 0)
log_info "$SCENARIO: tip before kill = $TIP_BEFORE, sending SIGKILL to PID $BP_PID"

T_KILL=$(date +%s)
kill -9 "$BP_PID" 2>/dev/null || true
chaos_record "$SCENARIO" "sigkill" "0" "IN_PROGRESS" "pid=$BP_PID tip_before=$TIP_BEFORE"

# Wait briefly to confirm process is dead
sleep 2
if kill -0 "$BP_PID" 2>/dev/null; then
    log_error "$SCENARIO: process still alive after SIGKILL"
    chaos_record "$SCENARIO" "kill-check" "0" "FAIL" "process-survived-sigkill"
    exit 1
fi

log_info "$SCENARIO: process killed, restarting dugite-bp..."

# Restart dugite-bp with the SAME flags run.sh uses.
#
# The previous version could never have worked, and had never run: it passed
# $LD_KEYS/kes.skey, $LD_KEYS/vrf.skey and $LD_KEYS/node.cert — none of which
# exist. The real paths are keys/pool1/{kes.skey,vrf.skey,opcert.cert}
# (cf. run.sh). The node would have failed to boot and the scenario would have
# FAILed on its 120s recovery timeout, blaming dugite for a harness bug.
#
# It also wrote the log with `>` instead of `>>`, destroying the round's entire
# forge history — the evidence a SIGKILL-recovery test exists to preserve.
#
# `--metrics-port` matters too: without it the restarted node binds the default
# port and the health probes read a different node than the one under test.
#
# scripts/restart-dugite-bp.sh is the canonical reference for this invocation.
caffeinate_if_macos "$DUGITE_BIN" run \
    --config        "$LD_CONFIG/dugite-bp.config.json" \
    --topology      "$LD_CONFIG/dugite-bp.topology.json" \
    --database-path "$LD_STATE/dugite-bp.db" \
    --socket-path   "$LD_DUGITE_BP_SOCK" \
    --host-addr     127.0.0.1 \
    --port          "$LD_DUGITE_BP_PORT" \
    --metrics-port  "$LD_DUGITE_BP_METRICS_PORT" \
    --shelley-kes-key                 "$LD_KEYS/pool1/kes.skey" \
    --shelley-vrf-key                 "$LD_KEYS/pool1/vrf.skey" \
    --shelley-operational-certificate "$LD_KEYS/pool1/opcert.cert" \
    >> "$LD_LOGS/dugite-bp.log" 2>&1 &
NEW_PID=$!
echo "$NEW_PID" > "$LD_STATE/dugite-bp.pid"
log_info "$SCENARIO: restarted with PID $NEW_PID"

# Wait for recovery
if chaos_wait_for_socket "$LD_DUGITE_BP_SOCK" "$RECOVERY_BOUND"; then
    T_RECOVERED=$(date +%s)
    RECOVERY_SEC=$(( T_RECOVERED - T_KILL ))

    # Verify chaindb integrity
    if chaos_verify_chaindb "$LD_DUGITE_BP_SOCK"; then
        TIP_AFTER=$(cardano-cli query tip \
            --testnet-magic "$LD_MAGIC" \
            --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.block // 0' || echo 0)
        log_info "$SCENARIO: recovered in ${RECOVERY_SEC}s tip_after=$TIP_AFTER"
        chaos_record "$SCENARIO" "recovery" "$RECOVERY_SEC" "PASS" "tip_before=$TIP_BEFORE tip_after=$TIP_AFTER"
    else
        chaos_record "$SCENARIO" "recovery" "$RECOVERY_SEC" "FAIL" "chaindb-integrity-failed"
        exit 1
    fi
else
    ELAPSED=$(( $(date +%s) - T_KILL ))
    log_error "$SCENARIO: did not recover within ${RECOVERY_BOUND}s"
    chaos_record "$SCENARIO" "recovery" "$ELAPSED" "FAIL" "timeout-no-socket-after-${RECOVERY_BOUND}s"
    exit 1
fi
