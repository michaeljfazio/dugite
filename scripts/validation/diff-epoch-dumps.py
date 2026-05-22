#!/usr/bin/env python3
"""Diff per-epoch canonical ledger-state dumps between Haskell and dugite.

Usage:
    diff-epoch-dumps.py \\
        --haskell-dir ./epoch-dumps-haskell \\
        --dugite-dir  ./epoch-dumps-dugite \\
        --from-epoch 1 --to-epoch 10 \\
        [--tolerance-config tolerance.yaml] \\
        [--report-json report.json] \\
        [--report-md   report.md]

Produces a human-readable report with:
  - Per-epoch divergence count
  - Top-10 most-divergent fields
  - Class buckets (rewards / governance / stake / utxo / nonce / pp / era)

Exit code 0 iff no `severity = high` divergences are flagged after
tolerance is applied.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any


# Import sibling normalizer without requiring it to be on sys.path.
_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "normalize_epoch_dump", _HERE / "normalize-epoch-dump.py"
)
assert _SPEC and _SPEC.loader
_NORM = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_NORM)
normalize_haskell = _NORM.normalize_haskell  # type: ignore[attr-defined]
normalize_dugite = _NORM.normalize_dugite  # type: ignore[attr-defined]


# Severity / bucket classification keyed by top-level canonical field.
BUCKET_BY_PREFIX: dict[str, str] = {
    "rewards": "rewards",
    "governance": "governance",
    "stake_snapshot": "stake",
    "pools": "stake",
    "utxo": "utxo",
    "nonce": "nonce",
    "pp_current": "pp",
    "pp_previous": "pp",
    "pp_future": "pp",
    "era": "era",
    "protocol_version": "era",
    "scalars": "rewards",  # treasury/reserves/fees are reward-class
    "slot": "era",
    "epoch": "era",
}


def classify(field_path: str) -> str:
    prefix = field_path.split(".", 1)[0]
    return BUCKET_BY_PREFIX.get(prefix, "other")


# ── Tolerance config ──────────────────────────────────────────────────


def load_tolerance(path: Path | None) -> dict[str, Any]:
    if path is None:
        return {}
    try:
        import yaml  # type: ignore[import-not-found]
    except ImportError:
        print(
            "tolerance config requires PyYAML; install with `pip install pyyaml`",
            file=sys.stderr,
        )
        sys.exit(2)
    with path.open("r", encoding="utf-8") as f:
        cfg = yaml.safe_load(f) or {}
    return cfg


def within_tolerance(
    field: str, h_val: Any, d_val: Any, cfg: dict[str, Any]
) -> bool:
    """Return True iff the difference is whitelisted by the tolerance
    config.
    """
    rule = cfg.get(field)
    if rule is None:
        return False
    if rule == "ignore":
        return True
    if isinstance(rule, dict):
        if rule.get("ignore"):
            return True
        if "abs_tol" in rule and isinstance(h_val, (int, float)) and isinstance(
            d_val, (int, float)
        ):
            return abs(h_val - d_val) <= rule["abs_tol"]
        if "set_eq" in rule and rule["set_eq"]:
            try:
                return set(h_val) == set(d_val)
            except TypeError:
                return False
    return False


# ── Field walking ─────────────────────────────────────────────────────


def walk(obj: Any, prefix: str = "") -> list[tuple[str, Any]]:
    """Flatten a nested dict to (dotted_path, leaf_value) pairs."""
    out: list[tuple[str, Any]] = []
    if isinstance(obj, dict):
        for k, v in sorted(obj.items()):
            out.extend(walk(v, f"{prefix}.{k}" if prefix else k))
    else:
        out.append((prefix, obj))
    return out


def diff_records(
    epoch: int,
    haskell: dict,
    dugite: dict,
    tolerance: dict[str, Any],
) -> list[dict]:
    h_flat = dict(walk(haskell))
    d_flat = dict(walk(dugite))
    keys = sorted(set(h_flat.keys()) | set(d_flat.keys()))
    out: list[dict] = []
    for k in keys:
        h_val = h_flat.get(k, "<MISSING>")
        d_val = d_flat.get(k, "<MISSING>")
        if h_val == d_val:
            continue
        if within_tolerance(k, h_val, d_val, tolerance):
            continue
        severity = "high"
        # Demote a few classes to "info" by default: synthetic fields
        # and Haskell-side-missing structural defaults.
        if k == "governance.committee_hash":
            severity = "info"
        if k.startswith("pp_future"):
            severity = "info"
        out.append(
            {
                "epoch": epoch,
                "field_path": k,
                "haskell_value": h_val,
                "dugite_value": d_val,
                "bucket": classify(k),
                "severity": severity,
            }
        )
    return out


# ── Loaders ───────────────────────────────────────────────────────────


def load_dump(dir_: Path, epoch: int, source: str) -> dict | None:
    path = dir_ / f"epoch_{epoch:06d}.json"
    if not path.exists():
        return None
    with path.open("r", encoding="utf-8") as f:
        record = json.load(f)
    if source == "haskell":
        return normalize_haskell(record)
    return normalize_dugite(record)


# ── Reporting ─────────────────────────────────────────────────────────


def render_markdown(records: list[dict], from_e: int, to_e: int) -> str:
    by_epoch: dict[int, int] = defaultdict(int)
    by_bucket: dict[str, int] = defaultdict(int)
    by_field: dict[str, int] = defaultdict(int)
    for r in records:
        if r["severity"] == "info":
            continue
        by_epoch[r["epoch"]] += 1
        by_bucket[r["bucket"]] += 1
        by_field[r["field_path"]] += 1

    lines: list[str] = []
    lines.append(f"# Epoch ledger-state diff report (epochs {from_e}..{to_e})")
    lines.append("")
    lines.append(f"Total non-info divergences: **{sum(by_epoch.values())}**")
    lines.append("")
    lines.append("## Per-epoch divergence count")
    lines.append("")
    lines.append("| Epoch | Diffs |")
    lines.append("| ----: | ----: |")
    for e in range(from_e, to_e + 1):
        lines.append(f"| {e} | {by_epoch.get(e, 0)} |")
    lines.append("")
    lines.append("## Top-10 most-divergent fields")
    lines.append("")
    lines.append("| Field | Count |")
    lines.append("| --- | ---: |")
    for field, count in sorted(by_field.items(), key=lambda x: -x[1])[:10]:
        lines.append(f"| `{field}` | {count} |")
    lines.append("")
    lines.append("## Bucket totals")
    lines.append("")
    lines.append("| Bucket | Diffs |")
    lines.append("| --- | ---: |")
    for bucket, count in sorted(by_bucket.items(), key=lambda x: -x[1]):
        lines.append(f"| {bucket} | {count} |")
    lines.append("")
    lines.append("## Sample divergences (first 50)")
    lines.append("")
    lines.append("| Epoch | Field | Haskell | Dugite | Bucket |")
    lines.append("| ---: | --- | --- | --- | --- |")
    shown = 0
    for r in records:
        if r["severity"] == "info":
            continue
        if shown >= 50:
            break
        h = json.dumps(r["haskell_value"])
        d = json.dumps(r["dugite_value"])
        if len(h) > 60:
            h = h[:57] + "..."
        if len(d) > 60:
            d = d[:57] + "..."
        lines.append(
            f"| {r['epoch']} | `{r['field_path']}` | `{h}` | `{d}` | {r['bucket']} |"
        )
        shown += 1
    lines.append("")
    return "\n".join(lines)


def render_json(records: list[dict], from_e: int, to_e: int) -> str:
    by_bucket: Counter[str] = Counter()
    for r in records:
        if r["severity"] != "info":
            by_bucket[r["bucket"]] += 1
    summary = {
        "from_epoch": from_e,
        "to_epoch": to_e,
        "total_diffs": sum(by_bucket.values()),
        "by_bucket": dict(by_bucket),
        "diffs": records,
    }
    return json.dumps(summary, indent=2)


# ── Main ──────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--haskell-dir", type=Path, required=True)
    ap.add_argument("--dugite-dir", type=Path, required=True)
    ap.add_argument("--from-epoch", type=int, required=True)
    ap.add_argument("--to-epoch", type=int, required=True)
    ap.add_argument("--tolerance-config", type=Path)
    ap.add_argument("--report-json", type=Path)
    ap.add_argument("--report-md", type=Path)
    ap.add_argument(
        "--allow-missing",
        action="store_true",
        help="treat missing per-epoch files as a warning rather than an error",
    )
    args = ap.parse_args()

    tolerance = load_tolerance(args.tolerance_config)
    all_diffs: list[dict] = []
    high_count = 0

    for epoch in range(args.from_epoch, args.to_epoch + 1):
        h = load_dump(args.haskell_dir, epoch, "haskell")
        d = load_dump(args.dugite_dir, epoch, "dugite")
        if h is None or d is None:
            msg = f"[diff] missing dump for epoch {epoch} (haskell={h is not None}, dugite={d is not None})"
            if args.allow_missing:
                print(msg, file=sys.stderr)
                continue
            else:
                print(msg + " (use --allow-missing to skip)", file=sys.stderr)
                return 3
        diffs = diff_records(epoch, h, d, tolerance)
        all_diffs.extend(diffs)
        high_count += sum(1 for r in diffs if r["severity"] == "high")

    md = render_markdown(all_diffs, args.from_epoch, args.to_epoch)
    js = render_json(all_diffs, args.from_epoch, args.to_epoch)

    if args.report_md:
        args.report_md.write_text(md, encoding="utf-8")
    if args.report_json:
        args.report_json.write_text(js, encoding="utf-8")

    if not args.report_md and not args.report_json:
        print(md)

    return 0 if high_count == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
