#!/usr/bin/env bash
# 04c — Conway-era combined registration + stake delegation + vote delegation
# in a single certificate. Targets wallet-b (so it's distinct from 04a/b).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WB="$ZOO_KEYS/wallet-b"
ADDR=$(cat "$WB/payment-stake.addr")
STAKE_ADDR=$(cat "$WB/stake.addr")
[ -s "$LD_KEYS/pool2/cold.vkey" ] || die "pool2 cold key missing"
POOL_ID=$(cardano-cli conway stake-pool id --cold-verification-key-file "$LD_KEYS/pool2/cold.vkey")

# Skip if already registered.
REG=$(cardano-cli conway query stake-address-info \
        --testnet-magic "$LD_MAGIC" \
        --socket-path "$ZOO_SOCKET" \
        --address "$STAKE_ADDR" 2>/dev/null \
        | jq -r 'if length>0 then "yes" else "no" end')
if [ "$REG" = "yes" ]; then
    zoo_skip "$STAKE_ADDR already registered"
    zoo_record "$NAME" SKIP "" "already-registered"
    exit 0
fi

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")

# Conway "stake-and-vote-registration-delegation-certificate" combines
# register + delegate-stake + delegate-vote in one cert.
CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address registration-stake-and-vote-delegation-certificate \
    --stake-verification-key-file "$WB/stake.vkey" \
    --stake-pool-id "$POOL_ID" \
    --always-abstain \
    --key-reg-deposit-amt "$DEPOSIT" \
    --out-file "$CERT"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --change-address "$ADDR" \
    --certificate-file "$CERT" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$WB/payment.skey" \
    --signing-key-file "$WB/stake.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "pool=${POOL_ID:0:16},vote=abstain" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
