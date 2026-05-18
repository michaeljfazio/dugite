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
# the $ZOO_SOCKET (default relay).
#
# Args: $1=txid  [$2=timeout=60]  [$3=address=$ZOO_PAY_ADDR_FILE]
#
# The 3rd argument is required for any tx whose CHANGE address is NOT the
# funding genesis-utxo address — cert/governance/voting txs send change
# back to wallet-a or wallet-b, so passing the genesis address would
# always time out even when the tx made it into a block.
#
# We REQUIRE the relay socket (not BP / cardano-bp) because the next test
# will call zoo_largest_utxo against it; returning early on a different
# observer leaves a race where the relay still shows the old UTxO and
# the next tx picks the same input → mempool conflict → silent eviction
# with "not-included" 60s later. Returns 0 when seen at $ZOO_SOCKET.
zoo_wait_inclusion() {
    local txid="$1" timeout="${2:-60}" addr="${3:-}"
    [ -z "$addr" ] && addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local i=0
    while [ "$i" -lt "$timeout" ]; do
        local hit
        hit=$(cardano-cli conway query utxo \
                --testnet-magic "$LD_MAGIC" \
                --socket-path "$ZOO_SOCKET" \
                --address "$addr" \
                --output-json 2>/dev/null \
              | jq --arg t "$txid" '[keys[] | select(startswith($t))] | length' 2>/dev/null \
              || echo 0)
        if [ "${hit:-0}" -ge 1 ]; then
            zoo_ok "tx $txid seen at ZOO_SOCKET after ${i}s"
            return 0
        fi
        sleep 1
        i=$((i+1))
    done
    zoo_fail "tx $txid not visible at ZOO_SOCKET after ${timeout}s"
    return 1
}

# Wait for a given tx to land on all three observers (dugite-relay,
# dugite-bp, cardano-bp). Strongest available assertion: dugite forged
# the tx, the relay propagated it, AND the Haskell cardano-node accepts
# the block carrying it. This requires a non-forking topology — see
# setup.sh, which runs cardano-bp as a passive validator (no forging
# keys) so cardano-bp's chain is always a prefix or copy of dugite's.
#
# Args: $1=txid  [$2=timeout=120]  [$3=address=$ZOO_PAY_ADDR_FILE]
#
# Use this for tests where the change goes to a known wallet address
# other than the genesis funder (cert / governance / voting txs).
zoo_wait_all_observers() {
    local txid="$1" timeout="${2:-120}" addr="${3:-}"
    [ -z "$addr" ] && addr=$(cat "$ZOO_PAY_ADDR_FILE")
    local i=0 n=0
    while [ "$i" -lt "$timeout" ]; do
        n=0
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

# ---- Anchor-data HTTP server ----
#
# cardano-cli 11.0's `transaction build` and the cert-/proposal-create
# subcommands ALWAYS fetch `--anchor-url` (or `--drep-metadata-url`,
# `--metadata-url`) at build time and validate the downloaded content
# against the `--anchor-data-hash`. Placeholder URLs like
# `https://example.com/*` 404 and the script crashes pre-record.
#
# We stand up a tiny local HTTP server on $ZOO_ANCHOR_PORT that serves
# pre-generated JSON files under $ZOO_ANCHOR_DIR. The helpers below
# resolve to that server's URL+hash for a named anchor.
#
# Lifecycle: `zoo_anchor_start` is invoked once by run-all.sh before any
# script runs; `zoo_anchor_stop` is invoked at the end. Individual
# scripts call only `zoo_anchor_url` and `zoo_anchor_hash`.
ZOO_ANCHOR_PORT="${ZOO_ANCHOR_PORT:-18019}"
ZOO_ANCHOR_DIR="$ZOO_STATE/anchor"
ZOO_ANCHOR_PID="$ZOO_STATE/anchor.pid"

# Compute the Blake2b-256 hash of a file, lower-case hex. Matches
# what cardano-cli computes for anchor-data-hash.
_zoo_anchor_b2b256() {
    local file="$1"
    if command -v b2sum >/dev/null 2>&1; then
        b2sum -l 256 -a blake2b "$file" 2>/dev/null | awk '{print $1}'
    else
        # Fall back to cardano-cli's hashing surface (always available
        # in the zoo's environment) via a temp Cardano command.
        python3 - "$file" <<'PY'
import sys, hashlib
h = hashlib.blake2b(digest_size=32)
with open(sys.argv[1], 'rb') as f:
    for chunk in iter(lambda: f.read(8192), b''):
        h.update(chunk)
print(h.hexdigest())
PY
    fi
}

# Generate the anchor JSON files we serve. Each tx-zoo entry refers to
# them by name (the file stem). New anchors should be added here and
# in the per-script helper calls below.
_zoo_anchor_seed() {
    mkdir -p "$ZOO_ANCHOR_DIR"
    local f
    declare -A anchors=(
        [pool3]='{"name":"tx-zoo pool 3","ticker":"TXZP3","description":"tx-zoo synthetic pool","homepage":"http://127.0.0.1/"}'
        [drep-1]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"references":[],"comment":"tx-zoo drep","externalUpdates":[]}}'
        [drep-1-v2]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"references":[],"comment":"tx-zoo drep v2","externalUpdates":[]}}'
        [info-action]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"title":"tx-zoo info","abstract":"info","motivation":"info","rationale":"info","references":[]}}'
        [pparam-change]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"title":"tx-zoo pparam","abstract":"pparam","motivation":"pparam","rationale":"pparam","references":[]}}'
        [hardfork]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"title":"tx-zoo hardfork","abstract":"hf","motivation":"hf","rationale":"hf","references":[]}}'
        [treasury]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"title":"tx-zoo treasury","abstract":"tw","motivation":"tw","rationale":"tw","references":[]}}'
        [no-confidence]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"title":"tx-zoo noconf","abstract":"nc","motivation":"nc","rationale":"nc","references":[]}}'
        [update-committee]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"title":"tx-zoo cc","abstract":"cc","motivation":"cc","rationale":"cc","references":[]}}'
        [new-constitution]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"title":"tx-zoo constitution","abstract":"cn","motivation":"cn","rationale":"cn","references":[]}}'
        [constitution-body]='{"@context":{},"hashAlgorithm":"blake2b-256","body":{"text":"tx-zoo constitution body","articles":[]}}'
    )
    for f in "${!anchors[@]}"; do
        printf '%s' "${anchors[$f]}" > "$ZOO_ANCHOR_DIR/$f.json"
    done
}

