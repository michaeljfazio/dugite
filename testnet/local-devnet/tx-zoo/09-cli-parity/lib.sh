#!/usr/bin/env bash
# Shared library for CLI parity tests (09-cli-parity/).
#
# Exports:
#   parity_query_json  — run a cardano-cli query against both sockets, diff JSON
#   parity_record      — write one row to cli-parity.csv
#   PARITY_CSV         — path to the output CSV
#   KNOWN_DIVERGENCES  — associative array: query_name → tracking issue URL
#
# Source: . "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
set -euo pipefail

# ---- Environment ----------------------------------------------------------

_LIB_SELF="${BASH_SOURCE[0]:-$0}"
ZOO09_DIR="$(cd "$(dirname "$_LIB_SELF")" && pwd)"
ZOO_DIR="$(cd "$ZOO09_DIR/.." && pwd)"
LD_ROOT="$(cd "$ZOO_DIR/.." && pwd)"

. "$LD_ROOT/lib/common.sh"

# Output CSV — written by run.sh; individual scripts append rows.
PARITY_CSV="${PARITY_CSV:-$LD_ROOT/evidence/current/cli-parity.csv}"

# Ensure the CSV exists with a header if it doesn't yet.
_parity_ensure_csv() {
    local csv="${PARITY_CSV}"
    local dir; dir="$(dirname "$csv")"
    mkdir -p "$dir"
    if [ ! -f "$csv" ]; then
        echo "ts,query,status,dugite_sha256,cardano_sha256,equal,notes" > "$csv"
    fi
}

# ---- Known divergences ----------------------------------------------------
# Maps query name → tracking issue URL.
# Populated by individual test scripts using: KNOWN_DIVERGENCES[name]=url
declare -gA KNOWN_DIVERGENCES=(
    # This array is ONLY for real divergences — both sides answered, the
    # answers differ. It must never be used to paper over an ERROR row: an
    # ERROR means cardano-cli refused the invocation or could not reach a
    # node, which is a harness bug, not a dugite behaviour to track. See #900.
    #
    # Tracked under #905, NOT #597 -- #597 was closed 2026-05-22 and these were
    # re-added against it on 2026-05-28, so nothing was watching them. #597's
    # fixes also all targeted dugite-cli, which this suite never invokes: it
    # runs cardano-cli against BOTH sockets, so what it measures is
    # dugite-NODE's LSQ responses.
    ["protocol-parameters"]="https://github.com/michaeljfazio/dugite/issues/905"
    ["stake-distribution"]="https://github.com/michaeljfazio/dugite/issues/905"
    # Proposal sets genuinely differ -- dugite ratifies a ParameterChange an
    # epoch early and drops its sibling. Same root cause as the proposals row.
    ["gov-state"]="https://github.com/michaeljfazio/dugite/issues/903"
    # drep-stake-distribution diverges in the per-DRep ordering; future-pparams
    # in the no-pending-proposal "null" envelope shape. Neither is characterised
    # at field level yet -- the next run writes cli-parity-diffs/ for that.
    ["drep-stake-distribution"]="https://github.com/michaeljfazio/dugite/issues/905"
    ["future-pparams"]="https://github.com/michaeljfazio/dugite/issues/905"
    # Root-caused 2026-07-26: the "error" protocolVersion is the 7-vs-8-field
    # PraosState encoder, same bug as the kes-period-info error below. Retargeted
    # off the #597 umbrella onto the specific issue so both close together.
    ["protocol-state/version"]="https://github.com/michaeljfazio/dugite/issues/902"
    # dugite ratifies a ParameterChange in the epoch it was submitted and drops
    # the sibling, so the live sets differ (6 vs 8 at an identical tip). Found
    # once #900 stopped the missing --all-proposals selector masking this row.
    ["proposals"]="https://github.com/michaeljfazio/dugite/issues/903"
    # NB: slot-number and treasury were listed here until 2026-07-26. They never
    # diverged — cardano-cli rejected the harness's own arguments, both sides
    # failed identically, and lib.sh mislabelled that as "dugite ERROR". Fixed
    # in #900; do not re-add without a real two-sided diff.
)

# ---- Known errors ---------------------------------------------------------
# Queries that genuinely ERROR against dugite because of a tracked dugite bug,
# mapped to the issue. Distinct from KNOWN_DIVERGENCES: there is no two-sided
# comparison to make, the query does not complete at all.
#
# An entry here is a promise that the failure is understood and filed, NOT
# permission to ignore it. run.sh still prints these; it just does not fail the
# round on them. Anything erroring that is NOT listed here fails the round.
declare -gA KNOWN_ERRORS=(
    # PraosState array(7) vs cardano-node 11.0.x's expected array(8).
    ["kes-period-info"]="https://github.com/michaeljfazio/dugite/issues/902"
)

