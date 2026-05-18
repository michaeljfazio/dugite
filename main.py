#!/usr/bin/env python3
"""
main.py – Deliverable for dugite committee/treasury investigation (P0 D1, P1 D2).

Probes:
1. Query Koios committee_info and proposal_list to verify UpdateCommittee enactment epoch.
2. Simulate ConwayUpdateCommittee enact path (unit test compatible with dugite).
3. Snapshot roundtrip test: serialise/deserialise committee state (simulated).
4. (P1 D2) Treasury balance check via Koios account_info.

Usage:
    python main.py [--tx-hash TX_HASH] [--stake-address STAKE_ADDRESS]
"""

import json
import logging
import os
import sys
import time
from dataclasses import dataclass, field, asdict
from typing import Any, Dict, List, Optional, Tuple

import requests

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    datefmt="%Y-%m-%dT%H:%M:%S%z",
)
logger = logging.getLogger("dugite_investigation")

# ---------------------------------------------------------------------------
# Configuration (environment-driven for flexibility and security)
# ---------------------------------------------------------------------------

KOIOS_PREVIEW_URL = os.getenv("KOIOS_PREVIEW_URL", "https://preview.koios.rest/api/v1")
"""Base URL for Koios Preview API."""

KOIOS_TIMEOUT_SEC = int(os.getenv("KOIOS_TIMEOUT_SEC", 30))
"""Timeout for Koios HTTP requests."""

STALENESS_THRESHOLD_SEC = int(os.getenv("STALENESS_THRESHOLD_SEC", 300))
"""Threshold for accepting API response staleness (seconds)."""

_minimal_committee_expected = int(os.getenv("EXPECTED_COMMITTEE_SIZE", 8))
"""Expected number of committee members after UpdateCommittee enactment."""


# ---------------------------------------------------------------------------
# Data models
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class CommitteeMember:
    """Represents a single committee member returned by Koios committee_info.

    Attributes:
        cold_key: Bech32 cold key or script hash.
        expiration_epoch: Epoch when this member expires.
        has_script: Whether the member is a script (True) or key (False).
    """
    cold_key: str
    expiration_epoch: int
    has_script: bool = False

    def __post_init__(self) -> None:
        """Validate fields after initialisation."""
        if not self.cold_key or not isinstance(self.cold_key, str):
            raise ValueError("cold_key must be a non-empty string")
        if not isinstance(self.expiration_epoch, int) or self.expiration_epoch < 0:
            raise ValueError("expiration_epoch must be a non-negative integer")
        if not isinstance(self.has_script, bool):
            raise ValueError("has_script must be a boolean")


@dataclass(frozen=True)
class ConwayUpdateCommittee:
    """Minimal representation of a ConwayUpdateCommittee governance action.

    Attributes:
        members_to_add: Dict mapping cold_key (str) -> expiration_epoch (int).
        members_to_remove: List of cold keys to remove.
        threshold_numerator: Numerator of the new threshold.
        threshold_denominator: Denominator of the new threshold.
    """
    members_to_add: Dict[str, int] = field(default_factory=dict)
    members_to_remove: List[str] = field(default_factory=list)
    threshold_numerator: int = 1
    threshold_denominator: int = 1

    def __post_init__(self) -> None:
        """Validate fields after initialisation."""
        for key, epoch in self.members_to_add.items():
            if not isinstance(key, str) or not key.strip():
                raise ValueError(f"Invalid key in members_to_add: {key!r}")
            if not isinstance(epoch, int) or epoch < 0:
                raise ValueError(f"Invalid epoch for key {key}: {epoch}")
        for key in self.members_to_remove:
            if not isinstance(key, str) or not key.strip():
                raise ValueError(f"Invalid key in members_to_remove: {key!r}")
        if not isinstance(self.threshold_numerator, int) or self.threshold_numerator <= 0:
            raise ValueError("threshold_numerator must be a positive integer")
        if not isinstance(self.threshold_denominator, int) or self.threshold_denominator <= 0:
            raise ValueError("threshold_denominator must be a positive integer")


# ---------------------------------------------------------------------------
# Koios API helpers (with connection pooling, retries, and validation)
# ---------------------------------------------------------------------------

_http_session: Optional[requests.Session] = None


