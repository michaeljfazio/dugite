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


def _cn_dump(epoch: int, treasury: int = 1_000_000) -> dict:
    """Minimal cn 11.0.1 `log-epoch-state`-shaped record.  Only
    fields the normalizer's `HASKELL_MAP` reads from are populated;
    all governance / nonce / pparams paths are absent so the
    normalizer emits `null` for them and the diff tool tags them
    `haskell-uncoverable`.
    """
    return {
        "currentEpoch": epoch,
        "currentEpochState": {
            "esChainAccountState": {
                "reserves": 5_000_000,
                "treasury": treasury,
            },
            "esLState": {
                "delegationState": {
                    "dstate": {"accounts": {}},
                    "pstate": {"stakePools": {"poolA": {}}, "retiring": {}},
                },
                "utxoState": {
                    "deposited": 0,
                    "fees": 0,
                    "utxo": {},
                    "ppups": {
                        "curPParams": {
                            "protocolVersion": {"major": 10, "minor": 0}
                        }
                    },
                },
            },
            "esSnapshots": {
                "pstakeMark": {
                    "activeStake": {
                        "k": {"swdStake": 1, "swdDelegation": "poolA"}
                    },
                    "stakePoolsSnapShot": {"poolA": {}},
                },
                "pstakeSet": {
                    "activeStake": {
                        "k": {"swdStake": 1, "swdDelegation": "poolA"}
                    },
                    "stakePoolsSnapShot": {"poolA": {}},
                },
                "pstakeGo": {
                    "activeStake": {
                        "k": {"swdStake": 1, "swdDelegation": "poolA"}
                    },
                    "stakePoolsSnapShot": {"poolA": {}},
                },
            },
        },
        "rewardUpdate": {"rs": {}},
    }


def test_uncoverable_fields_dont_inflate_real_divergence(tmp_path: Path) -> None:
    """A cn-shaped Haskell dump that exercises only the supported
    field-map and a dugite dump that fully populates governance /
    nonce / pp_* should produce:

    * 1 real divergence (treasury mismatch we inject)
    * many haskell-uncoverable entries (nonce, governance, pp_*,
      era, asset_count) — none of which count toward the real total
      or push the exit code to 1.
    """
    haskell_dir = tmp_path / "haskell"
    dugite_dir = tmp_path / "dugite"

    h = _cn_dump(epoch=1, treasury=1_000_000)
    # Single real divergence — dugite treasury is 1 lovelace higher.
    d = _dugite_dump(epoch=1, treasury=1_000_001)
    # Match the cn-derivable fields exactly so they don't generate
    # spurious diffs.
    d["scalars"]["reserves"] = 5_000_000
    d["scalars"]["fees"] = 0
    d["scalars"]["deposits_stake"] = 0
    d["protocol_version"] = {"major": 10, "minor": 0}
    d["utxo"]["count"] = 0
    d["utxo"]["total_lovelace"] = 0  # cn cannot derive; dugite stays 0
    d["stake_snapshot"]["mark"] = {"total_active_stake": 1, "pool_count": 1}
    d["stake_snapshot"]["set"] = {"total_active_stake": 1, "pool_count": 1}
    d["stake_snapshot"]["go"] = {"total_active_stake": 1, "pool_count": 1}
    d["pools"] = {"registered": 1, "retiring": 0, "retired_this_epoch": 0}
    d["rewards"]["total_distributed"] = 0

    _write_dump(haskell_dir, 1, h)
    _write_dump(dugite_dir, 1, d)

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
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 1, (
        f"expected exit=1 for treasury divergence; stderr={proc.stderr}"
    )
    report = json.loads(report_json.read_text(encoding="utf-8"))
    # Exactly the injected treasury diff is real; uncoverable fields
    # are bucketed separately.
    assert report["total_diffs"] == 1, (
        f"real divergence count should be 1, got {report['total_diffs']}: "
        f"{[d['field_path'] for d in report['diffs'] if d.get('severity') == 'high']}"
    )
    real = [r for r in report["diffs"] if r.get("severity") == "high"]
    assert len(real) == 1
    assert real[0]["field_path"] == "scalars.treasury"
    # Uncoverable bucket must be non-empty (covers nonce, governance,
    # pp_current/previous/future, era, asset_count, deposits_drep,
    # deposits_proposal).
    assert report["haskell_uncoverable_count"] >= 5
    uncov_fields = report["haskell_uncoverable_by_field"]
    for needle in (
        "nonce.eta_v",
        "governance.drep_count",
        "era",
        "scalars.deposits_drep",
        "utxo.asset_count",
    ):
        assert needle in uncov_fields, (
            f"expected {needle} in uncoverable list; got {sorted(uncov_fields)}"
        )


def test_strict_flag_promotes_info_diffs(tmp_path: Path) -> None:
    """`--strict` should treat any non-info, non-uncoverable diff as
    a failure even if its severity is below `high`.  Today every real
    diff is `high`, so strict mode behaves the same as default mode;
    this test pins the contract and guards against future regressions.
    """
    haskell_dir = tmp_path / "haskell"
    dugite_dir = tmp_path / "dugite"
    _write_dump(haskell_dir, 4, _cn_dump(epoch=4))
    d = _dugite_dump(epoch=4)
    # Make the dugite dump match cn-derivable fields so the *only*
    # diffs are haskell-uncoverable.
    d["scalars"]["reserves"] = 5_000_000
    d["scalars"]["fees"] = 0
    d["scalars"]["deposits_stake"] = 0
    d["protocol_version"] = {"major": 10, "minor": 0}
    d["utxo"]["count"] = 0
    d["utxo"]["total_lovelace"] = 0
    d["stake_snapshot"]["mark"] = {"total_active_stake": 1, "pool_count": 1}
    d["stake_snapshot"]["set"] = {"total_active_stake": 1, "pool_count": 1}
    d["stake_snapshot"]["go"] = {"total_active_stake": 1, "pool_count": 1}
    d["pools"] = {"registered": 1, "retiring": 0, "retired_this_epoch": 0}
    _write_dump(dugite_dir, 4, d)

    proc = subprocess.run(
        [
            sys.executable,
            str(DIFF),
            "--haskell-dir",
            str(haskell_dir),
            "--dugite-dir",
            str(dugite_dir),
            "--from-epoch",
            "4",
            "--to-epoch",
            "4",
            "--strict",
        ],
        capture_output=True,
        text=True,
    )
    # Strict mode still ignores haskell-uncoverable entries → exit 0.
    assert proc.returncode == 0, (
        f"strict mode must not fail on uncoverable-only diffs; "
        f"stdout={proc.stdout}\nstderr={proc.stderr}"
    )


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
