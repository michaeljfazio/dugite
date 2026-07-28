#!/usr/bin/env bash
# bidirectional-parity.sh — run the same tx-zoo categories twice, once via the
# default dugite-relay N2C socket and once via the cardano-bp socket, then
# build a parity matrix and assert there are zero off-diagonal cells.
#
# This implements the "bidirectional parity oracle" Round 1 predicate:
# for every transaction T, dugite and Haskell must reach the same accept/reject
# decision regardless of which node ingested it first.
#
# Per-socket key isolation: each batch gets its own ZOO_STATE directory and
# its own pre-funded payment key (created and funded from the devnet genesis
# utxo at wrapper-start). That way the second batch isn't fighting the first
# batch for the same UTxOs, so positive scripts can be re-run safely. The
# wrapper picks the funding amount per batch ($BATCH_FUND_LOVELACE) — default
# 600_000_000_000 (600K ADA) is enough for the full positive set including
# stake registration, pool registration, governance deposits, etc.
#
# Usage:
#   bidirectional-parity.sh [--out <parity-matrix.csv>]
#                           [--fund <lovelace>]
#                           [--skip-funding]
#                           CAT [CAT ...]
#
# Must be invoked from testnet/local-devnet/ AFTER ./run.sh has all 3 sockets up.
# (Each batch internally runs `./tx-zoo/run-all.sh --setup` against its own
# ZOO_STATE, so the wrapper does not require a prior --setup.)
#
# Produces:
#   tx-zoo/state-batch-relay/      (full per-batch state, including keys)
#   tx-zoo/state-batch-cardano-bp/ (full per-batch state, including keys)
#   <out> (default: evidence/<latest>/parity-matrix.csv)
#
# Exit codes:
#   0 — every (script,outcome) cell matches across both sockets
#   1 — at least one off-diagonal cell
#   2 — usage / environment / funding error
set -euo pipefail

OUT=""
CATS=()
# tx-zoo's own keygen funds each sub-wallet with 1.5e12 and pre-splits a
# collateral pool on top, so a batch wallet holding less than ~4e12 cannot
# complete `--setup` — it dies with "does not balance ... -900000000000" and
# the batch silently contributes ZERO rows to the matrix. The old 6e11 default
# could never work; every documented invocation of this script was failing the
# cardano-bp batch and reporting PASS off the relay batch alone.
BATCH_FUND_LOVELACE=6000000000000
SKIP_FUNDING=0
while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT="$2"; shift 2 ;;
        --fund) BATCH_FUND_LOVELACE="$2"; shift 2 ;;
        --skip-funding) SKIP_FUNDING=1; shift ;;
        -h|--help) sed -n '2,/^set -e/p' "$0" | sed 's/^# \{0,1\}//' ; exit 0 ;;
        --*) echo "unknown flag: $1" >&2; exit 2 ;;
        *) CATS+=("$1"); shift ;;
    esac
