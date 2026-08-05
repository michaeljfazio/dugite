//! Governance query handlers (tags 23, 24, 25, 26, 27, 28, 39).

use tracing::debug;

use super::filter::{
    filter_arg, read_credential, read_drep, read_gov_action_id, OnEmptySet, SetArgShape,
};
use crate::node::n2c_query::types::{
    DRepDelegationGroup, DRepKey, GovStateSnapshot, NodeStateSnapshot, QueryResult,
};

/// Handle GetConstitution (tag 23).
pub(crate) fn handle_constitution(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetConstitution");
    QueryResult::Constitution {
        url: state.constitution_url.clone(),
        data_hash: state.constitution_hash.clone(),
        script_hash: state.constitution_script.clone(),
    }
}

/// Handle GetGovState (tag 24).
pub(crate) fn handle_gov_state(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetGovState");
    QueryResult::GovState(Box::new(gov_state_snapshot(state)))
}

/// Build `ConwayGovState` from the node snapshot.
///
/// Shared by `GetGovState` (tag 24) and `GetRatifyState` (tag 32) — tag 32
/// needs the same `EnactState`, and building it a second time by hand is how
/// tag 24's copy came to be a hardcoded empty pulser while tag 32's was real
/// (#992). Also reused by `GetDebugEpochState`/`GetDebugNewEpochState` (tags
/// 8/12, #1027) for the embedded `UTxOState.utxosGovState` field — the same
/// "build it by hand a second time" trap that produced #992 is exactly what
/// left `DebugNewEpochState`'s `GovState` slot as a bare `array(0)`
/// placeholder for as long as it was.
pub(crate) fn gov_state_snapshot(state: &NodeStateSnapshot) -> GovStateSnapshot {
    GovStateSnapshot {
        proposals: state.governance_proposals.clone(),
        committee: state.committee.clone(),
        constitution_url: state.constitution_url.clone(),
        constitution_hash: state.constitution_hash.clone(),
        constitution_script: state.constitution_script.clone(),
        cur_pparams: Box::new(state.protocol_params.clone()),
        prev_pparams: Box::new(state.prev_protocol_params.clone()),
        // #994: the EnactState inside `nextRatifyState` reports what the pulser
        // SEALED, not the live params. Identical mid-chain; differs only before
        // the first boundary.
        enact_cur_pparams: Box::new(state.ratify_cur_protocol_params.clone()),
        enacted_pparam_update: state.enacted_pparam_update.clone(),
        enacted_hard_fork: state.enacted_hard_fork.clone(),
        enacted_committee: state.enacted_committee.clone(),
        enacted_constitution: state.enacted_constitution.clone(),
        treasury: state.treasury,
        future_pparams_tag: state.future_pparams_tag,
        future_pparams: state.future_pparams.clone(),
        // The embedded `DRepPulsingState` (#992). Every component comes from
        // the same field the dedicated query for it serves, so tag 24 cannot
        // disagree with tags 25/26/31/32 about what the pulser froze.
        pulser_proposals: state.governance_proposals_frozen.clone(),
        pulser_drep_distr: state.drep_stake_distr.clone(),
        pulser_drep_state: state.drep_entries.clone(),
        pulser_pool_distr: state.stake_pools.clone(),
        ratify_enacted: state.ratify_enacted.clone(),
        ratify_expired: state.ratify_expired.clone(),
        ratify_delayed: state.ratify_delayed,
    }
}

