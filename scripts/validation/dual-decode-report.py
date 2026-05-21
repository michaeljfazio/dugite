#!/usr/bin/env python3
"""
dual-decode-report.py — summarise dual-decode mismatch artefacts.

Walks a directory of artefacts written by dugite-node when
DUGITE_DUAL_DECODE=dump. Groups mismatches by Cardano era and by outcome
kind (content-diverged / error-diverged / result-shape-mismatch), prints a
per-era summary table, and for the first 5 mismatches in each era prints
the file paths and a diff snippet.

Exit codes:
  0 — no mismatches found in the directory
  1 — one or more mismatches found
  2 — usage / I/O error

Usage:
  dual-decode-report.py [<mismatch_dir>]

  <mismatch_dir>  directory to scan (default: ./dual_decode_mismatches/)

Artefact naming convention (set by dual_decode.rs):
  <era>_<outcome>_<hex-hash-or-counter>.{cbor,pallas.txt,inhouse.txt,diff.txt}

  era       := byron | shelley | allegra | mary | alonzo | babbage | conway
               | dijkstra | unknown
  outcome   := content-diverged | error-diverged | result-shape-mismatch
"""

import os
import sys
import re
import textwrap
from collections import defaultdict
from pathlib import Path

# ── Constants ────────────────────────────────────────────────────────────────

KNOWN_ERAS = [
    "byron", "shelley", "allegra", "mary",
    "alonzo", "babbage", "conway", "dijkstra",
]

KNOWN_OUTCOMES = [
    "content-diverged",
    "error-diverged",
    "result-shape-mismatch",
]

# Maximum diff snippet lines to display per artefact
DIFF_SNIPPET_LINES = 30

# Maximum mismatch files to detail per (era, outcome) cell
MAX_EXAMPLES = 5

# ── Helpers ──────────────────────────────────────────────────────────────────

def detect_era_and_outcome(filename: str):
    """
    Parse (era, outcome) from a mismatch artefact filename.

    Accepts two formats:
      1. <era>_<outcome>_<id>.<ext>          (canonical from dual_decode.rs)
      2. <era>/<outcome>/<id>.<ext>          (directory-partitioned variant)

    Falls back to ("unknown", "unknown") if the pattern is not recognised.
    """
    stem = Path(filename).stem  # strip extension
    name = stem.replace("/", "_").replace(os.sep, "_")

    for era in KNOWN_ERAS:
        for outcome in KNOWN_OUTCOMES:
            pattern = re.compile(
                rf"^{re.escape(era)}[_\-]{re.escape(outcome)}",
                re.IGNORECASE,
            )
            if pattern.match(name):
                return era, outcome

    # Try directory components: .../era/outcome/...
    parts = Path(filename).parts
    era_found = next((p.lower() for p in parts if p.lower() in KNOWN_ERAS), "unknown")
    outcome_found = next(
        (p.lower() for p in parts if p.lower() in KNOWN_OUTCOMES), "unknown"
    )
    return era_found, outcome_found


def read_snippet(path: Path, max_lines: int = DIFF_SNIPPET_LINES) -> str:
    """Read the first `max_lines` lines of a text file; return empty string on error."""
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            lines = []
            for i, line in enumerate(fh):
                if i >= max_lines:
                    lines.append(f"... (truncated at {max_lines} lines)")
                    break
                lines.append(line.rstrip())
            return "\n".join(lines)
    except OSError:
        return "(could not read file)"


def find_mismatch_groups(root: Path):
    """
    Walk `root`, collect mismatch artefacts, group by (era, outcome).

    Returns:
        groups  dict[(era, outcome)] -> list[dict]
            Each dict has keys: cbor, pallas_txt, inhouse_txt, diff_txt
            (all Path|None), and a "stem" key for display.

    We anchor on .cbor files as the canonical "one mismatch = one CBOR".
    If no .cbor is found for a stem we include .diff.txt files too so that
    the report is still useful when the dump only wrote diffs.
    """
    groups = defaultdict(list)
    seen_stems = set()

    # Pass 1: anchor on .cbor files
    for dirpath, _dirs, files in os.walk(root):
        for fname in sorted(files):
            if not fname.endswith(".cbor"):
                continue
            stem = fname[: -len(".cbor")]
            fpath = Path(dirpath) / fname
            era, outcome = detect_era_and_outcome(str(fpath.relative_to(root)))
            entry = _build_entry(stem, fpath.parent)
            groups[(era, outcome)].append(entry)
            seen_stems.add(fpath.parent / stem)

    # Pass 2: pick up diff-only artefacts (when CBOR was not dumped)
    for dirpath, _dirs, files in os.walk(root):
        for fname in sorted(files):
            if not fname.endswith(".diff.txt"):
                continue
            stem = fname[: -len(".diff.txt")]
            fpath = Path(dirpath) / fname
            key = fpath.parent / stem
            if key in seen_stems:
                continue
            era, outcome = detect_era_and_outcome(str(fpath.relative_to(root)))
            entry = _build_entry(stem, fpath.parent)
            groups[(era, outcome)].append(entry)
            seen_stems.add(key)

    return groups


def _build_entry(stem: str, parent: Path) -> dict:
    def maybe(ext):
        p = parent / (stem + ext)
        return p if p.exists() else None

    return {
        "stem": stem,
        "cbor": maybe(".cbor"),
        "pallas_txt": maybe(".pallas.txt"),
        "inhouse_txt": maybe(".inhouse.txt"),
        "diff_txt": maybe(".diff.txt"),
    }


