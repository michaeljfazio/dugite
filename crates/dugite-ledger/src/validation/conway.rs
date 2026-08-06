//! Conway-era specific validation: era gating, governance checks, and
//! certificate deposit/refund accounting.
//!
//! This module handles:
//! - Ensuring Conway-only certificates and governance actions are rejected on
//!   pre-Conway protocol versions (Rule 1d).
//! - Calculating the net deposit and refund amounts for all certificate types
//!   across eras, including pool re-registration logic.

use std::collections::HashSet;

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::{
    Certificate, GovAction, ProposalProcedure, TransactionBody, Voter,
};
use dugite_primitives::value::Lovelace;

use super::{ValidationContext, ValidationError};

/// Return the human-readable certificate type name when the certificate is
/// Conway-only (requires protocol version >= 9). Returns `None` for
/// pre-Conway certificates that are valid in all post-Shelley eras.
pub(super) fn conway_only_certificate_name(cert: &Certificate) -> Option<&'static str> {
    match cert {
        Certificate::RegDRep { .. } => Some("RegDRep"),
        Certificate::UnregDRep { .. } => Some("UnregDRep"),
        Certificate::UpdateDRep { .. } => Some("UpdateDRep"),
        Certificate::VoteDelegation { .. } => Some("VoteDelegation"),
        Certificate::StakeVoteDelegation { .. } => Some("StakeVoteDelegation"),
        Certificate::CommitteeHotAuth { .. } => Some("CommitteeHotAuth"),
        Certificate::CommitteeColdResign { .. } => Some("CommitteeColdResign"),
        Certificate::RegStakeVoteDeleg { .. } => Some("RegStakeVoteDeleg"),
        Certificate::VoteRegDeleg { .. } => Some("VoteRegDeleg"),
        Certificate::ConwayStakeRegistration { .. } => Some("ConwayStakeRegistration"),
        Certificate::ConwayStakeDeregistration { .. } => Some("ConwayStakeDeregistration"),
        Certificate::RegStakeDeleg { .. } => Some("RegStakeDeleg"),
        // Pre-Conway certificates — valid in all post-Shelley eras
        Certificate::StakeRegistration(_)
        | Certificate::StakeDeregistration(_)
        | Certificate::StakeDelegation { .. }
        | Certificate::PoolRegistration(_)
        | Certificate::PoolRetirement { .. }
        | Certificate::GenesisKeyDelegation { .. }
        | Certificate::MoveInstantaneousRewards { .. } => None,
    }
}

/// Validate era-gating rules (Rule 1d).
///
/// Conway-specific certificates and governance features are only valid when the
/// current protocol major version is >= 9 (Conway era). Violations are pushed
/// onto `errors`.
pub(super) fn check_era_gating(
    params: &ProtocolParameters,
    body: &dugite_primitives::transaction::TransactionBody,
    errors: &mut Vec<ValidationError>,
) {
    let proto_major = params.protocol_version_major;
    let current_era_name = if proto_major >= 9 {
        "Conway"
    } else if proto_major >= 7 {
        "Babbage"
    } else if proto_major >= 6 {
        "Alonzo"
    } else if proto_major >= 4 {
        "Mary"
    } else {
        "Shelley"
    };

    if proto_major < 9 {
        for cert in &body.certificates {
            if let Some(cert_name) = conway_only_certificate_name(cert) {
                errors.push(ValidationError::EraGatingViolation {
                    certificate_type: cert_name.to_string(),
                    required_era: "Conway (protocol >= 9)".to_string(),
                    current_era: format!("{} (protocol {})", current_era_name, proto_major),
                });
            }
        }
        if !body.voting_procedures.is_empty() {
            errors.push(ValidationError::GovernancePreConway {
                current_version: proto_major,
            });
        }
        if !body.proposal_procedures.is_empty() {
            errors.push(ValidationError::GovernancePreConway {
                current_version: proto_major,
            });
        }
    }
}

/// Calculate total deposits and refunds from certificates in a transaction.
///
/// Deposits are charged for:
/// - Stake registration (pre-Conway: `key_deposit`, Conway: inline deposit amount)
/// - Pool registration (new pools only; re-registrations are free)
/// - DRep registration
/// - Combined registration+delegation certificates (RegStakeDeleg, RegStakeVoteDeleg,
///   VoteRegDeleg)
///
/// Refunds are returned for:
/// - Stake deregistration
/// - DRep unregistration
///
/// When `registered_pools` is `Some`, pool re-registrations (updating an existing
/// pool's parameters) do not charge an additional deposit — only new pool
/// registrations do. When `None`, all pool registrations are treated as new.
///
/// When `stake_key_deposits` is `Some`, pre-Conway `StakeDeregistration` refund
/// amounts are looked up from the per-credential deposit map (the deposit paid
/// at registration time). When `None`, the current `key_deposit` parameter is
/// used as a fallback.
pub(super) fn calculate_deposits_and_refunds(
    certificates: &[Certificate],
    params: &ProtocolParameters,
    registered_pools: Option<&HashSet<Hash28>>,
    stake_key_deposits: Option<&imbl::HashMap<Hash32, u64>>,
) -> (u64, u64) {
    let mut deposits = 0u64;
    let mut refunds = 0u64;
    // Track pools newly registered within this transaction so that a second
    // PoolRegistration cert for the same pool in the same tx is treated as an
    // update (no additional deposit).
    let mut newly_registered: HashSet<Hash28> = HashSet::new();

    for cert in certificates {
        match cert {
            Certificate::StakeRegistration(_) => {
                deposits += params.key_deposit.0;
            }
            Certificate::StakeDeregistration(credential) => {
                // Use the stored per-credential deposit for correct refund when
                // key_deposit changes via governance. Falls back to current param
                // when deposit map is unavailable or credential not found.
                let key = credential.to_typed_hash32();
                refunds += stake_key_deposits
                    .and_then(|m| m.get(&key).copied())
                    .unwrap_or(params.key_deposit.0);
            }
            Certificate::ConwayStakeRegistration { deposit, .. } => {
                deposits += deposit.0;
            }
            Certificate::ConwayStakeDeregistration { refund, .. } => {
                refunds += refund.0;
            }
            Certificate::PoolRegistration(pool_params) => {
                // Only charge deposit for NEW pool registrations.
                // Re-registration (update) of an already-registered pool is free.
                let already_registered = registered_pools
                    .is_some_and(|pools| pools.contains(&pool_params.operator))
                    || newly_registered.contains(&pool_params.operator);
                if !already_registered {
                    deposits += params.pool_deposit.0;
                    newly_registered.insert(pool_params.operator);
                }
            }
            Certificate::RegDRep { deposit, .. } => {
                deposits += deposit.0;
            }
            Certificate::UnregDRep { refund, .. } => {
                refunds += refund.0;
            }
            Certificate::RegStakeDeleg { deposit, .. } => {
                deposits += deposit.0;
            }
            Certificate::RegStakeVoteDeleg { deposit, .. } => {
                deposits += deposit.0;
            }
            Certificate::VoteRegDeleg { deposit, .. } => {
                deposits += deposit.0;
            }
            _ => {}
        }
    }

    (deposits, refunds)
}

/// Check ppuWellFormed for ParameterChange governance proposals.
///
/// Haskell's `ppuWellFormed` (Conway/PParams.hs) rejects proposals with
/// zero values in specific fields. Only applies to ParameterChange actions.
pub(super) fn check_pparam_update_well_formed(
    params: &ProtocolParameters,
    body: &TransactionBody,
    errors: &mut Vec<ValidationError>,
) {
    if params.protocol_version_major < 9 {
        return;
    }
    for (proposal_index, proposal) in body.proposal_procedures.iter().enumerate() {
        if let GovAction::ParameterChange {
            protocol_param_update,
            ..
        } = &proposal.gov_action
        {
            let ppu = protocol_param_update;
            let mut reasons = Vec::new();

            if ppu.max_block_body_size == Some(0) {
                reasons.push("maxBBSize=0");
            }
            if ppu.max_tx_size == Some(0) {
                reasons.push("maxTxSize=0");
            }
            if ppu.max_block_header_size == Some(0) {
                reasons.push("maxBHSize=0");
            }
            if ppu.max_val_size == Some(0) {
                reasons.push("maxValSize=0");
            }
            if ppu.collateral_percentage == Some(0) {
                reasons.push("collateralPercentage=0");
            }
            if ppu.committee_term_limit == Some(0) {
                reasons.push("committeeMaxTermLength=0");
            }
            if ppu.gov_action_lifetime == Some(0) {
                reasons.push("govActionLifetime=0");
            }
            if matches!(ppu.pool_deposit, Some(Lovelace(0))) {
                reasons.push("poolDeposit=0");
            }
            if matches!(ppu.gov_action_deposit, Some(Lovelace(0))) {
                reasons.push("govActionDeposit=0");
            }
            if matches!(ppu.drep_deposit, Some(Lovelace(0))) {
                reasons.push("dRepDeposit=0");
            }
            // coinsPerUTxOByte zero check — only enforced post-bootstrap (PV >= 10)
            if params.protocol_version_major >= 10
                && matches!(ppu.ada_per_utxo_byte, Some(Lovelace(0)))
            {
                reasons.push("coinsPerUTxOByte=0");
            }
            // nOpt zero check — PV >= 11
            if params.protocol_version_major >= 11 && ppu.n_opt == Some(0) {
                reasons.push("nOpt=0");
            }

            // Strict CostModels structural validation — PV >= 11.
            //
            // Mirrors Haskell `validateCostModelsParamsUpdate` (cardano-ledger
            // 10.7 / cardano-ledger validate PR #755): under PV >= 11 every present
            // language's cost vector must have at least the language's
            // minimum parameter count. Pre-PV11 behaviour is unchanged so
            // historical mainnet/preview blocks replay cleanly.
            //
            // Per-language minimums (per Plutus core builtinCostModel.json
            // and `Plutus.Core.CostModelInterface`):
            //   PlutusV1: 166
            //   PlutusV2: 175
            //   PlutusV3: 251  (mainnet PV11 currently ships 297 — the
            //                  strict rule is a *minimum*, not equality, so
            //                  future builtin extensions remain accepted.)
            //
            // Plutus cost-model entries are encoded as `i64` in our
            // primitives, so the "all entries must be Int" Haskell check is
            // intrinsic to the type system here; only length is enforced.
            if params.protocol_version_major >= 11 {
                if let Some(ref cm) = ppu.cost_models {
                    const V1_MIN: usize = 166;
                    const V2_MIN: usize = 175;
                    const V3_MIN: usize = 251;
                    if let Some(v1) = &cm.plutus_v1 {
                        if v1.len() < V1_MIN {
                            reasons.push("cost_models[PlutusV1] too short");
                        }
                    }
                    if let Some(v2) = &cm.plutus_v2 {
                        if v2.len() < V2_MIN {
                            reasons.push("cost_models[PlutusV2] too short");
                        }
                    }
                    if let Some(v3) = &cm.plutus_v3 {
                        if v3.len() < V3_MIN {
                            reasons.push("cost_models[PlutusV3] too short");
                        }
                    }
                }
            }

            // Empty update check — all Option fields are None
            let is_empty = ppu.min_fee_a.is_none()
                && ppu.min_fee_b.is_none()
                && ppu.max_block_body_size.is_none()
                && ppu.max_tx_size.is_none()
                && ppu.max_block_header_size.is_none()
                && ppu.key_deposit.is_none()
                && ppu.pool_deposit.is_none()
                && ppu.e_max.is_none()
                && ppu.n_opt.is_none()
                && ppu.a0.is_none()
                && ppu.rho.is_none()
                && ppu.tau.is_none()
                && ppu.min_pool_cost.is_none()
                && ppu.ada_per_utxo_byte.is_none()
                && ppu.cost_models.is_none()
                && ppu.execution_costs.is_none()
                && ppu.max_tx_ex_units.is_none()
                && ppu.max_block_ex_units.is_none()
                && ppu.max_val_size.is_none()
                && ppu.collateral_percentage.is_none()
                && ppu.max_collateral_inputs.is_none()
                && ppu.min_fee_ref_script_cost_per_byte.is_none()
                && ppu.d.is_none()
                && ppu.protocol_version_major.is_none()
                && ppu.protocol_version_minor.is_none()
                && ppu.drep_deposit.is_none()
                && ppu.gov_action_deposit.is_none()
                && ppu.gov_action_lifetime.is_none()
                && ppu.dvt_pp_network_group.is_none()
                && ppu.dvt_pp_economic_group.is_none()
                && ppu.dvt_pp_technical_group.is_none()
                && ppu.dvt_pp_gov_group.is_none()
                && ppu.dvt_hard_fork.is_none()
                && ppu.dvt_no_confidence.is_none()
                && ppu.dvt_committee_normal.is_none()
                && ppu.dvt_committee_no_confidence.is_none()
                && ppu.dvt_constitution.is_none()
                && ppu.dvt_treasury_withdrawal.is_none()
                && ppu.pvt_motion_no_confidence.is_none()
                && ppu.pvt_committee_normal.is_none()
                && ppu.pvt_committee_no_confidence.is_none()
                && ppu.pvt_hard_fork.is_none()
                && ppu.pvt_pp_security_group.is_none()
                && ppu.min_committee_size.is_none()
                && ppu.committee_term_limit.is_none()
                && ppu.drep_activity.is_none();
            if is_empty {
                reasons.push("empty PParamsUpdate");
            }

            if !reasons.is_empty() {
                errors.push(ValidationError::MalformedProposal {
                    reason: reasons.join(", "),
                    proposal_index,
                });
            }
        }
    }
}

/// Returns `true` when a (voter, action) combination is disallowed by the
/// Conway voter × gov-action authority matrix.
///
/// | GovAction           | StakePool | Committee | DRep |
/// |---------------------|-----------|-----------|------|
/// | `NoConfidence`      | yes       | **NO**    | yes  |
/// | `UpdateCommittee`   | yes       | **NO**    | yes  |
/// | `NewConstitution`   | **NO**    | yes       | yes  |
/// | `HardForkInitiation`| yes       | yes       | yes  |
/// | `ParameterChange`   | yes\*     | yes       | yes  |
/// | `TreasuryWithdrawals`| **NO**   | yes       | yes  |
/// | `InfoAction`        | yes       | yes       | yes  |
///
/// \* SPO authorisation on `ParameterChange` is enforced at this Phase-1 level;
/// the per-group threshold (`NoVotingAllowed` for non–security-group changes)
/// is checked later at ratification time.
///
/// This is a pure predicate. The caller (in `validation/mod.rs`) accumulates
/// every disallowed `(voter, gov_action_id)` pair across the transaction and
/// emits a single [`ValidationError::DisallowedVoters`] holding the full list,
/// matching Haskell's `NonEmpty` predicate-failure shape.
///
/// Reference: Haskell `checkVotersAreValid` /
/// `is{Committee,DRep,StakePool}VotingAllowed` in
/// `Cardano.Ledger.Conway.Governance.Internal`.
/// Returns `true` when the (voter, action) pair is rejected by the Conway
/// bootstrap-phase voting restrictions.
///
/// Per Haskell `checkBootstrapVotes` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs` (lines 378-391
/// and `isBootstrapAction` at lines 633-639):
///
/// ```haskell
/// checkBootstrapVotes pp votes
///   | hardforkConwayBootstrapPhase (pp ^. ppProtocolVersionL) =
///       checkDisallowedVotes votes DisallowedVotesDuringBootstrap $ \gas ->
///         \case
///           DRepVoter {} | gasAction gas == InfoAction -> True
///           DRepVoter {} -> False
///           _ -> isBootstrapAction $ gasAction gas
///   | otherwise = pure ()
///
/// isBootstrapAction =
///   \case
///     ParameterChange {} -> True
///     HardForkInitiation {} -> True
///     InfoAction -> True
///     _ -> False
/// ```
///
/// `hardforkConwayBootstrapPhase pv = pvMajor pv < 10` — fires at PV9 only.
/// Callers gate on `protocol_version_major == 9` before invoking.
///
/// The action-type classification is shared with the proposal-side restriction
/// via [`is_bootstrap_action`] — Haskell gates both on the identical
/// `isBootstrapAction` predicate, so they must not be able to drift.
pub(super) fn is_bootstrap_vote_disallowed(voter: &Voter, action: &GovAction) -> bool {
    let is_bootstrap_action = is_bootstrap_action(action);
    match voter {
        // DRepVoter: only InfoAction is allowed during bootstrap.
        Voter::DRep(_) => !matches!(action, GovAction::InfoAction),
        // Committee + StakePool voters: only bootstrap-class actions
        // (ParameterChange / HardForkInitiation / InfoAction).
        Voter::ConstitutionalCommittee(_) | Voter::StakePool(_) => !is_bootstrap_action,
    }
}

