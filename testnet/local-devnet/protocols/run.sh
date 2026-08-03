#!/usr/bin/env bash
# Run all adversarial N2N protocol tests.
#
# Usage:
#   ./run.sh [evidence_dir]
#
# Writes: evidence/<ts>/n2n-trace.csv
#
# Exit codes: 0 = all PASS/SKIP; 1 = ≥1 FAIL; 2 = setup error
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

# Determine output directory
if [ -n "${1:-}" ]; then
    OUT_DIR="$1"
else
    LATEST=$(ls -t "$LD_EVIDENCE" 2>/dev/null | head -1 || true)
    if [ -n "$LATEST" ]; then
        OUT_DIR="$LD_EVIDENCE/$LATEST"
    else
        OUT_DIR="$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)"
        mkdir -p "$OUT_DIR"
    fi
fi

export ADV_CSV="$OUT_DIR/n2n-trace.csv"
mkdir -p "$OUT_DIR"

echo "ts,protocol,msg_type,peer,dir,size_bytes,outcome,notes" > "$ADV_CSV"

log_info "=== protocols/run.sh: adversarial N2N tests ==="
adv_require_devnet

TOTAL=0; PASS_TOTAL=0; FAIL_TOTAL=0; SKIP_TOTAL=0

for script in "$SCRIPT_DIR"/[0-9]*.sh; do
    [ -e "$script" ] || continue
    name="$(basename "$script" .sh)"
    TOTAL=$(( TOTAL + 1 ))
    log_info "--- $name ---"
    rc=0
    bash "$script" 2>&1 | sed 's/^/  /' || rc=$?
    case $rc in
        0) PASS_TOTAL=$(( PASS_TOTAL + 1 )) ;;
        2) SKIP_TOTAL=$(( SKIP_TOTAL + 1 )) ;;
        *) FAIL_TOTAL=$(( FAIL_TOTAL + 1 )) ;;
    esac
done

ROWS=$(awk 'NR>1 && NF' "$ADV_CSV" | wc -l | tr -d ' ')

log_info ""
log_info "=== Adversarial N2N summary ==="
log_info "  PASS:  $PASS_TOTAL"
log_info "  FAIL:  $FAIL_TOTAL"
log_info "  SKIP:  $SKIP_TOTAL"
log_info "  TOTAL: $TOTAL scripts / $ROWS cases"
log_info "  CSV:   $ADV_CSV"

if [ "$FAIL_TOTAL" -gt 0 ]; then
    log_error "FAIL: $FAIL_TOTAL adversarial tests failed — zero panics and zero silent skips required"
    exit 1
fi

# DENOMINATOR GATE (#953)
# -----------------------
# Every script exiting 0 does NOT mean every stimulus fired. A case that
# silently no-ops writes no row, and "N/N passed" computed from the rows you
# produced is tautological — that is precisely how "26/26 adversarial" could
# have meant 3/3. Compare against a count pinned OUTSIDE this run.
DENOM_FILE="${DENOM_FILE:-$SCRIPT_DIR/../../../.claude/skills/devnet-validate/schemas/denominators.json}"
if [ -f "$DENOM_FILE" ]; then
    EXPECTED=$(jq -r '.n2n_adversarial.expected_cases // 0' "$DENOM_FILE")
    EXPECTED_SCRIPTS=$(jq -r '.n2n_adversarial.expected_scripts // 0' "$DENOM_FILE")
    if [ "$TOTAL" -lt "$EXPECTED_SCRIPTS" ]; then
        log_error "FAIL: ran $TOTAL scripts, pinned denominator is $EXPECTED_SCRIPTS ($DENOM_FILE)"
        exit 1
    fi
    if [ "$ROWS" -lt "$EXPECTED" ]; then
        log_error "FAIL: n2n-trace.csv has $ROWS cases, pinned denominator is $EXPECTED"
        log_error "A stimulus that did not fire leaves no row. Either a case regressed"
        log_error "into a silent no-op, or a case was removed and the pin needs updating"
        log_error "TOGETHER WITH on-disk evidence of the new count."
        exit 1
    fi
    log_info "  denominator: $ROWS/$EXPECTED cases, $TOTAL/$EXPECTED_SCRIPTS scripts ✓"
else
    log_warn "denominator manifest not found ($DENOM_FILE) — case count is self-reported"
fi

log_info "PASS: all adversarial tests passed"
