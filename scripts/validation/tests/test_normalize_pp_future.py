"""Unit tests for `normalize-epoch-dump.py::_derive_pp_future` (issue #807).

`next_future_pp` (dugite, `crates/dugite-ledger/src/state/epoch_state_debug.rs`)
surfaces whatever `ProtocolParamUpdate` is queued in `pending_pp_updates`/
`future_pp_updates` for the upcoming epoch boundary. Prior to #807 the
Haskell-side normalizer had no equivalent: `pp_future` was unconditionally
`null` and listed in `HASKELL_UNCOVERABLE`, so the diff tool could never
actually compare PPUP enactment timing between the two implementations.

`_derive_pp_future` closes that gap by reading cn 11.0.1's
`ppups.proposals` / `ppups.futureProposals` (Haskell `ShelleyGovState`,
JSON keys hand-written in its `ToKeyValuePairs` instance as "proposals" /
"futureProposals" — a straight drop of the `sgs` record prefix) and
translating the `PParamsUpdate` fields dugite's legacy
`ProtocolParamUpdate` understands into canonical snake_case names.

Two non-obvious real-source facts drive these fixtures (both live-verified
against IntersectMBO/cardano-ledger — see
`.claude/agent-memory/cardano-ledger-oracle/ppup-json-field-names-debug-dump.md`):

1. `ProposedPPUpdates`'s `ToJSON` does `Map.toList` before `toJSON`, so it
   serializes as a JSON ARRAY of `[hexKeyHash, PParamsUpdate]` pairs, NOT
   an object keyed by hash. All fixtures below use that array-of-pairs
   shape, not a dict.
2. `PParamsUpdate`'s JSON keys are data-driven from each era's `ppName`
   table, NOT the abbreviated Shelley-paper record names — e.g. `nOpt` is
   NOT the JSON key, `stakePoolTargetNum` is.

Run with:
    python3 -m pytest scripts/validation/tests/test_normalize_pp_future.py
"""

from __future__ import annotations

import importlib.util
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
NORMALIZER = REPO_ROOT / "scripts" / "validation" / "normalize-epoch-dump.py"


def _load_normalizer():
    """Load the normalize-epoch-dump.py module by path (hyphenated name
    is not importable as a Python identifier)."""
    spec = importlib.util.spec_from_file_location("normalize_epoch_dump", NORMALIZER)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _base_record(ppups_extra: dict | None = None) -> dict:
    ppups = {"curPParams": {"protocolVersion": {"major": 9, "minor": 0}}}
    if ppups_extra:
        ppups.update(ppups_extra)
    return {
        "currentEpoch": 500,
        "currentEpochState": {
            "esLState": {
                "utxoState": {
                    "ppups": ppups,
                }
            }
        },
    }


def test_pp_future_is_none_when_nothing_queued() -> None:
    nm = _load_normalizer()
    record = _base_record()
    assert nm._derive_pp_future(record) is None


def test_pp_future_is_none_when_proposal_arrays_are_empty() -> None:
    nm = _load_normalizer()
    record = _base_record({"proposals": [], "futureProposals": []})
    assert nm._derive_pp_future(record) is None


def test_pp_future_surfaces_pending_proposal() -> None:
    """`proposals` is an ARRAY OF [hash, update] PAIRS (Map.toList), not
    an object keyed by hash."""
    nm = _load_normalizer()
    record = _base_record(
        {
            "proposals": [
                [
                    "a1b2c3",
                    {"stakePoolTargetNum": 750, "minPoolCost": 340_000_000},
                ]
            ]
        }
    )
    assert nm._derive_pp_future(record) == {
        "n_opt": 750,
        "min_pool_cost": 340_000_000,
    }


def test_pp_future_surfaces_future_proposal() -> None:
    """`futureProposals` is probed defensively even though in practice
    dugite (and, by the same timing, cn's NEWPP rule) promotes every
    future entry into the current map on every boundary transition."""
    nm = _load_normalizer()
    record = _base_record(
        {"futureProposals": [["a1b2c3", {"poolRetireMaxEpoch": 18}]]}
    )
    assert nm._derive_pp_future(record) == {"e_max": 18}


def test_pp_future_unpacks_nested_protocol_version() -> None:
    nm = _load_normalizer()
    record = _base_record(
        {
            "proposals": [
                ["a1b2c3", {"protocolVersion": {"major": 9, "minor": 0}}]
            ]
        }
    )
    assert nm._derive_pp_future(record) == {
        "protocol_version_major": 9,
        "protocol_version_minor": 0,
    }


