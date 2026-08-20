#!/usr/bin/env bash
# cli-surface-parity-selftest.sh — exercise cli-surface-parity.sh's walker,
# parser, and allowlist logic against small fixture stub CLIs (#1006).
#
# No real cardano-cli / dugite-cli binary is needed — this proves the CHECK
# ITSELF is correct: that it detects a real gap (MISSING, RED), passes clean
# on a matching tree, reports a superset without failing, and enforces a
# non-stale allowlist. "A green check nobody has seen go red is worth
# nothing" — this is that RED demonstration, made reproducible instead of
# a one-off manual run.
#
# Fixture tree (scripts/validation/fixtures/cli-surface/):
#   fixture-reference-cli.sh   stands in for cardano-cli: alpha/{one,two},
#                              beta (two-paragraph description — the exact
#                              shape that broke the parser against real
#                              cardano-cli output, see cli-surface-parity.sh
#                              header comment), gamma. 4 leaves total.
#   fixture-target-cli.sh      stands in for dugite-cli, three variants via
#                              $FIXTURE_VARIANT: full / missing / superset.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARITY_SCRIPT="$SCRIPT_DIR/cli-surface-parity.sh"
FIXTURE_DIR="$SCRIPT_DIR/fixtures/cli-surface"
REFERENCE="$FIXTURE_DIR/fixture-reference-cli.sh"
TARGET="$FIXTURE_DIR/fixture-target-cli.sh"

chmod +x "$REFERENCE" "$TARGET" "$PARITY_SCRIPT"

PASS_COUNT=0
FAIL_COUNT=0
LAST_OUTPUT=""

# Runs "$@", asserts its exit code equals expected_exit, prints its own
# PASS/FAIL status line directly (NOT via command substitution — a prior
# version of this function returned its captured output via stdout, which
# meant wrapping the whole call in out=$(...) silently swallowed the
# PASS/FAIL banner along with it). The subprocess's own output is left in
# $LAST_OUTPUT for the caller's assert_contains checks.
check() {
    local desc="$1" expected_exit="$2"
    shift 2
    local actual_exit=0
    LAST_OUTPUT=$("$@" 2>&1) || actual_exit=$?
    if [[ "$actual_exit" -eq "$expected_exit" ]]; then
        echo "PASS: $desc (exit $actual_exit as expected)"
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        echo "FAIL: $desc (expected exit $expected_exit, got $actual_exit)"
        echo "--- output ---"
        echo "$LAST_OUTPUT"
        echo "--------------"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
}

assert_contains() {
    local desc="$1" haystack="$2" needle="$3"
    if grep -qF "$needle" <<<"$haystack"; then
        echo "PASS: $desc (found '$needle')"
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        echo "FAIL: $desc (did not find '$needle')"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
}

EMPTY_ALLOWLIST=$(mktemp)
COVERING_ALLOWLIST=$(mktemp)
printf 'alpha two\t9999\tDEFERRED\ttest fixture entry\n' >"$COVERING_ALLOWLIST"
MALFORMED_ALLOWLIST=$(mktemp)
printf 'alpha two\t9999\n' >"$MALFORMED_ALLOWLIST"
trap 'rm -f "$EMPTY_ALLOWLIST" "$COVERING_ALLOWLIST" "$MALFORMED_ALLOWLIST"' EXIT

echo "=== 1. Matching tree: full variant must PASS with 0 missing, 0 superset ==="
FIXTURE_VARIANT=full CARDANO_CLI_BIN="$REFERENCE" DUGITE_CLI_BIN="$TARGET" \
    CLI_SURFACE_KNOWN_GAPS_FILE="$EMPTY_ALLOWLIST" \
    check "full-tree match" 0 "$PARITY_SCRIPT"
assert_contains "full-tree: reports 4/4 matched" "$LAST_OUTPUT" "MATCHED:  4 / 4"
assert_contains "full-tree: reports 0 missing" "$LAST_OUTPUT" "MISSING:  0 / 4"
assert_contains "full-tree: reports 0 superset" "$LAST_OUTPUT" "SUPERSET: 0 dugite-cli"
# MATCHED: 4/4 (asserted above) is itself the depth>1 proof: the fixture
# tree has only 3 top-level entries (alpha, beta, gamma) — reaching 4
# requires the walker to have descended into "alpha" and found its two
# children (one, two) rather than stopping at "alpha" as a leaf. A regressed
# walker that flattened to top-level only would report 3/4 matched, 1
# missing ("alpha one" or "alpha two" absent from BOTH sides equally isn't
# possible here since both fixtures define the same nested shape — a
# flattening regression would instead make "alpha" itself the leaf on both
# sides, which the normalized-count assertions below catch directly.
assert_contains "full-tree: raw/normalized counts show real recursion, not a flat 3-entry top level" \
    "$LAST_OUTPUT" "cardano-cli:  4 raw leaf paths -> 4 unique normalized commands"

