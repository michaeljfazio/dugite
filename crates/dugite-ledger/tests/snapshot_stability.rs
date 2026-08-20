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
/// `PulsingSnapshot`, a real layout change requiring SNAPSHOT_VERSION
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
    // Last update: #977 — SNAPSHOT_VERSION 33 -> 34. `GovernanceState` gained
    // `future_pparams`. Another positional bincode change inside
    // `GovernanceState`, so existing snapshots are rejected.
    //
    // Previously: #988 — SNAPSHOT_VERSION 32 -> 33. `GovernanceState` gained
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
    //   4f5f06fba4d024e4c0370f52a3d7f92418dafad8391baf8b04a96126e1e46aaa
    //     SNAPSHOT 33 (#988, pulsed_ratify_state)
    //   4f914e63503247701e098638d7b2948ed3a1d8d7d0478c7da3e91b1cb706d099
    //     populated fixture, pre-#988 (SNAPSHOT 32)
    //   24039764909a573f5549262574928132b8e15d3bc9a6acc9e550e218db3a02e3
    //     empty fixture + a ratification snapshot (#966, SNAPSHOT 31 -> 32:
    //     `PulsingSnapshot` gained `treasury: u64`. That was a genuine
    //     layout change and this test stayed GREEN through it, because
    //     `ratification_snapshot` was `None` in the fixture — which is what
    //     #967 is about.)
    //   474c7869c31088f4310932825ca44657df5be7bfebd1da93abf57732b1f72f2e
    //     SNAPSHOT 37 (#994, PulsingSnapshot presence flags)
    //
    //   0b78b7f6df8e66b194d3578d74a9c53624742eda20fb8a7aa4685d4fa03f0089
    //     SNAPSHOT 38, #1067 fields only (never released — see
    //     xtask/tests/snapshot_one_bump_invariant.rs)
    //   ea9ce3ba3840ac8d99da93a47062410f322ed7736466b167ca2e536ad2f48e54
    //     SNAPSHOT 38 + #1072/Phase-1a RUPD fields, pre-#1073
    //   0d232f9217eadbb64a7e274dc120e6348178f5dc3d95d2b2c14d940f927e366b
    //     SNAPSHOT 38 + #1084 `DRepRegistration.delegs`, pre-#1085
    //   13ccf15836c55ac86885c9c9a5d0f8aa2347351b8421143cb51e5631454301ba
    //     SNAPSHOT 38 + #1073 `PulsedRatifyState.enact_state`, pre-#1084
    //
    // Current: SNAPSHOT 38, extended in place. #1067 added
    // `EpochSubState.non_myopic` + `PendingRewardUpdate.non_myopic`; #1072 adds
    // `EpochSubState.rupd_pulser_started`; Phase 1a adds
    // `EpochSubState.rupd_monetary`. All are positional bincode
    // additions, so both are layout changes — but 38 was never tagged, so they
    // ship as ONE re-sync rather than two. That invariant is enforced by
    // `xtask/tests/snapshot_one_bump_invariant.rs`, not by convention.
    //
    // #1073 joins them with `PulsedRatifyState.enact_state`: `rsEnactState`'s
    // `ensCommittee` / `ensConstitution` / `ensPrevGovActionIds`, captured from
    // the pulser's own dry run so the frozen plan is one boundary ahead of live
    // governance state exactly as upstream's is.
    //
    // #1084 joins them with `DRepRegistration.delegs` — Haskell's `drepDelegs`,
    // the reverse index a DRep deregistration uses to orphan its delegators.
    // It cannot be derived from the forward map below PV10 (ledger #4772), so
    // it is stored rather than reconstructed.
    //
    // #1085 joins them with `CertSubState.vrf_key_hashes` — Haskell's
    // `psVRFKeyHashes`, the occurrence count that makes the PV11
    // duplicate-VRF-key rejection possible. Also not derivable: POOLREAP
    // deletes a superseded key outright even when another pool still holds it.
    //
    // #1088 (SNAPSHOT 39, same version as #1067/#1073/#1085 — see
    // `state/snapshot.rs`'s SNAPSHOT_VERSION comment) moves this hash for a
    // DIFFERENT reason than every entry above: not a field added or moved,
    // but the FIXTURE widened. Every map/set field in `test_fixtures.rs` now
    // carries 2+ entries instead of ≤1, because a hash-ordered container
    // with 0 or 1 entries has nothing to reorder — the exact blindness that
    // let this test stay green for years while `imbl::HashMap`/`HashMap`
    // fields serialized in RANDOMIZED order. See `EXPECTED_SNAPSHOT_VERSION`
    // below for the guard this pairs with.
    //
    // #1071 (SAME SNAPSHOT_VERSION 39): `EpochSubState` gains `rupd_snapshot`
    // — the WIRE-ONLY `nesRu` mirror (`Option<PulsingRewUpdate>`), persisted
    // alongside the existing `rupd_pulser_started`/`rupd_monetary` pair so a
    // mid-epoch restart does not regress the N2C `Pulsing`/`Complete` arm to
    // `SNothing`. 39 was bumped in anticipation of exactly this addition
    // earlier in the same session (no released snapshot ever carried 39
    // without it), so this is a hash-only update, not a further version bump.
    const EXPECTED_HASH: &str = "48839e6b8429bfd9a7edb8ea40153fffcfcc58e6a4505d663b690a249e408d64";

    if EXPECTED_HASH == "COMPUTE_ON_FIRST_RUN" {
        panic!(
            "Snapshot format hash not yet set. Current hash: {hash_hex}\n\
             Update EXPECTED_HASH in this test with the value above."
        );
    }

    assert_eq!(
        hash_hex,
        EXPECTED_HASH,
        "LedgerStateSnapshot layout for SNAPSHOT_VERSION {} changed.\n\
         \n\
         If this is a genuine format change: bump SNAPSHOT_VERSION in \
         state/snapshot.rs AND update BOTH EXPECTED_HASH and \
         EXPECTED_SNAPSHOT_VERSION in this test to match — two different \
         layouts must never share one SNAPSHOT_VERSION number (that is what \
         `xtask/tests/snapshot_one_bump_invariant.rs` used to catch via `git \
         tag`, and what this pair of constants catches instead, without \
         needing tags or a non-shallow clone).\n\
         \n\
         If this is a fixture-only change (test_fixtures.rs gained or \
         changed a value with no LedgerStateSnapshot field/type change): \
         update EXPECTED_HASH alone.\n\
         \n\
         New hash: {hash_hex}",
        dugite_ledger::LedgerState::snapshot_version(),
    );
}

