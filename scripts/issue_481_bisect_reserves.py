#!/usr/bin/env python3
"""Bisect the first epoch where dugite.reserves_pre_rupd diverges from
Koios /totals for issue #481.

Reads dump-snapshot output under reward-dumps-issue-481/ and queries
the Koios preview /totals endpoint per epoch.  Prints:
  - per-epoch dugite/koios/diff
  - first nonzero divergence epoch B
  - per-epoch increment (delta from previous epoch's diff) to locate
    step-change events
  - confirms prev_protocol_version_major bumps correctly across PV9/PV10

Usage:
  python3 scripts/issue_481_bisect_reserves.py [DUMP_DIR]

If DUMP_DIR is omitted, defaults to reward-dumps-issue-481.
"""
from __future__ import annotations
import json
import pathlib
import sys
import time
import urllib.request
import urllib.error

DEFAULT_DUMP_DIR = pathlib.Path("reward-dumps-issue-481")
KOIOS_URL = "https://preview.koios.rest/api/v1/totals"


def fetch_koios_reserves(epoch: int, retries: int = 3) -> int | None:
    url = f"{KOIOS_URL}?_epoch_no={epoch}"
    req = urllib.request.Request(url, headers={"Accept": "application/json"})
    for attempt in range(retries):
        try:
            with urllib.request.urlopen(req, timeout=15) as r:
                data = json.loads(r.read())
                if not data:
                    return None
                return int(data[0]["reserves"])
        except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError) as e:
            if attempt == retries - 1:
                print(f"  ! koios fetch failed e{epoch}: {e}", file=sys.stderr)
                return None
            time.sleep(1.5 ** attempt)
    return None


def main(argv: list[str]) -> int:
    dump_dir = pathlib.Path(argv[1]) if len(argv) > 1 else DEFAULT_DUMP_DIR
    if not dump_dir.is_dir():
        print(f"error: dump dir {dump_dir} not found", file=sys.stderr)
        return 2

    dumps = sorted(dump_dir.glob("epoch_*_to_*.json"))
    print(f"Loaded {len(dumps)} dumps from {dump_dir}")
    print()

    # Phase 1: sparse sweep — confirm overall shape.
    sparse = [0, 100, 200, 300, 400, 500, 600, 646, 700, 743, 800, 900,
              1000, 1050, 1100, 1150, 1200, 1250, 1268, 1270, 1290]
    print(f"=== Sparse sweep ({len(sparse)} epochs) ===")
    print(f"{'epoch':>5}  {'pv':>3}  {'dugite_reserves':>20}  {'koios_reserves':>20}  "
          f"{'diff':>15}")
    print("-" * 80)

    diffs: dict[int, int] = {}
    pvs: dict[int, int] = {}
    for epoch in sparse:
        p = dump_dir / f"epoch_{epoch:06d}_to_{epoch + 1:06d}.json"
        if not p.exists():
            continue
        d = json.loads(p.read_text())
        s = d["scalars"]
        dugite = s["reserves_pre_rupd"]
        pv = s["prev_protocol_version_major"]
        koios = fetch_koios_reserves(epoch)
        if koios is None:
            print(f"{epoch:>5}  {pv:>3}  {dugite:>20,}  {'N/A':>20}  {'?':>15}")
            continue
        diff = dugite - koios
        diffs[epoch] = diff
        pvs[epoch] = pv
        marker = " ✓" if diff == 0 else (f"  ({'+' if diff > 0 else ''}{diff:,})")
        print(f"{epoch:>5}  {pv:>3}  {dugite:>20,}  {koios:>20,}  {diff:>+15,}")

    print()
    print("=== Increments between sparse sample points ===")
    eps = sorted(diffs.keys())
    for prev, curr in zip(eps[:-1], eps[1:]):
        delta = diffs[curr] - diffs[prev]
        flag = ""
        if abs(delta) > 1_000_000_000:
            flag = " *** STEP ***"
        elif abs(delta) > 100_000_000:
            flag = " * jump *"
        print(f"  e{prev:>4} → e{curr:>4}: Δdiff = {delta:>+18,}{flag}")

    print()
    print("=== PV transitions detected ===")
    for prev, curr in zip(eps[:-1], eps[1:]):
        if pvs[prev] != pvs[curr]:
            print(f"  e{prev} (pv={pvs[prev]}) → e{curr} (pv={pvs[curr]})")

    # Phase 2: tight bisection around identified step regions.
    interesting_regions = []
    for prev, curr in zip(eps[:-1], eps[1:]):
        delta = diffs[curr] - diffs[prev]
        if abs(delta) > 100_000_000:
            interesting_regions.append((prev, curr))

    if interesting_regions:
        print()
        print(f"=== Tight bisection of {len(interesting_regions)} interesting region(s) ===")
        for lo, hi in interesting_regions:
            print(f"\n--- region e{lo}..e{hi} ---")
            mid = (lo + hi) // 2
            checkpoints = sorted({lo, lo + (mid - lo) // 2, mid,
                                  mid + (hi - mid) // 2, hi})
            for epoch in checkpoints:
                if epoch in diffs:
                    continue
                p = dump_dir / f"epoch_{epoch:06d}_to_{epoch + 1:06d}.json"
                if not p.exists():
                    continue
                d = json.loads(p.read_text())
                s = d["scalars"]
                dugite = s["reserves_pre_rupd"]
                pv = s["prev_protocol_version_major"]
                koios = fetch_koios_reserves(epoch)
                if koios is None:
                    continue
                diff = dugite - koios
                diffs[epoch] = diff
                pvs[epoch] = pv
                print(f"  e{epoch:>4}  pv={pv}  diff={diff:>+18,}")

    # Phase 3: hunt for first divergence.
    print()
    print("=== First-divergence hunt ===")
    if all(v == 0 for v in diffs.values() if v is not None):
        print("  No divergence detected across sampled epochs — drift may be resolved.")
        return 0

    first_nonzero = min((e for e, v in diffs.items() if v != 0), default=None)
    if first_nonzero is None:
        print("  No first divergence found.")
        return 0
    print(f"  First nonzero in sparse/tight sweep: e{first_nonzero}")

    # Scan all epochs between e0 and first_nonzero to find the EXACT boundary.
    print(f"  Scanning every epoch in [0, {first_nonzero}] for tighter bound...")
    print(f"  (this hits Koios {first_nonzero} times — may take a few minutes)")
    print()
    earliest = first_nonzero
    for epoch in range(0, first_nonzero):
        if epoch in diffs:
            continue
        p = dump_dir / f"epoch_{epoch:06d}_to_{epoch + 1:06d}.json"
        if not p.exists():
            continue
        d = json.loads(p.read_text())
        s = d["scalars"]
        dugite = s["reserves_pre_rupd"]
        koios = fetch_koios_reserves(epoch)
        if koios is None:
            continue
        diff = dugite - koios
        diffs[epoch] = diff
        if diff != 0 and epoch < earliest:
            earliest = epoch
            pv = s["prev_protocol_version_major"]
            print(f"  e{epoch}  pv={pv}  FIRST divergence: diff={diff:+,}")
            break
        # Throttle: 5 reqs/sec at most.
        time.sleep(0.2)

    print()
    print(f"=== Final result ===")
    print(f"  First divergence epoch B = {earliest}")
    print(f"  Sign of B's diff:        {'positive' if diffs.get(earliest, 0) > 0 else 'negative'}")
    print(f"  Magnitude:               {abs(diffs.get(earliest, 0)):,} lovelace")

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
