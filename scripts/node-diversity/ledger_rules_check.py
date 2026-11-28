#!/usr/bin/env python3
"""Checks of the NDC ledger-rules fixtures against Amaru, run by fetch-corpora.sh.

    ledger_rules_check.py unresolved <corpora.toml> <ndc fixtures dir>
        Before the missing parameter files are added: the fixtures' `$ref`s that NDC does not
        resolve must be EXACTLY the files pinned in [[ledger_rules.missing_protocol_parameters]],
        each with its pinned fixture count.  A file NDC now ships fails the check (remove the
        workaround); so does a new unresolved reference.

    ledger_rules_check.py compare <corpora.toml> <ndc fixtures dir> <amaru checkout>
        After: every `$ref` resolves, and the NDC fixtures equal Amaru's at the pinned commit,
        apart from [ledger_rules.known_divergences] (id -> the exact LEAF PATHS that differ; an
        unlisted path fails, and so does a listed path that no longer differs).  Every file under
        Amaru's common/ directory must exist in NDC byte for byte; NDC-only files in those
        directories must be listed in [ledger_rules] ndc_only_files.

    ledger_rules_check.py diff-paths <corpora.toml> <ndc fixtures dir> <amaru checkout>
        Maintenance: print the divergences as they are now, in corpora.toml form.

Comparison is TYPE-STRICT (JSON `true`, `1` and `1.0` are three different values; Python's `==`
would equate them) and a JSON object with a repeated key is rejected, because a duplicate key
silently keeps the last value.  Input is untrusted data: it is only parsed and compared, never
executed.  Exits non-zero with every problem listed.
"""

import hashlib
import json
import sys
import tomllib
from pathlib import Path


def reject_duplicates(pairs):
    seen = {}
    for key, value in pairs:
        if key in seen:
            raise ValueError(f"duplicate JSON key {key!r}")
        seen[key] = value
    return seen


def reject_constant(name):
    raise ValueError(f"non-standard JSON constant {name}")


def load_json(path):
    try:
        return json.loads(path.read_text(), object_pairs_hook=reject_duplicates, parse_constant=reject_constant)
    except ValueError as error:
        raise SystemExit(f"ledger-rules: {path}: {error}")


def load_pins(path):
    with open(path, "rb") as handle:
        return tomllib.load(handle)


def strict_equal(a, b):
    """JSON equality that does not equate true, 1 and 1.0 (Python's == does)."""
    if type(a) is not type(b):
        return False
    if isinstance(a, dict):
        return a.keys() == b.keys() and all(strict_equal(a[k], b[k]) for k in a)
    if isinstance(a, list):
        return len(a) == len(b) and all(strict_equal(x, y) for x, y in zip(a, b))
    return a == b


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
        collect_refs(load_json(path), found)
        refs[path.name] = found
    return refs


def ref_target(fixtures, ref):
    """A fixture `$ref` is `common/<dir>/<file>` (Amaru's layout); NDC drops the `common/`."""
    if not ref.startswith("common/") or ".." in ref.split("/"):
        raise ValueError(f"unexpected $ref {ref!r}")
    return fixtures / ref[len("common/"):]


def by_prefix(names, what, problems):
    """{five-digit id: file name}; a shared prefix is a problem, not an overwrite."""
    ids = {}
    for name in sorted(names):
        key = name[:5]
        if not (len(key) == 5 and key.isdigit()):
            problems.append(f"{what}: {name} does not start with a five-digit id")
        elif key in ids:
            problems.append(f"{what}: {ids[key]} and {name} share the id {key}")
        else:
            ids[key] = name
    return ids


def unresolved(pins_path, fixtures):
    pins = load_pins(pins_path)
    pinned = {entry["file"]: entry["fixtures"] for entry in pins["ledger_rules"].get("missing_protocol_parameters", [])}
    problems = []
    absent = {}  # ref -> number of fixtures
    for refs in fixture_refs(fixtures).values():
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
    print(f"ledger-rules: NDC lacks {len(absent_files)} protocol-parameter files, referenced by {sum(absent_files.values())} fixtures")


