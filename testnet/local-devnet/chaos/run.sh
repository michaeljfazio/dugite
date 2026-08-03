#!/usr/bin/env bash
# Run the chaos suite and produce evidence/<ts>/chaos-events.csv.
#
# WHY THIS EXISTS (#959)
# ----------------------
# All six chaos scripts existed, but there was no driver, no preset invoked
# them, and NO chaos-events.csv had ever been produced anywhere under testnet/.
# Meanwhile SKILL.md's capability matrix advertised chaos at both standard
# ("kill-9 + app-nap") and extended ("+ partition + disk-full + flood"). The
# documentation described a suite that had never run.
#
# Two of the scripts could not have passed if it had:
#   * kill-9-mid-forge restarted the node with key paths that do not exist
#     ($LD_KEYS/kes.skey vs the real keys/pool1/kes.skey) and truncated
#     dugite-bp.log with `>`, destroying the round's forge history.
#   * clock-skew and inbound-syn-flood hard-required socat, absent on stock
#     macOS, so they skipped silently — the #923 mechanism exactly.
#
# Usage:
#   ./run.sh [evidence_dir]          run the default (standard) set
#   CHAOS_SET=extended ./run.sh      include the destructive scenarios
#
# Exit codes: 0 = no FAIL rows; 1 = >=1 FAIL; 2 = setup error.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/../lib/common.sh"
set +e   # lib/common.sh re-enables set -e; a scenario returning non-zero must
         # not kill the driver, or later scenarios "never ran" invisibly.

if [ -n "${1:-}" ]; then
    OUT_DIR="$1"
else
    LATEST=$(ls -t "$LD_EVIDENCE" 2>/dev/null | head -1 || true)
    OUT_DIR="${LATEST:+$LD_EVIDENCE/$LATEST}"
    OUT_DIR="${OUT_DIR:-$LD_EVIDENCE/$(date -u +%Y%m%dT%H%M%SZ)}"
fi
mkdir -p "$OUT_DIR"
export EVIDENCE_DIR="$OUT_DIR"
CHAOS_CSV="$OUT_DIR/chaos-events.csv"
[ -f "$CHAOS_CSV" ] || echo "ts,scenario,action,recovery_seconds,result,detail" > "$CHAOS_CSV"

# Which scenarios run where.
#
# standard: recoverable and non-destructive. kill-9 is the headline — it is the
#   exact incident class the #926-#929 storage-durability work fixed, and the
#   test for it had never run.
# extended: needs elevated privileges or deliberately damages the environment.
CHAOS_SET="${CHAOS_SET:-standard}"
case "$CHAOS_SET" in
    standard) SCENARIOS=(kill-9-mid-forge macos-app-nap clock-skew inbound-syn-flood) ;;
    extended) SCENARIOS=(kill-9-mid-forge macos-app-nap clock-skew inbound-syn-flood network-partition disk-full) ;;
    *) echo "unknown CHAOS_SET: $CHAOS_SET (want standard|extended)" >&2; exit 2 ;;
esac

log_info "=== chaos/run.sh: set=$CHAOS_SET (${#SCENARIOS[@]} scenarios) ==="
log_info "Output: $CHAOS_CSV"

BEFORE=$(awk 'NR>1 && NF' "$CHAOS_CSV" | wc -l | tr -d ' ')
for name in "${SCENARIOS[@]}"; do
    f="$SCRIPT_DIR/$name.sh"
    if [ ! -f "$f" ]; then
        log_error "scenario script missing: $f"
        printf '%s,%s,%s,%s,%s,%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            "$name" "missing" 0 "FAIL" "script-not-found" >> "$CHAOS_CSV"
        continue
    fi
    log_info "--- $name ---"
    rows_before=$(awk 'NR>1 && NF' "$CHAOS_CSV" | wc -l | tr -d ' ')
    bash "$f" 2>&1 | sed 's/^/  /'
    rc=${PIPESTATUS[0]}
    rows_after=$(awk 'NR>1 && NF' "$CHAOS_CSV" | wc -l | tr -d ' ')
    # A scenario that records NOTHING is the silent-skip failure mode this
    # whole backlog exists to remove. Give it a row so it cannot vanish.
    if [ "$rows_after" -eq "$rows_before" ]; then
        log_error "$name produced no chaos-events row (rc=$rc)"
        printf '%s,%s,%s,%s,%s,%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            "$name" "no-row" 0 "FAIL" "scenario-recorded-nothing-rc=$rc" >> "$CHAOS_CSV"
    fi
done

TOTAL=$(awk 'NR>1 && NF' "$CHAOS_CSV" | wc -l | tr -d ' ')
PASS=$(awk -F, 'NR>1 && $5=="PASS" {c++} END{print c+0}' "$CHAOS_CSV")
FAIL=$(awk -F, 'NR>1 && $5=="FAIL" {c++} END{print c+0}' "$CHAOS_CSV")
ENVSKIP=$(awk -F, 'NR>1 && $5=="ENV_SKIP" {c++} END{print c+0}' "$CHAOS_CSV")
SKIP=$(awk -F, 'NR>1 && $5=="SKIP" {c++} END{print c+0}' "$CHAOS_CSV")

log_info ""
log_info "=== chaos summary ==="
log_info "  PASS:     $PASS"
log_info "  FAIL:     $FAIL"
log_info "  ENV_SKIP: $ENVSKIP  (missing tool/permission — the surface was NOT exercised)"
log_info "  SKIP:     $SKIP     (precondition absent this run)"
log_info "  rows:     $TOTAL"
log_info "  CSV:      $CHAOS_CSV"

# Pinned denominator, from outside the run.
DENOM_FILE="${DENOM_FILE:-$SCRIPT_DIR/../../../.claude/skills/devnet-validate/schemas/denominators.json}"
if [ -f "$DENOM_FILE" ] && [ "$CHAOS_SET" = "extended" ]; then
    EXPECTED=$(jq -r '.chaos.expected_cases // 0' "$DENOM_FILE")
    if [ "$TOTAL" -lt "$EXPECTED" ]; then
        log_error "FAIL: chaos-events.csv has $TOTAL rows, pinned denominator is $EXPECTED"
        exit 1
    fi
    log_info "  denominator: $TOTAL/$EXPECTED ✓"
fi

if [ "$ENVSKIP" -gt 0 ]; then
    log_warn "$ENVSKIP scenario(s) ENV-skipped — those surfaces were not exercised:"
    awk -F, 'NR>1 && $5=="ENV_SKIP" {print "    " $2 ": " $6}' "$CHAOS_CSV" >&2
fi

[ "$FAIL" -eq 0 ] || { log_error "FAIL: $FAIL chaos scenario(s) failed"; exit 1; }
log_info "PASS: chaos suite clean"
