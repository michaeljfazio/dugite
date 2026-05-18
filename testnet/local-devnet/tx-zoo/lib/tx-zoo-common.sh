#!/usr/bin/env bash
# Shared helpers for the tx-zoo. Sourced by every script in the zoo.
# Builds on testnet/local-devnet/lib/common.sh (paths, sockets, magic, helpers).
set -euo pipefail

ZOO_SELF="${BASH_SOURCE[0]:-$0}"
ZOO_LIB="$(cd "$(dirname "$ZOO_SELF")" && pwd)"
ZOO_ROOT="$(cd "$ZOO_LIB/.." && pwd)"

. "$ZOO_ROOT/../lib/common.sh"

# Per-tx working dir under the zoo state so artifacts persist across reruns.
ZOO_STATE="${ZOO_STATE:-$ZOO_ROOT/state}"
ZOO_KEYS="$ZOO_STATE/keys"
ZOO_BUILT="$ZOO_STATE/built"
ZOO_LOGS="$ZOO_STATE/logs"
mkdir -p "$ZOO_STATE" "$ZOO_KEYS" "$ZOO_BUILT" "$ZOO_LOGS"

# Default socket: prefer relay (everyone connects to it). Override with
# ZOO_SOCKET to target dugite-bp or cardano-bp directly.
ZOO_SOCKET="${ZOO_SOCKET:-$LD_RELAY_SOCK}"

# Default funding key — the genesis utxo key. tx-zoo scripts spend from this.
ZOO_PAY_ADDR_FILE="$LD_KEYS/utxo/payment.addr"
ZOO_PAY_SKEY="$LD_KEYS/utxo/payment.skey"
ZOO_PAY_VKEY="$LD_KEYS/utxo/payment.vkey"

# ---- Logging shorthand ----
zoo_info()  { printf '\033[0;36m[ZOO]\033[0m   %s\n' "$*" >&2; }
zoo_ok()    { printf '\033[0;32m[ZOO OK]\033[0m %s\n' "$*" >&2; }
zoo_fail()  { printf '\033[0;31m[ZOO FAIL]\033[0m %s\n' "$*" >&2; }
zoo_skip()  { printf '\033[0;33m[ZOO SKIP]\033[0m %s\n' "$*" >&2; }

# Identify the calling script for logs/output naming.
zoo_name() {
    local s="${1:-${BASH_SOURCE[1]:-${0}}}"
    basename "$s" .sh
}

# ---- Devnet liveness ----
zoo_require_devnet() {
    [ -S "$ZOO_SOCKET" ] || die "tx-zoo: socket not present at $ZOO_SOCKET — run ./run.sh"
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" >/dev/null 2>&1 \
        || die "tx-zoo: tip query failed on $ZOO_SOCKET"
}

# Current tip slot, useful for TTL / validity-interval choices.
zoo_tip_slot() {
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        | jq -r '.slot'
}

zoo_tip_epoch() {
    cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        | jq -r '.epoch'
}

# ---- UTxO selection ----
# Print the largest-lovelace UTxO at $addr as "<txin> <lovelace>".
zoo_largest_utxo() {
    local addr="$1" sock="${2:-$ZOO_SOCKET}"
    local tmp
    tmp="$(mktemp)"
    cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$sock" \
        --address       "$addr" \
        --out-file      "$tmp"
    local line
    line=$(jq -r 'to_entries | sort_by(-.value.value.lovelace) | .[0] | "\(.key) \(.value.value.lovelace)"' "$tmp")
    rm -f "$tmp"
    if [ -z "$line" ] || [ "$line" = "null null" ]; then
        return 1
    fi
    echo "$line"
}

# Print the Nth-largest UTxO (0-indexed) — useful when scripts share a wallet
# and need disjoint inputs.
zoo_utxo_at() {
    local addr="$1" idx="$2" sock="${3:-$ZOO_SOCKET}"
    local tmp
    tmp="$(mktemp)"
    cardano-cli conway query utxo \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$sock" \
        --address       "$addr" \
        --out-file      "$tmp"
    local line
    line=$(jq -r --argjson i "$idx" \
        'to_entries | sort_by(-.value.value.lovelace) | .[$i] | "\(.key) \(.value.value.lovelace)"' \
        "$tmp")
    rm -f "$tmp"
    if [ -z "$line" ] || [ "$line" = "null null" ]; then
        return 1
    fi
    echo "$line"
}

