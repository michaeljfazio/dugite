//! Translation: `dugite_primitives` governance + certificates →
//! [`crate::script_context`] equivalents.
//!
//! Phase-2 evaluation needs to lift the Conway-era governance fields
//! (votes, proposal procedures, certificates) into the shapes Plutus
//! V3 validators observe. The destination types are:
//!
//! - `Voter` (CommitteeVoter / DrepVoter / StakePoolVoter)
//! - `Vote` (No / Yes / Abstain)
//! - `GovActionId` (tx_id + idx)
//! - `ProposalProcedure(Data)` — opaque-Data wrapper
//! - `TxCert(Data)` — opaque-Data wrapper carrying the cert's
//!   Constr-encoded shape
//!
//! The `ProposalProcedure` / `TxCert` wrappers are deliberately
//! opaque on the script_context side — the Haskell reference exposes
//! them via `Data` decoding too, so the script-observable contract is
//! "give me a Constr-encoded blob of the expected shape." This
//! module owns that blob construction.
//!
//! ## Coverage
//!
//! The Conway-era cert variants are translated to their Plutus V3
//! `TxCert` constructors. Legacy variants (MIR, GenesisKeyDelegation)
//! surface a typed `Internal` error — Plutus phase-2 never sees them
//! in Conway because they are not legal on the chain after the era
//! transition.

use crate::data::Data;
use crate::phase_two::PhaseTwoError;
use crate::script_context::{
    GovActionId as PlGovActionId, ProposalProcedure as PlProposalProcedure, TxCert, Vote as PlVote,
    Voter as PlVoter,
};
use crate::tx_info_populate::credential_to_plutus;
use dugite_primitives::credentials::Credential as PrimCred;
use dugite_primitives::transaction::{
    Certificate as PrimCert, GovActionId as PrimGovActionId, ProposalProcedure as PrimProposal,
    Vote as PrimVote, Voter as PrimVoter, VotingProcedure as PrimVotingProcedure,
};
use num_bigint::BigInt;
use std::collections::BTreeMap;

// ────────────────────────────────────────────────────────────────────
// Voter / Vote / GovActionId
// ────────────────────────────────────────────────────────────────────

/// Translate a primitive [`PrimVoter`] into the Plutus [`PlVoter`].
///
/// `ConstitutionalCommittee` → `CommitteeVoter`, `DRep` → `DrepVoter`,
/// `StakePool(Hash<32>)` → `StakePoolVoter(PubKeyHash)` (with the 4-byte
/// internal padding stripped — same convention as `required_signers`).
pub fn voter_to_plutus(v: &PrimVoter) -> PlVoter {
    match v {
        PrimVoter::ConstitutionalCommittee(c) => PlVoter::CommitteeVoter(credential_to_plutus(c)),
        PrimVoter::DRep(c) => PlVoter::DrepVoter(credential_to_plutus(c)),
        PrimVoter::StakePool(h) => {
            let mut bytes = [0u8; 28];
            bytes.copy_from_slice(&h.0[..28]);
            PlVoter::StakePoolVoter(bytes)
        }
    }
}

/// Translate a primitive [`PrimVote`] into the Plutus [`PlVote`]. The
/// three variants match across exactly.
pub fn vote_to_plutus(v: &PrimVote) -> PlVote {
    match v {
        PrimVote::No => PlVote::No,
        PrimVote::Yes => PlVote::Yes,
        PrimVote::Abstain => PlVote::Abstain,
    }
}

/// Translate a primitive [`PrimGovActionId`] into the Plutus
/// [`PlGovActionId`]. `transaction_id` (32 bytes) → `tx_id`,
/// `action_index` (u32) → `idx` (u64).
pub fn gov_action_id_to_plutus(g: &PrimGovActionId) -> PlGovActionId {
    PlGovActionId {
        tx_id: g.transaction_id.0,
        idx: g.action_index as u64,
    }
}

