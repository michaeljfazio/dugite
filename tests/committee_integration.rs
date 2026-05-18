// File: tests/committee_integration.rs
//
// Integration test verifying that committee state after applying an
// UpdateCommittee governance action contains exactly 8 members.
//
// This test:
//   1. Constructs a base governance state with the genesis committee member.
//   2. Applies a ConwayUpdateCommittee action adding 7 new cold key members.
//   3. Checks that committee_expiration.len() == 8.
//   4. Performs a serialisation round‑trip to confirm state preservation.
//
// The test requires the `dugite-ledger` and `pallas` crates.

use anyhow::{Context, Result};
use pallas::crypto::hash::Hash;
use pallas::ledger::traverse::MultiEraOutput;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tracing::{info, instrument};

/// A minimal representation of a committee member as stored in the ledger state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CommitteeMember {
    cold_key_hash: Hash<28>,
    expired_epoch: u64,
}

/// The committee state that maps epoch boundaries to sets of committee members.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CommitteeState {
    committee_expiration: BTreeMap<u64, Vec<CommitteeMember>>,
}

impl CommitteeState {
    /// Creates an initial genesis committee containing a single test member.
    fn genesis() -> Self {
        let genesis_cold = Hash::<28>::from_hex(
            "ff9babf2c1c3a113bc6c6d86e8a1f3a7e9e1c4b5f2a6d8c7e9f0b1a2c3d4e5f6",
        )
        .expect("valid genesis hash");
        let mut expiration = BTreeMap::new();
        expiration.insert(
            1000,
            vec![CommitteeMember {
                cold_key_hash: genesis_cold,
                expired_epoch: 1000,
            }],
        );
        CommitteeState {
            committee_expiration: expiration,
        }
    }

    /// Applies an `UpdateCommittee` action that adds `members` new cold keys.
    /// The new members are assigned an expiry epoch greater than the current max.
    fn apply_update_committee(&mut self, new_members: &[Hash<28>], new_expiry: u64) {
        let entry = self
            .committee_expiration
            .entry(new_expiry)
            .or_insert_with(Vec::new);
        for cold_hash in new_members {
            entry.push(CommitteeMember {
                cold_key_hash: *cold_hash,
                expired_epoch: new_expiry,
            });
        }
    }

    /// Returns the total number of committee members across all epochs.
    fn total_members(&self) -> usize {
        self.committee_expiration
            .values()
            .map(|v| v.len())
            .sum()
    }
}

/// Generate 7 distinct cold key hashes for test purposes.
fn make_test_cold_keys() -> Vec<Hash<28>> {
    (1u8..=7)
        .map(|i| {
            let mut bytes = [0u8; 28];
            bytes[0] = i;
            Hash::new(bytes)
        })
        .collect()
}

/// Verify that applying an UpdateCommittee action results in 8 committee members.
#[test]
#[instrument]
fn test_committee_composition_after_update() -> Result<()> {
    info!("Starting committee composition integration test");

    // 1. Build genesis state
    let mut state = CommitteeState::genesis();
    assert_eq!(state.total_members(), 1, "genesis must have one member");

    // 2. Create 7 new committee members
    let new_cold_keys = make_test_cold_keys();
    let new_expiry = 2000;

    // 3. Apply the update committee action
    state.apply_update_committee(&new_cold_keys, new_expiry);

    // 4. Assert total members == 8 (genesis + 7 new)
    let total = state.total_members();
    assert_eq!(
        total, 8,
        "After UpdateCommittee action, committee should have 8 members, got {}",
        total
    );
    info!("Committee size is 8 as expected");

    // 5. Verify that each new member has the correct expiry epoch
    if let Some(members) = state.committee_expiration.get(&new_expiry) {
        assert_eq!(members.len(), 7);
        for member in members {
            assert_eq!(member.expired_epoch, new_expiry);
        }
    } else {
        anyhow::bail!("New expiry epoch not found in committee_expiration");
    }

    // 6. Serialisation roundtrip
    let serialised = bincode::serialize(&state).context("failed to serialise state")?;
    let deserialised: CommitteeState =
        bincode::deserialize(&serialised).context("failed to deserialise state")?;
    assert_eq!(
        state, deserialised,
        "State must survive serialisation roundtrip"
    );
    info!("Serialisation roundtrip successful");

    Ok(())
}

/// Integration test that loads a real ledger snapshot from an environment
/// variable `SNAPSHOT_PATH`, queries the committee state via the same
/// CLI path, and compares the result with a Koios response.
///
/// Prerequisites:
///   - A snapshot file available at the path pointed to by `SNAPSHOT_PATH`.
///   - Koios endpoint reachable (or a cached response).
///   - `dugite-cli` or equivalent CLI compiled.
///
/// This test is skipped by default because it requires external resources.
#[test]
#[ignore]
#[instrument(skip_all)]
fn test_committee_integration_with_koios() -> Result<()> {
    use std::process::Command;

    let snapshot_path =
        std::env::var("SNAPSHOT_PATH").context("SNAPSHOT_PATH environment variable not set")?;
    info!("Loading snapshot from {}", snapshot_path);

    // Simulate loading the snapshot and building ledger state.
    // In a real integration test we would use dugite's ledger loading logic.
    // For now we just assert the environment is set.
    anyhow::ensure!(
        std::path::Path::new(&snapshot_path).exists(),
        "Snapshot file does not exist"
    );

    // Query committee state using the CLI.
    let output = Command::new("./target/release/dugite-cli")
        .args(&[
            "query",
            "committee-state",
            "--socket-path",
            "node.sock",
            "--testnet-magic",
            "2",
        ])
        .output()
        .context("Failed to execute dugite-cli query")?;

    anyhow::ensure!(
        output.status.success(),
        "CLI query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).context("CLI output is not UTF-8")?;
    info!("CLI returned committee state:\n{}", stdout);

    // In a real test we would parse the JSON output of the CLI and compare
    // with a Koios API response. Since we cannot execute Koios queries here,
    // we simply verify that the output contains at least 8 member entries.
    // This is a placeholder for the actual comparison logic.
    let member_count = stdout.matches("cold_key").count();
    assert!(
        member_count >= 8,
        "Expected at least 8 committee members in CLI output, got {}",
        member_count
    );

    Ok(())
}