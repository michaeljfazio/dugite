#!/usr/bin/env bash
# cli-surface-parity.sh — enumerate cardano-cli's subcommand surface against
# dugite-cli's, in both directions. (#1006, follow-up to #998.)
#
# #998 asked dugite-cli to implement three CIP-0094 poll commands that
# turned out not to exist in current cardano-cli at all (removed 2025-05,
# PR #1178) — a false positive. The finding that mattered was structural: no
# suite in the release gate enumerates cardano-cli's subcommand surface
# against dugite-cli's, so a REAL missing command would have been just as
# invisible as that fictional one was. `tx-zoo/09-cli-parity` runs real
# cardano-cli against both N2C sockets, but it measures dugite-node's LSQ
# replies and never invokes dugite-cli at all (see "Reading the cli-parity
# suite" in CLAUDE.md). This script is that missing check.
#
# ─── What it does ────────────────────────────────────────────────────────
#
# Recursively walks `<binary> [path...] --help` on both cardano-cli and
# dugite-cli, starting from the bare top level, discovering every reachable
# leaf command purely from what each level's own --help output reports —
# NOT from a hardcoded era/command list. (A hardcoded list is exactly the
# shape that let #969's category 17 and the #971 fuzz-matrix drift sit
# invisible for months; see CLAUDE.md.) Leaf paths are then normalized by
# stripping a leading run of era/namespace tokens (conway, babbage, ...,
# legacy, compatible) so that e.g. cardano-cli's
# `compatible babbage governance create-poll` and a hypothetical dugite-cli
# `babbage governance create-poll` compare as the same underlying command
# regardless of which namespace either tool reaches it through.
#
# The normalized cardano-cli set and normalized dugite-cli set are then
# diffed in both directions:
#   - cardano-cli has it, dugite-cli doesn't  -> MISSING (real gap)
#   - dugite-cli has it, cardano-cli doesn't  -> SUPERSET (informational,
#     unless not covered by the documented era-prefix-leniency pattern)
#
# MISSING entries fail the script unless present in
# scripts/validation/cli-surface-known-gaps.txt (one `<normalized path>
# <issue-number>` per line — mirrors the ledger-rules SKIP_LIST discipline:
# CLAUDE.md "Phase 4 acceptance: SKIP_LIST is empty or every entry has a
# tracking issue"). A STALE allowlist entry (no longer actually missing)
# also fails the script, so the allowlist can't silently rot into a wider
# exemption than it started as.
#
# ─── Exit codes ──────────────────────────────────────────────────────────
#   0 = PASS  (no un-allowlisted MISSING entries, no stale allowlist entries)
#   1 = FAIL  (a real gap, or a stale allowlist entry)
#   2 = INCONCLUSIVE (cardano-cli or dugite-cli could not be run at all —
#       NEVER reported as PASS; see CLAUDE.md #923, `adv_send_expect_close`
#       silently returning PASS when socat was absent)
#
# ─── Usage ───────────────────────────────────────────────────────────────
#   scripts/validation/cli-surface-parity.sh
#   CARDANO_CLI_BIN=/path/to/cardano-cli DUGITE_CLI_BIN=/path/to/dugite-cli \
#     scripts/validation/cli-surface-parity.sh
#
# Override both binaries with CARDANO_CLI_BIN / DUGITE_CLI_BIN — this is how
# the self-test (cli-surface-parity-selftest.sh) drives the walker against
# fixture stub scripts instead of the real binaries, and how CI points at a
# freshly-downloaded cardano-cli release tarball.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KNOWN_GAPS_FILE="${CLI_SURFACE_KNOWN_GAPS_FILE:-$SCRIPT_DIR/cli-surface-known-gaps.txt}"

CARDANO_CLI_BIN="${CARDANO_CLI_BIN:-cardano-cli}"
DUGITE_CLI_BIN="${DUGITE_CLI_BIN:-$REPO_ROOT/target/debug/dugite-cli}"

