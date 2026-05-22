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

    cn 11.0.1's `cardano-cli debug log-epoch-state` emits `{"currentEpoch": N, ...}`
    per block-applied (NOT once per epoch).  We keep one record per
    epoch — the LAST seen for each epoch number (representing end-of-epoch
    state).  Older / future versions may differ; try common locations.
    """
    for path in (
        ("currentEpoch",),
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

    # The cli emits per-block, but we only need one record per epoch
    # (the last one BEFORE the epoch transition = end-of-epoch state).
    # Buffer the latest record for the current epoch and flush only when
    # the epoch number advances — this avoids ~thousands of needless
    # rewrites per epoch.
    cur_epoch: int | None = None
    cur_record: dict | None = None
    sequential = 0

    def flush(ep: int | None, rec: dict | None) -> None:
        if ep is None or rec is None:
            return
        path = out / f"epoch_{ep:06d}.json"
        tmp = path.with_suffix(".json.tmp")
        with tmp.open("w", encoding="utf-8") as f:
            json.dump(rec, f, indent=2, sort_keys=True)
            f.write("\n")
        os.replace(tmp, path)
        print(f"[split] wrote {path}", file=sys.stderr)

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
        if cur_epoch is None:
            cur_epoch = epoch
            cur_record = record
        elif epoch != cur_epoch:
            # Epoch advanced (or rolled back) — flush the previous one.
            flush(cur_epoch, cur_record)
            cur_epoch = epoch
            cur_record = record
        else:
            # Same epoch — overwrite buffer with newer record.
            cur_record = record
    # Final flush of the last in-flight epoch.
    flush(cur_epoch, cur_record)
    return 0


if __name__ == "__main__":
    sys.exit(main())
