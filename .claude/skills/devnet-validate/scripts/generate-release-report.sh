#!/usr/bin/env bash
# generate-release-report.sh — produce report.json + report.md from one or
# more devnet-validate evidence directories.
#
# Usage:
#   generate-release-report.sh [OPTIONS] <evidence_dir> [evidence_dir2 ...]
#
# Options:
#   --preset smoke|standard|extended   capability preset (default: standard)
#   --tag <v1.7.0>                     release tag; null if not a release gate
#   --tx-zoo-state <path>              path to tx-zoo/state/ (results.csv)
#   --previous-report <report.json>    prior release report for trend diff
#   --output-dir <dir>                 where to write report.json + report.md
#                                      (default: current directory)
#   --round-names <name1,name2,...>    comma-separated round names aligned with
#                                      evidence dirs (default: baseline,epoch-boundary,restart)
#
# Evidence dirs must each contain: metadata.json, blocks.csv, tip-samples.csv,
# tip-age-samples.csv, tx-submissions.csv.  An in-dir report.md (written by
# verify.sh) is parsed for predicate pass/fail when present.
#
# Writes:
#   <output-dir>/report.json   — schema: schemas/report.v1.json
#   <output-dir>/report.md     — GitHub-release-ready Markdown
#
# Exit codes: 0 = all rounds passed; 1 = one or more rounds failed; 2 = usage error.
set -euo pipefail

# ---- Defaults ----------------------------------------------------------------
PRESET="standard"
RELEASE_TAG="null"
TX_ZOO_STATE=""
PREV_REPORT=""
OUTPUT_DIR="."
ROUND_NAMES_CSV="baseline,epoch-boundary,restart"
EVIDENCE_DIRS=()

# ---- Arg parsing -------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --preset)          PRESET="$2";          shift 2 ;;
        --tag)             RELEASE_TAG="\"$2\""; shift 2 ;;
        --tx-zoo-state)    TX_ZOO_STATE="$2";    shift 2 ;;
        --previous-report) PREV_REPORT="$2";     shift 2 ;;
        --output-dir)      OUTPUT_DIR="$2";      shift 2 ;;
        --round-names)     ROUND_NAMES_CSV="$2"; shift 2 ;;
        -*)                echo "Unknown option: $1" >&2; exit 2 ;;
        *)                 EVIDENCE_DIRS+=("$1"); shift ;;
    esac
done

