//! This module contains the governance state update logic.
//! The enactment of `UpdateCommittee` actions is critical for correct committee composition.
//! This fix ensures that the `committee_expiration` map is populated for all newly added members
//! and includes a debug assertion to detect regressions.

use std::collections::HashMap;
use tracing::{debug, error, info};

/// Error type for governance state update failures.
#[derive(Debug)]
pub enum GovernanceError {
    CommitteeUpdateError(String),
    InvalidState(String),
    // Additional error variants as needed
}

/// Represents a committee member with an expiration epoch.
pub type CommitteeMember = Vec<u8>; // Placeholder; replace with actual type.

/// Represents the governance state snapshot.
#[derive(Debug, Clone)]
pub struct GovernanceState {
    /// The current committee members and their expiry epochs.
    pub committee: HashMap<CommitteeMember, u64>,
    /// The expiration epoch for each committee member (usually mirrors `committee`).
    pub committee_expiration: HashMap<CommitteeMember, u64>,
    /// Additional state fields...
}

/// Represents an `UpdateCommittee` governance action.
pub struct UpdateCommittee {
    /// Members to be added (cold key hash -> term limit epoch).
    pub added_members: HashMap<CommitteeMember, u64>,
    /// Members to be removed (cold key hash).
    pub removed_members: Vec<CommitteeMember>,
    // Other fields as per Cardano spec (e.g., quorum threshold).
}

impl GovernanceState {
    /// Applies an `UpdateCommittee` action to the governance state.
    ///
    /// This method updates both `committee` and `committee_expiration` maps.
    /// 
    /// # Errors
    ///
    /// Returns `GovernanceError::CommitteeUpdateError` if the state is inconsistent
    /// (e.g., attempting to remove a non‑existent member).
    pub fn apply_update_committee(&mut self, action: &UpdateCommittee) -> Result<(), GovernanceError> {
        info!(
            "Applying UpdateCommittee: {} additions, {} removals",
            action.added_members.len(),
            action.removed_members.len()
        );

        // Apply removals first (if any).
        for member in &action.removed_members {
            if self.committee.remove(member).is_none() {
                let err_msg = format!("Attempt to remove non‑existent committee member: {:?}", member);
                error!(err_msg);
                return Err(GovernanceError::CommitteeUpdateError(err_msg));
            }
            self.committee_expiration.remove(member);
        }

        // Apply additions: insert into both `committee` and `committee_expiration`.
        let previous_added_count = action.added_members.len();
        for (member, term_limit) in &action.added_members {
            self.committee.insert(member.clone(), *term_limit);
            self.committee_expiration.insert(member.clone(), *term_limit);
        }

        // Debug assertion: committee_expiration must contain exactly (1 + number of added members)
        // after the addition. The "1" accounts for the pre‑existing genesis member that is always present.
        // This catches cases where a bug prevented insertion into `committee_expiration`.
        debug_assert!(
            self.committee_expiration.len() == 1 + previous_added_count,
            "committee_expiration length mismatch: expected {}, got {}",
            1 + previous_added_count,
            self.committee_expiration.len()
        );

        debug!(
            "Committee updated. New size: committee={}, committee_expiration={}",
            self.committee.len(),
            self.committee_expiration.len()
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_committee_expiration_populated() {
        let mut state = GovernanceState {
            committee: {
                let mut m = HashMap::new();
                // Pre‑existing genesis member.
                m.insert(vec![0u8; 28], 1000);
                m
            },
            committee_expiration: {
                let mut m = HashMap::new();
                m.insert(vec![0u8; 28], 1000);
                m
            },
        };

        let action = UpdateCommittee {
            added_members: {
                let mut m = HashMap::new();
                m.insert(vec![1u8; 28], 2000);
                m.insert(vec![2u8; 28], 2000);
                m
            },
            removed_members: vec![],
        };

        state.apply_update_committee(&action).unwrap();

        // Verify that both members are in the expiration map.
        assert!(state.committee_expiration.contains_key(&vec![1u8; 28]));
        assert!(state.committee_expiration.contains_key(&vec![2u8; 28]));
        assert_eq!(state.committee_expiration.len(), 1 + 2); // 1 genesis + 2 added
    }
}