done
[ ${#CATS[@]} -eq 0 ] && { echo "usage: $0 [--out file] [--fund lovelace] CAT [CAT ...]" >&2; exit 2; }

if [ ! -d tx-zoo ] || [ ! -f lib/common.sh ]; then
    echo "must be run from testnet/local-devnet/ (no tx-zoo/ or lib/common.sh here)" >&2
    exit 2
fi

# Source the devnet env so LD_RELAY_SOCK and LD_CARDANO_BP_SOCK are defined.
# shellcheck source=/dev/null
. ./lib/common.sh

for s in "$LD_RELAY_SOCK" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$s" ] || { echo "socket not present: $s — is the devnet up?" >&2; exit 2; }
done

# Genesis payment key — used to fund each batch's per-batch payment key.
GENESIS_PAY_ADDR_FILE="$LD_KEYS/utxo/payment.addr"
GENESIS_PAY_SKEY="$LD_KEYS/utxo/payment.skey"
[ -s "$GENESIS_PAY_ADDR_FILE" ] || { echo "genesis pay addr missing: $GENESIS_PAY_ADDR_FILE" >&2; exit 2; }
[ -s "$GENESIS_PAY_SKEY" ]      || { echo "genesis pay skey missing: $GENESIS_PAY_SKEY"  >&2; exit 2; }

# Create + fund a per-batch payment key. Idempotent — re-uses an existing
# funded key if its balance is already at-or-above the requested amount.
prepare_batch_key() {
    local tag="$1" socket="$2"
    local kdir="tx-zoo/state-batch-${tag}/funding"
    mkdir -p "$kdir"
    local skey="$kdir/payment.skey" vkey="$kdir/payment.vkey" addr="$kdir/payment.addr"
    if [ ! -s "$skey" ]; then
        cardano-cli conway address key-gen \
            --signing-key-file "$skey" \
            --verification-key-file "$vkey" >/dev/null
    fi
    if [ ! -s "$addr" ]; then
        cardano-cli conway address build \
            --payment-verification-key-file "$vkey" \
            --testnet-magic "$LD_MAGIC" \
            --out-file "$addr" >/dev/null
    fi
    local addr_str
    addr_str=$(cat "$addr")

    # Skip funding if requested OR if balance already covers the request.
    local current
    current=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
        --socket-path "$socket" --address "$addr_str" --output-json 2>/dev/null \
        | jq -r '[ .[].value.lovelace ] | add // 0')
    if [ "$SKIP_FUNDING" -eq 1 ] || [ "$current" -ge "$BATCH_FUND_LOVELACE" ]; then
        echo "    batch '$tag' funded addr=$addr_str balance=$current (need $BATCH_FUND_LOVELACE) — skipping funding" >&2
        echo "$addr|$skey|$vkey"
        return
    fi

    echo "    batch '$tag' funding $addr_str with $BATCH_FUND_LOVELACE lovelace via $socket" >&2
    local genesis_addr
    genesis_addr=$(cat "$GENESIS_PAY_ADDR_FILE")
    # Find the largest UTxO at the genesis address on the supplied socket,
    # EXCLUDING any input a previous batch in this run already spent.
    #
    # The two batches are funded back-to-back from the same genesis address but
    # through DIFFERENT sockets. The second socket has not necessarily seen the
    # first batch's funding tx yet, so it still reports that input as unspent
    # and picks it again — submission then dies with
    # `ConwayMempoolFailure "All inputs are spent"` and the batch produces no
    # rows at all. This is a read-your-writes race across sockets, not a dugite
    # divergence; tracking spends in-process removes it without depending on
    # propagation timing.
    local utxo_in
    utxo_in=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
        --socket-path "$socket" --address "$genesis_addr" --output-json 2>/dev/null \
        | jq -r --arg spent "${SPENT_GENESIS_INPUTS:-}" '
            to_entries
            | map(select((.key as $k | ($spent | split(",") | index($k))) | not))
            | sort_by(-.value.value.lovelace)
            | .[0].key // empty')
    [ -n "$utxo_in" ] && [ "$utxo_in" != "null" ] \
        || { echo "no unspent genesis UTxO at $genesis_addr via $socket" >&2; exit 2; }
    # Reserve this input so the next batch cannot select it.
    SPENT_GENESIS_INPUTS="${SPENT_GENESIS_INPUTS:+$SPENT_GENESIS_INPUTS,}$utxo_in"

    local raw="$kdir/fund.raw" signed="$kdir/fund.signed"
    cardano-cli conway transaction build \
        --testnet-magic  "$LD_MAGIC" \
        --socket-path    "$socket" \
        --tx-in          "$utxo_in" \
        --tx-out         "${addr_str}+${BATCH_FUND_LOVELACE}" \
        --change-address "$genesis_addr" \
        --out-file       "$raw" >/dev/null
    cardano-cli conway transaction sign \
        --testnet-magic  "$LD_MAGIC" \
        --tx-body-file   "$raw" \
        --signing-key-file "$GENESIS_PAY_SKEY" \
        --out-file       "$signed" >/dev/null
    cardano-cli conway transaction submit \
        --testnet-magic  "$LD_MAGIC" \
        --socket-path    "$socket" \
        --tx-file        "$signed" 2>&1 | sed 's/^/      /' >&2

    # Wait for inclusion (up to 90s).
    local i
    for i in $(seq 1 90); do
        sleep 1
        local after
        after=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
            --socket-path "$socket" --address "$addr_str" --output-json 2>/dev/null \
            | jq -r '[ .[].value.lovelace ] | add // 0')
        if [ "$after" -ge "$BATCH_FUND_LOVELACE" ]; then
            echo "    batch '$tag' funded successfully after ${i}s (balance=$after)" >&2
            echo "$addr|$skey|$vkey"
            return
        fi
    done
    echo "batch '$tag' funding timed out — balance never reached $BATCH_FUND_LOVELACE" >&2
    exit 2
}

