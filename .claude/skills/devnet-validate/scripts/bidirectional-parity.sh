#!/usr/bin/env bash
# bidirectional-parity.sh — run the same tx-zoo categories twice, once via the
# default dugite-relay N2C socket and once via the cardano-bp socket, then
# build a parity matrix and assert there are zero off-diagonal cells.
#
# This implements the "bidirectional parity oracle" Round 1 predicate:
# for every transaction T, dugite and Haskell must reach the same accept/reject
# decision regardless of which node ingested it first.
#
# Usage:
#   bidirectional-parity.sh [--out <parity-matrix.csv>] CAT [CAT ...]
#
# Must be invoked from testnet/local-devnet/ AFTER ./run.sh has all 3 sockets up
# and the tx-zoo has been --setup'd. Produces:
#   tx-zoo/state/results.relay.csv
#   tx-zoo/state/results.cardano-bp.csv
#   <out> (default: evidence/<latest>/parity-matrix.csv)
#
# Exit codes:
#   0 — every (script,outcome) cell matches across both sockets
#   1 — at least one off-diagonal cell
#   2 — usage / environment error
set -euo pipefail

OUT=""
CATS=()
while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT="$2"; shift 2 ;;
        -h|--help) sed -n '2,/^set -e/p' "$0" | sed 's/^# \{0,1\}//' ; exit 0 ;;
        --*) echo "unknown flag: $1" >&2; exit 2 ;;
        *) CATS+=("$1"); shift ;;
    esac
done
[ ${#CATS[@]} -eq 0 ] && { echo "usage: $0 [--out file] CAT [CAT ...]" >&2; exit 2; }

if [ ! -d tx-zoo ] || [ ! -f lib/common.sh ]; then
    echo "must be run from testnet/local-devnet/ (no tx-zoo/ or lib/common.sh here)" >&2
    exit 2
fi

# Source the devnet env so LD_RELAY_SOCK and LD_CARDANO_BP_SOCK are defined.
# common.sh is idempotent and only sets env / helpers.
# shellcheck source=/dev/null
. ./lib/common.sh

for s in "$LD_RELAY_SOCK" "$LD_CARDANO_BP_SOCK"; do
    [ -S "$s" ] || { echo "socket not present: $s — is the devnet up?" >&2; exit 2; }
done

ZOO_STATE="tx-zoo/state"
mkdir -p "$ZOO_STATE"

run_batch() {
    local label="$1" socket="$2"
    shift 2
    echo "=== bidirectional-parity: batch '$label' via $socket ==="
    ZOO_SOCKET="$socket" ./tx-zoo/run-all.sh "$@" || true
    cp "$ZOO_STATE/results.csv" "$ZOO_STATE/results.${label}.csv"
}

run_batch "relay"      "$LD_RELAY_SOCK"      "${CATS[@]}"
run_batch "cardano-bp" "$LD_CARDANO_BP_SOCK" "${CATS[@]}"

# Build parity-matrix.csv by joining on script name.
# Schema: name,status_relay,detail_relay,status_cardano_bp,detail_cardano_bp,match
if [ -z "$OUT" ]; then
    EVD=$(ls -t evidence 2>/dev/null | head -n 1 || true)
    if [ -n "$EVD" ]; then
        OUT="evidence/$EVD/parity-matrix.csv"
    else
        OUT="$ZOO_STATE/parity-matrix.csv"
    fi
fi
mkdir -p "$(dirname "$OUT")"

awk -F, '
    FNR==1 { next }
    FILENAME ~ /relay\.csv$/      { r_status[$2]=$3; r_detail[$2]=$5; names[$2]=1; next }
    FILENAME ~ /cardano-bp\.csv$/ { c_status[$2]=$3; c_detail[$2]=$5; names[$2]=1; next }
    END {
        print "name,status_relay,detail_relay,status_cardano_bp,detail_cardano_bp,match"
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
' "$ZOO_STATE/results.relay.csv" "$ZOO_STATE/results.cardano-bp.csv" \
    | LC_ALL=C sort > "$OUT.tmp"
{ head -n 1 "$OUT.tmp"; tail -n +2 "$OUT.tmp" | grep -v '^$' | LC_ALL=C sort; } > "$OUT"
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
echo "  verdict: PASS (every script classified identically on both sockets)"
exit 0