# ── normalisation: undo ONLY what NDC is known to rewrite ───────────────────────────────────
#
# NDC copied Amaru's fixtures and stripped serde defaults (NDC commit 61b1732635, "remove default
# fields from JSON scenarios", and later batches).  Each rule below
# undoes exactly one such rewrite, at exactly one place, on BOTH sides, because Amaru's schema
# (fixtures/schema.json) says the two spellings mean the same:
#
#  R1  top-level `category`: NDC added it to 308 fixtures that had none.  Dropped from NDC's
#      side only when Amaru's fixture has no `category`; when both have one they must be equal.
#  R2  initial_state.committee[*].status and .valid_until: Amaru writes `null` for a member with
#      no hot credential / no term; the schema says "absent".  null == absent here only.
#  R3  initial_state.guardrail_script: the schema allows null and says "absent or null" mean the
#      same.  null == absent here only.
#  R4  initial_state.{utxo,pools,accounts,dreps,proposals,committee}: `[]` == absent (the
#      schema: "Empty when no ... state is seeded").
#  R5  initial_state.proposals_roots: `{}` == absent ("Use {} when none are enacted; absent
#      purposes default to none").
#  R6  initial_state.governance_activity == {"consecutive_dormant_epochs": 0} == absent (the
#      counter's default).  Compared type-strictly: `false` or `0.0` are not the default.
#
# Everything else (every other null, every other empty value, every other key) is compared as is.

EMPTY_LIST_SECTIONS = ("utxo", "pools", "accounts", "dreps", "proposals", "committee")
IS_DEFAULT_ACTIVITY = {"consecutive_dormant_epochs": 0}


def normalise(fixture, drop_category):
    fixture = dict(fixture)
    if drop_category:
        fixture.pop("category", None)  # R1
    state = fixture.get("initial_state")
    if not isinstance(state, dict):
        return fixture
    state = dict(state)
    committee = state.get("committee")
    if isinstance(committee, list):
        cleaned = []
        for member in committee:
            if isinstance(member, dict):
                member = {k: v for k, v in member.items() if not (k in ("status", "valid_until") and v is None)}  # R2
            cleaned.append(member)
        state["committee"] = cleaned
    if "guardrail_script" in state and state["guardrail_script"] is None:  # R3
        del state["guardrail_script"]
    for key in EMPTY_LIST_SECTIONS:  # R4
        if key in state and strict_equal(state[key], []):
            del state[key]
    if "proposals_roots" in state and strict_equal(state["proposals_roots"], {}):  # R5
        del state["proposals_roots"]
    if "governance_activity" in state and strict_equal(state["governance_activity"], IS_DEFAULT_ACTIVITY):  # R6
        del state["governance_activity"]
    fixture["initial_state"] = state
    return fixture


def diff_paths(a, b, path=""):
    """The leaf paths at which two JSON values differ (type-strictly)."""
    if isinstance(a, dict) and isinstance(b, dict):
        out = []
        for key in sorted(set(a) | set(b)):
            here = f"{path}.{key}" if path else key
            if key not in a or key not in b:
                out.append(here)
            else:
                out.extend(diff_paths(a[key], b[key], here))
        return out
    if isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        out = []
        for index, (x, y) in enumerate(zip(a, b)):
            out.extend(diff_paths(x, y, f"{path}[{index}]"))
        return out
    return [] if strict_equal(a, b) else [path]


def measured_divergences(fixtures, scenarios_dir, problems):
    ndc_files = {p.name for p in (fixtures / "scenarios").glob("*.json")}
    amaru_files = {p.name for p in scenarios_dir.glob("*.json")}
    ndc_ids = by_prefix(ndc_files, "NDC", problems)
    amaru_ids = by_prefix(amaru_files, "Amaru", problems)
    for name in sorted(ndc_files - amaru_files):
        problems.append(f"{name}: in NDC, not in Amaru at the pinned commit")
    for name in sorted(amaru_files - ndc_files):
        problems.append(f"{name}: in Amaru at the pinned commit, not in NDC")
    seen = {}
    for fixture_id in sorted(set(ndc_ids) & set(amaru_ids)):
        if ndc_ids[fixture_id] != amaru_ids[fixture_id]:
            continue  # reported as a missing file above
        ours = load_json(fixtures / "scenarios" / ndc_ids[fixture_id])
        theirs = load_json(scenarios_dir / amaru_ids[fixture_id])
        drop_category = "category" not in theirs
        paths = diff_paths(normalise(ours, drop_category), normalise(theirs, False))
        if paths:
            seen[fixture_id] = sorted(paths)
    return seen, len(ndc_files)


