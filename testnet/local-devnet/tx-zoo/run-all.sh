#!/usr/bin/env bash
# tx-zoo orchestrator. Runs scripts in lexical order across all (or selected)
# categories, recording results into state/results.csv.
#
# Usage:
#   ./run-all.sh              run everything (after a prior --setup)
#   ./run-all.sh --setup      check tooling, generate keys + Plutus binaries, exit
#   ./run-all.sh 01-bookkeeping 03-plutus    run only the named categories
#   ./run-all.sh --list       list scripts that would run
#   ./run-all.sh --reset      wipe state/ then exit
#   ./run-all.sh --summary    print final summary from results.csv
#   ./run-all.sh --strict-skips   exit non-zero if any script ENV-skips
#
# Skips are classified (see lib/tx-zoo-common.sh):
#   ENV-SKIP    the check could not run at all — missing tool/key/capability.
#               Structural: it will skip identically every run, so the surface
#               is never exercised. --strict-skips turns these into failures.
#   SKIP        the chain legitimately lacked the precondition this round
#               (e.g. 04g no-rewards before the first epoch boundary).
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
    10-gov-lifecycle
    11-mempool
    # MUST run after 10-gov-lifecycle: these negatives are only meaningful once
    # a governance action has actually been ENACTED (non-null root). Running
    # them earlier makes them SKIP, which is how the InvalidPrevGovActionId P0
    # went unnoticed — 06-proposals runs before any enactment, where
    # prev_action_id=None is legitimately valid.
    12-post-enactment
    # Plutus ScriptPurposes beyond spend/mint (#955). Runs last: 13d
    # deregisters the script stake credential, and 13g/13h create their own
    # DRep and governance action, so keeping it after 10/12 means it cannot
    # perturb the governance lifecycle those categories assert on.
    13-script-purposes
    # Governance NEGATIVES (#956). After 06 (which creates the actions they
    # vote on) and after 13 (which registers/deregisters its own DRep), so a
    # rejection here is never an artefact of another category's state.
    14-gov-negatives
)

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; }

# Number of SKIP rows whose reason classifies as environmental. Printed by
# print_summary and consulted by --strict-skips.
ENV_SKIP_COUNT=0

# Emit "<name>\t<detail>" for every SKIP row of the given class (env|state).
#
# Note the `done < <(...)` rather than a pipeline: the sourced common lib turns
# on `set -e`, and a `while` loop on the right of a pipe runs in a subshell
# whose exit status can be the last (false) test, which would abort run-all.sh
# mid-summary.
skips_of_class() {
    local want="$1" name detail
    [ -f "$ZOO_RESULTS_CSV" ] || return 0
    # Field 5 is the detail; commas are stripped at record time, so a plain
    # comma split is safe. Read name+detail, classify, print the matches.
    while IFS=$'\t' read -r name detail; do
        if [ "$(zoo_skip_class "$detail")" = "$want" ]; then
            printf '%s\t%s\n' "$name" "$detail"
        fi
    done < <(awk -F, 'NR>1 && $3=="SKIP" {printf "%s\t%s\n", $2, $5}' "$ZOO_RESULTS_CSV")
    return 0
}

