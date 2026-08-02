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
#   --round-names <name1,name2,...>    comma-separated round names
#   --no-strict                        DO NOT enforce the preset evidence
#                                      manifest. The omission is recorded in
#                                      gate_integrity, and the report is marked
#                                      inadmissible as a release gate.
#   --denominators <file>              pinned denominator manifest
#                                      (default: ../schemas/denominators.json)
#   --schema <file>                    JSON schema to validate output against
#                                      (default: ../schemas/report.v2.json)
#
# GATE INTEGRITY (#953)
# ---------------------
# The failure this script exists to prevent is the #945/#923 shape: a suite that
# never ran being reported as a row of zeros, which reads identically to "ran
# and found nothing wrong". Three rules enforce that here:
#
#   1. Every suite block carries a `status`. An absent CSV yields
#      status="absent" with NULL counts — never 0.
#   2. The preset declares which artifacts must exist (PRESET_MANIFEST below).
#      In strict mode (the default) a missing one is a hard failure, not a
#      warning, and never a zero.
#   3. Counts are checked against denominators that come from OUTSIDE the run
#      (schemas/denominators.json). "26/26" computed by counting the rows you
#      produced is tautological; "26 of a pinned 26" is not.
#
# Exit codes:
#   0 = all rounds passed and gate integrity held
#   1 = one or more rounds failed a predicate
#   2 = usage error
#   3 = gate integrity failure (required evidence absent / short / shared)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Shared level-token counting — keeps log_errors/log_warns in exact agreement
# with analyze-evidence.sh (#916).
# shellcheck source=lib/log-level-counts.sh
source "$SCRIPT_DIR/lib/log-level-counts.sh"

# ---- Defaults ----------------------------------------------------------------
PRESET="standard"
RELEASE_TAG="null"
TX_ZOO_STATE=""
PREV_REPORT=""
OUTPUT_DIR="."
ROUND_NAMES_CSV="baseline,epoch-boundary,restart"
STRICT=1
DENOM_FILE="$SCRIPT_DIR/../schemas/denominators.json"
SCHEMA_FILE="$SCRIPT_DIR/../schemas/report.v2.json"
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
        --no-strict)       STRICT=0;             shift   ;;
        --strict)          STRICT=1;             shift   ;;
        --denominators)    DENOM_FILE="$2";      shift 2 ;;
        --schema)          SCHEMA_FILE="$2";     shift 2 ;;
        -*)                echo "Unknown option: $1" >&2; exit 2 ;;
        *)                 EVIDENCE_DIRS+=("$1"); shift ;;
    esac
done

if [ ${#EVIDENCE_DIRS[@]} -eq 0 ]; then
    echo "Usage: $0 [OPTIONS] <evidence_dir> [evidence_dir2 ...]" >&2
    exit 2
fi

case "$PRESET" in
    smoke|standard|extended) ;;
    *) echo "Unknown preset: $PRESET (want smoke|standard|extended)" >&2; exit 2 ;;
esac

mkdir -p "$OUTPUT_DIR"

# ---- Pinned denominators -----------------------------------------------------
# Absent manifest is itself a gate-integrity failure in strict mode: without it
# every count reverts to being self-reported.
DENOM_OK=1
if [ ! -f "$DENOM_FILE" ]; then
    DENOM_OK=0
    echo "WARNING: denominator manifest not found: $DENOM_FILE" >&2
fi
_denom() { # _denom <jq-path> <default>
    if [ "$DENOM_OK" -eq 1 ]; then
        jq -r "$1 // $2" "$DENOM_FILE" 2>/dev/null || echo "$2"
    else
        echo "$2"
    fi
}
EXP_TX_ZOO=$(_denom '.tx_zoo.expected_scripts' 0)
EXP_CLI=$(_denom '.cli_parity.expected_queries' 0)
EXP_N2N=$(_denom '.n2n_adversarial.expected_cases' 0)
EXP_CHAOS=$(_denom '.chaos.expected_cases' 0)
EXP_RPC=$(_denom '.rpc.expected_checks' 0)

# ---- Preset evidence manifest ------------------------------------------------
# Format: "<key>|<filename>|<scope>"  where scope ∈ {every, any}
#   every — must be present in EVERY round's evidence dir
#   any   — must be present in AT LEAST ONE round (suites like cli-parity and
#           protocols run once per gate, in round 1, by design)
#
# A suite belongs here only once it HAS a driver that a preset invokes. The
# manifest must describe what the preset actually runs, otherwise the gate is
# red for a reason unrelated to the build under test. chaos-events.csv was
# held out until #959 supplied chaos/run.sh and wired it into the presets;
# it is required from standard upward now that both exist.
preset_manifest() {
    case "$1" in
        smoke)
            cat <<'EOF'
tx_results|tx-results.csv|any
EOF
            ;;
        standard)
            cat <<'EOF'
metadata|metadata.json|every
blocks|blocks.csv|every
tip_samples|tip-samples.csv|every
tip_age_samples|tip-age-samples.csv|every
tx_submissions|tx-submissions.csv|every
verify_report|report.md|every
tx_results|tx-results.csv|any
cli_parity|cli-parity.csv|any
n2n_trace|n2n-trace.csv|any
parity_matrix|parity-matrix.csv|any
chaos_events|chaos-events.csv|any
rpc|rpc.csv|any
EOF
            ;;
        extended)
            cat <<'EOF'
