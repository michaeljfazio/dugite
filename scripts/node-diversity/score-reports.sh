#!/usr/bin/env bash
# Score dugite's node-diversity reports with the site's OWN builder, as the site will score them.
#
#   score-reports.sh [reports-dir]       (default: target/node-diversity/reports)
#
# <reports-dir> holds what `just ndc-reports` writes: ledger-rules.json, cbor-decoding-conway.json,
# cbor-decoding-dijkstra.json, uplc-conformance.json, uplc-turbo.json (each with a .status.json).
#
# The builder (scripts/build.py of the pinned NDC checkout) runs with --preview --strict on a
# temporary COPY of the checkout that holds only a `dugite` node, so no other node's report is
# downloaded.  Nothing is installed: the builder is Python standard library only.  Prints one line
# per scenario and an overall score.  A score never fails this script; an unreadable report, a
# missing corpus, a builder error or a report written by a FAILED generator (status `error`) does.
# Each report's .status.json must also match the current corpora.toml and this checkout's commit;
# a report that does not is marked STALE (or UNBOUND when it has no status file), never passed off as current.
#
# Notes:
#  * The builder asks the GitHub API (unauthenticated: 60 requests an hour) which Amaru and CBOR
#    dataset commits are the latest, to mark a report "outdated".  Answers are cached in
#    target/node-diversity/score-cache; when the API cannot be reached the builder warns and the
#    score is unaffected.
#  * uplc-turbo scores each budget against the MAJORITY of nodes.  With one node in the copy there
#    is no majority, so its score here is indicative only.
#  * A line marked STUB is a placeholder report: it carries no result, whatever its score says.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
BUILD=${DUGITE_NDC_BUILD_DIR:-$ROOT/target/node-diversity}
CORPORA=${DUGITE_NDC_DIR:-$BUILD/corpora}
REPORTS=${1:-${DUGITE_NDC_REPORTS_DIR:-$BUILD/reports}}
PINS=${DUGITE_NDC_PINS:-$ROOT/tests/conformance/node-diversity/corpora.toml}

die() {
    echo "score-reports: $*" >&2
    exit 1
}

[[ -d "$CORPORA/ndc/scenarios" ]] || die "no NDC checkout under $CORPORA: run scripts/node-diversity/fetch-corpora.sh"
[[ -d "$REPORTS" ]] || die "no reports directory $REPORTS: run 'just ndc-reports'"
REPORTS=$(cd "$REPORTS" && pwd)
# The builder must be the pinned one.
eval "$(python3 -I "$HERE/pins.py" "$PINS")"
actual=$(git -C "$CORPORA/ndc" rev-parse HEAD)
[[ "$actual" == "$NDC_COMMIT" ]] || die "$CORPORA/ndc is at $actual, pin is $NDC_COMMIT"

mkdir -p "$BUILD/score-cache"
SITE=$(mktemp -d "$BUILD/score-site.XXXXXX")
trap 'rm -rf "$SITE"' EXIT

# A copy of the checkout without its git history, with the builder's download cache kept outside it.
(cd "$CORPORA/ndc" && tar --exclude=.git -c .) | tar -x -C "$SITE"
rm -rf "$SITE/_site" "$SITE/.cache"
ln -s "$BUILD/score-cache" "$SITE/.cache"

# Only dugite: every other node would be downloaded.
find "$SITE/nodes" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
mkdir "$SITE/nodes/dugite"
# The `reports` URLs are never fetched (a preview file wins); cbor-decoding needs an entry per era.
cat >"$SITE/nodes/dugite/metadata.json" <<'JSON'
{
  "id": "dugite",
  "name": "Dugite",
  "language": "Rust",
  "reports": {
    "cbor-decoding": {
      "conway": "https://invalid.example/dugite-cbor-conway.json",
      "dijkstra": "https://invalid.example/dugite-cbor-dijkstra.json"
    }
  }
}
JSON
printf 'Dugite\n' >"$SITE/nodes/dugite/README.md"
printf '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>\n' >"$SITE/nodes/dugite/logo.svg"

