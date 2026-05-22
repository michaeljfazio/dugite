"""Smoke test for `diff-epoch-dumps.py`.

Builds two synthetic 1-epoch dumps differing in exactly one field and
asserts the diff tool flags the divergence with the expected bucket.

Run with:
    python3 -m pytest scripts/validation/tests/
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
DIFF = REPO_ROOT / "scripts" / "validation" / "diff-epoch-dumps.py"


def _dugite_dump(epoch: int, treasury: int = 1_000_000) -> dict:
    """Canonical-form dugite dump (matches `EpochStateDump` serde)."""
    return {
        "epoch": epoch,
        "slot": 432_000,
        "era": "conway",
        "protocol_version": {"major": 10, "minor": 0},
        "scalars": {
            "reserves": 5_000_000,
            "treasury": treasury,
            "fees": 0,
            "deposits_stake": 0,
            "deposits_drep": 0,
            "deposits_proposal": 0,
        },
        "nonce": {
            "eta_v": "ab" * 32,
            "eta_c": "cd" * 32,
            "eta_h": "ef" * 32,
            "eta_lj": "01" * 32,
        },
        "utxo": {"count": 100, "total_lovelace": 50_000_000, "asset_count": 0},
        "stake_snapshot": {
            "mark": {"total_active_stake": 1, "pool_count": 1},
            "set": {"total_active_stake": 1, "pool_count": 1},
            "go": {"total_active_stake": 1, "pool_count": 1},
        },
        "pools": {"registered": 1, "retiring": 0, "retired_this_epoch": 0},
        "rewards": {"total_distributed": 0, "per_pool_top20": []},
        "governance": {
            "drep_count": 0,
            "drep_total_voting_power": 0,
            "drep_top20": [],
            "cc_members": [],
            "cc_threshold_num": 2,
            "cc_threshold_den": 3,
            "active_proposals": 0,
            "active_proposal_ids": [],
            "enacted_this_epoch": [],
            "expired_this_epoch": [],
            "constitution_anchor_hash": "00" * 32,
            "committee_hash": "00" * 32,
        },
        "pp_current": None,
        "pp_previous": None,
        "pp_future": None,
    }


def _haskell_dump(epoch: int, treasury: int = 1_000_000) -> dict:
    """Minimal Haskell-shaped record exercising the normalizer's
    field-map paths.  Only fields the normalizer knows about are set;
    everything else falls back to canonical defaults on both sides so
    they cancel in the diff.
    """
    return {
        "epoch": epoch,
        "slot": 432_000,
        "era": "conway",
        # Top-level shortcut paths the normalizer accepts.
        "protocolVersion": {"major": 10, "minor": 0},
        # Match the canonical defaults so they don't appear as diffs.
        "scalars": {
            "reserves": 5_000_000,
            "treasury": treasury,
            "fees": 0,
            "deposits_stake": 0,
            "deposits_drep": 0,
            "deposits_proposal": 0,
        },
        "nonce": {
            "eta_v": "ab" * 32,
            "eta_c": "cd" * 32,
            "eta_h": "ef" * 32,
            "eta_lj": "01" * 32,
        },
        "utxo": {"count": 100, "total_lovelace": 50_000_000, "asset_count": 0},
        "stake_snapshot": {
            "mark": {"total_active_stake": 1, "pool_count": 1},
            "set": {"total_active_stake": 1, "pool_count": 1},
            "go": {"total_active_stake": 1, "pool_count": 1},
        },
        "pools": {"registered": 1, "retiring": 0, "retired_this_epoch": 0},
        "rewards": {"total_distributed": 0, "per_pool_top20": []},
        "governance": {
            "drep_count": 0,
            "drep_total_voting_power": 0,
            "drep_top20": [],
            "cc_members": [],
            "cc_threshold_num": 2,
            "cc_threshold_den": 3,
            "active_proposals": 0,
            "active_proposal_ids": [],
            "enacted_this_epoch": [],
            "expired_this_epoch": [],
            "constitution_anchor_hash": "00" * 32,
            "committee_hash": "00" * 32,
        },
        "pp_current": None,
        "pp_previous": None,
        "pp_future": None,
    }


def _write_dump(dir_: Path, epoch: int, dump: dict) -> None:
    dir_.mkdir(parents=True, exist_ok=True)
    (dir_ / f"epoch_{epoch:06d}.json").write_text(json.dumps(dump), encoding="utf-8")


def test_diff_flags_treasury_divergence(tmp_path: Path) -> None:
    haskell_dir = tmp_path / "haskell"
    dugite_dir = tmp_path / "dugite"

    haskell_dump = _haskell_dump(epoch=1, treasury=1_000_000)
    # Single known divergence: dugite treasury is 1 lovelace higher.
    dugite_dump = _dugite_dump(epoch=1, treasury=1_000_001)

    _write_dump(haskell_dir, 1, haskell_dump)
    _write_dump(dugite_dir, 1, dugite_dump)

    report_json = tmp_path / "report.json"
    proc = subprocess.run(
        [
            sys.executable,
            str(DIFF),
            "--haskell-dir",
            str(haskell_dir),
            "--dugite-dir",
            str(dugite_dir),
            "--from-epoch",
            "1",
            "--to-epoch",
            "1",
            "--report-json",
            str(report_json),
        ],
        env={**os.environ, "PYTHONUNBUFFERED": "1"},
        capture_output=True,
        text=True,
    )
    # Non-zero exit means high-severity diffs were found — that's what
    # we want here.
    assert proc.returncode == 1, (
        f"diff tool should have exited 1; got {proc.returncode}\n"
        f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
    )
    assert report_json.exists()
    report = json.loads(report_json.read_text(encoding="utf-8"))
    assert report["total_diffs"] == 1
    assert report["by_bucket"].get("rewards") == 1  # scalars maps to rewards
    diff = report["diffs"][0]
    assert diff["field_path"] == "scalars.treasury"
    assert diff["haskell_value"] == 1_000_000
    assert diff["dugite_value"] == 1_000_001


def test_diff_passes_when_dumps_match(tmp_path: Path) -> None:
    haskell_dir = tmp_path / "haskell"
    dugite_dir = tmp_path / "dugite"
    _write_dump(haskell_dir, 2, _haskell_dump(epoch=2))
    _write_dump(dugite_dir, 2, _dugite_dump(epoch=2))
    proc = subprocess.run(
        [
            sys.executable,
            str(DIFF),
            "--haskell-dir",
            str(haskell_dir),
            "--dugite-dir",
            str(dugite_dir),
            "--from-epoch",
            "2",
            "--to-epoch",
            "2",
        ],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"identical dumps should pass: {proc.stderr}"


def test_diff_reports_missing_dump_without_allow(tmp_path: Path) -> None:
    haskell_dir = tmp_path / "haskell"
    dugite_dir = tmp_path / "dugite"
    _write_dump(haskell_dir, 3, _haskell_dump(epoch=3))
    # dugite missing
    proc = subprocess.run(
        [
            sys.executable,
            str(DIFF),
            "--haskell-dir",
            str(haskell_dir),
            "--dugite-dir",
            str(dugite_dir),
            "--from-epoch",
            "3",
            "--to-epoch",
            "3",
        ],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 3, "missing dump without --allow-missing should error"
