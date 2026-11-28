#!/usr/bin/env python3
"""Read tests/conformance/node-diversity/corpora.toml and print it as shell assignments.

    eval "$(python3 -I pins.py <corpora.toml>)"

The pin file has two readers: this one (for fetch-corpora.sh) and the Rust harness
(tests/conformance/src/node_diversity/pins.rs).  Both reject a malformed pin, so a typo in a
commit or digest cannot reach a fetch or a report.  Exits non-zero, with the reason on stderr,
on any problem.
"""

import re
import shlex
import sys
import tomllib

HEX40 = re.compile(r"^[0-9a-f]{40}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")


def fail(message):
    print(f"pins: {message}", file=sys.stderr)
    sys.exit(1)


def need(table, section, key, pattern=None):
    value = table.get(key)
    if not isinstance(value, str) or not value:
        fail(f"[{section}] {key}: missing or not a string")
    if pattern is not None and not pattern.match(value):
        fail(f"[{section}] {key}: {value!r} is not {pattern.pattern}")
    return value


def main():
    if len(sys.argv) != 2:
        fail("usage: pins.py <corpora.toml>")
    with open(sys.argv[1], "rb") as handle:
        pins = tomllib.load(handle)

    ndc, amaru = pins.get("ndc", {}), pins.get("amaru", {})
    ledger, cbor = pins.get("ledger_rules", {}), pins.get("cbor_dataset", {})
    uplc, turbo = pins.get("uplc_conformance", {}), pins.get("uplc_turbo", {})

    out = {
        "NDC_REPO": need(ndc, "ndc", "repository"),
        "NDC_COMMIT": need(ndc, "ndc", "commit", HEX40),
        "AMARU_DATASET_COMMIT": need(amaru, "amaru", "dataset_commit", HEX40),
        "AMARU_SCENARIOS_DIR": need(amaru, "amaru", "scenarios_dir"),
        "AMARU_COMMON_DIR": need(amaru, "amaru", "common_dir"),
        "AMARU_REPO": need(amaru, "amaru", "repository"),
        "CBOR_REPO": need(cbor, "cbor_dataset", "repository"),
        "CBOR_COMMIT": need(cbor, "cbor_dataset", "commit", HEX40),
        "UPLC_URL": need(uplc, "uplc_conformance", "url"),
        "UPLC_SHA256": need(uplc, "uplc_conformance", "sha256", HEX64),
        "TURBO_URL": need(turbo, "uplc_turbo", "url"),
        "TURBO_SHA256": need(turbo, "uplc_turbo", "sha256", HEX64),
    }

    missing = ledger.get("missing_protocol_parameters", [])
    if not isinstance(missing, list):
        fail("[[ledger_rules.missing_protocol_parameters]] must be an array of tables")
    entries = []
    for index, entry in enumerate(missing):
        section = f"ledger_rules.missing_protocol_parameters[{index}]"
        file = need(entry, section, "file")
        if "/" in file or file.startswith("."):
            fail(f"[{section}] file: {file!r} must be a bare file name")
        digest = need(entry, section, "sha256", HEX64)
        fixtures = entry.get("fixtures")
        if not isinstance(fixtures, int) or isinstance(fixtures, bool) or fixtures < 1:
            fail(f"[{section}] fixtures: must be a positive integer")
        entries.append(f"{file}:{digest}:{fixtures}")
    if len({entry.split(":")[0] for entry in entries}) != len(entries):
        fail("duplicate file in ledger_rules.missing_protocol_parameters")

    for key, value in out.items():
        print(f"{key}={shlex.quote(value)}")
    print("MISSING_PARAMS=(" + " ".join(shlex.quote(entry) for entry in entries) + ")")


if __name__ == "__main__":
    main()
