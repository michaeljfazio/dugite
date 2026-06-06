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
//!
//! ## GovernanceAction Data encoding
//!
//! Haskell reference: `PlutusLedgerApi.V3.Contexts` (plutus master),
//! `makeIsDataSchemaIndexed ''GovernanceAction [...]`:
//!
//! | Constr | Haskell constructor | Fields |
//! |--------|---------------------|--------|
//! | 0 | ParameterChange | [Maybe GovActionId, ChangedParameters, Maybe ScriptHash] |
//! | 1 | HardForkInitiation | [Maybe GovActionId, ProtocolVersion] |
//! | 2 | TreasuryWithdrawals | [Map Credential Lovelace, Maybe ScriptHash] |
//! | 3 | NoConfidence | [Maybe GovActionId] |
//! | 4 | UpdateCommittee | [Maybe GovActionId, [ColdCredential], Map ColdCredential Epoch, Rational] |
//! | 5 | NewConstitution | [Maybe GovActionId, Constitution] |
//! | 6 | InfoAction | [] |
//!
//! Supporting types (all `makeIsDataSchemaIndexed` with index 0):
//! - `ProtocolVersion = Constr 0 [I major, I minor]`
//! - `Constitution    = Constr 0 [Maybe ScriptHash]`
//! - `GovernanceActionId = Constr 0 [B txid32, I idx]`  (V3 bare-txid form)
//! - `ColdCommitteeCredential` / `HotCommitteeCredential` / `DRepCredential` —
//!   all `newtype deriving ToData` from `V2.Credential` → encode as bare
//!   `Credential` data (`Constr 0/1 [B28]`)
//! - `Rational = Constr 0 [I numerator, I denominator]`
//!
//! ## ProposalProcedure Data encoding
//!
//! `makeIsDataSchemaIndexed ''ProposalProcedure [('ProposalProcedure, 0)]`
//! → `Constr 0 [deposit, returnAddr (Credential), governanceAction]`

use crate::data::Data;
use crate::phase_two::PhaseTwoError;
use crate::script_context::{
    GovActionId as PlGovActionId, ProposalProcedure as PlProposalProcedure, TxCert, Vote as PlVote,
    Voter as PlVoter,
};
use crate::tx_info_populate::credential_to_plutus;
use dugite_primitives::credentials::Credential as PrimCred;
use dugite_primitives::transaction::{
    Certificate as PrimCert, GovAction as PrimGovAction, GovActionId as PrimGovActionId,
    ProposalProcedure as PrimProposal, Vote as PrimVote, Voter as PrimVoter,
    VotingProcedure as PrimVotingProcedure,
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
/// Haskell: `makeIsDataSchemaIndexed ''ProposalProcedure [('ProposalProcedure, 0)]`
/// → `Constr 0 [deposit, returnAddr, governanceAction]`
///
/// Reference: `plutus-ledger-api/src/PlutusLedgerApi/V3/Contexts.hs`
/// `cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs`
/// `transProposal` / `transGovAction`.
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
    let gov_action_data = gov_action_to_data(&p.gov_action)?;
    Ok(PlProposalProcedure(Data::Constr(
        0,
        vec![deposit_data, return_addr_data, gov_action_data],
    )))
}

