#!/usr/bin/env bash
# Summarise a testnet/local-devnet/evidence/<ts>/ directory into a single
# human-readable report block. Exits non-zero if any anomaly threshold is
# breached — usable as a CI gate.
#
# Usage: analyze-evidence.sh <evidence_dir> [--allowed-errors <file>]
#
# --allowed-errors declares ERROR lines that a round CAUSED ON PURPOSE. It exists for
# one situation: Round 1 runs the chaos suite, which SIGKILLs the sole forger mid-forge,
# and a forger killed between forging and adopting can legitimately come back to find
# its block beaten — `TraceDidntAdoptBlock`, which is the SAFE outcome and the same
# severity cardano-node uses. Without this the baseline round fails whenever the kill
# lands at an unlucky moment, which is a coin-flip, not a signal.
#
# Three properties keep it from becoming the over-broad allowlist that makes a round
# report success while measuring nothing (#916/#923/#945/#953/#959):
#   1. It is OPT-IN per invocation. No round gets it unless its caller passes it.
#   2. It NEVER applies to the invalid-block check. A Haskell-rejected dugite block
#      stays CRITICAL and unconditional — that check is the point of the round.
#   3. Both numbers are always printed ("N ERROR, M unexplained"), so an allowance is
#      visible in the output rather than silently subtracted.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Shared level-token counting — keeps this analyzer in exact agreement with
# generate-release-report.sh (#916).
# shellcheck source=lib/log-level-counts.sh
source "$SCRIPT_DIR/lib/log-level-counts.sh"

EVD=""
ALLOWED_ERRORS=""
while [ $# -gt 0 ]; do
    case "$1" in
        --allowed-errors) ALLOWED_ERRORS="${2:-}"; shift 2 ;;
        -*) echo "unknown option: $1" >&2; exit 2 ;;
        *)  EVD="$1"; shift ;;
    esac
done
if [ -z "$EVD" ] || ! [ -d "$EVD" ]; then
    echo "Usage: $0 <evidence_dir> [--allowed-errors <file>]" >&2
    echo "  e.g. $0 testnet/local-devnet/evidence/20260521T120000Z" >&2
    exit 2
fi
# A named-but-missing allowlist is a hard error, never a silent "allow nothing":
# that is how a caller ends up believing a suppression is active when it is not.
if [ -n "$ALLOWED_ERRORS" ] && [ ! -f "$ALLOWED_ERRORS" ]; then
    echo "--allowed-errors: no such file: $ALLOWED_ERRORS" >&2
    exit 2
fi

# Thresholds (override via env)
TIP_AGE_P99_MAX=${TIP_AGE_P99_MAX:-10}
ORPHAN_RATE_MAX=${ORPHAN_RATE_MAX:-0.05}   # 5%
DENSITY_TOLERANCE=${DENSITY_TOLERANCE:-0.2} # ±20% of activeSlotsCoeff
ACTIVE_SLOT_COEFF=${ACTIVE_SLOT_COEFF:-0.5}

# Find logs — prefer snapshot in evidence, fall back to live logs/
LOG_DIR="$EVD/logs"
[ -d "$LOG_DIR" ] || LOG_DIR="$(dirname "$(dirname "$EVD")")/logs"

ANOMALIES=()

# --- Metadata ----------------------------------------------------------------
TS=$(basename "$EVD")
GIT_REV="?"; CN_VER="?"; CCLI_VER="?"; DUR="?"
if [ -f "$EVD/metadata.json" ]; then
    GIT_REV=$(jq -r '.dugite_node_git // "?"' "$EVD/metadata.json")
    CN_VER=$(jq -r '.cardano_node_version // "?"' "$EVD/metadata.json")
    CCLI_VER=$(jq -r '.cardano_cli_version  // "?"' "$EVD/metadata.json")
    DUR=$(jq -r '.duration_seconds         // "?"' "$EVD/metadata.json")
fi