print_summary() {
    [ -f "$ZOO_RESULTS_CSV" ] || { echo "no results yet"; return; }
    local total pass fail skip env_skip state_skip
    total=$(tail -n +2 "$ZOO_RESULTS_CSV" | wc -l | tr -d ' ')
    pass=$( awk -F, 'NR>1 && $3=="PASS"' "$ZOO_RESULTS_CSV" | wc -l | tr -d ' ')
    fail=$( awk -F, 'NR>1 && $3=="FAIL"' "$ZOO_RESULTS_CSV" | wc -l | tr -d ' ')
    skip=$( awk -F, 'NR>1 && $3=="SKIP"' "$ZOO_RESULTS_CSV" | wc -l | tr -d ' ')
    # `wc -l`, not `grep -c .`: the sourced common lib turns on `set -e`, and
    # grep exits 1 on no match, which would abort the summary when — as we
    # want — there are zero environmental skips.
    env_skip=$(skips_of_class env | wc -l | tr -d ' ')
    state_skip=$(( skip - env_skip ))
    ENV_SKIP_COUNT="$env_skip"
    printf '\n=== tx-zoo summary ===\n  total=%d  pass=%d  fail=%d  skip=%d  (%d ENV-SKIP / %d state-skip)\n' \
        "$total" "$pass" "$fail" "$skip" "$env_skip" "$state_skip"
    if [ "$fail" -gt 0 ]; then
        echo
        echo "FAILED scripts:"
        awk -F, 'NR>1 && $3=="FAIL" {printf "  %-44s %s\n", $2, $5}' "$ZOO_RESULTS_CSV"
    fi
    if [ "$env_skip" -gt 0 ]; then
        echo
        echo "ENV-SKIPPED scripts (coverage did NOT run — structural gap):"
        while IFS=$'\t' read -r name detail; do
            printf '  %-44s %s\n' "$name" "$detail"
        done < <(skips_of_class env)
    fi
    if [ "$state_skip" -gt 0 ]; then
        echo
        echo "SKIPPED scripts (precondition absent this round):"
        while IFS=$'\t' read -r name detail; do
            printf '  %-44s %s\n' "$name" "$detail"
        done < <(skips_of_class state)
    fi
    return 0
}

# Argument parsing
SELECTED=()
DO_SETUP=0; DO_LIST=0; DO_RESET=0; DO_SUMMARY=0; STRICT_SKIPS=0
for arg in "$@"; do
    case "$arg" in
        --setup)   DO_SETUP=1 ;;
        --list)    DO_LIST=1 ;;
        --reset)   DO_RESET=1 ;;
        --summary) DO_SUMMARY=1 ;;
        --strict-skips) STRICT_SKIPS=1 ;;
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
    if [ "$STRICT_SKIPS" = "1" ] && [ "$ENV_SKIP_COUNT" -gt 0 ]; then
        zoo_fail "--strict-skips: $ENV_SKIP_COUNT environmental skip(s)"
        exit 3
    fi
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
    # Fail LOUDLY here on a missing required tool. Discovering it per-script at
    # run time produces a SKIP, and a SKIP reads as a pass in the summary line.
    zoo_require_tools
    zoo_require_devnet
    "$SCRIPT_DIR/lib/build-plutus.sh"
    "$SCRIPT_DIR/lib/keygen.sh"
    zoo_info "setup complete"
    exit 0
fi

# Default run: ensure setup ran at least once. Build-plutus is idempotent;
# keygen will short-circuit on existing keys.
zoo_require_tools
zoo_require_devnet
"$SCRIPT_DIR/lib/build-plutus.sh"
"$SCRIPT_DIR/lib/keygen.sh"

# Reset the results CSV for this run, and truncate per-script logs so the
# next debugger sees only output from THIS invocation (run-all redirects
# stderr with `>>`, so stale errors from prior runs would otherwise
# accumulate and mask the current failure).
> "$ZOO_RESULTS_CSV"
echo "ts,name,status,txid,detail" > "$ZOO_RESULTS_CSV"
mkdir -p "$ZOO_LOGS"
find "$ZOO_LOGS" -maxdepth 1 -name '*.log' -type f -exec sh -c ': > "$1"' _ {} \; 2>/dev/null || true

# cardano-cli 11.0 fetches --anchor-url / --metadata-url / --drep-metadata-url
# at build time and validates the downloaded content against the supplied hash.
# Stand up a local HTTP server that serves predictable anchor JSON files so
# cert-/proposal-create scripts don't crash on placeholder URLs (#515 follow-up).
zoo_anchor_start
trap 'zoo_anchor_stop' EXIT

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
if [ "$STRICT_SKIPS" = "1" ] && [ "$ENV_SKIP_COUNT" -gt 0 ]; then
    zoo_fail "--strict-skips: $ENV_SKIP_COUNT environmental skip(s) — that coverage did not run"
    [ "$EXIT_RC" -eq 0 ] && EXIT_RC=3
fi
exit "$EXIT_RC"