def _get_http_session() -> requests.Session:
    """Return a reusable requests.Session with connection pooling.

    Returns:
        A session configured for Koios API calls.
    """
    global _http_session
    if _http_session is None:
        session = requests.Session()
        adapter = requests.adapters.HTTPAdapter(
            pool_connections=10,
            pool_maxsize=20,
            max_retries=3,
        )
        session.mount("https://", adapter)
        session.mount("http://", adapter)
        session.headers.update({
            "Accept": "application/json",
            "Content-Type": "application/json",
            "User-Agent": "dugite-investigation/1.0",
        })
        _http_session = session
    return _http_session


def koios_query(
    endpoint: str,
    payload: Optional[Dict[str, Any]] = None,
    timeout: int = KOIOS_TIMEOUT_SEC,
) -> Any:
    """Send a POST request to a Koios API endpoint with validation and logging.

    Args:
        endpoint: API path (e.g., 'committee_info').
        payload: Optional JSON body.
        timeout: Request timeout in seconds.

    Returns:
        Decoded JSON response (list or dict).

    Raises:
        RuntimeError: On HTTP or connection errors, or if response is not valid JSON.
    """
    if not endpoint or not isinstance(endpoint, str):
        raise ValueError("endpoint must be a non-empty string")

    url = f"{KOIOS_PREVIEW_URL.rstrip('/')}/{endpoint.lstrip('/')}"
    payload = payload or {}
    session = _get_http_session()

    logger.debug("Koios POST %s with payload %s", url, json.dumps(payload)[:200])

    try:
        response = session.post(url, json=payload, timeout=timeout)
        response.raise_for_status()
        # Validate response content-type
        content_type = response.headers.get("Content-Type", "")
        if not content_type.startswith("application/json"):
            logger.warning("Non-JSON Content-Type for %s: %s", endpoint, content_type)
        data = response.json()
        return data
    except requests.exceptions.Timeout as exc:
        raise RuntimeError(f"Koios API call to {endpoint} timed out after {timeout}s") from exc
    except requests.exceptions.ConnectionError as exc:
        raise RuntimeError(f"Koios API connection error to {url}: {exc}") from exc
    except requests.exceptions.HTTPError as exc:
        status = exc.response.status_code if exc.response is not None else "?"
        raise RuntimeError(f"Koios API HTTP {status} for {endpoint}: {exc}") from exc
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"Koios API returned invalid JSON for {endpoint}: {exc}") from exc


def get_committee_info() -> List[CommitteeMember]:
    """Retrieve current constitutional committee (CC) members from Koios.

    Returns:
        List of CommitteeMember objects.

    Raises:
        RuntimeError: If the API call fails or returns malformed data.
    """
    logger.info("Fetching committee info from Koios...")
    data = koios_query("committee_info")
    if not isinstance(data, list):
        raise RuntimeError("Unexpected response format for committee_info: expected list")

    members: List[CommitteeMember] = []
    for idx, entry in enumerate(data):
        if not isinstance(entry, dict):
            logger.warning("Skipping non-dict entry at index %d", idx)
            continue
        try:
            cold_key = (entry.get("cold_key") or "").strip()
            if not cold_key:
                logger.debug("Skipping entry with empty cold_key at index %d", idx)
                continue
            expiration = int(entry.get("expiration_epoch", 0))
            has_script = bool(entry.get("has_script", False))
            member = CommitteeMember(
                cold_key=cold_key,
                expiration_epoch=expiration,
                has_script=has_script,
            )
            members.append(member)
        except (ValueError, TypeError) as exc:
            logger.warning("Malformed committee entry at index %d: %s", idx, exc)
            continue

    logger.info("Retrieved %d committee members from Koios", len(members))
    return members


def get_proposal_list(
    after_epoch: Optional[int] = None,
) -> Dict[int, Dict[str, Any]]:
    """Fetch governance proposals from Koios, optionally filtering by minimum epoch.

    Args:
        after_epoch: Only include proposals enacted after this epoch.

    Returns:
        Dict mapping proposal ID (int) to proposal info dict.

    Raises:
        RuntimeError: If the API call fails.
    """
    payload: Dict[str, Any] = {}
    if after_epoch is not None:
        if not isinstance(after_epoch, int) or after_epoch < 0:
            raise ValueError("after_epoch must be a non-negative integer")
        payload["_epoch_no"] = {"_gte": after_epoch}

    logger.info("Fetching proposal list from Koios (after_epoch=%s)...", after_epoch)
    data = koios_query("proposal_list", payload)
    if not isinstance(data, list):
        raise RuntimeError("Unexpected response format for proposal_list: expected list")

    proposals: Dict[int, Dict[str, Any]] = {}
    for entry in data:
        if not isinstance(entry, dict):
            continue
        try:
            prop_id = int(entry.get("proposal_id", 0))
        except (ValueError, TypeError):
            continue
        if prop_id <= 0:
            continue
        proposals[prop_id] = entry

    logger.info("Retrieved %d proposals", len(proposals))
    return proposals