# --- Block accounting --------------------------------------------------------
TOTAL_FORGES=0; CANONICAL=0; ORPHANS=0
ORPHAN_RATE=0
if [ -f "$EVD/blocks.csv" ]; then
    # Forges ABOVE cardano-bp's last observed slot are EXCLUDED: sampling
    # stopped before it could record them, so "not received" carries no
    # information about them.
    #
    # Without this the metric counts its own sampling TAIL as orphans.
    # Measured on three consecutive 1800s rounds: 27, 47, 65 "orphans" — and in
    # the 65 case ALL of them sat in the final 120 slots (34 in the final 60)
    # while every forged block had in fact been received by another observer.
    # It crossed the 5% threshold twice and failed two healthy rounds.
    #
    # `verify.sh` p1 already excludes exactly this population, and its comment
    # gives the reason: on a single-forger topology nothing competes, so an
    # unmatched forge "simply had not propagated yet when the check ran", and
    # calling it an orphan on a topology that structurally cannot orphan sends
    # the reader hunting forks that do not exist. (That same comment asserts
    # analyze computes orphans "from the CHAIN" — it does not; it compares the
    # same two event sets p1 does, which is why the two counters disagreed in
    # opposite directions on 2026-08-07 and again on 2026-08-11.)
    #
    # Self-calibrating rather than a fixed window, so it stays correct if the
    # sampler cadence or round length changes.
    LAST_OBS=$(awk -F, 'NR>1 && $3=="recv" && $2=="cardano-bp" && $4+0>m {m=$4+0} END{print m+0}' "$EVD/blocks.csv")
    UNJUDGEABLE=$(awk -F, -v last="$LAST_OBS" \
        'NR>1 && $3=="forge" && $4+0>last {print $4","$5}' "$EVD/blocks.csv" \
        | sort -u | grep -c '^' || true)
    TOTAL_FORGES=$(awk -F, -v last="$LAST_OBS" \
        'NR>1 && $3=="forge" && $4+0<=last {print $4","$5}' "$EVD/blocks.csv" \
        | sort -u | grep -c '^' || true)
    # Canonical = forged AND received by the other BP, over the same judgeable
    # window as TOTAL_FORGES above.
    CANONICAL=$(awk -F, -v last="$LAST_OBS" '
        NR>1 && $3=="forge"  && $2=="dugite-bp"  && $4+0<=last {f[$4","$5]=1}
        NR>1 && $3=="recv"   && $2=="cardano-bp" {r[$4","$5]=1}
        END { c=0; for (k in f) if (k in r) c++; print c }
    ' "$EVD/blocks.csv")
    ORPHANS=$((TOTAL_FORGES - CANONICAL))
    if [ "$TOTAL_FORGES" -gt 0 ]; then
        ORPHAN_RATE=$(awk -v o="$ORPHANS" -v t="$TOTAL_FORGES" 'BEGIN{printf "%.4f", o/t}')
        # Anomaly check
        awk -v r="$ORPHAN_RATE" -v m="$ORPHAN_RATE_MAX" 'BEGIN{exit !(r > m)}' \
            && ANOMALIES+=("orphan_rate=$ORPHAN_RATE exceeds threshold $ORPHAN_RATE_MAX") || true
    fi
fi

# --- Tip-age stats -----------------------------------------------------------
TIP_AGE_AVG="?"; TIP_AGE_P99="?"
if [ -f "$EVD/tip-age-samples.csv" ]; then
    SORTED=$(awk -F, 'NR>1 && $3 ~ /^[0-9.]+$/ {print $3+0}' "$EVD/tip-age-samples.csv" | sort -n)
    if [ -n "$SORTED" ]; then
        read TIP_AGE_AVG TIP_AGE_P99 < <(
            printf '%s\n' "$SORTED" | awk '
                {a[NR]=$1; sum+=$1}
                END{
                    n=NR
                    p99idx=int(n*0.99+0.5); if(p99idx<1) p99idx=1; if(p99idx>n) p99idx=n
                    printf "%.2f %.2f\n", (n>0?sum/n:0), a[p99idx]
                }')
    else
        TIP_AGE_AVG=0; TIP_AGE_P99=0
    fi
    awk -v p="$TIP_AGE_P99" -v m="$TIP_AGE_P99_MAX" 'BEGIN{exit !(p > m)}' \
        && ANOMALIES+=("tip_age_p99=${TIP_AGE_P99}s exceeds threshold ${TIP_AGE_P99_MAX}s") || true
fi

# --- Chain density -----------------------------------------------------------
DENSITY="?"
if [ -f "$EVD/tip-samples.csv" ] && [ -f "$EVD/blocks.csv" ]; then
    # Density ≈ canonical blocks / slots elapsed
    SLOT_FIRST=$(awk -F, 'NR>1 && $3 ~ /^[0-9]+$/ {print $3; exit}' "$EVD/tip-samples.csv")
    SLOT_LAST=$(awk -F, 'NR>1 && $3 ~ /^[0-9]+$/ {s=$3} END{print s}' "$EVD/tip-samples.csv")
    if [ -n "$SLOT_FIRST" ] && [ -n "$SLOT_LAST" ] && [ "$SLOT_LAST" -gt "$SLOT_FIRST" ]; then
        SLOTS=$((SLOT_LAST - SLOT_FIRST))
        DENSITY=$(awk -v b="$CANONICAL" -v s="$SLOTS" 'BEGIN{printf "%.3f", b/s}')

        # SCALE THE TOLERANCE TO THE SAMPLE SIZE. Block production here is Bernoulli per
        # slot with p = activeSlotsCoeff (the forger holds ~all the stake, so the Praos
        # leader probability 1-(1-f)^sigma tends to f), which makes the block count
        # binomial. A FIXED +/-20% band therefore means completely different confidence
        # at different window sizes:
        #
        #   n=1812 slots (Round 2)   1 sd = 0.012 density   =>  +/-0.1 is ~8 sd, far too loose
        #   n=54    slots (Round 3)  1 sd = 0.068 density   =>  +/-0.1 is ~1.5 sd, too tight
        #
        # At ~1.5 sd a perfectly healthy run fails roughly one time in eight. Measured:
        # Round 3 reported density 0.630 from 34 canonical blocks in 54 slots — 1.9 sd,
        # ordinary noise — with 0 orphans, every block triple-observed, and 0 ERROR on
        # all three nodes. That is the #917 forge-stall defect exactly: a per-sample coin
        # flip presented as a threshold, and the same remedy applies — derive the budget
        # from the distribution instead of guessing a percentage.
        #
        # z = 3.29 is the two-sided p99.9 quantile, the same confidence #917 chose, so a
        # healthy run fails about one time in a thousand. Note this makes the check
        # STRICTER where the sample supports it: Round 2's band tightens from
        # [0.400,0.600] to about [0.461,0.539].
        DENSITY_Z=${DENSITY_Z:-3.29}
        read LOW HIGH < <(awk -v f="$ACTIVE_SLOT_COEFF" -v n="$SLOTS" -v z="$DENSITY_Z" 'BEGIN{
            sd = sqrt(n * f * (1 - f)) / n      # sd of the DENSITY estimate
            lo = f - z * sd; hi = f + z * sd
            if (lo < 0) lo = 0; if (hi > 1) hi = 1
            printf "%.3f %.3f\n", lo, hi
        }')
        awk -v d="$DENSITY" -v l="$LOW" -v h="$HIGH" 'BEGIN{exit !(d < l || d > h)}' \
            && ANOMALIES+=("chain_density=$DENSITY outside [${LOW},${HIGH}] (n=${SLOTS} slots, p99.9 binomial)") || true
    fi
fi

# --- Log error histogram -----------------------------------------------------
declare -A ERR_COUNT WARN_COUNT UNEXPLAINED_COUNT
# Build one alternation from the allowlist, ignoring comments and blank lines.
ALLOW_RE=""
if [ -n "$ALLOWED_ERRORS" ]; then
    ALLOW_RE=$(grep -vE '^[[:space:]]*(#|$)' "$ALLOWED_ERRORS" | paste -sd'|' - || true)
fi
for node in dugite-bp dugite-relay cardano-bp; do
    log="$LOG_DIR/$node.log"
    [ -f "$log" ] || continue
    ec=$(count_log_errors "$log")
    wc=$(count_log_warns "$log")
    ERR_COUNT[$node]=$ec
    WARN_COUNT[$node]=$wc

    # Unexplained = ERROR lines that no allowlist pattern accounts for. With no
    # allowlist this equals the raw count, so behaviour is unchanged by default.
    unexplained=$ec
    if [ "$ec" -gt 0 ] && [ -n "$ALLOW_RE" ]; then
        unexplained=$(grep -E ' ERROR ' "$log" | grep -cvE "$ALLOW_RE" || true)
        unexplained=${unexplained:-0}
    fi
    UNEXPLAINED_COUNT[$node]=$unexplained

    # The invalid-block check is deliberately OUTSIDE the allowlist and outside the
    # `unexplained` gate: a dugite-forged block that Haskell rejected is the failure
    # this whole harness exists to detect, and no allowlist may suppress it.
    if grep -qE 'TraceForgedInvalidBlock|AddBlockValidation\.InvalidBlock|Forge\.Loop\.ForgedInvalidBlock' "$log"; then
        ANOMALIES+=("CRITICAL: invalid-block event in $node.log — Haskell rejected a dugite-forged block")
    fi
    if [ "$unexplained" -gt 0 ]; then
        ANOMALIES+=("$node: $unexplained unexplained ERROR line(s) (of $ec total)")
    fi
done

# --- Boot timing -------------------------------------------------------------
BOOT_RELAY="?"; BOOT_DUGITE_BP="?"; BOOT_CARDANO="?"
# (Best-effort: look for first forge / first adopt in the snapshot logs.)
if [ -f "$LOG_DIR/dugite-bp.log" ]; then
    BOOT_DUGITE_BP=$(awk '/Forged block|forge slot=/ {print NR; exit}' "$LOG_DIR/dugite-bp.log")
    BOOT_DUGITE_BP="${BOOT_DUGITE_BP:-?}"
fi

# --- Emit report -------------------------------------------------------------
cat <<EOF

────────────────────────────────────────────────────────────────────
devnet-validate — evidence analysis: $TS
git=$GIT_REV  cardano-node=$CN_VER  cardano-cli=$CCLI_VER  duration=${DUR}s

Blocks
  total forges            : $TOTAL_FORGES  (judgeable: forged at or below cardano-bp's last observed slot ${LAST_OBS:-?})
  canonical               : $CANONICAL
  orphans                 : $ORPHANS  (rate=$ORPHAN_RATE, threshold≤$ORPHAN_RATE_MAX)
  unjudgeable (tail)      : ${UNJUDGEABLE:-0}  (forged after sampling stopped — excluded, never silently)
  chain_density           : $DENSITY  (p99.9 binomial band [${LOW:-?},${HIGH:-?}] over n=${SLOTS:-?} slots, p=$ACTIVE_SLOT_COEFF)

Tip-age
  avg                     : ${TIP_AGE_AVG}s
  p99                     : ${TIP_AGE_P99}s  (threshold ≤ ${TIP_AGE_P99_MAX}s)

Log errors / warns
  dugite-bp               : ${ERR_COUNT[dugite-bp]:-?} ERROR (${UNEXPLAINED_COUNT[dugite-bp]:-?} unexplained) / ${WARN_COUNT[dugite-bp]:-?} WARN
  dugite-relay            : ${ERR_COUNT[dugite-relay]:-?} ERROR (${UNEXPLAINED_COUNT[dugite-relay]:-?} unexplained) / ${WARN_COUNT[dugite-relay]:-?} WARN
  cardano-bp              : ${ERR_COUNT[cardano-bp]:-?} ERROR (${UNEXPLAINED_COUNT[cardano-bp]:-?} unexplained) / ${WARN_COUNT[cardano-bp]:-?} WARN
  allowlist               : ${ALLOWED_ERRORS:-none}

EOF

if [ ${#ANOMALIES[@]} -eq 0 ]; then
    echo "Result: NO ANOMALIES"
    echo "────────────────────────────────────────────────────────────────────"
    exit 0
else
    echo "Anomalies (${#ANOMALIES[@]}):"
    for a in "${ANOMALIES[@]}"; do echo "  - $a"; done
    echo "────────────────────────────────────────────────────────────────────"
    exit 1
fi