def compare_common(fixtures, common_dir, pins, problems):
    """Every file Amaru ships under common/ must be in NDC, byte for byte; NDC extras must be pinned."""
    listed_extra = set(pins["ledger_rules"].get("ndc_only_files", []))
    amaru_files = {p.relative_to(common_dir).as_posix() for p in common_dir.rglob("*") if p.is_file()}
    ndc_dirs = {rel.split("/")[0] for rel in amaru_files}
    for rel in sorted(amaru_files):
        mine = fixtures / rel
        if not mine.is_file():
            problems.append(f"common/{rel}: Amaru ships it, NDC (with the pinned additions) does not")
        elif mine.read_bytes() != (common_dir / rel).read_bytes():
            problems.append(f"common/{rel}: differs from Amaru at the pinned commit")
    ndc_files = {
        p.relative_to(fixtures).as_posix()
        for d in sorted(ndc_dirs)
        for p in (fixtures / d).rglob("*")
        if p.is_file()
    }
    extras = ndc_files - amaru_files
    for rel in sorted(extras - listed_extra):
        problems.append(f"{rel}: in NDC only, and not listed in ndc_only_files")
    for rel in sorted(listed_extra - extras):
        problems.append(f"{rel}: listed in ndc_only_files but NDC no longer has it (or Amaru now does): remove the entry")


def compare(pins_path, fixtures, amaru_root):
    pins = load_pins(pins_path)
    scenarios_dir = amaru_root / pins["amaru"]["scenarios_dir"]
    common_dir = amaru_root / pins["amaru"]["common_dir"]
    known = {key: sorted(value) for key, value in pins["ledger_rules"].get("known_divergences", {}).items()}
    problems = []

    seen, count = measured_divergences(fixtures, scenarios_dir, problems)
    for fixture_id, paths in sorted(seen.items()):
        if fixture_id not in known:
            problems.append(f"{fixture_id}: NDC differs from Amaru at {paths} and corpora.toml does not list it")
        else:
            for path in sorted(set(paths) - set(known[fixture_id])):
                problems.append(f"{fixture_id}: NDC differs from Amaru at {path!r}, which corpora.toml does not list")
            for path in sorted(set(known[fixture_id]) - set(paths)):
                problems.append(f"{fixture_id}: {path!r} is listed in known_divergences but no longer differs: remove the entry")
    for fixture_id in sorted(set(known) - set(seen)):
        problems.append(f"{fixture_id}: listed in known_divergences but NDC now equals Amaru: remove the entry")

    compare_common(fixtures, common_dir, pins, problems)
    if (fixtures / "schema.json").read_bytes() != (scenarios_dir.parent / "schema.json").read_bytes():
        problems.append("schema.json: differs from Amaru at the pinned commit")

    for name, refs in fixture_refs(fixtures).items():
        for ref in set(refs):
            if not ref_target(fixtures, ref).is_file():
                problems.append(f"{name}: {ref} does not resolve")

    report(problems)
    digest = hashlib.sha256(",".join(f"{k}:{'|'.join(v)}" for k, v in sorted(seen.items())).encode()).hexdigest()[:12]
    print(
        f"ledger-rules: {count} NDC fixtures equal Amaru at the pinned commit apart from "
        f"{len(seen)} listed divergences ({sum(map(len, seen.values()))} leaf paths, {digest}); "
        f"common/ files byte-equal; all $refs resolve"
    )


def print_divergences(pins_path, fixtures, amaru_root):
    pins = load_pins(pins_path)
    problems = []
    seen, _ = measured_divergences(fixtures, amaru_root / pins["amaru"]["scenarios_dir"], problems)
    for fixture_id, paths in sorted(seen.items()):
        print(f'"{fixture_id}" = [{", ".join(json.dumps(p) for p in paths)}]')
    for problem in problems:
        print(f"# {problem}", file=sys.stderr)


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
    elif len(args) == 4 and args[0] == "diff-paths":
        print_divergences(args[1], Path(args[2]), Path(args[3]))
    else:
        print(__doc__, file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