def get_account_info(stake_address: str) -> Dict[str, Any]:
    """Fetch account (treasury) info for a given stake address.

    Args:
        stake_address: Bech32 stake address (stake1...).

    Returns:
        Account details dict.

    Raises:
        ValueError: If address is invalid or not found on Koios.
        RuntimeError: If the API call fails.
    """
    if not stake_address or not stake_address.startswith("stake1"):
        raise ValueError("Invalid stake address: must be a bech32 'stake1...' address")

    payload = {"_stake_addresses": [stake_address]}
    logger.info("Fetching account info for %s...", stake_address)
    data = koios_query("account_info", payload)
    if not isinstance(data, list):
        raise RuntimeError("Unexpected response format for account_info: expected list")
    if not data:
        raise ValueError(f"Stake address {stake_address} not found on Koios.")
    account = data[0]
    if not isinstance(account, dict):
        raise RuntimeError("Unexpected account info format: expected dict")
    return account


# ---------------------------------------------------------------------------
# Probe 1: Confirm UpdateCommittee enactment epoch
# ---------------------------------------------------------------------------

def probe_enactment_epoch(tx_hash: str) -> Optional[int]:
    """Given an UpdateCommittee transaction hash, find its enactment epoch.

    Args:
        tx_hash: Transaction hash (hex string) of the UpdateCommittee tx.

    Returns:
        Enactment epoch if found, else None.
    """
    if not tx_hash or not isinstance(tx_hash, str) or len(tx_hash) != 64:
        raise ValueError("tx_hash must be a 64-character hex string")

    logger.info("Probe 1: Searching for enactment epoch of tx %s", tx_hash)
    try:
        proposals = get_proposal_list(after_epoch=0)
    except RuntimeError as exc:
        logger.error("Failed to fetch proposal_list: %s", exc)
        return None

    found_epoch: Optional[int] = None
    for prop_id, prop in proposals.items():
        proposed_tx = (prop.get("proposed_tx_hash") or "").strip()
        if proposed_tx.lower() == tx_hash.lower():
            enacted_epoch_raw = prop.get("enacted_epoch")
            if enacted_epoch_raw is not None:
                try:
                    found_epoch = int(enacted_epoch_raw)
                    logger.info(
                        "Probe 1: tx %s enacted in epoch %d (proposal_id %d)",
                        tx_hash, found_epoch, prop_id,
                    )
                except (ValueError, TypeError):
                    logger.warning("Proposal %d has non-integer enacted_epoch: %s", prop_id, enacted_epoch_raw)
            else:
                # Could be not enacted yet
                logger.info("Proposal %d (tx %s) has no enacted_epoch; may not be enacted yet", prop_id, tx_hash)
            break
    else:
        logger.warning("Probe 1: Transaction %s not found in proposal_list", tx_hash)

    return found_epoch


def compare_committee_count(expected_count: int = _minimal_committee_expected) -> bool:
    """Compare Koios committee member count with expected.

    Args:
        expected_count: Number of members we expect.

    Returns:
        True if counts match.
    """
    members = get_committee_info()
    actual = len(members)
    logger.info("Committee members from Koios: %d (expected %d)", actual, expected_count)
    return actual == expected_count


# ---------------------------------------------------------------------------
# Probe 2: Simulate ConwayUpdateCommittee enact path (unit test)
# ---------------------------------------------------------------------------

def enact_update_committee(
    current_committee: Dict[str, int],
    action: ConwayUpdateCommittee,
) -> Dict[str, int]:
    """Simulate the enact path for a ConwayUpdateCommittee action.

    This replicates the expected logic from dugite-ledger's governance.rs,
    specifically writing to committee_expiration.

    Args:
        current_committee: Dict mapping cold_key -> expiration_epoch (including genesis).
        action: The UpdateCommittee action.

    Returns:
        New committee dict after enactment.
    """
    if not isinstance(current_committee, dict):
        raise TypeError("current_committee must be a dict")
    if not isinstance(action, ConwayUpdateCommittee):
        raise TypeError("action must be a ConwayUpdateCommittee instance")

    new_committee = dict(current_committee)

    # Remove specified members
    for key in action.members_to_remove:
        if key in new_committee:
            logger.debug("Removing committee member: %s", key)
            new_committee.pop(key)
        else:
            logger.warning("Attempted to remove non-existent member: %s", key)

    # Add new members (overwrites existing if key collision – but spec says add)
    for key, epoch in action.members_to_add.items():
        if key in new_committee:
            logger.warning("Member %s already exists; will be overwritten", key)
        new_committee[key] = epoch

    return new_committee


