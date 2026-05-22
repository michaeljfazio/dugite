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

log_info ""
log_info "=== Adversarial N2N summary ==="
log_info "  PASS:  $PASS_TOTAL"
log_info "  FAIL:  $FAIL_TOTAL"
log_info "  SKIP:  $SKIP_TOTAL"
log_info "  TOTAL: $TOTAL"
log_info "  CSV:   $ADV_CSV"

if [ "$FAIL_TOTAL" -gt 0 ]; then
    log_error "FAIL: $FAIL_TOTAL adversarial tests failed — zero panics and zero silent skips required"
    exit 1
fi
log_info "PASS: all adversarial tests passed"