# ---- Core parity function -------------------------------------------------

# parity_query_json <query_name> <cli_args...>
#
# Runs:  cardano-cli conway query <cli_args...> --testnet-magic $LD_MAGIC
# against both sockets ($LD_DUGITE_BP_SOCK and $LD_CARDANO_BP_SOCK).
#
# Compares normalised JSON (jq -Sc) — byte-identical output = EQUAL.
# Differences are diffed and the sha256 of each side is recorded.
#
# Comparison mode can be overridden via PARITY_MODE env var:
#   exact     — raw string compare (default for stable queries)
#   norm      — jq-sorted, number-rounded compare (for slot-dependent data)
#   skip      — record SKIP without comparing (for inherently divergent queries)
#
# Returns: 0 (equal or skip), 1 (divergent), 2 (error on one or both sides)
parity_query_json() {
    local query_name="$1"
    shift
    local cli_args=("$@")

    _parity_ensure_csv

    local mode="${PARITY_MODE:-exact}"
    local ts; ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    # Skip mode — just record without comparing
    if [ "$mode" = "skip" ]; then
        parity_record "$query_name" "SKIP" "skip" "skip" "skip-mode"
        return 0
    fi

    # Not every cardano-cli query accepts --output-json. `treasury` and
    # `slot-number` (11.0.0.0) have no output-format flags at all, and passing
    # one makes optparse-applicative reject the whole invocation before either
    # node is contacted. Scripts opt out with PARITY_OUTPUT_JSON=0.
    local fmt_args=()
    [ "${PARITY_OUTPUT_JSON:-1}" = "1" ] && fmt_args+=(--output-json)

    local dugite_out cardano_out dugite_rc cardano_rc
    dugite_out=$(cardano-cli conway query "${cli_args[@]}" \
                    --testnet-magic "$LD_MAGIC" \
                    --socket-path "$LD_DUGITE_BP_SOCK" \
                    "${fmt_args[@]}" 2>&1) && dugite_rc=0 || dugite_rc=$?
    cardano_out=$(cardano-cli conway query "${cli_args[@]}" \
                    --testnet-magic "$LD_MAGIC" \
                    --socket-path "$LD_CARDANO_BP_SOCK" \
                    "${fmt_args[@]}" 2>&1) && cardano_rc=0 || cardano_rc=$?

    if [ "$dugite_rc" -ne 0 ] || [ "$cardano_rc" -ne 0 ]; then
        # Attribute the failure to the side that actually failed. Both sides
        # run the SAME cardano-cli binary with the SAME arguments and differ
        # only in --socket-path, so a both-sides failure is the harness's own
        # invocation being wrong (bad flag, missing required arg, unreadable
        # file) — never a dugite gap. Blanket-labelling it "dugite ERROR" is
        # what made #900 look like four dugite-cli defects.
        local note
        if [ "$dugite_rc" -ne 0 ] && [ "$cardano_rc" -ne 0 ]; then
            note="HARNESS both-sides-failed rc=$dugite_rc/$cardano_rc: $(echo "$cardano_out" | head -1)"
        elif [ "$dugite_rc" -ne 0 ]; then
            note="dugite ERROR rc=$dugite_rc: $(echo "$dugite_out" | head -1)"
        else
            note="cardano ERROR rc=$cardano_rc: $(echo "$cardano_out" | head -1)"
        fi
        parity_record "$query_name" "ERROR" "error" "error" "$note"
        return 2
    fi

    # Normalise for comparison
    local dugite_norm cardano_norm
    case "$mode" in
        norm)
            dugite_norm=$(echo "$dugite_out" | jq -Sc . 2>/dev/null || echo "$dugite_out")
            cardano_norm=$(echo "$cardano_out" | jq -Sc . 2>/dev/null || echo "$cardano_out")
            ;;
        *)
            # exact: still sort JSON keys for byte-stable comparison
            dugite_norm=$(echo "$dugite_out" | jq -Sc . 2>/dev/null || echo "$dugite_out")
            cardano_norm=$(echo "$cardano_out" | jq -Sc . 2>/dev/null || echo "$cardano_out")
            ;;
    esac

    local dugite_sha cardano_sha
    dugite_sha=$(printf '%s' "$dugite_norm" | sha256sum | awk '{print $1}')
    cardano_sha=$(printf '%s' "$cardano_norm" | sha256sum | awk '{print $1}')

    local equal note
    if [ "$dugite_sha" = "$cardano_sha" ]; then
        equal="true"
        note=""
    else
        equal="false"
        # Capture BOTH sides plus a unified diff next to the CSV.
        #
        # Recording only sha256 pairs made every divergence opaque: the row told
        # you something differed but not what, so triaging one meant re-running
        # the whole devnet by hand. That is how five rows sat parked on the
        # closed #597 for two months without anyone able to say what they were.
        # `jq -S .` (indented, key-sorted) makes the diff readable rather than
        # one enormous line.
        local diff_dir="$(dirname "$PARITY_CSV")/cli-parity-diffs"
        mkdir -p "$diff_dir"
        local safe_name="${query_name//\//_}"
        printf '%s\n' "$dugite_out"  | jq -S . 2>/dev/null > "$diff_dir/${safe_name}.dugite.json"  || printf '%s\n' "$dugite_out"  > "$diff_dir/${safe_name}.dugite.json"
        printf '%s\n' "$cardano_out" | jq -S . 2>/dev/null > "$diff_dir/${safe_name}.cardano.json" || printf '%s\n' "$cardano_out" > "$diff_dir/${safe_name}.cardano.json"
        diff -u "$diff_dir/${safe_name}.cardano.json" "$diff_dir/${safe_name}.dugite.json" \
            > "$diff_dir/${safe_name}.diff" 2>/dev/null || true
        local diff_lines
        diff_lines=$(grep -cE '^[+-][^+-]' "$diff_dir/${safe_name}.diff" 2>/dev/null || echo 0)

        # Check if this is a known divergence
        if [[ -v "KNOWN_DIVERGENCES[$query_name]" ]]; then
            note="known-divergence:${KNOWN_DIVERGENCES[$query_name]} difflines=${diff_lines}"
        else
            note="DIVERGENT difflines=${diff_lines}"
        fi
    fi

    parity_record "$query_name" "$([ "$equal" = "true" ] && echo EQUAL || echo DIVERGENT)" \
        "$dugite_sha" "$cardano_sha" "$note"

    [ "$equal" = "true" ] && return 0 || return 1
}

