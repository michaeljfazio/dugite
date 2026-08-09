#!/usr/bin/env python3
"""Diff dugite's `dump-snapshot` output against cardano-streamer's, epoch by epoch.

    scripts/validation/diff-cstreamer-dumps.py \
        --dugite  reports/mainnet-exactness/dugite \
        --cstreamer reports/mainnet-exactness/cstreamer \
        [--json report.json] [--max-examples N]

Both sides dump at the SAME instant — the first block of the new epoch, after
every boundary transition has been applied, labelled with the NEW epoch.
cardano-streamer's README states this outright ("all fields reflect the
post-boundary state") and dugite's `run_dump_snapshot` writes post-`apply_block`
when `current_epoch > last_epoch`. Verify that before trusting any output here:
different instants inside one boundary make every epoch look divergent for a
reason that is not a bug.

WHY BISECTION RATHER THAN A SINGLE END-STATE NUMBER
---------------------------------------------------
A single number at the tip cannot distinguish a step change from a slow drift,
and those have completely different causes. #1073's predecessor investigation
burned ten hypotheses on one tail number. This reports the FIRST epoch at which
each field path diverges.

WHAT GETS COMPARED: EVERYTHING
------------------------------
This walks both JSON trees and compares every leaf, rather than a whitelist of
interesting fields. The first version of this tool DID use a whitelist, and it
silently never compared `rupdApplied`, `poolDistribution`, `snapshots`,
`conwayGov` or `instantaneousRewards` — five of cardano-streamer's largest
fields, including the entire mark/set/go stake state. A whitelist reports full
marks for whatever it happens to list, which is the defect class this tool
exists to catch.

ERA AWARENESS
-------------
Eras carry different ledger information, so a field's absence is only
meaningful relative to its era:

  * cardano-streamer emits NOTHING for Byron epochs — `buildSnapshotJson`
    returns `Nothing` there, because `ChainAccountState` (treasury/reserves) is
    introduced BY the Shelley translation. Byron epochs are reported
    ORACLE-SILENT, never divergent. Back-projecting a Shelley shape onto Byron
    is modelling, not reporting — that is what disqualified Koios, whose Byron
    rows satisfy the Shelley-only supply invariant.
  * `conwayGov` is null before Conway.
  * `instantaneousRewards` is Shelley-Babbage only (always empty in Conway+).
  * `epochNonce` is null for Byron and for the neutral nonce.

A field expected in its era and MISSING on one side is a hard failure, not a
skip. Null on both is counted VACUOUS and reported separately — two
implementations agreeing that there is nothing to say is not evidence.

EXIT CODES
----------
  0  every compared leaf matched, and the comparison was non-vacuous
  1  at least one leaf diverged
  2  a schema gap: a key expected in its era was absent on one side
  3  the comparison was vacuous (nothing was actually compared)
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections import defaultdict
import hashlib
from math import gcd

# ── Era model ────────────────────────────────────────────────────────────
ERA_ORDER = [
    "Byron", "Shelley", "Allegra", "Mary",
    "Alonzo", "Babbage", "Conway", "Dijkstra",
]


def era_index(name: str) -> int:
    try:
        return ERA_ORDER.index(name)
    except ValueError:
        return -1


def era_applicable(top_key: str, era: str) -> bool:
    """Is `top_key` expected to carry a value in `era`?"""
    i = era_index(era)
    if top_key == "conwayGov":
        return i >= era_index("Conway")
    if top_key == "instantaneousRewards":
        return era_index("Shelley") <= i <= era_index("Babbage")
    if top_key == "epochNonce":
        return i >= era_index("Shelley")
    return True


# Leaf paths deliberately not compared, each with a stated reason. Anything NOT
# listed here IS compared — there is no silent skip.
EXCLUDED_SUFFIXES = {
    # Display-only float, derived from stakeLovelace, which IS compared. Two
    # correct implementations can differ in its last bit for no ledger reason.
    "stakePercent",
}


LARGE_MAP_THRESHOLD = 500

def _canon(v):
    return json.dumps(v, sort_keys=True, separators=(",", ":"), default=str)


def summarise(obj):
    """Reduce one epoch's tree so two of them fit in memory at once.

    Mainnet dumps reach 416 MB per epoch — the mark/set/go snapshots carry one
    entry per stake credential. Holding two of those as Python objects is
    several GB, so each large map is replaced by a digest record BEFORE the
    other side is loaded.

    This does not weaken the comparison: a sha256 over every sorted entry
    detects ANY difference, down to one lovelace on one credential. What it
    gives up is knowing WHICH entry differs from the summary alone — use
    `--drill <path> --drill-epoch <n>` to enumerate that, on demand and for one
    epoch at a time.
    """
    if isinstance(obj, dict):
        if len(obj) > LARGE_MAP_THRESHOLD:
            vals = list(obj.values())
            total = sum(vals) if all(isinstance(v, (int, float)) and not isinstance(v, bool)
                                     for v in vals) else None
            h = hashlib.sha256()
            for k in sorted(obj):
                h.update(_canon(k).encode())
                h.update(b"=")
                h.update(_canon(obj[k]).encode())
                h.update(b";")
            rec = {"__count__": len(obj), "__digest__": h.hexdigest()}
            if total is not None:
                rec["__sum__"] = total
            return rec
        return {k: summarise(v) for k, v in obj.items()}
    if isinstance(obj, list):
        if len(obj) > LARGE_MAP_THRESHOLD:
            h = hashlib.sha256()
            for e in sorted((_canon(x) for x in obj)):
                h.update(e.encode())
                h.update(b";")
            return {"__count__": len(obj), "__digest__": h.hexdigest()}
        return [summarise(v) for v in obj]
    return obj


class Result:
    def __init__(self) -> None:
        self.match = defaultdict(int)
        self.diff = defaultdict(list)        # path -> [(epoch, a, b)]
        self.absent_one = defaultdict(list)  # path -> [(epoch, missing_side)]
        self.vacuous = defaultdict(int)
        self.oracle_silent: list[int] = []
        self.unpaired_dugite: list[int] = []
        self.unpaired_cstreamer: list[int] = []

    @property
    def comparisons(self) -> int:
        return sum(self.match.values()) + sum(len(v) for v in self.diff.values())


_MISSING = object()


def norm(v):
    """Normalise a leaf for comparison.

    Rationals compare by VALUE, not representation: both sides reduce to
    lowest terms today, but a future change to either reduction must not read
    as a ledger divergence.
    """
    if isinstance(v, dict) and set(v.keys()) == {"numerator", "denominator"}:
        n, d = v.get("numerator"), v.get("denominator")
        if isinstance(n, int) and isinstance(d, int) and d != 0:
            g = gcd(abs(n), abs(d)) or 1
            return ("rational", n // g, d // g)
    if isinstance(v, bool):
        return v
    # 5 and 5.0 are the same ledger quantity; JSON typing differs across encoders.
    if isinstance(v, float) and v.is_integer():
        return int(v)
    return v


def collapse_path(path: str) -> str:
    """Collapse list indices and map keys so the summary groups by SHAPE.

    `poolDistribution[17].stakeLovelace` and `snapshots.go.stake.<cred>` would
    otherwise produce one summary row per pool and per credential — hundreds of
    thousands of rows at mainnet scale, which is a dump, not a report. The
    per-epoch detail is preserved in the divergence list.
    """
    out, i = [], 0
    while i < len(path):
        c = path[i]
        if c == "[":
            j = path.find("]", i)
            out.append("[]")
            i = j + 1 if j != -1 else i + 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def walk(res: Result, epoch: int, era: str, path: str, a, b, top: str) -> None:
    """Compare two JSON subtrees leaf-by-leaf."""
    if path.rsplit(".", 1)[-1] in EXCLUDED_SUFFIXES:
        return

    a_missing, b_missing = a is _MISSING, b is _MISSING
    if a_missing and b_missing:
        return

    # An absence is only a gap if the field belongs in this era.
    if (a_missing or b_missing) and not era_applicable(top, era):
        return

    if a_missing or b_missing:
        res.absent_one[collapse_path(path)].append(
            (epoch, "dugite" if a_missing else "cstreamer")
        )
        return

    if a is None and b is None:
        res.vacuous[collapse_path(path)] += 1
        return

    if isinstance(a, dict) and isinstance(b, dict):
        # Rationals are leaves, not sub-objects.
        if set(a.keys()) == {"numerator", "denominator"} or \
           set(b.keys()) == {"numerator", "denominator"}:
            pass
        else:
            for k in sorted(set(a) | set(b)):
                walk(res, epoch, era, f"{path}.{k}",
                     a.get(k, _MISSING), b.get(k, _MISSING), top)
            return

    if isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            res.diff[collapse_path(path) + ".<length>"].append((epoch, len(a), len(b)))
            return
        # poolDistribution is a list of objects with a `poolId`; order is not
        # semantically meaningful, so match by id rather than position. A
        # positional compare would report every pool as divergent the moment
        # one side sorted differently.
        if a and isinstance(a[0], dict) and "poolId" in a[0]:
            am = {e.get("poolId"): e for e in a}
            bm = {e.get("poolId"): e for e in b}
            for pid in sorted(set(am) | set(bm)):
                walk(res, epoch, era, f"{path}[{pid}]",
                     am.get(pid, _MISSING), bm.get(pid, _MISSING), top)
            return
        for idx, (x, y) in enumerate(zip(a, b)):
            walk(res, epoch, era, f"{path}[{idx}]", x, y, top)
        return

    key = collapse_path(path)
    if norm(a) == norm(b):
        res.match[key] += 1
    else:
        res.diff[key].append((epoch, a, b))


def compare_epoch(res: Result, epoch: int, dug: dict, cst: dict) -> None:
    # Era comes from the ORACLE where available: it decides applicability, and
    # using dugite's own value would let a wrong era on dugite's side switch off
    # the very fields that would have exposed it.
    era = cst.get("snapshotEraName") or dug.get("snapshotEraName") or "Shelley"
    for k in sorted(set(dug) | set(cst)):
        walk(res, epoch, era, k, dug.get(k, _MISSING), cst.get(k, _MISSING), k)


def index_dir(path: str, split_slot: bool) -> dict[int, str]:
    """Map epoch -> file path, WITHOUT loading anything.

    dugite writes `<epoch>.json`; cardano-streamer writes `<epoch>-<slot>.json`.
    Loading every epoch up front is not an option: mainnet dumps reach 416 MB
    each and 8.5 GB in total.
    """
    out: dict[int, str] = {}
    for fn in sorted(os.listdir(path)):
        if not fn.endswith(".json"):
            continue
        stem = fn[:-5].split("-")[0] if split_slot else fn[:-5]
        if not stem.isdigit():
            continue
        out[int(stem)] = os.path.join(path, fn)
    return out


def load_summary(path: str):
    """Load one epoch and immediately reduce it to a comparable summary.

    The big tree is dropped before the caller loads the other side, so peak
    memory is one epoch rather than two.
    """
    with open(path) as fh:
        return summarise(json.load(fh))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dugite", required=True)
    ap.add_argument("--cstreamer", required=True)
    ap.add_argument("--json", help="write a machine-readable report here")
    ap.add_argument("--max-examples", type=int, default=6)
    ap.add_argument("--progress", action="store_true",
                    help="print each epoch as it is compared")
    args = ap.parse_args()

    dug_files = index_dir(args.dugite, split_slot=False)
    cst_files = index_dir(args.cstreamer, split_slot=True)
    res = Result()

    for epoch in sorted(set(dug_files) | set(cst_files)):
        dpath, cpath = dug_files.get(epoch), cst_files.get(epoch)
        if dpath is not None and cpath is None:
            # Byron is EXPECTED to be oracle-silent: cardano-streamer refuses to
            # dump it because the Shelley pots do not exist there. Read only the
            # era rather than the whole file — a Byron epoch can still be large.
            with open(dpath) as fh:
                era = json.load(fh).get("snapshotEraName", "")
            (res.oracle_silent if era == "Byron" else res.unpaired_dugite).append(epoch)
            continue
        if cpath is not None and dpath is None:
            res.unpaired_cstreamer.append(epoch)
            continue
        if args.progress:
            print(f"  comparing epoch {epoch} …", file=sys.stderr, flush=True)
        d = load_summary(dpath)
        c = load_summary(cpath)
        compare_epoch(res, epoch, d, c)
        del d, c

    paired = sorted(set(dug_files) & set(cst_files))
    print("=" * 74)
    print("dugite vs cardano-streamer — per-epoch ledger-state comparison")
    print("=" * 74)
    print(f"dugite epochs         : {len(dug_files)}")
    print(f"cstreamer epochs      : {len(cst_files)}")
    print(f"paired and compared   : {len(paired)}"
          + (f"  (epochs {paired[0]}..{paired[-1]})" if paired else ""))
    print(f"Byron (oracle-silent) : {len(res.oracle_silent)}"
          "  [expected — no Shelley pots in Byron]")
    print(f"LEAF COMPARISONS MADE : {res.comparisons}")
    print()

    if res.unpaired_dugite:
        print(f"!! {len(res.unpaired_dugite)} NON-Byron epochs dugite dumped and "
              f"cstreamer did not: {res.unpaired_dugite[:10]}")
    if res.unpaired_cstreamer:
        print(f"!! {len(res.unpaired_cstreamer)} epochs cstreamer dumped and "
              f"dugite did not: {res.unpaired_cstreamer[:10]}")
    if res.unpaired_dugite or res.unpaired_cstreamer:
        print()

    all_paths = sorted(set(res.match) | set(res.diff)
                       | set(res.vacuous) | set(res.absent_one))
    print(f"{'field path':<46}{'match':>8}{'diff':>7}{'vac':>6}{'abs1':>6}")
    print("-" * 74)
    # `.get`, never `[]` — these are defaultdicts and indexing here would
    # auto-vivify an entry for every path, which then reads downstream as a
    # real observation. The negative self-test caught exactly that.
    for p in all_paths:
        print(f"{p[:45]:<46}{res.match.get(p, 0):>8}{len(res.diff.get(p, [])):>7}"
              f"{res.vacuous.get(p, 0):>6}{len(res.absent_one.get(p, [])):>6}")
    print()

    rc = 0

    real_gaps = {p: v for p, v in res.absent_one.items() if v}
    if real_gaps:
        rc = 2
        print("SCHEMA GAPS — a key expected in its era was absent on one side:")
        for p, entries in sorted(real_gaps.items()):
            eps = sorted({e for e, _ in entries})
            sides = sorted({s for _, s in entries})
            print(f"  {p}: missing from {'/'.join(sides)}, "
                  f"{len(eps)} epochs, first={eps[0]}")
        print()

    real_diffs = {p: v for p, v in res.diff.items() if v}
    if real_diffs:
        rc = 1
        print("DIVERGENCES — bisected to the FIRST epoch per field path:")
        ordered = sorted(real_diffs.items(), key=lambda kv: min(e for e, _, _ in kv[1]))
        for p, entries in ordered[: args.max_examples * 4]:
            entries.sort(key=lambda t: t[0])
            first_epoch, a, b = entries[0]
            print(f"  {p}: {len(entries)} epochs, FIRST at epoch {first_epoch}")
            print(f"      dugite    = {str(a)[:160]}")
            print(f"      cstreamer = {str(b)[:160]}")
        if len(ordered) > args.max_examples * 4:
            print(f"  … and {len(ordered) - args.max_examples * 4} more paths "
                  f"(full detail in --json)")
        print()
        earliest = min(min(e for e, _, _ in v) for v in real_diffs.values())
        print(f"EARLIEST DIVERGENT EPOCH: {earliest}")
        print()

    if res.comparisons == 0:
        print("VACUOUS: nothing was compared. A green result here would mean nothing.")
        rc = 3
    elif rc == 0:
        print(f"PASS — {res.comparisons} leaf comparisons across {len(paired)} epochs, "
              "0 divergent, 0 schema gaps.")

    def has_leaf_evidence(parent: str) -> bool:
        """Did any leaf UNDER `parent` actually get compared?

        A parent path is counted vacuous only when the whole sub-object was
        null on both sides, which is normal for the early epochs. Listing
        `snapshots.mark` as "no evidence" while its leaves matched at 60 later
        epochs would UNDERSTATE coverage — the mirror of the overstatement this
        report exists to prevent, and just as misleading.
        """
        pre = parent + "."
        return any(k.startswith(pre) and (res.match.get(k, 0) or res.diff.get(k))
                   for k in all_paths)

    only_vacuous = [p for p in all_paths
                    if res.match.get(p, 0) == 0 and not res.diff.get(p)
                    and res.vacuous.get(p, 0) > 0 and not has_leaf_evidence(p)]
    if only_vacuous:
        print(f"\nNOTE — null on BOTH sides at every epoch, so no evidence was "
              f"gathered for: {', '.join(only_vacuous[:12])}"
              + (" …" if len(only_vacuous) > 12 else ""))

    if args.json:
        with open(args.json, "w") as fh:
            json.dump({
                "dugite_epochs": len(dug_files),
                "cstreamer_epochs": len(cst_files),
                "paired": len(paired),
                "byron_oracle_silent": len(res.oracle_silent),
                "leaf_comparisons": res.comparisons,
                "match": dict(res.match),
                "diff": {p: [{"epoch": e, "dugite": a, "cstreamer": b}
                             for e, a, b in v] for p, v in real_diffs.items()},
                "vacuous": dict(res.vacuous),
                "absent_one_side": {p: v for p, v in real_gaps.items()},
                "unpaired_dugite_non_byron": res.unpaired_dugite,
                "unpaired_cstreamer": res.unpaired_cstreamer,
                "exit_code": rc,
            }, fh, indent=2, default=str)
        print(f"\nreport written to {args.json}")

    return rc


if __name__ == "__main__":
    sys.exit(main())