metadata|metadata.json|every
blocks|blocks.csv|every
tip_samples|tip-samples.csv|every
tip_age_samples|tip-age-samples.csv|every
tx_submissions|tx-submissions.csv|every
verify_report|report.md|every
tx_results|tx-results.csv|any
cli_parity|cli-parity.csv|any
n2n_trace|n2n-trace.csv|any
parity_matrix|parity-matrix.csv|any
chaos_events|chaos-events.csv|any
rpc|rpc.csv|any
throughput|throughput.csv|any
resource_samples|resource-samples.csv|any
log_anomalies|log-anomalies.csv|any
EOF
            ;;
    esac
}

# Accumulates human-readable gate-integrity violations.
MISSING=()

# ---- Helpers -----------------------------------------------------------------
IFS=',' read -ra ROUND_NAMES <<< "$ROUND_NAMES_CSV"
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

# Count data rows (excludes header) in a CSV; 0 when absent.
_rows() { [ -f "$1" ] && [ -s "$1" ] && awk 'NR>1 && NF' "$1" | wc -l | tr -d ' ' || echo 0; }

# Emit a suite status given actual vs expected. Echoes ok|short.
_status_for() { # _status_for <actual> <expected>
    if [ "${2:-0}" -gt 0 ] && [ "${1:-0}" -lt "${2}" ]; then echo "short"; else echo "ok"; fi
}

