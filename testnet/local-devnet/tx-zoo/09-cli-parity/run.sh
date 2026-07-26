#!/usr/bin/env bash
# Run all CLI parity checks and produce evidence/<ts>/cli-parity.csv.
#
# Usage:
#   ./run.sh                         — run all checks, auto-detect evidence dir
#   ./run.sh <evidence_dir>          — write cli-parity.csv to specified dir
#   PARITY_IGNORE_FAIL=1 ./run.sh    — run all checks, exit 0 even on divergence
#
# Exit codes: 0 = all EQUAL/SKIP; 1 = ≥1 unexplained DIVERGENT or ≥1 ERROR;
#             2 = setup error
#
# CSV columns: ts,query,status,dugite_sha256,cardano_sha256,equal,notes
# where status ∈ {EQUAL, DIVERGENT, SKIP, ERROR} and is authoritative.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"

# Determine output directory
if [ -n "${1:-}" ]; then
    OUT_DIR="$1"
else
    # Auto-detect the most recent evidence directory
    LATEST=$(ls -t "$LD_EVIDENCE" 2>/dev/null | head -1 || true)
    if [ -n "$LATEST" ]; then
        OUT_DIR="$LD_EVIDENCE/$LATEST"
    else
        # Fall back to a timestamped dir (run.sh didn't create one yet)
        OUT_DIR="$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)"
        mkdir -p "$OUT_DIR"
    fi
fi

export PARITY_CSV="$OUT_DIR/cli-parity.csv"
mkdir -p "$OUT_DIR"

# Reset CSV
echo "ts,query,dugite_sha256,cardano_sha256,equal,notes" > "$PARITY_CSV"

log_info "=== 09-cli-parity: checking socket availability ==="
parity_check_sockets || exit 2

log_info "=== 09-cli-parity: running $(ls "$SCRIPT_DIR"/09*.sh | wc -l | tr -d ' ') query checks ==="
log_info "Output: $PARITY_CSV"

TOTAL=0; EQUAL=0; DIVERGENT=0; ERRORS=0; SKIPS=0

run_check() {
    local script="$1"
    local name; name="$(basename "$script" .sh)"
    TOTAL=$(( TOTAL + 1 ))
    local rc=0
    bash "$script" 2>&1 | sed "s/^/  /" || rc=$?
    case $rc in
        0)  ;;
        2)  ERRORS=$(( ERRORS + 1 )) ;;
        *)  DIVERGENT=$(( DIVERGENT + 1 )) ;;
    esac
}

for script in "$SCRIPT_DIR"/09*.sh; do
    run_check "$script"
done

# Tally from CSV. Column 3 is the authoritative status
# (ts,query,status,dugite_sha256,cardano_sha256,equal,notes).
if [ -f "$PARITY_CSV" ]; then
    EQUAL=$(awk -F, 'NR>1 && $3=="EQUAL" {c++} END{print c+0}' "$PARITY_CSV")
    DIVERGENT_CSV=$(awk -F, 'NR>1 && $3=="DIVERGENT" && $7!~/^known-divergence:/ {c++} END{print c+0}' "$PARITY_CSV")
    SKIPS=$(awk -F, 'NR>1 && $3=="SKIP" {c++} END{print c+0}' "$PARITY_CSV")
    ERRORS=$(awk -F, 'NR>1 && $3=="ERROR" {c++} END{print c+0}' "$PARITY_CSV")
fi

log_info ""
log_info "=== CLI parity summary ==="
log_info "  EQUAL:     $EQUAL"
log_info "  DIVERGENT: ${DIVERGENT_CSV:-$DIVERGENT} (non-known)"
log_info "  SKIP:      $SKIPS"
log_info "  ERROR:     $ERRORS"
log_info "  CSV:       $PARITY_CSV"
log_info ""

DIVERGENT_FINAL="${DIVERGENT_CSV:-$DIVERGENT}"

FAILED=0

if [ "$DIVERGENT_FINAL" -gt 0 ]; then
    log_error "FAIL: $DIVERGENT_FINAL divergent queries — check $PARITY_CSV for details"
    log_error "File a known-divergence issue for each divergence found."
    log_error "Add the tracking URL to KNOWN_DIVERGENCES[] in lib.sh."
    FAILED=1
fi

# ERROR rows are failures, not warnings. They used to be tolerated on the
# assumption that "the node may not implement them yet", which is how the four
# rows in #900 went untracked for a whole release — and all four turned out to
# be this harness passing arguments cardano-cli 11.0.0.0 does not accept, with
# both sides failing identically. There is no such thing as an acceptable ERROR
# row: either the query works on both sides, or it is recorded as SKIP with a
# reason. This is what holds cli-parity.csv to zero unexplained rows.
if [ "$ERRORS" -gt 0 ]; then
    log_error "FAIL: $ERRORS queries errored — check $PARITY_CSV for details"
    awk -F, 'NR>1 && $3=="ERROR" {print "  " $2 ": " $7}' "$PARITY_CSV" >&2
    log_error "A 'HARNESS both-sides-failed' note means the invocation is wrong,"
    log_error "not that dugite is missing anything. Fix the 09*.sh script."
    FAILED=1
fi

if [ "$FAILED" -ne 0 ] && [ "${PARITY_IGNORE_FAIL:-0}" != "1" ]; then
    exit 1
fi
if [ "$FAILED" -ne 0 ]; then
    log_warn "PARITY_IGNORE_FAIL=1 set — reporting PASS despite failures above"
fi

log_info "PASS: CLI parity check complete"