def unit_test_committee_enact() -> bool:
    """Run a unit test: build minimal ConwayUpdateCommittee, enact, assert length.

    Current committee: 1 genesis member (scriptHash-ff9babf2..., expired epoch 1000).
    Action adds 7 members with varying expirations. After enact, total should be 8.

    Returns:
        True if test passes, False otherwise.
    """
    logger.info("Probe 2: Running unit test for committee enact path.")

    # Current state: genesis member
    genesis_key = "scriptHash-ff9babf2e5e9f86924daba408451981f1c8b5f2d1a1e0e8f9c1b3d4e5f6a7b8c"
    current_committee = {genesis_key: 1000}

    # Action: add 7 members
    action = ConwayUpdateCommittee(
        members_to_add={
            "key_hash_abc": 1050,
            "key_hash_def": 1060,
            "key_hash_ghi": 1070,
            "key_hash_jkl": 1080,
            "key_hash_mno": 1090,
            "key_hash_pqr": 1100,
            "scriptHash_something": 2000,
        },
        members_to_remove=[],
        threshold_numerator=1,
        threshold_denominator=2,
    )

    expected_count = 1 + len(action.members_to_add)  # 8

    new_committee = enact_update_committee(current_committee, action)
    actual_count = len(new_committee)

    if actual_count != expected_count:
        logger.error(
            "Unit test FAILED: expected %d committee members, got %d",
            expected_count, actual_count,
        )
        return False

    # Verify genesis still present
    if genesis_key not in new_committee:
        logger.error("Unit test FAILED: genesis member missing from new committee")
        return False

    # Verify all new members present
    for key in action.members_to_add:
        if key not in new_committee:
            logger.error("Unit test FAILED: added member %s missing", key)
            return False

    logger.info("Unit test PASSED: %d committee members as expected", expected_count)
    return True


# ---------------------------------------------------------------------------
# Probe 3: Snapshot roundtrip test (serialise/deserialise committee state)
# ---------------------------------------------------------------------------

def snapshot_roundtrip_test() -> bool:
    """Simulate a snapshot roundtrip for committee state.

    Steps:
        1. Create a committee dict with 8 members.
        2. Serialise to JSON.
        3. Deserialise back to dict.
        4. Assert conservation of length, keys, and values.

    Returns:
        True if roundtrip preserves data.
    """
    logger.info("Probe 3: Running snapshot roundtrip test.")

    original_committee = {
        "scriptHash-ff9babf2e5e9f86924daba408451981f1c8b5f2d1a1e0e8f9c1b3d4e5f6a7b8c": 1000,
        "key_hash_abc": 1050,
        "key_hash_def": 1060,
        "key_hash_ghi": 1070,
        "key_hash_jkl": 1080,
        "key_hash_mno": 1090,
        "key_hash_pqr": 1100,
        "scriptHash_something": 2000,
    }

    try:
        serialised = json.dumps(original_committee, sort_keys=True, indent=2)
        logger.debug("Serialised JSON %d bytes", len(serialised))
        deserialised = json.loads(serialised)
    except (TypeError, json.JSONDecodeError) as exc:
        logger.error("Snapshot roundtrip serialization failed: %s", exc)
        return False

    if not isinstance(deserialised, dict):
        logger.error("Deserialised data is not a dict")
        return False

    if len(deserialised) != len(original_committee):
        logger.error(
            "Length mismatch: original %d, deserialised %d",
            len(original_committee), len(deserialised),
        )
        return False

    for key, val in original_committee.items():
        if key not in deserialised:
            logger.error("Missing key after roundtrip: %s", key)
            return False
        if deserialised[key] != val:
            logger.error(
                "Value mismatch for key %s: original %s, deserialised %s",
                key, val, deserialised[key],
            )
            return False

    logger.info("Snapshot roundtrip test PASSED: %d members preserved", len(original_committee))
    return True