# ---- Submit + wait ----
# Submit a signed tx file; print the txid on success, return non-zero on
# submit error. Optional follow-up wait for inclusion at $sock.
zoo_submit() {
    local signed="$1" sock="${2:-$ZOO_SOCKET}"
    local txid
    txid=$(cardano-cli conway transaction txid --tx-file "$signed" --output-text 2>/dev/null) \
        || die "tx-zoo: failed to compute txid for $signed"
    local err
    err="$(cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" \
            --socket-path   "$sock" \
            --tx-file       "$signed" 2>&1)" \
        || { zoo_fail "submit rejected ($txid): $err"; return 1; }
    echo "$txid"
}

# Wait up to $timeout seconds for the change UTxO carrying $txid to appear at
# any of the 3 devnet observers. Returns 0 when seen everywhere.
zoo_wait_inclusion() {
    local txid="$1" timeout="${2:-60}"
    local addr; addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local i=0
    while [ "$i" -lt "$timeout" ]; do
        local n=0
        for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
            [ -S "$sock" ] || continue
            local hit
            hit=$(cardano-cli conway query utxo \
                    --testnet-magic "$LD_MAGIC" \
                    --socket-path "$sock" \
                    --address "$addr" \
                    --output-json 2>/dev/null \
                  | jq --arg t "$txid" '[keys[] | select(startswith($t))] | length' 2>/dev/null \
                  || echo 0)
            [ "${hit:-0}" -ge 1 ] && n=$((n+1))
        done
        if [ "$n" -ge 1 ]; then
            zoo_ok "tx $txid seen on $n/3 observers after ${i}s"
            return 0
        fi
        sleep 1
        i=$((i+1))
    done
    zoo_fail "tx $txid not visible on any observer after ${timeout}s"
    return 1
}

# Wait for a given tx to land on the canonical chain (i.e., all three observers
# agree it's in their UTxO). Stricter than zoo_wait_inclusion.
zoo_wait_all_observers() {
    local txid="$1" timeout="${2:-120}"
    local addr; addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local i=0
    while [ "$i" -lt "$timeout" ]; do
        local n=0
        for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
            [ -S "$sock" ] || continue
            local hit
            hit=$(cardano-cli conway query utxo \
                    --testnet-magic "$LD_MAGIC" \
                    --socket-path "$sock" \
                    --address "$addr" \
                    --output-json 2>/dev/null \
                  | jq --arg t "$txid" '[keys[] | select(startswith($t))] | length' 2>/dev/null \
                  || echo 0)
            [ "${hit:-0}" -ge 1 ] && n=$((n+1))
        done
        if [ "$n" -ge 3 ]; then
            zoo_ok "tx $txid on all 3 observers after ${i}s"
            return 0
        fi
        sleep 1
        i=$((i+1))
    done
    zoo_fail "tx $txid only on $n/3 observers after ${timeout}s"
    return 1
}

# ---- Protocol params snapshot ----
zoo_pparams_file() {
    local f="$ZOO_BUILT/pparams.json"
    cardano-cli conway query protocol-parameters \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --out-file      "$f"
    echo "$f"
}

# ---- Result recording ----
# Append a line to the central run-all results CSV.
ZOO_RESULTS_CSV="${ZOO_RESULTS_CSV:-$ZOO_STATE/results.csv}"
zoo_record() {
    local name="$1" status="$2" txid="${3:-}" detail="${4:-}"
    [ ! -f "$ZOO_RESULTS_CSV" ] && echo "ts,name,status,txid,detail" > "$ZOO_RESULTS_CSV"
    printf '%s,%s,%s,%s,%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$name" "$status" "${txid:-}" "${detail//,/;}" \
        >> "$ZOO_RESULTS_CSV"
}

# ---- Negative-test helper ----
# Runs the given command, expecting NON-ZERO exit OR a recognised error keyword.
# Pass FAIL to mean PASS for negative tests.
zoo_expect_failure() {
    local desc="$1"; shift
    local out rc
    out="$("$@" 2>&1)" && rc=0 || rc=$?
    if [ "$rc" -ne 0 ] || echo "$out" | grep -qE 'invalid|error|reject|fail' ; then
        zoo_ok "$desc — rejected as expected (rc=$rc)"
        return 0
    fi
    zoo_fail "$desc — UNEXPECTED success: $out"
    return 1
}

ZOO_COMMON_LOADED=1
