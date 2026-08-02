#!/usr/bin/env bash
# 09t — query leadership-schedule
# Returns the leader schedule for the CURRENT epoch for pool1.
# Both nodes share the same genesis + VRF keys, so the schedule should match.
# This query requires the VRF signing key — skip gracefully if not available.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

VRF_SKEY="$LD_KEYS/pool1/vrf.skey"
POOL_ID_FILE="$LD_KEYS/pool1/pool.id"

if [ ! -f "$VRF_SKEY" ] || [ ! -f "$POOL_ID_FILE" ]; then
    parity_record "leadership-schedule" "SKIP" "skip" "skip" "env-skip: VRF key or pool id not found"
    exit 0
fi

# cardano-cli 11.0.0.0 spells the epoch selector `--current` / `--next`, not
# `--current-epoch`, and requires `--genesis`. The wrong flag made BOTH sides
# fail identically ("Invalid option `--current-epoch'") — a harness bug, never a
# dugite gap. Hidden until #953 fixed the missing keys/pool1/pool.id that had
# been short-circuiting this script to SKIP on every run.
POOL_ID=$(cat "$POOL_ID_FILE")
parity_query_json "leadership-schedule" "leadership-schedule" \
    "--genesis" "$LD_GENESIS/shelley-genesis.json" \
    "--stake-pool-id" "$POOL_ID" \
    "--vrf-signing-key-file" "$VRF_SKEY" \
    "--current"
