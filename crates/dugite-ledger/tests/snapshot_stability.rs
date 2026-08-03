//! Snapshot format stability tests.
//!
//! LedgerState uses bincode serialization via LedgerStateSnapshot (field-order-dependent,
//! not self-describing). Adding, removing, or reordering fields in LedgerStateSnapshot
//! BREAKS deserialization of existing snapshots. These tests detect accidental format
//! changes by hashing the serialized output and comparing against a known expected hash.

use dugite_ledger::state::snapshot_format::LedgerStateSnapshot;
use dugite_ledger::LedgerState;

/// The shared, fully-populated fixture (#967).
///
/// Deliberately NOT a bare `LedgerState::new()`. bincode writes nothing for a
/// `None` and nothing for an empty collection, so an empty fixture makes the
/// hash below cover top-level scalars and essentially nothing else — any
/// positional change inside a nested structure is invisible to it.
///
/// That is demonstrated, not theoretical: #966 added `treasury: u64` to
/// `RatificationSnapshot`, a real layout change requiring SNAPSHOT_VERSION
/// 31 -> 32, and this test stayed green through it because
/// `ratification_snapshot` was `None`.
///
/// The fixture lives in the crate rather than here so that ONE definition
/// serves both this hash and the in-crate
/// `fixture_populates_every_snapshot_field`, which exhaustively destructures
/// `LedgerStateSnapshot` and therefore fails to COMPILE when a new field is
/// added without being populated. Two fixtures would drift, and a drifting
/// fixture is how the blind spot opened in the first place.
fn canonical_ledger_state() -> LedgerState {
    dugite_ledger::state::test_fixtures::populated_ledger_state()
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
    // Last update: #988 — SNAPSHOT_VERSION 32 -> 33. `GovernanceState` gained
    // `pulsed_ratify_state`, the frozen DRep pulser result. This IS a layout
    // change: existing snapshots cannot supply the field and are rejected.
    //
    // The prior move (#967) was the opposite case and the distinction is the
    // point of the two rules below: it changed only the FIXTURE, so the hash
    // moved while the on-disk format did not.
    //
    // **Do not bump SNAPSHOT_VERSION for a fixture-only move.** The format is
    // byte-for-byte unchanged; what changed is that the fixture now populates
    // every field, so the serialized bytes are ~2.5 KB of real structure
    // instead of a few hundred bytes of top-level scalars. A hash over an
    // empty fixture and a hash over a populated one are simply not the same
    // number.
    //
    // Two distinct rules for changing this constant, and only the first is a
    // format change:
    //   * The layout of `LedgerStateSnapshot` (or anything nested in it)
    //     changed -> bump SNAPSHOT_VERSION as well. Existing snapshots on disk
    //     are now unreadable.
    //   * `test_fixtures::populated_ledger_state` gained a value -> update
    //     this constant alone. Nothing on disk is affected.
    //
    // Prior baselines:
    //   45d8c48be6338552a0dd04a8fbd38a65eba5d6d14930644bff9351945e745a00
    //     empty fixture, pre-#966
    //   4f914e63503247701e098638d7b2948ed3a1d8d7d0478c7da3e91b1cb706d099
    //     populated fixture, pre-#988 (SNAPSHOT 32)
    //   24039764909a573f5549262574928132b8e15d3bc9a6acc9e550e218db3a02e3
    //     empty fixture + a ratification snapshot (#966, SNAPSHOT 31 -> 32:
    //     `RatificationSnapshot` gained `treasury: u64`. That was a genuine
    //     layout change and this test stayed GREEN through it, because
    //     `ratification_snapshot` was `None` in the fixture — which is what
    //     #967 is about.)
    const EXPECTED_HASH: &str = "4f5f06fba4d024e4c0370f52a3d7f92418dafad8391baf8b04a96126e1e46aaa";

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
         AND bump SNAPSHOT_VERSION in state/mod.rs — unless all you changed was\n\
         the fixture in state/test_fixtures.rs, which moves this hash without\n\
         touching the on-disk format."
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
