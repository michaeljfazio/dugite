#!/usr/bin/env python3
"""Diff per-epoch canonical ledger-state dumps between Haskell and dugite.

Usage:
    diff-epoch-dumps.py \\
        --haskell-dir ./epoch-dumps-haskell \\
        --dugite-dir  ./epoch-dumps-dugite \\
        --from-epoch 1 --to-epoch 10 \\
        [--tolerance-config tolerance.yaml] \\
        [--report-json report.json] \\
        [--report-md   report.md] \\
        [--strict]

Produces a human-readable report with:
  - Per-epoch divergence count (real divergences only)
  - Top-10 most-divergent fields
  - Class buckets (rewards / governance / stake / utxo / nonce / pp / era)
  - Separate "Haskell-uncoverable" section for fields cn 11.0.1's
    `debug log-epoch-state` cannot supply (see EPOCH_DIFF.md and
    issue #612).

Exit semantics:
  - Default: exit 0 iff no `severity = high` divergences are flagged.
    `info` (including all `haskell-uncoverable`) does not fail the
    run.
  - `--strict`: exit 0 iff there are no diffs at all of severity
    `high` or `medium`.  Still ignores `info` / `haskell-uncoverable`.
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
is_haskell_uncoverable = _NORM.is_haskell_uncoverable  # type: ignore[attr-defined]


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

        # Haskell-side null / missing on an uncoverable field →
        # downgrade to `info` with a tag so the report can bucket
        # it separately.  Any of three signals indicates "cn cannot
        # cover this":
        #   1. The field path (or a prefix of it) is in the
        #      uncoverable list AND the Haskell value is the `null`
        #      sentinel emitted by the normalizer.
        #   2. The field path is in the uncoverable list AND the
        #      Haskell value is `<MISSING>` because the dugite-side
        #      walked into a nested dict (e.g. `pp_current.*`) that
        #      the Haskell side stamped as `None` at the parent.
        if is_haskell_uncoverable(k) and (
            h_val is None or h_val == "<MISSING>"
        ):
            out.append(
                {
                    "epoch": epoch,
                    "field_path": k,
                    "haskell_value": h_val,
                    "dugite_value": d_val,
                    "bucket": classify(k),
                    "severity": "info",
                    "tag": "haskell-uncoverable",
                }
            )
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


def _is_uncoverable_record(r: dict) -> bool:
    return r.get("tag") == "haskell-uncoverable"


def render_markdown(records: list[dict], from_e: int, to_e: int) -> str:
    # Real divergences = high/medium severity, excluding
    # haskell-uncoverable.
    real_by_epoch: dict[int, int] = defaultdict(int)
    real_by_bucket: dict[str, int] = defaultdict(int)
    real_by_field: dict[str, int] = defaultdict(int)
    uncov_by_field: dict[str, int] = defaultdict(int)
    info_other: int = 0

    for r in records:
        if _is_uncoverable_record(r):
            uncov_by_field[r["field_path"]] += 1
            continue
        if r["severity"] == "info":
            info_other += 1
            continue
        real_by_epoch[r["epoch"]] += 1
        real_by_bucket[r["bucket"]] += 1
        real_by_field[r["field_path"]] += 1

    lines: list[str] = []
    lines.append(f"# Epoch ledger-state diff report (epochs {from_e}..{to_e})")
    lines.append("")
    lines.append(f"Total real divergences: **{sum(real_by_epoch.values())}**")
    lines.append(
        f"Total Haskell-uncoverable fields (cn 11.0.1 `log-epoch-state` cannot supply): "
        f"**{sum(uncov_by_field.values())}**"
    )
    lines.append(f"Other info-level diffs: **{info_other}**")
    lines.append("")
    lines.append("## Per-epoch real divergence count")
    lines.append("")
    lines.append("| Epoch | Diffs |")
    lines.append("| ----: | ----: |")
    for e in range(from_e, to_e + 1):
        lines.append(f"| {e} | {real_by_epoch.get(e, 0)} |")
    lines.append("")
    lines.append("## Top-10 most-divergent real fields")
    lines.append("")
    lines.append("| Field | Count |")
    lines.append("| --- | ---: |")
    for field, count in sorted(real_by_field.items(), key=lambda x: -x[1])[:10]:
        lines.append(f"| `{field}` | {count} |")
    lines.append("")
    lines.append("## Real-divergence bucket totals")
    lines.append("")
    lines.append("| Bucket | Diffs |")
    lines.append("| --- | ---: |")
    for bucket, count in sorted(real_by_bucket.items(), key=lambda x: -x[1]):
        lines.append(f"| {bucket} | {count} |")
    lines.append("")
    lines.append("## Sample real divergences (first 50)")
    lines.append("")
    lines.append("| Epoch | Field | Haskell | Dugite | Bucket |")
    lines.append("| ---: | --- | --- | --- | --- |")
    shown = 0
    for r in records:
        if r["severity"] == "info" or _is_uncoverable_record(r):
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
    lines.append("## Haskell-uncoverable fields (informational)")
    lines.append("")
    lines.append(
        "These canonical fields are emitted as `null` by the Haskell "
        "normalizer because cn 11.0.1's `cardano-cli debug log-epoch-state` "
        "outputs only the `currentEpochState` subset of `NewEpochState`. "
        "They are excluded from the real-divergence count above. See "
        "[`EPOCH_DIFF.md`](EPOCH_DIFF.md) and issue "
        "[#612](https://github.com/michaeljfazio/dugite/issues/612) "
        "for the cn emission-shape limitation."
    )
    lines.append("")
    if uncov_by_field:
        lines.append("| Field | Occurrences |")
        lines.append("| --- | ---: |")
        for field, count in sorted(uncov_by_field.items(), key=lambda x: -x[1]):
            lines.append(f"| `{field}` | {count} |")
    else:
        lines.append("_None encountered in this run._")
    lines.append("")
    return "\n".join(lines)


def render_json(records: list[dict], from_e: int, to_e: int) -> str:
    real_by_bucket: Counter[str] = Counter()
    uncov_by_field: Counter[str] = Counter()
    for r in records:
        if _is_uncoverable_record(r):
            uncov_by_field[r["field_path"]] += 1
            continue
        if r["severity"] != "info":
            real_by_bucket[r["bucket"]] += 1
    summary = {
        "from_epoch": from_e,
        "to_epoch": to_e,
        "total_diffs": sum(real_by_bucket.values()),
        "by_bucket": dict(real_by_bucket),
        "haskell_uncoverable_count": sum(uncov_by_field.values()),
        "haskell_uncoverable_by_field": dict(uncov_by_field),
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
    ap.add_argument(
        "--strict",
        action="store_true",
        help=(
            "fail on any non-info divergence; default fails only on "
            "severity=high.  Both modes ignore haskell-uncoverable "
            "fields (cn 11.0.1 dump-shape limitation, see #612)."
        ),
    )
    args = ap.parse_args()

    tolerance = load_tolerance(args.tolerance_config)
    all_diffs: list[dict] = []
    fail_count = 0

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
        for r in diffs:
            if _is_uncoverable_record(r):
                continue
            if r["severity"] == "high":
                fail_count += 1
            elif args.strict and r["severity"] != "info":
                fail_count += 1

    md = render_markdown(all_diffs, args.from_epoch, args.to_epoch)
    js = render_json(all_diffs, args.from_epoch, args.to_epoch)

    if args.report_md:
        args.report_md.write_text(md, encoding="utf-8")
    if args.report_json:
        args.report_json.write_text(js, encoding="utf-8")

    if not args.report_md and not args.report_json:
        print(md)

    return 0 if fail_count == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
