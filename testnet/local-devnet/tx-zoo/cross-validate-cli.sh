#!/usr/bin/env bash
# Cross-validate dugite-cli vs cardano-cli at the submit layer.
#
# Strategy: for one representative tx per category (01..07), build + sign
# via cardano-cli, then submit via dugite-cli to dugite-bp's N2C socket.
# Verify the expected txid lands in the chain. This proves:
#   (1) dugite-cli's `transaction submit` works for each tx class
#   (2) dugite-cli's wire-format output is byte-identical to cardano-cli's
#       (otherwise the on-chain hash would differ from the file-derived
#       txid and the wait_inclusion check would time out)
#
# Only seven txs total — meant as a smoke-test, not a full re-run of the
# tx-zoo. Pre-conditions:
#   - ./run.sh up
#   - tx-zoo/run-all.sh --setup already ran (wallets funded, dreps exist)
#
# Usage: ./cross-validate-cli.sh
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/tx-zoo-common.sh"

DUGITE_CLI="${DUGITE_CLI:-$LD_REPO_ROOT/target/release/dugite-cli}"
[ -x "$DUGITE_CLI" ] || die "dugite-cli not found at $DUGITE_CLI — run 'cargo build --release'"

# Submit via dugite-cli against dugite-bp's socket (the producer's own N2C),
# then verify inclusion via cardano-cli query against the relay so the
# observation path is independent of the submit path.
SUBMIT_SOCK="$LD_DUGITE_BP_SOCK"
OBSERVE_SOCK="$LD_RELAY_SOCK"

XV_LOGS="$ZOO_STATE/cross-validate"
mkdir -p "$XV_LOGS"

XV_RESULTS="$ZOO_STATE/cross-validate.csv"
echo "ts,name,status,txid,detail" > "$XV_RESULTS"

xv_record() {
    local name="$1" status="$2" txid="${3:-}" detail="${4:-}"
    printf '%s,%s,%s,%s,%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$name" "$status" "${txid:-}" "${detail//,/;}" \
        >> "$XV_RESULTS"
}

# Submit signed tx via dugite-cli; print txid on success.
xv_submit_dugite() {
    local signed="$1" txid out rc
    txid=$(cardano-cli conway transaction txid --tx-file "$signed" --output-text)
    out=$("$DUGITE_CLI" transaction submit \
            --testnet-magic "$LD_MAGIC" \
            --socket-path   "$SUBMIT_SOCK" \
            --tx-file       "$signed" 2>&1) && rc=0 || rc=$?
    if [ "$rc" -ne 0 ]; then
        zoo_fail "dugite-cli submit failed ($txid): $out"
        return 1
    fi
    echo "$txid"
}

# Wait up to $timeout for a UTxO whose key starts with $txid# to appear at
# $addr (queried via cardano-cli against the observe socket).
xv_wait_inclusion() {
    local txid="$1" addr="$2" timeout="${3:-90}" sock="${4:-$OBSERVE_SOCK}"
    local i=0
    while [ "$i" -lt "$timeout" ]; do
        local hit
        hit=$(cardano-cli conway query utxo \
                --testnet-magic "$LD_MAGIC" \
                --socket-path "$sock" \
                --address "$addr" \
                --output-json 2>/dev/null \
              | jq --arg t "$txid" '[keys[] | select(startswith($t))] | length' 2>/dev/null \
              || echo 0)
        if [ "${hit:-0}" -ge 1 ]; then
            return 0
        fi
        sleep 1
        i=$((i+1))
    done
    return 1
}

# ---- Per-category cases ----------------------------------------------------