/// Handle GetProposals (tag 31) — filtered governance proposals.
///
/// Argument: tag(258) Set<GovActionId> where GovActionId = [tx_hash(32), action_index]
/// Returns: Seq (GovActionState)
///
/// Per the proven Haskell mechanism (#922), `queryProposals` NEVER reads live
/// `cgsProposals` — it reads the DRep pulsing state's frozen proposal list
/// (`dpProposals` / `psProposals`), refreshed only at epoch boundaries. This
/// handler must therefore answer from `governance_proposals_frozen` (the
/// #903 `PulsingSnapshot`-sourced view), NOT `governance_proposals` (the
/// live view `GetGovState`/tag 24 legitimately uses for its embedded
/// `ConwayGovState.cgsProposals`). Reading the live field here was the #922
/// bug: mid-epoch submissions appeared in dugite's answer immediately instead
/// of only after the next epoch boundary rotates the pulser.
pub(crate) fn handle_proposals(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetProposals");
    // `queryProposals nes gids | null gids = proposals` — an explicit `null`
    // guard, so an empty set means every proposal.
    let filter_ids = match filter_arg(
        decoder,
        "GetProposals",
        SetArgShape::Required,
        OnEmptySet::AllItems,
        read_gov_action_id,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    match filter_ids {
        None => QueryResult::Proposals(state.governance_proposals_frozen.clone()),
        Some(ids) => {
            let filtered = state
                .governance_proposals_frozen
                .iter()
                .filter(|p| {
                    ids.iter()
                        .any(|(tx_id, idx)| tx_id == &p.tx_id && *idx == p.action_index)
                })
                .cloned()
                .collect();
            QueryResult::Proposals(filtered)
        }
    }
}

/// Handle GetRatifyState (tag 32) — current ratification state.
///
/// Returns: array(4) [enacted_seq, expired_seq, delayed_bool, future_pparam_update]
/// The Haskell node computes this from the DRep pulsing state. We return the
/// results from the most recent epoch transition's ratification pass.
pub(crate) fn handle_ratify_state(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetRatifyState");
    let gov = gov_state_snapshot(state);
    QueryResult::RatifyState {
        enacted: gov.ratify_enacted.clone(),
        expired: gov.ratify_expired.clone(),
        delayed: gov.ratify_delayed,
        gov: Box::new(gov),
    }
}

/// Handle GetDRepState (tag 25).
///
/// Argument: tag(258) Set<Credential> where Credential = [0|1, hash(28)]
pub(crate) fn handle_drep_state(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetDRepState");
    // `queryDRepState nes creds | null creds = <every DRep>` — explicit guard,
    // so an empty set means everything.
    let filter_creds = match filter_arg(
        decoder,
        "GetDRepState",
        SetArgShape::Required,
        OnEmptySet::AllItems,
        read_credential,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    match filter_creds {
        None => QueryResult::DRepState(state.drep_entries.clone()),
        Some(creds) => {
            let filtered = state
                .drep_entries
                .iter()
                .filter(|d| creds.iter().any(|(_, h)| h == &d.credential_hash))
                .cloned()
                .collect();
            QueryResult::DRepState(filtered)
        }
    }
}

/// Handle GetDRepStakeDistr (tag 26).
///
/// Argument: `Set DRep`. Returns `Map DRep Coin` — total delegated stake per
/// DRep, read from the frozen `psDRepDistr` (#950).
///
/// `queryDRepStakeDistr nes creds | null creds = <every DRep> | otherwise =
/// distr `Map.restrictKeys` creds`. dugite ignored the argument entirely until
/// #963 — it did not even consume the bytes — and answered with every DRep for
/// every request, so a client asking about one DRep was told about all of them.
pub(crate) fn handle_drep_stake_distr(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetDRepStakeDistr");
    let filter_dreps = match filter_arg(
        decoder,
        "GetDRepStakeDistr",
        SetArgShape::Required,
        OnEmptySet::AllItems,
        read_drep,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    match filter_dreps {
        None => QueryResult::DRepStakeDistr(state.drep_stake_distr.clone()),
        Some(dreps) => {
            let filtered = state
                .drep_stake_distr
                .iter()
                .filter(|d| {
                    dreps
                        .iter()
                        .any(|(kind, hash)| *kind == d.drep_type && hash == &d.drep_hash)
                })
                .cloned()
                .collect();
            QueryResult::DRepStakeDistr(filtered)
        }
    }
}

/// Handle GetCommitteeMembersState (tag 27).
pub(crate) fn handle_committee_state(state: &NodeStateSnapshot) -> QueryResult {
    debug!("Query: GetCommitteeMembersState");
    // The second element is what makes `NextEpochChange` computable at all:
    // it is defined as a comparison of the live committee against the one this
    // epoch's ratification pass will install (#1020). `ratify_enacted` is the
    // frozen `rsEnacted` — the same source `GetRatifyState` answers from, so
    // the two queries cannot disagree about the incoming committee.
    let next = super::encoding::committee_after_enacted(&state.committee, &state.ratify_enacted);
    QueryResult::CommitteeState(state.committee.clone(), next)
}

/// Handle GetFilteredVoteDelegatees (tag 28).
///
/// Argument: tag(258) Set<Credential>
/// Returns: Map<Credential, DRep> -- vote delegation for filtered credentials
pub(crate) fn handle_filtered_vote_delegatees(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetFilteredVoteDelegatees");
    // `getFilteredVoteDelegatees ss creds | Set.null creds = <all accounts>`.
    let filter_creds = match filter_arg(
        decoder,
        "GetFilteredVoteDelegatees",
        SetArgShape::Required,
        OnEmptySet::AllItems,
        read_credential,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    match filter_creds {
        None => QueryResult::FilteredVoteDelegatees(state.vote_delegatees.clone()),
        Some(creds) => {
            let filtered = state
                .vote_delegatees
                .iter()
                .filter(|v| {
                    creds
                        .iter()
                        .any(|(k, h)| *k == v.credential_type && h == &v.credential_hash)
                })
                .cloned()
                .collect();
            QueryResult::FilteredVoteDelegatees(filtered)
        }
    }
}

/// Handle GetDRepDelegations (tag 39, N2C V23+).
///
/// Argument: tag(258) Set<DRep>
///   DRep = `array(2) [0|1, bstr(28)]` | `array(1) [2|3]`
///
/// Returns: `Map<DRep, Set<Credential Staking>>`
///
/// Per Haskell `ouroboros-consensus-cardano` `Shelley/Ledger/Query.hs`, the
/// request is a set of DReps to look up; the response returns, for each
/// requested DRep, the set of stake credentials currently delegating to it.
/// An empty request set means "return all DRep groups that have at least one
/// delegator" (matches the all-DReps view derived from the ledger).
///
/// This is the OPPOSITE orientation of `GetFilteredVoteDelegatees` (tag 28),
/// which is keyed by stake credential.
pub(crate) fn handle_drep_delegations(
    state: &NodeStateSnapshot,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetDRepDelegations (tag 39)");
    let requested = match filter_arg(
        decoder,
        "GetDRepDelegations",
        SetArgShape::Required,
        OnEmptySet::AllItems,
        read_drep,
    ) {
        Ok(f) => f,
        Err(e) => return *e,
    };
    let Some(requested) = requested else {
        // No filter — return all known DRep groups (every DRep that has at
        // least one delegator in the ledger).
        return QueryResult::DRepDelegations(state.drep_delegations.clone());
    };
    // Filter: return one group per requested DRep, with possibly-empty
    // credential set if no delegators currently point at that DRep.  This
    // mirrors the Haskell semantics for `Map.restrictKeys` over the
    // DRep→delegators map.
    let filtered: Vec<DRepDelegationGroup> = requested
        .into_iter()
        .map(|(drep_type, drep_hash)| {
            let drep = DRepKey {
                drep_type,
                drep_hash,
            };
            let credentials = state
                .drep_delegations
                .iter()
                .find(|g| g.drep == drep)
                .map(|g| g.credentials.clone())
                .unwrap_or_default();
            DRepDelegationGroup { drep, credentials }
        })
        .collect();
    QueryResult::DRepDelegations(filtered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::n2c_query::types::{
        DRepDelegationGroup, DRepKey, GovActionId, NodeStateSnapshot, ProposalSnapshot,
    };

    fn make_state_with_proposals() -> NodeStateSnapshot {
        let proposals = vec![
            ProposalSnapshot {
                tx_id: vec![1u8; 32],
                action_index: 0,
                action_type: "InfoAction".to_string(),
                proposed_epoch: 100,
                expires_epoch: 106,
                yes_votes: 10,
                no_votes: 2,
                abstain_votes: 1,
                deposit: 100_000_000_000,
                return_addr: vec![0u8; 29],
                anchor_url: "https://example.com/proposal1".to_string(),
                anchor_hash: vec![0xAA; 32],
                gov_action: dugite_primitives::transaction::GovAction::InfoAction,
                committee_votes: vec![],
                drep_votes: vec![(vec![0xCC; 28], 0, 1)], // one DRep Yes vote
                spo_votes: vec![],
            },
            ProposalSnapshot {
                tx_id: vec![2u8; 32],
                action_index: 1,
                action_type: "ParameterChange".to_string(),
                proposed_epoch: 101,
                expires_epoch: 107,
                yes_votes: 5,
                no_votes: 3,
                abstain_votes: 0,
                deposit: 100_000_000_000,
                return_addr: vec![0u8; 29],
                anchor_url: "https://example.com/proposal2".to_string(),
                anchor_hash: vec![0xBB; 32],
                gov_action: dugite_primitives::transaction::GovAction::InfoAction,
                committee_votes: vec![],
                drep_votes: vec![],
                spo_votes: vec![(vec![0xDD; 28], 0)], // one SPO No vote
            },
        ];
        NodeStateSnapshot {
            governance_proposals: proposals.clone(),
            // `handle_proposals` (GetProposals, tag 31) reads the FROZEN
            // view (#922); most tests here don't care about the live/frozen
            // distinction, so mirror the live list unless a test explicitly
            // wants to exercise a live/frozen divergence (see the dedicated
            // #922 tests below).
            governance_proposals_frozen: proposals,
            ..NodeStateSnapshot::default()
        }
    }

    #[test]
    fn test_proposals_no_filter() {
        let state = make_state_with_proposals();
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(0).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_proposals(&state, &mut dec);
        match result {
            QueryResult::Proposals(proposals) => {
                assert_eq!(proposals.len(), 2);
            }
            _ => panic!("Expected Proposals"),
        }
    }

    #[test]
    fn test_proposals_filtered() {
        let state = make_state_with_proposals();
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(1).ok();
            // GovActionId: [tx_hash, action_index]
            enc.array(2).ok();
            enc.bytes(&[1u8; 32]).ok();
            enc.u32(0).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_proposals(&state, &mut dec);
        match result {
            QueryResult::Proposals(proposals) => {
                assert_eq!(proposals.len(), 1);
                assert_eq!(proposals[0].tx_id, vec![1u8; 32]);
                assert_eq!(proposals[0].action_index, 0);
            }
            _ => panic!("Expected Proposals"),
        }
    }

    #[test]
    fn test_ratify_state_returns_empty() {
        let state = NodeStateSnapshot::default();
        let result = handle_ratify_state(&state);
        match result {
            QueryResult::RatifyState {
                gov: _,
                enacted,
                expired,
                delayed,
            } => {
                assert!(enacted.is_empty());
                assert!(expired.is_empty());
                assert!(!delayed);
            }
            _ => panic!("Expected RatifyState"),
        }
    }

    #[test]
    fn test_ratify_state_with_enacted_and_expired() {
        let enacted_proposal = ProposalSnapshot {
            tx_id: vec![0xAA; 32],
            action_index: 0,
            action_type: "NoConfidence".to_string(),
            proposed_epoch: 100,
            expires_epoch: 110,
            yes_votes: 5,
            no_votes: 1,
            abstain_votes: 0,
            deposit: 500_000_000,
            return_addr: vec![0xBB; 29],
            anchor_url: "https://example.com".to_string(),
            anchor_hash: vec![0xCC; 32],
            gov_action: dugite_primitives::transaction::GovAction::InfoAction,
            committee_votes: Vec::new(),
            drep_votes: Vec::new(),
            spo_votes: Vec::new(),
        };
        let enacted_id = GovActionId {
            tx_id: vec![0xAA; 32],
            action_index: 0,
        };
        let expired_id = GovActionId {
            tx_id: vec![0xDD; 32],
            action_index: 1,
        };
        let state = NodeStateSnapshot {
            ratify_enacted: vec![(enacted_proposal, enacted_id)],
            ratify_expired: vec![expired_id],
            ratify_delayed: true,
            ..NodeStateSnapshot::default()
        };
        let result = handle_ratify_state(&state);
        match result {
            QueryResult::RatifyState {
                gov: _,
                enacted,
                expired,
                delayed,
            } => {
                assert_eq!(enacted.len(), 1);
                assert_eq!(enacted[0].0.action_type, "NoConfidence");
                assert_eq!(enacted[0].1.tx_id, vec![0xAA; 32]);
                assert_eq!(expired.len(), 1);
                assert_eq!(expired[0].tx_id, vec![0xDD; 32]);
                assert_eq!(expired[0].action_index, 1);
                assert!(delayed);
            }
            _ => panic!("Expected RatifyState"),
        }
    }

    #[test]
    fn test_constitution() {
        let state = NodeStateSnapshot {
            constitution_url: "https://example.com/constitution".to_string(),
            constitution_hash: vec![0xAB; 32],
            constitution_script: Some(vec![0xCD; 28]),
            ..NodeStateSnapshot::default()
        };
        let result = handle_constitution(&state);
        match result {
            QueryResult::Constitution {
                url,
                data_hash,
                script_hash,
            } => {
                assert_eq!(url, "https://example.com/constitution");
                assert_eq!(data_hash, vec![0xAB; 32]);
                assert_eq!(script_hash, Some(vec![0xCD; 28]));
            }
            _ => panic!("Expected Constitution"),
        }
    }

    #[test]
    fn test_constitution_no_script() {
        let state = NodeStateSnapshot {
            constitution_url: "https://example.com/c".to_string(),
            constitution_hash: vec![0xAB; 32],
            constitution_script: None,
            ..NodeStateSnapshot::default()
        };
        let result = handle_constitution(&state);
        match result {
            QueryResult::Constitution { script_hash, .. } => {
                assert!(script_hash.is_none());
            }
            _ => panic!("Expected Constitution"),
        }
    }

    #[test]
    fn test_gov_state() {
        let state = make_state_with_proposals();
        let result = handle_gov_state(&state);
        match result {
            QueryResult::GovState(gs) => {
                assert_eq!(gs.proposals.len(), 2);
                assert_eq!(gs.constitution_url, "");
                // cur_pparams and prev_pparams should both be defaults
                assert_eq!(gs.cur_pparams.min_fee_a, gs.prev_pparams.min_fee_a);
            }
            _ => panic!("Expected GovState"),
        }
    }

    #[test]
    fn test_gov_state_with_enacted_roots() {
        let state = NodeStateSnapshot {
            enacted_pparam_update: Some((vec![0xAA; 32], 0)),
            enacted_hard_fork: Some((vec![0xBB; 32], 1)),
            ..NodeStateSnapshot::default()
        };
        let result = handle_gov_state(&state);
        match result {
            QueryResult::GovState(gs) => {
                assert_eq!(gs.enacted_pparam_update, Some((vec![0xAA; 32], 0)));
                assert_eq!(gs.enacted_hard_fork, Some((vec![0xBB; 32], 1)));
                assert!(gs.enacted_committee.is_none());
                assert!(gs.enacted_constitution.is_none());
            }
            _ => panic!("Expected GovState"),
        }
    }

    #[test]
    fn test_drep_state_no_filter() {
        use crate::node::n2c_query::types::DRepSnapshot;
        let state = NodeStateSnapshot {
            drep_entries: vec![
                DRepSnapshot {
                    credential_hash: vec![0xAA; 28],
                    credential_type: 0,
                    deposit: 500_000_000,
                    anchor_url: Some("https://example.com".to_string()),
                    anchor_hash: Some(vec![0xBB; 32]),
                    expiry_epoch: 200,
                    delegator_hashes: vec![],
                },
                DRepSnapshot {
                    credential_hash: vec![0xCC; 28],
                    credential_type: 1,
                    deposit: 500_000_000,
                    anchor_url: None,
                    anchor_hash: None,
                    expiry_epoch: 300,
                    delegator_hashes: vec![vec![0xDD; 28]],
                },
            ],
            ..NodeStateSnapshot::default()
        };
        // Empty filter = return all
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(0).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_drep_state(&state, &mut dec);
        match result {
            QueryResult::DRepState(dreps) => {
                assert_eq!(dreps.len(), 2);
            }
            _ => panic!("Expected DRepState"),
        }
    }

    #[test]
    fn test_drep_state_filtered() {
        use crate::node::n2c_query::types::DRepSnapshot;
        let state = NodeStateSnapshot {
            drep_entries: vec![
                DRepSnapshot {
                    credential_hash: vec![0xAA; 28],
                    credential_type: 0,
                    deposit: 500_000_000,
                    anchor_url: None,
                    anchor_hash: None,
                    expiry_epoch: 200,
                    delegator_hashes: vec![],
                },
                DRepSnapshot {
                    credential_hash: vec![0xCC; 28],
                    credential_type: 0,
                    deposit: 500_000_000,
                    anchor_url: None,
                    anchor_hash: None,
                    expiry_epoch: 300,
                    delegator_hashes: vec![],
                },
            ],
            ..NodeStateSnapshot::default()
        };
        // Filter for credential 0xAA only
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(1).ok();
            enc.array(2).ok();
            enc.u8(0).ok(); // KeyHash
            enc.bytes(&[0xAA; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_drep_state(&state, &mut dec);
        match result {
            QueryResult::DRepState(dreps) => {
                assert_eq!(dreps.len(), 1);
                assert_eq!(dreps[0].credential_hash, vec![0xAA; 28]);
            }
            _ => panic!("Expected DRepState"),
        }
    }

    #[test]
    fn test_committee_state() {
        use crate::node::n2c_query::types::{CommitteeMemberSnapshot, CommitteeSnapshot};
        let state = NodeStateSnapshot {
            committee: CommitteeSnapshot {
                members: vec![CommitteeMemberSnapshot {
                    cold_credential: vec![0xAA; 28],
                    cold_credential_type: 0,
                    hot_status: 0,
                    hot_credential: Some(vec![0xBB; 28]),
                    hot_credential_type: 0,
                    member_status: 0,
                    expiry_epoch: Some(500),
                }],
                threshold: Some((2, 3)),
                current_epoch: 42,
            },
            ..NodeStateSnapshot::default()
        };
        let result = handle_committee_state(&state);
        match result {
            QueryResult::CommitteeState(committee, _) => {
                assert_eq!(committee.members.len(), 1);
                assert_eq!(committee.threshold, Some((2, 3)));
                assert_eq!(committee.current_epoch, 42);
            }
            _ => panic!("Expected CommitteeState"),
        }
    }

    /// Issue #157: When a CC member uses a script hot key, `hot_credential_type`
    /// must be 1 (ScriptHash), not 0 (KeyHash).  The query handler reads the
    /// value directly from `CommitteeMemberSnapshot.hot_credential_type`; the
    /// node query builder (query.rs) is responsible for populating it correctly
    /// from `GovernanceState.script_committee_hot_credentials`.
    ///
    /// This test verifies that the query handler faithfully propagates whatever
    /// `hot_credential_type` value the node placed in the snapshot.
    #[test]
    fn test_committee_state_script_hot_credential_type_propagated() {
        use crate::node::n2c_query::types::{CommitteeMemberSnapshot, CommitteeSnapshot};

        // Member 1: key cold + script hot (the fix: hot_credential_type = 1)
        // Member 2: key cold + key hot (hot_credential_type = 0, unchanged)
        let state = NodeStateSnapshot {
            committee: CommitteeSnapshot {
                members: vec![
                    CommitteeMemberSnapshot {
                        cold_credential: vec![0xAA; 28],
                        cold_credential_type: 0, // KeyHash cold
                        hot_status: 0,           // Authorized
                        hot_credential: Some(vec![0xCC; 28]),
                        hot_credential_type: 1, // ScriptHash hot — must survive round-trip
                        member_status: 0,
                        expiry_epoch: Some(300),
                    },
                    CommitteeMemberSnapshot {
                        cold_credential: vec![0xBB; 28],
                        cold_credential_type: 0, // KeyHash cold
                        hot_status: 0,           // Authorized
                        hot_credential: Some(vec![0xDD; 28]),
                        hot_credential_type: 0, // KeyHash hot
                        member_status: 0,
                        expiry_epoch: Some(400),
                    },
                ],
                threshold: Some((3, 5)),
                current_epoch: 100,
            },
            ..NodeStateSnapshot::default()
        };

        let result = handle_committee_state(&state);
        match result {
            QueryResult::CommitteeState(committee, _) => {
                assert_eq!(committee.members.len(), 2);

                // First member: script hot key — hot_credential_type must be 1
                let m0 = &committee.members[0];
                assert_eq!(m0.cold_credential, vec![0xAA; 28]);
                assert_eq!(
                    m0.hot_credential_type, 1,
                    "script hot key must have hot_credential_type = 1"
                );
                assert_eq!(m0.hot_credential, Some(vec![0xCC; 28]));

                // Second member: key hot key — hot_credential_type must be 0
                let m1 = &committee.members[1];
                assert_eq!(m1.cold_credential, vec![0xBB; 28]);
                assert_eq!(
                    m1.hot_credential_type, 0,
                    "key hot key must have hot_credential_type = 0"
                );
            }
            _ => panic!("Expected CommitteeState"),
        }
    }

    /// `hot_credential_type` for a resigned member (hot_status = 2) must be 0
    /// and hot_credential must be None (no hot key to report).
    #[test]
    fn test_committee_state_resigned_member_no_hot_type() {
        use crate::node::n2c_query::types::{CommitteeMemberSnapshot, CommitteeSnapshot};

        let state = NodeStateSnapshot {
            committee: CommitteeSnapshot {
                members: vec![CommitteeMemberSnapshot {
                    cold_credential: vec![0xAA; 28],
                    cold_credential_type: 0,
                    hot_status: 2, // Resigned
                    hot_credential: None,
                    hot_credential_type: 0,
                    member_status: 0,
                    expiry_epoch: Some(200),
                }],
                threshold: Some((1, 1)),
                current_epoch: 10,
            },
            ..NodeStateSnapshot::default()
        };

        let result = handle_committee_state(&state);
        match result {
            QueryResult::CommitteeState(committee, _) => {
                let m = &committee.members[0];
                assert_eq!(m.hot_status, 2, "resigned member must have hot_status = 2");
                assert!(m.hot_credential.is_none());
                assert_eq!(m.hot_credential_type, 0);
            }
            _ => panic!("Expected CommitteeState"),
        }
    }

    #[test]
    fn test_drep_stake_distr() {
        use crate::node::n2c_query::types::DRepStakeEntry;
        let state = NodeStateSnapshot {
            drep_stake_distr: vec![
                DRepStakeEntry {
                    drep_type: 0,
                    drep_hash: Some(vec![0xAA; 28]),
                    stake: 1_000_000_000,
                },
                DRepStakeEntry {
                    drep_type: 2, // AlwaysAbstain
                    drep_hash: None,
                    stake: 500_000_000,
                },
            ],
            ..NodeStateSnapshot::default()
        };
        // `Nothing`/empty set: `queryDRepStakeDistr` guards on `null creds`.
        let mut dec = minicbor::Decoder::new(&[]);
        match handle_drep_stake_distr(&state, &mut dec) {
            QueryResult::DRepStakeDistr(entries) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].stake, 1_000_000_000);
                assert_eq!(entries[1].drep_type, 2);
            }
            other => panic!("Expected DRepStakeDistr, got {other:?}"),
        }
    }

    /// #963: tag 26 ignored its `Set DRep` argument entirely — it did not even
    /// consume the bytes — so every request was answered with every DRep.
    #[test]
    fn test_drep_stake_distr_honours_its_filter() {
        use crate::node::n2c_query::types::DRepStakeEntry;
        let state = NodeStateSnapshot {
            drep_stake_distr: vec![
                DRepStakeEntry {
                    drep_type: 0,
                    drep_hash: Some(vec![0xAA; 28]),
                    stake: 1_000_000_000,
                },
                DRepStakeEntry {
                    drep_type: 0,
                    drep_hash: Some(vec![0xBB; 28]),
                    stake: 7,
                },
                DRepStakeEntry {
                    drep_type: 2,
                    drep_hash: None,
                    stake: 500_000_000,
                },
            ],
            ..NodeStateSnapshot::default()
        };

        // `Just {DRepKeyHash 0xAA}` — exactly one entry.
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.tag(minicbor::data::Tag::new(258)).unwrap();
        enc.array(1).unwrap();
        enc.array(2).unwrap();
        enc.u8(0).unwrap();
        enc.bytes(&[0xAA; 28]).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        match handle_drep_stake_distr(&state, &mut dec) {
            QueryResult::DRepStakeDistr(e) => {
                assert_eq!(e.len(), 1, "asked for one DRep, got a superset");
                assert_eq!(e[0].drep_hash, Some(vec![0xAA; 28]));
            }
            other => panic!("Expected DRepStakeDistr, got {other:?}"),
        }

        // The payload-less `AlwaysAbstain` constructor is selectable too.
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.tag(minicbor::data::Tag::new(258)).unwrap();
        enc.array(1).unwrap();
        enc.array(1).unwrap();
        enc.u8(2).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        match handle_drep_stake_distr(&state, &mut dec) {
            QueryResult::DRepStakeDistr(e) => {
                assert_eq!(e.len(), 1);
                assert_eq!(e[0].drep_type, 2);
            }
            other => panic!("Expected DRepStakeDistr, got {other:?}"),
        }

        // A malformed argument must not degrade to "every DRep".
        let mut buf = Vec::new();
        minicbor::Encoder::new(&mut buf).u32(9).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        assert!(matches!(
            handle_drep_stake_distr(&state, &mut dec),
            QueryResult::Error(_)
        ));
    }

    #[test]
    fn test_filtered_vote_delegatees_no_filter() {
        use crate::node::n2c_query::types::VoteDelegateeEntry;
        let state = NodeStateSnapshot {
            vote_delegatees: vec![
                VoteDelegateeEntry {
                    credential_hash: vec![0xAA; 28],
                    credential_type: 0,
                    drep_type: 0,
                    drep_hash: Some(vec![0xBB; 28]),
                },
                VoteDelegateeEntry {
                    credential_hash: vec![0xCC; 28],
                    credential_type: 0,
                    drep_type: 2, // AlwaysAbstain
                    drep_hash: None,
                },
            ],
            ..NodeStateSnapshot::default()
        };
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(0).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_filtered_vote_delegatees(&state, &mut dec);
        match result {
            QueryResult::FilteredVoteDelegatees(entries) => {
                assert_eq!(entries.len(), 2);
            }
            _ => panic!("Expected FilteredVoteDelegatees"),
        }
    }

    #[test]
    fn test_filtered_vote_delegatees_filtered() {
        use crate::node::n2c_query::types::VoteDelegateeEntry;
        let state = NodeStateSnapshot {
            vote_delegatees: vec![
                VoteDelegateeEntry {
                    credential_hash: vec![0xAA; 28],
                    credential_type: 0,
                    drep_type: 0,
                    drep_hash: Some(vec![0xBB; 28]),
                },
                VoteDelegateeEntry {
                    credential_hash: vec![0xCC; 28],
                    credential_type: 0,
                    drep_type: 2,
                    drep_hash: None,
                },
            ],
            ..NodeStateSnapshot::default()
        };
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(1).ok();
            enc.array(2).ok();
            enc.u8(0).ok();
            enc.bytes(&[0xCC; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_filtered_vote_delegatees(&state, &mut dec);
        match result {
            QueryResult::FilteredVoteDelegatees(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].credential_hash, vec![0xCC; 28]);
                assert_eq!(entries[0].drep_type, 2);
            }
            _ => panic!("Expected FilteredVoteDelegatees"),
        }
    }

    // ─── GetDRepDelegations (tag 39, V23+) ──────────────────────────────────

    /// Build a snapshot with three DRep groups: one KeyHash DRep with two
    /// delegators, one AlwaysAbstain group with one delegator, and one
    /// AlwaysNoConfidence group with one (script) delegator.
    fn make_drep_delegations_state() -> NodeStateSnapshot {
        NodeStateSnapshot {
            drep_delegations: vec![
                DRepDelegationGroup {
                    drep: DRepKey {
                        drep_type: 0,
                        drep_hash: Some(vec![0xBB; 28]),
                    },
                    credentials: vec![(0, vec![0xAA; 28]), (1, vec![0xAB; 28])],
                },
                DRepDelegationGroup {
                    drep: DRepKey {
                        drep_type: 2,
                        drep_hash: None,
                    },
                    credentials: vec![(0, vec![0xCC; 28])],
                },
                DRepDelegationGroup {
                    drep: DRepKey {
                        drep_type: 3,
                        drep_hash: None,
                    },
                    credentials: vec![(1, vec![0xDD; 28])],
                },
            ],
            ..NodeStateSnapshot::default()
        }
    }

    /// Empty request set returns every known DRep group.
    #[test]
    fn test_drep_delegations_no_filter_returns_all() {
        let state = make_drep_delegations_state();
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(0).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_drep_delegations(&state, &mut dec);
        match result {
            QueryResult::DRepDelegations(groups) => {
                assert_eq!(groups.len(), 3);
                assert_eq!(groups[0].credentials.len(), 2);
            }
            _ => panic!("Expected DRepDelegations"),
        }
    }

    /// Request for a known KeyHash DRep returns its full delegator set.
    #[test]
    fn test_drep_delegations_filtered_by_keyhash_drep() {
        let state = make_drep_delegations_state();
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(1).ok();
            enc.array(2).ok();
            enc.u8(0).ok(); // KeyHash DRep
            enc.bytes(&[0xBB; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_drep_delegations(&state, &mut dec);
        match result {
            QueryResult::DRepDelegations(groups) => {
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].drep.drep_type, 0);
                assert_eq!(groups[0].drep.drep_hash, Some(vec![0xBB; 28]));
                assert_eq!(
                    groups[0].credentials,
                    vec![(0, vec![0xAA; 28]), (1, vec![0xAB; 28])]
                );
            }
            _ => panic!("Expected DRepDelegations"),
        }
    }

    /// Request for AlwaysAbstain DRep returns its delegators.
    #[test]
    fn test_drep_delegations_filtered_always_abstain() {
        let state = make_drep_delegations_state();
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(1).ok();
            enc.array(1).ok();
            enc.u8(2).ok(); // AlwaysAbstain
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_drep_delegations(&state, &mut dec);
        match result {
            QueryResult::DRepDelegations(groups) => {
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].drep.drep_type, 2);
                assert!(groups[0].drep.drep_hash.is_none());
                assert_eq!(groups[0].credentials, vec![(0, vec![0xCC; 28])]);
            }
            _ => panic!("Expected DRepDelegations"),
        }
    }

    /// Request for a DRep not present in the ledger returns a group with an
    /// empty credential set (`Map.restrictKeys`-style semantics).
    #[test]
    fn test_drep_delegations_filtered_unknown_drep_empty_set() {
        let state = make_drep_delegations_state();
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(1).ok();
            enc.array(2).ok();
            enc.u8(0).ok();
            enc.bytes(&[0xFF; 28]).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_drep_delegations(&state, &mut dec);
        match result {
            QueryResult::DRepDelegations(groups) => {
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].drep.drep_type, 0);
                assert_eq!(groups[0].drep.drep_hash, Some(vec![0xFF; 28]));
                assert!(groups[0].credentials.is_empty());
            }
            _ => panic!("Expected DRepDelegations"),
        }
    }

    /// Empty ledger state + empty request → empty result.
    #[test]
    fn test_drep_delegations_empty_state_no_filter() {
        let state = NodeStateSnapshot::default();
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(0).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_drep_delegations(&state, &mut dec);
        match result {
            QueryResult::DRepDelegations(groups) => {
                assert!(groups.is_empty());
            }
            _ => panic!("Expected DRepDelegations"),
        }
    }

    /// Request mixing all four DRep variants: each yields its own group.
    #[test]
    fn test_drep_delegations_request_all_variants() {
        let state = make_drep_delegations_state();
        let cbor = {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(4).ok();
            // KeyHash DRep
            enc.array(2).ok();
            enc.u8(0).ok();
            enc.bytes(&[0xBB; 28]).ok();
            // ScriptHash DRep (not present in state)
            enc.array(2).ok();
            enc.u8(1).ok();
            enc.bytes(&[0x99; 28]).ok();
            // AlwaysAbstain
            enc.array(1).ok();
            enc.u8(2).ok();
            // AlwaysNoConfidence
            enc.array(1).ok();
            enc.u8(3).ok();
            buf
        };
        let mut dec = minicbor::Decoder::new(&cbor);
        let result = handle_drep_delegations(&state, &mut dec);
        match result {
            QueryResult::DRepDelegations(groups) => {
                assert_eq!(groups.len(), 4);
                // Order is preserved from the request.
                assert_eq!(groups[0].drep.drep_type, 0);
                assert_eq!(groups[0].credentials.len(), 2);
                assert_eq!(groups[1].drep.drep_type, 1);
                assert!(groups[1].credentials.is_empty()); // unknown ScriptHash DRep
                assert_eq!(groups[2].drep.drep_type, 2);
                assert_eq!(groups[2].credentials.len(), 1);
                assert_eq!(groups[3].drep.drep_type, 3);
                assert_eq!(groups[3].credentials.len(), 1);
            }
            _ => panic!("Expected DRepDelegations"),
        }
    }
}
