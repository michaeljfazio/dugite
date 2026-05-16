#!/usr/bin/env bash
# Evaluate the 4 soak predicates against an evidence directory.
# Usage:
#   verify.sh <evidence_dir>            — full report on real evidence
#   verify.sh --self-test                — run predicates against test fixtures
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/common.sh"

PREDICATE_PASS=()
PREDICATE_FAIL=()

# Predicate 1: every (slot, hash) seen by any observer is seen by all 3 observers
# (Tolerance: most-recent 10 blocks may have partial observers, since they
# may not have propagated yet at end of soak.)
p1_forge_cross_check() {
    local blocks="$1"
    [ -s "$blocks" ] || { PREDICATE_FAIL+=("p1:no-data"); return; }

    # Get all unique (slot, hash) pairs and trim the most-recent 10
    local distinct_blocks total trimmed
    distinct_blocks=$(awk -F, 'NR>1 && $4!="?" && $5!="?" && $4!="" && $5!="" {print $4","$5}' "$blocks" | sort -u)
    total=$(printf '%s\n' "$distinct_blocks" | grep -c '^' || true)
    if [ "$total" -le 10 ]; then
        trimmed="$distinct_blocks"
    else
        trimmed=$(printf '%s\n' "$distinct_blocks" | sort -t, -k1n -k2 | head -n -10)
    fi

    local fails=0
    local fail_examples=""
    while IFS=, read -r slot hash; do
        [ -z "$slot" ] && continue
        local n_obs
        n_obs=$(awk -F, -v s="$slot" -v h="$hash" 'NR>1 && $4==s && $5==h {print $2}' "$blocks" | sort -u | wc -l | tr -d ' ')
        if [ "$n_obs" -lt 3 ]; then
            fails=$((fails + 1))
            [ -z "$fail_examples" ] && fail_examples="slot=$slot hash=$hash n_obs=$n_obs"
        fi
    done <<< "$trimmed"

    if [ "$fails" -eq 0 ]; then
        PREDICATE_PASS+=("p1:forge-cross-check ($total blocks, >=3 observers each)")
    else
        PREDICATE_FAIL+=("p1:forge-cross-check ($fails/$total blocks missing observers; example: $fail_examples)")
    fi
}
p2_per_bp_attribution() { :; }  # Filled in by Task 20
p3_tx_inclusion()      { :; }   # Filled in by Task 21
p4_tip_parity()        { :; }   # Filled in by Task 22

generate_report() { :; }        # Filled in by Task 23

self_test() {
    local fix="$SCRIPT_DIR/lib/test-fixtures"
    log_info "=== Self-test predicates against fixtures ==="

    log_info "p1 - good fixture (expect PASS)"
    local saved_pass=("${PREDICATE_PASS[@]:+${PREDICATE_PASS[@]}}") saved_fail=("${PREDICATE_FAIL[@]:+${PREDICATE_FAIL[@]}}")
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p1_forge_cross_check "$fix/predicate-1-good.csv"
    [ ${#PREDICATE_PASS[@]} -gt 0 ] && [ ${#PREDICATE_FAIL[@]} -eq 0 ] \
        || die "p1 self-test on good fixture: expected PASS, got ${PREDICATE_FAIL[*]:-}"
    log_info "  OK"

    log_info "p1 - bad fixture (expect FAIL)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p1_forge_cross_check "$fix/predicate-1-bad.csv"
    [ ${#PREDICATE_FAIL[@]} -gt 0 ] && [ ${#PREDICATE_PASS[@]} -eq 0 ] \
        || die "p1 self-test on bad fixture: expected FAIL, got ${PREDICATE_PASS[*]:-}"
    log_info "  OK"

    PREDICATE_PASS=("${saved_pass[@]:+${saved_pass[@]}}"); PREDICATE_FAIL=("${saved_fail[@]:+${saved_fail[@]}}")
    log_info "Self-test complete."
}

if [ "${1:-}" = "--self-test" ]; then
    self_test
    exit 0
fi

EVD="${1:?evidence dir required}"
[ -d "$EVD" ] || die "$EVD is not a directory"

p1_forge_cross_check  "$EVD/blocks.csv"
p2_per_bp_attribution "$EVD/blocks.csv"
p3_tx_inclusion       "$EVD/tx-submissions.csv" "$EVD"
p4_tip_parity         "$EVD/tip-samples.csv"
generate_report       "$EVD"

if [ ${#PREDICATE_FAIL[@]} -gt 0 ]; then
    log_error "FAILED: ${PREDICATE_FAIL[*]}"
    exit 1
fi
log_info "PASSED all predicates: ${PREDICATE_PASS[*]}"