# ---- Per-round extraction ----------------------------------------------------
process_round() {
    local idx="$1"
    local evd="$2"
    local name="${ROUND_NAMES[$idx]:-round$idx}"

    local metadata_file="$evd/metadata.json"
    local blocks_file="$evd/blocks.csv"
    local tips_file="$evd/tip-samples.csv"
    local tip_age_file="$evd/tip-age-samples.csv"
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
        # Count distinct (slot,hash) forge events. Deliberately NOT
        # `... | grep -c '^' || echo 0`: grep -c prints its count AND exits 1
        # when the count is zero, so the `|| echo 0` appended a SECOND value and
        # total_forges became the literal "0\n0", which then died in the
        # arithmetic below with "syntax error (error token is 0)". The same
        # idiom had already been fixed once for epoch_transitions (see the note
        # further down) and survived here. awk counts uniques without the
        # exit-status hazard.
        total_forges=$(awk -F, 'NR>1 && $3=="forge" {k[$4","$5]=1} END{print length(k)}' "$blocks_file")
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
            read -r tip_age_avg tip_age_p99 < <(
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
    # `$evd/tx-results.csv` is the per-round slice snapshotted by soak.sh.
    # `$TX_ZOO_STATE/results.csv` is SHARED across rounds. Falling back to it
    # silently attributes another round's transactions to this one — that is how
    # v2.4.5's "+12 tx-zoo pass" trend was manufactured (#953 finding 3). The
    # fallback is retained ONLY so non-strict ad-hoc reports still say something,
    # and it is recorded as status="shared", which strict mode rejects.
    local tz_pass="null" tz_fail="null" tz_skip="null" tz_total="null"
    local tz_source="none" tz_status="absent"
    for candidate in "$evd/tx-results.csv" "$TX_ZOO_STATE/results.csv"; do
        [ -n "$candidate" ] || continue
        if [ -f "$candidate" ] && [ -s "$candidate" ]; then
            tz_pass=$(awk -F, 'NR>1 && $3=="PASS" {c++} END{print c+0}' "$candidate")
            tz_fail=$(awk -F, 'NR>1 && $3=="FAIL" {c++} END{print c+0}' "$candidate")
            tz_skip=$(awk -F, 'NR>1 && $3=="SKIP" {c++} END{print c+0}' "$candidate")
            tz_total=$(awk -F, 'NR>1 && NF {c++} END{print c+0}' "$candidate")
            case "$candidate" in
                "$evd/"*) tz_source="round"; tz_status=$(_status_for "$tz_total" "$EXP_TX_ZOO") ;;
                *)        tz_source="shared"; tz_status="shared" ;;
            esac
            break
        fi
    done

    # --- Predicate pass/fail (parse from verify.sh's in-dir report.md) ---
    local p1="null" p2="null" p3="null" p4="null" p5="null"
    local in_dir_report="$evd/report.md"
    if [ -f "$in_dir_report" ]; then
        # SKIP = predicate out of scope for this round (e.g. an idle soak has no
        # txs for p3) — treated as null, not FAIL.
        p1=$(awk -F'|' '/\| p1 /{gsub(/ /,""); if($4~/PASS/) print "true"; else if($4~/SKIP/) print "null"; else print "false"; exit}' "$in_dir_report")
        p2=$(awk -F'|' '/\| p2 /{gsub(/ /,""); if($4~/PASS/) print "true"; else if($4~/SKIP/) print "null"; else print "false"; exit}' "$in_dir_report")
        p3=$(awk -F'|' '/\| p3 /{gsub(/ /,""); if($4~/PASS/) print "true"; else if($4~/SKIP/) print "null"; else print "false"; exit}' "$in_dir_report")
        p4=$(awk -F'|' '/\| p4 /{gsub(/ /,""); if($4~/PASS/) print "true"; else if($4~/SKIP/) print "null"; else print "false"; exit}' "$in_dir_report")
        p5=$(awk -F'|' '/\| p5 /{gsub(/ /,""); if($4~/PASS/) print "true"; else if($4~/SKIP/) print "null"; else print "false"; exit}' "$in_dir_report")
        p1="${p1:-null}"; p2="${p2:-null}"; p3="${p3:-null}"; p4="${p4:-null}"; p5="${p5:-null}"
    fi

    local round_pass="true"
    for pv in "$p1" "$p2" "$p3" "$p4" "$p5"; do
        [ "$pv" = "false" ] && round_pass="false"
    done
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
            ec=$(count_log_errors "$log")
            wc=$(count_log_warns "$log")
            [ "$first" -eq 0 ] && log_json+=","
            log_json+="\"$node\":{\"errors\":$ec,\"warns\":$wc}"
            first=0
        done
        log_json+="}"
    fi

    # --- Anomalies list ---
    local anomalies_json="[]"
    if [ -f "$in_dir_report" ]; then
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

    # --- D3: Adversarial N2N ---
    local n2n_status="absent" n2n_pass="null" n2n_fail="null" n2n_panic="null" n2n_silent="null" n2n_total="null"
    local n2n_csv="$evd/n2n-trace.csv"
    if [ -f "$n2n_csv" ] && [ -s "$n2n_csv" ]; then
        n2n_pass=$(awk -F, 'NR>1 && ($7=="PASS"||$7=="REJECTED") {c++} END{print c+0}' "$n2n_csv")
        n2n_fail=$(awk -F, 'NR>1 && ($7=="PANIC"||$7=="SILENT_SKIP"||$7=="ERROR") {c++} END{print c+0}' "$n2n_csv")
        n2n_panic=$(awk -F, 'NR>1 && $7=="PANIC" {c++} END{print c+0}' "$n2n_csv")
        n2n_silent=$(awk -F, 'NR>1 && $7=="SILENT_SKIP" {c++} END{print c+0}' "$n2n_csv")
        n2n_total=$(_rows "$n2n_csv")
        n2n_status=$(_status_for "$n2n_total" "$EXP_N2N")
    fi

    # --- D4: CLI parity ---
    # Columns: ts,query,status,dugite_sha256,cardano_sha256,equal,notes (7).
    # `status` ($3) is authoritative — see #945 for what indexing off the header
    # instead cost. env-skip vs state-skip are separated so a setup gap
    # ("pool1 id not found") is never mistaken for a compared query.
    local cp_status="absent" cp_equal="null" cp_div="null" cp_env="null" cp_state="null" cp_err="null" cp_total="null"
    local parity_csv="$evd/cli-parity.csv"
    if [ -f "$parity_csv" ] && [ -s "$parity_csv" ]; then
        cp_equal=$(awk -F, 'NR>1 && $3=="EQUAL" {c++} END{print c+0}' "$parity_csv")
        cp_div=$(awk -F, 'NR>1 && $3=="DIVERGENT" && $7!~/known-divergence/ {c++} END{print c+0}' "$parity_csv")
        cp_env=$(awk -F, 'NR>1 && $3=="SKIP" && $7~/env-skip/ {c++} END{print c+0}' "$parity_csv")
        cp_state=$(awk -F, 'NR>1 && $3=="SKIP" && $7!~/env-skip/ {c++} END{print c+0}' "$parity_csv")
        cp_err=$(awk -F, 'NR>1 && $3=="ERROR" {c++} END{print c+0}' "$parity_csv")
        cp_total=$(_rows "$parity_csv")
        cp_status=$(_status_for "$cp_total" "$EXP_CLI")
    fi

    # --- Bidirectional parity matrix (the strongest predicate) ---
    # Schema: name,status_relay,detail_relay,status_cardano_bp,detail_cardano_bp,match
    # Category is derived from the script-name numeric prefix so the CSV format
    # stays stable; expected/categories come from the sidecar meta written by
    # bidirectional-parity.sh.
    local pm_status="absent" pm_total="null" pm_match="null" pm_offdiag="null"
    local pm_classdiff="null" pm_knowndiff="null" pm_stateful="null"
    local pm_expected="null" pm_cats="null" pm_percat="null"
    local pm_csv="$evd/parity-matrix.csv"
    local pm_meta="$evd/parity-matrix.meta.json"
    if [ -f "$pm_csv" ] && [ -s "$pm_csv" ]; then
        pm_total=$(_rows "$pm_csv")
        pm_match=$(awk -F, 'NR>1 && $NF=="MATCH" {c++} END{print c+0}' "$pm_csv")
        pm_offdiag=$(awk -F, 'NR>1 && $NF=="OFFDIAG" {c++} END{print c+0}' "$pm_csv")
        pm_classdiff=$(awk -F, 'NR>1 && $NF=="CLASSDIFF" {c++} END{print c+0}' "$pm_csv")
        pm_knowndiff=$(awk -F, 'NR>1 && $NF=="KNOWNDIFF" {c++} END{print c+0}' "$pm_csv")
        pm_stateful=$(awk -F, 'NR>1 && $NF=="STATEFUL" {c++} END{print c+0}' "$pm_csv")
        # Category comes from column 2 when present (matrix schema >= #954).
        # Older matrices have no category column, so fall back to the script
        # name's numeric prefix (01a-simple-pay -> 01) rather than mis-reading
        # whatever happens to sit in field 2.
        pm_percat=$(awk -F, '
            NR==1 { has_cat = ($2 == "category"); next }
            NF {
                if (has_cat) pfx = $2
                else { split($1, parts, "-"); pfx = parts[1]; gsub(/[a-z]+$/, "", pfx) }
                t[pfx]++
                if ($NF=="OFFDIAG")        o[pfx]++
                else if ($NF=="CLASSDIFF") d[pfx]++
                else if ($NF=="KNOWNDIFF") k[pfx]++
                else if ($NF=="STATEFUL")  x[pfx]++
                else                       m[pfx]++
            }
            END {
                printf "{"
                first=1
                for (ck in t) {
                    if (!first) printf ","
                    printf "\"%s\":{\"total\":%d,\"match\":%d,\"offdiag\":%d,\"classdiff\":%d,\"knowndiff\":%d,\"stateful\":%d}", \
                           ck, t[ck], m[ck]+0, o[ck]+0, d[ck]+0, k[ck]+0, x[ck]+0
                    first=0
                }
                printf "}"
            }' "$pm_csv")
        if [ -f "$pm_meta" ]; then
            pm_expected=$(jq -r '.expected // "null"' "$pm_meta")
            pm_cats=$(jq -c '.categories // null' "$pm_meta")
        fi
        pm_status=$(_status_for "$pm_total" "$( [ "$pm_expected" = "null" ] && echo 0 || echo "$pm_expected" )")
    fi

    # --- D6: Chaos ---
    local ch_status="absent" ch_pass="null" ch_fail="null" ch_env="null" ch_total="null"
    local chaos_csv="$evd/chaos-events.csv"
    if [ -f "$chaos_csv" ] && [ -s "$chaos_csv" ]; then
        ch_pass=$(awk -F, 'NR>1 && $NF=="PASS" {c++} END{print c+0}' "$chaos_csv")
        ch_fail=$(awk -F, 'NR>1 && $NF=="FAIL" {c++} END{print c+0}' "$chaos_csv")
        ch_env=$(awk -F, 'NR>1 && $NF=="ENV_SKIP" {c++} END{print c+0}' "$chaos_csv")
        ch_total=$(_rows "$chaos_csv")
        ch_status=$(_status_for "$ch_total" "$EXP_CHAOS")
    fi

    # --- D10: UTxO RPC (gRPC) --- (#960)
    # Columns: ts,check,api_version,endpoint,status,detail
    local rpc_status="absent" rpc_pass="null" rpc_fail="null" rpc_err="null" \
          rpc_envskip="null" rpc_stateskip="null" rpc_total="null"
    local rpc_csv="$evd/rpc.csv"
    if [ -f "$rpc_csv" ] && [ -s "$rpc_csv" ]; then
        rpc_pass=$(awk -F, 'NR>1 && $5=="PASS"  {c++} END{print c+0}' "$rpc_csv")
        rpc_fail=$(awk -F, 'NR>1 && $5=="FAIL"  {c++} END{print c+0}' "$rpc_csv")
        rpc_err=$( awk -F, 'NR>1 && $5=="ERROR" {c++} END{print c+0}' "$rpc_csv")
        rpc_envskip=$(awk -F, 'NR>1 && $5=="SKIP" && $6~/env-skip/  {c++} END{print c+0}' "$rpc_csv")
        rpc_stateskip=$(awk -F, 'NR>1 && $5=="SKIP" && $6!~/env-skip/ {c++} END{print c+0}' "$rpc_csv")
        rpc_total=$(_rows "$rpc_csv")
        rpc_status=$(_status_for "$rpc_total" "$EXP_RPC")
    fi

    # --- D5: Throughput ---
    local tp_status="absent" tp_scen="null" tp_max="null" tp_min="null"
    local tp_csv="$evd/throughput.csv"
    if [ -f "$tp_csv" ] && [ -s "$tp_csv" ]; then
        tp_scen=$(_rows "$tp_csv")
        tp_max=$(awk -F, 'NR>1 && $5+0>0 {if($5+0>m) m=$5+0} END{printf "%.3f", m+0}' "$tp_csv")
        tp_min=$(awk -F, 'NR>1 && $5+0>0 {if(m==0||$5+0<m) m=$5+0} END{printf "%.3f", m+0}' "$tp_csv")
        tp_status="ok"
    fi

    # --- D8: Resources ---
    local rs_status="absent" rs_samples="null" rs_rss="null" rs_fds="null" rs_cpu="null"
    local rs_csv="$evd/resource-samples.csv"
    if [ -f "$rs_csv" ] && [ -s "$rs_csv" ]; then
        rs_samples=$(_rows "$rs_csv")
        rs_cpu=$(awk -F, 'NR>1 && $5+0>m {m=$5+0} END{printf "%.1f", m+0}' "$rs_csv")
        rs_rss=$(awk -F, 'NR>1 && $6+0>m {m=$6+0} END{printf "%d", m+0}' "$rs_csv")
        rs_fds=$(awk -F, 'NR>1 && $7+0>m {m=$7+0} END{printf "%d", m+0}' "$rs_csv")
        rs_status="ok"
    fi

    # --- D9: Determinism ---
    local dt_status="absent" dt_verdict="null"
    local dt_file="$evd/determinism.txt"
    if [ -f "$dt_file" ] && [ -s "$dt_file" ]; then
        dt_verdict=$(tail -1 "$dt_file" | jq -R .)
        dt_status="ok"
    fi

    # --- Log anomalies ---
    local la_status="absent" la_rows="null"
    local la_csv="$evd/log-anomalies.csv"
    if [ -f "$la_csv" ] && [ -s "$la_csv" ]; then
        la_rows=$(_rows "$la_csv")
        la_status="ok"
    fi

    # --- Epoch transitions ---
    local epoch_transitions="null"
    if [ -d "$logs_dir" ] && [ -f "$logs_dir/dugite-bp.log" ]; then
        # `grep -c` writes a count AND exits 1 on zero matches; the old
        # `|| echo "null"` therefore appended a second value to stdout and
        # crashed the downstream jq.
        epoch_transitions=$(grep -ciE 'epoch transition|EpochTransition|epoch_transition' \
            "$logs_dir/dugite-bp.log" 2>/dev/null || true)
        [ -z "$epoch_transitions" ] && epoch_transitions="null"
    fi

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
    "status": "$tz_status",
    "pass": $tz_pass,
    "fail": $tz_fail,
    "skip": $tz_skip,
    "total": $tz_total,
    "expected": $EXP_TX_ZOO,
    "source": "$tz_source"
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
  "n2n_adversarial": {
    "status": "$n2n_status",
    "pass": $n2n_pass,
    "fail": $n2n_fail,
    "panic": $n2n_panic,
    "silent_skip": $n2n_silent,
    "total": $n2n_total,
    "expected": $EXP_N2N
  },
  "cli_parity": {
    "status": "$cp_status",
    "equal": $cp_equal,
    "divergent": $cp_div,
    "env_skip": $cp_env,
    "state_skip": $cp_state,
    "error": $cp_err,
    "total": $cp_total,
    "expected": $EXP_CLI
  },
  "parity_matrix": {
    "status": "$pm_status",
    "total": $pm_total,
    "match": $pm_match,
    "offdiag": $pm_offdiag,
    "classdiff": $pm_classdiff,
    "knowndiff": $pm_knowndiff,
    "stateful": $pm_stateful,
    "expected": $pm_expected,
    "categories": $pm_cats,
    "per_category": $pm_percat
  },
  "chaos": {
    "status": "$ch_status",
    "pass": $ch_pass,
    "fail": $ch_fail,
    "env_skip": $ch_env,
    "total": $ch_total,
    "expected": $EXP_CHAOS
  },
  "rpc": {
    "status": "$rpc_status",
    "pass": $rpc_pass,
    "fail": $rpc_fail,
    "error": $rpc_err,
    "env_skip": $rpc_envskip,
    "state_skip": $rpc_stateskip,
    "total": $rpc_total,
    "expected": $EXP_RPC
  },
  "throughput": {
    "status": "$tp_status",
    "scenarios": $tp_scen,
    "max_blocks_per_sec": $tp_max,
    "min_blocks_per_sec": $tp_min
  },
  "resources": {
    "status": "$rs_status",
    "samples": $rs_samples,
    "max_rss_kb": $rs_rss,
    "max_fds": $rs_fds,
    "max_cpu_pct": $rs_cpu
  },
  "determinism": {
    "status": "$dt_status",
    "verdict": $dt_verdict
  },
  "log_anomalies": {
    "status": "$la_status",
    "rows": $la_rows
  }
}
ROUND_JSON
}

