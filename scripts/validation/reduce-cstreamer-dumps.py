#!/usr/bin/env python3
"""Shrink cardano-streamer's epoch dumps in place, losslessly for comparison.

**Why this is not optional at tip scale.** Measured on the mainnet oracle dumps:

    epoch 208    0.0 MB
    epoch 227   42.3 MB
    epoch 247  107.6 MB
    epoch 273  373.8 MB      <- 66 epochs = 8.3 GB

Growth is superlinear in the delegator count, and mainnet's stake distribution
keeps growing, so a full run to epoch 648 needs several hundred GB of oracle
dumps alone. There was 224 GB free — the run would have died somewhere past
epoch 400, hours in, with no partial result.

`stake` and `delegations` are ~98% of each file, and the comparator ALREADY
reduces them to `{__count__, __sum__, __digest__}` before comparing
(`diff-cstreamer-dumps.py`, `DIGESTABLE_KEYS`). Storing them raw buys nothing.

**Faithful by construction**: this calls the comparator's OWN `digest_of_map`,
so a reduced file compares byte-identically to the raw one — the comparator
passes an existing `__digest__` record straight through. Verified by diffing the
same dumps before and after reduction and requiring an identical verdict, rather
than by arguing it should be.

Only files cardano-streamer has finished with are touched: it writes epoch files
in ascending order, so anything below the highest-numbered file is complete. The
newest is skipped while the producer may still be writing it — pass `--all` for
the final pass once it has exited, or the largest file in the set stays raw.

Usage:
    reduce-cstreamer-dumps.py <dir>              # one pass over completed files
    reduce-cstreamer-dumps.py <dir> --watch      # keep reducing as they appear
    reduce-cstreamer-dumps.py <dir> --all        # final pass, producer exited
"""
import argparse
import importlib.util
import json
import pathlib
import re
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location(
    "diffmod", HERE / "diff-cstreamer-dumps.py"
)
_diff = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_diff)

DIGESTABLE = getattr(_diff, "DIGESTABLE_KEYS", {"stake", "delegations"})


def epoch_of(p: pathlib.Path):
    m = re.match(r"(\d+)", p.name)
    return int(m.group(1)) if m else None


def reduce_obj(obj) -> bool:
    """Replace digestable maps with the comparator's digest form. True if changed."""
    changed = False
    if isinstance(obj, dict):
        for k, v in list(obj.items()):
            if (
                k in DIGESTABLE
                and isinstance(v, dict)
                and "__digest__" not in v
                and v
                and all(isinstance(x, (int, str)) and not isinstance(x, bool)
                        for x in v.values())
            ):
                obj[k] = _diff.digest_of_map(v)
                changed = True
            else:
                changed |= reduce_obj(v)
    elif isinstance(obj, list):
        for v in obj:
            changed |= reduce_obj(v)
    return changed


def reduce_file(p: pathlib.Path) -> int:
    """Reduce one file in place. Returns bytes saved (0 if unchanged)."""
    before = p.stat().st_size
    try:
        obj = json.loads(p.read_text())
    except Exception as e:
        print(f"  {p.name}: unreadable, left alone ({e})")
        return 0
    if not reduce_obj(obj):
        return 0
    tmp = p.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(obj, separators=(",", ":")))
    tmp.replace(p)  # atomic: a crash never leaves a half-written dump
    return before - p.stat().st_size


def pass_once(d: pathlib.Path, verbose: bool, include_newest: bool = False,
              done: set | None = None) -> int:
    files = sorted((f for f in d.glob("*.json") if epoch_of(f) is not None),
                   key=epoch_of)
    if not files or (len(files) == 1 and not include_newest):
        return 0
    # The newest file may still be being written, so it is skipped while the
    # producer is alive. `--all` is for the final pass AFTER it has exited,
    # where the last epoch is complete and would otherwise stay raw — that is
    # the single largest file in the set at tip scale.
    targets = files if include_newest else files[:-1]
    # Skip files already handled in this process.
    #
    # `reduce_file` reads and PARSES a whole file before it can discover there
    # is nothing to do, so without this every 30 s pass re-parsed the entire
    # dump set. Measured on the mainnet tip run at 474 epochs: a sustained 99%
    # of one core and ~3.3 GB of re-reads per pass, against two replays that
    # were themselves only at ~35% CPU. The cost grows with the run, so it is
    # worst exactly when the run is longest.
    #
    # Correct because cstreamer writes each epoch file ONCE and never rewrites
    # it, and `targets` excludes the newest file while the producer is alive —
    # so anything processed here is already complete.
    if done is not None:
        targets = [f for f in targets if f not in done]
    saved = 0
    for f in targets:
        s = reduce_file(f)
        if s and verbose:
            print(f"  {f.name}: -{s / 1048576:.1f} MB")
        saved += s
        if done is not None:
            done.add(f)
    return saved


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("dump_dir")
    ap.add_argument("--watch", action="store_true")
    ap.add_argument("--interval", type=int, default=30)
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument("--all", action="store_true",
                    help="also reduce the newest file; only safe once the "
                         "producer has exited")
    args = ap.parse_args()

    d = pathlib.Path(args.dump_dir)
    if not d.is_dir():
        print(f"no such directory: {d}")
        return 2

    total = 0
    # Only the watch loop needs the memo. A one-shot pass visits each file once
    # anyway, and `--all` must be free to revisit the newest file the watch loop
    # deliberately left alone.
    done: set | None = set() if (args.watch and not args.all) else None
    while True:
        total += pass_once(d, not args.quiet, include_newest=args.all, done=done)
        if not args.watch:
            break
        time.sleep(args.interval)
    print(f"reclaimed {total / 1073741824:.2f} GB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
