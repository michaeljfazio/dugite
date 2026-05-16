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
# Predicate 2: both pools must have forged >=3 blocks
p2_per_bp_attribution() {
    local blocks="$1"
    [ -s "$blocks" ] || { PREDICATE_FAIL+=("p2:no-data"); return; }

    local pool1_vkey="" pool2_vkey=""
    if [ -f "$LD_KEYS/pool1/cold.vkey" ]; then
        pool1_vkey=$(jq -r '.cborHex' "$LD_KEYS/pool1/cold.vkey" 2>/dev/null \
            | tail -c +5 | head -c 64 || echo "")
    fi
    if [ -f "$LD_KEYS/pool2/cold.vkey" ]; then
        pool2_vkey=$(jq -r '.cborHex' "$LD_KEYS/pool2/cold.vkey" 2>/dev/null \
            | tail -c +5 | head -c 64 || echo "")
    fi

    # If we don't have keys (running on test fixtures), match against literal POOL1/POOL2 strings
    if [ -z "$pool1_vkey" ]; then pool1_vkey="POOL1"; fi
    if [ -z "$pool2_vkey" ]; then pool2_vkey="POOL2"; fi

    local p1_forges p2_forges
    p1_forges=$(awk -F, -v k="$pool1_vkey" '$3=="forge" && $6==k' "$blocks" | wc -l | tr -d ' ')
    p2_forges=$(awk -F, -v k="$pool2_vkey" '$3=="forge" && $6==k' "$blocks" | wc -l | tr -d ' ')

    if [ "$p1_forges" -ge 3 ] && [ "$p2_forges" -ge 3 ]; then
        PREDICATE_PASS+=("p2:per-bp-attribution (pool1=$p1_forges pool2=$p2_forges)")
        {
            printf 'pool1_forges\t%s\n' "$p1_forges"
            printf 'pool2_forges\t%s\n' "$p2_forges"
        } > "$(dirname "$blocks")/forge-attribution.tsv"
    else
        PREDICATE_FAIL+=("p2:per-bp-attribution (pool1=$p1_forges pool2=$p2_forges; need >=3 each)")
    fi
}
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

    # For p2 self-test: force vkey fallback to literal POOL1/POOL2 by pointing
    # LD_KEYS at a non-existent path so cold.vkey lookups fail.
    local saved_ld_keys="$LD_KEYS"
    LD_KEYS="$fix/_nonexistent_keys"

    log_info "p2 - good fixture (expect PASS)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p2_per_bp_attribution "$fix/predicate-2-good.csv"
    [ ${#PREDICATE_PASS[@]} -gt 0 ] && [ ${#PREDICATE_FAIL[@]} -eq 0 ] \
        || { LD_KEYS="$saved_ld_keys"; die "p2 self-test good: expected PASS, got ${PREDICATE_FAIL[*]:-}"; }
    log_info "  OK"

    log_info "p2 - bad fixture (expect FAIL)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p2_per_bp_attribution "$fix/predicate-2-bad.csv"
    [ ${#PREDICATE_FAIL[@]} -gt 0 ] && [ ${#PREDICATE_PASS[@]} -eq 0 ] \
        || { LD_KEYS="$saved_ld_keys"; die "p2 self-test bad: expected FAIL, got ${PREDICATE_PASS[*]:-}"; }
    log_info "  OK"

    LD_KEYS="$saved_ld_keys"
    # Clean up artifact written by p2 on the good fixture
    rm -f "$fix/forge-attribution.tsv"

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