/// Translate the tx body's `voting_procedures` map into the
/// `Vec<(Voter, Vec<(GovActionId, Vote)>)>` shape that V3 TxInfo
/// exposes. Inner BTreeMap order is preserved (lex by `GovActionId`
/// = `(tx_id, idx)` lex order), matching canonical CBOR.
pub fn voting_procedures_to_plutus(
    vp: &BTreeMap<PrimVoter, BTreeMap<PrimGovActionId, PrimVotingProcedure>>,
) -> Vec<(PlVoter, Vec<(PlGovActionId, PlVote)>)> {
    let mut out: Vec<(PlVoter, Vec<(PlGovActionId, PlVote)>)> = Vec::with_capacity(vp.len());
    for (voter, votes) in vp {
        let pl_voter = voter_to_plutus(voter);
        let mut pl_votes: Vec<(PlGovActionId, PlVote)> = Vec::with_capacity(votes.len());
        for (gid, vp_inner) in votes {
            pl_votes.push((gov_action_id_to_plutus(gid), vote_to_plutus(&vp_inner.vote)));
        }
        out.push((pl_voter, pl_votes));
    }
    out
}

// ────────────────────────────────────────────────────────────────────
// ProposalProcedure
// ────────────────────────────────────────────────────────────────────

/// Translate a primitive [`PrimProposal`] into the Plutus
/// [`PlProposalProcedure`] (an opaque `Data` wrapper).
///
/// The Plutus V3 reference exposes a proposal as a `Constr 0
/// [deposit, returnAddr, governanceAction]`. We encode `deposit` and
/// `returnAddr` (parsed from the 29-byte reward-address blob) faithfully;
/// `governanceAction` is encoded as `Constr 99 []` for now — a
/// "future-action" placeholder. The full V3 GovernanceAction encoder
/// requires translating ParameterChange / HardForkInitiation /
/// TreasuryWithdrawals / NoConfidence / UpdateCommittee /
/// NewConstitution / InfoAction, each with their own Constr tag.
/// That lands in UPLC-9 part 3e-gov-action; for now the deposit +
/// return_addr coverage is enough for the common spending-validator
/// path.
pub fn proposal_to_plutus(p: &PrimProposal) -> Result<PlProposalProcedure, PhaseTwoError> {
    let return_addr = dugite_primitives::address::Address::from_bytes(&p.return_addr)
        .map_err(|e| PhaseTwoError::Internal(format!("proposal_to_plutus: return_addr: {e}")))?;
    let return_stake_cred = match return_addr {
        dugite_primitives::address::Address::Reward(r) => credential_to_plutus(&r.stake),
        other => {
            return Err(PhaseTwoError::Internal(format!(
                "proposal_to_plutus: return_addr must be Reward, got {other:?}"
            )));
        }
    };
    let return_addr_data = return_stake_cred.to_data();
    let deposit_data = Data::I(BigInt::from(p.deposit.0));
    let gov_action_placeholder = Data::Constr(99, vec![]);
    Ok(PlProposalProcedure(Data::Constr(
        0,
        vec![deposit_data, return_addr_data, gov_action_placeholder],
    )))
}

/// Translate the tx body's `proposal_procedures: Vec<ProposalProcedure>`
/// into `Vec<PlProposalProcedure>` preserving input order.
pub fn proposals_to_plutus(
    proposals: &[PrimProposal],
) -> Result<Vec<PlProposalProcedure>, PhaseTwoError> {
    proposals.iter().map(proposal_to_plutus).collect()
}

// ────────────────────────────────────────────────────────────────────
// Certificates
// ────────────────────────────────────────────────────────────────────