# 01 — bookkeeping: simple pay from wallet-a to wallet-b
xv_01_simple_pay() {
    local name="xv-01-simple-pay"
    local WA="$ZOO_KEYS/wallet-a" WB="$ZOO_KEYS/wallet-b"
    local from_addr to_addr utxo txin raw signed txid
    from_addr=$(cat "$WA/payment-stake.addr")
    to_addr=$(cat "$WB/payment-stake.addr")
    utxo=$(zoo_largest_utxo "$from_addr") || { xv_record "$name" FAIL "" "no-utxo"; return 1; }
    txin=${utxo%% *}
    raw="$ZOO_BUILT/$name.raw"
    signed="$ZOO_BUILT/$name.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$txin" \
        --tx-out "${to_addr}+1500000" \
        --change-address "$from_addr" \
        --out-file "$raw" >/dev/null 2> "$XV_LOGS/$name-build.err" \
        || { xv_record "$name" FAIL "" "build"; return 1; }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" --signing-key-file "$WA/payment.skey" \
        --out-file "$signed" >/dev/null
    txid=$(xv_submit_dugite "$signed") || { xv_record "$name" FAIL "" "submit"; return 1; }
    xv_wait_inclusion "$txid" "$to_addr" 90 \
        && xv_record "$name" PASS "$txid" "via=dugite-cli" \
        || { xv_record "$name" FAIL "$txid" "not-included"; return 1; }
}

# 02 — native script mint (allowing wallet-a's payment vkey).
xv_02_mint() {
    local name="xv-02-mint-native"
    local WA="$ZOO_KEYS/wallet-a"
    local from_addr utxo txin raw signed txid policy script asset
    from_addr=$(cat "$WA/payment-stake.addr")
    local key_hash; key_hash=$(cardano-cli conway address key-hash \
        --payment-verification-key-file "$WA/payment.vkey")
    script="$ZOO_BUILT/$name.script"
    cat > "$script" <<EOF
{ "type": "sig", "keyHash": "$key_hash" }
EOF
    policy=$(cardano-cli conway transaction policyid --script-file "$script")
    asset="${policy}.787663726f7373"  # "xvcross"
    utxo=$(zoo_largest_utxo "$from_addr") || { xv_record "$name" FAIL "" "no-utxo"; return 1; }
    txin=${utxo%% *}
    raw="$ZOO_BUILT/$name.raw"
    signed="$ZOO_BUILT/$name.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$txin" \
        --tx-out "${from_addr}+2000000 + 1 ${asset}" \
        --mint "1 ${asset}" \
        --mint-script-file "$script" \
        --change-address "$from_addr" \
        --out-file "$raw" >/dev/null 2> "$XV_LOGS/$name-build.err" \
        || { xv_record "$name" FAIL "" "build"; return 1; }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" --signing-key-file "$WA/payment.skey" \
        --out-file "$signed" >/dev/null
    txid=$(xv_submit_dugite "$signed") || { xv_record "$name" FAIL "" "submit"; return 1; }
    xv_wait_inclusion "$txid" "$from_addr" 90 \
        && xv_record "$name" PASS "$txid" "policy=${policy:0:16}" \
        || { xv_record "$name" FAIL "$txid" "not-included"; return 1; }
}

