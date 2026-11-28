#!/usr/bin/env bash
# Every entry of the ledger-rules expected-results contract names a GitHub issue that holds the
# evidence for that divergence (#1125).  An entry whose issue is closed is not "tracked": either the
# divergence is fixed (the gate then fails on the entry, which must be removed) or the issue was
# closed without the evidence being resolved.  This checks the second case, which no test can see.
#
#   check-expected-issues.sh [expected-results.toml]
#
# Needs `gh` (authenticated; GH_TOKEN in CI) and python3 >= 3.11.  Exits non-zero when any issue is
# not OPEN, is not in this repository, or cannot be read.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
FILE=${1:-$ROOT/tests/conformance/node-diversity/ledger-rules/expected-results.toml}
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

status=0
count=0
while IFS=$'\t' read -r n class; do
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
