//! Per-redeemer script + datum resolution.
//!
//! Phase-2 evaluation runs every redeemer in a tx in order. For each
//! one, the evaluator needs to know:
//!
//! 1. **Which script does this redeemer execute?** The
//!    `(RedeemerTag, index)` pair points into the tx body (inputs,
//!    mint, certificates, withdrawals, voting, proposals); resolving
//!    that index back to a script hash is purpose-specific.
//! 2. **Where does that script's bytes live?** Plutus scripts in the
//!    witness set (V1/V2/V3 keyed by language) or referenced inline
//!    via a UTxO's `script_ref`.
//! 3. **Is there a datum?** Only V1/V2 spending redeemers consume a
//!    datum; V3 inlines the datum into `ScriptInfo::Spending`.
//! 4. **What `ScriptPurpose` does Plutus see?** The Plutus
//!    `ScriptPurpose` enum mirrors the redeemer tag but carries the
//!    purpose-specific payload (the spent input's outref, the minted
//!    policy id, etc).
//!
//! This module exposes `resolve_redeemer` which packages all four
//! into a single [`ResolvedRedeemer`] struct. Per-version
//! `ScriptContext` construction (UPLC-9 part 4b) takes that struct
//! and produces the actual `Data` value passed to the CEK machine.

use crate::phase_two::PhaseTwoError;
use crate::script_context::ScriptPurpose;
use crate::tx_info_populate::{script_ref_hash, sort_inputs};
use dugite_primitives::credentials::Credential as PrimCred;
use dugite_primitives::transaction::{
    PlutusData as PrimPlutusData, Redeemer, RedeemerTag, ScriptRef as PrimScriptRef, Transaction,
    TransactionInput as PrimTxIn, TransactionOutput as PrimTxOut,
};

/// Plutus language version a script targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptLanguage {
    PlutusV1,
    PlutusV2,
    PlutusV3,
}

/// Everything a [`crate::phase_two::eval_phase_two_raw`] needs to know
/// about a single redeemer before invoking the CEK machine on it.
#[derive(Debug, Clone)]
pub struct ResolvedRedeemer {
    /// The redeemer's tag (Spend / Mint / Cert / Reward / Vote / Propose).
    pub tag: RedeemerTag,
    /// Index into the relevant tx-body list.
    pub index: u32,
    /// The script hash this redeemer dispatches against.
    pub script_hash: [u8; 28],
    /// Where the script's raw bytes live (witness set vs reference input).
    pub script_bytes: Vec<u8>,
    /// Plutus language version the script targets.
    pub language: ScriptLanguage,
    /// The Plutus `ScriptPurpose` value the script context exposes.
    pub purpose: ScriptPurpose,
    /// Datum bytes, only set for V1/V2 spending redeemers.
    ///
    /// V3 spending redeemers embed the datum in
    /// `ScriptInfo::Spending { datum: Option<Data> }`, so this field
    /// stays `None` for them; the per-redeemer ScriptContext builder
    /// (UPLC-9 part 4b) handles V3 inline-datum lookup separately.
    pub datum: Option<PrimPlutusData>,
    /// The redeemer's declared `ExUnits` budget. Per-redeemer
    /// budget enforcement happens at eval time.
    pub redeemer_data: PrimPlutusData,
    /// (cpu, mem) declared per the redeemer wire entry.
    pub declared_ex_units: (u64, u64),
}

/// Resolve every redeemer in `tx.witness_set.redeemers` against the
/// resolved-UTxO map and the witness set scripts/datums. Returns one
/// [`ResolvedRedeemer`] per witness-set redeemer in the order they
/// appear.
pub fn resolve_redeemers(
    tx: &Transaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<Vec<ResolvedRedeemer>, PhaseTwoError> {
    let mut out = Vec::with_capacity(tx.witness_set.redeemers.len());
    for r in &tx.witness_set.redeemers {
        out.push(resolve_redeemer(tx, resolved, r)?);
    }
    Ok(out)
}

/// Resolve a single redeemer entry.
pub fn resolve_redeemer(
    tx: &Transaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
    r: &Redeemer,
) -> Result<ResolvedRedeemer, PhaseTwoError> {
    match r.tag {
        RedeemerTag::Spend => resolve_spend(tx, resolved, r),
        RedeemerTag::Mint => resolve_mint(tx, r, resolved),
        RedeemerTag::Cert => resolve_cert(tx, r, resolved),
        RedeemerTag::Reward => resolve_reward(tx, r, resolved),
        RedeemerTag::Vote => resolve_vote(tx, r, resolved),
        RedeemerTag::Propose => resolve_propose(tx, r, resolved),
        // Dijkstra `DijkstraGuarding` — credential-based guard
        // satisfied by a Plutus script.
        RedeemerTag::Guarding => resolve_guarding(tx, r, resolved),
    }
}

// ────────────────────────────────────────────────────────────────────
// Per-purpose resolvers
// ────────────────────────────────────────────────────────────────────

fn resolve_spend(
    tx: &Transaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
    r: &Redeemer,
) -> Result<ResolvedRedeemer, PhaseTwoError> {
    let idx = r.index as usize;
    // cardano-ledger's `alonzoRedeemerPointer` / `conwayRedeemerPointer`
    // resolve a `Spending` redeemer index into `Set.elemAt idx (txBody ^.
    // inputsTxBodyL)` — i.e. the i-th element of the inputs treated as an
    // **ascending `Ord TxIn` sorted set** (TxId raw bytes, then TxIx
    // numerically). The on-wire CBOR `array` is an unordered multiset from
    // the Plutus ledger's perspective; indexing into wire order produces the
    // wrong input when the on-chain encoding order differs from the sorted
    // order — the confirmed Alonzo mainnet divergence (tx
    // 4a3b78c246f30425754966396d10ffcba0b9cc8b97c6d3f9f54d8c6d30154422).
    let sorted_inputs = sort_inputs(&tx.body.inputs);
    let input = sorted_inputs.get(idx).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "spend redeemer references inputs[{idx}] but tx has {n} inputs",
            n = tx.body.inputs.len()
        ))
    })?;
    // Find the resolved output for this input.
    let (_, resolved_out, _) = resolved
        .iter()
        .find(|(i, _, _)| i.transaction_id == input.transaction_id && i.index == input.index)
        .ok_or_else(|| {
            PhaseTwoError::UtxoDecode(format!(
                "spend redeemer #{idx}: input not in resolved utxo map"
            ))
        })?;
    // The payment credential must be a Script for a spend redeemer.
    let script_hash = script_hash_from_address(&resolved_out.address).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "spend redeemer #{idx}: spent output's address is not script-locked"
        ))
    })?;
    let (script_bytes, language) = find_script_bytes(tx, resolved, &script_hash)?;
    // Datum lookup for V1/V2 only.
    let datum = match language {
        ScriptLanguage::PlutusV1 | ScriptLanguage::PlutusV2 => {
            Some(resolve_spend_datum(tx, resolved_out, &script_hash)?)
        }
        ScriptLanguage::PlutusV3 => None,
    };
    let purpose = ScriptPurpose::Spending(crate::script_context::TxOutRef {
        tx_id: input.transaction_id.0,
        idx: input.index as u64,
    });
    Ok(ResolvedRedeemer {
        tag: r.tag.clone(),
        index: r.index,
        script_hash,
        script_bytes,
        language,
        purpose,
        datum,
        redeemer_data: r.data.clone(),
        declared_ex_units: (r.ex_units.mem, r.ex_units.steps),
    })
}