def test_pp_future_merges_multiple_proposers() -> None:
    """Multiple distinct genesis-key proposers in the same epoch each
    contribute their own fields to the merged result (mirrors dugite's
    `next_future_pp` merge loop over every entry under the lookup key)."""
    nm = _load_normalizer()
    record = _base_record(
        {
            "proposals": [
                ["genhash1", {"stakePoolTargetNum": 500}],
                ["genhash2", {"minPoolCost": 170_000_000}],
            ]
        }
    )
    assert nm._derive_pp_future(record) == {
        "n_opt": 500,
        "min_pool_cost": 170_000_000,
    }


def test_pp_future_drops_unmapped_fields() -> None:
    """A Haskell field with no dugite `ProtocolParamUpdate` equivalent
    (synthetic name here) is silently dropped rather than surfaced."""
    nm = _load_normalizer()
    record = _base_record(
        {
            "proposals": [
                ["genhash1", {"stakePoolTargetNum": 500, "notARealField": 42}]
            ]
        }
    )
    assert nm._derive_pp_future(record) == {"n_opt": 500}


def test_pp_future_translates_utxo_cost_per_byte() -> None:
    """Both Alonzo (per-word) and Babbage+ (per-byte) eras use the SAME
    JSON key `utxoCostPerByte` despite the unit changing underneath it —
    a known cross-era naming quirk, not two distinct keys."""
    nm = _load_normalizer()
    record = _base_record({"proposals": [["g", {"utxoCostPerByte": 4310}]]})
    assert nm._derive_pp_future(record) == {"ada_per_utxo_byte": 4310}


def test_pp_future_ignores_dict_shaped_map_not_array_of_pairs() -> None:
    """A `{"<hash>": {...}}` object (the natural first guess for a Haskell
    `Map`) is NOT the real wire shape — `ProposedPPUpdates` always
    serializes as an array. This must yield nothing, not silently work by
    accident, so a future regression back to the wrong shape is caught."""
    nm = _load_normalizer()
    record = _base_record(
        {"proposals": {"a1b2c3": {"stakePoolTargetNum": 750}}}
    )
    assert nm._derive_pp_future(record) is None


def test_pp_future_ignores_conway_gov_action_state_list() -> None:
    """On an actual cn 11.0.1 (Conway) dump, `ppups.proposals` is the
    CIP-1694 `GovActionState` list (plain objects), not `[hash, update]`
    pairs, and there is no `futureProposals` key at all. This must be
    structurally ignored (not misparsed as a PPUP proposal), matching
    dugite's own `next_future_pp`, which also returns `None` post-Conway
    since its legacy maps are never populated by governance actions."""
    nm = _load_normalizer()
    record = _base_record(
        {
            "proposals": [
                {
                    "gasId": {"txId": "deadbeef", "govActionIx": 0},
                    "gasCommitteeVotes": {},
                    "gasDRepVotes": {},
                }
            ]
            # No futureProposals key at all on a Conway dump.
        }
    )
    assert nm._derive_pp_future(record) is None


def test_pp_future_no_longer_haskell_uncoverable() -> None:
    """#807: `pp_future` (and its leaves) must NOT be in the uncoverable
    set anymore — cn can now supply a real (partial) value for it.
    `pp_current`/`pp_previous` remain uncoverable (no full PParams
    renamer exists)."""
    nm = _load_normalizer()
    assert nm.is_haskell_uncoverable("pp_future") is False
    assert nm.is_haskell_uncoverable("pp_future.n_opt") is False
    assert nm.is_haskell_uncoverable("pp_current") is True
    assert nm.is_haskell_uncoverable("pp_current.n_opt") is True
    assert nm.is_haskell_uncoverable("pp_previous") is True


def test_normalize_haskell_wires_pp_future_end_to_end() -> None:
    """`normalize_haskell` (the public entry point) resolves `pp_future`
    via the `fn:pp_future` derivation registered in `HASKELL_MAP`."""
    nm = _load_normalizer()
    record = _base_record({"proposals": [["g", {"stakePoolTargetNum": 600}]]})
    out = nm.normalize_haskell(record)
    assert out["pp_future"] == {"n_opt": 600}