MAX_DEPTH="${CLI_SURFACE_MAX_DEPTH:-10}"

# Era/namespace tokens stripped from the FRONT of a path (repeatedly) before
# comparison. "byron" is deliberately excluded: it's a standalone command
# group on both tools, not an era selector for an otherwise-shared surface.
ERA_TOKENS_RE='^(shelley|allegra|mary|alonzo|babbage|conway|dijkstra|latest|legacy|compatible)$'

warn() { echo "WARN: $*" >&2; }
info() { echo "$*" >&2; }

# ─── Help-text parsing ─────────────────────────────────────────────────────
#
# Handles both header spellings:
#   "Available commands:" (Haskell optparse-applicative, cardano-cli)
#   "Commands:"            (Rust clap-derive, dugite-cli)
#
# A genuine new entry is indented by EXACTLY 2 spaces before its first
# non-space character. A wrapped-description continuation line (Haskell only
# — clap-derive never wraps) is indented to the description column, always
# 3+ spaces. This holds regardless of whether the command name itself is
# short (description starts inline) or long enough that the description is
# pushed entirely to the next line — in both cases the *continuation* line
# is indented past column 2, and the *new-entry* line never is. Verified
# empirically against both frameworks' real output before relying on it;
# see the selftest fixtures for the exact cases that motivated this rule
# (`create-protocol-parameters-update`, whose own description doesn't fit
# on the name line at all).
#
# Deliberately does NOT end the block on a bare blank line — only on another
# recognized section header (Options:/Available options:) or a fresh Usage:
# line. Two frameworks, two different reasons this matters:
#
#   - cardano-cli (Haskell, optparse-applicative): some entries
#     (`transaction build`/`build-raw`/`build-estimate`) have a
#     multi-PARAGRAPH description — a blank line, then an ANSI-colored
#     "Please note the order..." warning paragraph. A first cut of this
#     parser ended the block at that internal blank line, silently
#     truncating the walk to the first entry it saw and losing
#     sign/witness/assemble/submit/policyid/... entirely.
#   - dugite-cli (Rust, clap-derive): "Commands:" is immediately followed by
#     a blank line and then a REAL "Options:" section. A second cut that
#     simply stopped treating blank lines as terminators (to fix the first
#     bug) started reading INTO "Options:" and captured "-h," (from
#     "  -h, --help  Print help") as a bogus child command — which then
#     recursed on itself combinatorially (clap's error-recovery help output
#     for a garbled path re-lists the same commands, so one bad token
#     multiplied into an exponential blowup; see the #1006 report for the
#     'max depth exceeded' flood this produced before the fix).
#
# Both are satisfied by terminating ONLY on an explicit header, never on a
# bare blank line — a blank line inside the block is always either a
# paragraph break (Haskell) or immediately followed by a real header we
# already catch (clap), never a block end in its own right.
extract_commands() {
    # Strip ANSI SGR escape sequences first — defensive, since the
    # multi-paragraph entries above also carry raw \x1b[...m color codes in
    # their continuation text. Not currently load-bearing for correctness
    # (continuation lines are discarded by content regardless of what they
    # contain) but a future entry could plausibly put color codes on the
    # name line itself, so strip unconditionally rather than relying on that
    # not happening.
    sed -E $'s/\x1b\\[[0-9;]*[a-zA-Z]//g' | awk '
        BEGIN { in_block = 0 }
        /^(Available )?[Cc]ommands:[[:space:]]*$/ { in_block = 1; next }
        in_block == 0 { next }
        /^(Available )?[Oo]ptions:[[:space:]]*$/ { in_block = 0; next }
        /^Usage:/ { in_block = 0; next }
        /^  [^ ]/ {
            line = $0
            sub(/^  /, "", line)
            n = split(line, parts, /[ \t]/)
            print parts[1]
            next
        }
        { next }
    '
}

