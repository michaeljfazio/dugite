#!/usr/bin/env bash
# Genesis-consensus-mode from-genesis preview resync soak.
# Validates the deferred Phase-2 pooled-flush fixes under Ouroboros Genesis (LoE/GDD):
#   599bf69d5a  bound + cancel-arm the pooled flush (memory/CPU runaway fix)
#   f1372ffd7c  flush deferred Phase-2 before fork-switch ledger rollback (genesis-mode safety)
#   d43e784371  deferred_phase2 flush counters (soak validity telemetry)
#
# DB path is SEPARATE from ./db-preview (the 15G apply_bench ep1333 fixture — do NOT touch).
set -euo pipefail
cd "$(dirname "$0")/../.."

TS="$(cat reports/.genesis-soak-ts 2>/dev/null || date -u +%Y%m%dT%H%M%SZ)"
LOG="reports/genesis-preview-resync-${TS}.log"

export DUGITE_DEFER_PHASE2_WINDOW=64        # REQUIRED: enable cross-block Phase-2 deferral
# Genesis from-origin now works WITHOUT any bypass flag: the GSM satisfies the
# Honest-Availability-Assumption via the connected bootstrap relay (Haskell-faithful
# UseBootstrapPeers HAA, commit d0dd252e08) and transitions PreSyncing->Syncing.
# DUGITE_DEFER_PHASE2_MAX_ITEMS=256 and DUGITE_PHASE2_POOL_THREADS=6 are the resolved
# defaults on this 12-core box; left unset.
export RUST_LOG=${RUST_LOG:-info}

echo "[genesis-soak] launching; log -> $LOG"
exec caffeinate -dimsu ./target/release/dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview-genesis \
  --socket-path ./node-genesis.sock \
  --host-addr 0.0.0.0 --port 3001 \
  --metrics-port 12796 \
  --consensus-mode genesis \
  2>&1 | tee "$LOG"
