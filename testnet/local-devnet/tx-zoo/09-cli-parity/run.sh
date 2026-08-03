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
# 7 columns, matching the rows actually written below. The header used to omit
# `status`, so every header-based consumer misread the file and the release
# report recorded cli_parity as all-zero in every release (#945).
echo "ts,query,status,dugite_sha256,cardano_sha256,equal,notes" > "$PARITY_CSV"

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
    ERRORS=$(awk -F, 'NR>1 && $3=="ERROR" && $7!~/^known-error:/ {c++} END{print c+0}' "$PARITY_CSV")
    KNOWN_ERR=$(awk -F, 'NR>1 && $3=="ERROR" && $7~/^known-error:/ {c++} END{print c+0}' "$PARITY_CSV")
    KNOWN_DIV=$(awk -F, 'NR>1 && $3=="DIVERGENT" && $7~/^known-divergence:/ {c++} END{print c+0}' "$PARITY_CSV")
fi

ENV_SKIPS=$(awk -F, 'NR>1 && $3=="SKIP" && $7~/env-skip/ {c++} END{print c+0}' "$PARITY_CSV")
STATE_SKIPS=$(awk -F, 'NR>1 && $3=="SKIP" && $7!~/env-skip/ {c++} END{print c+0}' "$PARITY_CSV")
ROWS=$(awk 'NR>1 && NF' "$PARITY_CSV" | wc -l | tr -d ' ')

log_info ""
log_info "=== CLI parity summary ==="
log_info "  EQUAL:      $EQUAL"
log_info "  DIVERGENT:  ${DIVERGENT_CSV:-$DIVERGENT} unexplained, ${KNOWN_DIV:-0} tracked"
log_info "  ENV-SKIP:   $ENV_SKIPS  (setup artifact missing — these are UNCOMPARED queries)"
log_info "  STATE-SKIP: $STATE_SKIPS (legitimately not comparable this run)"
log_info "  ERROR:      $ERRORS unexplained, ${KNOWN_ERR:-0} tracked"
log_info "  CSV:        $PARITY_CSV"
log_info ""

# DENOMINATOR GATE (#953)
# -----------------------
# EQUAL is not a coverage number on its own: 18 EQUAL out of 22 queries means
# 4 were never compared, and every release note published the 18 without the
# 22. Assert the row count against a pin held outside this run, and surface
# env-skips as their own class so a setup gap can never read as a clean pass.
DENOM_FILE="${DENOM_FILE:-$SCRIPT_DIR/../../../../.claude/skills/devnet-validate/schemas/denominators.json}"
if [ -f "$DENOM_FILE" ]; then
    EXPECTED=$(jq -r '.cli_parity.expected_queries // 0' "$DENOM_FILE")
    if [ "$ROWS" -lt "$EXPECTED" ]; then
        log_error "FAIL: cli-parity.csv has $ROWS rows, pinned denominator is $EXPECTED ($DENOM_FILE)"
        log_error "A query that produced no row was never run at all."
        exit 1
    fi
    log_info "  denominator: $ROWS/$EXPECTED queries ✓"
else
    log_warn "denominator manifest not found ($DENOM_FILE) — query count is self-reported"
fi

if [ "$ENV_SKIPS" -gt 0 ]; then
    log_warn "$ENV_SKIPS quer(ies) env-skipped — the compared surface is $EQUAL of $EXPECTED, not $EQUAL of $EQUAL:"
    awk -F, 'NR>1 && $3=="SKIP" && $7~/env-skip/ {print "    " $2 ": " $7}' "$PARITY_CSV" >&2
    log_warn "Fix the setup gap; do not report this as full parity."
fi

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
    log_error "FAIL: $ERRORS untracked query errors — check $PARITY_CSV for details"
    awk -F, 'NR>1 && $3=="ERROR" && $7!~/^known-error:/ {print "  " $2 ": " $7}' "$PARITY_CSV" >&2
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