# ── Formatting ───────────────────────────────────────────────────────────────

def build_summary_table(groups) -> str:
    """Render an ASCII summary table: rows = eras, columns = outcome kinds."""
    all_eras = sorted(
        {era for era, _ in groups},
        key=lambda e: KNOWN_ERAS.index(e) if e in KNOWN_ERAS else 99,
    )
    all_outcomes = sorted(
        {outcome for _, outcome in groups},
        key=lambda o: KNOWN_OUTCOMES.index(o) if o in KNOWN_OUTCOMES else 99,
    )

    col_w = max(len(o) for o in all_outcomes) + 2
    era_w = max((len(e) for e in all_eras), default=7) + 2

    header = f"{'Era':<{era_w}}" + "".join(f"{o:>{col_w}}" for o in all_outcomes) + f"{'Total':>{col_w}}"
    sep = "-" * len(header)

    rows = [header, sep]
    grand_total = 0
    for era in all_eras:
        row_total = 0
        cells = []
        for outcome in all_outcomes:
            n = len(groups.get((era, outcome), []))
            row_total += n
            grand_total += n
            cells.append(f"{n:>{col_w}}")
        rows.append(f"{era:<{era_w}}" + "".join(cells) + f"{row_total:>{col_w}}")

    rows.append(sep)
    rows.append(
        f"{'TOTAL':<{era_w}}"
        + "".join(
            f"{sum(len(groups.get((e, o), [])) for e in all_eras):>{col_w}}"
            for o in all_outcomes
        )
        + f"{grand_total:>{col_w}}"
    )
    return "\n".join(rows)


def print_examples(groups):
    """Print file paths + diff snippets for the first MAX_EXAMPLES mismatches in each group."""
    all_eras = sorted(
        {era for era, _ in groups},
        key=lambda e: KNOWN_ERAS.index(e) if e in KNOWN_ERAS else 99,
    )
    all_outcomes = sorted(
        {outcome for _, outcome in groups},
        key=lambda o: KNOWN_OUTCOMES.index(o) if o in KNOWN_OUTCOMES else 99,
    )

    for era in all_eras:
        for outcome in all_outcomes:
            entries = groups.get((era, outcome), [])
            if not entries:
                continue
            total = len(entries)
            shown = entries[:MAX_EXAMPLES]
            print(f"\n{'='*72}")
            print(f"  {era.upper()} / {outcome}  ({total} mismatch{'es' if total != 1 else ''})")
            print(f"{'='*72}")
            for i, entry in enumerate(shown, 1):
                print(f"\n  [{i}/{min(total, MAX_EXAMPLES)}] stem: {entry['stem']}")
                for label, key in [
                    ("CBOR       ", "cbor"),
                    ("Pallas out ", "pallas_txt"),
                    ("In-house   ", "inhouse_txt"),
                    ("Diff       ", "diff_txt"),
                ]:
                    p = entry[key]
                    if p:
                        print(f"    {label}: {p}")
                    else:
                        print(f"    {label}: (not present)")

                # Prefer .diff.txt; fall back to pallas vs inhouse comparison
                diff_path = entry["diff_txt"]
                if diff_path:
                    snippet = read_snippet(diff_path)
                    print(f"\n    -- diff snippet ({diff_path.name}) --")
                    print(textwrap.indent(snippet, "    "))
                elif entry["pallas_txt"] and entry["inhouse_txt"]:
                    pallas_lines = read_snippet(entry["pallas_txt"], 15).splitlines()
                    inhouse_lines = read_snippet(entry["inhouse_txt"], 15).splitlines()
                    print("\n    -- pallas (first 15 lines) --")
                    for ln in pallas_lines:
                        print(f"    {ln}")
                    print("\n    -- in-house (first 15 lines) --")
                    for ln in inhouse_lines:
                        print(f"    {ln}")

            if total > MAX_EXAMPLES:
                print(f"\n  ... and {total - MAX_EXAMPLES} more (increase MAX_EXAMPLES to see them)")


# ── Entry point ───────────────────────────────────────────────────────────────

def main():
    if len(sys.argv) > 2:
        print(f"Usage: {sys.argv[0]} [<mismatch_dir>]", file=sys.stderr)
        sys.exit(2)

    root = Path(sys.argv[1]) if len(sys.argv) == 2 else Path("./dual_decode_mismatches")

    if not root.exists():
        print(f"[INFO] Mismatch directory does not exist: {root}", file=sys.stderr)
        print("[INFO] No mismatches found (directory absent is treated as zero mismatches).")
        sys.exit(0)

    if not root.is_dir():
        print(f"ERROR: {root} is not a directory", file=sys.stderr)
        sys.exit(2)

    groups = find_mismatch_groups(root)

    total = sum(len(v) for v in groups.values())

    print(f"\ndual-decode-report — scanning: {root.resolve()}")
    print(f"Total mismatch artefacts found: {total}\n")

    if total == 0:
        print("No mismatches. Shadow decode is in sync with reference decoder.")
        sys.exit(0)

    # Summary table
    print(build_summary_table(groups))

    # Per-era examples
    print_examples(groups)

    print(f"\n{'='*72}")
    print(f"RESULT: {total} MISMATCH{'ES' if total != 1 else ''} FOUND — investigate before M6 cutover.")
    print(f"{'='*72}\n")
    sys.exit(1)


if __name__ == "__main__":
    main()
