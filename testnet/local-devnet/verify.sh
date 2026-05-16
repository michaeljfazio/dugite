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

p1_forge_cross_check() { :; }   # Filled in by Task 19
p2_per_bp_attribution() { :; }  # Filled in by Task 20
p3_tx_inclusion()      { :; }   # Filled in by Task 21
p4_tip_parity()        { :; }   # Filled in by Task 22

generate_report() { :; }        # Filled in by Task 23

self_test() {
    local fix="$SCRIPT_DIR/lib/test-fixtures"
    log_info "=== Self-test predicates against fixtures ==="
    # Tasks 19-22 add per-predicate self-test calls here
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