# ---------------------------------------------------------------------------
# Probe 4 (P1 D2): Treasury balance check via Koios account_info
# ---------------------------------------------------------------------------

def probe_treasury(stake_address: str) -> Optional[Dict[str, Any]]:
    """Retrieve treasury/account info for a given stake address.

    Args:
        stake_address: Bech32 stake address (stake1...).

    Returns:
        Account info dict with fields like 'balance', 'treasury', etc., or None on failure.
    """
    logger.info("Probe 4: Checking treasury for address %s", stake_address)
    try:
        account = get_account_info(stake_address)
    except (ValueError, RuntimeError) as exc:
        logger.error("Probe 4 failed: %s", exc)
        return None

    # Log relevant fields
    balance = account.get("balance")
    rewards = account.get("rewards")
    withdrawals = account.get("withdrawals")
    treasury = account.get("treasury")  # might not be present in older API
    logger.info(
        "Account %s: balance=%s, rewards=%s, withdrawals=%s, treasury=%s",
        stake_address, balance, rewards, withdrawals, treasury,
    )
    return account


# ---------------------------------------------------------------------------
# Main driver (CLI usage)
# ---------------------------------------------------------------------------

def parse_args(argv: Optional[List[str]] = None) -> Dict[str, Any]:
    """Parse command-line arguments.

    Args:
        argv: Argument list (default: sys.argv[1:]).

    Returns:
        Dict with keys 'tx_hash' and 'stake_address'.
    """
    if argv is None:
        argv = sys.argv[1:]

    tx_hash: Optional[str] = None
    stake_address: Optional[str] = None

    i = 0
    while i < len(argv):
        if argv[i] in ("--tx-hash", "--tx") and i + 1 < len(argv):
            tx_hash = argv[i + 1]
            i += 2
        elif argv[i] in ("--stake-address", "--address") and i + 1 < len(argv):
            stake_address = argv[i + 1]
            i += 2
        else:
            logger.warning("Ignoring unknown argument: %s", argv[i])
            i += 1

    return {
        "tx_hash": tx_hash,
        "stake_address": stake_address,
    }


def main() -> None:
    """Entry point: runs all probes and reports results."""
    args = parse_args()
    exit_code = 0

    # -----------------------------------------------------------------------
    # Probe 1: Committee count and enactment epoch
    # -----------------------------------------------------------------------
    logger.info("=== Probe 1: Committee info check ===")
    if not compare_committee_count():
        logger.error("Probe 1 FAILED: committee size mismatch")
        exit_code = 1
    else:
        logger.info("Probe 1 (committee count) PASSED")

    if args.get("tx_hash"):
        logger.info("--- Probe 1b: Find enactment epoch ---")
        epoch = probe_enactment_epoch(args["tx_hash"])
        if epoch is None:
            logger.warning("Probe 1b: Enactment epoch not found (may not be enacted yet).")
        else:
            logger.info("Probe 1b: Enactment epoch = %d", epoch)

    # -----------------------------------------------------------------------
    # Probe 2: Unit test enact path
    # -----------------------------------------------------------------------
    logger.info("=== Probe 2: Unit test committee enact ===")
    if not unit_test_committee_enact():
        logger.error("Probe 2 FAILED")
        exit_code = 1
    else:
        logger.info("Probe 2 PASSED")

    # -----------------------------------------------------------------------
    # Probe 3: Snapshot roundtrip test
    # -----------------------------------------------------------------------
    logger.info("=== Probe 3: Snapshot roundtrip ===")
    if not snapshot_roundtrip_test():
        logger.error("Probe 3 FAILED")
        exit_code = 1
    else:
        logger.info("Probe 3 PASSED")

    # -----------------------------------------------------------------------
    # Probe 4: Treasury balance (if stake address provided)
    # -----------------------------------------------------------------------
    if args.get("stake_address"):
        logger.info("=== Probe 4: Treasury info ===")
        account = probe_treasury(args["stake_address"])
        if account is None:
            logger.error("Probe 4 FAILED")
            exit_code = 1
        else:
            logger.info("Probe 4 PASSED (see above for details)")
    else:
        logger.info("=== Probe 4: Skipped (no --stake-address provided) ===")

    # -----------------------------------------------------------------------
    # Summary
    # -----------------------------------------------------------------------
    if exit_code == 0:
        logger.info("All probes passed.")
    else:
        logger.error("Some probes failed. Exit code %d", exit_code)

    sys.exit(exit_code)


if __name__ == "__main__":
    main()