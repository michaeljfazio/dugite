#!/usr/bin/env bash
# tx-zoo orchestrator. Runs scripts in lexical order across all (or selected)
# categories, recording results into state/results.csv.
#
# Usage:
#   ./run-all.sh              run everything (after a prior --setup)
#   ./run-all.sh --setup      generate keys + Plutus binaries, then exit
#   ./run-all.sh 01-bookkeeping 03-plutus    run only the named categories
#   ./run-all.sh --list       list scripts that would run
#   ./run-all.sh --reset      wipe state/ then exit
#   ./run-all.sh --summary    print final summary from results.csv
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/tx-zoo-common.sh"

ALL_CATEGORIES=(
    01-bookkeeping
    02-native-scripts
    03-plutus
    04-stake
    05-governance-certs
    06-proposals
    07-voting
    08-negative
)

usage() { sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; }

print_summary() {
    [ -f "$ZOO_RESULTS_CSV" ] || { echo "no results yet"; return; }
    local total pass fail skip
    total=$(tail -n +2 "$ZOO_RESULTS_CSV" | wc -l | tr -d ' ')
    pass=$( awk -F, 'NR>1 && $3=="PASS"' "$ZOO_RESULTS_CSV" | wc -l | tr -d ' ')
    fail=$( awk -F, 'NR>1 && $3=="FAIL"' "$ZOO_RESULTS_CSV" | wc -l | tr -d ' ')
    skip=$( awk -F, 'NR>1 && $3=="SKIP"' "$ZOO_RESULTS_CSV" | wc -l | tr -d ' ')
    printf '\n=== tx-zoo summary ===\n  total=%d  pass=%d  fail=%d  skip=%d\n' \
        "$total" "$pass" "$fail" "$skip"
    if [ "$fail" -gt 0 ]; then
        echo
        echo "FAILED scripts:"
        awk -F, 'NR>1 && $3=="FAIL" {printf "  %-44s %s\n", $2, $5}' "$ZOO_RESULTS_CSV"
    fi
    if [ "$skip" -gt 0 ]; then
        echo
        echo "SKIPPED scripts:"
        awk -F, 'NR>1 && $3=="SKIP" {printf "  %-44s %s\n", $2, $5}' "$ZOO_RESULTS_CSV"
    fi
}

# Argument parsing
SELECTED=()
DO_SETUP=0; DO_LIST=0; DO_RESET=0; DO_SUMMARY=0
for arg in "$@"; do
    case "$arg" in
        --setup)   DO_SETUP=1 ;;
        --list)    DO_LIST=1 ;;
        --reset)   DO_RESET=1 ;;
        --summary) DO_SUMMARY=1 ;;
        --help|-h) usage; exit 0 ;;
        --*)       echo "unknown flag: $arg" >&2; usage >&2; exit 2 ;;
        *)         SELECTED+=("$arg") ;;
    esac
done
[ ${#SELECTED[@]} -eq 0 ] && SELECTED=("${ALL_CATEGORIES[@]}")

if [ "$DO_RESET" = "1" ]; then
    zoo_info "wiping $ZOO_STATE"
    rm -rf "$ZOO_STATE"
    exit 0
fi
if [ "$DO_SUMMARY" = "1" ]; then
    print_summary
    exit 0
fi
if [ "$DO_LIST" = "1" ]; then
    for cat in "${SELECTED[@]}"; do
        for s in "$SCRIPT_DIR/$cat"/[0-9]*.sh; do
            [ -e "$s" ] && echo "$cat/$(basename "$s")"
        done
    done
    exit 0
fi
if [ "$DO_SETUP" = "1" ]; then
    zoo_require_devnet
    "$SCRIPT_DIR/lib/build-plutus.sh"
    "$SCRIPT_DIR/lib/keygen.sh"
    zoo_info "setup complete"
    exit 0
fi

# Default run: ensure setup ran at least once. Build-plutus is idempotent;
# keygen will short-circuit on existing keys.
zoo_require_devnet
"$SCRIPT_DIR/lib/build-plutus.sh"
"$SCRIPT_DIR/lib/keygen.sh"

# Reset the results CSV for this run (preserve old logs/built artifacts).
> "$ZOO_RESULTS_CSV"
echo "ts,name,status,txid,detail" > "$ZOO_RESULTS_CSV"

EXIT_RC=0
for cat in "${SELECTED[@]}"; do
    cat_dir="$SCRIPT_DIR/$cat"
    if [ ! -d "$cat_dir" ]; then
        zoo_fail "unknown category: $cat"
        EXIT_RC=2
        continue
    fi
    zoo_info ">>> category $cat"
    for s in "$cat_dir"/[0-9]*.sh; do
        [ -e "$s" ] || continue
        sname="$(basename "$s")"
        zoo_info "  --> $sname"
        if ! "$s" 2>>"$ZOO_LOGS/$(basename "$s" .sh).log"; then
            zoo_fail "  $sname EXITED non-zero (see $ZOO_LOGS/$(basename "$s" .sh).log)"
            EXIT_RC=1
        fi
    done
done

print_summary
exit "$EXIT_RC"