/// Haskell `isBootstrapAction` — the governance-action types permitted during
/// the Conway bootstrap phase (PV9).
///
/// ```haskell
/// isBootstrapAction =
///   \case
///     ParameterChange {} -> True
///     HardForkInitiation {} -> True
///     InfoAction -> True
///     _ -> False
/// ```
///
/// Source: `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`
/// (lines 633-639), pinned at `4849c13d6f70e5ab46add9af6e0ec5c537b61f69`.
///
/// Haskell gates BOTH bootstrap restrictions on this one predicate — votes via
/// `checkBootstrapVotes` and proposal submission via `checkBootstrapProposal` —
/// so dugite shares it rather than keeping two matching `matches!` arms that
/// could drift (#1026).
pub(super) fn is_bootstrap_action(action: &GovAction) -> bool {
    matches!(
        action,
        GovAction::ParameterChange { .. }
            | GovAction::HardForkInitiation { .. }
            | GovAction::InfoAction
    )
}

/// Returns `true` when a proposal may NOT be SUBMITTED during the Conway
/// bootstrap phase.
///
/// Per Haskell `checkBootstrapProposal`, step 1 of the per-proposal fold in
/// `conwayGovTransition`'s `processProposal`:
///
/// ```haskell
/// checkBootstrapProposal pp proposal
///   | hardforkConwayBootstrapPhase (pp ^. ppProtocolVersionL) =
///       failureUnless (isBootstrapAction (pProcGovAction proposal)) $
///         DisallowedProposalDuringBootstrap proposal
///   | otherwise = pure ()
/// ```
///
/// So at PV9 only `ParameterChange` / `HardForkInitiation` / `InfoAction` may be
/// proposed; `NoConfidence`, `UpdateCommittee`, `NewConstitution` and
/// `TreasuryWithdrawals` are rejected with `ConwayGovPredFailure` tag 12.
///
/// **Ordering is load-bearing**: `runTest $ checkBootstrapProposal pp proposal`
/// is the FIRST test in `processProposal`, ahead of the `ProposalCantFollow`
/// hard-fork check and `actionWellFormed`. Callers must run it before those, so
/// a bootstrap-disallowed proposal reports tag 12 and not some later failure.
///
/// Callers gate on `protocol_version_major == 9` before invoking
/// (`hardforkConwayBootstrapPhase pv = pvMajor pv == natVersion @9`).
pub(super) fn is_bootstrap_proposal_disallowed(action: &GovAction) -> bool {
    !is_bootstrap_action(action)
}

pub(super) fn is_voter_disallowed(voter: &Voter, action: &GovAction) -> bool {
    match (voter, action) {
        // InfoAction: every voter type is allowed (NoVotingThreshold).
        (_, GovAction::InfoAction) => false,
        // SPO is forbidden on NewConstitution and TreasuryWithdrawals.
        (Voter::StakePool(_), GovAction::NewConstitution { .. }) => true,
        (Voter::StakePool(_), GovAction::TreasuryWithdrawals { .. }) => true,
        // SPO on ParameterChange: allowed only when the embedded PParamsUpdate
        // touches at least one SecurityGroup-relevant field. Haskell's
        // `votingStakePoolThresholdInternal` (Conway/Governance/Internal.hs
        // L388-L393) returns `NoVotingAllowed` for ParameterChange updates
        // that touch no SecurityGroup field, which surfaces as
        // `DisallowedVoters` from the GOV rule.
        //
        // SecurityGroup field set (cardano-ledger Conway/PParams.hs L643-L709):
        //   keys 0 (minFeeA), 1 (minFeeB), 2 (maxBBSize), 3 (maxTxSize),
        //   4 (maxBHSize), 17 (coinsPerUTxOByte), 21 (maxBlockExUnits),
        //   22 (maxValSize), 30 (govActionDeposit),
        //   33 (minFeeRefScriptCostPerByte).
        (
            Voter::StakePool(_),
            GovAction::ParameterChange {
                protocol_param_update,
                ..
            },
        ) => !ppu_is_security_group_relevant(protocol_param_update),
        // Constitutional Committee is forbidden on NoConfidence and UpdateCommittee.
        (Voter::ConstitutionalCommittee(_), GovAction::NoConfidence { .. }) => true,
        (Voter::ConstitutionalCommittee(_), GovAction::UpdateCommittee { .. }) => true,
        // All other (voter, action) combinations are permitted at this layer.
        _ => false,
    }
}

/// Returns `true` when at least one field in the given `ProtocolParamUpdate`
/// belongs to the Conway SecurityGroup (`PPGroups _ SecurityGroup` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs` L643-L709). Used
/// by [`is_voter_disallowed`] to decide whether SPOs may vote on a given
/// `ParameterChange` action.
///
/// The check mirrors Haskell's `any isSecurityRelevant (modifiedPPGroups ppu)`
/// — a single `Some(_)` (i.e. `SJust`) in any SecurityGroup field is enough.
pub(super) fn ppu_is_security_group_relevant(
    ppu: &dugite_primitives::transaction::ProtocolParamUpdate,
) -> bool {
    ppu.min_fee_a.is_some()                          // key 0
        || ppu.min_fee_b.is_some()                   // key 1
        || ppu.max_block_body_size.is_some()         // key 2
        || ppu.max_tx_size.is_some()                 // key 3
        || ppu.max_block_header_size.is_some()       // key 4
        || ppu.ada_per_utxo_byte.is_some()           // key 17 (coinsPerUTxOByte)
        || ppu.max_block_ex_units.is_some()          // key 21
        || ppu.max_val_size.is_some()                // key 22
        || ppu.gov_action_deposit.is_some()          // key 30
        || ppu.min_fee_ref_script_cost_per_byte.is_some() // key 33
}

/// Returns `true` when the given voter is unknown to the ledger, i.e. its
/// credential / pool ID is not present in the corresponding registry:
///
///   - `DRepVoter`        — credential not in `registered_dreps`.
///   - `StakePoolVoter`   — pool ID not in `registered_pools`.
///   - `CommitteeVoter`   — hot credential not in
///     `committee_authorized_hot_keys`.
///
/// When the relevant context field is `None` (not provided by the caller),
/// this returns `false` (voter treated as known) — matching the lenient
/// default used elsewhere (see [`ValidationContext::active_proposals`] for
/// the same convention).  This avoids false-positive rejections when the
/// caller hasn't yet plumbed in the relevant ledger state.
///
/// This is a pure predicate.  The caller (in `validation/mod.rs`) accumulates
/// every unknown voter across the transaction's `voting_procedures` and emits
/// a single [`ValidationError::VotersDoNotExist`] holding the full list,
/// matching Haskell's `NonEmpty` predicate-failure shape.
///
/// Reference: Haskell `internVoter` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`.
pub(super) fn is_voter_unknown(voter: &Voter, ctx: &ValidationContext) -> bool {
    match voter {
        Voter::DRep(credential) => match ctx.registered_dreps.as_ref() {
            Some(dreps) => {
                // Use `to_typed_hash32` so byte 28 carries the credential kind
                // (0x00 key, 0x01 script). The producer side
                // (`state::credential_to_hash`) keys the DRep map the same way,
                // matching Haskell's `Map (Credential 'DRepRole) DRepState`.
                let key = credential.to_typed_hash32();
                !dreps.contains(&key)
            }
            None => false,
        },
        Voter::StakePool(pool_hash32) => match ctx.registered_pools.as_ref() {
            Some(pools) => {
                // Voter::StakePool wraps a Hash32 produced by zero-padding the
                // 28-byte pool key hash (see decoder in
                // dugite-serialization::multi_era).  registered_pools stores
                // the canonical 28-byte form, so truncate here.
                let mut bytes28 = [0u8; 28];
                bytes28.copy_from_slice(&pool_hash32.as_bytes()[..28]);
                let pool_id = Hash28::from_bytes(bytes28);
                !pools.contains(&pool_id)
            }
            None => false,
        },
        Voter::ConstitutionalCommittee(hot_credential) => {
            match ctx.committee_authorized_hot_keys.as_ref() {
                Some(hot_keys) => {
                    // Same key/script disambiguation as DRep above —
                    // `Credential 'HotCommitteeRole` carries the kind tag.
                    let key = hot_credential.to_typed_hash32();
                    !hot_keys.contains(&key)
                }
                None => false,
            }
        }
    }
}

/// Returns `true` when a vote against the governance action identified by
/// `gov_action_id` is rejected because the action has already expired.
///
/// Per Haskell `checkVotesAreNotForExpiredActions` in
/// `Cardano.Ledger.Conway.Rules.Gov`, a vote is allowed when
/// `current_epoch <= gasExpiresAfter` (boundary inclusive); it is rejected
/// only when `current_epoch > gasExpiresAfter`.
///
/// The action's expiry epoch is looked up from
/// `ctx.active_proposals[gov_action_id].expires_after_epoch`.
///
/// This predicate returns `false` (vote allowed) when:
///   - `ctx.active_proposals` is `None` — lenient default, mirroring the
///     same convention used by [`is_voter_disallowed`] when the caller has
///     not plumbed in proposal state.
///   - `ctx.current_epoch` is `None` — without a current epoch we can't
///     compare, so we accept the vote.
///   - `gov_action_id` is not present in the active-proposal map — that's a
///     different predicate failure (`GovActionsDoNotExist`), handled
///     elsewhere; this rule must not double-fire on it.
///
/// The caller (in `validation/mod.rs`) aggregates every expired
/// `(voter, gov_action_id)` pair into a single
/// [`ValidationError::VotingOnExpiredGovAction`], matching Haskell's
/// `NonEmpty` predicate-failure shape.
///
/// Reference: Haskell `checkVotesAreNotForExpiredActions` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`.
pub(super) fn is_vote_on_expired_action(
    gov_action_id: &dugite_primitives::transaction::GovActionId,
    ctx: &ValidationContext,
) -> bool {
    let Some(active) = ctx.active_proposals.as_ref() else {
        return false;
    };
    let Some(current_epoch) = ctx.current_epoch else {
        return false;
    };
    let Some(proposal) = active.get(gov_action_id) else {
        return false;
    };
    // Boundary inclusive: rejected only when current_epoch > expires_after.
    current_epoch > proposal.expires_after_epoch.0
}

/// Returns `Some((target_major, target_minor, base_major, base_minor))` when
/// `action` is a `HardForkInitiation` whose target `ProtVer` does NOT
/// `pvCanFollow` its resolved base version — i.e. the proposal must be
/// rejected with `ProposalCantFollow`. Returns `None` for any non-HardFork
/// action, or a `HardForkInitiation` whose base is unresolved or whose
/// target legally follows.
///
/// Implements Haskell `preceedingHardFork`
/// (`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:673-694`)'s
/// three-way base resolution followed by
/// [`crate::state::governance::pv_can_follow`] (the same formula reused —
/// NOT reimplemented — from the live block-apply GOV rule so the
/// reachability arithmetic has one source of truth):
///
///  1. `hf_prev == ctx.enacted_gov_roots.hard_fork` (the proposal's prev
///     pointer matches the currently-ENACTED HardFork root), OR the
///     proposed major version already jumps more than one step past the
///     CURRENT on-chain major — base = the live `(cur_major, cur_minor)`.
///     The second disjunct is a deliberate short-circuit: it forbids
///     compounding two major-version bumps within one live proposal set,
///     even when a same-purpose ancestor is in flight.
///  2. Otherwise, `hf_prev` must resolve to another `HardForkInitiation`,
///     either an EARLIER proposal in this same transaction (Haskell folds
///     `processProposal` over the tx's proposals in order, so proposal N
///     may chain onto proposal N-1's OWN target version) or an on-chain
///     active (in-flight) proposal — base = that sibling's target
///     `ProtVer`.
///  3. Base unresolved (`hf_prev` missing, or does not resolve to a
///     `HardForkInitiation`) — no `ProposalCantFollow` here; that
///     malformed-ancestor shape is instead caught by the separate
///     structural [`ValidationError::InvalidPrevGovActionId`] check.
///
/// Silently skipped when `ctx.enacted_gov_roots` is `None` (the same
/// lenient default used by the sibling `InvalidPrevGovActionId` check).
///
/// Reference: Haskell `ProposalCantFollow` (`ConwayGovPredFailure` tag 10,
/// `Conway/Rules/Gov.hs:193-199`), raised from `badHardFork` inside
/// `processProposal` (`Conway/Rules/Gov.hs:483-499`). Without this check, a
/// `HardForkInitiation` proposal with an illegal version jump is admitted
/// to dugite's mempool and forged into a block; cardano-node rejects the
/// WHOLE TRANSACTION (and therefore the block) via `ConwayGovFailure
/// (ProposalCantFollow …)` — the `#996`-class wedge, where a Haskell peer
/// re-requests the same block on every reconnect and never recovers.
pub(super) fn hardfork_proposal_cant_follow(
    action: &GovAction,
    idx: usize,
    proposals: &[ProposalProcedure],
    tx_hash: Hash32,
    ctx: &ValidationContext,
    cur_major: u64,
    cur_minor: u64,
) -> Option<(u64, u64, u64, u64)> {
    let GovAction::HardForkInitiation {
        prev_action_id: hf_prev,
        protocol_version: (tgt_major, tgt_minor),
    } = action
    else {
        return None;
    };

    let roots = ctx.enacted_gov_roots.as_ref()?;

    let base: Option<(u64, u64)> =
        if hf_prev.as_ref() == roots.hard_fork.as_ref() || *tgt_major > cur_major + 1 {
            Some((cur_major, cur_minor))
        } else {
            hf_prev.as_ref().and_then(|prev| {
                let sibling_action: Option<&GovAction> =
                    if prev.transaction_id == tx_hash && (prev.action_index as usize) < idx {
                        proposals
                            .get(prev.action_index as usize)
                            .map(|p| &p.gov_action)
                    } else {
                        ctx.active_proposals
                            .as_ref()
                            .and_then(|m| m.get(prev))
                            .map(|ap| &ap.gov_action)
                    };
                match sibling_action {
                    Some(GovAction::HardForkInitiation {
                        protocol_version, ..
                    }) => Some(*protocol_version),
                    _ => None,
                }
            })
        };

    match base {
        Some((bm, bn))
            if !crate::state::governance::pv_can_follow(bm, bn, *tgt_major, *tgt_minor) =>
        {
            Some((*tgt_major, *tgt_minor, bm, bn))
        }
        _ => None,
    }
}

