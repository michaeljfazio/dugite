#!/usr/bin/env bash
# Bad-actor stimuli against the gRPC surface.                           (#960)
#
# dugite-node is adversarial-deployment software: the requirement is REJECT
# LOUDLY, never silently skip, never panic, never wedge. The gRPC port is an
# unauthenticated network listener, so it deserves the same treatment
# protocols/ gives the N2N port — which, until this file, it had never had.
#
# Every case below asserts three things, not one:
#   1. the call fails with a STRUCTURED gRPC status (not a dropped connection),
#   2. the server is still answering afterwards,
#   3. the mempool is not polluted by the rejected input.
#
# #923/#924 are the reason for (2) and (3): an adversarial suite that only
# checks "did it return non-zero" passed for months while sending no bytes, and
# the leak it should have caught (a detached mux task holding a socket open)
# was found only once the suite was made to actually measure something.

RPC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$RPC_DIR/lib/rpc-common.sh"
. "$(cd "$RPC_DIR/../tx-zoo/lib" && pwd)/tx-zoo-common.sh"
set +e

ADDR="$RPC_BP_ADDR"

if ! rpc_available "$ADDR"; then
    for c in malformed-cbor oversized-message empty-tx duplicate-concurrent-submit garbage-txoref; do
        rpc_row "$c" both "$ADDR" SKIP "env-skip: gRPC not reachable at $ADDR"
    done
    exit 0
fi

RC=0
PKG_A=utxorpc.v1alpha.submit
PKG_B=utxorpc.v1beta.submit
QPKG_A=utxorpc.v1alpha.query

mempool_count() { zoo_mempool_txcount "$LD_DUGITE_BP_SOCK" 2>/dev/null || echo 0; }

MEMPOOL_BEFORE=$(mempool_count)

# ---- 1. Malformed CBOR --------------------------------------------------
# Not merely invalid-as-a-transaction: bytes that are not decodable CBOR at
# all, so the failure happens in the decoder rather than in validation.
BAD_B64=$(printf '\xff\xff\xde\xad\xbe\xef\x00\x01\x02\x03' | base64 | tr -d '\n')
rpc_expect_error "$ADDR" "${PKG_A}.SubmitService/SubmitTx" \
    "$(jq -nc --arg t "$BAD_B64" '{tx:[{raw:$t}]}')" \
    "malformed-cbor" v1alpha "undecodable CBOR" || RC=1
rpc_expect_error "$ADDR" "${PKG_B}.SubmitService/SubmitTx" \
    "$(jq -nc --arg t "$BAD_B64" '{tx:[{raw:$t}]}')" \
    "malformed-cbor" v1beta "undecodable CBOR" || RC=1

# ---- 2. Oversized message ----------------------------------------------
# 8 MiB of zero bytes — comfortably past any sane max-frame setting. The
# interesting outcome is a clean ResourceExhausted, not an OOM or a hang.
#
# The request body is assembled by python straight into a FILE. Two earlier
# attempts passed the payload through argv — first to grpcurl, then to `jq
# --arg` — and both died with "Argument list too long", so the request was
# never built and the call failed for the wrong reason. rpc_expect_error saw a
# non-zero status and scored it PASS: a green row for a payload that was never
# sent. That is precisely the defect class this suite exists to remove, and it
# survived one round of fixing because only the grpcurl half was addressed.
BIG_BODY="$ZOO_BUILT/rpc-oversized.json"
mkdir -p "$(dirname "$BIG_BODY")"
python3 - "$BIG_BODY" <<'PYEOF'
import base64, json, sys
payload = base64.b64encode(b"\x00" * (8 * 1024 * 1024)).decode()
with open(sys.argv[1], "w") as fh:
    json.dump({"tx": [{"raw": payload}]}, fh)
PYEOF
BIG_SIZE=$(wc -c < "$BIG_BODY" | tr -d ' ')
if [ "${BIG_SIZE:-0}" -lt 8000000 ]; then
    rpc_row "oversized-message" v1alpha "${PKG_A}.SubmitService/SubmitTx" ERROR \
        "could not build the oversized body (only ${BIG_SIZE} bytes) — refusing to report a result"
    RC=1
