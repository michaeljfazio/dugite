//! Snapshot format stability tests.
//!
//! LedgerState uses bincode serialization via LedgerStateSnapshot (field-order-dependent,
//! not self-describing). Adding, removing, or reordering fields in LedgerStateSnapshot
//! BREAKS deserialization of existing snapshots. These tests detect accidental format
//! changes by hashing the serialized output and comparing against a known expected hash.

use dugite_ledger::state::snapshot_format::LedgerStateSnapshot;
use dugite_ledger::LedgerState;
use dugite_primitives::protocol_params::ProtocolParameters;

/// Create a deterministic LedgerState with known default values.
fn canonical_ledger_state() -> LedgerState {
    let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

    // Populate the governance ratification snapshot.
    //
    // WHY: `LedgerState::new()` alone leaves every Option `None` and every
    // collection empty, so the format hash below only ever covered top-level
    // scalars. A positional bincode change INSIDE a nested optional structure
    // was invisible to it — the guard could not go red for exactly the class
    // of change it exists to catch.
    //
    // That is not hypothetical: adding `treasury` to `RatificationSnapshot`
    // (#966) is a real layout change requiring SNAPSHOT_VERSION 31 -> 32, and
    // this test stayed green through it because `ratification_snapshot` was
    // `None` in the fixture. A guard that cannot fail is not a guard.
    //
    // `RatificationSnapshot` is embedded in `GovernanceState`, which is
    // serialized as part of every `LedgerState` snapshot, so populating it
    // here brings its layout under the hash.
    state.capture_ratification_snapshot();

    state
}

/// Round-trip: serialize → deserialize → serialize produces identical bytes.
#[test]
fn snapshot_round_trip_deterministic() {
    let state = canonical_ledger_state();

    let snap1 = LedgerStateSnapshot::from(&state);
    let bytes1 = bincode::serialize(&snap1).expect("serialize 1");
    let snap2: LedgerStateSnapshot = bincode::deserialize(&bytes1).expect("deserialize");
    let state2 = LedgerState::from(snap2);
    let snap3 = LedgerStateSnapshot::from(&state2);
    let bytes2 = bincode::serialize(&snap3).expect("serialize 2");

    assert_eq!(
        bytes1, bytes2,
        "Round-trip serialization produced different bytes — bincode format is not stable"
    );
}

/// Hash the serialized bytes and compare against a known value.
/// If this test fails, it means the LedgerStateSnapshot serialization format has changed,
/// which will break deserialization of existing snapshots on disk.
///
/// To update: run the test, copy the new hash from the failure message, and update
/// the EXPECTED_HASH constant below. Only do this intentionally when bumping
/// SNAPSHOT_VERSION.
#[test]
fn snapshot_format_hash_stability() {
    let state = canonical_ledger_state();
    let snap = LedgerStateSnapshot::from(&state);
    let bytes = bincode::serialize(&snap).expect("serialize");

    // blake2b-256 of the serialized bytes
    let hash = dugite_primitives::hash::blake2b_256(&bytes);
    let hash_hex = hash
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    // This hash was computed from the current LedgerStateSnapshot layout.
    // If this changes, existing snapshot files become unreadable.
    //
    // Last update: SNAPSHOT_VERSION 31 → 32 (#966, 2026-08-02) —
    // `RatificationSnapshot` gained `treasury: u64` (Haskell `ensTreasury`,
    // the frozen pot `withdrawalCanWithdraw` gates `TreasuryWithdrawals`
    // against). It was the one `dpEnactState` term never captured, so
    // ratification fell back to the LIVE treasury, which already included the
    // current boundary's `applyRUpd` — one boundary newer than Haskell's,
    // enacting withdrawals an epoch early.
    //
    // The same commit populates the fixture with a ratification snapshot. The
    // fixture was previously a bare `LedgerState::new()`, so nested optional
    // structures were absent from the serialized bytes and this hash could not
    // detect a layout change inside them — the #966 field was added and this
    // test stayed green. The hash therefore moves for TWO reasons here: the
    // new field, and the fixture now actually covering the structure.
    //
    // Prior baseline (empty fixture, pre-#966):
    //   45d8c48be6338552a0dd04a8fbd38a65eba5d6d14930644bff9351945e745a00
    const EXPECTED_HASH: &str = "24039764909a573f5549262574928132b8e15d3bc9a6acc9e550e218db3a02e3";

    if EXPECTED_HASH == "COMPUTE_ON_FIRST_RUN" {
        panic!(
            "Snapshot format hash not yet set. Current hash: {hash_hex}\n\
             Update EXPECTED_HASH in this test with the value above."
        );
    }

    assert_eq!(
        hash_hex, EXPECTED_HASH,
        "LedgerState serialization format changed — existing snapshots will be incompatible.\n\
         If this change was intentional, update EXPECTED_HASH to: {hash_hex}\n\
         and bump SNAPSHOT_VERSION in state/mod.rs."
    );
}

/// Verify the snapshot header format (magic + version + checksum).
#[test]
fn snapshot_save_load_round_trip() {
    let state = canonical_ledger_state();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("test_snapshot.bin");

    state.save_snapshot(&path).expect("save snapshot");

    let loaded = LedgerState::load_snapshot(&path).expect("load snapshot");

    // Compare key fields
    assert_eq!(state.epoch, loaded.epoch);
    assert_eq!(state.era, loaded.era);
    assert_eq!(state.epochs.treasury, loaded.epochs.treasury);
    assert_eq!(state.epochs.reserves, loaded.epochs.reserves);
    assert_eq!(state.epoch_length, loaded.epoch_length);
    assert_eq!(
        state.consensus.evolving_nonce,
        loaded.consensus.evolving_nonce
    );
    assert_eq!(state.consensus.epoch_nonce, loaded.consensus.epoch_nonce);
}

/// Verify that the snapshot file starts with the expected header.
#[test]
fn snapshot_file_has_correct_header() {
    let state = canonical_ledger_state();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("test_snapshot.bin");

    state.save_snapshot(&path).expect("save snapshot");

    let raw = std::fs::read(&path).expect("read snapshot");
    assert!(raw.len() >= 37, "snapshot file too small");
    assert_eq!(&raw[0..4], b"DUGT", "missing DUGT magic");
    assert!(raw[4] > 0 && raw[4] < 128, "invalid version byte");
}
