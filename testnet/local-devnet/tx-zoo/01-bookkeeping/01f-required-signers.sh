#!/usr/bin/env bash
# 01f — tx with `requiredSigners` populated. wallet-a's key is added to the
# required signers set even though it's not spending — exercises the
# extraKeyWitnesses ledger field.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
WA="$ZOO_KEYS/wallet-a"
[ -s "$WA/payment.vkey" ] || die "wallet-a missing — run ./run-all.sh --setup"

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_fail "no UTxO"; zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}

# Resolve wallet-a's payment key hash (the form ledger stores as a required-signer).
SIGNER_KH=$(cardano-cli conway address key-hash \
    --payment-verification-key-file "$WA/payment.vkey")

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$TXIN" \
    --tx-out        "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --required-signer-hash "$SIGNER_KH" \
    --out-file      "$RAW" >/dev/null
# Both the genesis utxo key AND wallet-a must sign because of the required-signer
# entry.
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --signing-key-file "$WA/payment.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" || zoo_record "$NAME" FAIL "$TXID" "not-included"