if [ ${#EVIDENCE_DIRS[@]} -eq 0 ]; then
    echo "Usage: $0 [OPTIONS] <evidence_dir> [evidence_dir2 ...]" >&2
    exit 2
fi

mkdir -p "$OUTPUT_DIR"

# ---- Helpers -----------------------------------------------------------------
_jq_str()  { printf '%s' "$1" | jq -R .; }
_jq_num()  { printf '%s' "$1" | jq -e 'tonumber' 2>/dev/null || echo "null"; }
_jq_bool() { [ "$1" = "true" ] && echo "true" || echo "false"; }

# Split comma-separated round names into an indexed array
IFS=',' read -ra ROUND_NAMES <<< "$ROUND_NAMES_CSV"

TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

# ---- Per-round extraction ----------------------------------------------------
# Returns JSON object for one round.
process_round() {
    local idx="$1"
    local evd="$2"
    local name="${ROUND_NAMES[$idx]:-round$idx}"

    local metadata_file="$evd/metadata.json"
    local blocks_file="$evd/blocks.csv"
    local tips_file="$evd/tip-samples.csv"
    local tip_age_file="$evd/tip-age-samples.csv"
    local txs_file="$evd/tx-submissions.csv"
    local logs_dir="$evd/logs"

    # --- Metadata ---
    local git_rev="unknown" cn_ver="unknown" ccli_ver="unknown" duration="null"
    if [ -f "$metadata_file" ]; then
        git_rev=$(jq -r '.dugite_node_git  // "unknown"' "$metadata_file")
        cn_ver=$(jq -r '.cardano_node_version // "unknown"' "$metadata_file")
        ccli_ver=$(jq -r '.cardano_cli_version // "unknown"' "$metadata_file")
        duration=$(jq -r '.duration_seconds // "null"' "$metadata_file")
    fi

    # --- Block accounting ---
    local total_forges=0 canonical=0 orphans=0 orphan_rate=0
    if [ -f "$blocks_file" ] && [ -s "$blocks_file" ]; then
        total_forges=$(awk -F, 'NR>1 && $3=="forge" {print $4","$5}' "$blocks_file" | sort -u | grep -c '^' || echo 0)
        canonical=$(awk -F, '
            NR>1 && $3=="forge"  && $2=="dugite-bp"  {f[$4","$5]=1}
            NR>1 && $3=="recv"   && $2=="cardano-bp" {r[$4","$5]=1}
            END { c=0; for (k in f) if (k in r) c++; print c }
        ' "$blocks_file")
        orphans=$(( total_forges - canonical ))
        if [ "$total_forges" -gt 0 ]; then
            orphan_rate=$(awk -v o="$orphans" -v t="$total_forges" 'BEGIN{printf "%.4f", o/t}')
        fi
    fi

    # --- Tip-age stats ---
    local tip_age_avg="null" tip_age_p99="null"
    if [ -f "$tip_age_file" ] && [ -s "$tip_age_file" ]; then
        local sorted
        sorted=$(awk -F, 'NR>1 && $3 ~ /^[0-9.]+$/ {print $3+0}' "$tip_age_file" | sort -n)
        if [ -n "$sorted" ]; then
            read tip_age_avg tip_age_p99 < <(
                printf '%s\n' "$sorted" | awk '
                    {a[NR]=$1; sum+=$1}
                    END{
                        n=NR
                        p99idx=int(n*0.99+0.5); if(p99idx<1) p99idx=1; if(p99idx>n) p99idx=n
                        printf "%.2f %.2f\n", (n>0?sum/n:0), a[p99idx]
                    }')
        fi
    fi

    # --- Chain density ---
    local density="null"
    if [ -f "$tips_file" ] && [ -s "$tips_file" ]; then
        local slot_first slot_last
        slot_first=$(awk -F, 'NR>1 && $3 ~ /^[0-9]+$/ {print $3; exit}' "$tips_file")
        slot_last=$(awk -F, 'NR>1 && $3 ~ /^[0-9]+$/ {s=$3} END{print s}' "$tips_file")
        if [ -n "${slot_first:-}" ] && [ -n "${slot_last:-}" ] && [ "$slot_last" -gt "$slot_first" ]; then
            local slots=$(( slot_last - slot_first ))
            density=$(awk -v b="$canonical" -v s="$slots" 'BEGIN{printf "%.3f", b/s}')
        fi
    fi

    # --- Tx-zoo for this round ---
    local tz_pass=0 tz_fail=0 tz_skip=0 tz_total=0
    # Check per-round tx state if round-specific dir exists, fallback to global
    for candidate in "$evd/tx-results.csv" "$TX_ZOO_STATE/results.csv"; do
        if [ -f "$candidate" ] && [ -s "$candidate" ]; then
            tz_pass=$(awk -F, 'NR>1 && $2=="PASS" {c++} END{print c+0}' "$candidate")
            tz_fail=$(awk -F, 'NR>1 && $2=="FAIL" {c++} END{print c+0}' "$candidate")
            tz_skip=$(awk -F, 'NR>1 && $2=="SKIP" {c++} END{print c+0}' "$candidate")
            tz_total=$(awk -F, 'NR>1 {c++} END{print c+0}' "$candidate")
            break
        fi
    done

    # --- Predicate pass/fail (parse from verify.sh's in-dir report.md) ---
    local p1="null" p2="null" p3="null" p4="null" p5="null"
    local in_dir_report="$evd/report.md"
    if [ -f "$in_dir_report" ]; then
        # Extract "PASS" or "FAIL" from the predicate table rows
        p1=$(awk -F'|' '/\| p1 /{gsub(/ /,""); if($4~/PASS/) print "true"; else print "false"; exit}' "$in_dir_report")
        p2=$(awk -F'|' '/\| p2 /{gsub(/ /,""); if($4~/PASS/) print "true"; else print "false"; exit}' "$in_dir_report")
        p3=$(awk -F'|' '/\| p3 /{gsub(/ /,""); if($4~/PASS/) print "true"; else print "false"; exit}' "$in_dir_report")
        p4=$(awk -F'|' '/\| p4 /{gsub(/ /,""); if($4~/PASS/) print "true"; else print "false"; exit}' "$in_dir_report")
        p5=$(awk -F'|' '/\| p5 /{gsub(/ /,""); if($4~/PASS/) print "true"; else print "false"; exit}' "$in_dir_report")
        # Null-safe: empty string → null
        p1="${p1:-null}"; p2="${p2:-null}"; p3="${p3:-null}"; p4="${p4:-null}"; p5="${p5:-null}"
    fi

    # Round passes iff all known predicates pass and none failed
    local round_pass="true"
    for pv in "$p1" "$p2" "$p3" "$p4" "$p5"; do
        [ "$pv" = "false" ] && round_pass="false"
    done
    # If no predicates could be parsed (no in-dir report), fall back conservatively
    if [ "$p1" = "null" ] && [ "$p2" = "null" ] && [ "$p3" = "null" ] && [ "$p4" = "null" ] && [ "$p5" = "null" ]; then
        round_pass="null"
    fi

    # --- Log error/warn counts ---
    local log_json="{}"
    if [ -d "$logs_dir" ]; then
        log_json="{"
        local first=1
        for node in dugite-bp dugite-relay cardano-bp; do
            local log="$logs_dir/$node.log"
            [ -f "$log" ] || continue
            local ec wc
            ec=$(grep -ciE 'ERROR|panicked|TraceForgedInvalidBlock' "$log" || echo 0)
            wc=$(grep -ciE 'WARN|stale intersection' "$log" || echo 0)
            [ "$first" -eq 0 ] && log_json+=","
            log_json+="\"$node\":{\"errors\":$ec,\"warns\":$wc}"
            first=0
        done
        log_json+="}"
    fi

    # --- Anomalies list ---
    local anomalies_json="[]"
    if [ -f "$in_dir_report" ]; then
        # Check for FAIL predicates in the report to populate anomalies
        local anom_list=()
        while IFS= read -r line; do
            if echo "$line" | grep -q '\*\*FAIL\*\*'; then
                local detail
                detail=$(echo "$line" | sed 's/.*\*\*FAIL\*\* | //')
                anom_list+=("$(printf '%s' "$detail" | jq -R .)")
            fi
        done < "$in_dir_report"
        if [ ${#anom_list[@]} -gt 0 ]; then
            anomalies_json="[$(IFS=,; echo "${anom_list[*]}")]"
        fi
    fi

    # --- CLI parity summary ---
    local cli_parity_equal=0 cli_parity_divergent=0 cli_parity_skip=0 cli_parity_error=0
    local parity_csv="$evd/cli-parity.csv"
    if [ -f "$parity_csv" ] && [ -s "$parity_csv" ]; then
        cli_parity_equal=$(awk -F, 'NR>1 && $5=="true" {c++} END{print c+0}' "$parity_csv")
        cli_parity_divergent=$(awk -F, 'NR>1 && $5=="false" && $6!~/skip|known-divergence/ {c++} END{print c+0}' "$parity_csv")
        cli_parity_skip=$(awk -F, 'NR>1 && ($6~/^skip/ || $2~/\//) {c++} END{print c+0}' "$parity_csv" || echo 0)
        cli_parity_error=$(awk -F, 'NR>1 && $6~/^error|ERROR/ {c++} END{print c+0}' "$parity_csv" || echo 0)
    fi

    # --- Epoch transitions ---
    local epoch_transitions="null"
    if [ -d "$logs_dir" ] && [ -f "$logs_dir/dugite-bp.log" ]; then
        epoch_transitions=$(grep -ciE 'epoch transition|EpochTransition|epoch_transition' \
            "$logs_dir/dugite-bp.log" 2>/dev/null || echo "null")
    fi

    # --- Relative evidence path ---
    local rel_evd
    rel_evd=$(realpath --relative-to="$(pwd)" "$evd" 2>/dev/null || echo "$evd")

    cat <<ROUND_JSON
{
  "name": $(printf '%s' "$name" | jq -R .),
  "pass": $round_pass,
  "evidence_dir": $(printf '%s' "$rel_evd" | jq -R .),
  "duration_seconds": $duration,
  "git_rev": $(printf '%s' "$git_rev" | jq -R .),
  "cardano_node_version": $(printf '%s' "$cn_ver" | jq -R .),
  "cardano_cli_version": $(printf '%s' "$ccli_ver" | jq -R .),
  "blocks": {
    "total_forges": $total_forges,
    "canonical": $canonical,
    "orphans": $orphans,
    "orphan_rate": $orphan_rate
  },
  "tip_age": {
    "avg_seconds": $tip_age_avg,
    "p99_seconds": $tip_age_p99
  },
  "chain_density": $density,
  "tx_zoo": {
    "pass": $tz_pass,
    "fail": $tz_fail,
    "skip": $tz_skip,
    "total": $tz_total
  },
  "predicates": {
    "p1_forge_cross_check": $p1,
    "p2_per_bp_attribution": $p2,
    "p3_tx_inclusion": $p3,
    "p4_tip_parity": $p4,
    "p5_tip_age": $p5
  },
  "log_errors": $log_json,
  "anomalies": $anomalies_json,
  "epoch_transitions_observed": $epoch_transitions,
  "cli_parity": {
    "equal": $cli_parity_equal,
    "divergent": $cli_parity_divergent,
    "skip": $cli_parity_skip,
    "error": $cli_parity_error
  }
}
ROUND_JSON
}

# ---- Build rounds array ------------------------------------------------------
ROUNDS_JSON="["
SUMMARY_PASS=true
ROUNDS_PASS=0; ROUNDS_FAIL=0
TOTAL_CANONICAL=0; TOTAL_TZ_PASS=0; TOTAL_TZ_FAIL=0
TOTAL_INVALID_FORGES=0; TOTAL_CRIT=0
FIRST_GIT_REV="unknown"; FIRST_CN_VER="unknown"; FIRST_CCLI_VER="unknown"

for i in "${!EVIDENCE_DIRS[@]}"; do
    [ "$i" -gt 0 ] && ROUNDS_JSON+=","
    round_json=$(process_round "$i" "${EVIDENCE_DIRS[$i]}")
    ROUNDS_JSON+="$round_json"

    # Accumulate summary stats
    r_pass=$(echo "$round_json" | jq -r '.pass // "null"')
    r_canon=$(echo "$round_json" | jq -r '.blocks.canonical // 0')
    r_tz_pass=$(echo "$round_json" | jq -r '.tx_zoo.pass // 0')
    r_tz_fail=$(echo "$round_json" | jq -r '.tx_zoo.fail // 0')

    if [ "$r_pass" = "false" ]; then
        SUMMARY_PASS=false
        ROUNDS_FAIL=$(( ROUNDS_FAIL + 1 ))
    elif [ "$r_pass" = "true" ]; then
        ROUNDS_PASS=$(( ROUNDS_PASS + 1 ))
    fi
    TOTAL_CANONICAL=$(( TOTAL_CANONICAL + r_canon ))
    TOTAL_TZ_PASS=$(( TOTAL_TZ_PASS + r_tz_pass ))
    TOTAL_TZ_FAIL=$(( TOTAL_TZ_FAIL + r_tz_fail ))

    # Grab versions from first round
    if [ "$i" -eq 0 ]; then
        FIRST_GIT_REV=$(echo "$round_json" | jq -r '.git_rev // "unknown"')
        FIRST_CN_VER=$(echo "$round_json" | jq -r '.cardano_node_version // "unknown"')
        FIRST_CCLI_VER=$(echo "$round_json" | jq -r '.cardano_cli_version // "unknown"')
    fi
done
ROUNDS_JSON+="]"

# ---- Trend comparison --------------------------------------------------------
TREND_JSON="null"
if [ -n "$PREV_REPORT" ] && [ -f "$PREV_REPORT" ]; then
    prev_tag=$(jq -r '.release_tag // "unknown"' "$PREV_REPORT")
    prev_canonical=$(jq -r '.summary.total_canonical_blocks // 0' "$PREV_REPORT")
    prev_tz_pass=$(jq -r '.summary.total_tx_zoo_pass // 0' "$PREV_REPORT")

    canon_delta=$(( TOTAL_CANONICAL - prev_canonical ))
    tz_delta=$(( TOTAL_TZ_PASS - prev_tz_pass ))

    # Regression: predicate passed in prev but fails now
    REGRESSIONS_JSON="[]"
    IMPROVEMENTS_JSON="[]"

    # Compare per-predicate across first matching rounds
    for pred in p1_forge_cross_check p2_per_bp_attribution p3_tx_inclusion p4_tip_parity p5_tip_age; do
        cur_val=$(echo "$ROUNDS_JSON" | jq -r ".[0].predicates.$pred // \"null\"")
        prev_val=$(jq -r ".rounds[0].predicates.$pred // \"null\"" "$PREV_REPORT")
        if [ "$prev_val" = "true" ] && [ "$cur_val" = "false" ]; then
            REGRESSIONS_JSON=$(echo "$REGRESSIONS_JSON" | jq ". + [\"$pred\"]")
        elif [ "$prev_val" = "false" ] && [ "$cur_val" = "true" ]; then
            IMPROVEMENTS_JSON=$(echo "$IMPROVEMENTS_JSON" | jq ". + [\"$pred\"]")
        fi
    done

    TREND_JSON=$(cat <<TREND
{
  "baseline_tag": $(printf '%s' "$prev_tag" | jq -R .),
  "canonical_blocks_delta": $canon_delta,
  "tx_zoo_pass_delta": $tz_delta,
  "regressions": $REGRESSIONS_JSON,
  "improvements": $IMPROVEMENTS_JSON
}
TREND
)
fi

# ---- Assemble report.json ----------------------------------------------------
REPORT_JSON=$(cat <<REPORT
{
  "schema_version": 1,
  "timestamp": $(printf '%s' "$TIMESTAMP" | jq -R .),
  "git_rev": $(printf '%s' "$FIRST_GIT_REV" | jq -R .),
  "release_tag": $RELEASE_TAG,
  "preset": $(printf '%s' "$PRESET" | jq -R .),
  "cardano_node_version": $(printf '%s' "$FIRST_CN_VER" | jq -R .),
  "cardano_cli_version": $(printf '%s' "$FIRST_CCLI_VER" | jq -R .),
  "rounds": $ROUNDS_JSON,
  "summary": {
    "pass": $SUMMARY_PASS,
    "rounds_pass": $ROUNDS_PASS,
    "rounds_fail": $ROUNDS_FAIL,
    "total_canonical_blocks": $TOTAL_CANONICAL,
    "total_tx_zoo_pass": $TOTAL_TZ_PASS,
    "total_tx_zoo_fail": $TOTAL_TZ_FAIL,
    "invalid_forges_detected": $TOTAL_INVALID_FORGES,
    "critical_anomalies": $TOTAL_CRIT
  },
  "trend": $TREND_JSON
}
REPORT
)

# Validate the JSON is well-formed before writing
echo "$REPORT_JSON" | jq . > /dev/null 2>&1 || {
    echo "ERROR: generated malformed JSON — writing raw output for debugging" >&2
    echo "$REPORT_JSON" > "$OUTPUT_DIR/report.json.raw"
    exit 1
}

echo "$REPORT_JSON" | jq . > "$OUTPUT_DIR/report.json"

# ---- Assemble report.md ------------------------------------------------------
OVERALL_BADGE=$([ "$SUMMARY_PASS" = "true" ] && echo "✅ PASS" || echo "❌ FAIL")

{
cat <<HEADER
# devnet-validate report

| | |
|---|---|
| **Result** | $OVERALL_BADGE |
| **Git** | \`$FIRST_GIT_REV\` |
| **Preset** | $PRESET |
| **cardano-node** | $FIRST_CN_VER |
| **cardano-cli** | $FIRST_CCLI_VER |
| **Generated** | $TIMESTAMP |

## Round summary

| Round | Result | Canonical blocks | Tx-zoo | CLI parity | Tip-age p99 | Chain density |
|---|---|---|---|---|---|---|
HEADER

for i in "${!EVIDENCE_DIRS[@]}"; do
    rname="${ROUND_NAMES[$i]:-round$i}"
    r_pass=$(echo "$ROUNDS_JSON"  | jq -r ".[$i].pass // \"null\"")
    r_can=$(echo "$ROUNDS_JSON"   | jq -r ".[$i].blocks.canonical // 0")
    r_tz_p=$(echo "$ROUNDS_JSON"  | jq -r ".[$i].tx_zoo.pass // 0")
    r_tz_t=$(echo "$ROUNDS_JSON"  | jq -r ".[$i].tx_zoo.total // 0")
    r_cp_e=$(echo "$ROUNDS_JSON"  | jq -r ".[$i].cli_parity.equal // \"?\"")
    r_cp_d=$(echo "$ROUNDS_JSON"  | jq -r ".[$i].cli_parity.divergent // 0")
    r_tip=$(echo "$ROUNDS_JSON"   | jq -r ".[$i].tip_age.p99_seconds // \"?\"")
    r_dens=$(echo "$ROUNDS_JSON"  | jq -r ".[$i].chain_density // \"?\"")
    badge=$([ "$r_pass" = "true" ] && echo "✅ PASS" || echo "❌ FAIL")
    cp_badge=$([ "${r_cp_d:-0}" -eq 0 ] && echo "✅ ${r_cp_e}✓" || echo "❌ ${r_cp_d}✗")
    echo "| $rname | $badge | $r_can | $r_tz_p/$r_tz_t | $cp_badge | ${r_tip}s | $r_dens |"
done

cat <<TOTALS

**Total canonical blocks:** $TOTAL_CANONICAL
**Total tx-zoo pass/fail:** $TOTAL_TZ_PASS / $TOTAL_TZ_FAIL

TOTALS

# Predicate breakdown per round
echo "## Predicate breakdown"
echo
for i in "${!EVIDENCE_DIRS[@]}"; do
    rname="${ROUND_NAMES[$i]:-round$i}"
    echo "### Round: $rname"
    echo
    echo "| Predicate | Result | Detail |"
    echo "|---|---|---|"
    in_rpt="${EVIDENCE_DIRS[$i]}/report.md"
    if [ -f "$in_rpt" ]; then
        awk -F'|' '
            /^\| p[0-9]/ {
                id=$2; name=$3; result=$4; detail=$5
                gsub(/^ +| +$/, "", id)
                gsub(/^ +| +$/, "", name)
                gsub(/^ +| +$/, "", result)
                gsub(/^ +| +$/, "", detail)
                printf "| %s — %s | %s | %s |\n", id, name, result, detail
            }
        ' "$in_rpt"
    else
        echo "| (no in-round report.md found) | — | run verify.sh first |"
    fi
    echo
done

# Trend section
if [ "$TREND_JSON" != "null" ]; then
    prev_tag=$(echo "$TREND_JSON" | jq -r '.baseline_tag // "unknown"')
    canon_delta=$(echo "$TREND_JSON" | jq -r '.canonical_blocks_delta // "?"')
    tz_delta=$(echo "$TREND_JSON" | jq -r '.tx_zoo_pass_delta // "?"')
    regressions=$(echo "$TREND_JSON" | jq -r '.regressions | join(", ")' 2>/dev/null || echo "")
    improvements=$(echo "$TREND_JSON" | jq -r '.improvements | join(", ")' 2>/dev/null || echo "")

    cat <<TREND_SECTION
## Trend vs $prev_tag

| Metric | Delta |
|---|---|
| Canonical blocks | $( [ "$canon_delta" -ge 0 ] 2>/dev/null && echo "+$canon_delta" || echo "$canon_delta" ) |
| Tx-zoo pass | $( [ "$tz_delta" -ge 0 ] 2>/dev/null && echo "+$tz_delta" || echo "$tz_delta" ) |

TREND_SECTION
    if [ -n "$regressions" ]; then
        echo "**Regressions:** $regressions"
    fi
    if [ -n "$improvements" ]; then
        echo "**Improvements:** $improvements"
    fi
fi

cat <<FOOTER

---
*Generated by \`.claude/skills/devnet-validate/scripts/generate-release-report.sh\`*
*Schema: \`.claude/skills/devnet-validate/schemas/report.v1.json\`*
FOOTER
} > "$OUTPUT_DIR/report.md"

echo "report.json → $OUTPUT_DIR/report.json" >&2
echo "report.md   → $OUTPUT_DIR/report.md"   >&2

# Exit non-zero if any round failed
[ "$SUMMARY_PASS" = "true" ] && exit 0 || exit 1
