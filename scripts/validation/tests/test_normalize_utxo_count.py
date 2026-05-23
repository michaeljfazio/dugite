"""Unit tests for `normalize-epoch-dump.py::_count_utxo` (Bug 4 of #615).

The Haskell `cardano-cli debug log-epoch-state` dumper emits the UTxO
map as `{}` even when the live UTxO set is populated (the field is
intentionally suppressed).  The old normalizer collapsed this to
`len({}) == 0`, producing a false-positive divergence against dugite
which DOES emit the true count.  The fix returns `None` when neither
`utxo` nor `utxosUtxo` is enumerable so the diff tool marks the field
"uncoverable" rather than reporting a divergence.

Run with:
    python3 -m pytest scripts/validation/tests/test_normalize_utxo_count.py
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


def test_count_utxo_returns_none_when_haskell_utxo_is_empty_dict() -> None:
    """The classic Haskell schema gap: `utxo: {}` AND no `utxosUtxo`
    fallback.  Must return None (uncoverable), not 0 (false zero).
    """
    nm = _load_normalizer()
    record = {
        "currentEpochState": {
            "esLState": {
                "utxoState": {
                    "utxo": {},
                    # utxosUtxo absent
                }
            }
        }
    }
    assert nm._count_utxo(record) is None


def test_count_utxo_returns_none_when_both_paths_empty() -> None:
    """Both `utxo` and `utxosUtxo` present but both empty -- still
    uncoverable."""
    nm = _load_normalizer()
    record = {
        "currentEpochState": {
            "esLState": {
                "utxoState": {
                    "utxo": {},
                    "utxosUtxo": {},
                }
            }
        }
    }
    assert nm._count_utxo(record) is None


def test_count_utxo_honours_populated_utxo() -> None:
    """When `utxo` IS populated (fixture data, reduced dumps), use it."""
    nm = _load_normalizer()
    record = {
        "currentEpochState": {
            "esLState": {
                "utxoState": {
                    "utxo": {f"tx{i}#{i}": {} for i in range(5)},
                }
            }
        }
    }
    assert nm._count_utxo(record) == 5


def test_count_utxo_falls_through_to_utxos_utxo() -> None:
    """Some Haskell dumps put the data under `utxosUtxo`.  Use it when
    `utxo` is empty/absent."""
    nm = _load_normalizer()
    record = {
        "currentEpochState": {
            "esLState": {
                "utxoState": {
                    "utxo": {},
                    "utxosUtxo": {f"tx{i}#{i}": {} for i in range(3)},
                }
            }
        }
    }
    assert nm._count_utxo(record) == 3


def test_count_utxo_returns_none_when_no_utxo_state() -> None:
    """No utxoState at all -- still uncoverable."""
    nm = _load_normalizer()
    assert nm._count_utxo({}) is None