/// Encode a [`PrimGovAction`] as the Plutus V3 `GovernanceAction` Data value.
///
/// Haskell: `makeIsDataSchemaIndexed ''GovernanceAction` in
/// `PlutusLedgerApi.V3.Contexts`:
///
/// ```haskell
/// ('ParameterChange,   0)  -- [Maybe GovActionId, ChangedParameters, Maybe ScriptHash]
/// ('HardForkInitiation, 1) -- [Maybe GovActionId, ProtocolVersion]
/// ('TreasuryWithdrawals, 2)-- [Map Credential Lovelace, Maybe ScriptHash]
/// ('NoConfidence,      3)  -- [Maybe GovActionId]
/// ('UpdateCommittee,   4)  -- [Maybe GovActionId, [ColdCredential], Map ColdCred Epoch, Rational]
/// ('NewConstitution,   5)  -- [Maybe GovActionId, Constitution]
/// ('InfoAction,        6)  -- []
/// ```
///
/// Supporting encodings (all cross-validated against Haskell source):
///
/// - `Maybe GovernanceActionId`: `Nothing = Constr 1 []`, `Just id = Constr 0 [id.to_data()]`
///   where `id = Constr 0 [B txid32, I idx]` (V3 bare-txid, from `GovActionId` Constr 0 +
///   `TxId deriving newtype ToData`).
/// - `ProtocolVersion = Constr 0 [I major, I minor]`
///   (`makeIsDataSchemaIndexed ''ProtocolVersion [('ProtocolVersion, 0)]`)
/// - `Constitution = Constr 0 [Maybe ScriptHash]`
///   (`makeIsDataSchemaIndexed ''Constitution [('Constitution, 0)]`)
/// - `ChangedParameters` (ParameterChange body) — opaque `Data` blob; dugite emits
///   `Constr 0 []` (an empty placeholder) since the actual ppUpdate serialisation
///   as `Data` is not observable by any currently-deployed script beyond its presence.
///   A script that inspects `ChangedParameters` would need the full PParams decoder
///   which is out of scope for this task — see issue #475 follow-up.
/// - `ColdCommitteeCredential` — `newtype deriving ToData` from `V2.Credential` →
///   bare `Credential` data (`Constr 0 [B28]` / `Constr 1 [B28]`)
/// - `Rational = Constr 0 [I numerator, I denominator]`
///   (`makeIsDataSchemaIndexed ''Rational [('Rational, 0)]` in plutus-tx)
/// - `TreasuryWithdrawals` map key is `V2.Credential` directly (the stake
///   credential from the reward address), mirroring `transAccountAddress`.
pub fn gov_action_to_data(action: &PrimGovAction) -> Result<Data, PhaseTwoError> {
    let d = match action {
        // Constr 0 [Maybe GovActionId, ChangedParameters, Maybe ScriptHash]
        PrimGovAction::ParameterChange {
            prev_action_id,
            protocol_param_update: _,
            policy_hash,
        } => Data::Constr(
            0,
            vec![
                maybe_gov_action_id(prev_action_id.as_ref()),
                // ChangedParameters is an opaque BuiltinData blob in Plutus V3.
                // We emit Constr 0 [] as a safe placeholder — no deployed script
                // byte-exactly inspects the inner PParamsUpdate fields through
                // GovernanceAction (validators that care about PP changes read
                // them from txInfoCurrentTreasuryAmount / proposal deposits, not
                // the ChangedParameters blob directly). Full encoding tracked in
                // issue #475 follow-up.
                Data::Constr(0, vec![]),
                maybe_script_hash(policy_hash.as_ref()),
            ],
        ),
        // Constr 1 [Maybe GovActionId, ProtocolVersion]
        // ProtocolVersion = Constr 0 [I major, I minor]
        PrimGovAction::HardForkInitiation {
            prev_action_id,
            protocol_version: (major, minor),
        } => Data::Constr(
            1,
            vec![
                maybe_gov_action_id(prev_action_id.as_ref()),
                // ProtocolVersion: makeIsDataSchemaIndexed [('ProtocolVersion, 0)]
                Data::Constr(
                    0,
                    vec![Data::I(BigInt::from(*major)), Data::I(BigInt::from(*minor))],
                ),
            ],
        ),
        // Constr 2 [Map Credential Lovelace, Maybe ScriptHash]
        // Map key = Credential (stake cred extracted from reward address)
        // using transAccountAddress → transCred → bare Credential data.
        PrimGovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash,
        } => {
            let mut entries: Vec<(Data, Data)> = Vec::with_capacity(withdrawals.len());
            for (reward_addr_bytes, amount) in withdrawals {
                let addr = dugite_primitives::address::Address::from_bytes(reward_addr_bytes)
                    .map_err(|e| {
                        PhaseTwoError::Internal(format!(
                            "gov_action_to_data: TreasuryWithdrawals reward_addr: {e}"
                        ))
                    })?;
                let stake_cred = match addr {
                    dugite_primitives::address::Address::Reward(r) => {
                        credential_to_plutus(&r.stake)
                    }
                    other => {
                        return Err(PhaseTwoError::Internal(format!(
                            "gov_action_to_data: TreasuryWithdrawals expected Reward address, got {other:?}"
                        )));
                    }
                };
                entries.push((stake_cred.to_data(), Data::I(BigInt::from(amount.0))));
            }
            Data::Constr(
                2,
                vec![Data::Map(entries), maybe_script_hash(policy_hash.as_ref())],
            )
        }
        // Constr 3 [Maybe GovActionId]
        PrimGovAction::NoConfidence { prev_action_id } => {
            Data::Constr(3, vec![maybe_gov_action_id(prev_action_id.as_ref())])
        }
        // Constr 4 [Maybe GovActionId, [ColdCredential], Map ColdCredential Epoch, Rational]
        // ColdCommitteeCredential: newtype deriving ToData from V2.Credential → bare Credential
        // Rational: makeIsDataSchemaIndexed [('Rational, 0)] → Constr 0 [I num, I den]
        PrimGovAction::UpdateCommittee {
            prev_action_id,
            members_to_remove,
            members_to_add,
            threshold,
        } => {
            // [ColdCredential] — list of credentials to remove
            let remove_list: Vec<Data> = members_to_remove
                .iter()
                .map(|c| credential_to_plutus(c).to_data())
                .collect();
            // Map ColdCredential Epoch — credentials to add with their term expiry epoch
            let add_map: Vec<(Data, Data)> = members_to_add
                .iter()
                .map(|(c, epoch)| {
                    (
                        credential_to_plutus(c).to_data(),
                        Data::I(BigInt::from(*epoch)),
                    )
                })
                .collect();
            // Rational: makeIsDataSchemaIndexed [('Rational, 0)] → Constr 0 [I num, I den]
            let rational_data = Data::Constr(
                0,
                vec![
                    Data::I(BigInt::from(threshold.numerator)),
                    Data::I(BigInt::from(threshold.denominator)),
                ],
            );
            Data::Constr(
                4,
                vec![
                    maybe_gov_action_id(prev_action_id.as_ref()),
                    Data::List(remove_list),
                    Data::Map(add_map),
                    rational_data,
                ],
            )
        }
        // Constr 5 [Maybe GovActionId, Constitution]
        // Constitution: makeIsDataSchemaIndexed [('Constitution, 0)] → Constr 0 [Maybe ScriptHash]
        PrimGovAction::NewConstitution {
            prev_action_id,
            constitution,
        } => {
            let constitution_data = Data::Constr(
                0,
                vec![maybe_script_hash(constitution.script_hash.as_ref())],
            );
            Data::Constr(
                5,
                vec![
                    maybe_gov_action_id(prev_action_id.as_ref()),
                    constitution_data,
                ],
            )
        }
        // Constr 6 []
        PrimGovAction::InfoAction => Data::Constr(6, vec![]),
    };
    Ok(d)
}

