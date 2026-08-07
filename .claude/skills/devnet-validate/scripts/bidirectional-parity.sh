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
# Reject-reason comparison. `both rejected` is a weaker predicate than it looks:
# dugite and Haskell can agree a tx is invalid while disagreeing about WHY, and
# a wrong-reason rejection is a real compatibility defect (a client cannot act
# on it). The methodology doc grades accept-set mismatches P0 and reject-reason
# mismatches P2 — both are reported, and both fail by default, because a check
# that reports without enforcing is the disease this harness keeps catching.
ALLOW_CLASS_DRIFT=0
SCRIPT_SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DENOM_FILE="${DENOM_FILE:-$SCRIPT_SELF_DIR/../schemas/denominators.json}"
while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT="$2"; shift 2 ;;
        --fund) BATCH_FUND_LOVELACE="$2"; shift 2 ;;
        --skip-funding) SKIP_FUNDING=1; shift ;;
        --allow-class-drift) ALLOW_CLASS_DRIFT=1; shift ;;
        --denominators) DENOM_FILE="$2"; shift 2 ;;
        -h|--help) sed -n '2,/^set -e/p' "$0" | sed 's/^# \{0,1\}//' ; exit 0 ;;
        --*) echo "unknown flag: $1" >&2; exit 2 ;;
        *) CATS+=("$1"); shift ;;
    esac
done

# No categories named => use the standard gate set from the pinned manifest.
# Hardcoding the list at every call site is how it stayed at 4 categories (41 of
# 85 scripts) while the release notes said "41/41" without ever stating the 85.
if [ ${#CATS[@]} -eq 0 ]; then
    if [ -f "$DENOM_FILE" ]; then
        while IFS= read -r c; do CATS+=("$c"); done < <(
            jq -r '.parity_matrix.required_categories_standard[]' "$DENOM_FILE")
        echo "no categories given — using the standard set from $(basename "$DENOM_FILE"): ${CATS[*]}" >&2
    else
        echo "usage: $0 [--out file] [--fund lovelace] CAT [CAT ...]" >&2
        echo "(and no denominator manifest at $DENOM_FILE to take the default set from)" >&2
        exit 2
    fi
fi

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
    # Normalise a result detail into a comparable REJECTION CLASS.
    #
    # tx-zoo records the rule it matched in the detail column, e.g.
    #   rejected-NoInputs / rejected-InputNotFound / rejected-ValueNotConserved
    #   rejected-as-expected                      (generic: script matched only a pattern)
    #   duplicate-input-rejected rc=1 reason-matches-rule: <node text>
    # Reduce to the discriminating token so the two sockets can be compared
    # without dragging in node-specific prose, txids or timings.
    #
    # CRITICAL: only ACCEPTANCE-side details that describe a rejection are
    # comparable. For a tx that was accepted, the detail is free-form metadata
    # about THIS run — a minted policy id, a script address, a reference txid —
    # and every one of those legitimately differs between batches because each
    # batch uses its own keys. Comparing those produced 7 false CLASSDIFFs on
    # the first full run (02a-02d, 02f, 03h, 03i). Gate on the detail actually
    # looking like a rejection.
    function is_rejection(d) { return (d ~ /reject/) }
    function rclass(d,    m, parts) {
        if (d == "") return ""
        if (!is_rejection(d)) return "(accepted)"
        if (match(d, /rejected-[A-Za-z0-9_]+/)) {
            m = substr(d, RSTART, RLENGTH)
            sub(/^rejected-/, "", m)
            return m
        }
        if (d ~ /reason-matches-rule/) return "reason-matches-rule"
        split(d, parts, /[ \t]+/)
        return parts[1]
    }
    # Scripts whose subject is a GLOBAL, non-replicable devnet resource, so the
    # second batch necessarily operates on state the first batch already
    # mutated. These are not parity defects and must not be counted as such —
    # but they must not be silently dropped either, so they get their own
    # verdict and appear in the matrix with the reason.
    BEGIN {
        stateful["05g-cc-hot-key-authorization"] = "constitutional committee is seated at genesis and is devnet-global: batch 1 resigns cc-1, so batch 2 correctly gets ConwayCommitteeHasPreviouslyResigned. Giving batch 2 its own committee needs an UpdateCommittee governance action (2+ epoch boundaries)."
        stateful["05h-cc-resign"] = "same: a cold-key resignation is one-shot per member, and only cc-1/cc-2 are seated."
        stateful["11d-replay-resubmit"] = "funds from the SHARED genesis address via zoo_largest_utxo, so both batches select the SAME largest UTxO and one invalidates the other. Measured: cardano-bp logged Mempool.AddedTx for the loser then Mempool.RemoveTxs dropping it with four other txs 4s later, and the tx reached 0/3 observers in 120s. The PARITY ASSERTION ITSELF HELD — in the batch that completed, both sockets rejected the replay identically — so this is an inclusion race on a global resource, not an accept/reject divergence. 11d still runs, and passes, in Round 1s full zoo, so nothing is uncovered."
        # Deliberate, documented protocol difference (#925): a Conway duplicate
        # input fails at the CBOR set layer. Haskell drops the connection
        # ("mux: bearer closed"); dugite answers a structured MsgRejectTx naming
        # the rule. dugite is deliberately more informative here — this is not a
        # defect and must not be normalised away silently.
        known_class_diff["08f-double-spend"] = "#925 — Haskell drops the connection at the codec layer; dugite returns a structured MsgRejectTx"
    }
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
            rc = rclass(rd); cc = rclass(cd)
            # Derive the category from the numeric prefix (01a-... -> 01).
            split(n, np, "-"); pfx = np[1]; gsub(/[a-z]+$/, "", pfx)
            # STATEFUL is claimed only when the row ACTUALLY differs. If a
            # future genesis seats enough committee members for batch 2 to have
            # its own resign target, 05g/05h start matching and are counted as
            # MATCH — the exclusion retires itself instead of becoming a
            # permanent blind spot. An exclusion that cannot notice it is no
            # longer needed is just another silent gap.
            if ((n in stateful) && rs != cs) match_flag = "STATEFUL"
            else if (rs != cs)             match_flag = "OFFDIAG"   # P0: accept-set differs
            else if (rc != cc) {
                match_flag = (n in known_class_diff) ? "KNOWNDIFF" : "CLASSDIFF"
            }
            else                           match_flag = "MATCH"
            print n "," pfx "," rs "," rd "," rc "," cs "," cd "," cc "," match_flag
        }
    }