# ---- Build rounds array ------------------------------------------------------
ROUNDS_JSON="["
SUMMARY_PASS=true
ROUNDS_PASS=0; ROUNDS_FAIL=0
TOTAL_CANONICAL=0; TOTAL_TZ_PASS=0; TOTAL_TZ_FAIL=0; SHARED_TZ_COUNTED=0
TOTAL_INVALID_FORGES=0; TOTAL_CRIT=0; TOTAL_OFFDIAG=0
FIRST_GIT_REV="unknown"; FIRST_CN_VER="unknown"; FIRST_CCLI_VER="unknown"

for i in "${!EVIDENCE_DIRS[@]}"; do
    [ "$i" -gt 0 ] && ROUNDS_JSON+=","
    round_json=$(process_round "$i" "${EVIDENCE_DIRS[$i]}")
    ROUNDS_JSON+="$round_json"

    r_pass=$(echo "$round_json" | jq -r '.pass // "null"')
    r_canon=$(echo "$round_json" | jq -r '.blocks.canonical // 0')
    r_tz_pass=$(echo "$round_json" | jq -r '.tx_zoo.pass // 0')
    r_tz_fail=$(echo "$round_json" | jq -r '.tx_zoo.fail // 0')
    r_offdiag=$(echo "$round_json" | jq -r '.parity_matrix.offdiag // 0')

    if [ "$r_pass" = "false" ]; then
        SUMMARY_PASS=false
        ROUNDS_FAIL=$(( ROUNDS_FAIL + 1 ))
    elif [ "$r_pass" = "true" ]; then
        ROUNDS_PASS=$(( ROUNDS_PASS + 1 ))
    fi
    TOTAL_CANONICAL=$(( TOTAL_CANONICAL + r_canon ))
    TOTAL_OFFDIAG=$(( TOTAL_OFFDIAG + r_offdiag ))

    # Only accumulate a round's tx-zoo counts when they came from that round's
    # own snapshot; a "shared" source is the same cumulative CSV for every round.
    r_tz_src=$(echo "$round_json" | jq -r '.tx_zoo.source // "none"')
    if [ "$r_tz_src" = "round" ]; then
        TOTAL_TZ_PASS=$(( TOTAL_TZ_PASS + r_tz_pass ))
        TOTAL_TZ_FAIL=$(( TOTAL_TZ_FAIL + r_tz_fail ))
    elif [ "$r_tz_src" = "shared" ] && [ "$SHARED_TZ_COUNTED" = "0" ]; then
        TOTAL_TZ_PASS=$(( TOTAL_TZ_PASS + r_tz_pass ))
        TOTAL_TZ_FAIL=$(( TOTAL_TZ_FAIL + r_tz_fail ))
        SHARED_TZ_COUNTED=1
    fi

    if [ "$i" -eq 0 ]; then
        FIRST_GIT_REV=$(echo "$round_json" | jq -r '.git_rev // "unknown"')
        FIRST_CN_VER=$(echo "$round_json" | jq -r '.cardano_node_version // "unknown"')
        FIRST_CCLI_VER=$(echo "$round_json" | jq -r '.cardano_cli_version // "unknown"')
    fi