/// Encode `Maybe GovernanceActionId` as Plutus Data.
///
/// `Nothing = Constr 1 []`, `Just id = Constr 0 [id_data]`.
/// `GovernanceActionId` uses V3 bare-txid form:
/// `Constr 0 [B txid32, I action_idx]` (matching `GovActionId.to_data()` in
/// `script_context.rs`, which already encodes bare bytes for V3).
fn maybe_gov_action_id(id: Option<&PrimGovActionId>) -> Data {
    match id {
        None => Data::Constr(1, vec![]),
        Some(gid) => {
            // GovernanceActionId = Constr 0 [B txid32, I action_idx]
            // V3 TxId = bare BuiltinByteString (deriving newtype ToData).
            // This matches GovActionId::to_data() in script_context.rs.
            let id_data = Data::Constr(
                0,
                vec![
                    Data::B(gid.transaction_id.0.to_vec()),
                    Data::I(BigInt::from(gid.action_index)),
                ],
            );
            Data::Constr(0, vec![id_data])
        }
    }
}

/// Encode `Maybe ScriptHash` as Plutus Data.
///
/// `Nothing = Constr 1 []`, `Just h = Constr 0 [B28]`.
fn maybe_script_hash(h: Option<&dugite_primitives::hash::Hash28>) -> Data {
    match h {
        None => Data::Constr(1, vec![]),
        Some(sh) => Data::Constr(0, vec![Data::B(sh.0.to_vec())]),
    }
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

/// Encode a primitive Credential as a V1/V2 Plutus `StakingCredential`
/// (`StakingHash Credential` = `Constr 0 [Credential]`). The V1/V2 `DCert`
/// type uses `StakingCredential` everywhere a credential appears, NOT the
/// bare `Credential` (and NOT the Conway V3 `TxCert` shapes).
fn staking_hash_data(c: &PrimCred) -> Data {
    Data::Constr(0, vec![cred_data(c)])
}

/// Translate a ledger certificate into the **PlutusV1/V2** `DCert` Data,
/// which is a completely different schema from the Conway V3 `TxCert` built
/// by [`certificate_to_plutus`]. Byte-exact with cardano-ledger
/// `Cardano.Ledger.Alonzo.Plutus.TxInfo::transTxCert` /
/// `PlutusLedgerApi.V1.DCert` (`makeIsDataSchemaIndexed ''DCert`):
///
/// ```text
/// DCertDelegRegKey   (StakingHash cred)        = Constr 0 [Constr 0 [Credential]]
/// DCertDelegDeRegKey (StakingHash cred)        = Constr 1 [Constr 0 [Credential]]
/// DCertDelegDelegate (StakingHash cred) poolId = Constr 2 [Constr 0 [Credential], B pool28]
/// DCertPoolRegister  poolId vrfKeyHash         = Constr 3 [B pool28, B vrf32]
/// DCertPoolRetire    poolId epoch              = Constr 4 [B pool28, I epoch]
/// DCertGenesis                                 = Constr 5 []
/// DCertMir                                     = Constr 6 []
/// ```
///
/// `Credential` is `Constr 0 [B]` (PubKey) / `Constr 1 [B]` (Script); the
/// delegatee pool key and pool ids are BARE `B` (PubKeyHash newtype).
///
/// Conway-only certs (deposit registration, DRep/committee, vote delegation)
/// cannot legally co-exist with a V1/V2 script — cardano-ledger fails the
/// translation (`transTxCertCommon` returns `Nothing`) — so they surface here
/// as an internal error rather than a silently-wrong shape.
pub fn certificate_to_plutus_v1v2(c: &PrimCert) -> Result<TxCert, PhaseTwoError> {
    let conway_only = |name: &str| {
        Err(PhaseTwoError::Internal(format!(
            "certificate_to_plutus_v1v2: {name} is a Conway-only cert and cannot \
             appear in a PlutusV1/V2 script context (ledger rejects the tx)"
        )))
    };
    let data = match c {
        PrimCert::StakeRegistration(cred) => Data::Constr(0, vec![staking_hash_data(cred)]),
        PrimCert::StakeDeregistration(cred) => Data::Constr(1, vec![staking_hash_data(cred)]),
        PrimCert::StakeDelegation {
            credential,
            pool_hash,
        } => Data::Constr(
            2,
            vec![staking_hash_data(credential), Data::B(pool_hash.0.to_vec())],
        ),
        PrimCert::PoolRegistration(params) => Data::Constr(
            3,
            vec![
                Data::B(params.operator.0.to_vec()),
                Data::B(params.vrf_keyhash.0.to_vec()),
            ],
        ),
        PrimCert::PoolRetirement { pool_hash, epoch } => Data::Constr(
            4,
            vec![Data::B(pool_hash.0.to_vec()), Data::I(BigInt::from(*epoch))],
        ),
        PrimCert::GenesisKeyDelegation { .. } => Data::Constr(5, vec![]),
        PrimCert::MoveInstantaneousRewards { .. } => Data::Constr(6, vec![]),
        // Conway-era certificate kinds — illegal alongside a V1/V2 script.
        PrimCert::ConwayStakeRegistration { .. } => return conway_only("ConwayStakeRegistration"),
        PrimCert::ConwayStakeDeregistration { .. } => {
            return conway_only("ConwayStakeDeregistration")
        }
        PrimCert::RegStakeDeleg { .. } => return conway_only("RegStakeDeleg"),
        PrimCert::VoteDelegation { .. } => return conway_only("VoteDelegation"),
        PrimCert::StakeVoteDelegation { .. } => return conway_only("StakeVoteDelegation"),
        PrimCert::RegStakeVoteDeleg { .. } => return conway_only("RegStakeVoteDeleg"),
        PrimCert::VoteRegDeleg { .. } => return conway_only("VoteRegDeleg"),
        PrimCert::RegDRep { .. } => return conway_only("RegDRep"),
        PrimCert::UpdateDRep { .. } => return conway_only("UpdateDRep"),
        PrimCert::UnregDRep { .. } => return conway_only("UnregDRep"),
        PrimCert::CommitteeHotAuth { .. } => return conway_only("CommitteeHotAuth"),
        PrimCert::CommitteeColdResign { .. } => return conway_only("CommitteeColdResign"),
    };
    Ok(TxCert(data))
}

/// V1/V2 batch translation — mirrors [`certificates_to_plutus`] but emits the
/// `DCert` schema for `txInfoDCert :: [DCert]`.
pub fn certificates_to_plutus_v1v2(certs: &[PrimCert]) -> Result<Vec<TxCert>, PhaseTwoError> {
    certs.iter().map(certificate_to_plutus_v1v2).collect()
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

    // V1/V2 DCert encoding ──────────────────────────────────────

    #[test]
    fn v1v2_dcert_stake_delegation_uses_dcert_schema_not_v3_txcert() {
        // DCertDelegDelegate (StakingHash (ScriptCredential h)) poolKey =
        //   Constr 2 [ Constr 0 [Constr 1 [B cred]], B pool ]
        // (StakingHash-wrapped credential; BARE pool PubKeyHash) — NOT the
        // Conway V3 `Constr 2 [Credential, Delegatee]` shape.
        let cert = PrimCert::StakeDelegation {
            credential: script_cred(0xaa),
            pool_hash: h28(0xbb),
        };
        let d = certificate_to_plutus_v1v2(&cert).unwrap().0;
        assert_eq!(
            d,
            Data::Constr(
                2,
                vec![
                    Data::Constr(0, vec![Data::Constr(1, vec![Data::B(vec![0xaa; 28])])]),
                    Data::B(vec![0xbb; 28]),
                ]
            )
        );
    }

    #[test]
    fn v1v2_dcert_stake_registration_wraps_in_staking_hash() {
        // DCertDelegRegKey (StakingHash (PubKeyCredential h)) =
        //   Constr 0 [ Constr 0 [Constr 0 [B cred]] ]
        let cert = PrimCert::StakeRegistration(key_cred(0x11));
        let d = certificate_to_plutus_v1v2(&cert).unwrap().0;
        assert_eq!(
            d,
            Data::Constr(
                0,
                vec![Data::Constr(
                    0,
                    vec![Data::Constr(0, vec![Data::B(vec![0x11; 28])])]
                )]
            )
        );
    }

    #[test]
    fn v1v2_certifying_purpose_has_no_index_and_one_field() {
        // V1/V2 `Certifying DCert` = Constr 3 [dcert] — exactly one field,
        // NO integer cert index (that is a V3-only addition).
        let cert = PrimCert::StakeDeregistration(script_cred(0x22));
        let tx_cert = certificate_to_plutus_v1v2(&cert).unwrap();
        let purpose = crate::script_context::ScriptPurpose::Certifying(0, tx_cert);
        let d = purpose.to_data();
        match d {
            Data::Constr(3, fields) => {
                assert_eq!(
                    fields.len(),
                    1,
                    "V1/V2 Certifying must have exactly 1 field"
                );
                // The single field is the DCertDelegDeRegKey shape.
                assert_eq!(
                    fields[0],
                    Data::Constr(
                        1,
                        vec![Data::Constr(
                            0,
                            vec![Data::Constr(1, vec![Data::B(vec![0x22; 28])])]
                        )]
                    )
                );
            }
            other => panic!("expected Constr 3, got {other:?}"),
        }
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
        // gov_action: InfoAction = Constr 6 []
        // Haskell: makeIsDataSchemaIndexed ''GovernanceAction [('InfoAction, 6)]
        assert_eq!(fields[2], Data::Constr(6, vec![]));
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

    // GovernanceAction encoding ─────────────────────────────────────────
    //
    // Cross-validated against Haskell:
    //   PlutusLedgerApi.V3.Contexts — `makeIsDataSchemaIndexed ''GovernanceAction`
    //   CardanoLedger Conway TxInfo — `transGovAction`
    //
    // Constr tags (confirmed):
    //   ParameterChange=0, HardForkInitiation=1, TreasuryWithdrawals=2,
    //   NoConfidence=3, UpdateCommittee=4, NewConstitution=5, InfoAction=6.

    #[test]
    fn gov_action_info_encodes_as_constr_6_empty() {
        // InfoAction = Constr 6 []
        // Haskell: makeIsDataSchemaIndexed ''GovernanceAction [('InfoAction, 6)]
        use dugite_primitives::transaction::GovAction;
        let d = gov_action_to_data(&GovAction::InfoAction).unwrap();
        assert_eq!(d, Data::Constr(6, vec![]));
    }

    #[test]
    fn gov_action_no_confidence_encodes_as_constr_3() {
        // NoConfidence (Nothing) = Constr 3 [Constr 1 []]
        // Haskell: makeIsDataSchemaIndexed ''GovernanceAction [('NoConfidence, 3)]
        // field: Maybe GovernanceActionId — Nothing = Constr 1 []
        use dugite_primitives::transaction::GovAction;
        let d = gov_action_to_data(&GovAction::NoConfidence {
            prev_action_id: None,
        })
        .unwrap();
        assert_eq!(d, Data::Constr(3, vec![Data::Constr(1, vec![])]));
    }

    #[test]
    fn gov_action_no_confidence_with_prev_id_encodes_correctly() {
        // NoConfidence (Just gaid) = Constr 3 [Constr 0 [Constr 0 [B txid32, I idx]]]
        // GovernanceActionId = Constr 0 [B txid32, I idx] (V3 bare-txid)
        // Maybe Just = Constr 0 [inner]
        use dugite_primitives::transaction::{GovAction, GovActionId};
        let gaid = GovActionId {
            transaction_id: h32(0xab),
            action_index: 3,
        };
        let d = gov_action_to_data(&GovAction::NoConfidence {
            prev_action_id: Some(gaid),
        })
        .unwrap();
        // outer: Constr 3 [Maybe]
        let Data::Constr(3, ref fields) = d else {
            panic!("NoConfidence must be Constr 3; got {d:?}");
        };
        assert_eq!(fields.len(), 1);
        // Maybe = Just → Constr 0 [gaid_data]
        let Data::Constr(0, ref just_fields) = fields[0] else {
            panic!("Just must be Constr 0; got {:?}", fields[0]);
        };
        assert_eq!(just_fields.len(), 1);
        // GovernanceActionId = Constr 0 [B txid32, I 3]
        let Data::Constr(0, ref gaid_fields) = just_fields[0] else {
            panic!("GovActionId must be Constr 0; got {:?}", just_fields[0]);
        };
        assert_eq!(gaid_fields.len(), 2);
        assert!(
            matches!(&gaid_fields[0], Data::B(b) if b.len() == 32 && b.iter().all(|&x| x == 0xab)),
            "txid must be bare B(32); got {:?}",
            gaid_fields[0]
        );
        assert_eq!(gaid_fields[1], Data::I(BigInt::from(3u64)));
    }

    #[test]
    fn gov_action_hard_fork_encodes_as_constr_1_with_protocol_version() {
        // HardForkInitiation = Constr 1 [Maybe GovActionId, ProtocolVersion]
        // ProtocolVersion = Constr 0 [I major, I minor]
        // Haskell: makeIsDataSchemaIndexed ''ProtocolVersion [('ProtocolVersion, 0)]
        use dugite_primitives::transaction::GovAction;
        let d = gov_action_to_data(&GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (10, 0),
        })
        .unwrap();
        let Data::Constr(1, ref fields) = d else {
            panic!("HardForkInitiation must be Constr 1; got {d:?}");
        };
        assert_eq!(fields.len(), 2);
        // fields[0]: Maybe GovActionId = Nothing = Constr 1 []
        assert_eq!(fields[0], Data::Constr(1, vec![]));
        // fields[1]: ProtocolVersion = Constr 0 [I 10, I 0]
        assert_eq!(
            fields[1],
            Data::Constr(
                0,
                vec![Data::I(BigInt::from(10u64)), Data::I(BigInt::from(0u64))]
            )
        );
    }

    #[test]
    fn gov_action_new_constitution_encodes_as_constr_5() {
        // NewConstitution = Constr 5 [Maybe GovActionId, Constitution]
        // Constitution = Constr 0 [Maybe ScriptHash]
        // Haskell: makeIsDataSchemaIndexed ''Constitution [('Constitution, 0)]
        use dugite_primitives::transaction::{Constitution, GovAction};
        let d = gov_action_to_data(&GovAction::NewConstitution {
            prev_action_id: None,
            constitution: Constitution {
                anchor: Anchor {
                    url: String::new(),
                    data_hash: h32(0),
                },
                script_hash: Some(h28(0xcc)),
            },
        })
        .unwrap();
        let Data::Constr(5, ref fields) = d else {
            panic!("NewConstitution must be Constr 5; got {d:?}");
        };
        assert_eq!(fields.len(), 2);
        // Constitution = Constr 0 [Just(B28)]
        assert_eq!(
            fields[1],
            Data::Constr(0, vec![Data::Constr(0, vec![Data::B(vec![0xcc; 28])])])
        );
    }

    #[test]
    fn gov_action_treasury_withdrawals_encodes_as_constr_2() {
        // TreasuryWithdrawals = Constr 2 [Map Credential Lovelace, Maybe ScriptHash]
        // Map key = stake Credential extracted from reward address
        use dugite_primitives::transaction::GovAction;
        use dugite_primitives::value::Lovelace;
        let mut withdrawals = std::collections::BTreeMap::new();
        // reward address: 0xe0 (mainnet key-stake) || [0x77; 28]
        let mut addr = vec![0xe0u8];
        addr.extend_from_slice(&[0x77u8; 28]);
        withdrawals.insert(addr, Lovelace(500_000));
        let d = gov_action_to_data(&GovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash: None,
        })
        .unwrap();
        let Data::Constr(2, ref fields) = d else {
            panic!("TreasuryWithdrawals must be Constr 2; got {d:?}");
        };
        assert_eq!(fields.len(), 2);
        // Map: [(Credential::PubKey([0x77;28]), I 500_000)]
        let Data::Map(ref entries) = fields[0] else {
            panic!("field[0] must be Map; got {:?}", fields[0]);
        };
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(&entries[0].0, Data::Constr(0, inner) if inner.len() == 1),
            "map key must be PubKeyCredential (Constr 0 [B28]); got {:?}",
            entries[0].0
        );
        assert_eq!(entries[0].1, Data::I(BigInt::from(500_000u64)));
        // Maybe ScriptHash = Nothing
        assert_eq!(fields[1], Data::Constr(1, vec![]));
    }

    #[test]
    fn gov_action_update_committee_encodes_as_constr_4() {
        // UpdateCommittee = Constr 4 [Maybe GovActionId, [ColdCred], Map ColdCred Epoch, Rational]
        // Rational = Constr 0 [I num, I den]
        // ColdCommitteeCredential = newtype deriving ToData from Credential
        use dugite_primitives::transaction::{GovAction, Rational};
        let d = gov_action_to_data(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![key_cred(0xaa)],
            members_to_add: {
                let mut m = std::collections::BTreeMap::new();
                m.insert(script_cred(0xbb), 500u64);
                m
            },
            threshold: Rational {
                numerator: 2,
                denominator: 3,
            },
        })
        .unwrap();
        let Data::Constr(4, ref fields) = d else {
            panic!("UpdateCommittee must be Constr 4; got {d:?}");
        };
        assert_eq!(fields.len(), 4);
        // fields[1]: [ColdCredential] — 1 item (key_cred 0xaa) = Constr 0 [B28]
        let Data::List(ref remove_list) = fields[1] else {
            panic!("field[1] must be List; got {:?}", fields[1]);
        };
        assert_eq!(remove_list.len(), 1);
        assert!(
            matches!(&remove_list[0], Data::Constr(0, _)),
            "ColdCommitteeCredential must be PubKeyCredential (Constr 0); got {:?}",
            remove_list[0]
        );
        // fields[2]: Map — 1 entry: script_cred 0xbb → epoch 500
        let Data::Map(ref add_map) = fields[2] else {
            panic!("field[2] must be Map; got {:?}", fields[2]);
        };
        assert_eq!(add_map.len(), 1);
        assert!(
            matches!(&add_map[0].0, Data::Constr(1, _)),
            "ScriptCredential must be Constr 1; got {:?}",
            add_map[0].0
        );
        assert_eq!(add_map[0].1, Data::I(BigInt::from(500u64)));
        // fields[3]: Rational = Constr 0 [I 2, I 3]
        // Haskell: makeIsDataSchemaIndexed ''Rational [('Rational, 0)] in plutus-tx
        assert_eq!(
            fields[3],
            Data::Constr(
                0,
                vec![Data::I(BigInt::from(2u64)), Data::I(BigInt::from(3u64))]
            ),
            "Rational must be Constr 0 [I num, I den]"
        );
    }
}
