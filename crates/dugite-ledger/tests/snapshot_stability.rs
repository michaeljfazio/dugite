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
    LedgerState::new(ProtocolParameters::mainnet_defaults())
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
    // Last update: SNAPSHOT_VERSION 30 → 31 (#919, 2026-07-29) —
    // `ProtocolParameters` (embedded in every `protocol_params` /
    // `prev_protocol_params` field) gained `min_utxo_value: Lovelace` (flat
    // Shelley/Allegra/Mary `minUTxOValue`) and `coins_per_utxo_word: Lovelace`
    // (lossless Alonzo `coinsPerUTxOWord`) — both required for the per-era
    // minimum-UTxO dispatch that fixes false `OutputTooSmall` rejections of
    // real Shelley/Allegra/Mary mainnet transactions. Positional bincode
    // layout change. Pre-existing snapshots are quarantined on load and
    // operators re-sync (no migration shim — see SNAPSHOT_VERSION docs).
    const EXPECTED_HASH: &str = "45d8c48be6338552a0dd04a8fbd38a65eba5d6d14930644bff9351945e745a00";

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