run_batch() {
    local label="$1" socket="$2" addr="$3" skey="$4" vkey="$5"
    shift 5
    local zoo_state="tx-zoo/state-batch-${label}"
    echo
    echo "=== bidirectional-parity: batch '$label' via $socket ==="
    echo "    ZOO_STATE=$zoo_state"
    echo "    ZOO_PAY_ADDR_FILE=$addr"
    echo "    ZOO_PAY_SKEY=$skey"
    mkdir -p "$zoo_state"
    # Each batch needs its own keys / collateral pool. --setup is idempotent.
    ZOO_STATE="$zoo_state" \
    ZOO_SOCKET="$socket" \
    ZOO_PAY_ADDR_FILE="$addr" \
    ZOO_PAY_SKEY="$skey" \
    ZOO_PAY_VKEY="$vkey" \
        ./tx-zoo/run-all.sh --setup >/dev/null 2>&1 || true
    ZOO_STATE="$zoo_state" \
    ZOO_SOCKET="$socket" \
    ZOO_PAY_ADDR_FILE="$addr" \
    ZOO_PAY_SKEY="$skey" \
    ZOO_PAY_VKEY="$vkey" \
        ./tx-zoo/run-all.sh "$@" || true
    # Snapshot results so the join below has stable filenames.
    cp "$zoo_state/results.csv" "tx-zoo/state/results.${label}.csv" 2>/dev/null || true
}

ZOO_STATE_TOP="tx-zoo/state"
mkdir -p "$ZOO_STATE_TOP"

# Per-invocation reset. These two CSVs are the ONLY inputs to the matrix, and
# they persist across runs — without this, a run that requests categories A
# joins rows left over from an earlier run that requested categories B, and the
# verdict is computed over a mixture of both. Observed: a run covering only
# 08-negative printed "PASS (every script classified identically)" while the
# entire accept side had never executed.
rm -f "$ZOO_STATE_TOP/results.relay.csv" "$ZOO_STATE_TOP/results.cardano-bp.csv"

# Stale per-batch zoo state makes keygen replay an already-spent funding tx
# ("All inputs are spent") and the batch then contributes zero rows.
rm -rf tx-zoo/state-batch-relay tx-zoo/state-batch-cardano-bp

# The scripts this invocation is REQUIRED to classify on both sockets. Used
# below to prove the matrix is complete rather than coincidentally symmetric.
EXPECTED_SCRIPTS=()
for cat in "${CATS[@]}"; do
    for s in "tx-zoo/$cat"/[0-9]*.sh; do
        [ -e "$s" ] && EXPECTED_SCRIPTS+=("$(basename "$s" .sh)")
    done
done

echo "=== preparing per-batch funded payment keys ==="
RELAY_TRIPLE=$(prepare_batch_key relay "$LD_RELAY_SOCK")
CBP_TRIPLE=$(prepare_batch_key cardano-bp "$LD_CARDANO_BP_SOCK")
RELAY_ADDR="${RELAY_TRIPLE%%|*}"; rest="${RELAY_TRIPLE#*|}"; RELAY_SKEY="${rest%%|*}"; RELAY_VKEY="${rest##*|}"
CBP_ADDR="${CBP_TRIPLE%%|*}";     rest="${CBP_TRIPLE#*|}";     CBP_SKEY="${rest%%|*}";   CBP_VKEY="${rest##*|}"

run_batch "relay"      "$LD_RELAY_SOCK"      "$RELAY_ADDR" "$RELAY_SKEY" "$RELAY_VKEY" "${CATS[@]}"
run_batch "cardano-bp" "$LD_CARDANO_BP_SOCK" "$CBP_ADDR"   "$CBP_SKEY"   "$CBP_VKEY"   "${CATS[@]}"