done
ROUNDS_JSON+="]"

# ---- Gate integrity ----------------------------------------------------------
# Evaluate the preset manifest against what is actually on disk. This runs
# regardless of --no-strict; the flag only decides whether violations are fatal.
[ "$DENOM_OK" -eq 1 ] || MISSING+=("denominator manifest absent ($DENOM_FILE) — all counts are self-reported")

while IFS='|' read -r key fname scope; do
    [ -n "$key" ] || continue
    case "$scope" in
        every)
            for i in "${!EVIDENCE_DIRS[@]}"; do
                rname="${ROUND_NAMES[$i]:-round$i}"
                f="${EVIDENCE_DIRS[$i]}/$fname"
                [ -f "$f" ] && [ -s "$f" ] || MISSING+=("$fname absent in round '$rname' (required for preset '$PRESET')")
            done
            ;;
        any)
            found=0
            for i in "${!EVIDENCE_DIRS[@]}"; do
                f="${EVIDENCE_DIRS[$i]}/$fname"
                if [ -f "$f" ] && [ -s "$f" ]; then found=1; break; fi
            done
            [ "$found" -eq 1 ] || MISSING+=("$fname absent in EVERY round (preset '$PRESET' requires it in at least one)")
            ;;
    esac
done < <(preset_manifest "$PRESET")

