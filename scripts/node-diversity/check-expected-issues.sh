#!/usr/bin/env bash
# Every entry of the ledger-rules expected-results contract names a GitHub issue that holds the
# evidence for that divergence (#1125).  An entry whose issue is closed is not "tracked": either the
# divergence is fixed (the gate then fails on the entry, which must be removed) or the issue was
# closed without the evidence being resolved.  This checks the second case, which no test can see.
#
#   check-expected-issues.sh [expected-results.toml [ledger_rules_replay.tsv]]
#
# It also checks the ImpSpec apply-and-compare contract
# (tests/conformance/expected/ledger_rules_replay.tsv): every `#N` in its issue column must have a
# `# issue #N: <title>` header line, and that issue must be OPEN and titled exactly so.
#
# Needs `gh` (authenticated; GH_TOKEN in CI) and python3 >= 3.11.  Exits non-zero when any issue is
# not OPEN, is not in this repository, or cannot be read.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
FILE=${1:-$ROOT/tests/conformance/node-diversity/ledger-rules/expected-results.toml}
REPLAY=${2:-$ROOT/tests/conformance/expected/ledger_rules_replay.tsv}
REPO=${DUGITE_NDC_ISSUE_REPO:-michaeljfazio/dugite}

command -v gh >/dev/null 2>&1 || {
    echo "check-expected-issues: gh is not installed" >&2
    exit 2
}

issues=$(python3 -I - "$FILE" <<'PY'
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    entries = tomllib.load(f).get("entry", [])
bad = [e for e in entries if not isinstance(e.get("issue"), int) or e["issue"] <= 0]
if bad:
    sys.exit("entries without an issue number: " + ", ".join(e.get("id", "?") for e in bad))
classes = {}
for e in entries:
    classes.setdefault(e["issue"], set()).add(e.get("class", ""))
for n in sorted(classes):
    if len(classes[n]) != 1:
        sys.exit(f"issue {n} is used with different classes: {sorted(classes[n])}")
    print(n, next(iter(classes[n])), sep="\t")
PY
) || exit 1

# The ImpSpec replay contract: `<id>\t<observed>\t<#N[,#M...]|->`, titles in `# issue #N: title`.
replay_issues=$(python3 -I - "$REPLAY" <<'PY'
import re, sys
titles, used = {}, set()
with open(sys.argv[1], encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        m = re.match(r"# issue #(\d+): (.*)$", line)
        if m:
            titles[int(m.group(1))] = m.group(2)
            continue
        if not line or line.startswith("#"):
            continue
        cols = line.split("\t")
        if len(cols) != 3:
            sys.exit(f"malformed entry: {line[:80]}")
        for one in cols[2].split(","):
            if one != "-":
                used.add(int(one.lstrip("#")))
missing = sorted(n for n in used if n not in titles)
if missing:
    sys.exit("issues used without a `# issue #N: title` header: " + ", ".join(map(str, missing)))
for n in sorted(used):
    print(n, titles[n], sep="\t")
PY
) || exit 1
issues=$(printf '%s\n%s\n' "$issues" "$replay_issues")

status=0
count=0
while IFS=$'\t' read -r n class; do
    [[ -z "$n" ]] && continue
    count=$((count + 1))
    if ! info=$(gh issue view "$n" --repo "$REPO" --json state,title --jq '[.state, .title] | @tsv' 2>&1); then
        echo "check-expected-issues: cannot read $REPO#$n: $info" >&2
        status=1
        continue
    fi
    state=${info%%$'\t'*}
    title=${info#*$'\t'}
    if [[ "$state" != "OPEN" ]]; then
        echo "check-expected-issues: $REPO#$n is $state; an expected-results entry needs an OPEN issue" >&2
        status=1
    fi
    if [[ "$title" != "$class" ]]; then
        echo "check-expected-issues: $REPO#$n is titled '$title', its entries say class '$class'" >&2
        status=1
    fi
done <<<"$issues"
[[ $status -eq 0 ]] && echo "check-expected-issues: $count issues, all open and titled as their entries say"
exit $status
