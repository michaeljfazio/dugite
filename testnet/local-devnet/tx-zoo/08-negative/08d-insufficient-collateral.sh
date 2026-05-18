#!/usr/bin/env bash
# 08d — Plutus tx with a collateral UTxO too small for the declared total
# collateral. Submission must fail with InsufficientCollateral.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v2.plutus"
[ -s "$SCRIPT" ] || { zoo_skip "missing $SCRIPT"; zoo_record "$NAME" SKIP; exit 0; }

PAIR=$(plutus_lock "$SCRIPT" inline 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collat"; exit 1; }
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
TIP=$(zoo_tip_slot); TTL=$((TIP + 600))
PPARAMS=$(zoo_pparams_file)

# Declare a total_collateral larger than any sane balance — guarantees rejection.
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build-raw \
    --tx-in         "$SCRIPT_TXIN" \
    --tx-in-script-file "$SCRIPT" \
    --tx-in-inline-datum-present \
    --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-execution-units "(1000000,1000000)" \
    --tx-in-collateral  "$COLLAT" \
    --tx-total-collateral 999999999999 \
    --tx-out        "${ADDR}+2000000" \
    --fee           500000 \
    --ttl           "$TTL" \
    --protocol-params-file "$PPARAMS" \
    --out-file      "$RAW" >/dev/null
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
zoo_expect_failure "insufficient-collateral submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-as-expected" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject"
