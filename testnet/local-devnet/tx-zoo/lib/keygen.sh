#!/usr/bin/env bash
# Generate the auxiliary keys the tx-zoo needs.
# Idempotent — re-running only fills in missing files.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/tx-zoo-common.sh"

zoo_info "keygen — writing into $ZOO_KEYS"

gen_payment() {
    local name="$1"
    local dir="$ZOO_KEYS/$name"
    mkdir -p "$dir"
    if [ ! -s "$dir/payment.skey" ]; then
        zoo_info "  payment key: $name"
        cardano-cli conway address key-gen \
            --verification-key-file "$dir/payment.vkey" \
            --signing-key-file      "$dir/payment.skey"
    fi
    if [ ! -s "$dir/payment.addr" ]; then
        cardano-cli conway address build \
            --payment-verification-key-file "$dir/payment.vkey" \
            --testnet-magic "$LD_MAGIC" \
            --out-file      "$dir/payment.addr"
    fi
}

gen_stake() {
    local name="$1"
    local dir="$ZOO_KEYS/$name"
    mkdir -p "$dir"
    if [ ! -s "$dir/stake.skey" ]; then
        zoo_info "  stake key: $name"
        cardano-cli conway stake-address key-gen \
            --verification-key-file "$dir/stake.vkey" \
            --signing-key-file      "$dir/stake.skey"
    fi
    if [ ! -s "$dir/stake.addr" ]; then
        cardano-cli conway stake-address build \
            --stake-verification-key-file "$dir/stake.vkey" \
            --testnet-magic "$LD_MAGIC" \
            --out-file      "$dir/stake.addr"
    fi
}

# Combined payment+stake address used by reward-withdrawal tests etc.
gen_payment_with_stake() {
    local name="$1"
    local dir="$ZOO_KEYS/$name"
    gen_payment "$name"
    gen_stake   "$name"
    if [ ! -s "$dir/payment-stake.addr" ]; then
        cardano-cli conway address build \
            --payment-verification-key-file "$dir/payment.vkey" \
            --stake-verification-key-file   "$dir/stake.vkey" \
            --testnet-magic "$LD_MAGIC" \
            --out-file      "$dir/payment-stake.addr"
    fi
}

gen_drep() {
    local name="$1"
    local dir="$ZOO_KEYS/$name"
    mkdir -p "$dir"
    if [ ! -s "$dir/drep.skey" ]; then
        zoo_info "  drep key: $name"
        cardano-cli conway governance drep key-gen \
            --verification-key-file "$dir/drep.vkey" \
            --signing-key-file      "$dir/drep.skey"
    fi
    if [ ! -s "$dir/drep.id" ]; then
        cardano-cli conway governance drep id \
            --drep-verification-key-file "$dir/drep.vkey" \
            --out-file "$dir/drep.id"
    fi
}

gen_cc() {
    local name="$1"
    local dir="$ZOO_KEYS/$name"
    mkdir -p "$dir"
    # If setup.sh pre-provisioned a CC keypair at $LD_KEYS/$name (it bootstraps
    # cc-1 as a real committee member in the conway-genesis), reuse those keys
    # so the zoo's CC hot-key auth + voting + resign scripts operate on a
    # genuinely seated member rather than an orphan. Otherwise fall back to
    # generating an orphan keypair (legacy path).
    local devnet_dir="$LD_KEYS/$name"
    if [ -s "$devnet_dir/cc-cold.skey" ] && [ -s "$devnet_dir/cc-hot.skey" ]; then
        if [ ! -s "$dir/cc-cold.skey" ]; then
            zoo_info "  CC keys: reusing $name from devnet ($devnet_dir)"
            cp "$devnet_dir/cc-cold.skey" "$dir/cc-cold.skey"
            cp "$devnet_dir/cc-cold.vkey" "$dir/cc-cold.vkey"
            cp "$devnet_dir/cc-hot.skey"  "$dir/cc-hot.skey"
            cp "$devnet_dir/cc-hot.vkey"  "$dir/cc-hot.vkey"
        fi
        return 0
    fi
    if [ ! -s "$dir/cc-cold.skey" ]; then
        zoo_info "  CC cold key: $name (orphan — devnet has no seated CC)"
        cardano-cli conway governance committee key-gen-cold \
            --verification-key-file "$dir/cc-cold.vkey" \
            --cold-signing-key-file "$dir/cc-cold.skey"
    fi
    if [ ! -s "$dir/cc-hot.skey" ]; then
        zoo_info "  CC hot key: $name"
        cardano-cli conway governance committee key-gen-hot \
            --verification-key-file "$dir/cc-hot.vkey" \
            --signing-key-file      "$dir/cc-hot.skey"
    fi
}

