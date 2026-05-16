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

# ---- dugite-relay ----
# Note: --no-metrics on both dugite processes — otherwise they both try to bind
# the default Prometheus port (12798) and the second one fails to start.
log_info "Starting dugite-relay on port $LD_RELAY_PORT"
caffeinate_if_macos "$DUGITE_BIN" run \
    --config        "$LD_CONFIG/dugite-relay.config.json" \
    --topology      "$LD_CONFIG/dugite-relay.topology.json" \
    --database-path "$LD_STATE/dugite-relay.db" \
    --socket-path   "$LD_RELAY_SOCK" \
    --host-addr     127.0.0.1 \
    --port          "$LD_RELAY_PORT" \
    --no-metrics \
    > "$LD_LOGS/dugite-relay.log" 2>&1 &
echo $! > "$LD_STATE/dugite-relay.pid"
log_info "dugite-relay PID $(cat "$LD_STATE/dugite-relay.pid")"

# ---- dugite-bp ----
log_info "Starting dugite-bp on port $LD_DUGITE_BP_PORT (pool1)"
caffeinate_if_macos "$DUGITE_BIN" run \
    --config        "$LD_CONFIG/dugite-bp.config.json" \
    --topology      "$LD_CONFIG/dugite-bp.topology.json" \
    --database-path "$LD_STATE/dugite-bp.db" \
    --socket-path   "$LD_DUGITE_BP_SOCK" \
    --host-addr     127.0.0.1 \
    --port          "$LD_DUGITE_BP_PORT" \
    --no-metrics \
    --shelley-kes-key                 "$LD_KEYS/pool1/kes.skey" \
    --shelley-vrf-key                 "$LD_KEYS/pool1/vrf.skey" \
    --shelley-operational-certificate "$LD_KEYS/pool1/opcert.cert" \
    > "$LD_LOGS/dugite-bp.log" 2>&1 &
echo $! > "$LD_STATE/dugite-bp.pid"
log_info "dugite-bp PID $(cat "$LD_STATE/dugite-bp.pid")"

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

# ---- Wait for sockets ----
wait_for_socket "$LD_RELAY_SOCK"      120
wait_for_socket "$LD_DUGITE_BP_SOCK"  120
wait_for_socket "$LD_CARDANO_BP_SOCK" 120

log_info "All three sockets ready."
log_info "Query tips:"
for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
    name="$(basename "$sock" .sock)"
    tip_line="$(query_tip_oneline "$sock")"
    printf '  %-16s %s\n' "$name" "$tip_line"
done

log_info "Devnet running. Logs: $LD_LOGS/. Stop with ./stop.sh."