/// The `SNAPSHOT_VERSION` [`EXPECTED_HASH`] above was computed against.
///
/// This is the direct replacement for
/// `xtask/tests/snapshot_one_bump_invariant.rs` (deleted — its `git
/// tag`-based mechanism for detecting "two different `LedgerStateSnapshot`
/// layouts both claiming the same `SNAPSHOT_VERSION`" was vacuous under CI's
/// shallow `actions/checkout`, which fetches no tags, so the guard had been
/// silently passing on every push since the day it was written).
///
/// The replacement needs no tags and works identically at any clone depth:
/// it pins the hash AND the version number it describes side by side, in one
/// commit, by a human. If `SNAPSHOT_VERSION` moves without this constant
/// moving with it — in EITHER direction, a bump that forgot to update the
/// pair or a layout change that forgot to bump — the assertion below fails
/// with a message naming exactly what to do. `snapshot_format_hash_stability`
/// itself is the layout guard `#1088`'s fix makes trustworthy: before that
/// fix, a multi-entry map field made the hash vary between RUNS of identical
/// code, so pinning a version number to it would have been pinning noise.
#[test]
fn snapshot_hash_is_pinned_to_the_current_snapshot_version() {
    const EXPECTED_SNAPSHOT_VERSION: u8 = 39;

    assert_eq!(
        dugite_ledger::LedgerState::snapshot_version(),
        EXPECTED_SNAPSHOT_VERSION,
        "SNAPSHOT_VERSION changed ({EXPECTED_SNAPSHOT_VERSION} -> {}) without updating \
         this test.\n\
         \n\
         Compute the new layout hash from `snapshot_format_hash_stability`'s failure \
         message (or by temporarily setting its EXPECTED_HASH to a bogus value and \
         re-running), then update BOTH `snapshot_format_hash_stability`'s EXPECTED_HASH \
         and this test's EXPECTED_SNAPSHOT_VERSION together, in the same commit as the \
         SNAPSHOT_VERSION bump.",
        dugite_ledger::LedgerState::snapshot_version(),
    );
}

/// Two INDEPENDENT serializations of logically identical state must produce
/// byte-identical output (#1088) — this is the test that actually proves the
/// fix, rather than re-asserting a fixed hash constant.
///
/// `snapshot_format_hash_stability` pins a single serialization's hash and
/// so cannot, by itself, distinguish "this hash is deterministic" from "this
/// hash happens to be what one process produced". The two calls below are
/// enough to catch that gap WITHOUT a second hand-maintained fixture builder
/// (which would have to be kept in sync with `populated_ledger_state`
/// forever, and drift is exactly how #1088-shaped bugs hide): `std`'s
/// `RandomState::new()` increments a thread-local counter on every call
/// (`k0.wrapping_add(1)`, `library/std/src/collections/hash/map.rs`), so two
/// SEPARATE `LedgerState::new()` calls in the same test process build their
/// `HashMap`s from DIFFERENT seeds — same logical content, same insertion
/// order, still a different underlying hash function per call. A
/// `HashMap`/`imbl::HashMap` field that survived the ordering fix would
/// therefore (overwhelmingly likely, across the ~40 affected fields) iterate
/// differently between the two calls and produce different bytes.
///
/// Proven RED by disarming the fix before writing this test: reverting a
/// single field's wire type from `BTreeMap` back to `HashMap` — see the
/// commit history for #1088 — made this fail on the very first run, with no
/// retry needed. A flaky RED here would mean the seed argument above does
/// not hold in practice; it held.
#[test]
fn snapshot_bytes_are_independent_of_insertion_order() {
    let state_a = dugite_ledger::state::test_fixtures::populated_ledger_state();
    let state_b = dugite_ledger::state::test_fixtures::populated_ledger_state();

    let bytes_a = bincode::serialize(&LedgerStateSnapshot::from(&state_a)).expect("serialize a");
    let bytes_b = bincode::serialize(&LedgerStateSnapshot::from(&state_b)).expect("serialize b");

    assert_eq!(
        bytes_a, bytes_b,
        "two independently-built LedgerState values with identical logical content \
         serialized to DIFFERENT bytes — a map or set reachable from \
         LedgerStateSnapshot is still writing in hash-iteration order instead of key \
         order (std::collections::hash_map::RandomState reseeds every HashMap::new() \
         call, so this catches it even within one process/thread)"
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