/// Returns `true` when the proposal's `return_addr` references a stake
/// credential that is not registered in `ctx.reward_accounts`.
///
/// Per Haskell `processProposal` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`, every
/// proposal procedure's `pProcReturnAddr` must point to a registered stake
/// credential (so the proposal deposit can be refunded at expiry/enactment).
///
/// The check is **skipped during Conway bootstrap** (`pvMajor == 9`) per
/// `hardforkConwayBootstrapPhase`, returning `false` regardless of the
/// reward-accounts contents.  It activates from PV ≥ 10 onwards.
///
/// This predicate returns `false` (proposal accepted) when:
///   - `params.protocol_version_major == 9` — bootstrap skip.
///   - `ctx.reward_accounts` is `None` — lenient default, mirroring the
///     same convention used by [`is_voter_unknown`] and
///     [`is_vote_on_expired_action`] when the caller has not plumbed in
///     the relevant ledger state.
///   - `proposal.return_addr` is shorter than 29 bytes — malformed
///     reward addresses are caught by a different predicate
///     (`ProposalProcedureNetworkIdMismatch` / decoder); this rule must
///     not double-fire on shape errors.
///
/// The lookup uses [`crate::state::LedgerState::reward_account_to_hash`]
/// to mirror the canonical key derivation used by the withdrawal-amount
/// check earlier in `validate_transaction_with_pools` (and by the on-chain
/// reward-accounts map populated in `state/certificates.rs`).  Byte 28 of
/// the resulting `Hash32` carries the credential type (`0x00` = key,
/// `0x01` = script), so key and script credentials with the same 28-byte
/// hash are distinguished — matching Haskell's
/// `KeyHashObj` / `ScriptHashObj`.
///
/// The caller (in `validation/mod.rs`) aggregates every offending proposal's
/// raw `return_addr` (hex-encoded) into a single
/// [`ValidationError::ProposalReturnAccountDoesNotExist`], matching
/// Haskell's `NonEmpty` predicate-failure shape.
///
/// Reference: Haskell `ProposalReturnAccountDoesNotExist` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
/// inside `processProposal`.
pub(super) fn is_proposal_return_account_unregistered(
    proposal: &ProposalProcedure,
    params: &ProtocolParameters,
    ctx: &ValidationContext,
) -> bool {
    // Bootstrap skip — pv == 9 disables the check entirely.
    if params.protocol_version_major == 9 {
        return false;
    }
    // Lenient default when reward_accounts state is not plumbed in.
    let Some(accounts) = ctx.reward_accounts.as_ref() else {
        return false;
    };
    // Malformed reward addresses (< 29 bytes) are not this predicate's
    // concern; treat as accepted here.
    if proposal.return_addr.len() < 29 {
        return false;
    }
    let key = crate::state::LedgerState::reward_account_to_hash(&proposal.return_addr);
    !accounts.contains_key(&key)
}

/// Returns `Some(actual_network)` when the proposal's `return_addr` network
/// id does not match `ctx.node_network`, otherwise `None`.
///
/// Per Haskell `processProposal` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`, every
/// proposal procedure's `pProcReturnAddr` must be on the same network as
/// the node.  Bit 0 of the reward-account header byte encodes the network
/// (`0` = testnet, `1` = mainnet) — the same encoding used by
/// `WrongNetworkWithdrawal` (see `phase1.rs`).
///
/// Unlike [`is_proposal_return_account_unregistered`] this predicate is
/// **always enforced** (there is no Conway-bootstrap skip): the network id
/// is a structural property of the proposal payload, not a post-bootstrap
/// state lookup.
///
/// The predicate returns `None` (proposal accepted) when:
///   - `ctx.node_network` is `None` — lenient default, mirroring the
///     convention used by the other GOV predicates when the caller has
///     not plumbed in the relevant context.
///   - `proposal.return_addr` is empty — malformed reward addresses are
///     caught by a different predicate (decoder); this rule must not
///     double-fire on shape errors.
///   - The header network bit matches `ctx.node_network`.
///
/// The shape (`Option<u8>` rather than `bool`) is intentional — the
/// `ProposalProcedureNetworkIdMismatch` error payload aggregates the
/// **actual** mismatched network values, so the caller needs to surface
/// them.
///
/// Reference: Haskell `ProposalProcedureNetworkIdMismatch` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
/// inside `processProposal`.  Always enforced (no bootstrap skip).
pub(super) fn is_proposal_return_addr_wrong_network(
    proposal: &ProposalProcedure,
    ctx: &ValidationContext,
) -> Option<u8> {
    // Lenient default — skip when the node network isn't plumbed in.
    let expected_net = ctx.node_network?;
    // Malformed reward addresses are not this predicate's concern.
    let header = *proposal.return_addr.first()?;
    // Bit 0 of the header byte: 0 = testnet, 1 = mainnet (matches the
    // encoding used by `WrongNetworkWithdrawal` in phase1.rs).
    let actual = header & 0x01;
    if actual != expected_net.to_u8() {
        Some(actual)
    } else {
        None
    }
}

/// Returns the list of `(hex_addr, actual_network_id)` mismatches between a
/// `TreasuryWithdrawals` proposal's destination addresses and the node's
/// configured network.
///
/// Per Haskell `processProposal` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`, when the
/// proposal action is `TreasuryWithdrawals` every key in the withdrawals
/// map is a reward address whose network id must match the node's network.
/// Bit 0 of the reward-account header byte encodes the network
/// (`0` = testnet, `1` = mainnet) — same encoding as
/// [`is_proposal_return_addr_wrong_network`] and `WrongNetworkWithdrawal`
/// in `phase1.rs`.
///
/// Like the proposal-procedure network check, this predicate is **always
/// enforced** — there is no Conway-bootstrap skip; the network id is a
/// structural property of the proposal payload, not a post-bootstrap
/// state lookup.
///
/// The predicate returns an empty `Vec` (proposal accepted) when:
///   - `proposal.gov_action` is not [`GovAction::TreasuryWithdrawals`] —
///     this rule applies only to TW proposals, mirroring Haskell's
///     branch-specific check.
///   - `ctx.node_network` is `None` — lenient default, mirroring the
///     convention used by the other GOV predicates when the caller has
///     not plumbed in the relevant context.
///   - All entries' header network bits match `ctx.node_network`.
///
/// The shape (`Vec<(String, u8)>`) is intentional — every mismatched entry
/// in a single TreasuryWithdrawals proposal is surfaced, and the caller in
/// `validation/mod.rs` flattens these across all proposals into a single
/// [`ValidationError::TreasuryWithdrawalsNetworkIdMismatch`], mirroring
/// Haskell's `NonEmpty` predicate-failure shape.
///
/// Reference: Haskell `TreasuryWithdrawalsNetworkIdMismatch` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
/// inside `processProposal`'s `TreasuryWithdrawals` branch.  Always
/// enforced (no bootstrap skip).
pub(super) fn treasury_withdrawal_network_mismatches(
    proposal: &ProposalProcedure,
    ctx: &ValidationContext,
) -> Vec<(String, u8)> {
    // Only TreasuryWithdrawals proposals are subject to this rule.
    let GovAction::TreasuryWithdrawals { withdrawals, .. } = &proposal.gov_action else {
        return Vec::new();
    };
    // Lenient default — skip when the node network isn't plumbed in.
    let Some(expected_net) = ctx.node_network else {
        return Vec::new();
    };
    let expected = expected_net.to_u8();
    let mut out: Vec<(String, u8)> = Vec::new();
    for addr in withdrawals.keys() {
        // Empty address → skip (decoder-shape error, not this predicate's
        // concern; mirrors `is_proposal_return_addr_wrong_network`).
        let Some(&header) = addr.first() else {
            continue;
        };
        let actual = header & 0x01;
        if actual != expected {
            let addr_hex = addr
                .iter()
                .fold(String::with_capacity(addr.len() * 2), |mut s, b| {
                    use std::fmt::Write;
                    let _ = write!(s, "{b:02x}");
                    s
                });
            out.push((addr_hex, actual));
        }
    }
    out
}

/// Returns `true` when the proposal is a `TreasuryWithdrawals` action whose
/// total amount is exactly zero — including the all-zero-entries case and
/// the empty-map case.
///
/// Per Haskell `processProposal` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`
/// (`TreasuryWithdrawals` branch), the sum of every entry's `Coin` must be
/// strictly positive.  This guards against degenerate proposals that lock
/// up a deposit without actually moving any treasury value.
///
/// The check is **skipped during Conway bootstrap** (`pvMajor == 9`) per
/// `hardforkConwayBootstrapPhase`, returning `false` regardless of the
/// withdrawals contents.  It activates from PV ≥ 10 onwards.
///
/// The predicate returns `false` (proposal accepted) when:
///   - `params.protocol_version_major == 9` — bootstrap skip.
///   - `proposal.gov_action` is not [`GovAction::TreasuryWithdrawals`] —
///     this rule applies only to TW proposals, mirroring Haskell's
///     branch-specific check.
///   - The sum of withdrawal `Coin` values is non-zero.
///
/// The caller (in `validation/mod.rs`) collects every offending TW
/// proposal's descriptor (or hex of its `return_addr`) into a single
/// [`ValidationError::ZeroTreasuryWithdrawals`], mirroring Haskell's
/// `NonEmpty` predicate-failure shape.
///
/// Reference: Haskell `ZeroTreasuryWithdrawals` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
/// inside `processProposal`'s `TreasuryWithdrawals` branch.  Skipped
/// during Conway bootstrap (PV == 9).
pub(super) fn is_treasury_withdrawals_zero_sum(
    proposal: &ProposalProcedure,
    params: &ProtocolParameters,
) -> bool {
    // Bootstrap skip — pv == 9 disables the check entirely.
    if params.protocol_version_major == 9 {
        return false;
    }
    let GovAction::TreasuryWithdrawals { withdrawals, .. } = &proposal.gov_action else {
        return false;
    };
    // Saturating sum: even an absurdly long all-u64::MAX list cannot wrap
    // around to zero — `0` is reachable only when every entry is `0` (or
    // the map is empty).
    let total: u128 = withdrawals.values().map(|c| c.0 as u128).sum();
    total == 0
}

/// Returns the list of hex-encoded credential hashes that appear both in
/// `members_to_add` (as a key) and in `members_to_remove` for an
/// `UpdateCommittee` proposal — i.e. credentials the proposal both adds
/// and removes in the same action.
///
/// Per Haskell `processProposal` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`
/// (`UpdateCommittee` branch), the intersection of the add-set keys and
/// the remove-set must be empty:
///
/// ```haskell
/// let conflicting = Set.intersection (Map.keysSet membersToAdd) membersToRemove
/// in unless (Set.null conflicting) (failBecause $ ConflictingCommitteeUpdate conflicting)
/// ```
///
/// This check is **always enforced** (no Conway-bootstrap skip): the
/// add/remove conflict is a structural property of the action payload,
/// not a post-bootstrap state lookup.
///
/// The predicate returns an empty `Vec` (proposal accepted) when:
///   - `proposal.gov_action` is not [`GovAction::UpdateCommittee`] —
///     this rule applies only to UpdateCommittee proposals, mirroring
///     Haskell's branch-specific check.
///   - The intersection of add-set keys and remove-set is empty.
///
/// The credential identity used for the intersection mirrors Haskell's
/// `Credential 'ColdCommitteeRole`: a key-credential and a script-credential
/// with the same 28-byte hash are *distinct* members.  We use
/// [`Credential::to_typed_hash32`] (byte 28 = `0x01` for scripts, `0x00`
/// for keys) so the intersection respects this distinction — exactly the
/// same convention used by the other GOV credential-keyed sets in
/// `ValidationContext`.
///
/// The shape (`Vec<String>`) is intentional — the
/// `ConflictingCommitteeUpdate` error payload aggregates every conflicting
/// credential across all `UpdateCommittee` proposals in the transaction
/// into a single failure, mirroring Haskell's `NonEmpty` predicate-failure
/// shape.
///
/// Reference: Haskell `ConflictingCommitteeUpdate` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
/// inside `processProposal`'s `UpdateCommittee` branch.  Always enforced
/// (no bootstrap skip).
pub(super) fn committee_update_conflicts(proposal: &ProposalProcedure) -> Vec<String> {
    let GovAction::UpdateCommittee {
        members_to_remove,
        members_to_add,
        ..
    } = &proposal.gov_action
    else {
        return Vec::new();
    };
    // Build the remove set keyed by the typed-hash32 representation so the
    // intersection respects the key-vs-script credential distinction.
    let remove_keys: HashSet<Hash32> = members_to_remove
        .iter()
        .map(|c| c.to_typed_hash32())
        .collect();
    let mut conflicts: Vec<String> = Vec::new();
    for cred in members_to_add.keys() {
        let key = cred.to_typed_hash32();
        if remove_keys.contains(&key) {
            conflicts.push(key.to_hex());
        }
    }
    conflicts
}