echo
echo "=== 2. RED demonstration: missing variant must FAIL with 'alpha two' uncovered ==="
FIXTURE_VARIANT=missing CARDANO_CLI_BIN="$REFERENCE" DUGITE_CLI_BIN="$TARGET" \
    CLI_SURFACE_KNOWN_GAPS_FILE="$EMPTY_ALLOWLIST" \
    check "missing 'alpha two' -> real gap, uncovered, must FAIL" 1 "$PARITY_SCRIPT"
assert_contains "missing: reports the gap by name" "$LAST_OUTPUT" "alpha two"
assert_contains "missing: FAIL banner present" "$LAST_OUTPUT" "FAIL: 1 real gap(s)"

echo
echo "=== 3. Allowlist coverage: same missing tree, but 'alpha two' is allowlisted -> PASS ==="
FIXTURE_VARIANT=missing CARDANO_CLI_BIN="$REFERENCE" DUGITE_CLI_BIN="$TARGET" \
    CLI_SURFACE_KNOWN_GAPS_FILE="$COVERING_ALLOWLIST" \
    check "missing 'alpha two' but allowlisted (DEFERRED) -> PASS" 0 "$PARITY_SCRIPT"
assert_contains "allowlisted: shows its disposition, not uncovered" "$LAST_OUTPUT" "[DEFERRED]"
assert_contains "allowlisted: names the entry" "$LAST_OUTPUT" "alpha two"

echo
echo "=== 3b. Malformed disposition: same missing tree, allowlist entry has NO disposition/reason -> FAIL ==="
FIXTURE_VARIANT=missing CARDANO_CLI_BIN="$REFERENCE" DUGITE_CLI_BIN="$TARGET" \
    CLI_SURFACE_KNOWN_GAPS_FILE="$MALFORMED_ALLOWLIST" \
    check "allowlist entry missing disposition -> FAIL, not silently covered" 1 "$PARITY_SCRIPT"
assert_contains "malformed: reports malformed-entry banner" "$LAST_OUTPUT" "malformed allowlist entry"
assert_contains "malformed: names the entry" "$LAST_OUTPUT" "alpha two"

echo
echo "=== 4. Stale allowlist: full tree (nothing missing) but allowlist still claims 'alpha two' -> FAIL ==="
FIXTURE_VARIANT=full CARDANO_CLI_BIN="$REFERENCE" DUGITE_CLI_BIN="$TARGET" \
    CLI_SURFACE_KNOWN_GAPS_FILE="$COVERING_ALLOWLIST" \
    check "stale allowlist entry -> FAIL" 1 "$PARITY_SCRIPT"
assert_contains "stale: reports stale allowlist banner" "$LAST_OUTPUT" "stale allowlist entry"
assert_contains "stale: names the stale entry" "$LAST_OUTPUT" "alpha two"

echo
echo "=== 5. Superset: extra 'delta' command is informational only, still PASS ==="
FIXTURE_VARIANT=superset CARDANO_CLI_BIN="$REFERENCE" DUGITE_CLI_BIN="$TARGET" \
    CLI_SURFACE_KNOWN_GAPS_FILE="$EMPTY_ALLOWLIST" \
    check "superset 'delta' does not fail the check" 0 "$PARITY_SCRIPT"
assert_contains "superset: reports delta" "$LAST_OUTPUT" "delta"
assert_contains "superset: still 4/4 matched" "$LAST_OUTPUT" "MATCHED:  4 / 4"

echo
echo "=== 6. INCONCLUSIVE: a nonexistent binary must never report PASS ==="
CARDANO_CLI_BIN=/nonexistent/cardano-cli DUGITE_CLI_BIN="$TARGET" \
    CLI_SURFACE_KNOWN_GAPS_FILE="$EMPTY_ALLOWLIST" \
    check "missing binary -> INCONCLUSIVE (exit 2), never PASS" 2 "$PARITY_SCRIPT"
assert_contains "inconclusive: says so explicitly, not silently" "$LAST_OUTPUT" "INCONCLUSIVE"

echo
echo "=== Selftest summary: $PASS_COUNT passed, $FAIL_COUNT failed ==="
if [[ "$FAIL_COUNT" -ne 0 ]]; then
    exit 1
fi
exit 0