# Build parity-matrix.csv by joining on script name.
# Schema: name,status_relay,detail_relay,status_cardano_bp,detail_cardano_bp,match
if [ -z "$OUT" ]; then
    EVD=$(ls -t evidence 2>/dev/null | head -n 1 || true)
    if [ -n "$EVD" ]; then
        OUT="evidence/$EVD/parity-matrix.csv"
    else
        OUT="$ZOO_STATE_TOP/parity-matrix.csv"
    fi
fi
mkdir -p "$(dirname "$OUT")"

awk -F, '
    FNR==1 { next }
    FILENAME ~ /relay\.csv$/      { r_status[$2]=$3; r_detail[$2]=$5; names[$2]=1; next }
    FILENAME ~ /cardano-bp\.csv$/ { c_status[$2]=$3; c_detail[$2]=$5; names[$2]=1; next }
    END {
        # NOTE: header is emitted by the shell below, NOT here. Printing it
        # inside awk put it through the `sort` in the pipeline, which moved it
        # to the bottom ("name," sorts after digits); `head -n 1` then took the
        # alphabetically-FIRST DATA ROW as the header and dropped it from the
        # body. Every consumer that skips NR>1 — including the OFFDIAG and
        # TOTAL counters right below — therefore ignored one real row, so an
        # off-diagonal in the first row was invisible.
        for (n in names) {
            rs = (n in r_status) ? r_status[n] : "ABSENT"
            cs = (n in c_status) ? c_status[n] : "ABSENT"
            rd = (n in r_detail) ? r_detail[n] : ""
            cd = (n in c_detail) ? c_detail[n] : ""
            gsub(/,/," ",rd); gsub(/,/," ",cd)
            match_flag = (rs == cs) ? "MATCH" : "OFFDIAG"
            print n "," rs "," rd "," cs "," cd "," match_flag
        }
    }
' "$ZOO_STATE_TOP/results.relay.csv" "$ZOO_STATE_TOP/results.cardano-bp.csv" \
    | grep -v '^$' | LC_ALL=C sort > "$OUT.tmp"
# Header first, then the sorted body — the header never enters the sort.
{
    echo "name,status_relay,detail_relay,status_cardano_bp,detail_cardano_bp,match"
    cat "$OUT.tmp"
} > "$OUT"
rm -f "$OUT.tmp"

OFFDIAG=$(awk -F, 'NR>1 && $NF=="OFFDIAG" {c++} END {print c+0}' "$OUT")
TOTAL=$(awk 'NR>1' "$OUT" | wc -l | tr -d ' ')
echo
echo "=== parity matrix written to $OUT ==="
echo "  total scripts: $TOTAL  off-diagonal: $OFFDIAG"
if [ "$OFFDIAG" -gt 0 ]; then
    echo
    echo "OFF-DIAGONAL CELLS:"
    awk -F, 'NR>1 && $NF=="OFFDIAG" {printf "  %-44s relay=%-6s cardano-bp=%-6s\n", $1, $2, $4}' "$OUT"
    exit 1
fi

# COMPLETENESS GATE — a symmetric matrix is worthless if it is also empty.
#
# OFFDIAG==0 only says "the rows present agree". If a batch died before running
# anything (bad funding, stale state, keygen failure), BOTH sides are missing
# the same scripts, every present row matches, and the old code printed PASS.
# That is how the InvalidPrevGovActionId P0 slipped through a "PASS" parity
# run whose accept side had never executed. Require that every script in every
# requested category is actually present.
MISSING=()
for name in "${EXPECTED_SCRIPTS[@]}"; do
    awk -F, -v n="$name" 'NR>1 && $1==n {found=1} END {exit !found}' "$OUT" || MISSING+=("$name")
done
if [ ${#MISSING[@]} -gt 0 ]; then
    echo
    echo "INCOMPLETE MATRIX — ${#MISSING[@]} requested script(s) produced no row on either socket:"
    printf '  %s\n' "${MISSING[@]}"
    echo
    echo "This is NOT a pass. A batch failed before running these (check the"
    echo "funding/keygen output above). Re-run after fixing; do not interpret"
    echo "'off-diagonal: 0' as parity when the rows are absent."
    exit 1
fi

echo "  verdict: PASS ($TOTAL/${#EXPECTED_SCRIPTS[@]} requested scripts classified identically on both sockets)"
exit 0