# Suite-level status violations: "short" (below pinned denominator) and
# "shared" (tx-zoo counts borrowed from another round) are both integrity
# failures, not data points.
for i in "${!EVIDENCE_DIRS[@]}"; do
    rname="${ROUND_NAMES[$i]:-round$i}"
    # tx_zoo is short-gated at GATE level, not per round: setup.sh archives
    # tx-zoo/state/results.csv at the start of each round, so only the round
    # that runs the full zoo has all 85 rows — round 2 (trickle) and round 3
    # (restart) hold a real partial slice, not a truncated one. But "shared" is
    # ALWAYS a violation regardless of round, so it is checked here.
    tz_st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].tx_zoo.status // \"absent\"")
    if [ "$tz_st" = "shared" ]; then
        MISSING+=("tx_zoo in round '$rname' has source=\"shared\" — counts belong to another round, not this one")
    fi
    for suite in n2n_adversarial cli_parity parity_matrix chaos; do
        st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].$suite.status // \"absent\"")
        case "$st" in
            short)
                act=$(echo "$ROUNDS_JSON" | jq -r ".[$i].$suite.total // 0")
                exp=$(echo "$ROUNDS_JSON" | jq -r ".[$i].$suite.expected // 0")
                MISSING+=("$suite in round '$rname' recorded $act rows, below the pinned $exp")
                ;;
            shared)
                MISSING+=("tx_zoo in round '$rname' has source=\"shared\" — counts belong to another round, not this one")
                ;;
        esac
    done
done

# The full tx-zoo must have run in at least ONE round. Checking the max
# rather than each round keeps partial trickle rounds honest without letting a
# gate through where the zoo never ran to completion anywhere.
MAX_TZ=0
for i in "${!EVIDENCE_DIRS[@]}"; do
    t=$(echo "$ROUNDS_JSON" | jq -r ".[$i].tx_zoo.total // 0")
    [ "$t" = "null" ] && t=0
    [ "$t" -gt "$MAX_TZ" ] && MAX_TZ="$t"
done
if [ "$EXP_TX_ZOO" -gt 0 ] && [ "$MAX_TZ" -lt "$EXP_TX_ZOO" ]; then
    MISSING+=("no round ran the full tx-zoo: best round recorded $MAX_TZ scripts, pinned denominator is $EXP_TX_ZOO")
fi

