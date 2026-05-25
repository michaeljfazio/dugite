//! Conway governance action proposals + votes → utxorpc protobuf.
//!
//! Covers every CIP-1694 governance action variant (ParameterChange,
//! HardForkInitiation, TreasuryWithdrawals, NoConfidence,
//! UpdateCommittee, NewConstitution, InfoAction) plus the
//! Voter / VotingProcedure / GovActionId surface.

use crate::map::cert::{anchor_to_proto, credential_to_proto};
use crate::map::common::{coin_bigint, hash_bytes};
use crate::proto::v1beta::cardano as pb;
use dugite_primitives::transaction::{
    GovAction, GovActionId, ProposalProcedure, Rational as DRational, Vote, Voter, VotingProcedure,
};
use std::collections::BTreeMap;

/// Project a single proposal procedure into the protobuf shape.
pub fn proposal_to_proto(p: &ProposalProcedure) -> pb::GovernanceActionProposal {
    pb::GovernanceActionProposal {
        deposit: Some(coin_bigint(p.deposit.0)),
        reward_account: p.return_addr.clone(),
        gov_action: Some(gov_action_to_proto(&p.gov_action)),
        anchor: Some(anchor_to_proto(&p.anchor)),
    }
}

/// Project the (Voter → (GovActionId → VotingProcedure)) map into the
/// protobuf `repeated VoterVotes`.
pub fn votes_to_proto(
    votes: &BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>>,
) -> Vec<pb::VoterVotes> {
    votes
        .iter()
        .map(|(voter, inner)| pb::VoterVotes {
            voter: Some(voter_to_proto(voter)),
            votes: inner
                .iter()
                .map(|(action_id, vp)| pb::VotingProcedure {
                    gov_action_id: Some(gov_action_id_to_proto(action_id)),
                    vote: vote_to_proto(&vp.vote) as i32,
                    anchor: vp.anchor.as_ref().map(anchor_to_proto),
                })
                .collect(),
        })
        .collect()
}

fn gov_action_id_to_proto(id: &GovActionId) -> pb::GovernanceActionId {
    pb::GovernanceActionId {
        transaction_id: hash_bytes(&id.transaction_id),
        governance_action_index: id.action_index,
    }
}

fn voter_to_proto(v: &Voter) -> pb::voter_votes::Voter {
    use pb::voter_votes::Voter as Inner;
    match v {
        Voter::ConstitutionalCommittee(cred) => {
            Inner::ConstitutionalCommittee(credential_to_proto(cred))
        }
        Voter::DRep(cred) => Inner::Drep(credential_to_proto(cred)),
        Voter::StakePool(hash) => Inner::Spo(hash.as_ref().to_vec()),
    }
}

fn vote_to_proto(v: &Vote) -> pb::Vote {
    match v {
        Vote::No => pb::Vote::No,
        Vote::Yes => pb::Vote::Yes,
        Vote::Abstain => pb::Vote::Abstain,
    }
}

fn gov_action_to_proto(a: &GovAction) -> pb::GovernanceAction {
    use pb::governance_action::GovernanceAction as Inner;
    let inner = match a {
        GovAction::ParameterChange {
            prev_action_id,
            policy_hash,
            // protocol_param_update mapping needs a dedicated
            // ProtocolParamUpdate → PParams projection. Empty PParams
            // is the conservative fallback until that lands.
            protocol_param_update: _,
        } => Inner::ParameterChangeAction(pb::ParameterChangeAction {
            gov_action_id: prev_action_id.as_ref().map(gov_action_id_to_proto),
            protocol_param_update: None,
            policy_hash: policy_hash
                .as_ref()
                .map(|h| h.as_ref().to_vec())
                .unwrap_or_default(),
        }),
        GovAction::HardForkInitiation {
            prev_action_id,
            protocol_version,
        } => Inner::HardForkInitiationAction(pb::HardForkInitiationAction {
            gov_action_id: prev_action_id.as_ref().map(gov_action_id_to_proto),
            protocol_version: Some(pb::ProtocolVersion {
                major: protocol_version.0 as u32,
                minor: protocol_version.1 as u32,
            }),
        }),
        GovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash,
        } => Inner::TreasuryWithdrawalsAction(pb::TreasuryWithdrawalsAction {
            withdrawals: withdrawals
                .iter()
                .map(|(account, coin)| pb::WithdrawalAmount {
                    reward_account: account.clone(),
                    coin: Some(coin_bigint(coin.0)),
                })
                .collect(),
            policy_hash: policy_hash
                .as_ref()
                .map(|h| h.as_ref().to_vec())
                .unwrap_or_default(),
        }),
        GovAction::NoConfidence { prev_action_id } => {
            Inner::NoConfidenceAction(pb::NoConfidenceAction {
                gov_action_id: prev_action_id.as_ref().map(gov_action_id_to_proto),
            })
        }
        GovAction::UpdateCommittee {
            prev_action_id,
            members_to_remove,
            members_to_add,
            threshold,
        } => Inner::UpdateCommitteeAction(pb::UpdateCommitteeAction {
            gov_action_id: prev_action_id.as_ref().map(gov_action_id_to_proto),
            remove_committee_credentials: members_to_remove
                .iter()
                .map(credential_to_proto)
                .collect(),
            new_committee_credentials: members_to_add
                .iter()
                .map(|(cred, expiry_epoch)| pb::NewCommitteeCredentials {
                    committee_cold_credential: Some(credential_to_proto(cred)),
                    expires_epoch: *expiry_epoch as u32,
                })
                .collect(),
            new_committee_threshold: Some(rational_to_proto(threshold)),
        }),
        GovAction::NewConstitution {
            prev_action_id,
            constitution,
        } => Inner::NewConstitutionAction(pb::NewConstitutionAction {
            gov_action_id: prev_action_id.as_ref().map(gov_action_id_to_proto),
            constitution: Some(pb::Constitution {
                anchor: Some(anchor_to_proto(&constitution.anchor)),
                hash: constitution
                    .script_hash
                    .as_ref()
                    .map(|h| h.as_ref().to_vec())
                    .unwrap_or_default(),
            }),
        }),
        GovAction::InfoAction => Inner::InfoAction(pb::InfoAction {}),
    };
    pb::GovernanceAction {
        governance_action: Some(inner),
    }
}

