#!/usr/bin/env bash
# futurepparams-boundary-parity.sh — cross-validate `futurePParams` against
# cardano-node across at least one epoch boundary (#977).
#
# WHY THIS EXISTS SEPARATELY FROM 09k-gov-state
# --------------------------------------------
# `09k-gov-state` already diffs the whole gov-state JSON between sockets, and
# that is what should catch a wrong `futurePParams`. It does not, in practice,
# because of the devnet's own parameters:
#
#   2 * stabilityWindow = 2 * (3k/f) = 480  >  epochLength = 400
#
# so `solidifyNextEpochPParams`'s point of no return is *before* every epoch
# even starts. `PotentialPParamsUpdate` therefore survives only from the
# boundary block until the very next block — three or four slots out of four
# hundred. A one-shot query lands in that window roughly 1% of the time, and
# `NoPParamsUpdate` (the value dugite used to hardcode) is correct for the
# other 99%. A green 09k is close to no evidence at all.
#
# This script closes that gap by SAMPLING continuously across a boundary, so
# the narrow window is actually observed rather than hoped for.
#
# It was written after #977 shipped with the boundary reset wired into the
# `#[doc(hidden)]` test-only path: every unit test passed and only a live diff
# against cardano-node caught it.
#
# TIP PINNING
# -----------
# The two sockets cannot be queried atomically. Sampling naively produces a
# false DIFF whenever a block lands between the two calls — observed once in
# 700 samples, right at a boundary, which is exactly where it is most
# misleading. So each sample re-reads the tip after both queries and DISCARDS
# itself unless the tip slot was identical before and after on both nodes.
# Discarded samples are counted and reported; they are not failures.
#
# Usage: futurepparams-boundary-parity.sh [--seconds N] [--out FILE]
set +e
unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LD_ROOT="$(cd "$SCRIPT_DIR/../../../../testnet/local-devnet" && pwd)"
. "$LD_ROOT/lib/common.sh"

SECONDS_TO_RUN=900
OUT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --seconds) SECONDS_TO_RUN="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done
[ -n "$OUT" ] || OUT="$LD_ROOT/evidence/current/futurepparams-parity.csv"
mkdir -p "$(dirname "$OUT")"

D="${LD_DUGITE_BP_SOCK:-/tmp/ld-$UID/dbp.sock}"
C="${LD_CARDANO_BP_SOCK:-/tmp/ld-$UID/cbp.sock}"
MAGIC="${LD_MAGIC:-42}"

_tip_slot() { cardano-cli query tip --testnet-magic "$MAGIC" --socket-path "$1" 2>/dev/null | jq -r '.slot // empty'; }
_fpp() { cardano-cli conway query gov-state --testnet-magic "$MAGIC" --socket-path "$1" 2>/dev/null | jq -Sc '.futurePParams // "ABSENT"'; }

echo "ts,slot,epoch,dugite_tag,cardano_tag,equal,verdict" > "$OUT"

deadline=$(( $(date +%s) + SECONDS_TO_RUN ))
compared=0; diffs=0; unstable=0; potential_seen=0; boundaries=0
last_epoch=""

while [ "$(date +%s)" -lt "$deadline" ]; do
    s0d=$(_tip_slot "$D"); s0c=$(_tip_slot "$C")
    dg=$(_fpp "$D");       cg=$(_fpp "$C")
    s1d=$(_tip_slot "$D"); s1c=$(_tip_slot "$C")

    # Tip pinning: any block landing mid-sample invalidates the comparison.
    if [ -z "$s0d" ] || [ -z "$s0c" ] || [ "$s0d" != "$s1d" ] || [ "$s0c" != "$s1c" ] || [ "$s0d" != "$s0c" ]; then
        unstable=$((unstable + 1))
        echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),${s0d:-?},?,?,?,?,TIP_UNSTABLE" >> "$OUT"
        continue
    fi
    [ -n "$dg" ] && [ -n "$cg" ] || { unstable=$((unstable + 1)); continue; }

    epoch=$(( s0d / ${LD_EPOCH_LENGTH:-400} ))
    [ -n "$last_epoch" ] && [ "$epoch" != "$last_epoch" ] && boundaries=$((boundaries + 1))
    last_epoch="$epoch"

    dt=$(echo "$dg" | jq -r 'if type=="object" then .tag else tostring end')
    ct=$(echo "$cg" | jq -r 'if type=="object" then .tag else tostring end')
    [ "$dt" = "PotentialPParamsUpdate" ] && potential_seen=$((potential_seen + 1))

    compared=$((compared + 1))
    if [ "$dg" = "$cg" ]; then
        v=MATCH
    else
        v=DIFF; diffs=$((diffs + 1))
    fi
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),$s0d,$epoch,$dt,$ct,$( [ "$dg" = "$cg" ] && echo true || echo false ),$v" >> "$OUT"
done

echo "futurePParams parity: compared=$compared diffs=$diffs tip_unstable=$unstable boundaries_crossed=$boundaries potential_samples=$potential_seen"

rc=0
if [ "$diffs" -gt 0 ]; then
    echo "FAIL: $diffs futurePParams divergence(s) vs cardano-node — see $OUT" >&2
    rc=1
fi
if [ "$compared" -lt 30 ]; then
    echo "FAIL: only $compared tip-stable comparisons — this measured nothing" >&2
    rc=1
fi
# A run that never crosses a boundary has not exercised the field's only
# interesting state. Report that as INCONCLUSIVE rather than as a pass: a
# green run with boundaries_crossed=0 is precisely the false confidence this
# script was written to remove.
if [ "$boundaries" -lt 1 ]; then
    echo "INCONCLUSIVE: no epoch boundary crossed in ${SECONDS_TO_RUN}s — \
futurePParams never left NoPParamsUpdate, so the reset path was not exercised" >&2
    rc=1
fi
if [ "$potential_seen" -lt 1 ]; then
    echo "INCONCLUSIVE: crossed $boundaries boundary(ies) but never sampled \
PotentialPParamsUpdate — the post-boundary window is only ~3 slots wide, so \
sample faster or run longer" >&2
    rc=1
fi
exit $rc
