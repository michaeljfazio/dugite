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

# cardano-node does NOT open its N2C socket until the chain's system start has
# passed. In two-forger mode the genesis start is deliberately pushed ~150s out
# (so both producers connect before slot 0 and cannot fork at genesis past k),
# which outran the fixed 120s socket wait and made run.sh report
# "Socket /tmp/ld-.../cbp.sock did not become ready within 120s" on a node that
# was perfectly healthy and simply waiting. Extend every socket wait by however
# long remains until genesis.
SOCKET_WAIT_EXTRA=0
if [ "$SKEW" -lt 0 ]; then
    SOCKET_WAIT_EXTRA=$(( -SKEW ))
    log_info "Genesis is ${SOCKET_WAIT_EXTRA}s in the future — extending socket waits accordingly"
fi

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
    --rpc-host      127.0.0.1 \
    --rpc-port      "$LD_DUGITE_RELAY_RPC_PORT" \
    > "$LD_LOGS/dugite-relay.log" 2>&1 &
write_node_pidfile "$LD_STATE/dugite-relay.db" "$LD_STATE/dugite-relay.pid" \
    || { echo $! > "$LD_STATE/dugite-relay.pid"; log_info "WARN: could not resolve dugite-relay node pid; pidfile may be stale"; }
log_info "dugite-relay PID $(cat "$LD_STATE/dugite-relay.pid")"

# ---- cardano-bp ----
# cardano-bp runs as a non-forging relay so we never get asymmetric
# forks between the two BPs racing for the same height. cardano-node
# 11.0.1 only treats a node as a forger when --shelley-kes-key /
# --shelley-vrf-key / --shelley-operational-certificate are passed
# explicitly (the config file is ignored for these); omitting them
# means ncProtocolFiles is all Nothing and the node runs as a passive
# chainsync+blockfetch validator. This gives us byte-exact Haskell
# cross-validation of every dugite-forged block without any chain
# divergence risk. Setup.sh redirects all 20 stake-delegators to
# pool1 so pool2 has no active stake (its absence here is fine).
#
# TWO-FORGER MODE (#957): when setup.sh was run with LD_TWO_FORGERS=1 it left a
# marker and split the genesis delegation across both pools. In that mode
# cardano-bp gets pool2's forging keys, so the devnet has two competing
# producers and slot battles / rollback / chain selection become reachable.
# The mode is read from the marker, not the environment, so a run.sh invoked
# without the variable cannot silently produce a single-forger devnet that a
# two-forger round then asserts against.
CBP_FORGE_ARGS=()
if [ -f "$LD_GENESIS/.two-forgers" ]; then
    CBP_FORGE_ARGS=(
        --shelley-kes-key                 "$LD_KEYS/pool2/kes.skey"
        --shelley-vrf-key                 "$LD_KEYS/pool2/vrf.skey"
        --shelley-operational-certificate "$LD_KEYS/pool2/opcert.cert"
    )
    log_info "Starting cardano-bp on port $LD_CARDANO_BP_PORT (FORGING pool2 — two-forger mode, pool2 stake $(cat "$LD_GENESIS/.two-forgers")%)"
else
    log_info "Starting cardano-bp on port $LD_CARDANO_BP_PORT (relay / validator only)"
fi
cardano-node run \
    --config        "$LD_CONFIG/cardano-bp.config.json" \
    --topology      "$LD_CONFIG/cardano-bp.topology.json" \
    --database-path "$LD_STATE/cardano-bp.db" \
    --socket-path   "$LD_CARDANO_BP_SOCK" \
    --host-addr     127.0.0.1 \
    --port          "$LD_CARDANO_BP_PORT" \
    "${CBP_FORGE_ARGS[@]+"${CBP_FORGE_ARGS[@]}"}" \
    > "$LD_LOGS/cardano-bp.log" 2>&1 &
echo $! > "$LD_STATE/cardano-bp.pid"
log_info "cardano-bp PID $(cat "$LD_STATE/cardano-bp.pid")"

