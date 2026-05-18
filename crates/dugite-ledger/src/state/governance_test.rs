//! Tests for governance state mutations.
//!
//! This module contains integration-style tests that verify correct handling
//! of `UpdateCommittee` governance actions and replay-based treasury
//! consistency checks against known Koios endpoint values.

use crate::state::governance::{enact, CommitteeExpiration, ConwayUpdateCommittee};
use crate::state::ledger::LedgerState;
use crate::testing::{
    committee_from_genesis, empty_epochs_builder, genesis_config,
};
use log::{info, warn};
use std::collections::BTreeMap;

/// Test that applying an `UpdateCommittee` action via `enact` correctly
/// writes the new members into `committee_expiration`.
///
/// # Setup
/// - Creates a minimal ledger state containing only the genesis committee
///   member (so `committee_expiration.len() == 1`).
/// - Constructs a `ConwayUpdateCommittee` action that adds `N` new members
///   with a specific expiry epoch.
/// - Calls the `enact` path of the governance action.
///
/// # Assertion
/// - After enactment, `committee_expiration.len()` equals `1 + N`.
#[test]
fn test_update_committee_enact() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let genesis_members = committee_from_genesis();
    let state = LedgerState::new_with_committee(genesis_members.clone());
    let initial_len = state.committee_expiration.len();
    assert_eq!(
        initial_len, 1,
        "expected exactly one genesis member in committee_expiration"
    );

    let members_to_add: BTreeMap<Vec<u8>, u64> = [
        (b"cold_key_01".to_vec(), 250),
        (b"cold_key_02".to_vec(), 250),
        (b"cold_key_03".to_vec(), 250),
    ]
    .into_iter()
    .collect();
    let action = ConwayUpdateCommittee {
        members: members_to_add.clone(),
    };

    // Act
    let mut state_clone = state.clone();
    enact(&mut state_clone, action).map_err(|e| {
        format!("enact failed: {e}")
    })?;

    // Assert
    let expected_len = 1 + members_to_add.len();
    let actual_len = state_clone.committee_expiration.len();
    assert_eq!(
        actual_len, expected_len,
        "committee_expiration should contain genesis member + new members, \
         expected {expected_len}, got {actual_len}"
    );

    // Also verify that the added members exist with correct expiries
    for (cold_key, &expiry) in &members_to_add {
        let entry = state_clone.committee_expiration.get(cold_key);
        assert!(
            entry.is_some(),
            "added member {:?} missing from committee_expiration",
            cold_key
        );
        assert_eq!(
            *entry.unwrap(),
            expiry,
            "expiry mismatch for member {:?}",
            cold_key
        );
    }

    info!("test_update_committee_enact passed");
    Ok(())
}

/// Replay the chain from genesis for the first ten epochs and compare the
/// cumulative treasury balance against the Koios `totals` endpoint value
/// for the same epoch range on preview testnet.
///
/// # Caveats
/// - The comparison is approximate: Koios reflects real‑time pot
///   distributions that may include fees from non‑test transactions.
///   This test uses the value observed after creating twenty empty blocks
///   per epoch (mimicking a silent network).
/// - If the discrepancy exceeds a threshold, the test logs a warning
///   instead of failing, because the Koios value could drift.
/// - The expected cumulative treasury is derived from the genesis
///   configuration `treasury_amount` plus the per‑epoch reward pot
///   (reserve 0.05 ada * 10⁹ Lovelace) for ten epochs.
#[test]
fn test_treasury_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let config = genesis_config();

    // Build a state from genesis, then advance 10 epochs with empty blocks.
    let mut state = LedgerState::new_from_config(config.clone());
    let epoch_count = 10;
    let blocks_per_epoch = config.blocks_per_epoch; // typically 2160
    for epoch in 0..epoch_count {
        let ledger = empty_epochs_builder(&state, blocks_per_epoch)
            .finalize();
        state = ledger.into_state();
        info!("Processed epoch {epoch}");
    }

    // Calculate expected cumulative treasury:
    // - Starting treasury from genesis config: 100_000_000 Ada =
    //   100_000_000_000_000 Lovelace
    // - Each epoch the reward pot adds 0.05 Ada per epoch =
    //   50_000_000 Lovelace (preview testnet constants)
    // - No fees or reserve withdrawals occur in empty blocks.
    let genesis_treasury = config.treasury_amount_lovelace;
    let per_epoch_reward = 50_000_000u64; // 0.5 Ada in Lovelace
    let expected_treasury = genesis_treasury + (per_epoch_reward * epoch_count);

    // Query Koios totals endpoint for epoch range [0, 10)
    // (Simulated via a local helper; in production this would be an HTTP call)
    let koios_treasury = match crate::testing::koios::fetch_total_treasury(
        0..10,
    ) {
        Ok(val) => val,
        Err(e) => {
            warn!("Koios query failed, falling back to expected: {e}");
            expected_treasury
        }
    };

    let actual_treasury = state.treasury;
    // Tolerance: small fees from test transactions may shift the Koios value.
    let tolerance = 1_000_000; // 1 ADA
    let diff = if actual_treasury > koios_treasury {
        actual_treasury - koios_treasury
    } else {
        koios_treasury - actual_treasury
    };

    assert!(
        diff <= tolerance,
        "Treasury mismatch after 10 epochs: expected ~{expected_treasury}, \
         actual {actual_treasury}, Koios {koios_treasury}, diff {diff} > tolerance {tolerance}"
    );

    info!("test_treasury_boundary passed (treasury={actual_treasury}, koios={koios_treasury})");
    Ok(())
}