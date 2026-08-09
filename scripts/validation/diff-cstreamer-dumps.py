#!/usr/bin/env python3
"""Diff dugite's `dump-snapshot` output against cardano-streamer's, epoch by epoch.

    scripts/validation/diff-cstreamer-dumps.py \
        --dugite  reports/mainnet-exactness/dugite \
        --cstreamer reports/mainnet-exactness/cstreamer \
        [--first-divergence-only] [--json report.json]

Both sides dump at the SAME instant — the first block of the new epoch, after
every boundary transition has been applied, labelled with the NEW epoch.
cardano-streamer's README states this outright ("all fields reflect the
post-boundary state") and dugite's `run_dump_snapshot` writes post-`apply_block`
when `current_epoch > last_epoch`. Verify that before trusting any output here:
different instants inside one boundary make every epoch look divergent for a
reason that is not a bug.

WHY THIS EXISTS RATHER THAN A SINGLE END-STATE COMPARISON
---------------------------------------------------------
A single number at the tip cannot distinguish a step change from a slow drift,
and those have completely different causes. #1073's predecessor investigation
burned ten hypotheses on one tail number. This bisects: it reports the FIRST
epoch at which each field diverges.

ERA AWARENESS
-------------
Eras carry different ledger information, so a field's absence is only
meaningful relative to its era:

  * cardano-streamer emits NOTHING for Byron epochs — `buildSnapshotJson`
    returns `Nothing` there, because `ChainAccountState` (treasury/reserves) is
    introduced BY the Shelley translation and does not exist in Byron. Byron
    epochs are therefore reported as ORACLE-SILENT, never as divergent. Any
    tool that back-projects a Shelley shape onto Byron is modelling, not
    reporting — that is precisely what disqualified Koios as an oracle.
  * `conwayGov` is null before Conway.
  * `instantaneousRewards` is Shelley-Babbage only (always empty in Conway+).
  * `epochNonce` is null for Byron and for the neutral nonce.

A field expected in an era and MISSING on one side is a hard failure, not a
skip. A field present on both but null on both is counted as VACUOUS and
reported separately — two implementations agreeing that there is nothing to
say is not evidence of agreement.

EXIT CODES
----------
  0  every compared field matched, and the comparison was non-vacuous
  1  at least one field diverged
  2  a schema gap: a field expected in its era was absent on one side
  3  the comparison was vacuous (nothing was actually compared)
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections import defaultdict

# ── Era model ────────────────────────────────────────────────────────────
#
# Ordered oldest-first. `snapshotEraName` is compared directly as a field, so
# this table only decides which fields are APPLICABLE, never what is correct.
ERA_ORDER = [
    "Byron",
    "Shelley",
    "Allegra",
    "Mary",
    "Alonzo",
    "Babbage",
    "Conway",
    "Dijkstra",
]


def era_index(name: str) -> int:
    try:
        return ERA_ORDER.index(name)
    except ValueError:
        return -1


def applicable(field: str, era: str) -> bool:
    """Is `field` expected to carry a value in `era`?

    Only genuinely era-scoped fields appear here; everything else is expected
    in every era cardano-streamer dumps at all (i.e. Shelley onward).
    """
    i = era_index(era)
    if field == "conwayGov":
        return i >= era_index("Conway")
    if field == "instantaneousRewards":
        # Shelley-Babbage; always empty in Conway+.
        return era_index("Shelley") <= i <= era_index("Babbage")
    if field == "epochNonce":
        # Present from Shelley on, but legitimately null for the neutral nonce,
        # which `compare_scalar` treats as an agreed absence.
        return i >= era_index("Shelley")
    return True


# ── Fields compared ──────────────────────────────────────────────────────
#
# Scalars are compared for exact equality. These are the reward/pot/stake
# quantities the dataset exists to validate.
SCALAR_FIELDS = [
    "epoch",
    "snapshotEraName",
    "treasury",
    "reserves",
    "totalStake",
    "activeStake",
    "totalPools",
    "epochFees",
    "expectedBlocks",
    "epochNonce",
]

# Nested objects compared leaf-by-leaf.
NESTED_FIELDS = {
    "rupdNext": [
        "deltaR1",
        "deltaR2",
        "deltaT1",
        "rPot",
        "rewardPot",
        "totalDistributed",
    ],
    "protocolParams": ["rho", "tau", "d", "a0", "nOpt", "minPoolCost", "protocolVersion"],
    "deposits": ["stakeKey", "pool", "dRep", "proposal", "total"],
    "eta": ["numerator", "denominator"],
}


class Result:
    def __init__(self) -> None:
        self.match = defaultdict(int)
        self.diff = defaultdict(list)          # field -> [(epoch, a, b)]
        self.absent_one = defaultdict(list)    # field -> [(epoch, which)]
        self.vacuous = defaultdict(int)        # field -> count of null==null
        self.oracle_silent: list[int] = []     # Byron epochs, expected
        self.unpaired_dugite: list[int] = []
        self.unpaired_cstreamer: list[int] = []

    @property
    def comparisons(self) -> int:
        return sum(self.match.values()) + sum(len(v) for v in self.diff.values())


def norm(v):
    """Normalise for comparison.

    Rationals are compared by VALUE, not by representation: cardano-streamer
    reduces to simplest form and dugite does too, but a future change on
    either side reducing differently must not read as a ledger divergence.
    """
    if isinstance(v, dict) and set(v.keys()) == {"numerator", "denominator"}:
        n, d = v["numerator"], v["denominator"]
        if isinstance(n, int) and isinstance(d, int) and d != 0:
            from math import gcd

            g = gcd(abs(n), abs(d)) or 1
            return ("rational", n // g, d // g)
    return v


def compare_leaf(res: Result, epoch: int, era: str, name: str, a, b) -> None:
    if not applicable(name.split(".")[0], era):
        return
    a_missing, b_missing = a is _MISSING, b is _MISSING
    if a_missing and b_missing:
        return
    if a_missing or b_missing:
        res.absent_one[name].append((epoch, "dugite" if a_missing else "cstreamer"))
        return
    if a is None and b is None:
        res.vacuous[name] += 1
        return
    if norm(a) == norm(b):
        res.match[name] += 1
    else:
        res.diff[name].append((epoch, a, b))


_MISSING = object()


def get(d: dict, key: str):
    return d.get(key, _MISSING) if isinstance(d, dict) else _MISSING


def compare_epoch(res: Result, epoch: int, dug: dict, cst: dict) -> None:
    # Era is taken from the ORACLE where available: it decides applicability,
    # and using dugite's own value would let a wrong era on dugite's side
    # silently switch off the very fields that would have exposed it.
    era = cst.get("snapshotEraName") or dug.get("snapshotEraName") or "Shelley"

    for f in SCALAR_FIELDS:
        compare_leaf(res, epoch, era, f, get(dug, f), get(cst, f))

    for parent, leaves in NESTED_FIELDS.items():
        da, ca = get(dug, parent), get(cst, parent)
        # A whole sub-object null on both sides is one vacuous observation,
        # not N — otherwise a schema nobody populates inflates the denominator.
        if (da is None or da is _MISSING) and (ca is None or ca is _MISSING):
            if da is None and ca is None:
                res.vacuous[parent] += 1
            continue
        if da is None or da is _MISSING or ca is None or ca is _MISSING:
            res.absent_one[parent].append(
                (epoch, "dugite" if da in (None, _MISSING) else "cstreamer")
            )
            continue
        for leaf in leaves:
            compare_leaf(res, epoch, era, f"{parent}.{leaf}", get(da, leaf), get(ca, leaf))


def load_dugite(path: str) -> dict[int, dict]:
    out = {}
    for fn in os.listdir(path):
        if not fn.endswith(".json"):
            continue
        stem = fn[:-5]
        if not stem.isdigit():
            continue
        with open(os.path.join(path, fn)) as fh:
            out[int(stem)] = json.load(fh)
    return out


def load_cstreamer(path: str) -> dict[int, dict]:
    """cardano-streamer writes `<epoch>-<slot>.json`."""
    out = {}
    for fn in os.listdir(path):
        if not fn.endswith(".json"):
            continue
        stem = fn[:-5]
        epoch_part = stem.split("-")[0]
        if not epoch_part.isdigit():
            continue
        with open(os.path.join(path, fn)) as fh:
            out[int(epoch_part)] = json.load(fh)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dugite", required=True)
    ap.add_argument("--cstreamer", required=True)
    ap.add_argument("--json", help="write a machine-readable report here")
    ap.add_argument(
        "--first-divergence-only",
        action="store_true",
        help="print only the first divergent epoch per field",
    )
    args = ap.parse_args()

    dug = load_dugite(args.dugite)
    cst = load_cstreamer(args.cstreamer)
    res = Result()

    for epoch in sorted(set(dug) | set(cst)):
        d, c = dug.get(epoch), cst.get(epoch)
        if d is not None and c is None:
            era = d.get("snapshotEraName", "")
            # Byron is EXPECTED to be oracle-silent: cardano-streamer refuses
            # to dump it because the Shelley pots do not exist there.
            (res.oracle_silent if era == "Byron" else res.unpaired_dugite).append(epoch)
            continue
        if c is not None and d is None:
            res.unpaired_cstreamer.append(epoch)
            continue
        compare_epoch(res, epoch, d, c)

    # ── Report ──────────────────────────────────────────────────────────
    print("=" * 72)
    print("dugite vs cardano-streamer — per-epoch ledger-state comparison")
    print("=" * 72)
    print(f"dugite epochs        : {len(dug)}")
    print(f"cstreamer epochs     : {len(cst)}")
    print(f"paired and compared  : {len(set(dug) & set(cst))}")
    print(f"Byron (oracle-silent): {len(res.oracle_silent)}  [expected — no Shelley pots in Byron]")
    print(f"leaf comparisons MADE: {res.comparisons}")
    print()

    if res.unpaired_dugite:
        print(f"!! {len(res.unpaired_dugite)} non-Byron epochs dugite dumped and cstreamer did not: "
              f"{res.unpaired_dugite[:10]}")
    if res.unpaired_cstreamer:
        print(f"!! {len(res.unpaired_cstreamer)} epochs cstreamer dumped and dugite did not: "
              f"{res.unpaired_cstreamer[:10]}")

    print("field                              match     diff  vacuous  absent-1")
    print("-" * 72)
    all_fields = sorted(set(res.match) | set(res.diff) | set(res.vacuous) | set(res.absent_one))
    # `.get`, never `[]` — these are defaultdicts, and indexing them here would
    # auto-vivify an empty entry for every field, which then reads downstream as
    # a real observation. The negative self-test caught exactly that.
    for f in all_fields:
        print(f"{f:<34}{res.match.get(f, 0):>7}{len(res.diff.get(f, [])):>9}"
              f"{res.vacuous.get(f, 0):>9}{len(res.absent_one.get(f, [])):>10}")
    print()

    rc = 0

    real_gaps = {f: v for f, v in res.absent_one.items() if v}
    if real_gaps:
        rc = 2
        print("SCHEMA GAPS — a field expected in its era was absent on one side:")
        for f, entries in sorted(real_gaps.items()):
            eps = sorted({e for e, _ in entries})
            sides = sorted({s for _, s in entries})
            print(f"  {f}: missing from {'/'.join(sides)} at {len(eps)} epochs, first={eps[0]}")
        print()

    real_diffs = {f: v for f, v in res.diff.items() if v}
    if real_diffs:
        rc = 1
        print("DIVERGENCES — bisected to the FIRST epoch per field:")
        for f, entries in sorted(real_diffs.items(), key=lambda kv: min(e for e, _, _ in kv[1])):
            entries.sort(key=lambda t: t[0])
            first_epoch, a, b = entries[0]
            print(f"  {f}: {len(entries)} epochs, FIRST at epoch {first_epoch}")
            print(f"      dugite    = {a}")
            print(f"      cstreamer = {b}")
            if not args.first_divergence_only and len(entries) > 1:
                print(f"      also: {[e for e, _, _ in entries[1:6]]}"
                      f"{' ...' if len(entries) > 6 else ''}")
        print()

    if res.comparisons == 0:
        print("VACUOUS: nothing was compared. A green result here would mean nothing.")
        rc = 3
    elif rc == 0:
        print(f"PASS — {res.comparisons} leaf comparisons, 0 divergent, 0 schema gaps.")

    # Fields that were ONLY ever vacuous are called out: they contributed no
    # evidence, and a summary that hides them reads as coverage it does not have.
    def has_leaf_evidence(parent: str) -> bool:
        """Did any leaf under `parent` actually get compared?

        A parent key is only counted vacuous when the WHOLE sub-object was null
        on both sides, which is normal for pre-Shelley epochs. Listing the
        parent as "no evidence" while its leaves matched 62 times would
        understate the coverage — the mirror of the overstatement this report
        exists to prevent.
        """
        pre = parent + "."
        return any(k.startswith(pre) and (res.match.get(k, 0) or res.diff.get(k))
                   for k in all_fields)

    only_vacuous = [f for f in all_fields
                    if res.match.get(f, 0) == 0 and not res.diff.get(f)
                    and res.vacuous.get(f, 0) > 0 and not has_leaf_evidence(f)]
    if only_vacuous:
        print(f"\nNOTE — never non-null on either side (no evidence gathered): "
              f"{', '.join(only_vacuous)}")

    if args.json:
        with open(args.json, "w") as fh:
            json.dump(
                {
                    "dugite_epochs": len(dug),
                    "cstreamer_epochs": len(cst),
                    "paired": len(set(dug) & set(cst)),
                    "byron_oracle_silent": len(res.oracle_silent),
                    "comparisons": res.comparisons,
                    "match": dict(res.match),
                    "diff": {f: [{"epoch": e, "dugite": a, "cstreamer": b}
                                 for e, a, b in v] for f, v in res.diff.items()},
                    "vacuous": dict(res.vacuous),
                    "absent_one_side": {f: v for f, v in res.absent_one.items()},
                    "unpaired_dugite_non_byron": res.unpaired_dugite,
                    "unpaired_cstreamer": res.unpaired_cstreamer,
                    "exit_code": rc,
                },
                fh,
                indent=2,
                default=str,
            )
        print(f"\nreport written to {args.json}")

    return rc


if __name__ == "__main__":
    sys.exit(main())