' "$ZOO_STATE_TOP/results.relay.csv" "$ZOO_STATE_TOP/results.cardano-bp.csv" \
    | grep -v '^$' | LC_ALL=C sort > "$OUT.tmp"
# Header first, then the sorted body — the header never enters the sort.
{
    echo "name,category,status_relay,detail_relay,class_relay,status_cardano_bp,detail_cardano_bp,class_cardano_bp,match"
    cat "$OUT.tmp"
} > "$OUT"
rm -f "$OUT.tmp"

OFFDIAG=$(awk -F, 'NR>1 && $NF=="OFFDIAG" {c++} END {print c+0}' "$OUT")
CLASSDIFF=$(awk -F, 'NR>1 && $NF=="CLASSDIFF" {c++} END {print c+0}' "$OUT")
KNOWNDIFF=$(awk -F, 'NR>1 && $NF=="KNOWNDIFF" {c++} END {print c+0}' "$OUT")
STATEFUL=$(awk -F, 'NR>1 && $NF=="STATEFUL" {c++} END {print c+0}' "$OUT")
MATCHED=$(awk -F, 'NR>1 && $NF=="MATCH" {c++} END {print c+0}' "$OUT")
TOTAL=$(awk 'NR>1 && NF' "$OUT" | wc -l | tr -d ' ')

