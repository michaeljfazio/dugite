#!/usr/bin/env python3
"""Normalize one epoch-state dump to the canonical schema.

Two source modes:

* `--source dugite`  — the input is already in canonical form; this
  mode is a near-passthrough that fills in any missing fields with
  sentinel values so the diff tool sees uniform input.
* `--source haskell` — the input is a `cardano-cli debug
  log-epoch-state` record; this mode walks a field map to project
  the nested Haskell record into canonical form.

cn 11.0.1's `log-epoch-state` only emits the `currentEpochState`
subset of `NewEpochState` (no `chainDepState` nonces, no
`utxosGovState`, no era info, no fully-materialised `PParams` in
dugite's shape).  Canonical fields that **cannot** be derived from
that output are emitted as `null` and listed in
`HASKELL_UNCOVERABLE` so the diff tool can downgrade them to `info`
severity rather than flag them as real divergences.  See
`scripts/validation/EPOCH_DIFF.md` and issue #612 for context.

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
from typing import Any, Callable


# ── Field-mapping table ────────────────────────────────────────────────
#
# Mapping rules for Haskell → canonical.  Each tuple is:
#   (canonical_path, haskell_path_options, default)
# where haskell_path_options is a list of dotted paths tried in order
# until one resolves to a value of the right shape.  When no path
# resolves, the default is used.
#
# A path that begins with `fn:` invokes a registered derivation
# function on the full record (see `_DERIVATIONS`).
#
# See EPOCH_DIFF.md for the rationale behind each mapping.

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


# ── Haskell-side uncoverable fields ───────────────────────────────────
#
# Canonical fields that `cardano-cli debug log-epoch-state` (cn
# 11.0.1) cannot supply because the underlying record is the
# `currentEpochState` subset, not the full `NewEpochState`.  The
# normalizer emits `null` for each so the diff tool can tag
# divergences as "haskell-side uncoverable" rather than flag them as
# real bugs.
#
# These match `field_path` strings produced by the diff tool's
# `walk()` flattener.  A prefix entry (e.g. `governance.`) covers all
# descendants.

HASKELL_UNCOVERABLE: set[str] = {
    # Praos nonce state — lives on `chainDepState`, not surfaced.
    "nonce.eta_v",
    "nonce.eta_c",
    "nonce.eta_h",
    "nonce.eta_lj",
    # Governance — cn 11.0.1 emits the pre-Conway `ppups` block, not
    # `utxosGovState`.  Drep/CC/proposal counts and ids are entirely
    # absent.
    "governance.drep_count",
    "governance.drep_total_voting_power",
    "governance.drep_top20",
    "governance.cc_members",
    "governance.cc_threshold_num",
    "governance.cc_threshold_den",
    "governance.active_proposals",
    "governance.active_proposal_ids",
    "governance.enacted_this_epoch",
    "governance.expired_this_epoch",
    "governance.constitution_anchor_hash",
    "governance.committee_hash",
    # Full materialised PParams use cn's camelCase Haskell shape; we
    # cannot map them onto dugite's snake_case `ProtocolParameters`
    # serde without a full PParams renamer.  Treat as uncoverable.
    "pp_current",
    "pp_previous",
    "pp_future",
    # Era label is not on the cn dump (only the protocol version is).
    "era",
    # Governance-class deposits are PV9+ only; cn dump pre-Conway has
    # no field for them at all.
    "scalars.deposits_drep",
    "scalars.deposits_proposal",
    # Asset count needs a full UTxO walk which the cn dump only
    # provides as an empty `utxo: {}` at this dump stage.
    "utxo.asset_count",
    # `utxo.total_lovelace` likewise needs the full UTxO walk that cn
    # 11.0.1's empty `utxo: {}` cannot supply.
    "utxo.total_lovelace",
    # Boundary `slot` is not surfaced by cn's `log-epoch-state` (the
    # record describes the epoch state but not the slot the boundary
    # fired at).
    "slot",
    # Reward `per_pool_top20` would require additional aggregation
    # the cn dump's `rewardUpdate.rs` does not naturally key by pool.
    # Keep total only.
    "rewards.per_pool_top20",
    # Pools "retired_this_epoch" is not surfaced — cn pre-Conway dump
    # exposes `retiring` (a future map) but not the just-retired set.
    "pools.retired_this_epoch",
}


def is_haskell_uncoverable(field_path: str) -> bool:
    """Return True iff `field_path` (or a prefix of it) is in the
    `HASKELL_UNCOVERABLE` set.  Prefix match lets a single entry like
    `pp_current` cover every nested leaf under it.
    """
    if field_path in HASKELL_UNCOVERABLE:
        return True
    for prefix in HASKELL_UNCOVERABLE:
        if field_path.startswith(prefix + "."):
            return True
    return False


# ── Derivation functions (cn → canonical scalar) ──────────────────────
#
# Some canonical fields require summation or counting over the cn
# record rather than a direct lookup.  These live behind the `fn:`
# prefix in the mapping table.

def _sum_snapshot_stake(record: Any, snapshot: str) -> int | None:
    """Sum `swdStake` across `currentEpochState.esSnapshots.<snapshot>.activeStake`."""
    snap = _resolve(record, f"currentEpochState.esSnapshots.{snapshot}.activeStake")
    if not isinstance(snap, dict):
        return None
    total = 0
    for v in snap.values():
        if isinstance(v, dict):
            stake = v.get("swdStake")
            if isinstance(stake, int):
                total += stake
    return total


def _count_snapshot_pools(record: Any, snapshot: str) -> int | None:
    snap = _resolve(
        record, f"currentEpochState.esSnapshots.{snapshot}.stakePoolsSnapShot"
    )
    if isinstance(snap, dict):
        return len(snap)
    return None


def _count_utxo(record: Any) -> int | None:
    """Return the UTxO count from a Haskell `cardano-cli debug log-epoch-state`
    record, or `None` when the count cannot be derived.

    Bug 4 (#615): `cardano-cli debug log-epoch-state` emits the UTxO map
    as an empty object `{}` even when the live UTxO set is populated --
    the field is intentionally not enumerable in the public CLI output.
    Returning `len({}) == 0` from this function masked that as a real
    `0`, producing a 100-vs-0 divergence against dugite which DOES emit
    the true count.

    Fix: when the `utxo` field is empty AND the alternative `utxosUtxo`
    path is not enumerable either, return `None`.  This causes the diff
    tool to mark the field "uncoverable" rather than report a false
    divergence.  A non-empty `utxo` or `utxosUtxo` map is honoured as
    before -- some fixtures and reduced dumps DO populate one or the
    other.
    """
    utxo = _resolve(record, "currentEpochState.esLState.utxoState.utxo")
    if isinstance(utxo, dict) and len(utxo) > 0:
        return len(utxo)
    utxos_utxo = _resolve(record, "currentEpochState.esLState.utxoState.utxosUtxo")
    if isinstance(utxos_utxo, dict) and len(utxos_utxo) > 0:
        return len(utxos_utxo)
    # Neither path is enumerable -- treat as "Haskell schema gap, not
    # divergence" (Bug 4 of #615).
    return None


def _count_registered_pools(record: Any) -> int | None:
    pools = _resolve(
        record, "currentEpochState.esLState.delegationState.pstate.stakePools"
    )
    if isinstance(pools, dict):
        return len(pools)
    return None


def _count_retiring_pools(record: Any) -> int | None:
    retiring = _resolve(
        record, "currentEpochState.esLState.delegationState.pstate.retiring"
    )
    if isinstance(retiring, dict):
        return len(retiring)
    return None


def _sum_total_rewards(record: Any) -> int | None:
    """Sum `rewardAmount` across `rewardUpdate.rs` entries (a map of
    cred-hex → list of {rewardAmount, rewardPool, rewardType}).
    """
    rs = _resolve(record, "rewardUpdate.rs")
    if not isinstance(rs, dict):
        return None
    total = 0
    for entries in rs.values():
        if isinstance(entries, list):
            for e in entries:
                if isinstance(e, dict):
                    amt = e.get("rewardAmount")
                    if isinstance(amt, int):
                        total += amt
    return total


_DERIVATIONS: dict[str, Callable[[Any], Any]] = {
    "stake_mark_total": lambda r: _sum_snapshot_stake(r, "pstakeMark"),
    "stake_set_total": lambda r: _sum_snapshot_stake(r, "pstakeSet"),
    "stake_go_total": lambda r: _sum_snapshot_stake(r, "pstakeGo"),
    "stake_mark_pools": lambda r: _count_snapshot_pools(r, "pstakeMark"),
    "stake_set_pools": lambda r: _count_snapshot_pools(r, "pstakeSet"),
    "stake_go_pools": lambda r: _count_snapshot_pools(r, "pstakeGo"),
    "utxo_count": _count_utxo,
    "pools_registered": _count_registered_pools,
    "pools_retiring": _count_retiring_pools,
    "rewards_total": _sum_total_rewards,
}


# Haskell mapping table.  Keys are canonical dotted paths; values are
# lists of Haskell dotted-path candidates plus a final default.  A
# path of the form `fn:<name>` calls the matching derivation.
#
# Only fields cn 11.0.1 actually emits are listed here.  Anything
# else falls through to its CANONICAL_DEFAULTS entry, and is marked
# `HASKELL_UNCOVERABLE` so the diff tool can downgrade it.

HASKELL_MAP: list[tuple[str, list[str], Any]] = [
    ("epoch", ["currentEpoch", "epoch"], 0),
    # Protocol version — cn dump has it under ppups.curPParams.
    (
        "protocol_version.major",
        [
            "currentEpochState.esLState.utxoState.ppups.curPParams.protocolVersion.major",
            "protocolVersion.major",
        ],
        0,
    ),
    (
        "protocol_version.minor",
        [
            "currentEpochState.esLState.utxoState.ppups.curPParams.protocolVersion.minor",
            "protocolVersion.minor",
        ],
        0,
    ),
    # Account scalars.  First path is the real cn 11.0.1 dump
    # location; subsequent paths are top-level shortcuts used by
    # smoke-test fixtures.
    (
        "scalars.reserves",
        ["currentEpochState.esChainAccountState.reserves", "scalars.reserves"],
        0,
    ),
    (
        "scalars.treasury",
        ["currentEpochState.esChainAccountState.treasury", "scalars.treasury"],
        0,
    ),
    (
        "scalars.fees",
        ["currentEpochState.esLState.utxoState.fees", "scalars.fees"],
        0,
    ),
    (
        "scalars.deposits_stake",
        [
            "currentEpochState.esLState.utxoState.deposited",
            "scalars.deposits_stake",
        ],
        0,
    ),
    # UTxO totals — only `count` is reliably derivable from real cn
    # dumps (the empty `utxo: {}` precludes total_lovelace and
    # asset_count).  Top-level shortcut path for fixtures.
    ("utxo.count", ["fn:utxo_count", "utxo.count"], 0),
    # Stake snapshot totals — sum the per-account `swdStake` entries
    # from real cn dumps; honor top-level shortcuts in fixtures.
    (
        "stake_snapshot.mark.total_active_stake",
        ["fn:stake_mark_total", "stake_snapshot.mark.total_active_stake"],
        0,
    ),
    (
        "stake_snapshot.mark.pool_count",
        ["fn:stake_mark_pools", "stake_snapshot.mark.pool_count"],
        0,
    ),
    (
        "stake_snapshot.set.total_active_stake",
        ["fn:stake_set_total", "stake_snapshot.set.total_active_stake"],
        0,
    ),
    (
        "stake_snapshot.set.pool_count",
        ["fn:stake_set_pools", "stake_snapshot.set.pool_count"],
        0,
    ),
    (
        "stake_snapshot.go.total_active_stake",
        ["fn:stake_go_total", "stake_snapshot.go.total_active_stake"],
        0,
    ),
    (
        "stake_snapshot.go.pool_count",
        ["fn:stake_go_pools", "stake_snapshot.go.pool_count"],
        0,
    ),
    # Pool counts.
    ("pools.registered", ["fn:pools_registered", "pools.registered"], 0),
    ("pools.retiring", ["fn:pools_retiring", "pools.retiring"], 0),
    # Reward total — sum the rewardUpdate.rs payouts.
    (
        "rewards.total_distributed",
        ["fn:rewards_total", "rewards.total_distributed"],
        0,
    ),
    # ── Legacy / shortcut paths ─────────────────────────────────────
    #
    # cn 11.0.1 does NOT emit any of the fields below.  They are
    # listed so that callers (chiefly the smoke-test fixtures and any
    # hypothetical future cn release that grows back the missing
    # data) can supply pre-canonicalised values that override the
    # `HASKELL_UNCOVERABLE` `null` sentinels stamped earlier.  Real
    # cn dumps will resolve all of these to `None` and the diff tool
    # will tag the resulting diffs as `haskell-uncoverable`.
    ("era", ["era"], None),
    ("slot", ["slot", "tip.slot", "tip.slotNo"], None),
    ("nonce.eta_v", ["nonce.eta_v"], None),
    ("nonce.eta_c", ["nonce.eta_c"], None),
    ("nonce.eta_h", ["nonce.eta_h"], None),
    ("nonce.eta_lj", ["nonce.eta_lj"], None),
    ("utxo.asset_count", ["utxo.asset_count"], None),
    ("utxo.total_lovelace", ["utxo.total_lovelace"], None),
    ("scalars.deposits_drep", ["scalars.deposits_drep"], None),
    ("scalars.deposits_proposal", ["scalars.deposits_proposal"], None),
    ("pools.retired_this_epoch", ["pools.retired_this_epoch"], None),
    ("rewards.per_pool_top20", ["rewards.per_pool_top20"], None),
    ("governance.drep_count", ["governance.drep_count"], None),
    (
        "governance.drep_total_voting_power",
        ["governance.drep_total_voting_power"],
        None,
    ),
    ("governance.drep_top20", ["governance.drep_top20"], None),
    ("governance.cc_members", ["governance.cc_members"], None),
    ("governance.cc_threshold_num", ["governance.cc_threshold_num"], None),
    ("governance.cc_threshold_den", ["governance.cc_threshold_den"], None),
    ("governance.active_proposals", ["governance.active_proposals"], None),
    (
        "governance.active_proposal_ids",
        ["governance.active_proposal_ids"],
        None,
    ),
    ("governance.enacted_this_epoch", ["governance.enacted_this_epoch"], None),
    ("governance.expired_this_epoch", ["governance.expired_this_epoch"], None),
    (
        "governance.constitution_anchor_hash",
        ["governance.constitution_anchor_hash"],
        None,
    ),
    ("governance.committee_hash", ["governance.committee_hash"], None),
    ("pp_current", ["pp_current"], None),
    ("pp_previous", ["pp_previous"], None),
    ("pp_future", ["pp_future"], None),
]


# ── Path resolution ───────────────────────────────────────────────────


def _resolve(record: Any, path: str) -> Any:
    """Resolve a dotted path, or invoke a registered derivation when
    the path begins with `fn:`.
    """
    if path.startswith("fn:"):
        fn = _DERIVATIONS.get(path[3:])
        if fn is None:
            return None
        return fn(record)
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


def _set_uncoverable(target: dict, path: str) -> None:
    """Write a `None` sentinel at `path`, marking the leaf as
    cn-uncoverable.  The diff tool downgrades these.
    """
    _set(target, path, None)


def _deep_default(schema: Any) -> Any:
    """Recursively clone the schema as the default canonical structure."""
    if isinstance(schema, dict):
        return {k: _deep_default(v) for k, v in schema.items()}
    if isinstance(schema, list):
        return list(schema)
    return schema


# ── Normalizers ───────────────────────────────────────────────────────


def normalize_haskell(record: dict) -> dict:
    """Project a cn 11.0.1 `log-epoch-state` record into canonical
    form.  Fields cn cannot supply are written as `None` so the diff
    tool can mark them `haskell-uncoverable`.
    """
    out = _deep_default(CANONICAL_DEFAULTS)

    # 1. Stamp every uncoverable leaf with `None` up front.  These
    #    can still be overridden below if some new cn version starts
    #    emitting them — first non-None resolved path wins.
    for path in HASKELL_UNCOVERABLE:
        # Only stamp scalar / top-level paths.  Nested-dict prefixes
        # like `pp_current` get the whole subtree replaced with None.
        _set_uncoverable(out, path)

    # 2. Apply the field map.
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
