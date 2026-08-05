#!/usr/bin/env bash
# 18j — a V3 spend of a datum-LESS script UTxO. ACCEPTED — datum is OPTIONAL
# for Plutus V3 spending scripts (unlike V1/V2, which require one).
#
# Upstream: tests_plutus_v3/test_spend_build.py::test_txout_locking_no_datum.
#
# 03c (03-plutus/03c-spend-v3.sh) only covers the inline-datum V3 form. This
# is the complementary datum-less form.
#
# #969 TRAP, read carefully: upstream's `alwaysSucceedsNoDatum` succeeds for
# every purpose EXCEPT a spending script that carries a datum — i.e. for
# spending WITHOUT a datum it is exactly the variant we want. Do not reach
# for `always-true-v3-spend.plutus` (the WithDatum alias 03c uses) here —
# that variant unconditionally requires a datum and would fail this spend
# outright. `always-true-v3.plutus` (alwaysSucceedsNoDatum, the SAME script
# 03f/13-script-purposes use for mint/certify/vote/propose) is the correct
# one for a datum-less V3 spend.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/18-plutus-edges/_edge-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
SCRIPT="$ZOO_DIR/lib/plutus/always-true-v3.plutus"
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing-script-binary $(basename "$SCRIPT")"; exit 0; }

PAIR=$(plutus_lock "$SCRIPT" none 5000000) || { zoo_record "$NAME" FAIL "" "lock"; exit 1; }
SCRIPT_TXIN=${PAIR%% *}

COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
echo '{"int": 0}' > "$REDEEMER"
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
# No --tx-in-datum-file, no --tx-in-inline-datum-present at all — the
# locked UTxO genuinely carries no datum in any form.
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "$SCRIPT_TXIN" \
    --tx-in-script-file "$SCRIPT" \
    --tx-in-redeemer-file "$REDEEMER" \
    --tx-in-collateral  "$COLLAT" \
    --tx-out        "${ADDR}+2000000" \
    --change-address "$ADDR" \
    --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null
# RED-PROOF: swap SCRIPT to always-true-v3-spend.plutus (the WithDatum
# alias) with a datum-less lock — must FAIL, since that variant asserts
# datum presence. Proves this test is not vacuously accepting any V3 script.
TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
if zoo_wait_all_observers "$TXID" 120 "$ADDR"; then
    zoo_record "$NAME" PASS "$TXID" "V3 spend, no datum in any form"
else
    zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1
fi