# parity_record <query_name> <status> <dugite_sha256> <cardano_sha256> <notes>
parity_record() {
    local qname="$1" status="$2" dsha="$3" csha="$4" notes="${5:-}"
    _parity_ensure_csv
    # If this query is known to diverge, prefix the note so the runner's
    # CSV tally excludes it from the non-known DIVERGENT count.  Scripts
    # that bypass parity_query_json (e.g. 09h) still get the same treatment.
    if [ "$status" = "DIVERGENT" ] && [[ -v "KNOWN_DIVERGENCES[$qname]" ]]; then
        notes="known-divergence:${KNOWN_DIVERGENCES[$qname]} ${notes}"
    fi
    if [ "$status" = "ERROR" ] && [[ -v "KNOWN_ERRORS[$qname]" ]]; then
        notes="known-error:${KNOWN_ERRORS[$qname]} ${notes}"
    fi
    # `equal` is derived from the STATUS, not from a sha compare. ERROR rows
    # carry the sentinel sha "error" on both sides, so the old
    # `[ "$dsha" = "$csha" ]` test scored every failed query as equal=true.
    local equal
    case "$status" in
        EQUAL) equal=true  ;;
        SKIP)  equal=skip  ;;   # nothing was compared; not a failure
        *)     equal=false ;;   # DIVERGENT, ERROR
    esac
    printf '%s,%s,%s,%s,%s,%s,%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$qname" "$status" "$dsha" "$csha" "$equal" \
        "${notes//,/;}" \
        >> "$PARITY_CSV"
    local icon
    case "$status" in
        EQUAL)    icon="✓" ;;
        DIVERGENT) icon="✗" ;;
        SKIP)     icon="-" ;;
        ERROR)    icon="!" ;;
        *)        icon="?" ;;
    esac
    printf '[09-cli-parity] %s %-40s %s\n' "$icon" "$qname" "${notes:-}" >&2
}

# Check that sockets are available — call this at the start of run.sh
parity_check_sockets() {
    local ok=1
    if ! [ -S "$LD_DUGITE_BP_SOCK" ]; then
        log_error "dugite-bp socket not found: $LD_DUGITE_BP_SOCK (is the devnet running?)"
        ok=0
    fi
    if ! [ -S "$LD_CARDANO_BP_SOCK" ]; then
        log_error "cardano-bp socket not found: $LD_CARDANO_BP_SOCK"
        ok=0
    fi
    [ "$ok" -eq 1 ]
}
