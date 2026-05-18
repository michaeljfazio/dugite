#!/usr/bin/env bash
# 04e — register a third pool (pool3) using newly generated cold/VRF keys.
# Uses wallet-a as the reward + owner-stake holder.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
POOL="$ZOO_KEYS/pool3"
[ -s "$POOL/cold.vkey" ] || die "pool3 cold key missing — run setup"
[ -s "$POOL/vrf.vkey" ]  || die "pool3 vrf key missing — run setup"
[ -s "$WA/stake.vkey" ]  || die "wallet-a stake key missing"

ADDR=$(cat "$WA/payment-stake.addr")
PPARAMS=$(zoo_pparams_file)
POOL_DEPOSIT=$(jq -r '.stakePoolDeposit' "$PPARAMS")
MIN_POOL_COST=$(jq -r '.minPoolCost' "$PPARAMS")

REG_CERT="$ZOO_BUILT/$NAME.reg.cert"
DELEG_CERT="$ZOO_BUILT/$NAME.deleg.cert"
cardano-cli conway stake-pool registration-certificate \
    --cold-verification-key-file "$POOL/cold.vkey" \
    --vrf-verification-key-file  "$POOL/vrf.vkey" \
    --pool-pledge   1000000 \
    --pool-cost     "$MIN_POOL_COST" \
    --pool-margin   0.05 \
    --pool-reward-account-verification-key-file "$WA/stake.vkey" \
    --pool-owner-stake-verification-key-file    "$WA/stake.vkey" \
    --single-host-pool-relay 127.0.0.1 --pool-relay-port 3099 \
    --metadata-url   "$(zoo_anchor_url pool3)" \
    --metadata-hash  "$(zoo_anchor_hash pool3)" \
    --testnet-magic  "$LD_MAGIC" \
    --out-file       "$REG_CERT"
cardano-cli conway stake-address stake-delegation-certificate \
    --stake-verification-key-file "$WA/stake.vkey" \
    --cold-verification-key-file  "$POOL/cold.vkey" \
    --out-file "$DELEG_CERT"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --change-address "$ADDR" \
    --certificate-file "$REG_CERT" \
    --certificate-file "$DELEG_CERT" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$WA/stake.skey" \
    --signing-key-file "$POOL/cold.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "deposit=$POOL_DEPOSIT" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