# ---- cardano-arbiter (two-forger mode only) ----
#
# A second, NON-forging cardano-node peered directly with both producers. It is
# the independent Haskell oracle for "which block won this slot battle".
#
# Without it the round could only compare dugite-bp against dugite-relay, and
# two dugite processes agreeing tells us nothing about whether dugite's Praos
# tiebreaker matches cardano-node's. cardano-bp cannot play the arbiter in this
# mode either — it is one of the two contestants, and a producer prefers its
# own block by construction, so its view is not neutral.
if [ -f "$LD_GENESIS/.two-forgers" ]; then
    log_info "Starting cardano-arbiter on port $LD_CARDANO_ARBITER_PORT (Haskell tiebreak oracle, non-forging)"
    cardano-node run \
        --config        "$LD_CONFIG/cardano-arbiter.config.json" \
        --topology      "$LD_CONFIG/cardano-arbiter.topology.json" \
        --database-path "$LD_STATE/cardano-arbiter.db" \
        --socket-path   "$LD_CARDANO_ARBITER_SOCK" \
        --host-addr     127.0.0.1 \
        --port          "$LD_CARDANO_ARBITER_PORT" \
        > "$LD_LOGS/cardano-arbiter.log" 2>&1 &
    echo $! > "$LD_STATE/cardano-arbiter.pid"
    log_info "cardano-arbiter PID $(cat "$LD_STATE/cardano-arbiter.pid")"
fi

# Wait for relay + cardano-bp sockets first.
wait_for_socket "$LD_RELAY_SOCK"      $(( 120 + SOCKET_WAIT_EXTRA ))
wait_for_socket "$LD_CARDANO_BP_SOCK" $(( 120 + SOCKET_WAIT_EXTRA ))
[ -f "$LD_GENESIS/.two-forgers" ] && wait_for_socket "$LD_CARDANO_ARBITER_SOCK" $(( 180 + SOCKET_WAIT_EXTRA ))

# The original "Bug-A workaround" stagger that waited for cardano-bp to
# push a block into the relay is no longer needed: cardano-bp now runs
# as a validator (no forging keys), so dugite-bp is the sole producer.
# Both relay and dugite-bp start at origin and the relay advances from
# dugite-bp's own forges — the stale-intersection class can't fire
# because dugite-bp's own chain is always at the producing edge.

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
    --rpc-host      127.0.0.1 \
    --rpc-port      "$LD_DUGITE_BP_RPC_PORT" \
    --shelley-kes-key                 "$LD_KEYS/pool1/kes.skey" \
    --shelley-vrf-key                 "$LD_KEYS/pool1/vrf.skey" \
    --shelley-operational-certificate "$LD_KEYS/pool1/opcert.cert" \
    > "$LD_LOGS/dugite-bp.log" 2>&1 &
write_node_pidfile "$LD_STATE/dugite-bp.db" "$LD_STATE/dugite-bp.pid" \
    || { echo $! > "$LD_STATE/dugite-bp.pid"; log_info "WARN: could not resolve dugite-bp node pid; pidfile may be stale"; }
log_info "dugite-bp PID $(cat "$LD_STATE/dugite-bp.pid")"

wait_for_socket "$LD_DUGITE_BP_SOCK"  $(( 120 + SOCKET_WAIT_EXTRA ))

# gRPC listeners (#960). Non-fatal: a node that failed to bind its RPC port is
# still a usable devnet for every N2C-based suite, and failing the whole run
# here would make an RPC regression look like a total harness outage. The rpc/
# suite asserts reachability itself and reports a real failure if it is down.
for _rp in "$LD_DUGITE_BP_RPC_PORT" "$LD_DUGITE_RELAY_RPC_PORT"; do
    if wait_for_tcp_port "$_rp" 60; then
        log_info "UTxO RPC listening on 127.0.0.1:$_rp"
    else
        log_warn "UTxO RPC port $_rp never came up — rpc/ suite will report it"
    fi
done

log_info "All three sockets ready."
log_info "Query tips:"
for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
    name="$(basename "$sock" .sock)"
    tip_line="$(query_tip_oneline "$sock")"
    printf '  %-16s %s\n' "$name" "$tip_line"
done

log_info "Devnet running. Logs: $LD_LOGS/. Stop with ./stop.sh."