ADMISSIBLE=true
[ ${#MISSING[@]} -gt 0 ] && ADMISSIBLE=false
[ "$STRICT" -eq 1 ] || ADMISSIBLE=false

MISSING_JSON="[]"
if [ ${#MISSING[@]} -gt 0 ]; then
    MISSING_JSON=$(printf '%s\n' "${MISSING[@]}" | jq -R . | jq -s .)
fi

GATE_JSON=$(cat <<GATE
{
  "strict": $( [ "$STRICT" -eq 1 ] && echo true || echo false ),
  "admissible": $ADMISSIBLE,
  "missing": $MISSING_JSON,
  "denominators_file": $( [ "$DENOM_OK" -eq 1 ] && printf '%s' "$DENOM_FILE" | jq -R . || echo null )
}
GATE
)

# ---- Trend comparison --------------------------------------------------------
TREND_JSON="null"
if [ -n "$PREV_REPORT" ] && [ -f "$PREV_REPORT" ]; then
    prev_tag=$(jq -r '.release_tag // "unknown"' "$PREV_REPORT")
    prev_canonical=$(jq -r '.summary.total_canonical_blocks // 0' "$PREV_REPORT")
    prev_tz_pass=$(jq -r '.summary.total_tx_zoo_pass // 0' "$PREV_REPORT")

    canon_delta=$(( TOTAL_CANONICAL - prev_canonical ))
    tz_delta=$(( TOTAL_TZ_PASS - prev_tz_pass ))

    REGRESSIONS_JSON="[]"
    IMPROVEMENTS_JSON="[]"
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
  "schema_version": 2,
  "timestamp": $(printf '%s' "$TIMESTAMP" | jq -R .),
  "git_rev": $(printf '%s' "$FIRST_GIT_REV" | jq -R .),
  "release_tag": $RELEASE_TAG,
  "preset": $(printf '%s' "$PRESET" | jq -R .),
  "cardano_node_version": $(printf '%s' "$FIRST_CN_VER" | jq -R .),
  "cardano_cli_version": $(printf '%s' "$FIRST_CCLI_VER" | jq -R .),
  "gate_integrity": $GATE_JSON,
  "rounds": $ROUNDS_JSON,
  "summary": {
    "pass": $SUMMARY_PASS,
    "rounds_pass": $ROUNDS_PASS,
    "rounds_fail": $ROUNDS_FAIL,
    "total_canonical_blocks": $TOTAL_CANONICAL,
    "total_tx_zoo_pass": $TOTAL_TZ_PASS,
    "total_tx_zoo_fail": $TOTAL_TZ_FAIL,
    "invalid_forges_detected": $TOTAL_INVALID_FORGES,
    "critical_anomalies": $TOTAL_CRIT,
    "parity_offdiag_total": $TOTAL_OFFDIAG
  },
  "trend": $TREND_JSON
}
REPORT
)

echo "$REPORT_JSON" | jq . > /dev/null 2>&1 || {
    echo "ERROR: generated malformed JSON — writing raw output for debugging" >&2
    echo "$REPORT_JSON" > "$OUTPUT_DIR/report.json.raw"
    exit 1
}

echo "$REPORT_JSON" | jq . > "$OUTPUT_DIR/report.json"

# ---- Schema validation -------------------------------------------------------
# A schema that nothing validates against is decoration; v1 drifted from the
# emitted keys precisely because no step ever compared them.
SCHEMA_VERDICT="skipped (no validator available)"
if [ -f "$SCHEMA_FILE" ]; then
    if python3 -c 'import jsonschema' 2>/dev/null; then
        if python3 - "$SCHEMA_FILE" "$OUTPUT_DIR/report.json" <<'PYVALIDATE'
import json, sys
import jsonschema
schema = json.load(open(sys.argv[1]))
doc = json.load(open(sys.argv[2]))
jsonschema.validate(doc, schema)
PYVALIDATE
        then
            SCHEMA_VERDICT="valid against $(basename "$SCHEMA_FILE")"
        else
            echo "ERROR: report.json does not validate against $SCHEMA_FILE" >&2
            exit 1
        fi
    fi
fi
echo "schema:      $SCHEMA_VERDICT" >&2

# ---- Assemble report.md ------------------------------------------------------
OVERALL_BADGE=$([ "$SUMMARY_PASS" = "true" ] && echo "✅ PASS" || echo "❌ FAIL")

# Renders "n" for a null count so an absent suite never prints as 0.
_md() { # _md <json-path>
    local v; v=$(echo "$ROUNDS_JSON" | jq -r "$1")
    [ "$v" = "null" ] && echo "—" || echo "$v"
}

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
| **Gate admissible** | $([ "$ADMISSIBLE" = "true" ] && echo "✅ yes" || echo "❌ no — see Gate integrity") |

HEADER

if [ "$ADMISSIBLE" != "true" ]; then
    cat <<GATEBANNER
> [!WARNING]
> **This report is NOT admissible as a release gate.**
> $( [ "$STRICT" -eq 1 ] || echo "Strict evidence checking was disabled (\`--no-strict\`). " )$( [ ${#MISSING[@]} -gt 0 ] && echo "${#MISSING[@]} required artifact(s) were absent, short of their pinned denominator, or borrowed from another round." )
> Counts below under-report coverage rather than measuring it.

## Gate integrity

GATEBANNER
    if [ ${#MISSING[@]} -gt 0 ]; then
        printf '%s\n' "${MISSING[@]}" | sed 's/^/- /'
    else
        echo "- (no missing artifacts; inadmissible only because \`--no-strict\` was passed)"
    fi
    echo
fi

cat <<ROUNDHDR
## Round summary

| Round | Result | Canonical blocks | Tx-zoo | CLI parity | Parity matrix | Tip-age p99 | Chain density |
|---|---|---|---|---|---|---|---|
ROUNDHDR

for i in "${!EVIDENCE_DIRS[@]}"; do
    rname="${ROUND_NAMES[$i]:-round$i}"
    r_pass=$(echo "$ROUNDS_JSON"  | jq -r ".[$i].pass // \"null\"")
    r_can=$(echo "$ROUNDS_JSON"   | jq -r ".[$i].blocks.canonical // 0")
    r_tz_st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].tx_zoo.status")
    r_cp_st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].cli_parity.status")
    r_pm_st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].parity_matrix.status")
    r_tip=$(_md ".[$i].tip_age.p99_seconds")
    r_dens=$(_md ".[$i].chain_density")
    case "$r_pass" in
        true)  badge="✅ PASS" ;;
        false) badge="❌ FAIL" ;;
        *)     badge="— N/A"  ;;
    esac
    # NB: every `$( [ cond ] && echo ... )` below carries `|| true`. Under
    # `set -e`, an assignment whose LAST command substitution exits non-zero
    # takes that exit status and aborts the script — so a suite simply not
    # being "shared"/"short" silently killed report generation mid-file.
    if [ "$r_tz_st" = "absent" ]; then tz_cell="— not run"
    else tz_cell="$(_md ".[$i].tx_zoo.pass")/$(_md ".[$i].tx_zoo.total")$( [ "$r_tz_st" = "shared" ] && echo " ⚠shared" || true )"; fi
    if [ "$r_cp_st" = "absent" ]; then cp_cell="— not run"
    else
        d=$(echo "$ROUNDS_JSON" | jq -r ".[$i].cli_parity.divergent // 0")
        e=$(echo "$ROUNDS_JSON" | jq -r ".[$i].cli_parity.equal // 0")
        s=$(echo "$ROUNDS_JSON" | jq -r ".[$i].cli_parity.env_skip // 0")
        cp_cell=$([ "$d" -eq 0 ] && echo "✅ ${e}✓" || echo "❌ ${d}✗")
        [ "$s" -gt 0 ] && cp_cell="$cp_cell (${s} env-skip)"
    fi
    if [ "$r_pm_st" = "absent" ]; then pm_cell="— not run"
    else
        od=$(echo "$ROUNDS_JSON" | jq -r ".[$i].parity_matrix.offdiag // 0")
        cdf=$(echo "$ROUNDS_JSON" | jq -r ".[$i].parity_matrix.classdiff // 0")
        kdf=$(echo "$ROUNDS_JSON" | jq -r ".[$i].parity_matrix.knowndiff // 0")
        stf=$(echo "$ROUNDS_JSON" | jq -r ".[$i].parity_matrix.stateful // 0")
        pm_m=$(_md ".[$i].parity_matrix.match"); pm_t=$(_md ".[$i].parity_matrix.total")
        # Show match/total, not total/total: with KNOWNDIFF and STATEFUL rows
        # present, "79✓" would imply 79 matched when 76 did.
        if [ "$od" -ne 0 ]; then       pm_cell="❌ ${od} offdiag"
        elif [ "$cdf" -ne 0 ]; then    pm_cell="❌ ${cdf} class-diff"
        else                           pm_cell="✅ ${pm_m}/${pm_t}"
        fi
        [ "$kdf" != "0" ] && pm_cell="$pm_cell (+${kdf} known)" || true
        [ "$stf" != "0" ] && pm_cell="$pm_cell (+${stf} stateful)" || true
    fi
    echo "| $rname | $badge | $r_can | $tz_cell | $cp_cell | $pm_cell | ${r_tip}s | $r_dens |"
done

cat <<TOTALS

**Total canonical blocks:** $TOTAL_CANONICAL
**Total tx-zoo pass/fail:** $TOTAL_TZ_PASS / $TOTAL_TZ_FAIL
**Parity off-diagonal (all rounds):** $TOTAL_OFFDIAG

TOTALS

# Suite coverage table — makes "never ran" visually distinct from "ran clean".
echo "## Suite coverage"
echo
echo "| Round | Adversarial N2N | Chaos | Throughput | Resources | Determinism |"
echo "|---|---|---|---|---|---|"
for i in "${!EVIDENCE_DIRS[@]}"; do
    rname="${ROUND_NAMES[$i]:-round$i}"
    n2n_st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].n2n_adversarial.status")
    if [ "$n2n_st" = "absent" ]; then n2n="— not run"; else
        n2n="$(_md ".[$i].n2n_adversarial.pass")/$(_md ".[$i].n2n_adversarial.total")$( [ "$n2n_st" = "short" ] && echo " ⚠short" || true )"; fi
    ch_st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].chaos.status")
    if [ "$ch_st" = "absent" ]; then ch="— not run"; else
        ch_env=$(_md ".[$i].chaos.env_skip")
        ch="$(_md ".[$i].chaos.pass")/$(_md ".[$i].chaos.total")$( [ "$ch_env" != "0" ] && [ "$ch_env" != "—" ] && echo " (+$ch_env env-skip)" || true )"; fi
    tp_st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].throughput.status")
    tp=$([ "$tp_st" = "absent" ] && echo "— not run" || echo "$(_md ".[$i].throughput.max_blocks_per_sec") blk/s peak")
    rs_st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].resources.status")
    rs=$([ "$rs_st" = "absent" ] && echo "— not run" || echo "$(_md ".[$i].resources.samples") samples")
    dt_st=$(echo "$ROUNDS_JSON" | jq -r ".[$i].determinism.status")
    dt=$([ "$dt_st" = "absent" ] && echo "— not run" || _md ".[$i].determinism.verdict")
    echo "| $rname | $n2n | $ch | $tp | $rs | $dt |"