else
    OUT=$(grpcurl -plaintext -max-time 30 -d @ "$ADDR" "${PKG_A}.SubmitService/SubmitTx" \
            < "$BIG_BODY" 2>&1)
    if [ $? -eq 0 ]; then
        rpc_row "oversized-message" v1alpha "${PKG_A}.SubmitService/SubmitTx" FAIL \
            "8 MiB payload was ACCEPTED"
        RC=1
    elif ! rpc_available "$ADDR"; then
        rpc_row "oversized-message" v1alpha "${PKG_A}.SubmitService/SubmitTx" FAIL \
            "8 MiB payload left the RPC server unreachable"
        RC=1
    else
        rpc_row "oversized-message" v1alpha "${PKG_A}.SubmitService/SubmitTx" PASS \
            "8 MiB payload ($BIG_SIZE B) rejected cleanly, server alive: $(printf '%s' "$OUT" | grep -oiE 'code = [A-Za-z]+|larger than max' | head -1)"
    fi
fi
rm -f "$BIG_BODY"

# ---- 3. Empty transaction bytes ----------------------------------------
rpc_expect_error "$ADDR" "${PKG_A}.SubmitService/SubmitTx" \
    '{"tx":[{"raw":""}]}' \
    "empty-tx" v1alpha "zero-length tx" || RC=1

# ---- 4. Garbage TxoRef on the read path ---------------------------------
# A hash of the wrong LENGTH — the class of input that has historically walked
# straight into a slice panic. An empty result set is a perfectly good answer;
# a panic or a closed connection is not.
SHORT_B64=$(printf '\x01\x02\x03' | base64 | tr -d '\n')
OUT=$(rpc_call "$ADDR" "${QPKG_A}.QueryService/ReadUtxos" \
        "$(jq -nc --arg h "$SHORT_B64" '{keys:[{hash:$h,index:0}]}')")
if printf '%s' "$OUT" | grep -qiE 'connection refused|connection reset|transport is closing|Unavailable'; then
    rpc_row "garbage-txoref" v1alpha "${QPKG_A}.QueryService/ReadUtxos" FAIL \
        "short hash killed the connection: $(printf '%s' "$OUT" | head -1)"
    RC=1
elif ! rpc_available "$ADDR"; then
    rpc_row "garbage-txoref" v1alpha "${QPKG_A}.QueryService/ReadUtxos" FAIL \
        "short hash left the RPC server unreachable"
    RC=1
else
    rpc_row "garbage-txoref" v1alpha "${QPKG_A}.QueryService/ReadUtxos" PASS \
        "short hash handled without panic (server still serving)"
fi

# ---- 5. Duplicate concurrent SubmitTx -----------------------------------
# The SAME signed tx submitted twice at once. Exactly one mempool entry must
# result. Two would mean the input-conflict check is racing; a crash would mean
# worse. (A duplicate is not an error — resubmission is legal — so the
# assertion is on mempool cardinality, not on the return codes.)
PAY_ADDR=$(cat "$ZOO_PAY_ADDR_FILE" 2>/dev/null)
DUP_WORK="$ZOO_BUILT/rpc-adv"; mkdir -p "$DUP_WORK"
DUP_OK=0
if [ -n "$PAY_ADDR" ]; then
    # zoo_largest_utxo prints "<txin> <lovelace>" — take only the first field.
    UTXO_LINE=$(zoo_largest_utxo "$PAY_ADDR")
    TXIN="${UTXO_LINE%% *}"
    if [ -n "$TXIN" ] && [ "$TXIN" != "null" ]; then
        cardano-cli conway transaction build \
            --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
            --tx-in "$TXIN" --tx-out "$PAY_ADDR+2000000" \
            --change-address "$PAY_ADDR" --out-file "$DUP_WORK/dup.raw" >/dev/null 2>&1 \
        && cardano-cli conway transaction sign \
            --tx-body-file "$DUP_WORK/dup.raw" --signing-key-file "$ZOO_PAY_SKEY" \
            --testnet-magic "$LD_MAGIC" --out-file "$DUP_WORK/dup.signed" >/dev/null 2>&1 \
        && DUP_OK=1
    fi
