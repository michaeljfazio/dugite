#!/usr/bin/env python3
"""Split a JSONL stream from `cardano-cli debug log-epoch-state` into
one `epoch_NNNNNN.json` file per record under the specified output
directory.

Reads from stdin so it can be piped from `tail -F` (see
`capture-haskell-epoch-dumps.sh`).  Idempotent: re-writes a per-epoch
file if a later record carries the same epoch (Haskell may re-emit on
rollback).

Usage:
    tail -F epoch-states.jsonl | split-haskell-jsonl.py --out-dir DIR
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path


def epoch_of(record: dict) -> int | None:
    """Best-effort extract of the epoch number from a Haskell record.

    The exact path depends on the cli version.  We try a few obvious
    locations before giving up.
    """
    for path in (
        ("epoch",),
        ("nesEL", "unEpochNo"),
        ("nesEL",),
        ("newEpochState", "nesEL", "unEpochNo"),
        ("newEpochState", "nesEL"),
    ):
        cur: object = record
        ok = True
        for key in path:
            if isinstance(cur, dict) and key in cur:
                cur = cur[key]
            else:
                ok = False
                break
        if ok and isinstance(cur, int):
            return cur
    return None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out-dir", required=True, type=Path)
    ap.add_argument(
        "--strict",
        action="store_true",
        help="fail if a record cannot be epoch-tagged (default: warn + use sequential index)",
    )
    args = ap.parse_args()

    out: Path = args.out_dir
    out.mkdir(parents=True, exist_ok=True)

    sequential = 0
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError as e:
            print(f"[split] skipping malformed line: {e}", file=sys.stderr)
            continue
        epoch = epoch_of(record)
        if epoch is None:
            if args.strict:
                print(
                    "[split] strict mode: record has no epoch tag, aborting",
                    file=sys.stderr,
                )
                return 1
            print(
                f"[split] warning: no epoch tag, falling back to seq#{sequential}",
                file=sys.stderr,
            )
            epoch = -sequential
            sequential += 1
        path = out / f"epoch_{epoch:06d}.json"
        tmp = path.with_suffix(".json.tmp")
        with tmp.open("w", encoding="utf-8") as f:
            json.dump(record, f, indent=2, sort_keys=True)
            f.write("\n")
        os.replace(tmp, path)
        print(f"[split] wrote {path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
