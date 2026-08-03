#!/usr/bin/env bash
# 09s — query stake-pool-default-vote
# Returns the default vote an SPO would cast (abstain or no-confidence).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

POOL_ID_FILE="$LD_KEYS/pool1/pool.id"
if [ ! -f "$POOL_ID_FILE" ]; then
    parity_record "stake-pool-default-vote" "SKIP" "skip" "skip" "env-skip: pool1 id not found"
    exit 0
fi
# cardano-cli 11.0.0.0 takes the SPO by key hash / verification key here, not by
# bech32 pool id: the option group is
#   ( --spo-verification-key STRING | --spo-verification-key-file FILEPATH | --spo-key-hash HASH )
# `--stake-pool-id` does not exist on this subcommand and made BOTH sides fail
# identically ("Invalid option `--stake-pool-id'"), which is a harness bug, never
# a dugite gap (see the "Reading the cli-parity suite" note in CLAUDE.md). It was
# invisible until #953 fixed the missing keys/pool1/pool.id that had been
# short-circuiting this script to SKIP on every run.
POOL_ID_HEX=$(cat "$LD_KEYS/pool1/pool.id.hex")
parity_query_json "stake-pool-default-vote" "stake-pool-default-vote" \
    "--spo-key-hash" "$POOL_ID_HEX"