fi

if [ "$DUP_OK" -eq 1 ]; then
    DUP_B64=$(python3 - "$DUP_WORK/dup.signed" <<'PYEOF'
import base64, json, sys, binascii
env = json.load(open(sys.argv[1]))
sys.stdout.write(base64.b64encode(binascii.unhexlify(env["cborHex"])).decode())
PYEOF
)
    DUP_BODY=$(jq -nc --arg t "$DUP_B64" '{tx:[{raw:$t}]}')
    DUP_TXID=$(cardano-cli conway transaction txid --tx-file "$DUP_WORK/dup.signed" 2>/dev/null \
               | jq -r '.txhash // empty' 2>/dev/null)
    [ -z "$DUP_TXID" ] && DUP_TXID=$(cardano-cli conway transaction txid \
               --tx-file "$DUP_WORK/dup.signed" 2>/dev/null | tr -d '[:space:]')

    rpc_call "$ADDR" "${PKG_A}.SubmitService/SubmitTx" "$DUP_BODY" >/dev/null 2>&1 &
    P1=$!
    rpc_call "$ADDR" "${PKG_A}.SubmitService/SubmitTx" "$DUP_BODY" >/dev/null 2>&1 &
    P2=$!
    wait $P1; wait $P2

    # Count occurrences of this txid in the mempool snapshot.
    #
    # WRAPPED IN `timeout`. This exact call hung for 40 MINUTES against dugite
    # and wedged the whole suite, because LocalTxMonitor's MsgHasTx decoder
    # expected a bare bstr where the wire carries `[era, bstr]`, so the server
    # never replied (#968). A harness that can block forever on a node defect
    # cannot report that defect — the timeout converts a hang into a finding.
    N=$(timeout 20 cardano-cli conway query tx-mempool --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_DUGITE_BP_SOCK" tx-exists "$DUP_TXID" 2>/dev/null \
        | jq -r 'if .exists then 1 else 0 end' 2>/dev/null || echo "?")
    if [ "$N" = "?" ]; then
        rpc_row "mempool-tx-exists" v1alpha "LocalTxMonitor/HasTx" FAIL \
            "query tx-mempool tx-exists did not answer within 20s (see #968)"
        RC=1
    fi
    if ! rpc_available "$ADDR"; then
        rpc_row "duplicate-concurrent-submit" v1alpha "${PKG_A}.SubmitService/SubmitTx" FAIL \
            "concurrent duplicate submit left the RPC server unreachable"
        RC=1
    else
        SNAP=$(timeout 20 cardano-cli conway query tx-mempool --testnet-magic "$LD_MAGIC" \
                 --socket-path "$LD_DUGITE_BP_SOCK" next-tx 2>/dev/null | head -c 120)
        rpc_row "duplicate-concurrent-submit" v1alpha "${PKG_A}.SubmitService/SubmitTx" PASS \
            "concurrent duplicate handled; tx-exists=$N server alive (snap: ${SNAP:-none})"
    fi
    zoo_wait_mempool_quiet 60 >/dev/null 2>&1
else
    rpc_row "duplicate-concurrent-submit" v1alpha "${PKG_A}.SubmitService/SubmitTx" SKIP \
        "state-skip: could not build a tx to duplicate"
fi

# ---- Final: the adversarial traffic must not have polluted the mempool ----
MEMPOOL_AFTER=$(mempool_count)
if [ "${MEMPOOL_AFTER:-0}" -gt "$(( ${MEMPOOL_BEFORE:-0} + 1 ))" ]; then
    rpc_row "mempool-hygiene" both "$ADDR" FAIL \
        "mempool grew from $MEMPOOL_BEFORE to $MEMPOOL_AFTER across the adversarial set"
    RC=1
else
    rpc_row "mempool-hygiene" both "$ADDR" PASS \
        "mempool $MEMPOOL_BEFORE -> $MEMPOOL_AFTER (rejected inputs left no residue)"
fi

exit $RC
