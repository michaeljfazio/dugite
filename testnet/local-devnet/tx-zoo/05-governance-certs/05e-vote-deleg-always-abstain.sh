#!/usr/bin/env bash
# 05e — delegate a stake-cred's vote to the special AlwaysAbstain DRep.
# Uses a freshly generated stake key so we don't conflict with 04c/05d.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
# Ad-hoc stake credential for this test (separate from wallets a/b).
STK_DIR="$ZOO_KEYS/stake-abstain"
mkdir -p "$STK_DIR"
if [ ! -s "$STK_DIR/stake.skey" ]; then
    cardano-cli conway stake-address key-gen \
        --verification-key-file "$STK_DIR/stake.vkey" \
        --signing-key-file      "$STK_DIR/stake.skey"
fi
# Build the combined payment+stake addr (using genesis utxo's payment key for funding).
COMBINED_ADDR_FILE="$STK_DIR/payment-stake.addr"
cardano-cli conway address build \
    --payment-verification-key-file "$ZOO_PAY_VKEY" \
    --stake-verification-key-file   "$STK_DIR/stake.vkey" \
    --testnet-magic "$LD_MAGIC" \
    --out-file "$COMBINED_ADDR_FILE"
ADDR=$(cat "$COMBINED_ADDR_FILE")

# Need a tiny funding step at this fresh address.
GENESIS_ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
SRC_UTXO=$(zoo_largest_utxo "$GENESIS_ADDR") || { zoo_record "$NAME" FAIL "" "no-src-utxo"; exit 1; }
FUND_RAW="$ZOO_BUILT/$NAME-fund.raw"
FUND_SIGNED="$ZOO_BUILT/$NAME-fund.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "${SRC_UTXO%% *}" \
    --tx-out        "${ADDR}+10000000" \
    --change-address "$GENESIS_ADDR" \
    --out-file      "$FUND_RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$FUND_RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$FUND_SIGNED" >/dev/null
FUND_TXID=$(zoo_submit "$FUND_SIGNED") || { zoo_record "$NAME" FAIL "" "fund-submit"; exit 1; }
zoo_wait_inclusion "$FUND_TXID" 60 || { zoo_record "$NAME" FAIL "$FUND_TXID" "fund-not-incl"; exit 1; }

# Register the stake key + delegate-vote-AlwaysAbstain in one Conway cert.
PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")
CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address registration-and-vote-delegation-certificate \
    --stake-verification-key-file "$STK_DIR/stake.vkey" \
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
    --signing-key-file "$ZOO_PAY_SKEY" \
    --signing-key-file "$STK_DIR/stake.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_all_observers "$TXID" 120 "$ADDR" && zoo_record "$NAME" PASS "$TXID" "vote=alwaysAbstain" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
