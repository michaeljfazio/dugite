# lib/expect-log-errors.sh — shared expected-error + allowlist log oracle (#1041).
#
# Modelled on upstream cardano-node-tests logfiles.py (`expect_errors` +
# `ERRORS_IGNORED`): a round that deliberately provokes a fault MUST see the
# fault's diagnostic in the log (a fault that produced no diagnostic is itself
# a failure), and MUST NOT see any error-class line it did not declare.
#
# Sourced by the fault-injecting rounds (kes-round.sh, rollback-round.sh).
# Allowlists are per-round files checked in next to the round script (the
# upstream `.errors_to_ignore` shape): one extended-regex per line, blank
# lines and `#` comments ignored. A new expected error is therefore a
# reviewed diff, not an inline regex edit.
#
# What counts as an "error-class line" comes from the SHARED
# LOG_ERROR_PATTERN in .claude/skills/devnet-validate/scripts/lib/
# log-level-counts.sh — level-token matching, never the substring "error"
# (the #916 rule). Sourcing the same file analyze-evidence.sh and
# generate-release-report.sh use means the three can never disagree on the
# classification; test-expect-log-errors.sh pins that agreement.
#
# Windows are LINE MARKS, not timestamps: dugite and cardano-node stamp log
# lines in different formats, so "since" is expressed as "lines appended
# after the mark was taken" (upstream seeks by file position for the same
# reason).
#
# API:
#   log_mark <log>
#       Echo the current line count of <log> (0 if absent). Take the mark
#       BEFORE injecting the fault; pass it to both assertions.
#   expect_log_errors <log> <mark> <ere>...
#       Every pattern MUST appear in <log> after <mark>. Returns 1 (and names
#       each missing pattern on stderr) otherwise.
#   assert_no_other_errors <log> <mark> <allowlist-file>
#       NO error-class line after <mark> survives the allowlist. Survivors are
#       printed to stderr and the function returns 1. The allowlist file must
#       exist — a typo'd path failing open as "no allowlist" would be the
#       silently-weakened-check class (#953), and failing closed would blame
#       the wrong thing; either way, loudly.

_ELE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../../.claude/skills/devnet-validate/scripts/lib/log-level-counts.sh
. "$_ELE_LIB_DIR/../../../.claude/skills/devnet-validate/scripts/lib/log-level-counts.sh"

log_mark() {
    local log="$1"
    if [ -f "$log" ]; then
        wc -l < "$log" | tr -d '[:space:]'
    else
        echo 0
    fi
}

expect_log_errors() {
    local log="$1" mark="$2"
    shift 2
    local fail=0 pat
    [ -f "$log" ] || { echo "expect_log_errors: log not found: $log" >&2; return 1; }
    for pat in "$@"; do
        if ! tail -n +"$((mark + 1))" "$log" | grep -qE "$pat"; then
            echo "expect_log_errors: expected pattern NOT FOUND after line $mark of $log: $pat" >&2
            fail=1
        fi
    done
    return "$fail"
}

assert_no_other_errors() {
    local log="$1" mark="$2" allowlist="$3"
    [ -f "$log" ] || { echo "assert_no_other_errors: log not found: $log" >&2; return 1; }
    [ -f "$allowlist" ] || { echo "assert_no_other_errors: allowlist not found: $allowlist" >&2; return 1; }
    local survivors pat
    survivors=$(tail -n +"$((mark + 1))" "$log" | grep -E "$LOG_ERROR_PATTERN" || true)
    [ -z "$survivors" ] && return 0
    while IFS= read -r pat || [ -n "$pat" ]; do
        case "$pat" in ''|\#*) continue ;; esac
        survivors=$(printf '%s\n' "$survivors" | grep -vE "$pat" || true)
        [ -z "$survivors" ] && return 0
    done < "$allowlist"
    echo "assert_no_other_errors: unexpected error-class lines in $log after line $mark:" >&2
    printf '%s\n' "$survivors" >&2
    return 1
}