fn resolve_mint(
    tx: &Transaction,
    r: &Redeemer,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<ResolvedRedeemer, PhaseTwoError> {
    let idx = r.index as usize;
    // The mint map is ordered by `BTreeMap` iteration (lex by policy id).
    // The redeemer's index corresponds to the i-th policy in that order.
    let policy_id = tx
        .body
        .mint
        .keys()
        .nth(idx)
        .ok_or_else(|| {
            PhaseTwoError::Internal(format!(
                "mint redeemer references policy[{idx}] but tx has {n} policies",
                n = tx.body.mint.len()
            ))
        })?
        .0;
    let (script_bytes, language) = find_script_bytes(tx, resolved, &policy_id)?;
    Ok(ResolvedRedeemer {
        tag: r.tag.clone(),
        index: r.index,
        script_hash: policy_id,
        script_bytes,
        language,
        purpose: ScriptPurpose::Minting(policy_id),
        datum: None,
        redeemer_data: r.data.clone(),
        declared_ex_units: (r.ex_units.mem, r.ex_units.steps),
    })
}

fn resolve_cert(
    tx: &Transaction,
    r: &Redeemer,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<ResolvedRedeemer, PhaseTwoError> {
    let idx = r.index as usize;
    let cert = tx.body.certificates.get(idx).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "cert redeemer references certificates[{idx}] but tx has {n}",
            n = tx.body.certificates.len()
        ))
    })?;
    let script_hash = cert_script_hash(cert).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "cert redeemer #{idx}: cert does not carry a script credential"
        ))
    })?;
    let (script_bytes, language) = find_script_bytes(tx, resolved, &script_hash)?;
    // The `Certifying` purpose carries the cert as Data. The SCHEMA is
    // version-specific: V1/V2 use the `DCert` shape (StakingHash-wrapped, no
    // index in the purpose); V3 uses the Conway `TxCert` shape (index kept).
    // `ScriptPurpose::to_data` / `to_data_v3` consume `c.0` accordingly, so the
    // payload must be built with the matching encoder for `language`.
    let tx_cert = match language {
        ScriptLanguage::PlutusV1 | ScriptLanguage::PlutusV2 => {
            crate::populate_gov::certificate_to_plutus_v1v2(cert)?
        }
        ScriptLanguage::PlutusV3 => crate::populate_gov::certificate_to_plutus(cert)?,
    };
    let purpose = ScriptPurpose::Certifying(idx as u64, tx_cert);
    Ok(ResolvedRedeemer {
        tag: r.tag.clone(),
        index: r.index,
        script_hash,
        script_bytes,
        language,
        purpose,
        datum: None,
        redeemer_data: r.data.clone(),
        declared_ex_units: (r.ex_units.mem, r.ex_units.steps),
    })
}

fn resolve_reward(
    tx: &Transaction,
    r: &Redeemer,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<ResolvedRedeemer, PhaseTwoError> {
    let idx = r.index as usize;
    // Withdrawals map iterates in BTreeMap order (lex by reward_account bytes).
    let (reward_account, _) = tx.body.withdrawals.iter().nth(idx).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "reward redeemer references withdrawals[{idx}] but tx has {n}",
            n = tx.body.withdrawals.len()
        ))
    })?;
    // Parse the 29-byte reward-address blob and unwrap to the stake credential.
    let addr = dugite_primitives::address::Address::from_bytes(reward_account).map_err(|e| {
        PhaseTwoError::Internal(format!("reward redeemer #{idx}: reward_account: {e}"))
    })?;
    let stake_cred = match addr {
        dugite_primitives::address::Address::Reward(r) => r.stake,
        other => {
            return Err(PhaseTwoError::Internal(format!(
                "reward redeemer #{idx}: expected Reward address, got {other:?}"
            )));
        }
    };
    let script_hash = match stake_cred {
        PrimCred::Script(h) => h.0,
        PrimCred::VerificationKey(_) => {
            return Err(PhaseTwoError::Internal(format!(
                "reward redeemer #{idx}: stake credential is a key, not a script"
            )));
        }
    };
    let (script_bytes, language) = find_script_bytes(tx, resolved, &script_hash)?;
    let purpose = ScriptPurpose::Rewarding(crate::script_context::Credential::Script(script_hash));
    Ok(ResolvedRedeemer {
        tag: r.tag.clone(),
        index: r.index,
        script_hash,
        script_bytes,
        language,
        purpose,
        datum: None,
        redeemer_data: r.data.clone(),
        declared_ex_units: (r.ex_units.mem, r.ex_units.steps),
    })
}