# ─── Recursive walker ──────────────────────────────────────────────────────
#
# Populates the global file $1 (one leaf path per line, space-joined tokens)
# by recursing purely off each level's own --help output. No hardcoded
# command or era list — see header comment.
declare -A VISITED
BINARY_OK=1

walk() {
    local binary="$1" out_file="$2"
    shift 2
    local -a path=("$@")
    local depth=${#path[@]}

    if ((depth > MAX_DEPTH)); then
        warn "max depth $MAX_DEPTH exceeded at '${path[*]}' — treating as leaf, tree may be truncated"
        echo "${path[*]}" >>"$out_file"
        return
    fi

    # bash associative arrays reject an empty-string key outright ("bad
    # array subscript"), which the root-level call (path=()) would produce
    # via "${path[*]}" — use a non-empty sentinel for that case.
    local key="<root>"
    ((${#path[@]} > 0)) && key="${path[*]}"
    if [[ -n "${VISITED[$key]:-}" ]]; then
        return
    fi
    VISITED[$key]=1

    local output
    output=$("$binary" "${path[@]}" --help 2>&1)
    local status=$?
    # Strip ANSI SGR codes up front so every downstream check (header
    # detection, Usage: detection, extract_commands) sees plain text —
    # cardano-cli color-codes some warning paragraphs (see extract_commands).
    output=$(sed -E $'s/\x1b\\[[0-9;]*[a-zA-Z]//g' <<<"$output")

    if [[ $status -ne 0 && ${#path[@]} -eq 0 ]]; then
        # Bare `<binary> --help` failing means the binary itself is broken —
        # this is the INCONCLUSIVE trigger, not a parse failure.
        BINARY_OK=0
        return
    fi

    if ! grep -qE '^(Available )?[Cc]ommands:[[:space:]]*$' <<<"$output"; then
        if grep -q '^Usage:' <<<"$output"; then
            echo "${path[*]}" >>"$out_file"
        else
            warn "unexpected --help output for '$binary ${path[*]}' (exit $status), skipping: $(head -1 <<<"$output")"
        fi
        return
    fi

    local children
    children=$(extract_commands <<<"$output")
    while IFS= read -r child; do
        [[ -z "$child" ]] && continue
        [[ "$child" == "help" ]] && continue # framework meta-command, both sides have one, not comparable
        walk "$binary" "$out_file" "${path[@]}" "$child"
    done <<<"$children"
}

strip_era_prefix() {
    local -a toks
    read -r -a toks <<<"$1"
    while ((${#toks[@]} > 0)); do
        if [[ "${toks[0]}" =~ $ERA_TOKENS_RE ]]; then
            toks=("${toks[@]:1}")
        else
            break
        fi
    done
    echo "${toks[*]}"
}

# ─── Preflight: both binaries must at least run ───────────────────────────

if ! command -v "$CARDANO_CLI_BIN" >/dev/null 2>&1 && [[ ! -x "$CARDANO_CLI_BIN" ]]; then
    echo "INCONCLUSIVE: cardano-cli not found (CARDANO_CLI_BIN=$CARDANO_CLI_BIN)." >&2
    echo "Set CARDANO_CLI_BIN to a working cardano-cli binary. Never reported as PASS." >&2
    exit 2
fi
if [[ ! -x "$DUGITE_CLI_BIN" ]] && ! command -v "$DUGITE_CLI_BIN" >/dev/null 2>&1; then
    echo "INCONCLUSIVE: dugite-cli not found (DUGITE_CLI_BIN=$DUGITE_CLI_BIN)." >&2
    echo "Build it first: cargo build -p dugite-cli. Never reported as PASS." >&2
    exit 2
fi

CC_RAW_FILE=$(mktemp)
DC_RAW_FILE=$(mktemp)
trap 'rm -f "$CC_RAW_FILE" "$DC_RAW_FILE"' EXIT

info "Walking cardano-cli ($CARDANO_CLI_BIN) ..."
VISITED=()
BINARY_OK=1
walk "$CARDANO_CLI_BIN" "$CC_RAW_FILE"
if [[ "$BINARY_OK" -ne 1 ]]; then
    echo "INCONCLUSIVE: '$CARDANO_CLI_BIN --help' failed to run. Never reported as PASS." >&2
    exit 2
fi

info "Walking dugite-cli ($DUGITE_CLI_BIN) ..."
VISITED=()
BINARY_OK=1
walk "$DUGITE_CLI_BIN" "$DC_RAW_FILE"
if [[ "$BINARY_OK" -ne 1 ]]; then
    echo "INCONCLUSIVE: '$DUGITE_CLI_BIN --help' failed to run. Never reported as PASS." >&2
    exit 2
fi

CC_RAW_COUNT=$(wc -l <"$CC_RAW_FILE" | tr -d ' ')
DC_RAW_COUNT=$(wc -l <"$DC_RAW_FILE" | tr -d ' ')

if [[ "$CC_RAW_COUNT" -eq 0 ]]; then
    echo "INCONCLUSIVE: cardano-cli walk discovered 0 leaf commands — the walker is almost" >&2
    echo "certainly broken (parser regression), not that cardano-cli truly has no commands." >&2
    exit 2
fi
if [[ "$DC_RAW_COUNT" -eq 0 ]]; then
    echo "INCONCLUSIVE: dugite-cli walk discovered 0 leaf commands — likely a parser regression." >&2
    exit 2
fi

# ─── Normalize + diff ──────────────────────────────────────────────────────

CC_NORM_FILE=$(mktemp)
DC_NORM_FILE=$(mktemp)
trap 'rm -f "$CC_RAW_FILE" "$DC_RAW_FILE" "$CC_NORM_FILE" "$DC_NORM_FILE"' EXIT

while IFS= read -r p; do
    strip_era_prefix "$p"
done <"$CC_RAW_FILE" | sort -u >"$CC_NORM_FILE"

while IFS= read -r p; do
    strip_era_prefix "$p"
done <"$DC_RAW_FILE" | sort -u >"$DC_NORM_FILE"

CC_NORM_COUNT=$(wc -l <"$CC_NORM_FILE" | tr -d ' ')
DC_NORM_COUNT=$(wc -l <"$DC_NORM_FILE" | tr -d ' ')

MISSING_FILE=$(mktemp)   # cardano-cli has, dugite-cli doesn't
SUPERSET_FILE=$(mktemp)  # dugite-cli has, cardano-cli doesn't
trap 'rm -f "$CC_RAW_FILE" "$DC_RAW_FILE" "$CC_NORM_FILE" "$DC_NORM_FILE" "$MISSING_FILE" "$SUPERSET_FILE"' EXIT

comm -23 "$CC_NORM_FILE" "$DC_NORM_FILE" >"$MISSING_FILE"
comm -13 "$CC_NORM_FILE" "$DC_NORM_FILE" >"$SUPERSET_FILE"

MISSING_COUNT=$(wc -l <"$MISSING_FILE" | tr -d ' ')
SUPERSET_COUNT=$(wc -l <"$SUPERSET_FILE" | tr -d ' ')
MATCHED_COUNT=$((CC_NORM_COUNT - MISSING_COUNT))

# ─── Allowlist ──────────────────────────────────────────────────────────
#
# Format: "<normalized command path><TAB><issue-number>", blank lines and
# lines starting with # ignored. An allowlisted entry that is NOT actually
# missing is itself a failure (stale allowlist — same discipline as a stale
# KNOWN_DIVERGENCES entry, ref commit 6d5605afd5 in this repo's history).

declare -A ALLOWLISTED
if [[ -f "$KNOWN_GAPS_FILE" ]]; then
    while IFS=$'\t' read -r gap_path _issue; do
        [[ -z "$gap_path" || "$gap_path" == \#* ]] && continue
        ALLOWLISTED["$gap_path"]=1
    done <"$KNOWN_GAPS_FILE"
fi

UNCOVERED_MISSING_FILE=$(mktemp)
trap 'rm -f "$CC_RAW_FILE" "$DC_RAW_FILE" "$CC_NORM_FILE" "$DC_NORM_FILE" "$MISSING_FILE" "$SUPERSET_FILE" "$UNCOVERED_MISSING_FILE"' EXIT

while IFS= read -r m; do
    [[ -z "$m" ]] && continue
    if [[ -z "${ALLOWLISTED[$m]:-}" ]]; then
        echo "$m" >>"$UNCOVERED_MISSING_FILE"
    fi
done <"$MISSING_FILE"
UNCOVERED_MISSING_COUNT=$(wc -l <"$UNCOVERED_MISSING_FILE" | tr -d ' ')

STALE_ALLOWLIST=()
for gap_path in "${!ALLOWLISTED[@]}"; do
    if ! grep -qxF "$gap_path" "$MISSING_FILE"; then
        STALE_ALLOWLIST+=("$gap_path")
    fi
done

# ─── Report ────────────────────────────────────────────────────────────

echo "=== CLI surface parity: cardano-cli ($CARDANO_CLI_BIN) vs dugite-cli ($DUGITE_CLI_BIN) ==="
echo "cardano-cli:  $CC_RAW_COUNT raw leaf paths -> $CC_NORM_COUNT unique normalized commands"
echo "dugite-cli:   $DC_RAW_COUNT raw leaf paths -> $DC_NORM_COUNT unique normalized commands"
echo
echo "COMPARED: $CC_NORM_COUNT cardano-cli commands checked against dugite-cli's surface."
echo "  MATCHED:  $MATCHED_COUNT / $CC_NORM_COUNT"
echo "  MISSING:  $MISSING_COUNT / $CC_NORM_COUNT ($UNCOVERED_MISSING_COUNT not allowlisted)"
echo "  SUPERSET: $SUPERSET_COUNT dugite-cli commands with no cardano-cli counterpart"
echo

if [[ "$MISSING_COUNT" -gt 0 ]]; then
    echo "--- MISSING (cardano-cli has, dugite-cli doesn't) ---"
    while IFS= read -r m; do
        [[ -z "$m" ]] && continue
        if [[ -n "${ALLOWLISTED[$m]:-}" ]]; then
            echo "  [allowlisted] $m"
        else
            echo "  [UNCOVERED]   $m"
        fi
    done <"$MISSING_FILE"
    echo
fi

if [[ "$SUPERSET_COUNT" -gt 0 ]]; then
    echo "--- SUPERSET (dugite-cli has, cardano-cli doesn't; informational only) ---"
    cat "$SUPERSET_FILE" | sed 's/^/  /'
    echo
fi

FAIL=0

if [[ "$UNCOVERED_MISSING_COUNT" -gt 0 ]]; then
    echo "FAIL: $UNCOVERED_MISSING_COUNT real gap(s) not covered by $KNOWN_GAPS_FILE" >&2
    FAIL=1
fi

if [[ "${#STALE_ALLOWLIST[@]}" -gt 0 ]]; then
    echo "FAIL: ${#STALE_ALLOWLIST[@]} stale allowlist entry/entries in $KNOWN_GAPS_FILE" \
        "(no longer missing — dugite-cli now has them; remove the entry):" >&2
    for s in "${STALE_ALLOWLIST[@]}"; do
        echo "  - $s" >&2
    done
    FAIL=1
fi

if [[ "$FAIL" -ne 0 ]]; then
    exit 1
fi

echo "PASS: $MATCHED_COUNT/$CC_NORM_COUNT cardano-cli commands present in dugite-cli" \
    "(0 uncovered gaps, 0 stale allowlist entries)."
exit 0