# Start the anchor HTTP server. Idempotent: if already running, no-op.
zoo_anchor_start() {
    if [ -f "$ZOO_ANCHOR_PID" ] && kill -0 "$(cat "$ZOO_ANCHOR_PID")" 2>/dev/null; then
        return 0
    fi
    _zoo_anchor_seed
    ( cd "$ZOO_ANCHOR_DIR" && python3 -m http.server "$ZOO_ANCHOR_PORT" \
        --bind 127.0.0.1 >"$ZOO_LOGS/anchor-server.log" 2>&1 ) &
    echo $! > "$ZOO_ANCHOR_PID"
    # Wait up to 3 s for the server to accept connections.
    local i=0
    while [ "$i" -lt 30 ]; do
        if curl -sf "http://127.0.0.1:$ZOO_ANCHOR_PORT/" >/dev/null 2>&1; then
            zoo_info "anchor server up on http://127.0.0.1:$ZOO_ANCHOR_PORT"
            return 0
        fi
        sleep 0.1
        i=$((i+1))
    done
    zoo_fail "anchor server did not come up on port $ZOO_ANCHOR_PORT"
    return 1
}

zoo_anchor_stop() {
    if [ -f "$ZOO_ANCHOR_PID" ]; then
        local pid
        pid="$(cat "$ZOO_ANCHOR_PID")"
        kill "$pid" 2>/dev/null || true
        rm -f "$ZOO_ANCHOR_PID"
    fi
}

zoo_anchor_url() {
    local name="$1"
    echo "http://127.0.0.1:$ZOO_ANCHOR_PORT/$name.json"
}

zoo_anchor_hash() {
    local name="$1"
    local f="$ZOO_ANCHOR_DIR/$name.json"
    [ -s "$f" ] || die "anchor file missing: $f (did zoo_anchor_start run?)"
    _zoo_anchor_b2b256 "$f"
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
