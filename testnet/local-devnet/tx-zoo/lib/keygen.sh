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

# Script-credential stake wallet (#955).
#
# The zoo's certificates, withdrawals and votes were ALL key-credentialed, so
# the Certifying / Rewarding / Voting ScriptPurposes — each a distinct
# ScriptContext construction path and a distinct redeemer-pointer tag on the
# wire — were never constructed at all. This builds a wallet whose PAYMENT part
# is an ordinary key (so it can pay fees and be spent normally) and whose STAKE
# part is a script credential, which is what makes the script the subject of a
# certificate / withdrawal / vote.
#
#   gen_script_stake <name> <script-file>
#
# Writes into $ZOO_KEYS/<name>/:
#   payment.{skey,vkey}     ordinary payment key
#   stake-script.plutus     copy of the guarding script (so tests need one path)
#   stake-script.hash       its script hash
#   stake.addr              stake address built from the SCRIPT credential
#   payment-stake.addr      base address: key payment + script stake
gen_script_stake() {
    local name="$1" script_file="$2"
    local dir="$ZOO_KEYS/$name"
    mkdir -p "$dir"
    gen_payment "$name"

    [ -s "$script_file" ] || die "gen_script_stake: script not found: $script_file"

    # Re-derive EVERYTHING whenever the source script changes.
    #
    # These four artefacts are all functions of the script bytes, and caching
    # them independently of those bytes is how a stale credential survives a
    # toolchain change: swap the .plutus binary (as #969/#970 did, from Aiken's
    # always-true to upstream's plutus-tx `alwaysSucceedsNoDatum`) and the
    # cached stake.addr still points at the OLD script hash, so every
    # certificate and withdrawal fails with a witness mismatch that looks
    # exactly like a dugite bug. Compare bytes, not existence.
    if ! cmp -s "$script_file" "$dir/stake-script.plutus"; then
        rm -f "$dir/stake-script.plutus" "$dir/stake-script.hash" \
              "$dir/stake.addr" "$dir/payment-stake.addr"
        cp "$script_file" "$dir/stake-script.plutus"
    fi

    # Hash flavour must match the script flavour: a Plutus envelope hashes with
    # `transaction policyid --script-file`; a native script uses the same
    # command but the resulting hash is a native-script hash. cardano-cli picks
    # the right one off the envelope type, so one call covers both.
    if [ ! -s "$dir/stake-script.hash" ]; then
        cardano-cli conway transaction policyid \
            --script-file "$dir/stake-script.plutus" > "$dir/stake-script.hash"
    fi

    if [ ! -s "$dir/stake.addr" ]; then
        cardano-cli conway stake-address build \
            --stake-script-file "$dir/stake-script.plutus" \
            --testnet-magic "$LD_MAGIC" \
            --out-file "$dir/stake.addr"
    fi

    if [ ! -s "$dir/payment-stake.addr" ]; then
        cardano-cli conway address build \
            --payment-verification-key-file "$dir/payment.vkey" \
            --stake-script-file             "$dir/stake-script.plutus" \
            --testnet-magic "$LD_MAGIC" \
            --out-file "$dir/payment-stake.addr"
    fi
    zoo_info "  script-stake wallet: $name (cred $(cut -c1-16 < "$dir/stake-script.hash")…)"
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
        # Always refresh from devnet — setup.sh regenerates the system CC keys
        # on each run, so the tx-zoo copies become stale after re-setup. Without
        # refreshing, the zoo's CC keys diverge from the seated committee and
        # mempool admission correctly rejects CommitteeHotAuth/vote certs that
        # name unelected cold keys (see #551).
        local devnet_hash
        devnet_hash=$(cardano-cli conway governance committee key-hash \
            --verification-key-file "$devnet_dir/cc-cold.vkey" 2>/dev/null || true)
        local zoo_hash=""
        if [ -s "$dir/cc-cold.vkey" ]; then
            zoo_hash=$(cardano-cli conway governance committee key-hash \
                --verification-key-file "$dir/cc-cold.vkey" 2>/dev/null || true)
        fi
        if [ "$devnet_hash" != "$zoo_hash" ]; then
            zoo_info "  CC keys: syncing $name from devnet ($devnet_dir)"
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

# Pre-split the genesis payment address into many small UTxOs so the
# Plutus tests (03a..03h, 03j) always find a spare UTxO for collateral.
#
# Without this step the addr holds exactly ONE UTxO (the genesis fund)
# and each lock tx consumes it + emits a single change output, leaving
# the addr with only one UTxO again. `plutus_collateral` looks at the
# second-largest UTxO and fails with "no spare UTxO".
#
# We emit $count outputs of $each lovelace each, all at the genesis
# addr. Idempotent: if there are already $count or more "small"
# UTxOs (lovelace < threshold), skip.
prefund_collateral_pool() {
    local count="${1:-30}" each="${2:-50000000}"
    local addr; addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local tmp; tmp=$(mktemp)
    cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --address       "$addr" \
        --output-json > "$tmp"
    # Count UTxOs with lovelace at-or-below $each*2 (collateral-shaped).
    local existing
    existing=$(jq --argjson e "$each" '[.[] | select(.value.lovelace <= ($e * 2))] | length' "$tmp")
    if [ "${existing:-0}" -ge "$count" ]; then
        zoo_info "  collateral pool already provisioned ($existing small UTxOs)"
        rm -f "$tmp"
        return 0
    fi
    zoo_info "  pre-splitting collateral pool: $count x $each lovelace at $addr"

    # Pick the largest UTxO as the funding input.
    local in; in=$(jq -r 'to_entries | sort_by(-.value.value.lovelace) | .[0].key' "$tmp")
    rm -f "$tmp"
    [ -z "$in" ] || [ "$in" = "null" ] && die "prefund_collateral_pool: no UTxO at $addr"

    local raw="$ZOO_BUILT/prefund-collateral.raw"
    local signed="$ZOO_BUILT/prefund-collateral.signed"
    # Build a tx with $count outputs of $each lovelace each, change
    # back to $addr. cardano-cli auto-balances.
    local outs=()
    local i=0
    while [ "$i" -lt "$count" ]; do
        outs+=(--tx-out "${addr}+${each}")
        i=$((i+1))
    done
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-in         "$in" \
        "${outs[@]}" \
        --change-address "$addr" \
        --out-file      "$raw" >/dev/null
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file  "$raw" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file      "$signed" >/dev/null
    local txid; txid=$(zoo_submit "$signed")
    zoo_wait_inclusion "$txid" 120 || die "prefund_collateral_pool: tx $txid not seen"
    zoo_info "  collateral pool ready ($count outputs in tx ${txid:0:16}…)"
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
    gen_cc   "cc-2"
    gen_pool "pool3"
    # Script-credential stake wallets for the non-spend/mint script purposes
    # (#955). always-true drives the positives; always-false is the negative
    # twin that must take the phase-2 collateral path; the native one proves
    # the same certificate/withdrawal surface works without Plutus at all.
    gen_script_stake "script-stake-v3"        "$ZOO_LIB/plutus/always-true-v3.plutus"
    gen_script_stake "script-stake-v3-false"  "$ZOO_LIB/plutus/always-false-v3.plutus"
    gen_script_stake "script-stake-native"    "$ZOO_LIB/native/always-true.native.json"
    # Fund the sub-wallets so they can submit their own txs. The amount has
    # to cover SEVEN gov-action deposits in series (`govActionDeposit` from
    # conway-genesis.json — 100 000 000 000 lovelace = 100 K ADA on the
    # default cardano-testnet output): one each for 06a..06g, all spent from
    # wallet-a's largest UTxO chain. We give wallet-a 1.5 M ADA so there's
    # comfortable headroom (7 × 100 K = 700 K) plus fees + zoo cross-
    # validation traffic. wallet-b is funded similarly because the cross-
    # validate-cli.sh script (and any future wallet-b-driven proposals)
    # spends through it too.
    fund_address "$ZOO_KEYS/wallet-a/payment-stake.addr" 1500000000000 1000000000000
    fund_address "$ZOO_KEYS/wallet-b/payment-stake.addr" 1500000000000 1000000000000
    # Script-stake wallets. Their PAYMENT credential is an ordinary key, so
    # they fund and spend normally; only the stake side is script-guarded.
    # 200 K ADA each covers the stake deposit, a DRep deposit, a gov-action
    # deposit and fees with headroom.
    for _w in script-stake-v3 script-stake-v3-false script-stake-native; do
        fund_address "$ZOO_KEYS/$_w/payment-stake.addr" 200000000000 100000000000
    done
    # Pre-split the genesis addr so plutus_collateral always finds a
    # spare UTxO (the 03 category locks + collateral pattern would
    # otherwise exhaust the single genesis UTxO).
    prefund_collateral_pool 30 50000000
    zoo_info "keygen complete — $(ls -1 "$ZOO_KEYS" | wc -l | tr -d ' ') sub-dirs"
}

# Allow direct execution.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    keygen_all
fi