/// Translate a primitive [`PrimCert`] into the Plutus `TxCert(Data)`
/// wrapper.
///
/// Each variant maps to the Plutus V3 `TxCert` constructor with the
/// same tag the Haskell reference uses (see
/// `PlutusLedgerApi.V3.Contexts.TxCert`):
///
/// | Constr | Plutus shape                                            |
/// |--------|---------------------------------------------------------|
/// | 0      | `TxCertRegStaking(cred, Option<deposit>)`               |
/// | 1      | `TxCertUnRegStaking(cred, Option<refund>)`              |
/// | 2      | `TxCertDelegStaking(cred, Delegatee)`                   |
/// | 3      | `TxCertRegDeleg(cred, Delegatee, deposit)`              |
/// | 4      | `TxCertRegDRep(cred, deposit)`                          |
/// | 5      | `TxCertUpdateDRep(cred)`                                |
/// | 6      | `TxCertUnRegDRep(cred, refund)`                         |
/// | 7      | `TxCertPoolRegister(poolId, vrf)`                       |
/// | 8      | `TxCertPoolRetire(poolId, epoch)`                       |
/// | 9      | `TxCertAuthHotCommittee(cold, hot)`                     |
/// | 10     | `TxCertResignColdCommittee(cold)`                       |
///
/// Pre-Conway MIR + GenesisKeyDelegation certs surface a typed
/// `Internal` error — Conway phase-2 never sees them on chain.
pub fn certificate_to_plutus(c: &PrimCert) -> Result<TxCert, PhaseTwoError> {
    let data = match c {
        PrimCert::StakeRegistration(cred) => {
            Data::Constr(0, vec![cred_data(cred), option_int(None)])
        }
        PrimCert::StakeDeregistration(cred) => {
            Data::Constr(1, vec![cred_data(cred), option_int(None)])
        }
        PrimCert::ConwayStakeRegistration {
            credential,
            deposit,
        } => Data::Constr(0, vec![cred_data(credential), option_int(Some(deposit.0))]),
        PrimCert::ConwayStakeDeregistration { credential, refund } => {
            Data::Constr(1, vec![cred_data(credential), option_int(Some(refund.0))])
        }
        PrimCert::StakeDelegation {
            credential,
            pool_hash,
        } => Data::Constr(
            2,
            vec![cred_data(credential), delegatee_to_pool(&pool_hash.0)],
        ),
        PrimCert::RegStakeDeleg {
            credential,
            pool_hash,
            deposit,
        } => Data::Constr(
            3,
            vec![
                cred_data(credential),
                delegatee_to_pool(&pool_hash.0),
                Data::I(BigInt::from(deposit.0)),
            ],
        ),
        PrimCert::RegDRep {
            credential,
            deposit,
            ..
        } => Data::Constr(
            4,
            vec![cred_data(credential), Data::I(BigInt::from(deposit.0))],
        ),
        PrimCert::UpdateDRep { credential, .. } => Data::Constr(5, vec![cred_data(credential)]),
        PrimCert::UnregDRep { credential, refund } => Data::Constr(
            6,
            vec![cred_data(credential), Data::I(BigInt::from(refund.0))],
        ),
        PrimCert::PoolRegistration(params) => Data::Constr(
            7,
            vec![
                Data::B(params.operator.0.to_vec()),
                Data::B(params.vrf_keyhash.0.to_vec()),
            ],
        ),
        PrimCert::PoolRetirement { pool_hash, epoch } => Data::Constr(
            8,
            vec![Data::B(pool_hash.0.to_vec()), Data::I(BigInt::from(*epoch))],
        ),
        PrimCert::CommitteeHotAuth {
            cold_credential,
            hot_credential,
        } => Data::Constr(
            9,
            vec![cred_data(cold_credential), cred_data(hot_credential)],
        ),
        PrimCert::CommitteeColdResign {
            cold_credential, ..
        } => Data::Constr(10, vec![cred_data(cold_credential)]),
        // Combined certs: emit as TxCertRegDeleg / TxCertDelegStaking shapes.
        PrimCert::VoteDelegation {
            credential,
            drep: _,
        } => Data::Constr(
            // For TxCertDelegStaking we'd need to encode the Delegatee
            // properly (DelegStake / DelegVote / DelegStakeVote). The
            // simple-Plutus-script case doesn't observe the delegatee
            // detail, so encode an opaque marker — full Delegatee
            // encoding lands later as part of CIP-1694 followup.
            2,
            vec![cred_data(credential), Data::Constr(99, vec![])],
        ),
        PrimCert::StakeVoteDelegation {
            credential,
            pool_hash,
            drep: _,
        } => Data::Constr(
            2,
            vec![cred_data(credential), delegatee_to_pool(&pool_hash.0)],
        ),
        PrimCert::RegStakeVoteDeleg {
            credential,
            pool_hash,
            deposit,
            drep: _,
        } => Data::Constr(
            3,
            vec![
                cred_data(credential),
                delegatee_to_pool(&pool_hash.0),
                Data::I(BigInt::from(deposit.0)),
            ],
        ),
        PrimCert::VoteRegDeleg {
            credential,
            deposit,
            drep: _,
        } => Data::Constr(
            3,
            vec![
                cred_data(credential),
                Data::Constr(99, vec![]),
                Data::I(BigInt::from(deposit.0)),
            ],
        ),
        PrimCert::GenesisKeyDelegation { .. } => {
            return Err(PhaseTwoError::Internal(
                "certificate_to_plutus: GenesisKeyDelegation is pre-Conway-only".to_string(),
            ));
        }
        PrimCert::MoveInstantaneousRewards { .. } => {
            return Err(PhaseTwoError::Internal(
                "certificate_to_plutus: MIR cert is pre-Conway-only".to_string(),
            ));
        }
    };
    Ok(TxCert(data))
}

