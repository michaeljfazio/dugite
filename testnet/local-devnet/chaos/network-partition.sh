#!/usr/bin/env bash
# chaos/network-partition.sh — drop traffic between dugite-bp and the relay
# for PARTITION_SEC seconds, then restore, and verify reconnection.
#
# Uses pfctl on macOS, iptables on Linux. Skips with a warning if neither
# is available (e.g. rootless CI).
#
# Recovery bound: dugite-bp must reconnect within 60s of partition end.
set -euo pipefail

SCENARIO="network-partition"
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

PARTITION_SEC="${PARTITION_SEC:-60}"
RECOVERY_BOUND="${PARTITION_RECOVERY_SEC:-60}"

chaos_require_net_tool || { chaos_record "$SCENARIO" "skip" "0" "SKIP" "no-net-tool-$CHAOS_NET_TOOL"; exit 0; }
[ -S "$LD_DUGITE_BP_SOCK" ] || die "$SCENARIO: dugite-bp socket not present"

TIP_BEFORE=$(cardano-cli query tip \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.block // 0' || echo 0)

log_info "$SCENARIO: blocking port $LD_RELAY_PORT for ${PARTITION_SEC}s (tip_before=$TIP_BEFORE)"

# Install block rule
case "$CHAOS_NET_TOOL" in
    pfctl)
        # macOS: add a quick block rule for the relay port
        PF_ANCHOR="dugite-chaos"
        printf 'block quick proto tcp from any to any port %d\n' "$LD_RELAY_PORT" \
            | sudo pfctl -a "$PF_ANCHOR" -f - 2>/dev/null
        sudo pfctl -e 2>/dev/null || true
        ;;
    iptables)
        sudo iptables -A OUTPUT -p tcp --dport "$LD_RELAY_PORT" -j DROP
        sudo iptables -A INPUT  -p tcp --sport "$LD_RELAY_PORT" -j DROP
        ;;
esac

chaos_record "$SCENARIO" "partition-start" "0" "IN_PROGRESS" "port=$LD_RELAY_PORT duration=${PARTITION_SEC}s"
T_PART=$(date +%s)
sleep "$PARTITION_SEC"

# Remove block rule
case "$CHAOS_NET_TOOL" in
    pfctl)
        sudo pfctl -a "dugite-chaos" -F rules 2>/dev/null || true
        ;;
    iptables)
        sudo iptables -D OUTPUT -p tcp --dport "$LD_RELAY_PORT" -j DROP 2>/dev/null || true
        sudo iptables -D INPUT  -p tcp --sport "$LD_RELAY_PORT" -j DROP 2>/dev/null || true
        ;;
esac

log_info "$SCENARIO: partition lifted, waiting for reconnect..."
chaos_record "$SCENARIO" "partition-end" "$PARTITION_SEC" "IN_PROGRESS" "waiting-reconnect"

if chaos_wait_for_socket "$LD_DUGITE_BP_SOCK" "$RECOVERY_BOUND"; then
    T_RECOVERED=$(date +%s)
    RECOVERY_SEC=$(( T_RECOVERED - T_PART - PARTITION_SEC ))
    TIP_AFTER=$(cardano-cli query tip \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.block // 0' || echo 0)

    if chaos_verify_chaindb "$LD_DUGITE_BP_SOCK"; then
        chaos_record "$SCENARIO" "recovery" "$RECOVERY_SEC" "PASS" "tip_before=$TIP_BEFORE tip_after=$TIP_AFTER"
        log_info "$SCENARIO: PASS — recovered in ${RECOVERY_SEC}s"
    else
        chaos_record "$SCENARIO" "recovery" "$RECOVERY_SEC" "FAIL" "chaindb-integrity-failed"
        exit 1
    fi
else
    ELAPSED=$(( $(date +%s) - T_PART - PARTITION_SEC ))
    chaos_record "$SCENARIO" "recovery" "$ELAPSED" "FAIL" "no-reconnect-within-${RECOVERY_BOUND}s"
    exit 1
fi