# 03 — Plutus V3 lock-then-spend (split into two txs is heavy; we just
# spend an already-locked output by re-running 03c's lock+spend pattern
# inline). Lock to V3 always-true, then spend with empty redeemer.
xv_03_plutus_spend() {
    local name="xv-03-plutus-spend-v3"
    local WA="$ZOO_KEYS/wallet-a"
    # `-spend`, NOT the bare name. Since #969/#970 the legacy aliases point at
    # the upstream plutus-tx scripts, where `always-true-v3.plutus` is
    # `alwaysSucceedsNoDatum` — TRUE for every purpose EXCEPT spending with a
    # datum. This case locks with an inline datum, so it needs
    # `alwaysSucceedsWithDatum` or the spend fails phase-2 with PT5.
    #
    # The #969/#970 sweep fixed every call site inside a zoo CATEGORY and
    # missed this file, because cross-validate is not in `ALL_CATEGORIES` and
    # so is not covered by run-all.sh's drift guard.
    local plutus="$SCRIPT_DIR/lib/plutus/always-true-v3-spend.plutus"
    [ -s "$plutus" ] || { xv_record "$name" FAIL "" "no-plutus"; return 1; }
    local pay_addr; pay_addr=$(cat "$WA/payment-stake.addr")
    local script_addr; script_addr=$(cardano-cli conway address build \
        --payment-script-file "$plutus" --testnet-magic "$LD_MAGIC")

    # Step 1: lock 5 ADA at the script with an inline-datum.
    local lock_utxo lock_in lock_raw lock_signed lock_txid
    lock_utxo=$(zoo_largest_utxo "$pay_addr") || { xv_record "$name" FAIL "" "no-utxo"; return 1; }
    lock_in=${lock_utxo%% *}
    lock_raw="$ZOO_BUILT/$name-lock.raw"
    lock_signed="$ZOO_BUILT/$name-lock.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$lock_in" \
        --tx-out "${script_addr}+5000000" \
        --tx-out-inline-datum-value '42' \
        --change-address "$pay_addr" \
        --out-file "$lock_raw" >/dev/null 2> "$XV_LOGS/$name-lock-build.err" \
        || { xv_record "$name" FAIL "" "lock-build"; return 1; }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$lock_raw" --signing-key-file "$WA/payment.skey" \
        --out-file "$lock_signed" >/dev/null
    # Lock submission is via cardano-cli — we're not testing that one here.
    lock_txid=$(zoo_submit "$lock_signed" "$ZOO_SOCKET") || { xv_record "$name" FAIL "" "lock-submit"; return 1; }
    zoo_wait_inclusion "$lock_txid" 90 "$script_addr" \
        || { xv_record "$name" FAIL "$lock_txid" "lock-not-incl"; return 1; }

    # Step 2: spend it back to wallet-a with empty redeemer + collateral
    # (picked from the genesis collateral pool).
    local genesis_addr; genesis_addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local coll
    coll=$(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$genesis_addr" --output-json \
        | jq -r 'to_entries[] | select(.value.value.lovelace >= 10000000 and .value.value.lovelace <= 100000000)
                              | select((.value.inlineDatum // null) == null and (.value.datumhash // null) == null
                                     and (.value.referenceScript // null) == null)
                              | .key' | head -n 1)
    [ -n "$coll" ] || { xv_record "$name" FAIL "" "no-collateral"; return 1; }

    # Find the locked output: its txid is $lock_txid, ix is the script output (usually #0).
    local spend_in
    spend_in=$(cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --address "$script_addr" --output-json \
        | jq --arg t "$lock_txid" -r 'to_entries[] | select(.key | startswith($t)) | .key' | head -n 1)
    [ -n "$spend_in" ] || { xv_record "$name" FAIL "$lock_txid" "no-script-utxo"; return 1; }

    local spend_raw="$ZOO_BUILT/$name-spend.raw"
    local spend_signed="$ZOO_BUILT/$name-spend.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$spend_in" \
        --tx-in-script-file "$plutus" \
        --tx-in-inline-datum-present \
        --tx-in-redeemer-value '0' \
        --tx-in-collateral "$coll" \
        --change-address "$pay_addr" \
        --out-file "$spend_raw" >/dev/null 2> "$XV_LOGS/$name-spend-build.err" \
        || { xv_record "$name" FAIL "" "spend-build"; return 1; }
    # The collateral input is picked from the genesis utxo address, so the
    # spend tx requires a vkey witness for BOTH wallet-a's payment key
    # (collateral change return + script context) AND the genesis payment
    # key (which owns the collateral input). Without the genesis key, the
    # tx fails `MissingVKeyWitnessesUTXOW` at admission (post the dugite
    # collateral-witness fix in commit 0821af5d2; previously this slipped
    # through dugite's mempool but cardano-bp rejected on apply).
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$spend_raw" \
        --signing-key-file "$WA/payment.skey" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --out-file "$spend_signed" >/dev/null

    local spend_txid; spend_txid=$(xv_submit_dugite "$spend_signed") \
        || { xv_record "$name" FAIL "" "spend-submit"; return 1; }
    xv_wait_inclusion "$spend_txid" "$pay_addr" 120 \
        && xv_record "$name" PASS "$spend_txid" "v3-spend" \
        || { xv_record "$name" FAIL "$spend_txid" "spend-not-incl"; return 1; }
}

# 04 — stake address register (on a fresh stake key so it can't already exist).
xv_04_stake_register() {
    local name="xv-04-stake-register"
    local WA="$ZOO_KEYS/wallet-a"
    local dir="$ZOO_BUILT/$name-stake"
    mkdir -p "$dir"
    # Fresh stake key dedicated to this test.
    if [ ! -s "$dir/stake.skey" ]; then
        cardano-cli conway stake-address key-gen \
            --verification-key-file "$dir/stake.vkey" \
            --signing-key-file      "$dir/stake.skey"
    fi
    local pparams; pparams=$(zoo_pparams_file)
    local deposit; deposit=$(jq -r '.stakeAddressDeposit // .stakeAddrDeposit // 2000000' "$pparams")
    local cert="$ZOO_BUILT/$name.cert"
    cardano-cli conway stake-address registration-certificate \
        --stake-verification-key-file "$dir/stake.vkey" \
        --key-reg-deposit-amt "$deposit" \
        --out-file "$cert"
    local from_addr; from_addr=$(cat "$WA/payment-stake.addr")
    local utxo; utxo=$(zoo_largest_utxo "$from_addr") || { xv_record "$name" FAIL "" "no-utxo"; return 1; }
    local txin=${utxo%% *}
    local raw="$ZOO_BUILT/$name.raw"
    local signed="$ZOO_BUILT/$name.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$txin" --change-address "$from_addr" \
        --certificate-file "$cert" \
        --out-file "$raw" >/dev/null 2> "$XV_LOGS/$name-build.err" \
        || { xv_record "$name" FAIL "" "build"; return 1; }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" \
        --signing-key-file "$WA/payment.skey" \
        --signing-key-file "$dir/stake.skey" \
        --out-file "$signed" >/dev/null
    local txid; txid=$(xv_submit_dugite "$signed") || { xv_record "$name" FAIL "" "submit"; return 1; }
    xv_wait_inclusion "$txid" "$from_addr" 90 \
        && xv_record "$name" PASS "$txid" "stake-reg" \
        || { xv_record "$name" FAIL "$txid" "not-included"; return 1; }
}

# 05 — drep-register (uses drep-3 because the suite's 05c-drep-deregister
# leaves drep-3 retired by the end of the run, so we can re-register it
# here without hitting "already-registered").
xv_05_drep_register() {
    local name="xv-05-drep-register"
    local DREP="$ZOO_KEYS/drep-3"
    local WB="$ZOO_KEYS/wallet-b"
    local from_addr; from_addr=$(cat "$WB/payment-stake.addr")
    local drep_kh; drep_kh=$(cardano-cli conway governance drep id \
        --drep-verification-key-file "$DREP/drep.vkey" --output-hex)
    local already
    already=$(cardano-cli conway query drep-state \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --drep-key-hash "$drep_kh" 2>/dev/null || echo "[]")
    if echo "$already" | jq -e 'length>0' >/dev/null; then
        xv_record "$name" SKIP "" "already-registered"
        return 0
    fi
    local pparams; pparams=$(zoo_pparams_file)
    local deposit; deposit=$(jq -r '.dRepDeposit // .drepDeposit // 500000000' "$pparams")
    local cert="$ZOO_BUILT/$name.cert"
    cardano-cli conway governance drep registration-certificate \
        --drep-verification-key-file "$DREP/drep.vkey" \
        --key-reg-deposit-amt "$deposit" \
        --out-file "$cert"
    local utxo; utxo=$(zoo_largest_utxo "$from_addr") || { xv_record "$name" FAIL "" "no-utxo"; return 1; }
    local txin=${utxo%% *}
    local raw="$ZOO_BUILT/$name.raw"
    local signed="$ZOO_BUILT/$name.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$txin" --change-address "$from_addr" \
        --certificate-file "$cert" \
        --out-file "$raw" >/dev/null 2> "$XV_LOGS/$name-build.err" \
        || { xv_record "$name" FAIL "" "build"; return 1; }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" \
        --signing-key-file "$WB/payment.skey" \
        --signing-key-file "$DREP/drep.skey" \
        --out-file "$signed" >/dev/null
    local txid; txid=$(xv_submit_dugite "$signed") || { xv_record "$name" FAIL "" "submit"; return 1; }
    xv_wait_inclusion "$txid" "$from_addr" 90 \
        && xv_record "$name" PASS "$txid" "drep=${drep_kh:0:16}" \
        || { xv_record "$name" FAIL "$txid" "not-included"; return 1; }
}

# Register a wallet's stake credential if the chain does not already know it.
# Idempotent: a no-op when `stake-address-info` already returns a row.
xv_ensure_stake_registered() {
    local dir="$1" from_addr="$2"
    local stake_addr; stake_addr=$(cat "$dir/stake.addr" 2>/dev/null) || return 1
    local info
    info=$(cardano-cli conway query stake-address-info \
               --address "$stake_addr" --testnet-magic "$LD_MAGIC" \
               --socket-path "$LD_DUGITE_BP_SOCK" 2>/dev/null)
    [ "$(printf '%s' "$info" | jq 'length' 2>/dev/null || echo 0)" -gt 0 ] && return 0

    local pparams; pparams=$(zoo_pparams_file)
    local dep; dep=$(jq -r '.stakeAddressDeposit // .stakeAddrDeposit // 2000000' "$pparams")
    local cert="$ZOO_BUILT/xv-stake-precondition.cert"
    cardano-cli conway stake-address registration-certificate \
        --stake-verification-key-file "$dir/stake.vkey" \
        --key-reg-deposit-amt "$dep" \
        --out-file "$cert" >/dev/null 2>&1 || return 1
    local utxo; utxo=$(zoo_largest_utxo "$from_addr") || return 1
    local txin=${utxo%% *}
    local raw="$ZOO_BUILT/xv-stake-precondition.raw"
    local signed="$ZOO_BUILT/xv-stake-precondition.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$txin" --change-address "$from_addr" \
        --certificate-file "$cert" \
        --out-file "$raw" >/dev/null 2> "$XV_LOGS/xv-stake-precondition-build.err" \
        || return 1
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" --tx-body-file "$raw" \
        --signing-key-file "$dir/payment.skey" \
        --signing-key-file "$dir/stake.skey" \
        --out-file "$signed" >/dev/null 2>&1 || return 1
    local txid; txid=$(xv_submit_dugite "$signed") || return 1
    xv_wait_inclusion "$txid" "$from_addr" 90
}

# 06 — info-action proposal. `deposit-return-stake-verification-key-file` must
# reference a stake credential that is registered ON-CHAIN, so this registers
# it first if it is not.
#
# It used to assume 04a had done so and that nothing deregisters it. That
# assumption was false in practice — a full zoo run leaves wallet-a's stake key
# unregistered — and it failed as a cardano-cli build error rather than an
# assertion, so it read as a dugite defect rather than a missing precondition.
# Depending on another suite's leftover state is the bug; checking is the fix.
xv_06_info_proposal() {
    local name="xv-06-info-action"
    local WA="$ZOO_KEYS/wallet-a"
    local from_addr; from_addr=$(cat "$WA/payment-stake.addr")
    xv_ensure_stake_registered "$WA" "$from_addr" \
        || { xv_record "$name" FAIL "" "stake-register-precondition"; return 1; }
    local pparams; pparams=$(zoo_pparams_file)
    local deposit; deposit=$(jq -r '.govActionDeposit // .govDeposit // 100000000000' "$pparams")
    local action="$ZOO_BUILT/$name.action"
    local anchor_url; anchor_url=$(zoo_anchor_url info-action)
    local anchor_hash; anchor_hash=$(zoo_anchor_hash info-action)
    cardano-cli conway governance action create-info \
        --testnet \
        --governance-action-deposit "$deposit" \
        --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
        --anchor-url "$anchor_url" \
        --anchor-data-hash "$anchor_hash" \
        --out-file "$action"
    local utxo; utxo=$(zoo_largest_utxo "$from_addr") || { xv_record "$name" FAIL "" "no-utxo"; return 1; }
    local txin=${utxo%% *}
    local raw="$ZOO_BUILT/$name.raw"
    local signed="$ZOO_BUILT/$name.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$txin" --change-address "$from_addr" \
        --proposal-file "$action" \
        --out-file "$raw" >/dev/null 2> "$XV_LOGS/$name-build.err" \
        || { xv_record "$name" FAIL "" "build"; return 1; }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" \
        --signing-key-file "$WA/payment.skey" \
        --out-file "$signed" >/dev/null
    local txid; txid=$(xv_submit_dugite "$signed") || { xv_record "$name" FAIL "" "submit"; return 1; }
    xv_wait_inclusion "$txid" "$from_addr" 120 \
        && xv_record "$name" PASS "$txid" "info-action" \
        || { xv_record "$name" FAIL "$txid" "not-included"; return 1; }
}

# 07 — drep-1 votes YES on the info action we just submitted.
# We need to know the proposal txid from 06 — we read it from the latest
# xv_06 entry in $XV_RESULTS to keep the script self-contained.
xv_07_drep_vote() {
    local name="xv-07-drep-vote-yes"
    local action_txid
    action_txid=$(awk -F, '/xv-06-info-action/ && $3=="PASS" {print $4}' "$XV_RESULTS" | tail -1)
    if [ -z "$action_txid" ]; then
        xv_record "$name" SKIP "" "no-prior-action"
        return 0
    fi
    local DREP="$ZOO_KEYS/drep-1"
    local WA="$ZOO_KEYS/wallet-a"
    local from_addr; from_addr=$(cat "$WA/payment-stake.addr")
    # Ensure drep-1 is registered (05a does this in the suite — for the
    # standalone path, skip if not).
    local drep_kh; drep_kh=$(cardano-cli conway governance drep id \
        --drep-verification-key-file "$DREP/drep.vkey" --output-hex)
    local reg
    reg=$(cardano-cli conway query drep-state \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --drep-key-hash "$drep_kh" 2>/dev/null || echo "[]")
    if ! echo "$reg" | jq -e 'length>0' >/dev/null; then
        xv_record "$name" SKIP "" "drep-1-not-registered"
        return 0
    fi
    local vote="$ZOO_BUILT/$name.vote"
    cardano-cli conway governance vote create \
        --yes \
        --governance-action-tx-id "$action_txid" \
        --governance-action-index 0 \
        --drep-verification-key-file "$DREP/drep.vkey" \
        --out-file "$vote"
    local utxo; utxo=$(zoo_largest_utxo "$from_addr") || { xv_record "$name" FAIL "" "no-utxo"; return 1; }
    local txin=${utxo%% *}
    local raw="$ZOO_BUILT/$name.raw"
    local signed="$ZOO_BUILT/$name.signed"
    cardano-cli conway transaction build \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-in "$txin" --change-address "$from_addr" \
        --vote-file "$vote" \
        --out-file "$raw" >/dev/null 2> "$XV_LOGS/$name-build.err" \
        || { xv_record "$name" FAIL "" "build"; return 1; }
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$raw" \
        --signing-key-file "$WA/payment.skey" \
        --signing-key-file "$DREP/drep.skey" \
        --out-file "$signed" >/dev/null
    local txid; txid=$(xv_submit_dugite "$signed") || { xv_record "$name" FAIL "" "submit"; return 1; }
    xv_wait_inclusion "$txid" "$from_addr" 90 \
        && xv_record "$name" PASS "$txid" "drep-vote-yes" \
        || { xv_record "$name" FAIL "$txid" "not-included"; return 1; }
}

# ---- Driver ----------------------------------------------------------------

zoo_require_devnet
[ -S "$SUBMIT_SOCK" ] || die "submit socket $SUBMIT_SOCK missing"
zoo_anchor_start
trap 'zoo_anchor_stop' EXIT

zoo_info "=== Cross-validation: dugite-cli transaction submit ==="
zoo_info "submit socket: $SUBMIT_SOCK (dugite-bp)"
zoo_info "observe socket: $OBSERVE_SOCK (relay)"
zoo_info "dugite-cli: $DUGITE_CLI"

xv_01_simple_pay  || true
xv_02_mint        || true
xv_03_plutus_spend|| true
xv_04_stake_register || true
xv_05_drep_register  || true
xv_06_info_proposal  || true
xv_07_drep_vote   || true

echo
echo "=== cross-validate summary ==="
local_total=$(tail -n +2 "$XV_RESULTS" | wc -l | tr -d ' ')
local_pass=$( awk -F, 'NR>1 && $3=="PASS"' "$XV_RESULTS" | wc -l | tr -d ' ')
local_fail=$( awk -F, 'NR>1 && $3=="FAIL"' "$XV_RESULTS" | wc -l | tr -d ' ')
local_skip=$( awk -F, 'NR>1 && $3=="SKIP"' "$XV_RESULTS" | wc -l | tr -d ' ')
printf '  total=%d  pass=%d  fail=%d  skip=%d\n' \
    "$local_total" "$local_pass" "$local_fail" "$local_skip"
echo
awk -F, 'NR>1 { printf "  %-32s %-5s %s\n", $2, $3, $5 }' "$XV_RESULTS"

[ "$local_fail" -eq 0 ] || exit 1