/// Resolve a `Vote` redeemer.
///
/// A `Vote` redeemer with index `i` refers to the `i`-th voter in the
/// tx body's `voting_procedures` map (iterated in `BTreeMap` order —
/// `Voter` is `Ord` by constructor tag first, then credential bytes).
///
/// Haskell reference: `Cardano.Ledger.Conway.Scripts.ConwayPlutusPurpose`
/// (`ConwayVoting !(f Word32 Voter)`) — the `Word32` index is the key into
/// the sorted `VotingProcedures` map via `redeemerPointerInverse`.
/// `transPlutusPurposeV3` → `VotingPurpose (AsIxItem _ voter) → PV3.Voting (transVoter voter)`.
/// Conway TxInfo.hs `transVoter`: CommitteeVoter=0, DRepVoter=1, StakePoolVoter=2.
fn resolve_vote(
    tx: &Transaction,
    r: &Redeemer,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<ResolvedRedeemer, PhaseTwoError> {
    let idx = r.index as usize;
    // `voting_procedures` is `BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>>`.
    // `Voter` derives `Ord` by constructor order: ConstitutionalCommittee < DRep < StakePool
    // (matching the CBOR wire tags 0/2/4). Iteration in `Map.toList` order is the canonical
    // index space for `ConwayVoting` redeemers.
    let (voter, _) = tx.body.voting_procedures.iter().nth(idx).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "vote redeemer references voting_procedures[{idx}] but tx has {n}",
            n = tx.body.voting_procedures.len()
        ))
    })?;
    // Extract the script hash from the voter's credential.
    let script_hash = voter_script_hash(voter).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "vote redeemer #{idx}: voter is not script-credentialed (StakePool voters \
             are key-only; only DRep and CommitteeVoter with Script credential can run Plutus)"
        ))
    })?;
    let (script_bytes, language) = find_script_bytes(tx, resolved, &script_hash)?;
    let purpose = ScriptPurpose::Voting(crate::populate_gov::voter_to_plutus(voter));
    Ok(ResolvedRedeemer {
        tag: r.tag.clone(),
        index: r.index,
        script_hash,
        script_bytes,
        language,
        purpose,
        datum: None,
        redeemer_data: r.data.clone(),
        declared_ex_units: (r.ex_units.mem, r.ex_units.steps),
    })
}

/// Resolve a `Propose` redeemer.
///
/// A `Propose` redeemer with index `i` refers to the `i`-th proposal
/// in `tx.body.proposal_procedures` (a `Vec` preserving insertion/wire
/// order — Haskell's `OSet ProposalProcedure` preserves insertion order).
///
/// Haskell reference: `ConwayProposing !(f Word32 (ProposalProcedure era))`.
/// `transPlutusPurposeV3` → `ProposingPurpose (AsIxItem ix proposal) →
///   PV3.Proposing (toInteger ix) (transProposal proxy proposal)`.
/// The `ScriptPurpose::Proposing(idx, ProposalProcedure)` shape carries the
/// proposal index and the proposal's Data encoding as a `PlProposalProcedure`.
fn resolve_propose(
    tx: &Transaction,
    r: &Redeemer,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<ResolvedRedeemer, PhaseTwoError> {
    let idx = r.index as usize;
    let proposal = tx.body.proposal_procedures.get(idx).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "propose redeemer references proposal_procedures[{idx}] but tx has {n}",
            n = tx.body.proposal_procedures.len()
        ))
    })?;
    // The script hash for a proposal redeemer comes from the gov action's guardrail script.
    // In practice, only ParameterChange and TreasuryWithdrawals carry a policy_hash that
    // makes a Plutus script mandatory. However, the redeemer index was encoded on-chain
    // regardless — extract the policy hash or error if absent.
    let script_hash = proposal_script_hash(proposal).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "propose redeemer #{idx}: proposal does not carry a guardrail script hash \
             (only ParameterChange and TreasuryWithdrawals can have a mandatory Plutus script)"
        ))
    })?;
    let (script_bytes, language) = find_script_bytes(tx, resolved, &script_hash)?;
    let pl_proposal = crate::populate_gov::proposal_to_plutus(proposal)?;
    let purpose = ScriptPurpose::Proposing(idx as u64, pl_proposal);
    Ok(ResolvedRedeemer {
        tag: r.tag.clone(),
        index: r.index,
        script_hash,
        script_bytes,
        language,
        purpose,
        datum: None,
        redeemer_data: r.data.clone(),
        declared_ex_units: (r.ex_units.mem, r.ex_units.steps),
    })
}

/// Dijkstra `DijkstraGuarding` redeemer — resolves the script
/// dispatched by a credential-based guard at `tx.body.guards[index]`.
///
/// Only script-credential guards are valid targets for a Guarding
/// redeemer; key-hash guards are satisfied by vkey signatures and never
/// invoke a script. Per
/// `Cardano.Ledger.Dijkstra.Scripts.DijkstraGuarding` (`Sum 6`) and the
/// V3/V4 script-context emission in `populate_v3`. Issue #475 Phase 3.5.
fn resolve_guarding(
    tx: &Transaction,
    r: &Redeemer,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<ResolvedRedeemer, PhaseTwoError> {
    let idx = r.index as usize;
    let cred = tx.body.guards.get(idx).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "guarding redeemer references guards[{idx}] but tx has {n}",
            n = tx.body.guards.len()
        ))
    })?;
    let script_hash = match cred {
        PrimCred::Script(h) => h.0,
        PrimCred::VerificationKey(_) => {
            return Err(PhaseTwoError::Internal(format!(
                "guarding redeemer #{idx}: guard credential is a key, not a script — \
                 only Plutus / native-script guards admit a Guarding redeemer"
            )));
        }
    };
    let (script_bytes, language) = find_script_bytes(tx, resolved, &script_hash)?;
    Ok(ResolvedRedeemer {
        tag: r.tag.clone(),
        index: r.index,
        script_hash,
        script_bytes,
        language,
        purpose: ScriptPurpose::Guarding(script_hash),
        datum: None,
        redeemer_data: r.data.clone(),
        declared_ex_units: (r.ex_units.mem, r.ex_units.steps),
    })
}

