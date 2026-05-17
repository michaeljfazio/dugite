#!/usr/bin/env bash
# Launch dugite-relay, dugite-bp, cardano-bp.
# Logs to logs/<node>.log; PIDs to state/<node>.pid.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/common.sh"

log_info "=== Local devnet run ==="

# Genesis-time freshness check — refuse to start if >5 min has passed since setup
GENESIS_START="$(jq -r .systemStart "$LD_GENESIS/shelley-genesis.json")"
GENESIS_EPOCH=$(date -u -j -f '%Y-%m-%dT%H:%M:%SZ' "$GENESIS_START" '+%s' 2>/dev/null \
    || date -u -d "$GENESIS_START" '+%s')
NOW_EPOCH=$(date -u '+%s')
SKEW=$((NOW_EPOCH - GENESIS_EPOCH))
if [ "$SKEW" -gt 300 ]; then
    die "Genesis is $SKEW seconds old (>300s). Re-run ./setup.sh to regenerate with a fresh start time."
fi
if [ "$SKEW" -lt -300 ]; then
    die "Genesis is $((-SKEW)) seconds in the future (>300s). Check clock skew."
fi
log_info "Genesis start ${SKEW}s from now — OK"

assert_ports_free
mkdir -p "$LD_STATE" "$LD_LOGS"
rm -f "$LD_STATE"/*.pid "$LD_STATE"/*.sock

# ---- Staggered launch ----
#
# Bug-A workaround (2026-05-16): dugite-node has a known stale-intersection
# bug where if its ChainSync upstream peer is at "origin" when the connection
# is established, dugite never re-intersects after the peer's chain advances
# (memory: project_stale_intersection_when_peer_behind.md, 2026-05-15).
#
# In a hub-and-spoke devnet, if all three nodes start simultaneously, dugite-bp
# connects to dugite-relay before the relay has received any blocks from
# cardano-bp — so the intersection lands at origin and dugite-bp never adopts
# cardano-bp's blocks for the rest of the run.
#
# Workaround: start relay + cardano-bp first, wait until the relay's chain
# has advanced past slot 0 (i.e., it has received at least one block from
# cardano-bp), and only then start dugite-bp. By the time dugite-bp opens
# its ChainSync to the relay, the relay reports a non-origin tip and
# intersection lands at a real point.

# ---- dugite-relay ----
# Both dugite processes export Prometheus metrics. The BP uses the default port
# 12798 (so dugite-monitor's default endpoint works without overrides); the
# relay gets 12799 to avoid the listener collision.
log_info "Starting dugite-relay on port $LD_RELAY_PORT (metrics $LD_DUGITE_RELAY_METRICS_PORT)"
caffeinate_if_macos "$DUGITE_BIN" run \
    --config        "$LD_CONFIG/dugite-relay.config.json" \
    --topology      "$LD_CONFIG/dugite-relay.topology.json" \
    --database-path "$LD_STATE/dugite-relay.db" \
    --socket-path   "$LD_RELAY_SOCK" \
    --host-addr     127.0.0.1 \
    --port          "$LD_RELAY_PORT" \
    --metrics-port  "$LD_DUGITE_RELAY_METRICS_PORT" \
    > "$LD_LOGS/dugite-relay.log" 2>&1 &
echo $! > "$LD_STATE/dugite-relay.pid"
log_info "dugite-relay PID $(cat "$LD_STATE/dugite-relay.pid")"

# ---- cardano-bp ----
# cardano-node 11.0.1 only reads pool key paths from CLI flags, not from
# ShelleyKesKeyFile / ShelleyVrfKeyFile / ShelleyOperationalCertificateFile
# in the config JSON. Without these flags the node silently runs as a
# non-producing relay (ncProtocolFiles all Nothing) and the chain never
# advances. Pass them explicitly.
log_info "Starting cardano-bp on port $LD_CARDANO_BP_PORT (pool2)"
cardano-node run \
    --config        "$LD_CONFIG/cardano-bp.config.json" \
    --topology      "$LD_CONFIG/cardano-bp.topology.json" \
    --database-path "$LD_STATE/cardano-bp.db" \
    --socket-path   "$LD_CARDANO_BP_SOCK" \
    --host-addr     127.0.0.1 \
    --port          "$LD_CARDANO_BP_PORT" \
    --shelley-kes-key                 "$LD_KEYS/pool2/kes.skey" \
    --shelley-vrf-key                 "$LD_KEYS/pool2/vrf.skey" \
    --shelley-operational-certificate "$LD_KEYS/pool2/opcert.cert" \
    > "$LD_LOGS/cardano-bp.log" 2>&1 &
echo $! > "$LD_STATE/cardano-bp.pid"
log_info "cardano-bp PID $(cat "$LD_STATE/cardano-bp.pid")"

# Wait for relay + cardano-bp sockets first.
wait_for_socket "$LD_RELAY_SOCK"      120
wait_for_socket "$LD_CARDANO_BP_SOCK" 120

# Wait for the relay's chain to advance past slot 0. cardano-bp forges blocks
# and pushes them to the relay over N2N; once the relay's tip slot > 0, we
# know the relay has accepted at least one cardano-bp block and is no longer
# at origin.
log_info "Waiting for dugite-relay chain to advance past slot 0 (Bug-A workaround)"
STAGGER_TIMEOUT=120
i=0
while [ $i -lt "$STAGGER_TIMEOUT" ]; do
    relay_slot="$(query_slot "$LD_RELAY_SOCK" 2>/dev/null || echo 0)"
    # Treat non-numeric as 0
    case "$relay_slot" in
        ''|*[!0-9]*) relay_slot=0 ;;
    esac
    if [ "$relay_slot" -gt 0 ]; then
        log_info "dugite-relay tip slot=$relay_slot after ${i}s — proceeding to start dugite-bp"
        break
    fi
    sleep 1
    i=$((i + 1))
done
if [ "$i" -ge "$STAGGER_TIMEOUT" ]; then
    die "dugite-relay did not see any cardano-bp block within ${STAGGER_TIMEOUT}s — aborting"
fi

# ---- dugite-bp ----
# Metrics on default port 12798 so `dugite-monitor` works without overrides.
log_info "Starting dugite-bp on port $LD_DUGITE_BP_PORT (pool1, metrics $LD_DUGITE_BP_METRICS_PORT)"
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
    > "$LD_LOGS/dugite-bp.log" 2>&1 &
echo $! > "$LD_STATE/dugite-bp.pid"
log_info "dugite-bp PID $(cat "$LD_STATE/dugite-bp.pid")"

wait_for_socket "$LD_DUGITE_BP_SOCK"  120

log_info "All three sockets ready."
log_info "Query tips:"
for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
    name="$(basename "$sock" .sock)"
    tip_line="$(query_tip_oneline "$sock")"
    printf '  %-16s %s\n' "$name" "$tip_line"
done

log_info "Devnet running. Logs: $LD_LOGS/. Stop with ./stop.sh."
