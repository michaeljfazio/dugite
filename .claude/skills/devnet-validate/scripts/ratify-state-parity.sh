#!/usr/bin/env bash
# ratify-state-parity.sh — cross-validate the frozen DRep pulser against
# cardano-node, and then check that what it PREDICTED is what actually
# ENACTED (#988, #990, #991, #992).
#
# WHAT THIS MEASURES THAT NOTHING ELSE DOES
# -----------------------------------------
# `nextRatifyState` in `cardano-cli conway query gov-state` is Haskell's
#
#     queryRatifyState = snd . finishedPulserState
#
# i.e. the ratification decided at the LAST epoch boundary over the inputs
# frozen there, which the NEXT boundary applies verbatim (`ConwayEPOCH` reads
# `extractDRepPulsingState` and never re-runs RATIFY). Since #988 step 2 dugite
# applies it too, so this one field is simultaneously:
#
#   * a query-parity check — does dugite agree with cardano-node about what is
#     about to happen; and
#   * a consensus check — because that value IS the enactment.
#
# It is the only externally observable form of several things that are
# otherwise invisible:
#
#   #990  `rsExpired` is decided over the pulser's own candidate set, and an
#         action gets a final ratification attempt on the same pass that
#         expires it. dugite used to skip expired candidates outright and
#         derive the expired set by rescanning live proposals after the fact.
#   #991  DRep voting power had proposal deposits counted twice against a
#         denominator that counted them once, inflating `dRepAcceptedRatio`.
#         Nothing else surfaces that: `GetDRepStakeDistr` serves the
#         distribution itself, which was never doubled. It only shows up as a
#         DIFFERENT SET OF ENACTED ACTIONS — which is this field.
#
# It does NOT cover #992, and the distinction matters. `cardano-cli conway query
# gov-state` renders `nextRatifyState` from `GetRatifyState` (tag 32), not from
# the `DRepPulsingState` embedded in the tag-24 reply — verified by observing
# that `gov-state`'s `nextRatifyState` is byte-identical to `ratify-state` on a
# node whose tag-24 pulser was still hardcoded empty. So the embedded pulser has
# no cardano-cli-visible form at all and needs the encoder-level golden test
# (`gov_state_embeds_the_same_ratify_state_tag_32_serves`) instead.
#
# WHY IT SAMPLES CONTINUOUSLY
# ---------------------------
# Same reason as `futurepparams-boundary-parity.sh`: the interesting states are
# narrow. `nextRatifyState` is empty for most of the run and non-empty only
# around the boundaries where the gov lifecycle actually ratifies something. A
# single query almost certainly lands on the empty case, where a broken
# implementation and a correct one give the same answer.
#
# TIP PINNING
# -----------
# The two sockets cannot be queried atomically, so each sample re-reads both
# tips afterwards and discards itself unless nothing moved. Discarded samples
# are counted, not failed.
#
# Usage: ratify-state-parity.sh [--seconds N] [--out FILE]
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
[ -n "$OUT" ] || OUT="$LD_ROOT/evidence/current/ratify-state-parity.csv"
mkdir -p "$(dirname "$OUT")"

D="${LD_DUGITE_BP_SOCK:-/tmp/ld-$UID/dbp.sock}"
C="${LD_CARDANO_BP_SOCK:-/tmp/ld-$UID/cbp.sock}"
MAGIC="${LD_MAGIC:-42}"
EPOCH_LEN="${LD_EPOCH_LENGTH:-400}"

_tip_slot() {
    cardano-cli query tip --testnet-magic "$MAGIC" --socket-path "$1" 2>/dev/null \
        | jq -r '.slot // empty'
}

# Only the decision is compared, never the whole `nextEnactState`: that carries
# the full pparams and committee, which `09k-gov-state` already diffs, and
# including it here would turn every unrelated pparam difference into a
# ratify-state failure.
# A GovActionId renders as `{"govActionIx":N,"txId":"…"}`, and this flattens it
# to `txId#ix` so both sockets' answers are directly comparable.
#
# The two lists nest it DIFFERENTLY, and getting that wrong is silent:
#
#   .enactedGovActions[]  is a whole GovActionState — id at `.actionId`
#   .expiredGovActions[]  is a bare GovActionId     — id at the top level
#   .proposals[]          is a whole GovActionState — id at `.actionId`
#
# Applying the bare-id form to `enactedGovActions` yields `"null#null"` for
# every entry, which compares EQUAL across sockets no matter which actions each
# one listed. That is the "reports success while measuring nothing" family this
# harness keeps finding, and it happened here on the first run — caught only
# because the CSV records the flattened value rather than just a verdict.
# `_assert_ids` below makes it fail loudly instead.
_gid_expr='"\(.txId)#\(.govActionIx)"'

# Fail the run if a flattened id came out degenerate. A shape change upstream
# must break this suite, not quietly turn it into a tautology.
_assert_ids() {
    case "$1" in
        *'"null#null"'*|*'"#"'*)
            echo "HARNESS BUG: a GovActionId flattened to a null id — the JSON \
shape changed and every comparison would now be vacuously equal. Sample: $1" >&2
            return 1 ;;
    esac
    return 0
}

