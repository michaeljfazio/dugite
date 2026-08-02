#!/usr/bin/env bash
# SubmitService.SubmitTx — the "path D" submit route.                   (#960)
#
# SKILL.md's submit-path axis has always listed "the UTxO RPC gRPC submit_tx"
# alongside the three N2C sockets. It was never exercised: run.sh did not pass
# --rpc-port, so no transaction in the history of this harness had ever entered
# dugite through gRPC.
#
# What this proves, and why it is not redundant with the N2C tests: SubmitTx
# hands bytes to the SAME mempool as N2C, but through a different decode and
# validation entry point. A tx that N2C accepts and gRPC rejects (or vice
# versa) is an accept-set asymmetry of exactly the kind the bidirectional
# parity oracle exists to catch — just on a different axis.
#
# The assertion is end-to-end: the tx must be accepted by gRPC AND land in a
# block observed by the Haskell node. An accept that never reaches the chain
# measures the RPC layer's politeness, not its correctness.

RPC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$RPC_DIR/lib/rpc-common.sh"
. "$(cd "$RPC_DIR/../tx-zoo/lib" && pwd)/tx-zoo-common.sh"
set +e

ADDR="$RPC_BP_ADDR"
NAME="submit-tx"

if ! rpc_available "$ADDR"; then
    rpc_row "$NAME" both "$ADDR" SKIP "env-skip: gRPC not reachable at $ADDR"
    exit 0
fi

WORK="$ZOO_BUILT/rpc-submit"
mkdir -p "$WORK"

PAY_ADDR=$(cat "$ZOO_PAY_ADDR_FILE" 2>/dev/null)
if [ -z "$PAY_ADDR" ]; then
    rpc_row "$NAME" both "$ADDR" SKIP "state-skip: no funded address (run tx-zoo/run-all.sh --setup)"
    exit 0
fi

# build_one <outfile> <lovelace> -> signed tx path, or empty on failure
build_one() {
    local out="$1" amount="$2"
    local txin bal
    txin=$(zoo_largest_utxo "$PAY_ADDR")
    [ -z "$txin" ] && return 1
    bal=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
            --socket-path "$ZOO_SOCKET" --address "$PAY_ADDR" --output-json 2>/dev/null \
          | jq -r --arg k "$txin" '.[$k].value.lovelace')
    [ -z "$bal" ] && return 1
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$txin" \
        --tx-out "$PAY_ADDR+$amount" \
        --change-address "$PAY_ADDR" \
        --out-file "$out.raw" >/dev/null 2>&1 || return 1
    cardano-cli conway transaction sign \
        --tx-body-file "$out.raw" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --testnet-magic "$LD_MAGIC" \
        --out-file "$out" >/dev/null 2>&1 || return 1
    printf '%s' "$out"
}

# tx_b64 <signed-tx-file>  — the raw CBOR bytes, base64 for grpcurl's `bytes`.
tx_b64() {
    python3 - "$1" <<'PYEOF'
import base64, json, sys, binascii
with open(sys.argv[1]) as fh:
    env = json.load(fh)
sys.stdout.write(base64.b64encode(binascii.unhexlify(env["cborHex"])).decode())
PYEOF
}

submit_via_rpc() {
    local ver="$1" pkg="$2" signed="$3"
    local b64 body out rc
    b64=$(tx_b64 "$signed") || { rpc_row "$NAME" "$ver" "$pkg" ERROR "could not read cborHex"; return 1; }
    body=$(jq -nc --arg t "$b64" '{tx:[{raw:$t}]}')
    out=$(rpc_call "$ADDR" "${pkg}.SubmitService/SubmitTx" "$body")
    rc=$?
    if [ "$rc" -ne 0 ]; then
        rpc_row "$NAME" "$ver" "${pkg}.SubmitService/SubmitTx" FAIL \
            "gRPC rejected a tx cardano-cli built as valid: $(printf '%s' "$out" | head -2 | tr '\n' ' ')"
        return 1
    fi
    # Response carries the tx ref(s) the server accepted.
    local ref
    ref=$(printf '%s' "$out" | jq -r '.. | strings' 2>/dev/null | head -1)
    printf '%s' "$ref"
    return 0
}

RC=0
for pair in "v1alpha utxorpc.v1alpha.submit" "v1beta utxorpc.v1beta.submit"; do
    set -- $pair
    ver="$1"; pkg="$2"
    signed=$(build_one "$WORK/pay-$ver.signed" 3000000)
    if [ -z "$signed" ]; then
        rpc_row "$NAME" "$ver" "${pkg}.SubmitService/SubmitTx" SKIP "state-skip: could not build a funding tx"
        continue
    fi
    txid=$(cardano-cli conway transaction txid --tx-file "$signed" 2>/dev/null \
           | jq -r '.txhash // empty' 2>/dev/null)
    [ -z "$txid" ] && txid=$(cardano-cli conway transaction txid --tx-file "$signed" 2>/dev/null | tr -d '[:space:]')

    ref=$(submit_via_rpc "$ver" "$pkg" "$signed") || { RC=1; continue; }

    # End-to-end: it must reach a block, and the Haskell node must see it.
    if zoo_wait_all_observers "$txid" 120 "$PAY_ADDR" >/dev/null 2>&1; then
        rpc_row "$NAME" "$ver" "${pkg}.SubmitService/SubmitTx" PASS \
            "tx $txid accepted via gRPC and observed on all 3 nodes"
    else
        rpc_row "$NAME" "$ver" "${pkg}.SubmitService/SubmitTx" FAIL \
            "tx $txid accepted by gRPC but never reached the chain on all observers"
        RC=1
    fi
    zoo_wait_mempool_quiet 60 >/dev/null 2>&1
done
exit $RC