/// Returns the list of `(typed_hash_hex, expiry_epoch)` pairs for new
/// committee members in an `UpdateCommittee` proposal whose `validUntil`
/// epoch is **not strictly greater than** the current epoch — i.e. the
/// member would expire on or before they enter office.
///
/// Per Haskell `processProposal` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`
/// (`UpdateCommittee` branch):
///
/// ```haskell
/// let invalidMembers = Map.filter (<= currentEpoch) membersToAdd
/// in unless (Map.null invalidMembers) (failBecause $ ExpirationEpochTooSmall invalidMembers)
/// ```
///
/// This check is **always enforced** — there is no Conway-bootstrap skip;
/// the expiry-vs-current-epoch comparison is a structural property of the
/// proposal payload combined with the live epoch.  When `current_epoch`
/// is `None` the predicate is silently lenient (returns the empty vec) so
/// callers that have not plumbed in epoch context don't get spurious
/// failures.
///
/// The credential identity uses [`Credential::to_typed_hash32`] (byte 28
/// = `0x01` for scripts, `0x00` for keys), matching the convention used
/// by [`committee_update_conflicts`] and by the credential-keyed
/// `ValidationContext` sets — so callers can distinguish key- from
/// script-credential entries.
///
/// The shape (`Vec<(String, u64)>`) is intentional — every offending
/// member across all `UpdateCommittee` proposals in the transaction is
/// surfaced, and the caller in `validation/mod.rs` aggregates these
/// into a single [`ValidationError::ExpirationEpochTooSmall`], mirroring
/// Haskell's `NonEmpty` predicate-failure shape.
///
/// Reference: Haskell `ExpirationEpochTooSmall` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
/// inside `processProposal`'s `UpdateCommittee` branch.  Always enforced
/// (no bootstrap skip).
pub(super) fn committee_update_invalid_expiries(
    proposal: &ProposalProcedure,
    ctx: &ValidationContext,
) -> Vec<(String, u64)> {
    let GovAction::UpdateCommittee { members_to_add, .. } = &proposal.gov_action else {
        return Vec::new();
    };
    // Lenient default — skip when current_epoch isn't plumbed in.
    let Some(current_epoch) = ctx.current_epoch else {
        return Vec::new();
    };
    let mut invalid: Vec<(String, u64)> = Vec::new();
    for (cred, expiry) in members_to_add.iter() {
        if *expiry <= current_epoch {
            invalid.push((cred.to_typed_hash32().to_hex(), *expiry));
        }
    }
    invalid
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap, HashSet};

    use dugite_primitives::credentials::Credential;
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::EpochNo;
    use dugite_primitives::transaction::{
        Anchor, Certificate, DRep, GovAction, GovActionId, PoolParams, ProposalProcedure, Rational,
        TransactionBody, Voter, VotingProcedure,
    };
    use dugite_primitives::value::Lovelace;

    use super::*;
    use crate::validation::{ActiveProposal, EnactedGovRoots};

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    /// Build a minimal `TransactionBody` with the given certificates,
    /// voting procedures, and proposal procedures. All other fields are left
    /// empty/default so tests stay focused on what they actually care about.
    fn make_body(
        certificates: Vec<Certificate>,
        voting_procedures: BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>>,
    ) -> TransactionBody {
        make_body_full(certificates, voting_procedures, vec![])
    }

    /// Like `make_body` but also accepts a `proposal_procedures` list.
    fn make_body_full(
        certificates: Vec<Certificate>,
        voting_procedures: BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>>,
        proposal_procedures: Vec<ProposalProcedure>,
    ) -> TransactionBody {
        TransactionBody {
            inputs: vec![],
            outputs: vec![],
            fee: Lovelace(0),
            ttl: None,
            certificates,
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
            voting_procedures,
            proposal_procedures,
            treasury_value: None,
            donation: None,
            sub_transactions: vec![],
            account_balance_intervals: vec![],
            direct_deposits: ::std::collections::BTreeMap::new(),
            guards: Vec::new(),
        }
    }

    /// A `Credential::VerificationKey` backed by a deterministic 28-byte hash.
    fn test_credential(byte: u8) -> Credential {
        Credential::VerificationKey(Hash28::from_bytes([byte; 28]))
    }

    /// A `PoolParams` stub with the given operator hash. Only `operator` is used
    /// by `calculate_deposits_and_refunds`; the other fields carry no-op values.
    fn make_pool_params(operator_byte: u8) -> PoolParams {
        PoolParams {
            operator: Hash28::from_bytes([operator_byte; 28]),
            vrf_keyhash: Hash32::from_bytes([0u8; 32]),
            pledge: Lovelace(0),
            cost: Lovelace(0),
            margin: Rational {
                numerator: 0,
                denominator: 1,
            },
            reward_account: vec![],
            pool_owners: vec![],
            relays: vec![],
            pool_metadata: None,
        }
    }

    // ---------------------------------------------------------------------------
    // check_era_gating — all 12 Conway-only cert types accepted at PV=9
    // ---------------------------------------------------------------------------

    #[test]
    fn test_conway_cert_in_conway_era() {
        // Protocol version 9 = Conway era; every Conway-only certificate must be
        // accepted without producing an EraGatingViolation.
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;

        // All 12 Conway-only certificate variants (tags 7-18 in the spec).
        let all_conway_certs: Vec<Certificate> = vec![
            Certificate::ConwayStakeRegistration {
                credential: test_credential(0x01),
                deposit: Lovelace(2_000_000),
            },
            Certificate::ConwayStakeDeregistration {
                credential: test_credential(0x02),
                refund: Lovelace(2_000_000),
            },
            Certificate::RegDRep {
                credential: test_credential(0x03),
                deposit: Lovelace(500_000_000),
                anchor: None,
            },
            Certificate::UnregDRep {
                credential: test_credential(0x04),
                refund: Lovelace(500_000_000),
            },
            Certificate::UpdateDRep {
                credential: test_credential(0x05),
                anchor: None,
            },
            Certificate::VoteDelegation {
                credential: test_credential(0x06),
                drep: DRep::Abstain,
            },
            Certificate::StakeVoteDelegation {
                credential: test_credential(0x07),
                pool_hash: Hash28::from_bytes([0x07u8; 28]),
                drep: DRep::NoConfidence,
            },
            Certificate::CommitteeHotAuth {
                cold_credential: test_credential(0x08),
                hot_credential: test_credential(0x09),
            },
            Certificate::CommitteeColdResign {
                cold_credential: test_credential(0x0A),
                anchor: None,
            },
            Certificate::RegStakeVoteDeleg {
                credential: test_credential(0x0B),
                pool_hash: Hash28::from_bytes([0x0Bu8; 28]),
                drep: DRep::Abstain,
                deposit: Lovelace(2_000_000),
            },
            Certificate::VoteRegDeleg {
                credential: test_credential(0x0C),
                drep: DRep::Abstain,
                deposit: Lovelace(2_000_000),
            },
            Certificate::RegStakeDeleg {
                credential: test_credential(0x0D),
                pool_hash: Hash28::from_bytes([0x0Du8; 28]),
                deposit: Lovelace(2_000_000),
            },
        ];

        // Each cert is tested individually so a failure message names the variant.
        for cert in all_conway_certs {
            // conway_only_certificate_name() tells us what name the production
            // code would use in an error; use it to label assertions.
            let cert_name = conway_only_certificate_name(&cert)
                .expect("all certs in this list must be Conway-only");

            let body = make_body(vec![cert], BTreeMap::new());
            let mut errors: Vec<ValidationError> = vec![];
            check_era_gating(&params, &body, &mut errors);

            let violations: Vec<_> = errors
                .iter()
                .filter(|e| matches!(e, ValidationError::EraGatingViolation { .. }))
                .collect();
            assert!(
                violations.is_empty(),
                "Conway cert '{cert_name}' must be accepted in Conway era (pv=9), got: {violations:?}"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // check_era_gating — Conway cert in Babbage era (error expected)
    // ---------------------------------------------------------------------------

    #[test]
    fn test_conway_cert_in_pre_conway_era() {
        // Protocol version 8 = Babbage era; Conway certs must be rejected.
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8;

        let cert = Certificate::RegDRep {
            credential: test_credential(0xBB),
            deposit: Lovelace(500_000_000),
            anchor: None,
        };
        let body = make_body(vec![cert], BTreeMap::new());

        let mut errors: Vec<ValidationError> = vec![];
        check_era_gating(&params, &body, &mut errors);

        let has_violation = errors
            .iter()
            .any(|e| matches!(e, ValidationError::EraGatingViolation { .. }));
        assert!(
            has_violation,
            "Expected EraGatingViolation for Conway cert in Babbage (pv=8)"
        );
    }

    // ---------------------------------------------------------------------------
    // check_era_gating — voting_procedures and proposal_procedures in pre-Conway
    // era (both branches must each produce a GovernancePreConway error)
    // ---------------------------------------------------------------------------

    #[test]
    fn test_governance_features_era_gated() {
        // Protocol version 8 = Babbage; voting procedures must be rejected.
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8;

        // --- Sub-test 1: non-empty voting_procedures -------------------------
        let gov_action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0x01u8; 32]),
            action_index: 0,
        };
        let voting_procedure = VotingProcedure {
            vote: dugite_primitives::transaction::Vote::Yes,
            anchor: None,
        };
        let voter = Voter::DRep(test_credential(0xCC));
        let mut inner = BTreeMap::new();
        inner.insert(gov_action_id, voting_procedure);
        let mut voting_procedures = BTreeMap::new();
        voting_procedures.insert(voter, inner);

        let body_voting = make_body(vec![], voting_procedures);

        let mut errors: Vec<ValidationError> = vec![];
        check_era_gating(&params, &body_voting, &mut errors);

        let has_gov_error = errors
            .iter()
            .any(|e| matches!(e, ValidationError::GovernancePreConway { .. }));
        assert!(
            has_gov_error,
            "Expected GovernancePreConway for voting_procedures in Babbage (pv=8)"
        );

        // --- Sub-test 2: non-empty proposal_procedures -----------------------
        // The production code has a separate `if !body.proposal_procedures.is_empty()`
        // branch; verify it is also exercised.
        let proposal = ProposalProcedure {
            deposit: Lovelace(1_000_000_000),
            return_addr: vec![0xE1u8], // minimal reward address stub
            gov_action: GovAction::InfoAction,
            anchor: Anchor {
                url: "https://example.com".to_string(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        };

        let body_proposal = make_body_full(vec![], BTreeMap::new(), vec![proposal]);

        let mut errors2: Vec<ValidationError> = vec![];
        check_era_gating(&params, &body_proposal, &mut errors2);

        let has_gov_error2 = errors2
            .iter()
            .any(|e| matches!(e, ValidationError::GovernancePreConway { .. }));
        assert!(
            has_gov_error2,
            "Expected GovernancePreConway for proposal_procedures in Babbage (pv=8)"
        );
    }

    // ---------------------------------------------------------------------------
    // calculate_deposits_and_refunds — StakeRegistration charges key_deposit
    // ---------------------------------------------------------------------------

    #[test]
    fn test_deposit_new_key_registration() {
        let params = ProtocolParameters::mainnet_defaults(); // key_deposit = 2_000_000
        let cert = Certificate::StakeRegistration(test_credential(0x01));

        let (deposits, refunds) = calculate_deposits_and_refunds(&[cert], &params, None, None);

        assert_eq!(
            deposits, params.key_deposit.0,
            "StakeRegistration should charge key_deposit"
        );
        assert_eq!(refunds, 0, "StakeRegistration should produce no refund");
    }

    // ---------------------------------------------------------------------------
    // calculate_deposits_and_refunds — RegDRep charges the inline cert deposit
    // ---------------------------------------------------------------------------

    #[test]
    fn test_deposit_new_drep_registration() {
        let params = ProtocolParameters::mainnet_defaults(); // drep_deposit = 500_000_000

        // Use a deposit amount distinct from params.drep_deposit.0 to prove
        // that the implementation reads the inline cert field rather than
        // falling back to the protocol parameter.
        let inline_deposit: u64 = 750_000_000;
        assert_ne!(
            inline_deposit, params.drep_deposit.0,
            "test setup: inline_deposit must differ from params.drep_deposit for this test to be meaningful"
        );

        let cert = Certificate::RegDRep {
            credential: test_credential(0x02),
            deposit: Lovelace(inline_deposit),
            anchor: None,
        };

        let (deposits, refunds) = calculate_deposits_and_refunds(&[cert], &params, None, None);

        assert_eq!(
            deposits, inline_deposit,
            "RegDRep should charge the inline cert deposit ({inline_deposit}), \
             not params.drep_deposit ({})",
            params.drep_deposit.0
        );
        assert_eq!(refunds, 0, "RegDRep should produce no refund");
    }

    // ---------------------------------------------------------------------------
    // calculate_deposits_and_refunds — PoolRegistration re-registration is free
    // ---------------------------------------------------------------------------

    #[test]
    fn test_deposit_pool_reregistration_free() {
        let params = ProtocolParameters::mainnet_defaults(); // pool_deposit = 500_000_000
        let pool_params = make_pool_params(0x03);
        let operator = pool_params.operator;

        // Pool is already in the registered set.
        let mut registered_pools: HashSet<Hash28> = HashSet::new();
        registered_pools.insert(operator);

        let cert = Certificate::PoolRegistration(pool_params);

        let (deposits, refunds) =
            calculate_deposits_and_refunds(&[cert], &params, Some(&registered_pools), None);

        assert_eq!(
            deposits, 0,
            "Re-registration of an existing pool should charge 0 deposit"
        );
        assert_eq!(refunds, 0);
    }

    // ---------------------------------------------------------------------------
    // calculate_deposits_and_refunds — StakeDeregistration refunds key_deposit
    // ---------------------------------------------------------------------------

    #[test]
    fn test_refund_deregistration() {
        let params = ProtocolParameters::mainnet_defaults(); // key_deposit = 2_000_000
        let credential = test_credential(0x04);
        let cert = Certificate::StakeDeregistration(credential.clone());

        // No deposit map provided — should fall back to current key_deposit.
        let (deposits, refunds) = calculate_deposits_and_refunds(&[cert], &params, None, None);

        assert_eq!(deposits, 0, "StakeDeregistration should produce no deposit");
        assert_eq!(
            refunds, params.key_deposit.0,
            "StakeDeregistration should refund key_deposit when deposit map is absent"
        );
    }

    // ---------------------------------------------------------------------------
    // calculate_deposits_and_refunds — UnregDRep refunds inline cert amount
    // ---------------------------------------------------------------------------

    /// The spec says "Key/DRep deregistration refunds [use] the stored
    /// per-credential deposit amount."  For `UnregDRep` the refund amount is
    /// carried inline in the certificate itself (not looked up from a map), so
    /// the implementation must use `refund.0` directly.
    #[test]
    fn test_refund_unreg_drep() {
        let params = ProtocolParameters::mainnet_defaults(); // drep_deposit = 500_000_000

        // Use a refund amount distinct from params.drep_deposit.0 to prove
        // the code reads the cert field, not the current protocol parameter.
        let original_deposit: u64 = 300_000_000;
        let cert = Certificate::UnregDRep {
            credential: test_credential(0x10),
            refund: Lovelace(original_deposit),
        };

        let (deposits, refunds) = calculate_deposits_and_refunds(&[cert], &params, None, None);

        assert_eq!(deposits, 0, "UnregDRep should produce no deposit");
        assert_eq!(
            refunds, original_deposit,
            "UnregDRep refund must equal the inline cert amount ({original_deposit}), \
             not the current drep_deposit ({})",
            params.drep_deposit.0
        );
    }

    // ---------------------------------------------------------------------------
    // calculate_deposits_and_refunds — ConwayStakeDeregistration uses inline amount
    // ---------------------------------------------------------------------------

    #[test]
    fn test_per_credential_deposit_map() {
        // current key_deposit = 2_000_000 ADA; original deposit was 1_500_000
        // (simulates a governance-changed key_deposit after original registration).
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let stored_deposit: u64 = 1_500_000;

        let credential = test_credential(0x05);

        // ConwayStakeDeregistration carries the inline refund amount agreed at
        // registration time.
        let cert = Certificate::ConwayStakeDeregistration {
            credential: credential.clone(),
            refund: Lovelace(stored_deposit),
        };

        // The deposit map is not consulted for ConwayStakeDeregistration because
        // the refund amount is encoded inline in the certificate itself.
        let mut deposit_map: imbl::HashMap<Hash32, u64> = imbl::HashMap::new();
        deposit_map.insert(credential.to_typed_hash32(), stored_deposit);

        let (deposits, refunds) =
            calculate_deposits_and_refunds(&[cert], &params, None, Some(&deposit_map));

        assert_eq!(deposits, 0);
        assert_eq!(
            refunds, stored_deposit,
            "ConwayStakeDeregistration refund must use the inline cert amount, \
             not the current key_deposit ({}) or deposit map",
            params.key_deposit.0
        );
    }

    // ---------------------------------------------------------------------------
    // check_pparam_update_well_formed — zero maxTxSize rejected
    // ---------------------------------------------------------------------------

    #[test]
    fn test_pparam_update_zero_max_tx_size_rejected() {
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        let body = make_body_full(
            vec![],
            BTreeMap::new(),
            vec![ProposalProcedure {
                deposit: Lovelace(0),
                return_addr: vec![0xe0; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(
                        dugite_primitives::transaction::ProtocolParamUpdate {
                            max_tx_size: Some(0),
                            ..Default::default()
                        },
                    ),
                    policy_hash: None,
                },
                anchor: Anchor {
                    url: String::new(),
                    data_hash: Hash32::from_bytes([0u8; 32]),
                },
            }],
        );
        let mut errors = Vec::new();
        check_pparam_update_well_formed(&params, &body, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(&errors[0], ValidationError::MalformedProposal { reason, .. } if reason.contains("maxTxSize=0")),
            "Expected MalformedProposal with maxTxSize=0, got: {:?}",
            errors
        );
    }

    // ---------------------------------------------------------------------------
    // check_pparam_update_well_formed — valid nonzero field accepted
    // ---------------------------------------------------------------------------

    #[test]
    fn test_pparam_update_valid_nonzero_accepted() {
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        let body = make_body_full(
            vec![],
            BTreeMap::new(),
            vec![ProposalProcedure {
                deposit: Lovelace(0),
                return_addr: vec![0xe0; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(
                        dugite_primitives::transaction::ProtocolParamUpdate {
                            max_tx_size: Some(16384),
                            ..Default::default()
                        },
                    ),
                    policy_hash: None,
                },
                anchor: Anchor {
                    url: String::new(),
                    data_hash: Hash32::from_bytes([0u8; 32]),
                },
            }],
        );
        let mut errors = Vec::new();
        check_pparam_update_well_formed(&params, &body, &mut errors);
        assert!(
            errors.is_empty(),
            "Valid nonzero max_tx_size should not trigger MalformedProposal, got: {:?}",
            errors
        );
    }

    // ---------------------------------------------------------------------------
    // check_pparam_update_well_formed — empty PParamsUpdate rejected
    // ---------------------------------------------------------------------------

    #[test]
    fn test_pparam_update_empty_rejected() {
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        let body = make_body_full(
            vec![],
            BTreeMap::new(),
            vec![ProposalProcedure {
                deposit: Lovelace(0),
                return_addr: vec![0xe0; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::default(),
                    policy_hash: None,
                },
                anchor: Anchor {
                    url: String::new(),
                    data_hash: Hash32::from_bytes([0u8; 32]),
                },
            }],
        );
        let mut errors = Vec::new();
        check_pparam_update_well_formed(&params, &body, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(&errors[0], ValidationError::MalformedProposal { reason, .. } if reason.contains("empty")),
            "Expected MalformedProposal with 'empty', got: {:?}",
            errors
        );
    }

    // ---------------------------------------------------------------------------
    // check_pparam_update_well_formed — non-ParameterChange proposal skipped
    // ---------------------------------------------------------------------------

    #[test]
    fn test_pparam_update_non_parameter_change_skipped() {
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        let body = make_body_full(
            vec![],
            BTreeMap::new(),
            vec![ProposalProcedure {
                deposit: Lovelace(0),
                return_addr: vec![0xe0; 29],
                gov_action: GovAction::InfoAction,
                anchor: Anchor {
                    url: String::new(),
                    data_hash: Hash32::from_bytes([0u8; 32]),
                },
            }],
        );
        let mut errors = Vec::new();
        check_pparam_update_well_formed(&params, &body, &mut errors);
        assert!(
            errors.is_empty(),
            "Non-ParameterChange proposals should not trigger ppuWellFormed check, got: {:?}",
            errors
        );
    }

    // ---------------------------------------------------------------------------
    // check_pparam_update_well_formed — strict CostModels validation (PV >= 11,
    // issue #464 / Haskell `validateCostModelsParamsUpdate`).
    // ---------------------------------------------------------------------------

    fn cost_models_proposal_body(
        cm: dugite_primitives::transaction::CostModels,
    ) -> TransactionBody {
        make_body_full(
            vec![],
            BTreeMap::new(),
            vec![ProposalProcedure {
                deposit: Lovelace(0),
                return_addr: vec![0xe0; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(
                        dugite_primitives::transaction::ProtocolParamUpdate {
                            cost_models: Some(cm),
                            ..Default::default()
                        },
                    ),
                    policy_hash: None,
                },
                anchor: Anchor {
                    url: String::new(),
                    data_hash: Hash32::from_bytes([0u8; 32]),
                },
            }],
        )
    }

    #[test]
    fn test_cost_models_pv10_accepts_loose_update() {
        // Pre-PV11: strict structural validation must NOT apply — a tiny
        // 5-entry V2 vector is benign at PV10 to preserve historical-block
        // replay compatibility.
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        let body = cost_models_proposal_body(dugite_primitives::transaction::CostModels {
            plutus_v1: None,
            plutus_v2: Some(vec![100; 5]),
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        });
        let mut errors = Vec::new();
        check_pparam_update_well_formed(&params, &body, &mut errors);
        assert!(
            errors.is_empty(),
            "PV10 must accept loose cost_models update, got: {errors:?}",
        );
    }

    #[test]
    fn test_cost_models_pv11_rejects_short_v1() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 11;
        let body = cost_models_proposal_body(dugite_primitives::transaction::CostModels {
            plutus_v1: Some(vec![100; 1]),
            plutus_v2: None,
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        });
        let mut errors = Vec::new();
        check_pparam_update_well_formed(&params, &body, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(&errors[0], ValidationError::MalformedProposal { reason, .. }
                if reason.contains("cost_models[PlutusV1] too short")),
            "PV11 must reject short V1 vector, got: {errors:?}",
        );
    }

    #[test]
    fn test_cost_models_pv11_rejects_short_v2() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 11;
        let body = cost_models_proposal_body(dugite_primitives::transaction::CostModels {
            plutus_v1: None,
            plutus_v2: Some(vec![100; 100]),
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        });
        let mut errors = Vec::new();
        check_pparam_update_well_formed(&params, &body, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(&errors[0], ValidationError::MalformedProposal { reason, .. }
                if reason.contains("cost_models[PlutusV2] too short")),
            "PV11 must reject short V2 vector, got: {errors:?}",
        );
    }

    #[test]
    fn test_cost_models_pv11_rejects_short_v3() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 11;
        let body = cost_models_proposal_body(dugite_primitives::transaction::CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: Some(vec![100; 250]),
            plutus_v4: None,
            ..Default::default()
        });
        let mut errors = Vec::new();
        check_pparam_update_well_formed(&params, &body, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(&errors[0], ValidationError::MalformedProposal { reason, .. }
                if reason.contains("cost_models[PlutusV3] too short")),
            "PV11 must reject short V3 vector, got: {errors:?}",
        );
    }

    #[test]
    fn test_cost_models_pv11_accepts_correct_lengths() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 11;
        let body = cost_models_proposal_body(dugite_primitives::transaction::CostModels {
            plutus_v1: Some(vec![100; 166]),
            plutus_v2: Some(vec![100; 175]),
            plutus_v3: Some(vec![100; 297]),
            plutus_v4: None,
            ..Default::default()
        });
        let mut errors = Vec::new();
        check_pparam_update_well_formed(&params, &body, &mut errors);
        assert!(
            errors.is_empty(),
            "PV11 must accept correctly sized cost_models update, got: {errors:?}",
        );
    }

    // ---------------------------------------------------------------------------
    // calculate_deposits_and_refunds — pre-Conway StakeDeregistration uses
    // stake_key_deposits map (not current key_deposit) when the map has an entry
    // ---------------------------------------------------------------------------

    /// Verifies the `StakeDeregistration` branch reads the per-credential stored
    /// deposit amount rather than falling back to the current `key_deposit`
    /// protocol parameter when `stake_key_deposits` is present and contains the
    /// credential.  This matters when `key_deposit` is later changed via
    /// governance action: the refund must equal what was paid at registration
    /// time.
    #[test]
    fn test_pre_conway_deregistration_uses_stored_deposit() {
        let mut params = ProtocolParameters::mainnet_defaults();
        // Simulate a governance-voted change: key_deposit is now 3_000_000
        // but the credential originally deposited 1_800_000.
        params.key_deposit = Lovelace(3_000_000);
        params.protocol_version_major = 8; // pre-Conway Babbage

        let original_deposit: u64 = 1_800_000;
        let credential = test_credential(0x20);
        let key = credential.to_typed_hash32();

        let mut deposit_map: imbl::HashMap<Hash32, u64> = imbl::HashMap::new();
        deposit_map.insert(key, original_deposit);

        let cert = Certificate::StakeDeregistration(credential.clone());

        let (deposits, refunds) =
            calculate_deposits_and_refunds(&[cert], &params, None, Some(&deposit_map));

        assert_eq!(deposits, 0, "StakeDeregistration should produce no deposit");
        assert_eq!(
            refunds, original_deposit,
            "StakeDeregistration refund must use the stored deposit map entry \
             ({original_deposit}) not the current key_deposit ({})",
            params.key_deposit.0
        );
    }

    // ---------------------------------------------------------------------------
    // check_voter_authority — DisallowedVoters predicate (Conway GOV)
    //
    // Voter × action authority matrix (Haskell `checkVotersAreValid`):
    //   NoConfidence:        SPO yes, DRep yes, CC NO
    //   UpdateCommittee:     SPO yes, DRep yes, CC NO
    //   NewConstitution:     SPO NO,  DRep yes, CC yes
    //   HardForkInitiation:  SPO yes, DRep yes, CC yes
    //   ParameterChange:     SPO yes (per-group threshold check at ratification),
    //                        DRep yes, CC yes
    //   TreasuryWithdrawals: SPO NO,  DRep yes, CC yes
    //   InfoAction:          all yes
    // ---------------------------------------------------------------------------

    fn cc_voter() -> Voter {
        Voter::ConstitutionalCommittee(test_credential(0xCC))
    }

    fn drep_voter() -> Voter {
        Voter::DRep(test_credential(0xDD))
    }

    fn spo_voter() -> Voter {
        Voter::StakePool(Hash32::from_bytes([0xAA; 32]))
    }

    fn anchor_stub() -> Anchor {
        Anchor {
            url: String::new(),
            data_hash: Hash32::from_bytes([0u8; 32]),
        }
    }

    #[test]
    fn test_disallowed_voters_spo_voting_on_new_constitution() {
        let action = GovAction::NewConstitution {
            prev_action_id: None,
            constitution: dugite_primitives::transaction::Constitution {
                anchor: anchor_stub(),
                script_hash: None,
            },
        };
        assert!(is_voter_disallowed(&spo_voter(), &action));
    }

    #[test]
    fn test_disallowed_voters_spo_voting_on_treasury_withdrawals() {
        let action = GovAction::TreasuryWithdrawals {
            withdrawals: BTreeMap::new(),
            policy_hash: None,
        };
        assert!(is_voter_disallowed(&spo_voter(), &action));
    }

    #[test]
    fn test_disallowed_voters_committee_voting_on_no_confidence() {
        let action = GovAction::NoConfidence {
            prev_action_id: None,
        };
        assert!(is_voter_disallowed(&cc_voter(), &action));
    }

    #[test]
    fn test_disallowed_voters_committee_voting_on_update_committee() {
        let action = GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![],
            members_to_add: BTreeMap::new(),
            threshold: Rational {
                numerator: 1,
                denominator: 2,
            },
        };
        assert!(is_voter_disallowed(&cc_voter(), &action));
    }

    #[test]
    fn test_voter_authority_spo_on_hard_fork_initiation_allowed() {
        let action = GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (10, 0),
        };
        assert!(!is_voter_disallowed(&spo_voter(), &action));
    }

    #[test]
    fn test_voter_authority_info_action_allows_all() {
        let action = GovAction::InfoAction;
        assert!(!is_voter_disallowed(&spo_voter(), &action));
        assert!(!is_voter_disallowed(&drep_voter(), &action));
        assert!(!is_voter_disallowed(&cc_voter(), &action));
    }

    #[test]
    fn test_voter_authority_drep_can_vote_on_no_confidence() {
        let action = GovAction::NoConfidence {
            prev_action_id: None,
        };
        assert!(!is_voter_disallowed(&drep_voter(), &action));
    }

    #[test]
    fn test_voter_authority_drep_can_vote_on_all_actions() {
        // DRep is authorised on every action type.
        let actions: Vec<GovAction> = vec![
            GovAction::NoConfidence {
                prev_action_id: None,
            },
            GovAction::UpdateCommittee {
                prev_action_id: None,
                members_to_remove: vec![],
                members_to_add: BTreeMap::new(),
                threshold: Rational {
                    numerator: 1,
                    denominator: 2,
                },
            },
            GovAction::NewConstitution {
                prev_action_id: None,
                constitution: dugite_primitives::transaction::Constitution {
                    anchor: anchor_stub(),
                    script_hash: None,
                },
            },
            GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (10, 0),
            },
            GovAction::TreasuryWithdrawals {
                withdrawals: BTreeMap::new(),
                policy_hash: None,
            },
            GovAction::InfoAction,
        ];
        for action in &actions {
            assert!(
                !is_voter_disallowed(&drep_voter(), action),
                "DRep must be allowed on action: {action:?}"
            );
        }
    }

    #[test]
    fn test_voter_authority_committee_allowed_on_constitution_and_hard_fork() {
        // CC is allowed on NewConstitution and HardForkInitiation
        // (it's only forbidden on NoConfidence and UpdateCommittee).
        let new_const = GovAction::NewConstitution {
            prev_action_id: None,
            constitution: dugite_primitives::transaction::Constitution {
                anchor: anchor_stub(),
                script_hash: None,
            },
        };
        assert!(!is_voter_disallowed(&cc_voter(), &new_const));

        let hf = GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (10, 0),
        };
        assert!(!is_voter_disallowed(&cc_voter(), &hf));
    }

    #[test]
    fn test_voter_authority_spo_on_parameter_change_allowed_at_phase1() {
        // D-5: SPO is only allowed to vote on ParameterChange if the update
        // touches at least one SecurityGroup-relevant field (Haskell
        // `votingStakePoolThresholdInternal` returns `NoVotingAllowed` for
        // non-security-group changes, which the GOV rule surfaces as
        // DisallowedVoters at Phase-1, not just at ratification time).
        //
        // Empty update (no SecurityGroup fields) → SPO is disallowed.
        let empty_action = GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::default(),
            policy_hash: None,
        };
        assert!(is_voter_disallowed(&spo_voter(), &empty_action));

        // ParameterChange touching a SecurityGroup field (min_fee_a = key 0)
        // → SPO is allowed.
        let security_action = GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::new(dugite_primitives::transaction::ProtocolParamUpdate {
                min_fee_a: Some(44),
                ..Default::default()
            }),
            policy_hash: None,
        };
        assert!(!is_voter_disallowed(&spo_voter(), &security_action));
    }

    // ---------------------------------------------------------------------------
    // is_voter_unknown — VotersDoNotExist predicate (Conway GOV)
    //
    // A voter is "unknown" when its credential / pool ID is not present in the
    // corresponding registry passed via `ValidationContext`:
    //   - DRepVoter: not in `registered_dreps`
    //   - StakePoolVoter: not in `registered_pools`
    //   - CommitteeVoter: hot credential not in `committee_authorized_hot_keys`
    //
    // When the corresponding context field is `None`, the voter is treated as
    // known (lenient default — see is_voter_unknown doc comment).
    // ---------------------------------------------------------------------------

    /// Build a 28-byte hash with all bytes equal to `b`, padded to Hash32 form
    /// matching the canonical kind-tagged shape (`to_typed_hash32`) used by
    /// registered_dreps, committee_authorized_hot_keys, etc.  For key
    /// credentials byte 28 is 0x00, which is identical to plain
    /// `to_hash32_padded`, so this helper is interchangeable for the
    /// VerificationKey case (which is what the call-sites use).
    fn padded_key_hash(b: u8) -> Hash32 {
        Hash28::from_bytes([b; 28]).to_hash32_padded()
    }

    #[test]
    fn test_voters_do_not_exist_unregistered_drep() {
        // A DRep key-hash voter whose credential is not in registered_dreps
        // is reported as unknown.
        let unregistered = test_credential(0xD0);
        let voter = Voter::DRep(unregistered);

        // The set contains some other DRep, not the voter's credential.
        let mut dreps: HashSet<Hash32> = HashSet::new();
        dreps.insert(padded_key_hash(0xEE));

        let ctx = ValidationContext::new().with_dreps(dreps);
        assert!(
            is_voter_unknown(&voter, &ctx),
            "DRep voter not in registered_dreps must be reported as unknown"
        );
    }

    #[test]
    fn test_voters_do_not_exist_unregistered_drep_script() {
        // A DRep script-hash voter whose credential is not in registered_dreps
        // is reported as unknown.  Mirrors the script-credential variant of
        // Haskell's `Credential 'DRepRole`.
        let unregistered = Credential::Script(Hash28::from_bytes([0xD1; 28]));
        let voter = Voter::DRep(unregistered);

        let dreps: HashSet<Hash32> = HashSet::new();
        let ctx = ValidationContext::new().with_dreps(dreps);

        assert!(
            is_voter_unknown(&voter, &ctx),
            "Script-credential DRep voter not in registered_dreps must be reported as unknown"
        );
    }

    #[test]
    fn test_voters_do_not_exist_unregistered_pool() {
        // A StakePool voter whose pool ID is not in registered_pools is
        // reported as unknown.  Voter::StakePool wraps a Hash32 (28 raw bytes
        // zero-padded), but registered_pools stores Hash28; the predicate
        // truncates and matches.
        let unregistered_id = Hash28::from_bytes([0xAA; 28]);
        let voter = Voter::StakePool(unregistered_id.to_hash32_padded());

        let mut pools: HashSet<Hash28> = HashSet::new();
        pools.insert(Hash28::from_bytes([0xBB; 28])); // some other pool

        let ctx = ValidationContext::new().with_pools(pools);
        assert!(
            is_voter_unknown(&voter, &ctx),
            "StakePool voter not in registered_pools must be reported as unknown"
        );
    }

    #[test]
    fn test_voters_do_not_exist_unauthorized_committee_hot_key() {
        // A ConstitutionalCommittee voter whose hot credential is not in
        // committee_authorized_hot_keys is reported as unknown.
        let unauthorized_hot = test_credential(0xCC);
        let voter = Voter::ConstitutionalCommittee(unauthorized_hot);

        let mut hot_keys: HashSet<Hash32> = HashSet::new();
        hot_keys.insert(padded_key_hash(0x77)); // a different authorised hot key

        let ctx = ValidationContext::new().with_committee_authorized_hot_keys(hot_keys);
        assert!(
            is_voter_unknown(&voter, &ctx),
            "Committee voter whose hot credential is not in \
             committee_authorized_hot_keys must be reported as unknown"
        );
    }

    #[test]
    fn test_voter_known_registered_drep() {
        // Positive case: a DRep voter whose credential IS in registered_dreps
        // is NOT reported as unknown.
        let cred = test_credential(0xD2);
        let voter = Voter::DRep(cred.clone());

        let mut dreps: HashSet<Hash32> = HashSet::new();
        dreps.insert(cred.to_typed_hash32());

        let ctx = ValidationContext::new().with_dreps(dreps);
        assert!(
            !is_voter_unknown(&voter, &ctx),
            "Registered DRep voter must NOT be reported as unknown"
        );
    }

    #[test]
    fn test_voter_known_registered_drep_script_credential() {
        // Regression: a script-credential DRep voter whose credential IS in
        // registered_dreps must NOT be reported as unknown.  This exercises
        // the kind-tag (byte 28 = 0x01) path of `to_typed_hash32` — a previous
        // bug looked up using `to_hash32_padded` (which drops the kind tag),
        // causing every script-credential DRep voter to be falsely rejected
        // even when registered.  Mirrors Haskell's `Map.member` lookup over a
        // `Credential 'DRepRole`-keyed map (`vsDReps`).
        let same_hash = Hash28::from_bytes([0xD4; 28]);
        let key_cred = Credential::VerificationKey(same_hash);
        let script_cred = Credential::Script(same_hash);

        // Register both: same 28-byte hash, distinct kinds → distinct entries.
        let mut dreps: HashSet<Hash32> = HashSet::new();
        dreps.insert(key_cred.to_typed_hash32());
        dreps.insert(script_cred.to_typed_hash32());
        assert_eq!(dreps.len(), 2, "key and script DReps must hash distinctly");

        let ctx = ValidationContext::new().with_dreps(dreps);

        let key_voter = Voter::DRep(key_cred);
        let script_voter = Voter::DRep(script_cred);
        assert!(
            !is_voter_unknown(&key_voter, &ctx),
            "Registered key-credential DRep voter must NOT be reported as unknown"
        );
        assert!(
            !is_voter_unknown(&script_voter, &ctx),
            "Registered script-credential DRep voter must NOT be reported as unknown"
        );
    }

    #[test]
    fn test_voter_known_registered_pool() {
        // Positive case: a StakePool voter whose pool ID IS in registered_pools
        // is NOT reported as unknown.
        let pool_id = Hash28::from_bytes([0xA1; 28]);
        let voter = Voter::StakePool(pool_id.to_hash32_padded());

        let mut pools: HashSet<Hash28> = HashSet::new();
        pools.insert(pool_id);

        let ctx = ValidationContext::new().with_pools(pools);
        assert!(
            !is_voter_unknown(&voter, &ctx),
            "Registered pool voter must NOT be reported as unknown"
        );
    }

    #[test]
    fn test_voter_known_authorized_committee_hot_key() {
        // Positive case: a Committee voter whose hot credential IS in
        // committee_authorized_hot_keys is NOT reported as unknown.
        let hot = test_credential(0xC1);
        let voter = Voter::ConstitutionalCommittee(hot.clone());

        let mut hot_keys: HashSet<Hash32> = HashSet::new();
        hot_keys.insert(hot.to_typed_hash32());

        let ctx = ValidationContext::new().with_committee_authorized_hot_keys(hot_keys);
        assert!(
            !is_voter_unknown(&voter, &ctx),
            "Authorised committee hot key voter must NOT be reported as unknown"
        );
    }

    #[test]
    fn test_voter_known_authorized_committee_hot_key_script_credential() {
        // Regression: a script-credential committee voter whose hot
        // credential IS in committee_authorized_hot_keys must NOT be reported
        // as unknown — matches the script-cred DRep regression above.
        let same_hash = Hash28::from_bytes([0xC4; 28]);
        let key_hot = Credential::VerificationKey(same_hash);
        let script_hot = Credential::Script(same_hash);

        let mut hot_keys: HashSet<Hash32> = HashSet::new();
        hot_keys.insert(key_hot.to_typed_hash32());
        hot_keys.insert(script_hot.to_typed_hash32());
        assert_eq!(hot_keys.len(), 2);

        let ctx = ValidationContext::new().with_committee_authorized_hot_keys(hot_keys);
        assert!(
            !is_voter_unknown(&Voter::ConstitutionalCommittee(key_hot), &ctx),
            "Authorised key-credential committee hot key voter must NOT be reported as unknown"
        );
        assert!(
            !is_voter_unknown(&Voter::ConstitutionalCommittee(script_hot), &ctx),
            "Authorised script-credential committee hot key voter must NOT be reported as unknown"
        );
    }

    #[test]
    fn test_voter_unknown_lenient_default_when_context_missing() {
        // When the relevant context field is `None`, the predicate must return
        // `false` (voter treated as known) — same lenient default as
        // `active_proposals` for `DisallowedVoters` (Task 2).
        let drep_voter_unset = Voter::DRep(test_credential(0xD3));
        let pool_voter_unset = Voter::StakePool(Hash28::from_bytes([0xA3; 28]).to_hash32_padded());
        let cc_voter_unset = Voter::ConstitutionalCommittee(test_credential(0xC3));

        let ctx = ValidationContext::new(); // all None
        assert!(
            !is_voter_unknown(&drep_voter_unset, &ctx),
            "DRep voter must default to known when registered_dreps is None"
        );
        assert!(
            !is_voter_unknown(&pool_voter_unset, &ctx),
            "Pool voter must default to known when registered_pools is None"
        );
        assert!(
            !is_voter_unknown(&cc_voter_unset, &ctx),
            "Committee voter must default to known when committee_authorized_hot_keys is None"
        );
    }

    // ---------------------------------------------------------------------------
    // is_vote_on_expired_action — VotingOnExpiredGovAction predicate (Conway GOV)
    //
    // Per Haskell `checkVotesAreNotForExpiredActions`, a vote is rejected only
    // when `current_epoch > gasExpiresAfter`.  The boundary case
    // (`current_epoch == gasExpiresAfter`) is allowed.
    // ---------------------------------------------------------------------------

    /// Build a `(GovActionId, ValidationContext)` pair where `current_epoch =
    /// current` and the proposal at `gov_action_id` has the given
    /// `expires_after_epoch`.
    fn ctx_with_proposal(
        action_id: &GovActionId,
        current: u64,
        expires_after: u64,
    ) -> ValidationContext {
        let proposal = ActiveProposal {
            gov_action: GovAction::InfoAction,
            return_addr: vec![0xe0; 29],
            deposit: Lovelace(0),
            expires_after_epoch: EpochNo(expires_after),
            // proposed_in_epoch is unused by this predicate; pick something sane.
            proposed_in_epoch: EpochNo(expires_after.saturating_sub(6)),
        };
        let mut active: HashMap<GovActionId, ActiveProposal> = HashMap::new();
        active.insert(action_id.clone(), proposal);
        ValidationContext::new()
            .with_active_proposals(active)
            .with_epoch(current)
    }

    #[test]
    fn test_voting_on_expired_gov_action_rejected_strict_greater() {
        // current_epoch = 20, expires_after = 10 -> rejected (strictly past).
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0x77; 32]),
            action_index: 0,
        };
        let ctx = ctx_with_proposal(&action_id, 20, 10);
        assert!(
            is_vote_on_expired_action(&action_id, &ctx),
            "current=20, expires_after=10 must be rejected"
        );
    }

    #[test]
    fn test_voting_on_action_at_expiry_boundary_allowed() {
        // current_epoch == expires_after -> allowed (boundary inclusive).
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0x77; 32]),
            action_index: 1,
        };
        let ctx = ctx_with_proposal(&action_id, 10, 10);
        assert!(
            !is_vote_on_expired_action(&action_id, &ctx),
            "current=10, expires_after=10 is the boundary and must be allowed \
             (Haskell: current_epoch <= gasExpiresAfter)"
        );
    }

    #[test]
    fn test_voting_on_future_action_allowed() {
        // current_epoch < expires_after -> trivially allowed.
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0x77; 32]),
            action_index: 2,
        };
        let ctx = ctx_with_proposal(&action_id, 5, 10);
        assert!(
            !is_vote_on_expired_action(&action_id, &ctx),
            "current=5, expires_after=10 must be allowed"
        );
    }

    #[test]
    fn test_voting_on_action_skipped_when_active_proposals_none() {
        // ctx.active_proposals = None -> lenient default, predicate returns false.
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0x77; 32]),
            action_index: 3,
        };
        // Set current_epoch but leave active_proposals = None.
        let ctx = ValidationContext::new().with_epoch(9999);
        assert!(
            !is_vote_on_expired_action(&action_id, &ctx),
            "Predicate must skip (return false) when active_proposals is None"
        );
    }

    // ---------------------------------------------------------------------------
    // hardfork_proposal_cant_follow — ProposalCantFollow (Conway GOV)
    //
    // Worked examples byte-verified against
    // `.claude/agent-memory/cardano-ledger-oracle/hardfork-pvcanfollow-exact-mechanics.md`
    // (live-verified against `preceedingHardFork`, Gov.hs:673-694).
    // ---------------------------------------------------------------------------

    fn hf_action(prev: Option<GovActionId>, target: (u64, u64)) -> GovAction {
        GovAction::HardForkInitiation {
            prev_action_id: prev,
            protocol_version: target,
        }
    }

    #[test]
    fn test_hardfork_cant_follow_genesis_root_major_bump_valid() {
        // Current (9,0). Proposal targets (10,0) with prev=None and no
        // enacted HardFork root yet -> matches disjunct 1 (root match) ->
        // base=(9,0) -> pvCanFollow(9,0 -> 10,0) = true -> not rejected.
        let action = hf_action(None, (10, 0));
        let ctx = ValidationContext::new().with_enacted_gov_roots(EnactedGovRoots::default());
        let result = hardfork_proposal_cant_follow(
            &action,
            0,
            &[],
            Hash32::from_bytes([0x01; 32]),
            &ctx,
            9,
            0,
        );
        assert!(
            result.is_none(),
            "genesis-root major bump (9,0)->(10,0) must be accepted, got {result:?}"
        );
    }

    #[test]
    fn test_hardfork_cant_follow_genesis_root_double_major_bump_rejected() {
        // Current (9,0). Proposal targets (11,0) with prev=None -> matches
        // disjunct 1 (root match) -> base=(9,0) -> pvCanFollow(9,0 -> 11,0)
        // = false (skips a major version) -> rejected.
        let action = hf_action(None, (11, 0));
        let ctx = ValidationContext::new().with_enacted_gov_roots(EnactedGovRoots::default());
        let result = hardfork_proposal_cant_follow(
            &action,
            0,
            &[],
            Hash32::from_bytes([0x01; 32]),
            &ctx,
            9,
            0,
        );
        assert_eq!(
            result,
            Some((11, 0, 9, 0)),
            "genesis-root double major bump (9,0)->(11,0) must be rejected"
        );
    }

    #[test]
    fn test_hardfork_cant_follow_same_tx_chain_minor_bump_valid() {
        // Oracle worked example 2: current (9,0). Proposal A (idx 0):
        // prev=None, target=(10,0) — root-anchored, valid. Proposal B
        // (idx 1) in the SAME tx: prev=A's id, target=(10,1) — chains onto
        // A's OWN target (10,0), pvCanFollow(10,0 -> 10,1) = true -> valid.
        let tx_hash = Hash32::from_bytes([0x02; 32]);
        let a_id = GovActionId {
            transaction_id: tx_hash,
            action_index: 0,
        };
        let proposal_a = ProposalProcedure {
            deposit: Lovelace(0),
            return_addr: vec![0xe0; 29],
            gov_action: hf_action(None, (10, 0)),
            anchor: anchor_stub(),
        };
        let proposal_b_action = hf_action(Some(a_id), (10, 1));
        let proposals = vec![proposal_a];
        let ctx = ValidationContext::new().with_enacted_gov_roots(EnactedGovRoots::default());
        let result =
            hardfork_proposal_cant_follow(&proposal_b_action, 1, &proposals, tx_hash, &ctx, 9, 0);
        assert!(
            result.is_none(),
            "same-tx minor bump chained onto an in-flight major bump must be accepted, got {result:?}"
        );
    }

    #[test]
    fn test_hardfork_cant_follow_same_tx_chain_double_major_bump_rejected() {
        // Oracle worked example 3: same setup as above, but B instead
        // targets (11,0) — attempting to chain a SECOND major bump onto A
        // before A is enacted. newMajor(11) > succVersion(9)=10, so the
        // short-circuit forces base back to the LIVE current (9,0)
        // (bypassing the chain lookup entirely) -> pvCanFollow(9,0 -> 11,0)
        // = false -> rejected.
        let tx_hash = Hash32::from_bytes([0x03; 32]);
        let a_id = GovActionId {
            transaction_id: tx_hash,
            action_index: 0,
        };
        let proposal_a = ProposalProcedure {
            deposit: Lovelace(0),
            return_addr: vec![0xe0; 29],
            gov_action: hf_action(None, (10, 0)),
            anchor: anchor_stub(),
        };
        let proposal_b_action = hf_action(Some(a_id), (11, 0));
        let proposals = vec![proposal_a];
        let ctx = ValidationContext::new().with_enacted_gov_roots(EnactedGovRoots::default());
        let result =
            hardfork_proposal_cant_follow(&proposal_b_action, 1, &proposals, tx_hash, &ctx, 9, 0);
        assert_eq!(
            result,
            Some((11, 0, 9, 0)),
            "chaining a second major bump onto an in-flight major bump must be rejected \
             (base forced back to live current, not A's target), got {result:?}"
        );
    }

    #[test]
    fn test_hardfork_cant_follow_active_onchain_parent_chain() {
        // prev references an on-chain ACTIVE (in-flight) HardForkInitiation
        // proposal (not same-tx) -> base = that proposal's own target.
        let parent_id = GovActionId {
            transaction_id: Hash32::from_bytes([0x04; 32]),
            action_index: 0,
        };
        let mut active: HashMap<GovActionId, ActiveProposal> = HashMap::new();
        active.insert(
            parent_id.clone(),
            ActiveProposal {
                gov_action: hf_action(None, (10, 0)),
                return_addr: vec![0xe0; 29],
                deposit: Lovelace(0),
                expires_after_epoch: EpochNo(100),
                proposed_in_epoch: EpochNo(1),
            },
        );
        let action = hf_action(Some(parent_id), (10, 1));
        let ctx = ValidationContext::new()
            .with_enacted_gov_roots(EnactedGovRoots::default())
            .with_active_proposals(active);
        let result = hardfork_proposal_cant_follow(
            &action,
            0,
            &[],
            Hash32::from_bytes([0x05; 32]),
            &ctx,
            9,
            0,
        );
        assert!(
            result.is_none(),
            "minor bump chained onto an active on-chain HardFork parent must be accepted, got {result:?}"
        );
    }

    #[test]
    fn test_hardfork_cant_follow_non_hardfork_action_never_fires() {
        let action = GovAction::InfoAction;
        let ctx = ValidationContext::new().with_enacted_gov_roots(EnactedGovRoots::default());
        let result = hardfork_proposal_cant_follow(
            &action,
            0,
            &[],
            Hash32::from_bytes([0x06; 32]),
            &ctx,
            9,
            0,
        );
        assert!(
            result.is_none(),
            "non-HardFork actions must never fire this predicate"
        );
    }

    #[test]
    fn test_hardfork_cant_follow_skipped_when_enacted_gov_roots_none() {
        // Lenient default: no enacted_gov_roots plumbed in -> predicate
        // never fires, matching the sibling InvalidPrevGovActionId check.
        let action = hf_action(None, (11, 0)); // would otherwise be rejected
        let ctx = ValidationContext::new();
        let result = hardfork_proposal_cant_follow(
            &action,
            0,
            &[],
            Hash32::from_bytes([0x07; 32]),
            &ctx,
            9,
            0,
        );
        assert!(
            result.is_none(),
            "predicate must be skipped when enacted_gov_roots is None"
        );
    }

    // ---------------------------------------------------------------------------
    // is_proposal_return_account_unregistered — ProposalReturnAccountDoesNotExist
    //                                            (Conway GOV)
    //
    // Per Haskell `processProposal`, every proposal procedure's
    // `pProcReturnAddr` credential must be present in `accounts`.  The check
    // is **skipped during Conway bootstrap** (`pvMajor == 9`) and runs from
    // PV >= 10 onwards.  When `ctx.reward_accounts` is `None`, the predicate
    // returns false (lenient default) — matching the convention used by the
    // other GOV predicates.
    // ---------------------------------------------------------------------------

    /// Build a `ProtocolParameters` pinned to the given protocol-version
    /// major number.  The other parameter values are irrelevant to this
    /// predicate.
    fn pparams_at_pv(pv_major: u64) -> ProtocolParameters {
        let mut p = ProtocolParameters::mainnet_defaults();
        p.protocol_version_major = pv_major;
        p
    }

    /// Build a 29-byte stake/reward address: `header || credential_hash[28]`.
    /// `is_script` flips bit 4 of the header (`0xe0` key vs `0xf0` script),
    /// matching the Cardano reward-address layout used by
    /// `reward_account_to_hash` for the credential-type tag in byte 28 of
    /// the resulting Hash32.
    fn return_addr_29(cred_byte: u8, is_script: bool) -> Vec<u8> {
        let header: u8 = if is_script { 0xf0 } else { 0xe0 };
        let mut bytes = Vec::with_capacity(29);
        bytes.push(header);
        bytes.extend_from_slice(&[cred_byte; 28]);
        bytes
    }

    /// Build a `ProposalProcedure` whose `return_addr` is the given 29-byte
    /// reward-address payload.  Only `return_addr` matters for this
    /// predicate; the other fields carry no-op values.
    fn proposal_with_return_addr(return_addr: Vec<u8>) -> ProposalProcedure {
        ProposalProcedure {
            deposit: Lovelace(1_000_000_000),
            return_addr,
            gov_action: GovAction::InfoAction,
            anchor: Anchor {
                url: String::new(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        }
    }

    #[test]
    fn test_proposal_return_account_unregistered_post_bootstrap_rejected() {
        // PV=10 (post-bootstrap), reward_accounts present but does NOT contain
        // the proposal's return-address credential -> predicate returns true.
        let params = pparams_at_pv(10);
        let proposal = proposal_with_return_addr(return_addr_29(0x88, false));

        // Reward-accounts map carries some other credential — definitely not
        // the [0x88; 28] credential the proposal references.
        let mut accounts: HashMap<Hash32, Lovelace> = HashMap::new();
        accounts.insert(
            Hash28::from_bytes([0x11; 28]).to_hash32_padded(),
            Lovelace(0),
        );
        let ctx = ValidationContext::new().with_reward_accounts(accounts);

        assert!(
            is_proposal_return_account_unregistered(&proposal, &params, &ctx),
            "Unregistered return-addr credential at PV=10 must be rejected"
        );
    }

    #[test]
    fn test_proposal_return_account_check_skipped_during_bootstrap() {
        // PV=9 (Conway bootstrap), unregistered credential -> predicate must
        // return false regardless (bootstrap-phase skip per
        // hardforkConwayBootstrapPhase).
        let params = pparams_at_pv(9);
        let proposal = proposal_with_return_addr(return_addr_29(0x88, false));

        // Empty reward_accounts — the credential is definitely not registered,
        // but the bootstrap skip must short-circuit the check.
        let ctx = ValidationContext::new().with_reward_accounts(HashMap::new());

        assert!(
            !is_proposal_return_account_unregistered(&proposal, &params, &ctx),
            "Bootstrap (PV=9) must skip the return-account check entirely"
        );
    }

    #[test]
    fn test_proposal_return_account_registered_passes() {
        // PV=10 with a registered credential matching the proposal's
        // return_addr -> predicate returns false (accepted).
        let params = pparams_at_pv(10);
        let proposal = proposal_with_return_addr(return_addr_29(0x88, false));

        // Compute the same key the predicate will compute via
        // `reward_account_to_hash`, so the registered set actually matches.
        let key = crate::state::LedgerState::reward_account_to_hash(&proposal.return_addr);
        let mut accounts: HashMap<Hash32, Lovelace> = HashMap::new();
        accounts.insert(key, Lovelace(0));
        let ctx = ValidationContext::new().with_reward_accounts(accounts);

        assert!(
            !is_proposal_return_account_unregistered(&proposal, &params, &ctx),
            "Registered return-addr credential must be accepted"
        );
    }

    #[test]
    fn test_proposal_return_account_skipped_when_reward_accounts_none() {
        // PV=10 but ctx.reward_accounts = None -> lenient default (false).
        // This mirrors `is_voter_unknown` / `is_vote_on_expired_action`
        // behaviour when the caller hasn't plumbed in the relevant state.
        let params = pparams_at_pv(10);
        let proposal = proposal_with_return_addr(return_addr_29(0x88, false));

        let ctx = ValidationContext::new();
        assert!(ctx.reward_accounts.is_none());

        assert!(
            !is_proposal_return_account_unregistered(&proposal, &params, &ctx),
            "Predicate must skip (return false) when reward_accounts is None"
        );
    }

    // ---------------------------------------------------------------------------
    // is_proposal_return_addr_wrong_network — ProposalProcedureNetworkIdMismatch
    //                                          (Conway GOV)
    //
    // Per Haskell `processProposal`, every proposal procedure's
    // `pProcReturnAddr` must be on the same network as the node.  Bit 0 of
    // the reward-account header byte encodes the network (0 = testnet,
    // 1 = mainnet).  This check is **always enforced** — there is NO
    // Conway-bootstrap skip (the network id is a structural property, not
    // a post-bootstrap state lookup).  When `ctx.node_network` is `None`,
    // the predicate returns `None` (lenient default).
    // ---------------------------------------------------------------------------

    use dugite_primitives::network::NetworkId;

    /// Build a 29-byte reward address whose header network bit matches
    /// `network` (0 = testnet, 1 = mainnet).  The high nibble is set to
    /// `0xe0` (key-credential, stake/reward address) and the low bit is
    /// flipped accordingly.
    fn return_addr_29_with_network(network_bit: u8) -> Vec<u8> {
        // 0xe0 has bit 0 = 0 (testnet); 0xe1 has bit 0 = 1 (mainnet).
        let header: u8 = 0xe0 | (network_bit & 0x01);
        let mut bytes = Vec::with_capacity(29);
        bytes.push(header);
        bytes.extend_from_slice(&[0x88u8; 28]);
        bytes
    }

    fn proposal_with_addr(addr: Vec<u8>) -> ProposalProcedure {
        ProposalProcedure {
            deposit: Lovelace(1_000_000_000),
            return_addr: addr,
            gov_action: GovAction::InfoAction,
            anchor: Anchor {
                url: String::new(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        }
    }

    #[test]
    fn test_proposal_return_addr_wrong_network_returns_actual() {
        // node = mainnet (1), proposal = testnet (0) -> Some(0)
        let proposal = proposal_with_addr(return_addr_29_with_network(0));
        let ctx = ValidationContext::new().with_network(NetworkId::Mainnet);
        assert_eq!(
            is_proposal_return_addr_wrong_network(&proposal, &ctx),
            Some(0),
            "Mismatched network must return the actual mismatched value"
        );
    }

    #[test]
    fn test_proposal_return_addr_correct_network_returns_none() {
        // node = mainnet (1), proposal = mainnet (1) -> None
        let proposal = proposal_with_addr(return_addr_29_with_network(1));
        let ctx = ValidationContext::new().with_network(NetworkId::Mainnet);
        assert_eq!(
            is_proposal_return_addr_wrong_network(&proposal, &ctx),
            None,
            "Matching network must return None"
        );
    }

    #[test]
    fn test_proposal_return_addr_check_skipped_when_network_id_none() {
        // ctx.node_network = None -> lenient default (None), regardless of
        // the proposal's network bit.
        let proposal = proposal_with_addr(return_addr_29_with_network(0));
        let ctx = ValidationContext::new();
        assert!(ctx.node_network.is_none());
        assert_eq!(
            is_proposal_return_addr_wrong_network(&proposal, &ctx),
            None,
            "Predicate must skip (return None) when node_network is None"
        );
    }

    #[test]
    fn test_proposal_return_addr_network_check_runs_in_bootstrap() {
        // PV=9 (Conway bootstrap) is irrelevant here — the network-id check
        // is always enforced.  This test proves the predicate fires for a
        // mismatch even when the surrounding context represents bootstrap.
        // Note: this predicate doesn't even take `params` (no PV gating).
        let proposal = proposal_with_addr(return_addr_29_with_network(0));
        let ctx = ValidationContext::new().with_network(NetworkId::Mainnet);
        // The presence of a hypothetical PV=9 ProtocolParameters cannot
        // suppress the predicate — it has no PV input.
        assert_eq!(
            is_proposal_return_addr_wrong_network(&proposal, &ctx),
            Some(0),
            "Network-id check must fire even in (hypothetical) bootstrap; \
             unlike ProposalReturnAccountDoesNotExist, there is no PV=9 skip"
        );
    }

    #[test]
    fn test_proposal_return_addr_empty_addr_returns_none() {
        // Malformed (empty) return_addr is handled by a different predicate;
        // this rule must not double-fire on shape errors.
        let proposal = proposal_with_addr(Vec::new());
        let ctx = ValidationContext::new().with_network(NetworkId::Mainnet);
        assert_eq!(
            is_proposal_return_addr_wrong_network(&proposal, &ctx),
            None,
            "Empty return_addr must short-circuit (None)"
        );
    }

    // ---------------------------------------------------------------------------
    // treasury_withdrawal_network_mismatches —
    //   TreasuryWithdrawalsNetworkIdMismatch (Conway GOV)
    //
    // Per Haskell `processProposal`, every destination address in a
    // TreasuryWithdrawals proposal's `withdrawals` map must be on the same
    // network as the node.  Bit 0 of the reward-account header byte encodes
    // the network (0 = testnet, 1 = mainnet).  This check is **always
    // enforced** — there is NO Conway-bootstrap skip (same as
    // `ProposalProcedureNetworkIdMismatch`).  When `ctx.node_network` is
    // `None`, the predicate returns an empty vec (lenient default).
    // ---------------------------------------------------------------------------

    /// Build a `TreasuryWithdrawals` proposal whose `withdrawals` map is
    /// the given list of `(reward_addr_bytes, coin)` entries.  No
    /// `policy_hash` (constitution check is irrelevant to this predicate).
    fn treasury_withdrawals_proposal(entries: Vec<(Vec<u8>, Lovelace)>) -> ProposalProcedure {
        let withdrawals: BTreeMap<Vec<u8>, Lovelace> = entries.into_iter().collect();
        ProposalProcedure {
            deposit: Lovelace(1_000_000_000),
            return_addr: return_addr_29_with_network(1), // node network — irrelevant here
            gov_action: GovAction::TreasuryWithdrawals {
                withdrawals,
                policy_hash: None,
            },
            anchor: Anchor {
                url: String::new(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        }
    }

    #[test]
    fn test_treasury_withdrawals_network_mismatch_collects_all() {
        // node = mainnet (1).  Two testnet entries + one mainnet entry → only
        // the testnet entries are surfaced.
        let mut testnet_a = vec![0xe0u8];
        testnet_a.extend_from_slice(&[0x11u8; 28]);
        let mut testnet_b = vec![0xe0u8];
        testnet_b.extend_from_slice(&[0x22u8; 28]);
        let mut mainnet_ok = vec![0xe1u8];
        mainnet_ok.extend_from_slice(&[0x33u8; 28]);

        let proposal = treasury_withdrawals_proposal(vec![
            (testnet_a.clone(), Lovelace(1)),
            (testnet_b.clone(), Lovelace(2)),
            (mainnet_ok.clone(), Lovelace(3)),
        ]);
        let ctx = ValidationContext::new().with_network(NetworkId::Mainnet);

        let mismatches = treasury_withdrawal_network_mismatches(&proposal, &ctx);
        assert_eq!(
            mismatches.len(),
            2,
            "must surface both testnet entries; got: {mismatches:?}"
        );
        for (_, actual) in &mismatches {
            assert_eq!(
                *actual, 0,
                "mismatched entries must report actual=0 (testnet)"
            );
        }
    }

    #[test]
    fn test_treasury_withdrawals_network_match_returns_empty() {
        // All entries' network bit matches the node → empty vec.
        let mut a = vec![0xe1u8];
        a.extend_from_slice(&[0x11u8; 28]);
        let mut b = vec![0xe1u8];
        b.extend_from_slice(&[0x22u8; 28]);

        let proposal = treasury_withdrawals_proposal(vec![(a, Lovelace(1)), (b, Lovelace(2))]);
        let ctx = ValidationContext::new().with_network(NetworkId::Mainnet);

        assert!(
            treasury_withdrawal_network_mismatches(&proposal, &ctx).is_empty(),
            "All-match TreasuryWithdrawals must produce no mismatches"
        );
    }

    #[test]
    fn test_treasury_withdrawals_network_check_skips_non_tw_proposal() {
        // A non-TreasuryWithdrawals proposal must produce no mismatches even
        // if `return_addr` is on the wrong network — that's a different
        // predicate (`ProposalProcedureNetworkIdMismatch`).
        let proposal = proposal_with_addr(return_addr_29_with_network(0));
        let ctx = ValidationContext::new().with_network(NetworkId::Mainnet);

        assert!(
            treasury_withdrawal_network_mismatches(&proposal, &ctx).is_empty(),
            "Non-TreasuryWithdrawals proposals must short-circuit (empty vec)"
        );
    }

    #[test]
    fn test_treasury_withdrawals_network_check_skipped_when_network_none() {
        // ctx.node_network = None → lenient default (empty vec) regardless
        // of the entries' network bits.
        let mut testnet = vec![0xe0u8];
        testnet.extend_from_slice(&[0x11u8; 28]);
        let proposal = treasury_withdrawals_proposal(vec![(testnet, Lovelace(1))]);
        let ctx = ValidationContext::new();
        assert!(ctx.node_network.is_none());

        assert!(
            treasury_withdrawal_network_mismatches(&proposal, &ctx).is_empty(),
            "Predicate must skip (empty vec) when node_network is None"
        );
    }

    // ---------------------------------------------------------------------------
    // is_treasury_withdrawals_zero_sum — ZeroTreasuryWithdrawals (Conway GOV)
    //
    // Per Haskell `processProposal` (TreasuryWithdrawals branch), a TW proposal
    // whose total amount is zero (including the all-zero-entries case and
    // empty-map case) is rejected.  This check is **skipped during Conway
    // bootstrap** (PV == 9) per `hardforkConwayBootstrapPhase`.
    // ---------------------------------------------------------------------------

    #[test]
    fn test_treasury_withdrawals_zero_sum_post_bootstrap_rejected() {
        // PV=10, sum == 0 → predicate fires.
        let params = pparams_at_pv(10);
        let mut a = vec![0xe0u8];
        a.extend_from_slice(&[0x11u8; 28]);
        let mut b = vec![0xe0u8];
        b.extend_from_slice(&[0x22u8; 28]);
        let proposal = treasury_withdrawals_proposal(vec![(a, Lovelace(0)), (b, Lovelace(0))]);

        assert!(
            is_treasury_withdrawals_zero_sum(&proposal, &params),
            "All-zero TreasuryWithdrawals must fire post-bootstrap"
        );
    }

    #[test]
    fn test_treasury_withdrawals_zero_sum_at_pv9_skipped() {
        // PV=9 (Conway bootstrap) — predicate is silenced even with sum==0.
        let params = pparams_at_pv(9);
        let mut a = vec![0xe0u8];
        a.extend_from_slice(&[0x11u8; 28]);
        let proposal = treasury_withdrawals_proposal(vec![(a, Lovelace(0))]);

        assert!(
            !is_treasury_withdrawals_zero_sum(&proposal, &params),
            "Bootstrap (PV=9) must skip the zero-sum check entirely"
        );
    }

    #[test]
    fn test_treasury_withdrawals_nonzero_sum_accepted() {
        // PV=10, any non-zero entry → sum != 0 → predicate does not fire.
        let params = pparams_at_pv(10);
        let mut a = vec![0xe0u8];
        a.extend_from_slice(&[0x11u8; 28]);
        let mut b = vec![0xe0u8];
        b.extend_from_slice(&[0x22u8; 28]);
        let proposal = treasury_withdrawals_proposal(vec![(a, Lovelace(0)), (b, Lovelace(1))]);

        assert!(
            !is_treasury_withdrawals_zero_sum(&proposal, &params),
            "Non-zero total must accept the proposal"
        );
    }

    #[test]
    fn test_treasury_withdrawals_zero_sum_skips_non_tw_proposal() {
        // A non-TreasuryWithdrawals proposal must always return false,
        // independent of any other state.
        let params = pparams_at_pv(10);
        let proposal = proposal_with_addr(return_addr_29_with_network(1));
        assert!(
            !is_treasury_withdrawals_zero_sum(&proposal, &params),
            "Non-TreasuryWithdrawals proposals must short-circuit (false)"
        );
    }

    // ---------------------------------------------------------------------------
    // committee_update_conflicts — ConflictingCommitteeUpdate (Conway GOV)
    //
    // Per Haskell `processProposal` (UpdateCommittee branch), the
    // intersection of `members_to_add`'s keys and `members_to_remove` must
    // be empty — a member cannot be both added and removed in the same
    // action.  Always enforced (no Conway-bootstrap skip).  When the
    // proposal action is not `UpdateCommittee`, the predicate returns the
    // empty vec.
    // ---------------------------------------------------------------------------

    /// Helper: build a key-credential `Credential` from a single byte
    /// pattern.
    fn cred_key(b: u8) -> Credential {
        Credential::VerificationKey(Hash28::from_bytes([b; 28]))
    }

    /// Helper: build a script-credential `Credential` from a single byte
    /// pattern.
    fn cred_script(b: u8) -> Credential {
        Credential::Script(Hash28::from_bytes([b; 28]))
    }

    /// Build an `UpdateCommittee` proposal with the given add and remove
    /// sets.  Threshold/return_addr/anchor are stubs.
    fn update_committee_proposal(
        members_to_add: BTreeMap<Credential, u64>,
        members_to_remove: Vec<Credential>,
    ) -> ProposalProcedure {
        ProposalProcedure {
            deposit: Lovelace(1_000_000_000),
            return_addr: return_addr_29_with_network(1),
            gov_action: GovAction::UpdateCommittee {
                prev_action_id: None,
                members_to_remove,
                members_to_add,
                threshold: Rational {
                    numerator: 1,
                    denominator: 2,
                },
            },
            anchor: Anchor {
                url: String::new(),
                data_hash: Hash32::from_bytes([0u8; 32]),
            },
        }
    }

    #[test]
    fn test_committee_update_conflict_found() {
        // Same key-credential appears in both add-set and remove-set ->
        // surfaced as a conflict.  An additional non-conflicting add and a
        // non-conflicting remove must NOT appear in the conflict list.
        let mut adds: BTreeMap<Credential, u64> = BTreeMap::new();
        adds.insert(cred_key(0x11), 100);
        adds.insert(cred_key(0x22), 100);
        let removes = vec![cred_key(0x11), cred_key(0x33)];
        let proposal = update_committee_proposal(adds, removes);

        let conflicts = committee_update_conflicts(&proposal);
        assert_eq!(
            conflicts.len(),
            1,
            "exactly one conflicting credential, got: {conflicts:?}"
        );
        let expected = cred_key(0x11).to_typed_hash32().to_hex();
        assert_eq!(conflicts[0], expected);
    }

    #[test]
    fn test_committee_update_no_conflict_returns_empty() {
        // Disjoint add and remove sets -> no conflicts.  Also covers the
        // key-vs-script distinction: same 28-byte hash, different
        // credential type -> NOT a conflict (mirrors Haskell `Credential`).
        let mut adds: BTreeMap<Credential, u64> = BTreeMap::new();
        adds.insert(cred_key(0x11), 100);
        // Same 28-byte hash as the add but as a Script credential.
        adds.insert(cred_script(0x11), 100);
        // Remove a key with the same 28 bytes but treated as Script:
        // the typed-hash representation must distinguish, so this is a
        // conflict only with the Script add (intentional).  We instead
        // use a totally unrelated credential to keep this test purely
        // about disjoint sets.
        let removes = vec![cred_key(0x99), cred_script(0x88)];
        let proposal = update_committee_proposal(adds, removes);

        assert!(
            committee_update_conflicts(&proposal).is_empty(),
            "Disjoint add/remove sets must produce no conflicts"
        );
    }

    #[test]
    fn test_committee_update_conflicts_skips_non_update_committee() {
        // A non-UpdateCommittee proposal must always produce no conflicts.
        let proposal = proposal_with_addr(return_addr_29_with_network(1));
        assert!(
            committee_update_conflicts(&proposal).is_empty(),
            "Non-UpdateCommittee proposals must short-circuit (empty vec)"
        );
    }

    // ---------------------------------------------------------------------------
    // committee_update_invalid_expiries — ExpirationEpochTooSmall (Conway GOV)
    //
    // Per Haskell `processProposal` (UpdateCommittee branch), every entry in
    // `members_to_add`'s `(credential, validUntil)` pairs must satisfy
    // `validUntil > currentEpoch`.  Boundary `validUntil == currentEpoch`
    // is rejected (the Haskell filter is `<= currentEpoch`).  Always
    // enforced (no Conway-bootstrap skip).  When `ctx.current_epoch` is
    // `None`, the predicate returns an empty vec (lenient default).
    // ---------------------------------------------------------------------------

    #[test]
    fn test_committee_update_expiry_equal_current_rejected() {
        // expiry == current_epoch (boundary) -> rejected per Haskell `<=`.
        let cred = cred_key(0x71);
        let mut adds: BTreeMap<Credential, u64> = BTreeMap::new();
        adds.insert(cred.clone(), 100);
        let proposal = update_committee_proposal(adds, vec![]);
        let ctx = ValidationContext::new().with_epoch(100);

        let invalid = committee_update_invalid_expiries(&proposal, &ctx);
        assert_eq!(invalid.len(), 1, "boundary expiry must be invalid");
        assert_eq!(invalid[0].0, cred.to_typed_hash32().to_hex());
        assert_eq!(invalid[0].1, 100);
    }

    #[test]
    fn test_committee_update_expiry_below_current_rejected() {
        // expiry < current_epoch -> rejected.
        let cred = cred_key(0x72);
        let mut adds: BTreeMap<Credential, u64> = BTreeMap::new();
        adds.insert(cred.clone(), 50);
        let proposal = update_committee_proposal(adds, vec![]);
        let ctx = ValidationContext::new().with_epoch(100);

        let invalid = committee_update_invalid_expiries(&proposal, &ctx);
        assert_eq!(invalid.len(), 1, "expiry below current must be invalid");
        assert_eq!(invalid[0].1, 50);
    }

    #[test]
    fn test_committee_update_expiry_above_current_accepted() {
        // expiry > current_epoch -> accepted (no entries surfaced).
        let cred = cred_key(0x73);
        let mut adds: BTreeMap<Credential, u64> = BTreeMap::new();
        adds.insert(cred, 200);
        let proposal = update_committee_proposal(adds, vec![]);
        let ctx = ValidationContext::new().with_epoch(100);

        assert!(
            committee_update_invalid_expiries(&proposal, &ctx).is_empty(),
            "expiry strictly greater than current_epoch must be accepted"
        );
    }

    #[test]
    fn test_committee_update_expiry_skipped_when_epoch_none() {
        // ctx.current_epoch = None -> lenient default (empty vec) regardless
        // of the expiry values.
        let cred = cred_key(0x74);
        let mut adds: BTreeMap<Credential, u64> = BTreeMap::new();
        adds.insert(cred, 0); // would be invalid if epoch were known
        let proposal = update_committee_proposal(adds, vec![]);
        let ctx = ValidationContext::new();
        assert!(ctx.current_epoch.is_none());

        assert!(
            committee_update_invalid_expiries(&proposal, &ctx).is_empty(),
            "Predicate must skip (empty vec) when current_epoch is None"
        );
    }

    // ── #1026: PV9 bootstrap proposal-SUBMISSION restriction ──────────────
    //
    // Haskell `checkBootstrapProposal` (step 1 of `processProposal`) restricts
    // PROPOSAL submission at PV9 to the same action set the VOTE side already
    // restricted, via the identical `isBootstrapAction` predicate. dugite had
    // only the vote side, i.e. accept-where-Haskell-rejects.

    /// `isBootstrapAction` — allowed set is exactly ParameterChange /
    /// HardForkInitiation / InfoAction.
    #[test]
    fn is_bootstrap_action_matches_haskell_allowed_set() {
        assert!(is_bootstrap_action(&GovAction::InfoAction));
        assert!(is_bootstrap_action(&GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (10, 0),
        }));

        // Everything else is disallowed during bootstrap.
        assert!(!is_bootstrap_action(&GovAction::NoConfidence {
            prev_action_id: None
        }));
        assert!(!is_bootstrap_action(&GovAction::TreasuryWithdrawals {
            withdrawals: std::collections::BTreeMap::new(),
            policy_hash: None,
        }));
        assert!(!is_bootstrap_action(&GovAction::NewConstitution {
            prev_action_id: None,
            constitution: dugite_primitives::transaction::Constitution {
                anchor: dugite_primitives::transaction::Anchor {
                    url: "https://c".to_string(),
                    data_hash: dugite_primitives::hash::Hash32::from_bytes([0x05; 32]),
                },
                script_hash: None,
            },
        }));
    }

    /// The proposal-side predicate is the exact negation, and — critically —
    /// shares its classification with the vote side, so the two cannot drift.
    /// Haskell gates both on one `isBootstrapAction`.
    #[test]
    fn bootstrap_proposal_and_vote_restrictions_share_one_classification() {
        let disallowed = GovAction::TreasuryWithdrawals {
            withdrawals: std::collections::BTreeMap::new(),
            policy_hash: None,
        };
        let allowed = GovAction::InfoAction;

        assert!(is_bootstrap_proposal_disallowed(&disallowed));
        assert!(!is_bootstrap_proposal_disallowed(&allowed));

        // Same action set drives the SPO/CC vote restriction: a non-bootstrap
        // action is vote-disallowed for those voters too. If a future edit
        // changed one side's set, this pairing fails.
        let spo = Voter::StakePool(dugite_primitives::hash::Hash32::from_bytes([7u8; 32]));
        assert_eq!(
            is_bootstrap_proposal_disallowed(&disallowed),
            is_bootstrap_vote_disallowed(&spo, &disallowed),
            "proposal and SPO-vote restrictions must agree on a non-bootstrap action"
        );
        assert_eq!(
            is_bootstrap_proposal_disallowed(&allowed),
            is_bootstrap_vote_disallowed(&spo, &allowed),
            "proposal and SPO-vote restrictions must agree on a bootstrap action"
        );
    }

    /// A HardForkInitiation proposal must stay PROPOSABLE at PV9 — that is the
    /// whole point of the bootstrap phase, and rejecting it would break the
    /// PV9→PV10 hard fork itself. Guards against an over-broad fix.
    #[test]
    fn hardfork_initiation_stays_proposable_during_bootstrap() {
        assert!(!is_bootstrap_proposal_disallowed(
            &GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (10, 0),
            }
        ));
    }
}
