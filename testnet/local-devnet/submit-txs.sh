#!/usr/bin/env bash
# Submit N self-transfer txs from the genesis UTxO key via a given socket.
# Usage: submit-txs.sh <socket-path> <count> <label-prefix>
# Outputs: txid per submission on stdout, one per line.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/common.sh"

SOCKET="${1:?socket path required}"
COUNT="${2:?count required}"
LABEL_PREFIX="${3:-tx}"

ADDR=$(cat "$LD_KEYS/utxo/payment.addr")
PAYMENT_SKEY="$LD_KEYS/utxo/payment.skey"

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

# Pull current protocol params (not strictly required for `transaction build`
# since the node infers them, but warming the connection here surfaces a
# wrong-magic / dead-socket error early with a clear message).
cardano-cli conway query protocol-parameters \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$SOCKET" \
    --out-file      "$WORKDIR/pparams.json"

for i in $(seq 1 "$COUNT"); do
    # Refresh UTxO between txs so the next tx picks up the previous tx's change output
    cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$SOCKET" \
        --address       "$ADDR" \
        --out-file      "$WORKDIR/utxo.json"

    # Pick the largest UTxO as input
    INPUT=$(jq -r 'to_entries | sort_by(-.value.value.lovelace) | .[0].key' "$WORKDIR/utxo.json")

    if [ -z "$INPUT" ] || [ "$INPUT" = "null" ]; then
        log_error "No UTxO at $ADDR via $SOCKET"
        exit 1
    fi

    # Build a metadata file with our unique label
    cat > "$WORKDIR/meta.json" <<EOF
{ "$(($(date +%s) % 65536))": { "label": "$LABEL_PREFIX-$i" } }
EOF

    # Build, sign, submit. cardano-cli writes "Estimated transaction fee"
    # and submit JSON to stdout; redirect to stderr/log so our stdout stays
    # clean (one txid per line, as documented).
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$SOCKET" \
        --tx-in         "$INPUT" \
        --tx-out        "$ADDR+2000000" \
        --change-address "$ADDR" \
        --metadata-json-file "$WORKDIR/meta.json" \
        --out-file      "$WORKDIR/tx.raw" >"$WORKDIR/build.out" 2>"$WORKDIR/build.err" \
        || { cat "$WORKDIR/build.out" "$WORKDIR/build.err" >&2; exit 1; }

    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file  "$WORKDIR/tx.raw" \
        --signing-key-file "$PAYMENT_SKEY" \
        --out-file      "$WORKDIR/tx.signed" >/dev/null

    # cardano-cli 11.0.0.0 defaults to JSON for `transaction txid`; we want
    # the bare hex so the caller can grep / awk it cleanly.
    TXID=$(cardano-cli conway transaction txid --tx-file "$WORKDIR/tx.signed" --output-text)

    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$SOCKET" \
        --tx-file       "$WORKDIR/tx.signed" >"$WORKDIR/submit.out" 2>"$WORKDIR/submit.err" \
        || { log_warn "Submit failed for tx $TXID: $(cat "$WORKDIR/submit.err")"; }

    echo "$TXID"

    # Small gap so the next UTxO query sees the change output
    sleep 2
done
