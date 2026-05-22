#!/usr/bin/env bash
# perf/log-level-predicate.sh — D8: scan all node logs for zero panics,
# zero unwrap fails, and zero ERROR messages outside the allowlisted set.
#
# This must run after a completed devnet session (logs must exist).
# It is also called by verify.sh as predicate p3.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/../lib/common.sh"

EVIDENCE_DIR="${EVIDENCE_DIR:-$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$EVIDENCE_DIR"
LOG_ANOMALIES_CSV="$EVIDENCE_DIR/log-anomalies.csv"
[ -f "$LOG_ANOMALIES_CSV" ] || echo "ts,node,level,pattern,count" > "$LOG_ANOMALIES_CSV"

# Patterns that are hard failures — never expected
PANIC_PATTERNS=(
    'thread .* panicked'
    'PANIC'
    'called .* on a .* value'  # unwrap/expect fail
    'attempted to .* with overflow'
    'stack overflow'
)

# Allowlisted ERROR patterns — known legitimate rejections, not bugs
ALLOWLISTED_ERROR_PATTERNS=(
    'connection refused'          # normal when peer not yet up
    'broken pipe'                 # normal on peer disconnect
    'reset by peer'               # normal N2N close
    'os error 32'                 # Broken pipe on Linux
    'TxValidationError'           # expected for negative tx-zoo tests
    'BadInput'                    # expected reject path
    'DecodeError'                 # expected adversarial N2N path
    'connection closed'           # normal cleanup
    'IntersectNotFound'           # expected during initial chainsync
    'Protocol version mismatch'   # expected for adversarial handshake
)

FOUND_FAILURES=0
RESULT_LINES=()

for log_file in "$LD_LOGS"/*.log; do
    [ -f "$log_file" ] || continue
    node=$(basename "$log_file" .log)

    # Check for panics
    for pattern in "${PANIC_PATTERNS[@]}"; do
        count=$(grep -c -E "$pattern" "$log_file" 2>/dev/null || echo 0)
        if [ "$count" -gt 0 ]; then
            log_error "PANIC in $node: pattern='$pattern' count=$count"
            printf '%s,%s,PANIC,"%s",%s\n' \
                "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$node" "$pattern" "$count" \
                >> "$LOG_ANOMALIES_CSV"
            FOUND_FAILURES=$(( FOUND_FAILURES + 1 ))
        fi
    done

    # Check for non-allowlisted ERROR lines
    ERROR_LINES=$(grep -E '^\s*(ERROR|error\[)' "$log_file" 2>/dev/null || true)
    if [ -n "$ERROR_LINES" ]; then
        while IFS= read -r line; do
            allowed=0
            for allow in "${ALLOWLISTED_ERROR_PATTERNS[@]}"; do
                if echo "$line" | grep -qiE "$allow"; then
                    allowed=1
                    break
                fi
            done
            if [ "$allowed" -eq 0 ]; then
                pattern="${line:0:80}"
                log_error "Unexpected ERROR in $node: $pattern"
                printf '%s,%s,ERROR,"%s",1\n' \
                    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$node" "${pattern//,/;}" \
                    >> "$LOG_ANOMALIES_CSV"
                FOUND_FAILURES=$(( FOUND_FAILURES + 1 ))
            fi
        done <<< "$ERROR_LINES"
    fi
done

if [ "$FOUND_FAILURES" -gt 0 ]; then
    log_error "log-level-predicate FAIL: $FOUND_FAILURES violation(s)"
    exit 1
fi

log_info "log-level-predicate PASS: zero panics, zero unexpected ERRORs"
