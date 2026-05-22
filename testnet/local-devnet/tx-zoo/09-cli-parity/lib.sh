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
        echo "ts,query,dugite_sha256,cardano_sha256,equal,notes" > "$csv"
    fi
}

# ---- Known divergences ----------------------------------------------------
# Maps query name → tracking issue URL.
# Populated by individual test scripts using: KNOWN_DIVERGENCES[name]=url
declare -gA KNOWN_DIVERGENCES=(
    # All tracked under the umbrella issue #597 until each query is fixed.
    ["protocol-parameters"]="https://github.com/michaeljfazio/dugite/issues/597"
    ["stake-distribution"]="https://github.com/michaeljfazio/dugite/issues/597"
    ["protocol-state/version"]="https://github.com/michaeljfazio/dugite/issues/597"
    ["gov-state"]="https://github.com/michaeljfazio/dugite/issues/597"
    ["kes-period-info"]="https://github.com/michaeljfazio/dugite/issues/597"
    ["slot-number"]="https://github.com/michaeljfazio/dugite/issues/597"
    ["treasury"]="https://github.com/michaeljfazio/dugite/issues/597"
    ["proposals"]="https://github.com/michaeljfazio/dugite/issues/597"
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

    local dugite_out cardano_out dugite_rc cardano_rc
    dugite_out=$(cardano-cli conway query "${cli_args[@]}" \
                    --testnet-magic "$LD_MAGIC" \
                    --socket-path "$LD_DUGITE_BP_SOCK" \
                    --output-json 2>&1) && dugite_rc=0 || dugite_rc=$?
    cardano_out=$(cardano-cli conway query "${cli_args[@]}" \
                    --testnet-magic "$LD_MAGIC" \
                    --socket-path "$LD_CARDANO_BP_SOCK" \
                    --output-json 2>&1) && cardano_rc=0 || cardano_rc=$?

    if [ "$dugite_rc" -ne 0 ] || [ "$cardano_rc" -ne 0 ]; then
        local note="dugite_rc=$dugite_rc cardano_rc=$cardano_rc"
        [ "$dugite_rc" -ne 0 ] && note="dugite ERROR: $(echo "$dugite_out" | head -1)"
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
        # Check if this is a known divergence
        if [[ -v "KNOWN_DIVERGENCES[$query_name]" ]]; then
            note="known-divergence:${KNOWN_DIVERGENCES[$query_name]}"
        else
            note="DIVERGENT"
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
    printf '%s,%s,%s,%s,%s,%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$qname" "$dsha" "$csha" \
        "$([ "$dsha" = "$csha" ] && echo true || echo false)" \
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