// ────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────

/// Extract the script-credential hash from an address's payment
/// component, or `None` if the address is key-locked / Byron / reward.
fn script_hash_from_address(addr: &dugite_primitives::address::Address) -> Option<[u8; 28]> {
    use dugite_primitives::address::Address::*;
    let payment = match addr {
        Base(b) => &b.payment,
        Enterprise(e) => &e.payment,
        Pointer(p) => &p.payment,
        Reward(_) | Byron(_) => return None,
    };
    match payment {
        PrimCred::Script(h) => Some(h.0),
        PrimCred::VerificationKey(_) => None,
    }
}

/// Extract the script-hash a certificate dispatches against, or
/// `None` for certs that don't carry a script credential.
fn cert_script_hash(cert: &dugite_primitives::transaction::Certificate) -> Option<[u8; 28]> {
    use dugite_primitives::transaction::Certificate::*;
    let cred = match cert {
        StakeRegistration(c) | StakeDeregistration(c) => c,
        ConwayStakeRegistration { credential, .. }
        | ConwayStakeDeregistration { credential, .. }
        | StakeDelegation { credential, .. }
        | RegDRep { credential, .. }
        | UnregDRep { credential, .. }
        | UpdateDRep { credential, .. }
        | VoteDelegation { credential, .. }
        | StakeVoteDelegation { credential, .. }
        | RegStakeDeleg { credential, .. }
        | RegStakeVoteDeleg { credential, .. }
        | VoteRegDeleg { credential, .. } => credential,
        CommitteeHotAuth {
            cold_credential, ..
        }
        | CommitteeColdResign {
            cold_credential, ..
        } => cold_credential,
        PoolRegistration(_) | PoolRetirement { .. } => return None,
        GenesisKeyDelegation { .. } | MoveInstantaneousRewards { .. } => return None,
    };
    match cred {
        PrimCred::Script(h) => Some(h.0),
        PrimCred::VerificationKey(_) => None,
    }
}

/// Extract the script-credential hash from a `Voter`, or `None` if the
/// voter type / credential does not invoke a Plutus script.
///
/// Only `ConstitutionalCommittee(Script(_))` and `DRep(Script(_))` can
/// carry script credentials. `StakePool` voters are always key-only.
fn voter_script_hash(voter: &dugite_primitives::transaction::Voter) -> Option<[u8; 28]> {
    use dugite_primitives::transaction::Voter::*;
    let cred = match voter {
        ConstitutionalCommittee(c) => c,
        DRep(c) => c,
        StakePool(_) => return None,
    };
    match cred {
        PrimCred::Script(h) => Some(h.0),
        PrimCred::VerificationKey(_) => None,
    }
}

/// Extract the mandatory guardrail script hash from a `ProposalProcedure`,
/// or `None` if the proposal does not carry one.
///
/// Only `ParameterChange` and `TreasuryWithdrawals` carry a `policy_hash`
/// field. All other gov-action types cannot have a mandatory Plutus script
/// and should not appear with a `Propose` redeemer (the chain would have
/// rejected the tx in phase-1).
fn proposal_script_hash(p: &dugite_primitives::transaction::ProposalProcedure) -> Option<[u8; 28]> {
    use dugite_primitives::transaction::GovAction::*;
    match &p.gov_action {
        ParameterChange { policy_hash, .. } => policy_hash.as_ref().map(|h| h.0),
        TreasuryWithdrawals { policy_hash, .. } => policy_hash.as_ref().map(|h| h.0),
        _ => None,
    }
}

/// Find the script bytes for `script_hash` by checking, in order:
///
/// 1. `tx.witness_set.plutus_v1_scripts` (hash each, match)
/// 2. `tx.witness_set.plutus_v2_scripts`
/// 3. `tx.witness_set.plutus_v3_scripts`
/// 4. Each `resolved` UTxO's `script_ref` (V1/V2/V3 Plutus scripts).
///
/// Returns the raw script bytes + the language version. Native
/// scripts surface a typed `Internal` error — phase-2 only runs
/// Plutus.
fn find_script_bytes(
    tx: &Transaction,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
    script_hash: &[u8; 28],
) -> Result<(Vec<u8>, ScriptLanguage), PhaseTwoError> {
    use dugite_primitives::hash::blake2b_224;

    fn hash_with_tag(tag: u8, bytes: &[u8]) -> [u8; 28] {
        let mut buf = Vec::with_capacity(1 + bytes.len());
        buf.push(tag);
        buf.extend_from_slice(bytes);
        blake2b_224(&buf).0
    }

    // 1-3. Witness-set Plutus scripts, by language.
    for (lang_tag, list, language) in [
        (
            1u8,
            &tx.witness_set.plutus_v1_scripts,
            ScriptLanguage::PlutusV1,
        ),
        (
            2u8,
            &tx.witness_set.plutus_v2_scripts,
            ScriptLanguage::PlutusV2,
        ),
        (
            3u8,
            &tx.witness_set.plutus_v3_scripts,
            ScriptLanguage::PlutusV3,
        ),
    ] {
        for raw in list {
            if &hash_with_tag(lang_tag, raw) == script_hash {
                return Ok((raw.clone(), language));
            }
        }
    }
    // 4. Reference scripts on the resolved UTxOs.
    for (_, out, _) in resolved {
        if let Some(sref) = &out.script_ref {
            if &script_ref_hash(sref) == script_hash {
                match sref {
                    PrimScriptRef::PlutusV1(b) => return Ok((b.clone(), ScriptLanguage::PlutusV1)),
                    PrimScriptRef::PlutusV2(b) => return Ok((b.clone(), ScriptLanguage::PlutusV2)),
                    PrimScriptRef::PlutusV3(b) => return Ok((b.clone(), ScriptLanguage::PlutusV3)),
                    // PlutusV4 (Dijkstra) ref-script resolution is part of
                    // issue #475 Phase 5; the `ScriptLanguage::PlutusV4`
                    // variant + evaluator wiring don't exist yet. Until they
                    // land, refuse to resolve V4 ref-scripts so phase-2
                    // doesn't silently feed V4 bytes to the V3 machine.
                    PrimScriptRef::PlutusV4(_) => {
                        return Err(PhaseTwoError::Internal(format!(
                            "find_script_bytes: script {h} resolves to a PlutusV4 \
                             reference script — V4 evaluation is not yet implemented \
                             (issue #475 Phase 5)",
                            h = hex::encode(script_hash)
                        )));
                    }
                    PrimScriptRef::NativeScript(_) => {
                        return Err(PhaseTwoError::Internal(format!(
                            "find_script_bytes: script {h} resolves to a native script — \
                             phase-2 only evaluates Plutus",
                            h = hex::encode(script_hash)
                        )));
                    }
                }
            }
        }
    }
    Err(PhaseTwoError::MissingScript(hex::encode(script_hash)))
}