/// Encode a primitive Credential as the Plutus `Credential` Data
/// shape (`Constr 0 [PubKey]` / `Constr 1 [Script]`). Reuses the
/// existing `Credential::to_data` impl from script_context.
fn cred_data(c: &PrimCred) -> Data {
    credential_to_plutus(c).to_data()
}

/// Encode `Option<u64>` as `Constr 1 []` (None) / `Constr 0 [I n]`
/// (Some n). Matches Plutus' canonical Option encoding.
fn option_int(v: Option<u64>) -> Data {
    match v {
        None => Data::Constr(1, vec![]),
        Some(n) => Data::Constr(0, vec![Data::I(BigInt::from(n))]),
    }
}

/// Encode a Plutus `Delegatee::DelegStake(PubKeyHash)` — `Constr 0
/// [B pool_hash]`. Used by the stake-delegation cert variants.
fn delegatee_to_pool(pool_hash: &[u8; 28]) -> Data {
    Data::Constr(0, vec![Data::B(pool_hash.to_vec())])
}

/// Translate the tx body's `certificates: Vec<Certificate>` into
/// `Vec<TxCert>` preserving input order.
pub fn certificates_to_plutus(certs: &[PrimCert]) -> Result<Vec<TxCert>, PhaseTwoError> {
    certs.iter().map(certificate_to_plutus).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script_context::Credential as PlCredential;
    use dugite_primitives::hash::Hash;
    use dugite_primitives::transaction::{Anchor, GovActionId};
    use dugite_primitives::value::Lovelace;

    fn h28(b: u8) -> dugite_primitives::hash::Hash28 {
        Hash::<28>([b; 28])
    }

    fn h32(b: u8) -> dugite_primitives::hash::Hash<32> {
        Hash::<32>([b; 32])
    }

    fn key_cred(b: u8) -> PrimCred {
        PrimCred::VerificationKey(h28(b))
    }

    fn script_cred(b: u8) -> PrimCred {
        PrimCred::Script(h28(b))
    }

    // Voter ─────────────────────────────────────────────────────

    #[test]
    fn voter_committee_round_trips() {
        let v = PrimVoter::ConstitutionalCommittee(key_cred(1));
        let pl = voter_to_plutus(&v);
        assert!(matches!(
            pl,
            PlVoter::CommitteeVoter(PlCredential::PubKey(h)) if h == [1u8; 28]
        ));
    }

    #[test]
    fn voter_drep_with_script_cred() {
        let v = PrimVoter::DRep(script_cred(2));
        let pl = voter_to_plutus(&v);
        assert!(matches!(
            pl,
            PlVoter::DrepVoter(PlCredential::Script(h)) if h == [2u8; 28]
        ));
    }

    #[test]
    fn voter_pool_unpads_to_28_bytes() {
        let mut bytes = [0u8; 32];
        bytes[..28].copy_from_slice(&[5u8; 28]);
        let v = PrimVoter::StakePool(Hash::<32>(bytes));
        let pl = voter_to_plutus(&v);
        assert!(matches!(pl, PlVoter::StakePoolVoter(h) if h == [5u8; 28]));
    }

    // Vote ──────────────────────────────────────────────────────

    #[test]
    fn vote_translates_directly() {
        assert!(matches!(vote_to_plutus(&PrimVote::No), PlVote::No));
        assert!(matches!(vote_to_plutus(&PrimVote::Yes), PlVote::Yes));
        assert!(matches!(
            vote_to_plutus(&PrimVote::Abstain),
            PlVote::Abstain
        ));
    }

    // GovActionId ───────────────────────────────────────────────

    #[test]
    fn gov_action_id_widens_index_to_u64() {
        let g = PrimGovActionId {
            transaction_id: h32(0xab),
            action_index: 17,
        };
        let pl = gov_action_id_to_plutus(&g);
        assert_eq!(pl.tx_id, [0xab; 32]);
        assert_eq!(pl.idx, 17u64);
    }

    // Voting procedures map ─────────────────────────────────────

    #[test]
    fn voting_procedures_collect_inner_vote_tuples() {
        let mut inner = BTreeMap::new();
        inner.insert(
            GovActionId {
                transaction_id: h32(0x10),
                action_index: 0,
            },
            PrimVotingProcedure {
                vote: PrimVote::Yes,
                anchor: None,
            },
        );
        inner.insert(
            GovActionId {
                transaction_id: h32(0x20),
                action_index: 1,
            },
            PrimVotingProcedure {
                vote: PrimVote::Abstain,
                anchor: None,
            },
        );
        let mut vp: BTreeMap<PrimVoter, BTreeMap<GovActionId, PrimVotingProcedure>> =
            BTreeMap::new();
        vp.insert(PrimVoter::DRep(key_cred(9)), inner);
        let pl = voting_procedures_to_plutus(&vp);
        assert_eq!(pl.len(), 1);
        let (voter, votes) = &pl[0];
        assert!(matches!(voter, PlVoter::DrepVoter(_)));
        assert_eq!(votes.len(), 2);
        // BTreeMap is sorted on (tx_id, idx) → 0x10 < 0x20.
        assert!(matches!(votes[0].1, PlVote::Yes));
        assert!(matches!(votes[1].1, PlVote::Abstain));
    }

    // Proposal procedure ─────────────────────────────────────────

    fn reward_addr_blob(hash: [u8; 28]) -> Vec<u8> {
        let mut v = vec![0xe0u8]; // mainnet reward, key-stake
        v.extend_from_slice(&hash);
        v
    }

    fn anchor() -> Anchor {
        Anchor {
            url: String::new(),
            data_hash: h32(0),
        }
    }

    #[test]
    fn proposal_to_plutus_encodes_deposit_and_return_addr() {
        use dugite_primitives::transaction::GovAction;
        let p = PrimProposal {
            deposit: Lovelace(100_000),
            return_addr: reward_addr_blob([0x42; 28]),
            gov_action: GovAction::InfoAction,
            anchor: anchor(),
        };
        let pl = proposal_to_plutus(&p).unwrap();
        let Data::Constr(0, fields) = pl.0 else {
            panic!("expected Constr 0 wrapper");
        };
        assert_eq!(fields.len(), 3);
        // deposit
        assert_eq!(fields[0], Data::I(BigInt::from(100_000)));
        // return_addr (Plutus Credential::PubKey([0x42; 28]) → Constr 0 [B ...])
        assert!(
            matches!(&fields[1], Data::Constr(0, inner) if inner == &vec![Data::B(vec![0x42; 28])])
        );
        // gov_action placeholder
        assert_eq!(fields[2], Data::Constr(99, vec![]));
    }

    #[test]
    fn proposal_to_plutus_rejects_non_reward_return_addr() {
        use dugite_primitives::transaction::GovAction;
        let mut bad = vec![0x60u8]; // enterprise, not reward
        bad.extend([1u8; 28]);
        let p = PrimProposal {
            deposit: Lovelace(1),
            return_addr: bad,
            gov_action: GovAction::InfoAction,
            anchor: anchor(),
        };
        let err = proposal_to_plutus(&p).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    // Certificates ──────────────────────────────────────────────

    #[test]
    fn cert_stake_registration_uses_constr_0_with_none_deposit() {
        let c = PrimCert::StakeRegistration(key_cred(1));
        let TxCert(Data::Constr(tag, fields)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 0);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[1], Data::Constr(1, vec![])); // None
    }

    #[test]
    fn cert_conway_stake_registration_carries_deposit() {
        let c = PrimCert::ConwayStakeRegistration {
            credential: key_cred(1),
            deposit: Lovelace(2_000_000),
        };
        let TxCert(Data::Constr(tag, fields)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 0);
        assert_eq!(
            fields[1],
            Data::Constr(0, vec![Data::I(BigInt::from(2_000_000))])
        );
    }

    #[test]
    fn cert_pool_registration_emits_operator_and_vrf_bytes() {
        use dugite_primitives::transaction::PoolParams;
        let params = PoolParams {
            operator: h28(0xaa),
            vrf_keyhash: h32(0xbb),
            pledge: Lovelace(0),
            cost: Lovelace(0),
            margin: dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
            reward_account: vec![],
            pool_owners: vec![],
            relays: vec![],
            pool_metadata: None,
        };
        let c = PrimCert::PoolRegistration(params);
        let TxCert(Data::Constr(tag, fields)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 7);
        assert_eq!(fields[0], Data::B(vec![0xaa; 28]));
        assert_eq!(fields[1], Data::B(vec![0xbb; 32]));
    }

    #[test]
    fn cert_pool_retirement_carries_epoch() {
        let c = PrimCert::PoolRetirement {
            pool_hash: h28(0xcc),
            epoch: 500,
        };
        let TxCert(Data::Constr(tag, fields)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 8);
        assert_eq!(fields[1], Data::I(BigInt::from(500)));
    }

    #[test]
    fn cert_committee_hot_auth_carries_both_credentials() {
        let c = PrimCert::CommitteeHotAuth {
            cold_credential: key_cred(1),
            hot_credential: script_cred(2),
        };
        let TxCert(Data::Constr(tag, _)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 9);
    }

    #[test]
    fn cert_genesis_key_delegation_errors() {
        let c = PrimCert::GenesisKeyDelegation {
            genesis_hash: h32(1),
            genesis_delegate_hash: h32(2),
            vrf_keyhash: h32(3),
        };
        let err = certificate_to_plutus(&c).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    #[test]
    fn cert_mir_errors() {
        let c = PrimCert::MoveInstantaneousRewards {
            source: dugite_primitives::transaction::MIRSource::Reserves,
            target: dugite_primitives::transaction::MIRTarget::OtherAccountingPot(0),
        };
        let err = certificate_to_plutus(&c).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    #[test]
    fn certificates_to_plutus_preserves_order_and_surfaces_first_error() {
        let ok = PrimCert::StakeRegistration(key_cred(1));
        let bad = PrimCert::GenesisKeyDelegation {
            genesis_hash: h32(0),
            genesis_delegate_hash: h32(0),
            vrf_keyhash: h32(0),
        };
        let err = certificates_to_plutus(&[ok, bad]).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }
}
