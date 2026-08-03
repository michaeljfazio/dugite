#!/usr/bin/env bash
# SyncService.ReadTip  ==  cardano-cli query tip                        (#960)
# QueryService.ReadUtxos == cardano-cli query utxo --tx-in
#
# The tip comparison is deliberately tolerant of ONE block of drift: the two
# reads are separate round-trips against a chain producing a block roughly
# every two slots, so an exact-equality assertion would be a coin flip. It is
# NOT tolerant of a stale or absent tip, which is the actual regression class.

RPC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$RPC_DIR/lib/rpc-common.sh"

ADDR="$RPC_BP_ADDR"

if ! rpc_available "$ADDR"; then
    rpc_row "read-tip" both "$ADDR" SKIP "env-skip: gRPC not reachable at $ADDR"
    rpc_row "read-utxos" both "$ADDR" SKIP "env-skip: gRPC not reachable at $ADDR"
    exit 0
fi

# ---------- ReadTip ----------
check_tip() {
    local ver="$1" pkg="$2"
    local cli_slot rpc_out rpc_slot rpc_hash cli_hash
    cli_slot=$(cardano-cli query tip --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.slot // empty')
    cli_hash=$(cardano-cli query tip --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_DUGITE_BP_SOCK" 2>/dev/null | jq -r '.hash // empty')

    rpc_out=$(rpc_call "$ADDR" "${pkg}.SyncService/ReadTip" '{}')
    if [ $? -ne 0 ]; then
        rpc_row "read-tip" "$ver" "${pkg}.SyncService/ReadTip" ERROR \
            "call failed: $(printf '%s' "$rpc_out" | head -1)"
        return 1
    fi
    # The response is {"tip":{"slot":"38","hash":"<b64>","height":"16"}}. An
    # earlier draft looked for `.index` (the TxoRef field name) and reported
    # "no tip index" against a perfectly good reply — a harness bug that would
    # have read as an RPC regression.
    rpc_slot=$(printf '%s' "$rpc_out" | jq -r '.. | objects | select(has("slot")) | .slot' 2>/dev/null | head -1)
    rpc_hash=$(printf '%s' "$rpc_out" | jq -r '.. | objects | select(has("hash")) | .hash' 2>/dev/null | head -1)

    if [ -z "$cli_slot" ]; then
        rpc_row "read-tip" "$ver" "${pkg}.SyncService/ReadTip" ERROR "cardano-cli query tip returned no slot"
        return 1
    fi
    if [ -z "$rpc_slot" ]; then
        rpc_row "read-tip" "$ver" "${pkg}.SyncService/ReadTip" FAIL \
            "RPC returned no tip index (raw: $(printf '%s' "$rpc_out" | head -c 200))"
        return 1
    fi

    # utxorpc encodes the block hash as base64 bytes; cardano-cli as hex.
    local rpc_hash_hex=""
    if [ -n "$rpc_hash" ] && [ "$rpc_hash" != "null" ]; then
        rpc_hash_hex=$(printf '%s' "$rpc_hash" | base64 -d 2>/dev/null | xxd -p 2>/dev/null | tr -d '\n')
    fi

    local drift=$(( cli_slot > rpc_slot ? cli_slot - rpc_slot : rpc_slot - cli_slot ))
    if [ "$drift" -le 5 ]; then
        local hashnote="hash=<absent>"
        if [ -n "$rpc_hash_hex" ]; then
            if [ "$rpc_hash_hex" = "$cli_hash" ]; then
                hashnote="hash matches cli exactly"
            else
                hashnote="hash differs (drift $drift slots — expected while the chain advances)"
            fi
        fi
        rpc_row "read-tip" "$ver" "${pkg}.SyncService/ReadTip" PASS \
            "rpc slot=$rpc_slot cli slot=$cli_slot drift=$drift; $hashnote"
    else
        rpc_row "read-tip" "$ver" "${pkg}.SyncService/ReadTip" FAIL \
            "tip drift $drift slots (rpc=$rpc_slot cli=$cli_slot) — RPC tip is stale"
        return 1
    fi
    return 0
}

# ---------- ReadUtxos ----------
# Pick a real UTxO from the funded devnet address, then ask the RPC for exactly
# that TxoRef and compare the lovelace amount.
check_utxos() {
    local ver="$1" pkg="$2"
    local addr utxo_json txin txid idx want_coin
    addr=$(cat "$LD_KEYS/utxo/payment.addr" 2>/dev/null)
    if [ -z "$addr" ]; then
        rpc_row "read-utxos" "$ver" "${pkg}.QueryService/ReadUtxos" SKIP \
            "state-skip: no funded address file ($LD_KEYS/utxo/payment.addr)"
        return 0
    fi
    utxo_json=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_DUGITE_BP_SOCK" --address "$addr" --output-json 2>/dev/null)
    txin=$(printf '%s' "$utxo_json" | jq -r 'keys[0] // empty')
    if [ -z "$txin" ]; then
        rpc_row "read-utxos" "$ver" "${pkg}.QueryService/ReadUtxos" SKIP \
            "state-skip: funded address has no UTxOs"
        return 0
    fi
    txid="${txin%%#*}"; idx="${txin##*#}"
    want_coin=$(printf '%s' "$utxo_json" | jq -r --arg k "$txin" '.[$k].value.lovelace')

    # TxoRef.hash is `bytes` — grpcurl accepts base64 for bytes fields.
    local txid_b64
    txid_b64=$(printf '%s' "$txid" | xxd -r -p 2>/dev/null | base64 | tr -d '\n')
    local body
    body=$(jq -nc --arg h "$txid_b64" --argjson i "$idx" '{keys:[{hash:$h,index:$i}]}')

    local out rc
    out=$(rpc_call "$ADDR" "${pkg}.QueryService/ReadUtxos" "$body")
    rc=$?
    if [ "$rc" -ne 0 ]; then
        rpc_row "read-utxos" "$ver" "${pkg}.QueryService/ReadUtxos" ERROR \
            "call failed for $txin: $(printf '%s' "$out" | head -1)"
        return 1
    fi
    # `coin` is a BigInt MESSAGE (`oneof {int|big_u_int|big_n_int}`), not a
    # plain scalar, so proto3 JSON renders it as an OBJECT: {"int":"5000000"}.
    #
    # Two harness bugs died here. First `.. | select(has("coin")) | .coin` with
    # `head -1` grabbed the enclosing object and printed "coin rpc={". Then
    # restricting to scalars rejected the BigInt wrapper outright and reported
    # "no coin amount returned" against a response that carried it correctly.
    # Unwrap the oneof, and accept a bare number too in case the mapping ever
    # emits one.
    local got_coin
    got_coin=$(printf '%s' "$out" | jq -r '
        [ .. | objects | select(has("coin")) | .coin
          | if type == "object" then (.int // .bigUInt // .bigNInt)
            else . end
          | select(. != null) | tostring ] | first // empty' 2>/dev/null)
    if [ -z "$got_coin" ] || [ "$got_coin" = "null" ]; then
        rpc_row "read-utxos" "$ver" "${pkg}.QueryService/ReadUtxos" FAIL \
            "no coin amount returned for known UTxO $txin (raw: $(printf '%s' "$out" | head -c 200))"
        return 1
    fi
    if [ "$got_coin" = "$want_coin" ]; then
        rpc_row "read-utxos" "$ver" "${pkg}.QueryService/ReadUtxos" PASS \
            "$txin coin=$got_coin matches cardano-cli exactly"
    else
        rpc_row "read-utxos" "$ver" "${pkg}.QueryService/ReadUtxos" FAIL \
            "$txin coin rpc=$got_coin cli=$want_coin"
        return 1
    fi
    return 0
}

RC=0
check_tip   v1alpha utxorpc.v1alpha.sync  || RC=1
check_tip   v1beta  utxorpc.v1beta.sync   || RC=1
check_utxos v1alpha utxorpc.v1alpha.query || RC=1
check_utxos v1beta  utxorpc.v1beta.query  || RC=1
exit $RC
