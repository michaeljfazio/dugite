#!/usr/bin/env bash
# Relaunch ONLY dugite-bp with the same flags as testnet/local-devnet/run.sh,
# leaving dugite-relay and cardano-bp running. Used by Round 3 (restart
# resilience).
#
# Assumes:
# - testnet/local-devnet/setup.sh has been run
# - testnet/local-devnet/run.sh started the network earlier in the session
# - dugite-bp has already been killed (its PID file is stale)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
LD="$REPO_ROOT/testnet/local-devnet"

. "$LD/lib/common.sh"

# Pre-flight
if ! [ -S "$LD_RELAY_SOCK" ]; then
    die "dugite-relay socket missing — devnet not running. Run ./run.sh first."
fi
if ! [ -S "$LD_CARDANO_BP_SOCK" ]; then
    die "cardano-bp socket missing — devnet not running. Run ./run.sh first."
fi
if [ -S "$LD_DUGITE_BP_SOCK" ]; then
    die "dugite-bp socket still present — process is alive. Kill it first."
fi

rm -f "$LD_DUGITE_BP_SOCK" "$LD_STATE/dugite-bp.pid"

log_info "Relaunching dugite-bp (port $LD_DUGITE_BP_PORT, metrics $LD_DUGITE_BP_METRICS_PORT)"
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

echo $! > "$LD_STATE/dugite-bp.pid"
log_info "dugite-bp PID $(cat "$LD_STATE/dugite-bp.pid")"

wait_for_socket "$LD_DUGITE_BP_SOCK" 60
log_info "dugite-bp socket up."

# Print tips of all three so the caller can assert catch-up
log_info "Current tips:"
for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
    name="$(basename "$sock" .sock)"
    printf '  %-16s %s\n' "$name" "$(query_tip_oneline "$sock")"
done