_ratify() {
    cardano-cli conway query gov-state --testnet-magic "$MAGIC" --socket-path "$1" 2>/dev/null \
        | jq -Sc "(.nextRatifyState // {}) | {
              enacted: [ (.enactedGovActions // [])[] | .actionId | $_gid_expr ] | sort,
              expired: [ (.expiredGovActions // [])[] | $_gid_expr ] | sort,
              delayed: (.ratificationDelayed // false)
          }"
}

echo "ts,slot,epoch,socket_agree,dugite,cardano,verdict" > "$OUT"

deadline=$(( $(date +%s) + SECONDS_TO_RUN ))
compared=0; diffs=0; unstable=0; boundaries=0; nonempty=0; rc_harness=0
last_epoch=""
# The plan observed during the epoch that is about to end. Checked against the
# enactment once the boundary has passed.
pending_plan=""
plan_checks=0; plan_breaks=0

while [ "$(date +%s)" -lt "$deadline" ]; do
    s0d=$(_tip_slot "$D"); s0c=$(_tip_slot "$C")
    dr=$(_ratify "$D");    cr=$(_ratify "$C")
    s1d=$(_tip_slot "$D"); s1c=$(_tip_slot "$C")

    if [ -z "$s0d" ] || [ -z "$s0c" ] || [ "$s0d" != "$s1d" ] || [ "$s0c" != "$s1c" ] || [ "$s0d" != "$s0c" ]; then
        unstable=$((unstable + 1))
        echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),${s0d:-?},?,false,,,TIP_UNSTABLE" >> "$OUT"
        continue
    fi
    [ -n "$dr" ] && [ -n "$cr" ] || { unstable=$((unstable + 1)); continue; }

    epoch=$(( s0d / EPOCH_LEN ))

    if [ -n "$last_epoch" ] && [ "$epoch" != "$last_epoch" ]; then
        boundaries=$((boundaries + 1))
        # A boundary just passed. Whatever the pulser said during the epoch
        # that ended is what this boundary was required to enact. Both nodes
        # now report the NEXT plan, so the check is against cardano-node's
        # freshly-applied state rather than against dugite's own memory of it.
        if [ -n "$pending_plan" ]; then
            plan_checks=$((plan_checks + 1))
            enacted_now=$(echo "$pending_plan" | jq -c '.enacted')
            # An action the pulser planned to enact must no longer be a live
            # proposal on EITHER node: enactment removes it, and so does the
            # sibling cleanup. A planned action still sitting in the proposal
            # set means the boundary did not apply the plan.
            still_live=$(cardano-cli conway query gov-state --testnet-magic "$MAGIC" \
                             --socket-path "$C" 2>/dev/null \
                         | jq -Sc --argjson want "$enacted_now" \
                             "[ (.proposals // [])[] | .actionId | $_gid_expr ]
                              | map(select(. as \$p | \$want | index(\$p)))")
            if [ "$still_live" != "[]" ] && [ -n "$still_live" ]; then
                plan_breaks=$((plan_breaks + 1))
                echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),$s0d,$epoch,true,$pending_plan,$still_live,PLAN_NOT_APPLIED" >> "$OUT"
            else
                echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),$s0d,$epoch,true,$pending_plan,,PLAN_APPLIED" >> "$OUT"
            fi
        fi
        pending_plan=""
    fi
    last_epoch="$epoch"

    # Remember the most recent non-empty plan seen during this epoch.
    if [ "$(echo "$dr" | jq -r '.enacted | length')" != "0" ]; then
        nonempty=$((nonempty + 1))
        pending_plan="$dr"
    fi

    _assert_ids "$dr" || { rc_harness=1; break; }

    compared=$((compared + 1))
    if [ "$dr" = "$cr" ]; then
        v=MATCH
    else
        v=DIFF; diffs=$((diffs + 1))
    fi
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),$s0d,$epoch,$( [ "$dr" = "$cr" ] && echo true || echo false ),$dr,$cr,$v" >> "$OUT"
done

echo "ratify-state parity: compared=$compared diffs=$diffs tip_unstable=$unstable \
boundaries_crossed=$boundaries nonempty_samples=$nonempty \
plan_checks=$plan_checks plan_breaks=$plan_breaks"

rc=0
if [ "$rc_harness" -ne 0 ]; then
    echo "FAIL: harness aborted — see the HARNESS BUG line above" >&2
    rc=1
fi
if [ "$diffs" -gt 0 ]; then
    echo "FAIL: $diffs nextRatifyState divergence(s) vs cardano-node — see $OUT" >&2
    rc=1
fi
if [ "$plan_breaks" -gt 0 ]; then
    echo "FAIL: $plan_breaks boundary(ies) did not enact what the frozen pulser \
planned — the epoch boundary must APPLY the plan, not re-decide (#988) — see $OUT" >&2
    rc=1
fi
if [ "$compared" -lt 30 ]; then
    echo "FAIL: only $compared tip-stable comparisons — this measured nothing" >&2
    rc=1
fi
# Crossing a boundary is what moves the pulser. Without one, every sample is
# the same frozen value compared against itself.
if [ "$boundaries" -lt 1 ]; then
    echo "INCONCLUSIVE: no epoch boundary crossed in ${SECONDS_TO_RUN}s — the \
pulser never rotated, so nothing about its lifecycle was exercised" >&2
    rc=1
fi
# And an all-empty run proves only that two implementations agree about
# nothing happening. Report it rather than passing on it: this is the exact
# blind spot that let #992's hardcoded empty pulser survive.
if [ "$nonempty" -lt 1 ]; then
    echo "INCONCLUSIVE: crossed $boundaries boundary(ies) but never sampled a \
non-empty nextRatifyState — run the gov lifecycle (10-gov-lifecycle) alongside \
this, or an empty answer is indistinguishable from a hardcoded one" >&2
    rc=1
fi
exit $rc