# Sidecar meta — carries the denominator this invocation was MEANT to cover,
# so the release-report generator can tell "41 of 41 requested" from "41 rows
# happened to be produced" (#953). The CSV alone cannot express intent.
META="${OUT%.csv}.meta.json"
{
    printf '{\n'
    printf '  "expected": %d,\n' "${#EXPECTED_SCRIPTS[@]}"
    printf '  "total": %d,\n'    "$TOTAL"
    printf '  "match": %d,\n'    "$MATCHED"
    printf '  "offdiag": %d,\n'  "$OFFDIAG"
    printf '  "classdiff": %d,\n' "$CLASSDIFF"
    printf '  "knowndiff": %d,\n' "$KNOWNDIFF"
    printf '  "stateful": %d,\n'  "$STATEFUL"
    printf '  "categories": ['
    for ci in "${!CATS[@]}"; do
        [ "$ci" -gt 0 ] && printf ', '
        printf '"%s"' "${CATS[$ci]}"
    done
    printf ']\n'
    printf '}\n'
} > "$META"

echo
echo "=== parity matrix written to $OUT ==="
echo "=== parity meta   written to $META ==="
echo "  total scripts: $TOTAL  match: $MATCHED  off-diagonal: $OFFDIAG"
echo "  class-diff: $CLASSDIFF  known-diff: $KNOWNDIFF  stateful-excluded: $STATEFUL"
echo "  per category:"
awk -F, 'NR>1 && NF {t[$2]++; if($NF=="OFFDIAG") o[$2]++; else if($NF=="CLASSDIFF") c[$2]++}
         END {for (k in t) printf "    %-6s %3d scripts  offdiag=%d classdiff=%d\n", k, t[k], o[k]+0, c[k]+0}' \
    "$OUT" | LC_ALL=C sort
if [ "$OFFDIAG" -gt 0 ]; then
    echo
    echo "OFF-DIAGONAL CELLS (P0 — one node accepts what the other rejects):"
    awk -F, 'NR>1 && $NF=="OFFDIAG" {printf "  %-44s relay=%-8s cardano-bp=%-8s\n", $1, $3, $6}' "$OUT"
    exit 1
fi
if [ "$STATEFUL" -gt 0 ]; then
    echo
    echo "PATH-C STATEFUL EXCLUSIONS (not parity defects — global devnet resource):"
    awk -F, 'NR>1 && $NF=="STATEFUL" {printf "  %-44s relay=%-8s cardano-bp=%-8s\n", $1, $3, $6}' "$OUT"
    echo "  Reasons are recorded in the BEGIN block of this script."
fi
if [ "$KNOWNDIFF" -gt 0 ]; then
    echo
    echo "KNOWN REJECT-REASON DIFFERENCES (documented, deliberate):"
    awk -F, 'NR>1 && $NF=="KNOWNDIFF" {printf "  %-44s relay=%-22s cardano-bp=%-22s\n", $1, $5, $8}' "$OUT"
fi
if [ "$CLASSDIFF" -gt 0 ]; then
    echo
    echo "REJECT-REASON MISMATCHES (P2 — same verdict, different reason):"
    awk -F, 'NR>1 && $NF=="CLASSDIFF" {printf "  %-44s relay=%-24s cardano-bp=%-24s\n", $1, $5, $8}' "$OUT"
    if [ "$ALLOW_CLASS_DRIFT" -eq 0 ]; then
        echo
        echo "Both nodes agree the transaction is invalid but disagree about WHY."
        echo "A client cannot act on a wrong reason, so this is a real compat defect."
        echo "Fix it, or — if the difference is cosmetic — normalise it in rclass()"
        echo "rather than passing --allow-class-drift, which silences ALL of them."
        exit 1
    fi
    echo "  (--allow-class-drift set — reported, not fatal)"
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

echo "  verdict: PASS ($MATCHED/${#EXPECTED_SCRIPTS[@]} classified identically; $KNOWNDIFF known reason-diff, $STATEFUL stateful-excluded)"
exit 0
