#!/usr/bin/env bash
# Summarise a testnet/local-devnet/evidence/<ts>/ directory into a single
# human-readable report block. Exits non-zero if any anomaly threshold is
# breached — usable as a CI gate.
#
# Usage: analyze-evidence.sh <evidence_dir>
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Shared level-token counting — keeps this analyzer in exact agreement with
# generate-release-report.sh (#916).
# shellcheck source=lib/log-level-counts.sh
source "$SCRIPT_DIR/lib/log-level-counts.sh"

EVD="${1:-}"
if [ -z "$EVD" ] || ! [ -d "$EVD" ]; then
    echo "Usage: $0 <evidence_dir>" >&2
    echo "  e.g. $0 testnet/local-devnet/evidence/20260521T120000Z" >&2
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
    TOTAL_FORGES=$(awk -F, 'NR>1 && $3=="forge" {print $4","$5}' "$EVD/blocks.csv" | sort -u | grep -c '^' || true)
    # Canonical = forged AND received by the other BP
    CANONICAL=$(awk -F, '
        NR>1 && $3=="forge"  && $2=="dugite-bp"  {f[$4","$5]=1}
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
        LOW=$(awk -v f="$ACTIVE_SLOT_COEFF" -v t="$DENSITY_TOLERANCE" 'BEGIN{printf "%.3f", f*(1-t)}')
        HIGH=$(awk -v f="$ACTIVE_SLOT_COEFF" -v t="$DENSITY_TOLERANCE" 'BEGIN{printf "%.3f", f*(1+t)}')
        awk -v d="$DENSITY" -v l="$LOW" -v h="$HIGH" 'BEGIN{exit !(d < l || d > h)}' \
            && ANOMALIES+=("chain_density=$DENSITY outside [${LOW},${HIGH}]") || true
    fi
fi

# --- Log error histogram -----------------------------------------------------
declare -A ERR_COUNT WARN_COUNT
for node in dugite-bp dugite-relay cardano-bp; do
    log="$LOG_DIR/$node.log"
    [ -f "$log" ] || continue
    ec=$(count_log_errors "$log")
    wc=$(count_log_warns "$log")
    ERR_COUNT[$node]=$ec
    WARN_COUNT[$node]=$wc
    if [ "$ec" -gt 0 ]; then
        # Forged-invalid is always fatal-class — flag whichever name appears.
        if grep -qE 'TraceForgedInvalidBlock|AddBlockValidation\.InvalidBlock|Forge\.Loop\.ForgedInvalidBlock' "$log"; then
            ANOMALIES+=("CRITICAL: invalid-block event in $node.log — Haskell rejected a dugite-forged block")
        fi
        ANOMALIES+=("$node: $ec ERROR/invalid-block lines")
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
  total forges            : $TOTAL_FORGES
  canonical               : $CANONICAL
  orphans                 : $ORPHANS  (rate=$ORPHAN_RATE, threshold≤$ORPHAN_RATE_MAX)
  chain_density           : $DENSITY  (target ≈ $ACTIVE_SLOT_COEFF ± ${DENSITY_TOLERANCE})

Tip-age
  avg                     : ${TIP_AGE_AVG}s
  p99                     : ${TIP_AGE_P99}s  (threshold ≤ ${TIP_AGE_P99_MAX}s)

Log errors / warns
  dugite-bp               : ${ERR_COUNT[dugite-bp]:-?} ERROR / ${WARN_COUNT[dugite-bp]:-?} WARN
  dugite-relay            : ${ERR_COUNT[dugite-relay]:-?} ERROR / ${WARN_COUNT[dugite-relay]:-?} WARN
  cardano-bp              : ${ERR_COUNT[cardano-bp]:-?} ERROR / ${WARN_COUNT[cardano-bp]:-?} WARN

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