/// Resolve the datum for a V1/V2 spending redeemer. Two cases:
///
/// 1. The spent output has an inline datum (Babbage+): use it directly.
/// 2. The spent output has a datum hash: look up the matching `PlutusData`
///    in `tx.witness_set.plutus_data` and verify the hash matches.
fn resolve_spend_datum(
    tx: &Transaction,
    spent_output: &PrimTxOut,
    script_hash: &[u8; 28],
) -> Result<PrimPlutusData, PhaseTwoError> {
    use dugite_primitives::hash::blake2b_256;
    use dugite_primitives::transaction::OutputDatum as PrimOutputDatum;
    match &spent_output.datum {
        PrimOutputDatum::InlineDatum { data, .. } => Ok(data.clone()),
        PrimOutputDatum::DatumHash(h) => {
            // A datum hash is `blake2b_256` over the witness datum's ORIGINAL
            // CBOR bytes (Haskell hashes the memoised raw bytes). Many on-chain
            // datums are encoded non-canonically (e.g. the general `Constr`
            // tag-102 form for small indices) and do NOT round-trip through a
            // re-encode, so re-encoding each witness datum and hashing it fails
            // to match — the "datum not found" phase-2 divergence. Match against
            // the preserved per-element raw spans first (same fix as the phase-1
            // datum-witness check); fall back to a canonical re-encode only when
            // no raw bytes exist (datums the node constructs itself).
            let spans = tx
                .witness_set
                .raw_plutus_data_cbor
                .as_deref()
                .and_then(dugite_serialization::plutus_data_element_spans)
                .filter(|s| s.len() == tx.witness_set.plutus_data.len());
            if let Some(spans) = spans {
                for (i, raw) in spans.iter().enumerate() {
                    if blake2b_256(raw).0 == h.0 {
                        return Ok(tx.witness_set.plutus_data[i].clone());
                    }
                }
            } else {
                for d in &tx.witness_set.plutus_data {
                    let translated = crate::tx_info_populate::plutus_data_to_data(d);
                    let cbor = translated.to_cbor().map_err(|e| {
                        PhaseTwoError::Internal(format!("resolve_spend_datum: to_cbor: {e}"))
                    })?;
                    if blake2b_256(&cbor).0 == h.0 {
                        return Ok(d.clone());
                    }
                }
            }
            Err(PhaseTwoError::MissingDatum {
                hash: hex::encode(h.0),
            })
        }
        PrimOutputDatum::None => Err(PhaseTwoError::MissingDatum {
            hash: format!(
                "(no datum on script-locked output; script {})",
                hex::encode(script_hash)
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::address::{Address as PrimAddress, EnterpriseAddress};
    use dugite_primitives::era::Era;
    use dugite_primitives::hash::Hash;
    use dugite_primitives::network::NetworkId;
    use dugite_primitives::transaction::{
        ExUnits, OutputDatum as PrimOutputDatum, Redeemer, RedeemerTag, Transaction,
        TransactionBody, TransactionInput, TransactionOutput, TransactionWitnessSet,
    };
    use dugite_primitives::value::{Lovelace, Value};
    use std::collections::BTreeMap;

    fn h28(b: u8) -> dugite_primitives::hash::Hash28 {
        Hash::<28>([b; 28])
    }

    fn h32(b: u8) -> dugite_primitives::hash::Hash<32> {
        Hash::<32>([b; 32])
    }

    fn enterprise_script_output(script_hash: [u8; 28], lovelace: u64) -> TransactionOutput {
        TransactionOutput {
            address: PrimAddress::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: PrimCred::Script(Hash::<28>(script_hash)),
            }),
            value: Value::lovelace(lovelace),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    fn minimal_body() -> TransactionBody {
        TransactionBody {
            inputs: vec![],
            outputs: vec![],
            fee: Lovelace(0),
            ttl: None,
            certificates: vec![],
            withdrawals: BTreeMap::new(),
            auxiliary_data_hash: None,
            validity_interval_start: None,
            mint: BTreeMap::new(),
            script_data_hash: None,
            collateral: vec![],
            required_signers: vec![],
            network_id: None,
            collateral_return: None,
            total_collateral: None,
            reference_inputs: vec![],
            update: None,
            voting_procedures: BTreeMap::new(),
            proposal_procedures: vec![],
            treasury_value: None,
            donation: None,
            sub_transactions: vec![],
            account_balance_intervals: vec![],
            direct_deposits: ::std::collections::BTreeMap::new(),
            guards: Vec::new(),
        }
    }

    fn empty_witness() -> TransactionWitnessSet {
        TransactionWitnessSet {
            vkey_witnesses: vec![],
            native_scripts: vec![],
            bootstrap_witnesses: vec![],
            plutus_v1_scripts: vec![],
            plutus_v2_scripts: vec![],
            plutus_v3_scripts: vec![],
            plutus_data: vec![],
            redeemers: vec![],
            raw_redeemers_cbor: None,
            raw_plutus_data_cbor: None,
            original_script_data_hash: None,
        }
    }

    fn build_tx(body: TransactionBody, witness_set: TransactionWitnessSet) -> Transaction {
        Transaction {
            hash: h32(0),
            era: Era::Conway,
            body,
            witness_set,
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        }
    }

    fn hexd(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A datum hash is over the witness datum's ORIGINAL CBOR bytes, so phase-2
    /// datum resolution must match those — not a re-encode. Vector: mainnet
    /// datum `5ff23baed5…` encoded in the general `Constr` form (CBOR tag 102,
    /// `d866`) which re-encodes (canonically, tag 121) to a DIFFERENT hash. With
    /// the preserved raw spans it resolves; the re-encode fallback (no raw bytes)
    /// must NOT match. This is the phase-2 analogue of the phase-1 datum-witness
    /// fix and clears the "datum not found" divergence sub-class.
    #[test]
    fn resolve_spend_datum_matches_original_noncanonical_bytes() {
        use dugite_primitives::hash::blake2b_256;
        use dugite_primitives::transaction::{OutputDatum as PrimOutputDatum, PlutusData as PD};
        use num_bigint::BigInt;

        let original = hexd(
            "d866820086581ca3250750af6227b5a7dc689de94c83728a9d1d4029cc232d4a46f81e\
             1a041cdb40581c023cec350597bdf2a2b6945e62e0111d9808caf7a9353a2ab91e8beb\
             50534f434945545932354c4d4239323332581c63a3bc3807c6a51f85570ad9a82ed46b\
             db96feeabae6c4aa0526d4ed181e",
        );
        let datum_hash = Hash::<32>(blake2b_256(&original).0);
        // Structural form of the same datum (re-encodes to tag-121, different hash).
        let datum = PD::Constr(
            0,
            vec![
                PD::Bytes(hexd(
                    "a3250750af6227b5a7dc689de94c83728a9d1d4029cc232d4a46f81e",
                )),
                PD::Integer(BigInt::from(0x041cdb40u32)),
                PD::Bytes(hexd(
                    "023cec350597bdf2a2b6945e62e0111d9808caf7a9353a2ab91e8beb",
                )),
                PD::Bytes(hexd("534f434945545932354c4d4239323332")),
                PD::Bytes(hexd(
                    "63a3bc3807c6a51f85570ad9a82ed46bdb96feeabae6c4aa0526d4ed",
                )),
                PD::Integer(BigInt::from(0x1eu32)),
            ],
        );

        let mut out = enterprise_script_output([0xAA; 28], 5_000_000);
        out.datum = PrimOutputDatum::DatumHash(datum_hash);

        // With preserved raw spans → resolves by original bytes.
        let mut ws = empty_witness();
        ws.plutus_data = vec![datum.clone()];
        let mut raw_array = vec![0x81u8]; // array(1)
        raw_array.extend_from_slice(&original);
        ws.raw_plutus_data_cbor = Some(raw_array);
        let tx = build_tx(minimal_body(), ws);
        let resolved =
            resolve_spend_datum(&tx, &out, &[0xAA; 28]).expect("datum must resolve via raw spans");
        assert_eq!(resolved, datum);

        // Without raw bytes → the canonical re-encode (tag-121) cannot match.
        let mut ws2 = empty_witness();
        ws2.plutus_data = vec![datum];
        ws2.raw_plutus_data_cbor = None;
        let tx2 = build_tx(minimal_body(), ws2);
        assert!(
            resolve_spend_datum(&tx2, &out, &[0xAA; 28]).is_err(),
            "re-encode fallback must not match a non-canonically-encoded datum hash"
        );
    }

    fn plutus_v3_script_with_hash(bytes: &[u8]) -> [u8; 28] {
        let mut buf = vec![3u8];
        buf.extend_from_slice(bytes);
        dugite_primitives::hash::blake2b_224(&buf).0
    }

    // ────────────────────────────────────────────────────────────
    // resolve_spend
    // ────────────────────────────────────────────────────────────

    #[test]
    fn spend_resolves_witness_set_v3_script_with_inline_datum() {
        let script_bytes = vec![0xaa, 0xbb, 0xcc];
        let script_hash = plutus_v3_script_with_hash(&script_bytes);
        let input = TransactionInput {
            transaction_id: h32(0xee),
            index: 0,
        };
        let spent_out = enterprise_script_output(script_hash, 1_000_000);
        let mut body = minimal_body();
        body.inputs = vec![input.clone()];
        let mut ws = empty_witness();
        ws.plutus_v3_scripts = vec![script_bytes.clone()];
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let resolved = vec![(input, spent_out, vec![])];
        let r = resolve_redeemers(&tx, &resolved).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].language, ScriptLanguage::PlutusV3);
        assert_eq!(r[0].script_bytes, script_bytes);
        // V3 spend: datum lookup defers to ScriptContext builder.
        assert!(r[0].datum.is_none());
        assert!(matches!(r[0].purpose, ScriptPurpose::Spending(_)));
    }

    #[test]
    fn spend_v2_returns_datum_from_witness_set() {
        let script_bytes = vec![0x01, 0x02];
        // V2 script hash = blake2b_224(0x02 || bytes).
        let mut buf = vec![2u8];
        buf.extend_from_slice(&script_bytes);
        let script_hash = dugite_primitives::hash::blake2b_224(&buf).0;

        // Datum: integer 7. Compute its blake2b_256 via our Data encoder.
        let datum = PrimPlutusData::Integer(num_bigint::BigInt::from(7));
        let datum_translated = crate::tx_info_populate::plutus_data_to_data(&datum);
        let datum_hash = dugite_primitives::hash::blake2b_256(&datum_translated.to_cbor().unwrap());

        let input = TransactionInput {
            transaction_id: h32(0xee),
            index: 0,
        };
        let mut spent_out = enterprise_script_output(script_hash, 1);
        spent_out.datum = PrimOutputDatum::DatumHash(datum_hash);
        let mut body = minimal_body();
        body.inputs = vec![input.clone()];
        let mut ws = empty_witness();
        ws.plutus_v2_scripts = vec![script_bytes];
        ws.plutus_data = vec![datum.clone()];
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let resolved = vec![(input, spent_out, vec![])];
        let r = resolve_redeemers(&tx, &resolved).unwrap();
        assert_eq!(r[0].language, ScriptLanguage::PlutusV2);
        assert_eq!(r[0].datum, Some(datum));
    }

    #[test]
    fn spend_v2_with_missing_datum_errors() {
        // Same as above but witness_set.plutus_data is empty.
        let script_bytes = vec![0x01, 0x02];
        let mut buf = vec![2u8];
        buf.extend_from_slice(&script_bytes);
        let script_hash = dugite_primitives::hash::blake2b_224(&buf).0;

        let input = TransactionInput {
            transaction_id: h32(0xee),
            index: 0,
        };
        let mut spent_out = enterprise_script_output(script_hash, 1);
        spent_out.datum = PrimOutputDatum::DatumHash(h32(0xff));
        let mut body = minimal_body();
        body.inputs = vec![input.clone()];
        let mut ws = empty_witness();
        ws.plutus_v2_scripts = vec![script_bytes];
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let resolved = vec![(input, spent_out, vec![])];
        let err = resolve_redeemers(&tx, &resolved).unwrap_err();
        assert!(matches!(err, PhaseTwoError::MissingDatum { .. }));
    }

    #[test]
    fn spend_with_key_locked_output_errors() {
        let input = TransactionInput {
            transaction_id: h32(0xee),
            index: 0,
        };
        let spent_out = TransactionOutput {
            address: PrimAddress::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: PrimCred::VerificationKey(h28(0x55)),
            }),
            value: Value::lovelace(1),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let mut body = minimal_body();
        body.inputs = vec![input.clone()];
        let mut ws = empty_witness();
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let resolved = vec![(input, spent_out, vec![])];
        let err = resolve_redeemers(&tx, &resolved).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    #[test]
    fn spend_with_out_of_range_index_errors() {
        let mut ws = empty_witness();
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Spend,
            index: 99,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(minimal_body(), ws);
        let err = resolve_redeemers(&tx, &[]).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// Regression test for the Alonzo mainnet Spend redeemer sort-order bug.
    ///
    /// cardano-ledger's `alonzoRedeemerPointer` resolves a `Spending idx`
    /// redeemer into `Set.elemAt idx (txBody ^. inputsTxBodyL)` — the i-th
    /// element of the inputs treated as a **sorted `Set TxIn`** (ascending by
    /// TxId raw bytes then TxIx). The on-wire CBOR array order must NOT be
    /// used for redeemer index resolution.
    ///
    /// Ground-truth vector (mainnet tx
    /// 4a3b78c246f30425754966396d10ffcba0b9cc8b97c6d3f9f54d8c6d30154422):
    ///  - input_a: tx_id 0xfb…#0  (script-locked), wire-position 0, sorted-position 1
    ///  - input_b: tx_id 0xf9…#1  (key-locked),    wire-position 1, sorted-position 0
    ///
    /// Redeemer Spend index=1 must resolve to input_a (sorted[1]) — the
    /// script-locked output. Under the old wire-order code it resolved to
    /// input_b (wire[1]) — the key-locked output — producing "spent output's
    /// address is not script-locked".
    ///
    /// This test deliberately sets tx_id of the script-locked input to 0xfb
    /// and the key-locked input to 0xf9, so 0xf9 < 0xfb and the wire order
    /// (script first) is opposite to the sorted order (key first). The test
    /// would FAIL with the old `tx.body.inputs.get(idx)` wire-order code.
    #[test]
    fn spend_redeemer_index_uses_sorted_input_set_not_wire_order() {
        // Script bytes for V3 (simplest hash surface).
        let script_bytes: Vec<u8> = vec![0xde, 0xad, 0xbe, 0xef];
        let script_hash = {
            let mut buf = vec![3u8];
            buf.extend_from_slice(&script_bytes);
            dugite_primitives::hash::blake2b_224(&buf).0
        };

        // input_a: tx_id 0xfb…#0 — script-locked (this is the valid spend target).
        // In sorted Ord order this is SECOND (0xfb > 0xf9).
        let input_a = TransactionInput {
            transaction_id: Hash::<32>([0xfb; 32]),
            index: 0,
        };
        let spent_out_a = enterprise_script_output(script_hash, 5_000_000);

        // input_b: tx_id 0xf9…#1 — key-locked (NOT a valid spend target for Plutus).
        // In sorted Ord order this is FIRST (0xf9 < 0xfb).
        let input_b = TransactionInput {
            transaction_id: Hash::<32>([0xf9; 32]),
            index: 1,
        };
        let spent_out_b = TransactionOutput {
            address: PrimAddress::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: PrimCred::VerificationKey(h28(0x55)),
            }),
            value: Value::lovelace(2_000_000),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };

        // Wire order: [input_a(0xfb#0), input_b(0xf9#1)]
        // Sorted order: [input_b(0xf9#1), input_a(0xfb#0)]   ← 0xf9 < 0xfb
        // Redeemer Spend index=1 → sorted[1] = input_a (script-locked). Correct.
        // Old code: wire[1] = input_b (key-locked) → "not script-locked" error. Wrong.
        let mut body = minimal_body();
        body.inputs = vec![input_a.clone(), input_b.clone()];
        let mut ws = empty_witness();
        ws.plutus_v3_scripts = vec![script_bytes];
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Spend,
            index: 1, // sorted[1] = input_a (0xfb); wire[1] = input_b (0xf9)
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let resolved = vec![
            (input_a.clone(), spent_out_a, vec![]),
            (input_b.clone(), spent_out_b, vec![]),
        ];

        let r = resolve_redeemers(&tx, &resolved)
            .expect("spend redeemer must resolve to the script-locked sorted[1] input");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].script_hash, script_hash);

        // The ScriptPurpose must carry the outref of the SORTED input_a (0xfb#0),
        // not the wire-order input_b (0xf9#1).
        match &r[0].purpose {
            ScriptPurpose::Spending(outref) => {
                assert_eq!(
                    outref.tx_id, [0xfbu8; 32],
                    "Spending purpose must reference sorted[1]=input_a (0xfb…), \
                     not wire[1]=input_b (0xf9…) — wire-order bug"
                );
                assert_eq!(outref.idx, 0);
            }
            other => panic!("expected ScriptPurpose::Spending, got {other:?}"),
        }
    }

    // ────────────────────────────────────────────────────────────
    // resolve_mint
    // ────────────────────────────────────────────────────────────

    #[test]
    fn mint_resolves_to_first_policy_in_btreemap_order() {
        let script_bytes = vec![0xff];
        let policy_hash = plutus_v3_script_with_hash(&script_bytes);
        let mut mint = BTreeMap::new();
        mint.insert(Hash::<28>(policy_hash), BTreeMap::new());
        let mut body = minimal_body();
        body.mint = mint;
        let mut ws = empty_witness();
        ws.plutus_v3_scripts = vec![script_bytes];
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Mint,
            index: 0,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let r = resolve_redeemers(&tx, &[]).unwrap();
        assert_eq!(r[0].script_hash, policy_hash);
        assert!(matches!(r[0].purpose, ScriptPurpose::Minting(h) if h == policy_hash));
    }

    #[test]
    fn mint_out_of_range_errors() {
        let mut ws = empty_witness();
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Mint,
            index: 5,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(minimal_body(), ws);
        let err = resolve_redeemers(&tx, &[]).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    // ────────────────────────────────────────────────────────────
    // Vote / Propose redeemers
    // ────────────────────────────────────────────────────────────

    /// Vote redeemer with out-of-range index should return Internal error.
    #[test]
    fn vote_redeemer_out_of_range_errors() {
        let mut ws = empty_witness();
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Vote,
            index: 0, // tx has no voting_procedures
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(minimal_body(), ws);
        let err = resolve_redeemers(&tx, &[]).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// Vote redeemer pointing to a key-credentialed voter (not a Plutus script)
    /// should return Internal error.
    #[test]
    fn vote_redeemer_with_key_voter_errors() {
        use dugite_primitives::transaction::{GovActionId, Vote, Voter, VotingProcedure};
        let mut body = minimal_body();
        // Add a voting procedure keyed by a key-credentialed DRep.
        let mut inner = std::collections::BTreeMap::new();
        inner.insert(
            GovActionId {
                transaction_id: h32(0xab),
                action_index: 0,
            },
            VotingProcedure {
                vote: Vote::Yes,
                anchor: None,
            },
        );
        body.voting_procedures
            .insert(Voter::DRep(PrimCred::VerificationKey(h28(0xcc))), inner);
        let mut ws = empty_witness();
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Vote,
            index: 0,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let err = resolve_redeemers(&tx, &[]).unwrap_err();
        // Key voter cannot dispatch Plutus → Internal
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// Vote redeemer with a script-credentialed DRep voter resolves correctly.
    #[test]
    fn vote_redeemer_with_script_drep_voter_resolves() {
        use dugite_primitives::transaction::{GovActionId, Vote, Voter, VotingProcedure};
        let script_bytes = vec![0xde, 0xad];
        let script_hash = {
            let mut buf = vec![3u8];
            buf.extend_from_slice(&script_bytes);
            dugite_primitives::hash::blake2b_224(&buf).0
        };
        let mut body = minimal_body();
        let mut inner = std::collections::BTreeMap::new();
        inner.insert(
            GovActionId {
                transaction_id: h32(0x10),
                action_index: 0,
            },
            VotingProcedure {
                vote: Vote::Yes,
                anchor: None,
            },
        );
        body.voting_procedures
            .insert(Voter::DRep(PrimCred::Script(h28(0))), inner.clone());
        // Second voter with the script credential
        let mut inner2 = std::collections::BTreeMap::new();
        inner2.insert(
            GovActionId {
                transaction_id: h32(0x20),
                action_index: 0,
            },
            VotingProcedure {
                vote: Vote::No,
                anchor: None,
            },
        );
        body.voting_procedures.insert(
            Voter::DRep(PrimCred::Script(Hash::<28>(script_hash))),
            inner2,
        );
        let mut ws = empty_witness();
        ws.plutus_v3_scripts = vec![script_bytes.clone()];
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Vote,
            index: 1, // BTreeMap iteration order: Script([0;28]) < Script([script_hash])
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let r = resolve_redeemers(&tx, &[]).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].script_hash, script_hash);
        assert!(matches!(r[0].purpose, ScriptPurpose::Voting(_)));
    }

    /// Propose redeemer with out-of-range index errors.
    #[test]
    fn propose_redeemer_out_of_range_errors() {
        let mut ws = empty_witness();
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Propose,
            index: 0,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(minimal_body(), ws);
        let err = resolve_redeemers(&tx, &[]).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    /// Propose redeemer on InfoAction (no policy_hash) errors.
    #[test]
    fn propose_redeemer_on_info_action_errors() {
        use dugite_primitives::transaction::{Anchor, GovAction, ProposalProcedure};
        use dugite_primitives::value::Lovelace;
        let mut body = minimal_body();
        // reward address: mainnet key-stake 0xe0 || [0x42; 28]
        let mut return_addr = vec![0xe0u8];
        return_addr.extend_from_slice(&[0x42u8; 28]);
        body.proposal_procedures.push(ProposalProcedure {
            deposit: Lovelace(0),
            return_addr,
            gov_action: GovAction::InfoAction,
            anchor: Anchor {
                url: String::new(),
                data_hash: h32(0),
            },
        });
        let mut ws = empty_witness();
        ws.redeemers = vec![Redeemer {
            tag: RedeemerTag::Propose,
            index: 0,
            data: PrimPlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits { mem: 1, steps: 1 },
        }];
        let tx = build_tx(body, ws);
        let err = resolve_redeemers(&tx, &[]).unwrap_err();
        // InfoAction has no policy_hash → Internal
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }
}