gen_pool() {
    local name="$1"
    local dir="$ZOO_KEYS/$name"
    mkdir -p "$dir"
    if [ ! -s "$dir/cold.skey" ]; then
        zoo_info "  pool cold key: $name"
        cardano-cli conway node key-gen \
            --cold-verification-key-file "$dir/cold.vkey" \
            --cold-signing-key-file      "$dir/cold.skey" \
            --operational-certificate-issue-counter-file "$dir/opcert.counter"
    fi
    if [ ! -s "$dir/vrf.skey" ]; then
        cardano-cli conway node key-gen-VRF \
            --verification-key-file "$dir/vrf.vkey" \
            --signing-key-file      "$dir/vrf.skey"
    fi
}

# Fund a sub-payment address from the genesis utxo key. Idempotent: only sends
# if the address has less than $min_lovelace.
fund_address() {
    local addr_file="$1" amount="${2:-1000000000}" min="${3:-500000000}"
    local addr; addr=$(cat "$addr_file")
    local existing
    existing=$(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --address       "$addr" \
        --output-json 2>/dev/null \
        | jq '[.[].value.lovelace] | add // 0')
    if [ "${existing:-0}" -ge "$min" ]; then
        zoo_info "  $addr already funded (${existing} lovelace)"
        return 0
    fi
    zoo_info "  funding $addr with $amount lovelace"
    local src_addr; src_addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local utxo; utxo=$(zoo_largest_utxo "$src_addr") || die "no UTxO at $src_addr"
    local in=${utxo%% *}
    local raw="$ZOO_BUILT/fund-$(basename "$addr_file").raw"
    local signed="$ZOO_BUILT/fund-$(basename "$addr_file").signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-in         "$in" \
        --tx-out        "${addr}+${amount}" \
        --change-address "$src_addr" \
        --out-file      "$raw" >/dev/null
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file  "$raw" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file      "$signed" >/dev/null
    local txid; txid=$(zoo_submit "$signed")
    zoo_wait_inclusion "$txid" 120 || die "funding tx $txid not seen"
    # Wait until the funding socket itself shows the spent input gone — otherwise
    # the next fund_address picks the now-spent input and the submit fails.
    local consumed_in="$in" j=0
    while [ "$j" -lt 60 ]; do
        local still
        still=$(cardano-cli conway query utxo \
                  --testnet-magic "$LD_MAGIC" \
                  --socket-path   "$ZOO_SOCKET" \
                  --address       "$src_addr" \
                  --output-json 2>/dev/null \
                | jq --arg t "$consumed_in" '[keys[] | select(. == $t)] | length' 2>/dev/null \
                || echo 0)
        [ "${still:-0}" -eq 0 ] && return 0
        sleep 1
        j=$((j+1))
    done
    die "funder UTxO $consumed_in still visible at $ZOO_SOCKET after 60s"
}

# Top-level: provision every key the zoo needs.
keygen_all() {
    zoo_require_devnet
    # Two extra payment+stake wallets — used by 04-stake, 05-gov-certs, etc.
    gen_payment_with_stake "wallet-a"
    gen_payment_with_stake "wallet-b"
    # DRep + CC + pool keys.
    gen_drep "drep-1"
    gen_drep "drep-2"
    gen_drep "drep-3"
    gen_cc   "cc-1"
    gen_pool "pool3"
    # Fund the sub-wallets so they can submit their own txs.
    fund_address "$ZOO_KEYS/wallet-a/payment-stake.addr" 5000000000 1000000000
    fund_address "$ZOO_KEYS/wallet-b/payment-stake.addr" 5000000000 1000000000
    zoo_info "keygen complete — $(ls -1 "$ZOO_KEYS" | wc -l | tr -d ' ') sub-dirs"
}

# Allow direct execution.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    keygen_all
fi