done
echo

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
        echo "| (no in-round report.md) | — | verify.sh did not run for this round |"
    fi
    echo
done

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
    [ -n "$regressions" ]  && echo "**Regressions:** $regressions"
    [ -n "$improvements" ] && echo "**Improvements:** $improvements"
fi

cat <<FOOTER

---
*Generated by \`.claude/skills/devnet-validate/scripts/generate-release-report.sh\`*
*Schema: \`.claude/skills/devnet-validate/schemas/report.v2.json\` — $SCHEMA_VERDICT*
*Denominators: \`.claude/skills/devnet-validate/schemas/denominators.json\`*
FOOTER
} > "$OUTPUT_DIR/report.md"

echo "report.json → $OUTPUT_DIR/report.json" >&2
echo "report.md   → $OUTPUT_DIR/report.md"   >&2

# ---- Exit --------------------------------------------------------------------
if [ ${#MISSING[@]} -gt 0 ]; then
    echo >&2
    echo "GATE INTEGRITY: ${#MISSING[@]} violation(s)" >&2
    printf '  - %s\n' "${MISSING[@]}" >&2
    if [ "$STRICT" -eq 1 ]; then
        echo >&2
        echo "Refusing to report a PASS over evidence that was never produced." >&2
        echo "Re-run the missing suites, or pass --no-strict to acknowledge this" >&2
        echo "is a partial run (which marks the report inadmissible as a gate)." >&2
        exit 3
    fi
    echo "  (--no-strict: recorded, not fatal)" >&2
fi

[ "$SUMMARY_PASS" = "true" ] && exit 0 || exit 1
