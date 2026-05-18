#!/usr/bin/env bash
# Internal helper for the 03-plutus scripts: lock funds at the given Plutus
# script's address with a datum hash (legacy) or inline datum (Babbage+).
# Outputs the (txid#ix lovelace) tuple of the resulting UTxO.
#
# Usage: . _lock-helper.sh; plutus_lock <plutus-file> <datum-mode>
#   datum-mode: hash | inline | none
set -euo pipefail

plutus_lock() {
    local script="$1" mode="${2:-hash}" amount="${3:-5000000}"
    [ -s "$script" ] || die "plutus_lock: $script missing"

    local addr; addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local utxo; utxo=$(zoo_largest_utxo "$addr") || die "plutus_lock: no UTxO"
    local txin=${utxo%% *}

    local script_addr_file="$ZOO_BUILT/$(basename "$script" .plutus).addr"
    cardano-cli conway address build \
        --payment-script-file "$script" \
        --testnet-magic "$LD_MAGIC" \
        --out-file      "$script_addr_file"
    local script_addr; script_addr=$(cat "$script_addr_file")

    local datum_arg
    local datum_file="$ZOO_BUILT/$(basename "$script" .plutus).datum.json"
    echo '{"int": 42}' > "$datum_file"
    case "$mode" in
        hash)
            local dhash; dhash=$(cardano-cli conway transaction hash-script-data --script-data-file "$datum_file")
            datum_arg=(--tx-out-datum-hash "$dhash")
            ;;
        inline)
            datum_arg=(--tx-out-inline-datum-file "$datum_file")
            ;;
        none)
            datum_arg=()
            ;;
        *) die "plutus_lock: bad mode $mode" ;;
    esac

    local raw="$ZOO_BUILT/$(basename "$script" .plutus)-lock.raw"
    local signed="$ZOO_BUILT/$(basename "$script" .plutus)-lock.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-in         "$txin" \
        --tx-out        "${script_addr}+${amount}" \
        "${datum_arg[@]}" \
        --change-address "$addr" \
        --out-file      "$raw" >/dev/null
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file  "$raw" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file      "$signed" >/dev/null
    local txid; txid=$(zoo_submit "$signed") || die "plutus_lock: submit failed"
    zoo_wait_inclusion "$txid" 60 || die "plutus_lock: tx $txid not included"
    # Find the script-address output index.
    local tmp; tmp=$(mktemp)
    cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --address       "$script_addr" \
        --out-file      "$tmp"
    local pair; pair=$(jq -r --arg t "$txid" '
        to_entries
        | map(select(.key | startswith($t)))
        | sort_by(-.value.value.lovelace)
        | .[0] | "\(.key) \(.value.value.lovelace)"' "$tmp")
    rm -f "$tmp"
    [ -z "$pair" ] && die "plutus_lock: locked UTxO not visible"
    echo "$pair"
}

# Pick a fresh, datum-free collateral UTxO at the payer addr. Plutus txs
# require collateral; we use the genesis utxo address itself.
#
# `prefund_collateral_pool` in lib/keygen.sh pre-splits the addr into ~30
# small UTxOs (50M lovelace each) so we always have spare candidates. We
# pick a small UTxO (lovelace <= 100M) that has no datum and no
# reference script attached — those constraints match what cardano-cli's
# Plutus phase-2 evaluator requires from a collateral input.
#
# Two flavours:
#   plutus_collateral       — outputs "<txid>#<ix>"  (just the txin)
#   plutus_collateral_pair  — outputs "<txid>#<ix> <lovelace>" for callers
#                             that need the lovelace amount (e.g. 03j).
_plutus_collateral_jq='
    to_entries
    | map(select(
        .value.value.lovelace >= 10000000
        and .value.value.lovelace <= 100000000
        and (.value.value | keys) == ["lovelace"]
        and (.value.datumhash // null) == null
        and (.value.datum // null) == null
        and (.value.inlineDatum // null) == null
        and (.value.referenceScript // null) == null
      ))
    | sort_by(.value.value.lovelace)
'

plutus_collateral() {
    local addr; addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local tmp; tmp=$(mktemp)
    cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --address       "$addr" \
        --output-json > "$tmp"
    local txin
    txin=$(jq -r "$_plutus_collateral_jq | .[0].key // empty" "$tmp")
    rm -f "$tmp"
    if [ -z "$txin" ]; then
        die "plutus_collateral: no spare UTxO ≤100M ADA at $addr — re-run with --setup to provision the collateral pool"
    fi
    echo "$txin"
}

plutus_collateral_pair() {
    local addr; addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local tmp; tmp=$(mktemp)
    cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --address       "$addr" \
        --output-json > "$tmp"
    local pair
    pair=$(jq -r "$_plutus_collateral_jq | .[0] | select(. != null) | \"\(.key) \(.value.value.lovelace)\"" "$tmp")
    rm -f "$tmp"
    if [ -z "$pair" ]; then
        die "plutus_collateral_pair: no spare UTxO ≤100M ADA at $addr — re-run with --setup to provision the collateral pool"
    fi
    echo "$pair"
}
