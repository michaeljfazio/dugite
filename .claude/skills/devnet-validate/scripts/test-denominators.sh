#!/usr/bin/env bash
# test-denominators.sh — assert schemas/denominators.json still matches the
# suites on disk.
#
# A pinned denominator only protects the gate while it stays true. If someone
# adds a tx-zoo script or a cli-parity query without bumping the pin, the gate
# silently widens: the new case can fail to run and the count still clears the
# stale, lower bar. This script is the counterweight — it fails when the pin and
# the filesystem disagree, in EITHER direction.
#
# Case counts that can only be observed at runtime (n2n_adversarial.expected_cases,
# chaos.expected_cases) are checked against the SCRIPT count here; their case
# counts are asserted live by protocols/run.sh and chaos/run.sh.
#
# Usage: test-denominators.sh [--devnet-root <path>]
# Exit: 0 = manifest matches disk; 1 = drift.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DENOM="$SCRIPT_DIR/../schemas/denominators.json"
ROOT="$SCRIPT_DIR/../../../../testnet/local-devnet"
[ "${1:-}" = "--devnet-root" ] && ROOT="$2"

[ -f "$DENOM" ] || { echo "denominator manifest not found: $DENOM" >&2; exit 1; }
[ -d "$ROOT" ]  || { echo "devnet root not found: $ROOT" >&2; exit 1; }

PASSED=0; FAILED=0
check() { # check <label> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf '  \033[32mPASS\033[0m  %-46s pinned=%-4s disk=%s\n' "$1" "$2" "$3"
        PASSED=$(( PASSED + 1 ))
    else
        printf '  \033[31mFAIL\033[0m  %-46s pinned=%-4s disk=%s\n' "$1" "$2" "$3"
        FAILED=$(( FAILED + 1 ))
    fi
}

echo "=== denominator manifest vs disk ==="
echo "  manifest: $DENOM"
echo "  devnet:   $ROOT"
echo

# --- tx-zoo, per category and in total ---
TOTAL_DISK=0
while IFS= read -r cat; do
    pinned=$(jq -r ".tx_zoo.per_category[\"$cat\"] // \"absent\"" "$DENOM")
    disk=$(ls "$ROOT/tx-zoo/$cat"/[0-9]*.sh 2>/dev/null | wc -l | tr -d ' ')
    check "tx-zoo/$cat" "$pinned" "$disk"
    TOTAL_DISK=$(( TOTAL_DISK + disk ))
done < <(jq -r '.tx_zoo.per_category | keys[]' "$DENOM")

# Any category on disk that the manifest does not know about is drift too —
# otherwise a whole new category could be added and never pinned.
while IFS= read -r d; do
    cat=$(basename "$d")
    [ "$cat" = "09-cli-parity" ] && continue
    known=$(jq -r ".tx_zoo.per_category | has(\"$cat\")" "$DENOM")
    if [ "$known" != "true" ]; then
        printf '  \033[31mFAIL\033[0m  %-46s present on disk, absent from manifest\n' "tx-zoo/$cat"
        FAILED=$(( FAILED + 1 ))
    fi
done < <(find "$ROOT/tx-zoo" -maxdepth 1 -type d -name '[0-9]*' | sort)

check "tx_zoo.expected_scripts (sum)" \
      "$(jq -r '.tx_zoo.expected_scripts' "$DENOM")" "$TOTAL_DISK"

# --- cli-parity ---
# Two pins, both derived from disk. `expected_queries` is the script count;
# `expected_rows` is what actually lands in cli-parity.csv, and they differ
# because some scripts emit an extra assertion row (#963's
# parity_assert_pool_filter). Deriving both here is what keeps the seam
# between "scripts on disk" and "rows the gate counts" from drifting.
CLI_SCRIPTS=$(ls "$ROOT/tx-zoo/09-cli-parity"/09*.sh 2>/dev/null | wc -l | tr -d ' ')
CLI_EXTRA=$(grep -cE '^[[:space:]]*(parity_assert_pool_filter|PARITY_JQ_FILTER=)' \
              "$ROOT/tx-zoo/09-cli-parity"/09*.sh 2>/dev/null \
            | awk -F: '{s+=$2} END{print s+0}')
check "cli_parity.expected_queries" \
      "$(jq -r '.cli_parity.expected_queries' "$DENOM")" "$CLI_SCRIPTS"
check "cli_parity.expected_rows" \
      "$(jq -r '.cli_parity.expected_rows' "$DENOM")" \
      "$(( CLI_SCRIPTS + CLI_EXTRA ))"

# --- adversarial N2N ---
check "n2n_adversarial.expected_scripts" \
      "$(jq -r '.n2n_adversarial.expected_scripts' "$DENOM")" \
      "$(ls "$ROOT/protocols"/[0-9]*.sh 2>/dev/null | wc -l | tr -d ' ')"

# --- chaos ---
check "chaos.expected_scripts" \
      "$(jq -r '.chaos.expected_scripts' "$DENOM")" \
      "$(find "$ROOT/chaos" -maxdepth 1 -name '*.sh' ! -name 'lib.sh' ! -name 'run.sh' | wc -l | tr -d ' ')"

# --- parity-matrix required categories must all exist ---
PARITY_SUM=0
while IFS= read -r cat; do
    if [ -d "$ROOT/tx-zoo/$cat" ]; then
        printf '  \033[32mPASS\033[0m  %-46s exists\n' "parity required: $cat"
        PASSED=$(( PASSED + 1 ))
        PARITY_SUM=$(( PARITY_SUM + $(ls "$ROOT/tx-zoo/$cat"/[0-9]*.sh 2>/dev/null | wc -l | tr -d ' ') ))
    else
        printf '  \033[31mFAIL\033[0m  %-46s MISSING\n' "parity required: $cat"
        FAILED=$(( FAILED + 1 ))
    fi
done < <(jq -r '.parity_matrix.required_categories_standard[]' "$DENOM")

# The parity denominator must equal the scripts in the required categories.
# Otherwise the oracle can silently cover fewer scripts than the pin claims —
# the exact shape of the "41/41 out of an unstated 85" problem this closed.
check "parity_matrix.expected_scripts_standard" \
      "$(jq -r '.parity_matrix.expected_scripts_standard' "$DENOM")" "$PARITY_SUM"

# Required and excluded category sets must partition the zoo: every category
# must be either covered by the oracle or explicitly excluded WITH a reason.
# Silence about a category is how one gets left out unnoticed.
while IFS= read -r cat; do
    req=$(jq -r --arg c "$cat" '.parity_matrix.required_categories_standard | index($c) != null' "$DENOM")
    exc=$(jq -r --arg c "$cat" '.parity_matrix.excluded_categories | has($c)' "$DENOM")
    if [ "$req" = "true" ] || [ "$exc" = "true" ]; then
        PASSED=$(( PASSED + 1 ))
    else
        printf '  \033[31mFAIL\033[0m  %-46s neither required nor excluded by the parity manifest\n' "$cat"
        FAILED=$(( FAILED + 1 ))
    fi
done < <(jq -r '.tx_zoo.per_category | keys[]' "$DENOM")
printf '  \033[32mPASS\033[0m  %-46s every category is required or excluded\n' "parity manifest partitions the zoo"

echo
echo "=== $PASSED passed, $FAILED failed ==="
if [ "$FAILED" -gt 0 ]; then
    echo
    echo "The pin and the filesystem disagree. Do NOT just edit the JSON to match:"
    echo "a denominator is only meaningful if the new count was OBSERVED. Run the"
    echo "affected suite, confirm the new cases actually execute, then bump the pin"
    echo "in the same commit as the scripts."
    exit 1
fi