fn rational_to_proto(r: &DRational) -> pb::RationalNumber {
    pb::RationalNumber {
        numerator: r.numerator as i32,
        denominator: r.denominator as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::credentials::Credential;
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::value::Lovelace;

    fn cred() -> Credential {
        Credential::VerificationKey(Hash28::from_bytes([7u8; 28]))
    }

    #[test]
    fn info_action_round_trip() {
        let p = ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0xE0; 29],
            gov_action: GovAction::InfoAction,
            anchor: dugite_primitives::transaction::Anchor {
                url: "https://example.com/info".into(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        };
        let pb_p = proposal_to_proto(&p);
        let act = pb_p.gov_action.expect("gov_action set");
        match act.governance_action.unwrap() {
            pb::governance_action::GovernanceAction::InfoAction(_) => {}
            other => panic!("expected info_action, got {other:?}"),
        }
    }

    #[test]
    fn hardfork_action_carries_protocol_version() {
        let p = ProposalProcedure {
            deposit: Lovelace(0),
            return_addr: Vec::new(),
            gov_action: GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (11, 0),
            },
            anchor: dugite_primitives::transaction::Anchor {
                url: "".into(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        };
        let pb_p = proposal_to_proto(&p);
        let act = pb_p.gov_action.unwrap();
        match act.governance_action.unwrap() {
            pb::governance_action::GovernanceAction::HardForkInitiationAction(a) => {
                let pv = a.protocol_version.unwrap();
                assert_eq!(pv.major, 11);
            }
            other => panic!("expected hard_fork, got {other:?}"),
        }
    }

    #[test]
    fn treasury_withdrawals_round_trip() {
        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(vec![0xE0; 29], Lovelace(1_000_000));
        let p = ProposalProcedure {
            deposit: Lovelace(0),
            return_addr: Vec::new(),
            gov_action: GovAction::TreasuryWithdrawals {
                withdrawals,
                policy_hash: None,
            },
            anchor: dugite_primitives::transaction::Anchor {
                url: "".into(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        };
        let pb_p = proposal_to_proto(&p);
        match pb_p.gov_action.unwrap().governance_action.unwrap() {
            pb::governance_action::GovernanceAction::TreasuryWithdrawalsAction(a) => {
                assert_eq!(a.withdrawals.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn votes_map_to_voter_votes_with_action_ids() {
        let mut outer = BTreeMap::new();
        let mut inner = BTreeMap::new();
        inner.insert(
            GovActionId {
                transaction_id: Hash32::from_bytes([1u8; 32]),
                action_index: 0,
            },
            VotingProcedure {
                vote: Vote::Yes,
                anchor: None,
            },
        );
        outer.insert(Voter::DRep(cred()), inner);
        let pb_votes = votes_to_proto(&outer);
        assert_eq!(pb_votes.len(), 1);
        let vv = &pb_votes[0];
        assert_eq!(vv.votes.len(), 1);
        assert_eq!(vv.votes[0].vote, pb::Vote::Yes as i32);
    }
}
