#!/usr/bin/env python3
"""Normalize one epoch-state dump to the canonical schema.

Two source modes:

* `--source dugite`  — the input is already in canonical form; this
  mode is a near-passthrough that fills in any missing fields with
  sentinel values so the diff tool sees uniform input.
* `--source haskell` — the input is a `cardano-cli debug
  log-epoch-state` record; this mode walks a YAML-driven field map
  to project the nested Haskell record into canonical form.

Outputs canonical JSON on stdout.

Usage:
    normalize-epoch-dump.py --source haskell epoch_000123.json
    normalize-epoch-dump.py --source dugite  epoch_000123.json
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


# ── Field-mapping table ────────────────────────────────────────────────
#
# Mapping rules for Haskell → canonical.  Each tuple is:
#   (canonical_path, haskell_path_options, default)
# where haskell_path_options is a list of dotted paths tried in order
# until one resolves to a value of the right shape.  See EPOCH_DIFF.md
# for the rationale behind each mapping.

CANONICAL_DEFAULTS: dict[str, Any] = {
    "epoch": 0,
    "slot": 0,
    "era": "unknown",
    "protocol_version": {"major": 0, "minor": 0},
    "scalars": {
        "reserves": 0,
        "treasury": 0,
        "fees": 0,
        "deposits_stake": 0,
        "deposits_drep": 0,
        "deposits_proposal": 0,
    },
    "nonce": {
        "eta_v": "0" * 64,
        "eta_c": "0" * 64,
        "eta_h": "0" * 64,
        "eta_lj": "0" * 64,
    },
    "utxo": {"count": 0, "total_lovelace": 0, "asset_count": 0},
    "stake_snapshot": {
        "mark": {"total_active_stake": 0, "pool_count": 0},
        "set": {"total_active_stake": 0, "pool_count": 0},
        "go": {"total_active_stake": 0, "pool_count": 0},
    },
    "pools": {"registered": 0, "retiring": 0, "retired_this_epoch": 0},
    "rewards": {"total_distributed": 0, "per_pool_top20": []},
    "governance": {
        "drep_count": 0,
        "drep_total_voting_power": 0,
        "drep_top20": [],
        "cc_members": [],
        "cc_threshold_num": 0,
        "cc_threshold_den": 1,
        "active_proposals": 0,
        "active_proposal_ids": [],
        "enacted_this_epoch": [],
        "expired_this_epoch": [],
        "constitution_anchor_hash": "0" * 64,
        "committee_hash": "0" * 64,
    },
    "pp_current": None,
    "pp_previous": None,
    "pp_future": None,
}

# Haskell mapping table.  Keys are canonical dotted paths; values are
# lists of Haskell dotted-path candidates plus a final default.
HASKELL_MAP: list[tuple[str, list[str], Any]] = [
    ("epoch", ["epoch", "nesEL.unEpochNo", "newEpochState.nesEL.unEpochNo"], 0),
    ("slot", ["slot", "tip.slot", "tip.slotNo"], 0),
    ("era", ["era"], "unknown"),
    (
        "protocol_version.major",
        [
            "protocolVersion.major",
            "newEpochState.esLState.lsUTxOState.utxosGovState.curPParams.protocolVersion.major",
            "newEpochState.nesEs.esLState.lsUTxOState.utxosGovState.curPParams.protocolVersion.major",
        ],
        0,
    ),
    (
        "protocol_version.minor",
        [
            "protocolVersion.minor",
            "newEpochState.esLState.lsUTxOState.utxosGovState.curPParams.protocolVersion.minor",
            "newEpochState.nesEs.esLState.lsUTxOState.utxosGovState.curPParams.protocolVersion.minor",
        ],
        0,
    ),
    (
        "scalars.reserves",
        [
            "scalars.reserves",
            "newEpochState.esAccountState.reserves",
            "newEpochState.nesEs.esAccountState.asReserves",
        ],
        0,
    ),
    (
        "scalars.treasury",
        [
            "scalars.treasury",
            "newEpochState.esAccountState.treasury",
            "newEpochState.nesEs.esAccountState.asTreasury",
        ],
        0,
    ),
    (
        "scalars.fees",
        [
            "scalars.fees",
            "newEpochState.esLState.lsUTxOState.utxosFees",
            "newEpochState.nesEs.esLState.lsUTxOState.utxosFees",
        ],
        0,
    ),
    (
        "scalars.deposits_stake",
        [
            "scalars.deposits_stake",
            "newEpochState.esLState.lsCertState.certDState.totalDeposit",
        ],
        0,
    ),
    (
        "scalars.deposits_drep",
        [
            "scalars.deposits_drep",
            "newEpochState.esLState.lsCertState.certVState.vsDRepsTotalDeposit",
        ],
        0,
    ),
    (
        "scalars.deposits_proposal",
        [
            "scalars.deposits_proposal",
            "newEpochState.esLState.lsUTxOState.utxosGovState.proposalsDeposits",
        ],
        0,
    ),
    (
        "nonce.eta_v",
        ["nonce.eta_v", "chainDepState.csProtocol.prtclState.evolvingNonce"],
        "0" * 64,
    ),
    (
        "nonce.eta_c",
        ["nonce.eta_c", "chainDepState.csProtocol.prtclState.candidateNonce"],
        "0" * 64,
    ),
    (
        "nonce.eta_h",
        ["nonce.eta_h", "chainDepState.csTickn.ticknStateEpochNonce"],
        "0" * 64,
    ),
    (
        "nonce.eta_lj",
        [
            "nonce.eta_lj",
            "chainDepState.csTickn.ticknStateLastEpochBlockNonce",
            "chainDepState.csProtocol.prtclState.lastEpochBlockNonce",
        ],
        "0" * 64,
    ),
    (
        "utxo.count",
        [
            "utxo.count",
            "newEpochState.esLState.lsUTxOState.utxosUtxoCount",
        ],
        0,
    ),
    (
        "utxo.total_lovelace",
        [
            "utxo.total_lovelace",
            "newEpochState.esLState.lsUTxOState.utxosTotalLovelace",
        ],
        0,
    ),
    ("utxo.asset_count", ["utxo.asset_count"], 0),
    (
        "stake_snapshot.mark.total_active_stake",
        [
            "stake_snapshot.mark.total_active_stake",
            "newEpochState.esSnapshots.ssStakeMark.ssStakeTotal",
        ],
        0,
    ),
    (
        "stake_snapshot.mark.pool_count",
        [
            "stake_snapshot.mark.pool_count",
            "newEpochState.esSnapshots.ssStakeMark.ssStakePoolCount",
        ],
        0,
    ),
    (
        "stake_snapshot.set.total_active_stake",
        [
            "stake_snapshot.set.total_active_stake",
            "newEpochState.esSnapshots.ssStakeSet.ssStakeTotal",
        ],
        0,
    ),
    (
        "stake_snapshot.set.pool_count",
        [
            "stake_snapshot.set.pool_count",
            "newEpochState.esSnapshots.ssStakeSet.ssStakePoolCount",
        ],
        0,
    ),
    (
        "stake_snapshot.go.total_active_stake",
        [
            "stake_snapshot.go.total_active_stake",
            "newEpochState.esSnapshots.ssStakeGo.ssStakeTotal",
        ],
        0,
    ),
    (
        "stake_snapshot.go.pool_count",
        [
            "stake_snapshot.go.pool_count",
            "newEpochState.esSnapshots.ssStakeGo.ssStakePoolCount",
        ],
        0,
    ),
    (
        "pools.registered",
        [
            "pools.registered",
            "newEpochState.esLState.lsCertState.certPState.psPoolCount",
        ],
        0,
    ),
    (
        "rewards.total_distributed",
        [
            "rewards.total_distributed",
            "newEpochState.nesRu.totalRewards",
        ],
        0,
    ),
    (
        "governance.drep_count",
        [
            "governance.drep_count",
            "newEpochState.esLState.lsCertState.certVState.vsDRepsCount",
        ],
        0,
    ),
    (
        "governance.cc_threshold_num",
        [
            "governance.cc_threshold_num",
            "newEpochState.esLState.lsUTxOState.utxosGovState.committee.committeeThreshold.numerator",
        ],
        0,
    ),
    (
        "governance.cc_threshold_den",
        [
            "governance.cc_threshold_den",
            "newEpochState.esLState.lsUTxOState.utxosGovState.committee.committeeThreshold.denominator",
        ],
        1,
    ),
    (
        "governance.active_proposals",
        [
            "governance.active_proposals",
            "newEpochState.esLState.lsUTxOState.utxosGovState.proposals.unProposalsCount",
        ],
        0,
    ),
]


def _resolve(record: Any, path: str) -> Any:
    cur = record
    for key in path.split("."):
        if isinstance(cur, dict) and key in cur:
            cur = cur[key]
        else:
            return None
    return cur


def _set(target: dict, path: str, value: Any) -> None:
    keys = path.split(".")
    cur = target
    for k in keys[:-1]:
        cur = cur.setdefault(k, {})
    cur[keys[-1]] = value


def _deep_default(schema: Any) -> Any:
    """Recursively clone the schema as the default canonical structure."""
    if isinstance(schema, dict):
        return {k: _deep_default(v) for k, v in schema.items()}
    if isinstance(schema, list):
        return list(schema)
    return schema


def normalize_haskell(record: dict) -> dict:
    out = _deep_default(CANONICAL_DEFAULTS)
    for canonical_path, haskell_paths, default in HASKELL_MAP:
        value = None
        for hp in haskell_paths:
            value = _resolve(record, hp)
            if value is not None:
                break
        if value is None:
            value = default
        _set(out, canonical_path, value)
    return out


def normalize_dugite(record: dict) -> dict:
    """Dugite already emits canonical form — merge in any missing
    defaults so downstream comparators don't `KeyError` on partial
    records.
    """
    out = _deep_default(CANONICAL_DEFAULTS)
    _merge(out, record)
    return out


def _merge(dst: dict, src: dict) -> None:
    for k, v in src.items():
        if isinstance(v, dict) and isinstance(dst.get(k), dict):
            _merge(dst[k], v)
        else:
            dst[k] = v


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--source", choices=("haskell", "dugite"), required=True)
    ap.add_argument("file", type=Path)
    args = ap.parse_args()

    with args.file.open("r", encoding="utf-8") as f:
        record = json.load(f)
    if args.source == "haskell":
        out = normalize_haskell(record)
    else:
        out = normalize_dugite(record)
    json.dump(out, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
