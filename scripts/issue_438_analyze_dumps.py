#!/usr/bin/env python3
"""Analyze reward-debug dumps for issue #438 — compares dugite per-boundary
`rupd_credit` against a Koios oracle list of `account_reward_history`
entries.  Reports per-boundary delta, cumulative drift, and ratio.

Usage:
  # First save koios reward history to JSON (call koios account_reward_history
  # for stake_test1uz7xx6hy2xnnrmz0av0xl7qn9vdkhage7myf0nd49e7mvcg6z0smn,
  # all pages, save as [{"spendable_epoch":N,"amount":"M"}, ...])
  python3 scripts/issue_438_analyze_dumps.py /tmp/koios_rewards.json
"""
from __future__ import annotations
import json
import pathlib
import sys

DUMP_DIR = pathlib.Path("reward-dumps-issue-438")
OWNER_HEX = "bc636ae451a731ec4feb1e6ff8132b1b6bf519f6c897cdb52e7db661"
POOL_HEX = "a8e65680fe6a24a11d11f86b37f7c74e5c64a628b2256d8bcacbab52"


def load_koios(path: pathlib.Path) -> dict[int, int]:
    """Return {spendable_epoch: amount_lovelace}."""
    data = json.loads(path.read_text())
    return {int(r["spendable_epoch"]): int(r["amount"]) for r in data}


def load_dump(path: pathlib.Path) -> dict:
    return json.loads(path.read_text())


def find_owner_entry(dump: dict) -> dict | None:
    pool = next((p for p in dump["pools"] if p["pool_id_hex"] == POOL_HEX), None)
    if pool is None:
        return None
    for c in pool["credentials"]:
        if c["cred_hash_hex"] == OWNER_HEX and c["is_owner"]:
            return c
    return None


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    koios = load_koios(pathlib.Path(argv[1]))
    dumps = sorted(DUMP_DIR.glob("epoch_*_to_*.json"))

    print(f"{'epoch':>5}  {'dugite_credit':>15}  {'koios_amount':>15}  "
          f"{'delta':>10}  {'ratio_pct':>9}  {'cumulative':>14}  {'pool_R_overshoot':>17}")
    print("-" * 110)

    total_dugite = 0
    total_koios = 0
    first_diff = None
    cumulative_diff = 0
    diffs = []
    first_owner_stake = None

    for p in dumps:
        d = load_dump(p)
        epoch_to = d["epoch_to"]
        owner = find_owner_entry(d)
        if owner is None:
            continue
        credit = owner["rupd_credit"]
        bal_pre = owner["reward_balance_pre_rupd"]
        go_stake = owner["go_stake_distribution"]
        ko = koios.get(epoch_to)
        if credit == 0 and ko is None:
            continue
        koios_amt = ko if ko is not None else 0
        diff = credit - koios_amt
        if diff != 0 and first_diff is None and koios_amt > 0:
            first_diff = epoch_to
        cumulative_diff += diff
        total_dugite += credit
        total_koios += koios_amt
        # Back-derive pool_reward overshoot.
        # For pool with cost=340M, margin=1/20, pledge=0, owner_stake=s,
        # pool_stake=σ: leader_credit = cost + (R - cost) × (m + (1-m) × s/σ).
        # We need pool_stake which isn't in our flat dump format; for now
        # leave pool_R_overshoot empty and let downstream compute.
        ratio_pct = 100 * diff / max(credit, 1)
        diffs.append((epoch_to, credit, koios_amt, diff, ratio_pct, cumulative_diff, bal_pre, go_stake))

    # Print sample rows: first few, every-100th, and around the bug boundary.
    sample_epochs = set(range(12, 25)) | {50, 100, 200, 500, 800, 1000, 1100, 1200} | set(range(1260, 1275))
    for row in diffs:
        if row[0] in sample_epochs and row[2] > 0:
            print(f"{row[0]:>5}  {row[1]:>15,}  {row[2]:>15,}  {row[3]:>+10,}  "
                  f"{row[4]:>+8.4f}%  {row[5]:>+14,}")

    print("-" * 110)
    print(f"\nTotal boundaries with owner+koios data: {sum(1 for r in diffs if r[2] > 0)}")
    print(f"First nonzero diff at epoch: {first_diff}")
    print(f"Sum dugite credits:  {total_dugite:>20,}")
    print(f"Sum koios amounts:   {total_koios:>20,}")
    print(f"Cumulative overshoot: {total_dugite - total_koios:>+18,}")

    nonzero = [r for r in diffs if r[2] > 0]
    if nonzero:
        pos = [r for r in nonzero if r[3] > 0]
        neg = [r for r in nonzero if r[3] < 0]
        zero = [r for r in nonzero if r[3] == 0]
        print(f"\nBoundaries with valid comparison: {len(nonzero)}")
        print(f"  Positive diff (dugite > koios): {len(pos)}")
        print(f"  Negative diff (dugite < koios): {len(neg)}")
        print(f"  Zero diff: {len(zero)}")
        if pos:
            print(f"\nLargest +ve diff: epoch {max(pos, key=lambda r: r[3])[0]} = {max(r[3] for r in pos):+,}")
        if neg:
            print(f"Largest -ve diff: epoch {min(neg, key=lambda r: r[3])[0]} = {min(r[3] for r in neg):+,}")

        # Ratio statistics — the key insight: is the overshoot a constant ratio?
        print("\nRatio (delta/credit) statistics across all boundaries with both values:")
        ratios = [r[4] for r in nonzero]
        ratios.sort()
        print(f"  min:    {min(ratios):+.4f}%")
        print(f"  max:    {max(ratios):+.4f}%")
        print(f"  median: {ratios[len(ratios) // 2]:+.4f}%")
        print(f"  mean:   {sum(ratios) / len(ratios):+.4f}%")

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