place() { # REPORT SCENARIO PREVIEW_NAME
    local file="$REPORTS/$1"
    [[ -s "$file" ]] || die "missing report $file: run 'just ndc-reports'"
    mkdir -p "$SITE/scenarios/$2/preview"
    cp "$file" "$SITE/scenarios/$2/preview/$3"
}
place ledger-rules.json ledger-rules dugite.json
place cbor-decoding-conway.json cbor-decoding dugite-conway.json
place cbor-decoding-dijkstra.json cbor-decoding dugite-dijkstra.json
place uplc-conformance.json uplc-conformance dugite.json
place uplc-turbo.json uplc-turbo dugite.json

(cd "$SITE" && python3 scripts/build.py --preview --strict >"$SITE/build.log" 2>&1) || {
    cat "$SITE/build.log" >&2
    die "the NDC site builder failed"
}
grep -E "^(warn|failed)" "$SITE/build.log" >&2 || true

PINS_SHA=$(python3 -I -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$PINS")
HEAD_COMMIT=$(git -C "$ROOT" rev-parse HEAD)
python3 -I - "$SITE/_site" "$REPORTS" "$PINS_SHA" "$HEAD_COMMIT" <<'PY'
import json
import pathlib
import sys

site, reports = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
pins_sha, head = sys.argv[3], sys.argv[4]
errors = []


def status(name):
    """What the report's sidecar says, bound to the CURRENT pins and commit; "" only for a current ok."""
    try:
        side = json.loads((reports / f"{name}.status.json").read_text())
    except (OSError, ValueError):
        return "UNBOUND: no readable status file, so nothing ties this report to its inputs"
    notes = []
    if side.get("pins_sha256") != pins_sha:
        notes.append("STALE: made under other pins than the current corpora.toml")
    if side.get("implementation_commit") != head:
        notes.append(f'STALE: made at {str(side.get("implementation_commit"))[:10]}, this checkout is {head[:10]}')
    if side["status"] != "ok":
        notes.append(f'{side["status"].upper()}: {side["note"]}')
    if side["status"] == "error":
        errors.append(name)
    return "; ".join(notes)


files = {
    "ledger-rules": ["ledger-rules.json"],
    "cbor-decoding": ["cbor-decoding-conway.json", "cbor-decoding-dijkstra.json"],
    "uplc-conformance": ["uplc-conformance.json"],
    "uplc-turbo": ["uplc-turbo.json"],
}
catalog = json.loads((site / "catalog.json").read_text())
node = next(n for n in catalog["nodes"] if n["id"] == "dugite")
for score in node["scenario_scores"]:
    scenario = score["id"]
    if not score["configured"]:
        print(f"{scenario:<18} no report")
        continue
    flags = "  OUTDATED" if score["outdated"] else ""
    # The builder scores cbor-decoding on the conway report; the era report is read below.
    notes = "; ".join(dict.fromkeys(filter(None, (status(name) for name in files[scenario]))))
    line = f'{scenario:<18} {score["passed"]:>5} / {score["total"]:<5} {score["ratio"] * 100:6.2f}%{flags}'
    print(f"{line}  [{notes}]" if notes else line)
    if scenario == "cbor-decoding":
        data = json.loads((site / "scenarios" / scenario / "data.json").read_text())
        entry = next(n for n in data["nodes"] if n["id"] == "dugite")
        for era, result in sorted(entry.get("eras", {}).items()):
            totals = (result["report"] or {}).get("totals", {})
            expected = sum(v for k, v in totals.items() if k.endswith("_expected"))
            actual = sum(v for k, v in totals.items() if k.endswith("_actual"))
            print(f'  {era:<16} {actual:>5} / {expected:<5} {(actual / expected * 100 if expected else 0):6.2f}%')
print(f'{"overall":<18} {node["conformance"]["ratio"] * 100:.2f}%  (four scenarios weighted equally; a scenario with no result scores 0)')
# A score never fails this script.  A report left by a generator that FAILED is a harness failure.
if errors:
    print(f"score-reports: {len(errors)} report(s) come from a failed generator: {', '.join(sorted(errors))}", file=sys.stderr)
    sys.exit(1)
PY
