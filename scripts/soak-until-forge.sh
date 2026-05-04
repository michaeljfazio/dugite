#!/usr/bin/env bash
# Run scripts/soak-bp-6h.sh in a loop until a KOIOS OK forge event lands
# (or KOIOS FAIL — either is a real-world result, both are end states for
# verifying the OpCert fix). Each cycle is 6 hours; the wrapper just relaunches
# the inner script when it exits.
#
# The BP and relay are assumed to already be running. The wrapper does NOT
# touch them — it only restarts the monitor.
set -uo pipefail
cd "$(dirname "$0")/.."

LOG_DIR=./logs/soak-bp-6h
mkdir -p "$LOG_DIR"
WRAPPER_LOG="$LOG_DIR/wrapper-$(date +%Y%m%d-%H%M%S).log"
ln -sf "$(basename "$WRAPPER_LOG")" "$LOG_DIR/wrapper.current.log"

emit() {
    local ts
    ts=$(date '+%Y-%m-%d %H:%M:%S')
    echo "[$ts] $*" | tee -a "$WRAPPER_LOG"
}

emit "WRAPPER START — will run 6h soak cycles until a forge lands"

CYCLE=0
while true; do
    CYCLE=$((CYCLE + 1))
    emit "CYCLE $CYCLE — launching ./scripts/soak-bp-6h.sh"

    ./scripts/soak-bp-6h.sh
    rc=$?

    emit "CYCLE $CYCLE — soak script exited (rc=$rc)"

    # Check whether any actual forge event happened in this run's report.
    # IMPORTANT: match the event-line emit prefix (`FORGE ADOPT —` with the
    # em-dash) NOT the script's end-of-cycle summary line which contains the
    # substring "FORGE ADOPT" (e.g. "Total FORGE ADOPT events:  0"). The
    # summary line has no em-dash, so anchoring on `FORGE ADOPT —` is enough.
    #
    # Stop only on KOIOS OK (block on canonical chain). KOIOS FAIL means the
    # block was orphaned by a slot-battle — that's a normal Praos outcome,
    # not a dugite issue, so continue cycling for a non-orphaned forge.
    latest_report=$(ls -t "$LOG_DIR"/report-*.log 2>/dev/null | head -1)
    if [[ -n "$latest_report" ]]; then
        forge_count=$(grep -c 'FORGE ADOPT —' "$latest_report" 2>/dev/null | head -1)
        koios_ok=$(grep -c 'KOIOS OK —' "$latest_report" 2>/dev/null | head -1)
        koios_fail=$(grep -c 'KOIOS FAIL — forged' "$latest_report" 2>/dev/null | head -1)
        : "${forge_count:=0}" "${koios_ok:=0}" "${koios_fail:=0}"
        emit "CYCLE $CYCLE result — forges=$forge_count koios_ok=$koios_ok koios_fail=$koios_fail (report: $latest_report)"

        if (( koios_ok > 0 )); then
            emit "WRAPPER STOP — KOIOS OK forge in cycle $CYCLE; block on canonical chain"
            emit "Final result: forges=$forge_count KOIOS OK=$koios_ok KOIOS FAIL=$koios_fail"
            exit 0
        fi
        if (( forge_count > 0 )); then
            emit "CYCLE $CYCLE — forged $forge_count blocks but all orphaned by slot-battles (KOIOS FAIL=$koios_fail); continuing"
        fi
    else
        emit "CYCLE $CYCLE — no report file found (script may have failed early)"
    fi

    emit "CYCLE $CYCLE — no forge yet; sleeping 30s before next cycle"
    sleep 30
done
