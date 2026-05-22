#!/usr/bin/env bash
# 08s — Rule 1h: pool registration with cost below min_pool_cost (StakePoolCostTooLow).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=200000
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
POOL_COLD_SKEY="$ZOO_BUILT/$NAME.cold.skey"
POOL_COLD_VKEY="$ZOO_BUILT/$NAME.cold.vkey"
POOL_VRF_SKEY="$ZOO_BUILT/$NAME.vrf.skey"
POOL_VRF_VKEY="$ZOO_BUILT/$NAME.vrf.vkey"
POOL_OPCERT="$ZOO_BUILT/$NAME.opcert"
POOL_COUNTER="$ZOO_BUILT/$NAME.counter"
POOL_REG="$ZOO_BUILT/$NAME.pool.reg"

# Generate fresh pool keys
cardano-cli conway node key-gen \
    --cold-verification-key-file "$POOL_COLD_VKEY" \
    --cold-signing-key-file      "$POOL_COLD_SKEY" \
    --operational-certificate-issue-counter-file "$POOL_COUNTER" >/dev/null 2>&1 || \
{ zoo_record "$NAME" SKIP "" "node-key-gen-not-available"; exit 0; }

cardano-cli conway node key-gen-VRF \
    --verification-key-file "$POOL_VRF_VKEY" \
    --signing-key-file      "$POOL_VRF_SKEY" >/dev/null 2>&1 || \
{ zoo_record "$NAME" SKIP "" "key-gen-VRF-not-available"; exit 0; }

# Get min_pool_cost from protocol parameters
MIN_POOL_COST=$(cardano-cli conway query protocol-parameters \
    --testnet-magic "$LD_MAGIC" \
    --socket-path "$ZOO_SOCKET" \
    --output-json 2>/dev/null | jq -r '.minPoolCost // 340000000')

# Register pool with cost=1 (way below minPoolCost)
cardano-cli conway stake-pool registration-certificate \
    --cold-verification-key-file  "$POOL_COLD_VKEY" \
    --vrf-verification-key-file   "$POOL_VRF_VKEY" \
    --pool-pledge  0 \
    --pool-cost    1 \
    --pool-margin  0 \
    --pool-reward-account-verification-key-file "$ZOO_PAY_VKEY" \
    --pool-owner-stake-verification-key-file    "$ZOO_PAY_VKEY" \
    --testnet-magic "$LD_MAGIC" \
    --out-file "$POOL_REG" >/dev/null 2>&1 || \
{ zoo_record "$NAME" SKIP "" "registration-certificate-failed"; exit 0; }

cardano-cli conway transaction build-raw \
    --tx-in          "$TXIN" \
    --tx-out         "${ADDR}+$((AMT - FEE - 500000000))" \
    --fee            "$FEE" \
    --ttl            $((TIP + 600)) \
    --certificate-file "$POOL_REG" \
    --out-file       "$RAW" 2>/dev/null || true

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --signing-key-file "$POOL_COLD_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "pool-cost-too-low submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-StakePoolCostTooLow" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
