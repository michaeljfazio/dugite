#!/usr/bin/env bash
# Run a 30-min soak test (default), collect evidence into evidence/<ts>/.
# Usage: soak.sh [DURATION_SECONDS]   (default 1800)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/common.sh"

DURATION="${1:-1800}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
EVD="$LD_EVIDENCE/$TS"
mkdir -p "$EVD/logs"

log_info "=== Soak starting (duration ${DURATION}s, evidence $EVD) ==="

# Verify devnet is running (sockets exist + tip queries succeed)
for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$sock" ] || die "Socket $sock not present - start the devnet first (./run.sh)"
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$sock" >/dev/null \
        || die "Tip query failed on $sock"
done

# Metadata snapshot
cat > "$EVD/metadata.json" <<EOF
{
  "timestamp": "$TS",
  "duration_seconds": $DURATION,
  "magic": $LD_MAGIC,
  "ports": { "relay": $LD_RELAY_PORT, "dugite_bp": $LD_DUGITE_BP_PORT, "cardano_bp": $LD_CARDANO_BP_PORT },
  "cardano_node_version": "$(cardano-node --version | awk 'NR==1 {print $2}')",
  "cardano_cli_version": "$(cardano-cli --version | awk 'NR==1 {print $2}')",
  "dugite_node_git": "$(cd "$LD_REPO_ROOT" && git rev-parse HEAD)",
  "genesis_hash_shelley": "$(cardano-cli hash genesis-file --genesis "$LD_GENESIS/shelley-genesis.json")",
  "genesis_hash_conway":  "$(cardano-cli hash genesis-file --genesis "$LD_GENESIS/conway-genesis.json")"
}
EOF

# Write CSV headers
echo "ts,node,slot,block_no,hash,era" > "$EVD/tip-samples.csv"
echo "ts,observer,event,slot,hash,issuer_vkey,body_size,n_txs" > "$EVD/blocks.csv"
echo "ts,target_socket,wave,txid,submit_rc" > "$EVD/tx-submissions.csv"

# Sampler PIDs collected for cleanup
SAMPLER_PIDS=()

cleanup() {
    log_info "Stopping samplers"
    for pid in "${SAMPLER_PIDS[@]}"; do
        kill -TERM "$pid" 2>/dev/null || true
    done
    sleep 1
    for pid in "${SAMPLER_PIDS[@]}"; do
        kill -0 "$pid" 2>/dev/null && kill -KILL "$pid" 2>/dev/null || true
    done
    # Snapshot logs to evidence dir
    cp "$LD_LOGS"/*.log "$EVD/logs/" 2>/dev/null || true
    log_info "Soak evidence saved to $EVD"
}
trap cleanup EXIT INT TERM

# Tasks 15-17 add: tip-sampler, block-recorder, tx-injector

END_EPOCH=$(($(date +%s) + DURATION))
log_info "Soak end at epoch $END_EPOCH ($(date -u -r $END_EPOCH 2>/dev/null || date -u -d @$END_EPOCH))"

# Main loop: print a heartbeat every 30s
while [ "$(date +%s)" -lt "$END_EPOCH" ]; do
    REMAINING=$((END_EPOCH - $(date +%s)))
    RELAY_TIP="$(query_slot "$LD_RELAY_SOCK" 2>/dev/null || echo ?)"
    DBP_TIP="$(query_slot "$LD_DUGITE_BP_SOCK" 2>/dev/null || echo ?)"
    CBP_TIP="$(query_slot "$LD_CARDANO_BP_SOCK" 2>/dev/null || echo ?)"
    log_info "[+$((DURATION-REMAINING))s / ${DURATION}s] tips: relay=$RELAY_TIP dugite-bp=$DBP_TIP cardano-bp=$CBP_TIP"
    sleep 30
done

log_info "Soak duration reached. Cleanup running."
