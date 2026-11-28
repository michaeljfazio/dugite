#!/usr/bin/env python3
"""Checks of the NDC ledger-rules fixtures against Amaru, run by fetch-corpora.sh.

    ledger_rules_check.py unresolved <corpora.toml> <ndc fixtures dir>
        Before the missing parameter files are added: the fixtures' `$ref`s that NDC does not
        resolve must be EXACTLY the files pinned in [[ledger_rules.missing_protocol_parameters]],
        each with its pinned fixture count.  A file NDC now ships fails the check (remove the
        workaround); so does a new unresolved reference.

    ledger_rules_check.py compare <corpora.toml> <ndc fixtures dir> <amaru checkout>
        After: every `$ref` resolves, and the NDC fixtures equal Amaru's at the pinned commit,
        apart from the entries of [ledger_rules.known_divergences] (exactly: an unlisted
        difference fails, and so does a listed entry that no longer differs).

Both exit non-zero with every problem listed.  Input is untrusted data: it is only parsed as JSON
and compared, never executed.
"""

import json
import sys
import tomllib
from pathlib import Path


def load_pins(path):
    with open(path, "rb") as handle:
        return tomllib.load(handle)


def collect_refs(node, found):
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "$ref" and isinstance(value, str):
                found.append(value)
            else:
                collect_refs(value, found)
    elif isinstance(node, list):
        for item in node:
            collect_refs(item, found)


def fixture_refs(fixtures):
    """{fixture file name: [$ref, ...]} for every scenario fixture."""
    refs = {}
    for path in sorted((fixtures / "scenarios").glob("*.json")):
        found = []
        collect_refs(json.loads(path.read_text()), found)
        refs[path.name] = found
    return refs


def ref_target(fixtures, ref):
    """A fixture `$ref` is `common/<dir>/<file>` (Amaru's layout); NDC drops the `common/`."""
    if not ref.startswith("common/") or ".." in ref.split("/"):
        raise ValueError(f"unexpected $ref {ref!r}")
    return fixtures / ref[len("common/"):]


def unresolved(pins_path, fixtures):
    pins = load_pins(pins_path)
    pinned = {entry["file"]: entry["fixtures"] for entry in pins["ledger_rules"].get("missing_protocol_parameters", [])}
    problems = []
    absent = {}  # ref -> number of fixtures
    for name, refs in fixture_refs(fixtures).items():
        for ref in set(refs):
            if not ref_target(fixtures, ref).is_file():
                absent[ref] = absent.get(ref, 0) + 1
    absent_files = {ref.rsplit("/", 1)[1]: count for ref, count in absent.items()}
    for ref in absent:
        if not ref.startswith("common/protocol-parameters/"):
            problems.append(f"fixtures reference {ref}, which NDC does not ship, and it is not a protocol-parameter file")
    for file, count in absent_files.items():
        if file not in pinned:
            problems.append(f"{file} is referenced by {count} fixtures but NDC does not ship it, and it is not pinned")
        elif pinned[file] != count:
            problems.append(f"{file}: {count} fixtures reference it, the pin says {pinned[file]}")
    for file in pinned:
        if file not in absent_files:
            problems.append(f"{file} is now in node-diversity-conformance (or no fixture needs it): remove it from corpora.toml")
    report(problems)
    total = sum(absent_files.values())
    print(f"ledger-rules: NDC lacks {len(absent_files)} protocol-parameter files, referenced by {total} fixtures")


def normalise(fixture):
    """Undo what NDC is known to rewrite mechanically, so only real edits remain."""
    def drop_nulls(node):
        if isinstance(node, dict):
            return {k: drop_nulls(v) for k, v in node.items() if v is not None}
        if isinstance(node, list):
            return [drop_nulls(v) for v in node]
        return node

    fixture = drop_nulls(fixture)
    fixture.pop("category", None)
    state = fixture.get("initial_state", {})
    for key in list(state):
        if state[key] in ([], {}) or state[key] == {"consecutive_dormant_epochs": 0}:
            del state[key]
    fixture["initial_state"] = state
    return fixture


def compare(pins_path, fixtures, amaru_root):
    pins = load_pins(pins_path)
    scenarios_dir = amaru_root / pins["amaru"]["scenarios_dir"]
    common_dir = amaru_root / pins["amaru"]["common_dir"]
    known = {key: sorted(value) for key, value in pins["ledger_rules"].get("known_divergences", {}).items()}
    problems = []

    ndc_files = {p.name for p in (fixtures / "scenarios").glob("*.json")}
    amaru_files = {p.name for p in scenarios_dir.glob("*.json")}
    for name in sorted(ndc_files - amaru_files):
        problems.append(f"{name}: in NDC, not in Amaru at the pinned commit")
    for name in sorted(amaru_files - ndc_files):
        problems.append(f"{name}: in Amaru at the pinned commit, not in NDC")

    seen = {}
    for name in sorted(ndc_files & amaru_files):
        ours = normalise(json.loads((fixtures / "scenarios" / name).read_text()))
        theirs = normalise(json.loads((scenarios_dir / name).read_text()))
        differing = sorted(key for key in set(ours) | set(theirs) if ours.get(key) != theirs.get(key))
        if differing:
            seen[name[:5]] = differing
    for fixture_id, keys in sorted(seen.items()):
        if fixture_id not in known:
            problems.append(f"{fixture_id}: NDC differs from Amaru in {keys} and corpora.toml does not list it")
        elif known[fixture_id] != keys:
            problems.append(f"{fixture_id}: NDC differs from Amaru in {keys}, corpora.toml lists {known[fixture_id]}")
    for fixture_id in sorted(set(known) - set(seen)):
        problems.append(f"{fixture_id}: listed in known_divergences but NDC now equals Amaru: remove the entry")

    # The shared parameter, era-history and schema files NDC does ship must be Amaru's, byte for byte.
    for sub in ("era-history", "protocol-parameters"):
        for path in sorted((fixtures / sub).glob("*.json")):
            twin = common_dir / sub / path.name
            if not twin.is_file():
                problems.append(f"{sub}/{path.name}: NDC ships it, Amaru has no such file at the pinned commit")
            elif twin.read_bytes() != path.read_bytes():
                problems.append(f"{sub}/{path.name}: differs from Amaru at the pinned commit")
    if (fixtures / "schema.json").read_bytes() != (scenarios_dir.parent / "schema.json").read_bytes():
        problems.append("schema.json: differs from Amaru at the pinned commit")

    # Every reference now resolves.
    for name, refs in fixture_refs(fixtures).items():
        for ref in set(refs):
            if not ref_target(fixtures, ref).is_file():
                problems.append(f"{name}: {ref} does not resolve")

    report(problems)
    print(
        f"ledger-rules: {len(ndc_files)} NDC fixtures equal Amaru at the pinned commit apart from "
        f"{len(seen)} listed divergences; all $refs resolve"
    )


def report(problems):
    if problems:
        for problem in problems:
            print(f"ledger-rules: {problem}", file=sys.stderr)
        sys.exit(1)


def main():
    args = sys.argv[1:]
    if len(args) == 3 and args[0] == "unresolved":
        unresolved(args[1], Path(args[2]))
    elif len(args) == 4 and args[0] == "compare":
        compare(args[1], Path(args[2]), Path(args[3]))
    else:
        print(__doc__, file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
