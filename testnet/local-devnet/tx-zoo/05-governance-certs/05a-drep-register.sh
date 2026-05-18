#!/usr/bin/env bash
# 05a — register drep-1 (with deposit + metadata anchor).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
DREP="$ZOO_KEYS/drep-1"
WA="$ZOO_KEYS/wallet-a"
[ -s "$DREP/drep.vkey" ] || die "drep-1 missing — run setup"
ADDR=$(cat "$WA/payment-stake.addr")

PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.dRepDeposit // .drepDeposit // 500000000' "$PPARAMS")

CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway governance drep registration-certificate \
    --drep-verification-key-file "$DREP/drep.vkey" \
    --key-reg-deposit-amt "$DEPOSIT" \
    --drep-metadata-url  "https://example.com/drep-1.json" \
    --drep-metadata-hash "0000000000000000000000000000000000000000000000000000000000000000" \
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
    --signing-key-file "$WA/payment.skey" \
    --signing-key-file "$DREP/drep.skey" \
    --out-file      "$SIGNED" >/dev/null
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
zoo_wait_inclusion "$TXID" 60 && zoo_record "$NAME" PASS "$TXID" "deposit=$DEPOSIT" \
                              || zoo_record "$NAME" FAIL "$TXID" "not-included"
