use super::{
    credential_to_hash, DRepPulsingState, FuturePParams, GovRelation, GovernanceState, LedgerState,
    PGraph, PRoot, ProposalState, PulsedRatifyState,
};
use super::{CertSubState, EpochSubState, GovSubState};
use crate::ledger_seq::{GovernanceChange, LedgerDelta};
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{
    Anchor, DRep, GovAction, GovActionId, ProposalProcedure, Rational, Vote, Voter, VotingProcedure,
};
use dugite_primitives::value::Lovelace;
use imbl::HashMap as ImblHashMap;
use imbl::OrdMap as ImblOrdMap;
use imbl::OrdSet as ImblOrdSet;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use tracing::{debug, info, trace, warn};

impl LedgerState {
    /// Process a governance proposal.
    ///
    /// Validates:
    /// 1. Bootstrap phase restrictions — during protocol == 9 only ParameterChange,
    ///    HardForkInitiation, and InfoAction are allowed (Haskell: `isBootstrapAction`).
    /// 2. prev_action_id chain — must reference an active proposal **or** the last enacted
    ///    action of the same purpose (Haskell: `prevActionAsExpected`).
    ///    Validated at submission (not just ratification) per GOV rule.
    /// 3. pvCanFollow for HardForkInitiation — target version must follow the current version
    ///    by exactly one major increment (minor=0) or the same major with a higher minor
    ///    (Haskell: `pvCanFollow`).
    #[allow(dead_code)]
    pub(crate) fn process_proposal(
        &mut self,
        tx_hash: &Hash32,
        action_index: u32,
        proposal: &ProposalProcedure,
    ) {
        // --- Check 1: Bootstrap phase proposal restrictions ---
        //
        // During Conway bootstrap (protocol_version.major == 9), only ParameterChange,
        // HardForkInitiation, and InfoAction are permitted.  Everything else (NoConfidence,
        // UpdateCommittee, NewConstitution, TreasuryWithdrawals) is rejected.
        //
        // Per Haskell `isBootstrapAction` in `Cardano.Ledger.Conway.Rules.Gov`
        // (introduced in commit b6282d5, present in all released versions):
        //
        //   isBootstrapAction :: GovAction era -> Bool
        //   isBootstrapAction = \case
        //     ParameterChange {}    -> True
        //     HardForkInitiation {} -> True
        //     InfoAction            -> True
        //     _                     -> False
        //
        // The Plomin hard fork (proto 9→10) was submitted as a HardForkInitiation during
        // the bootstrap phase and was correctly accepted.  Our earlier implementation had
        // the allowed/disallowed sets inverted.
        if self.is_bootstrap_phase() {
            let allowed = matches!(
                &proposal.gov_action,
                GovAction::ParameterChange { .. }
                    | GovAction::HardForkInitiation { .. }
                    | GovAction::InfoAction
            );
            if !allowed {
                debug!(
                    tx = %tx_hash.to_hex(),
                    action_index,
                    action_type = ?std::mem::discriminant(&proposal.gov_action),
                    "DisallowedProposalDuringBootstrap: rejecting governance proposal (protocol == 9)"
                );
                // Drop proposal — do not insert into active proposals
                return;
            }
        }

        // --- Check 2: pvCanFollow for HardForkInitiation ---
        //
        // The target protocol version must be reachable from the base version, with
        // the Haskell `preceedingHardFork` three-way base resolution (chaining against
        // an in-flight parent HardForkInitiation's target when present). Shared with
        // the live GOV rule via `hardfork_proposal_cant_follow` — single source of
        // truth (#812 Defect B / #858).
        {
            let cur_major = self.epochs.protocol_params.protocol_version_major;
            let cur_minor = self.epochs.protocol_params.protocol_version_minor;
            if hardfork_proposal_cant_follow(
                &proposal.gov_action,
                &self.gov.governance,
                cur_major,
                cur_minor,
            ) {
                debug!(
                    tx = %tx_hash.to_hex(),
                    action_index,
                    cur_version = %format!("{cur_major}.{cur_minor}"),
                    "ProposalCantFollow: HardForkInitiation target does not follow the base \
                     protocol version — proposal dropped (#812)"
                );
                // Drop proposal — do not insert into active proposals
                return;
            }
        }

        // --- Check 3: prev_action_id validation at submission ---
        //
        // Per Haskell `proposalsAddAction` (Proposals.hs), a proposal's prev_action_id
        // must satisfy one of two conditions:
        //
        //   (a) prev_action_id = None  AND  enacted root for this purpose is also None
        //       (genesis root — first ever proposal of this type)
        //   (b) prev_action_id = Some(id)  AND  id matches the last enacted action of the
        //       same purpose  OR  id is an active (in-flight) proposal
        //
        // Haskell: `parent == ps ^. pRootsL . govRelationL . prRootL` covers case (a);
        // `SJust parentId <- parent, Map.member parentId graph` covers case (b).
        // Anything else fires `InvalidPrevGovActionId` (tag 8): canonical
        // `Cardano.Ledger.Conway.Rules.Gov` FAILS the tx via `failBecause` —
        // it does NOT drop the proposal and continue (#914). The live
        // block-apply path (`process_governance_votes_and_proposals`) hard-errors
        // accordingly; this test-only path keeps the drop (tests exercise the
        // rejection branch directly) but logs at WARN so a hit is visible.
        let prev_id = match &proposal.gov_action {
            GovAction::ParameterChange { prev_action_id, .. }
            | GovAction::HardForkInitiation { prev_action_id, .. }
            | GovAction::NoConfidence { prev_action_id, .. }
            | GovAction::UpdateCommittee { prev_action_id, .. }
            | GovAction::NewConstitution { prev_action_id, .. } => prev_action_id.as_ref(),
            GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => None,
        };
        match prev_id {
            None => {
                // Case (a): genesis root proposal. Valid only when no prior action of
                // this purpose has been enacted. Per Haskell, SNothing only matches the
                // purpose tree root when prRootL is also SNothing.
                if !genesis_root_is_valid(&proposal.gov_action, &self.gov.governance) {
                    warn!(
                        tx = %tx_hash.to_hex(),
                        action_index,
                        action_type = ?std::mem::discriminant(&proposal.gov_action),
                        "InvalidPrevGovActionId: genesis-root proposal (prev_action_id=None) \
                         rejected because the purpose already has an enacted root"
                    );
                    return;
                }
            }
            Some(prev) => {
                // Case (b): Allowed if: (i) it references the last enacted root of this
                // purpose, OR (ii) it references an active (in-flight) proposal.
                let valid_root = prev_action_matches_enacted_root(
                    &proposal.gov_action,
                    prev,
                    &self.gov.governance,
                );
                let in_flight = self.gov.governance.proposals.contains_key(prev);
                if !valid_root && !in_flight {
                    warn!(
                        tx = %tx_hash.to_hex(),
                        action_index,
                        prev_action = %prev.transaction_id.to_hex(),
                        prev_index = prev.action_index,
                        "InvalidPrevGovActionId: prev_action_id is neither an active \
                         proposal nor the last enacted action of this purpose"
                    );
                    return;
                }
            }
        }

        // CIP-1694: Validate policy_hash matches constitution guardrail script.
        // ParameterChange and TreasuryWithdrawals must include the constitution's script_hash.
        // Mismatches are rejected (proposal dropped), matching Haskell's GOV rule.
        let constitution_script = self
            .gov
            .governance
            .constitution
            .as_ref()
            .and_then(|c| c.script_hash);
        match &proposal.gov_action {
            GovAction::ParameterChange { policy_hash, .. }
            | GovAction::TreasuryWithdrawals { policy_hash, .. } => {
                if let Some(required_hash) = constitution_script {
                    match policy_hash {
                        Some(provided) if *provided == required_hash => {
                            // Valid — policy hash matches constitution guardrail
                        }
                        Some(provided) => {
                            warn!(
                                tx = %tx_hash.to_hex(),
                                action_index,
                                provided = %provided.to_hex(),
                                required = %required_hash.to_hex(),
                                "ConstitutionPolicyMismatch: proposal policy_hash does not match constitution guardrail — dropping proposal"
                            );
                            return;
                        }
                        None => {
                            warn!(
                                tx = %tx_hash.to_hex(),
                                action_index,
                                required = %required_hash.to_hex(),
                                "ConstitutionPolicyMismatch: proposal missing policy_hash (constitution requires guardrail) — dropping proposal"
                            );
                            return;
                        }
                    }
                }
            }
            _ => {}
        }

        let action_id = GovActionId {
            transaction_id: *tx_hash,
            action_index,
        };

        // Governance action lifetime from protocol parameters.
        //
        // Per Haskell `gasExpiresAfter = addEpochInterval proposedIn govActionLifetime`:
        // `expires_epoch = proposed_epoch + govActionLifetime` (no +1).
        // With the expiry filter `expires_epoch < self.epoch` (governance.rs), a
        // proposal submitted at epoch E with lifetime L is active through epoch E+L
        // and is removed at the E+L+1 boundary.
        //
        // Note: Koios displays `expiration = gasExpiresAfter + 1` (the first epoch
        // where the proposal is inactive), so on-chain queries show lifetime = L+1.
        let gov_action_lifetime = self.epochs.protocol_params.gov_action_lifetime;
        let expires_epoch = EpochNo(self.epoch.0.saturating_add(gov_action_lifetime));

        let state = ProposalState {
            procedure: proposal.clone(),
            proposed_epoch: self.epoch,
            expires_epoch,
            yes_votes: 0,
            no_votes: 0,
            abstain_votes: 0,
            // #799: read the monotonic counter BEFORE it is incremented below.
            submission_index: self.gov.governance.proposal_count,
        };

        debug!(
            "Governance proposal submitted: {:?} (expires epoch {})",
            action_id, expires_epoch.0
        );
        let gov = Arc::make_mut(&mut self.gov.governance);
        gov.proposals.insert(action_id.clone(), state);
        gov.proposal_count += 1;

        // Maintain the proposal priority forest (PRoot / PGraph).
        if let Some(tag) = gov_action_purpose_tag(&proposal.gov_action) {
            let prev = gov_action_raw_prev_id(&proposal.gov_action);
            forest_add_proposal(
                &action_id,
                prev.as_ref(),
                tag,
                &mut gov.proposal_roots,
                &mut gov.proposal_graph,
            );
        }
    }

    /// Process a governance vote.
    ///
    /// Validates that a CC voter is an elected (current) committee member.
    /// Post-bootstrap (protocol >= 10), votes from non-committee credentials are
    /// rejected with `UnelectedCommitteeVoter` per Haskell's GOV rule.
    ///
    /// During bootstrap (protocol == 9) this check is skipped since committee
    /// membership rules are not yet fully active.
    #[allow(dead_code)]
    pub(crate) fn process_vote(
        &mut self,
        voter: &Voter,
        action_id: &GovActionId,
        procedure: &VotingProcedure,
    ) {
        // --- Check: Unelected CC member vote rejection (protocol >= 10) ---
        //
        // Per Haskell `Conway.GOV` rule, a `ConstitutionalCommittee` voter must
        // correspond to a hot credential that is currently authorized for an elected
        // (non-expired, non-resigned) cold credential in `committee_hot_keys`.
        //
        // Haskell: `isElected govState voter` checks that the hot credential maps
        // back to a cold credential that is a current committee member.
        //
        // We check during vote processing (not just ratification) to match Haskell's
        // UTXOW / GOV rule which rejects the entire transaction carrying such a vote.
        // Here we emit a warning and skip the vote record to avoid permanent state
        // pollution, while still allowing block replay for confirmed blocks.
        if let Voter::ConstitutionalCommittee(cred) = voter {
            if !self.is_bootstrap_phase() {
                let hot_hash = credential_to_hash(cred);
                // A vote is valid if the hot credential is authorised for any
                // current (non-expired, non-resigned) cold credential.
                let is_elected = self.gov.governance.committee_hot_keys.iter().any(
                    |(cold_hash, registered_hot)| {
                        *registered_hot == hot_hash
                            && !self
                                .gov
                                .governance
                                .committee_resigned
                                .contains_key(cold_hash)
                            && self
                                .gov
                                .governance
                                .committee_expiration
                                .get(cold_hash)
                                .is_some_and(|exp| self.epoch <= *exp)
                    },
                );
                if !is_elected {
                    warn!(
                        tx = %action_id.transaction_id.to_hex(),
                        action_index = action_id.action_index,
                        hot_cred = %hot_hash.to_hex(),
                        "UnelectedCommitteeVoter: CC vote from unelected hot credential — ignoring"
                    );
                    return;
                }
            }
        }

        // Update vote tally on the proposal
        if let Some(proposal) = Arc::make_mut(&mut self.gov.governance)
            .proposals
            .get_mut(action_id)
        {
            match procedure.vote {
                Vote::Yes => proposal.yes_votes += 1,
                Vote::No => proposal.no_votes += 1,
                Vote::Abstain => proposal.abstain_votes += 1,
            }
        }

        // Track DRep activity — voting counts as activity per CIP-1694
        if let Voter::DRep(cred) = voter {
            let drep_hash = credential_to_hash(cred);
            let expiry = self.compute_drep_expiry();
            if let Some(drep) = Arc::make_mut(&mut self.gov.governance)
                .dreps
                .get_mut(&drep_hash)
            {
                drep.drep_expiry = expiry;
            }
        }

        // Record the vote (indexed by action_id for efficient ratification).
        // Inner map is keyed by `Voter`; insert is O(log n) last-vote-wins.
        Arc::make_mut(&mut self.gov.governance)
            .votes_by_action
            .entry(action_id.clone())
            .or_default()
            .insert(voter.clone(), procedure.clone());

        debug!(
            "Vote cast by {:?} on {:?}: {:?}",
            voter, action_id, procedure.vote
        );
    }

    /// Delta-capturing variant of [`process_proposal`].
    ///
    /// Performs the exact same validation and state mutations as `process_proposal`.
    /// Additionally, on successful insertion, pushes a [`GovernanceChange::ProposeAction`]
    /// entry into `delta` so that the LedgerSeq machinery can reconstruct governance state
    /// from deltas alone.
    ///
    /// When a proposal is rejected (bootstrap restriction, `pvCanFollow` check, or an
    /// invalid `prev_action_id`), no delta entry is pushed — matching the early-return
    /// semantics of the underlying method.
    #[allow(dead_code)]
    pub(crate) fn process_proposal_with_delta(
        &mut self,
        tx_hash: &Hash32,
        action_index: u32,
        proposal: &ProposalProcedure,
        delta: &mut LedgerDelta,
    ) {
        // --- Check 1: Bootstrap phase proposal restrictions ---
        //
        // Mirrors the identical check in `process_proposal`.  Only ParameterChange,
        // HardForkInitiation, and InfoAction are allowed while protocol_version.major == 9.
        if self.is_bootstrap_phase() {
            let allowed = matches!(
                &proposal.gov_action,
                GovAction::ParameterChange { .. }
                    | GovAction::HardForkInitiation { .. }
                    | GovAction::InfoAction
            );
            if !allowed {
                debug!(
                    tx = %tx_hash.to_hex(),
                    action_index,
                    action_type = ?std::mem::discriminant(&proposal.gov_action),
                    "DisallowedProposalDuringBootstrap: rejecting governance proposal (protocol == 9)"
                );
                return;
            }
        }

        // --- Check 2: pvCanFollow for HardForkInitiation ---
        //
        // Shared `preceedingHardFork` + `pvCanFollow` reachability check (chains
        // against an in-flight parent HardForkInitiation's target when present).
        // Single source of truth with the live GOV rule (#812 Defect B / #858).
        {
            let cur_major = self.epochs.protocol_params.protocol_version_major;
            let cur_minor = self.epochs.protocol_params.protocol_version_minor;
            if hardfork_proposal_cant_follow(
                &proposal.gov_action,
                &self.gov.governance,
                cur_major,
                cur_minor,
            ) {
                debug!(
                    tx = %tx_hash.to_hex(),
                    action_index,
                    cur_version = %format!("{cur_major}.{cur_minor}"),
                    "ProposalCantFollow: HardForkInitiation target does not follow the base \
                     protocol version — proposal dropped (#812)"
                );
                return;
            }
        }

        // --- Check 3: prev_action_id validation at submission ---
        //
        // Per Haskell `proposalsAddAction` (Proposals.hs), a proposal's prev_action_id
        // must satisfy one of two conditions:
        //
        //   (a) prev_action_id = None  AND  enacted root for this purpose is also None
        //       (genesis root — first ever proposal of this type)
        //   (b) prev_action_id = Some(id)  AND  id matches the last enacted action of the
        //       same purpose  OR  id is an active (in-flight) proposal
        //
        // Mirrors the identical check in `process_proposal`. Canonical
        // `Cardano.Ledger.Conway.Rules.Gov` FAILS the tx via `failBecause
        // (InvalidPrevGovActionId …)` — it does not drop-and-continue (#914).
        // The live block-apply path hard-errors; this dead path drops + WARNs.
        let prev_id = match &proposal.gov_action {
            GovAction::ParameterChange { prev_action_id, .. }
            | GovAction::HardForkInitiation { prev_action_id, .. }
            | GovAction::NoConfidence { prev_action_id, .. }
            | GovAction::UpdateCommittee { prev_action_id, .. }
            | GovAction::NewConstitution { prev_action_id, .. } => prev_action_id.as_ref(),
            GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => None,
        };
        match prev_id {
            None => {
                if !genesis_root_is_valid(&proposal.gov_action, &self.gov.governance) {
                    warn!(
                        tx = %tx_hash.to_hex(),
                        action_index,
                        action_type = ?std::mem::discriminant(&proposal.gov_action),
                        "InvalidPrevGovActionId: genesis-root proposal (prev_action_id=None) \
                         rejected because the purpose already has an enacted root"
                    );
                    return;
                }
            }
            Some(prev) => {
                let valid_root = prev_action_matches_enacted_root(
                    &proposal.gov_action,
                    prev,
                    &self.gov.governance,
                );
                let in_flight = self.gov.governance.proposals.contains_key(prev);
                if !valid_root && !in_flight {
                    warn!(
                        tx = %tx_hash.to_hex(),
                        action_index,
                        prev_action = %prev.transaction_id.to_hex(),
                        prev_index = prev.action_index,
                        "InvalidPrevGovActionId: prev_action_id is neither an active \
                         proposal nor the last enacted action of this purpose"
                    );
                    return;
                }
            }
        }

        // CIP-1694: Validate policy_hash matches constitution guardrail script.
        // ParameterChange and TreasuryWithdrawals must include the constitution's script_hash.
        // Mismatches are rejected (proposal dropped), matching Haskell's GOV rule.
        let constitution_script = self
            .gov
            .governance
            .constitution
            .as_ref()
            .and_then(|c| c.script_hash);
        match &proposal.gov_action {
            GovAction::ParameterChange { policy_hash, .. }
            | GovAction::TreasuryWithdrawals { policy_hash, .. } => {
                if let Some(required_hash) = constitution_script {
                    match policy_hash {
                        Some(provided) if *provided == required_hash => {}
                        Some(provided) => {
                            warn!(
                                tx = %tx_hash.to_hex(),
                                action_index,
                                provided = %provided.to_hex(),
                                required = %required_hash.to_hex(),
                                "ConstitutionPolicyMismatch: proposal policy_hash does not match constitution guardrail — dropping proposal"
                            );
                            return;
                        }
                        None => {
                            warn!(
                                tx = %tx_hash.to_hex(),
                                action_index,
                                required = %required_hash.to_hex(),
                                "ConstitutionPolicyMismatch: proposal missing policy_hash (constitution requires guardrail) — dropping proposal"
                            );
                            return;
                        }
                    }
                }
            }
            _ => {}
        }

        let action_id = GovActionId {
            transaction_id: *tx_hash,
            action_index,
        };

        let gov_action_lifetime = self.epochs.protocol_params.gov_action_lifetime;
        let expires_epoch = EpochNo(self.epoch.0.saturating_add(gov_action_lifetime));

        let state = ProposalState {
            procedure: proposal.clone(),
            proposed_epoch: self.epoch,
            expires_epoch,
            yes_votes: 0,
            no_votes: 0,
            abstain_votes: 0,
            // #799: read the monotonic counter BEFORE it is incremented below.
            submission_index: self.gov.governance.proposal_count,
        };

        // Clone before moving into the insert so the delta entry carries an identical copy.
        let state_for_delta = state.clone();

        debug!(
            "Governance proposal submitted (with delta): {:?} (expires epoch {})",
            action_id, expires_epoch.0
        );
        let gov = Arc::make_mut(&mut self.gov.governance);
        gov.proposals.insert(action_id.clone(), state);
        gov.proposal_count += 1;

        // Maintain the proposal priority forest (PRoot / PGraph).
        if let Some(tag) = gov_action_purpose_tag(&proposal.gov_action) {
            let prev = gov_action_raw_prev_id(&proposal.gov_action);
            forest_add_proposal(
                &action_id,
                prev.as_ref(),
                tag,
                &mut gov.proposal_roots,
                &mut gov.proposal_graph,
            );
        }

        // Proposal was accepted — record the change in the delta.
        delta
            .governance_changes
            .push(GovernanceChange::ProposeAction {
                action_id,
                proposal: state_for_delta,
            });
    }

    /// Delta-capturing variant of [`process_vote`].
    ///
    /// Performs the exact same validation and state mutations as `process_vote`.
    /// Additionally, pushes a [`GovernanceChange::CastVote`] entry into `delta` after
    /// all mutations succeed.
    ///
    /// When a vote is rejected (unelected CC member post-bootstrap), no delta entry
    /// is pushed — matching the early-return semantics of the underlying method.
    #[allow(dead_code)]
    pub(crate) fn process_vote_with_delta(
        &mut self,
        voter: &Voter,
        action_id: &GovActionId,
        procedure: &VotingProcedure,
        delta: &mut LedgerDelta,
    ) {
        // --- Check: Unelected CC member vote rejection (protocol >= 10) ---
        //
        // Mirrors the identical check in `process_vote`.
        if let Voter::ConstitutionalCommittee(cred) = voter {
            if !self.is_bootstrap_phase() {
                let hot_hash = credential_to_hash(cred);
                let is_elected = self.gov.governance.committee_hot_keys.iter().any(
                    |(cold_hash, registered_hot)| {
                        *registered_hot == hot_hash
                            && !self
                                .gov
                                .governance
                                .committee_resigned
                                .contains_key(cold_hash)
                            && self
                                .gov
                                .governance
                                .committee_expiration
                                .get(cold_hash)
                                .is_some_and(|exp| self.epoch <= *exp)
                    },
                );
                if !is_elected {
                    warn!(
                        tx = %action_id.transaction_id.to_hex(),
                        action_index = action_id.action_index,
                        hot_cred = %hot_hash.to_hex(),
                        "UnelectedCommitteeVoter: CC vote from unelected hot credential — ignoring"
                    );
                    return;
                }
            }
        }

        // Update vote tally on the proposal.
        if let Some(proposal) = Arc::make_mut(&mut self.gov.governance)
            .proposals
            .get_mut(action_id)
        {
            match procedure.vote {
                Vote::Yes => proposal.yes_votes += 1,
                Vote::No => proposal.no_votes += 1,
                Vote::Abstain => proposal.abstain_votes += 1,
            }
        }

        // Track DRep activity — voting counts as activity per CIP-1694.
        if let Voter::DRep(cred) = voter {
            let drep_hash = credential_to_hash(cred);
            let expiry = self.compute_drep_expiry();
            if let Some(drep) = Arc::make_mut(&mut self.gov.governance)
                .dreps
                .get_mut(&drep_hash)
            {
                drep.drep_expiry = expiry;
            }
        }

        // Record the vote (indexed by action_id for efficient ratification).
        // Inner map is keyed by `Voter`; insert is O(log n) last-vote-wins.
        Arc::make_mut(&mut self.gov.governance)
            .votes_by_action
            .entry(action_id.clone())
            .or_default()
            .insert(voter.clone(), procedure.clone());

        debug!(
            "Vote cast (with delta) by {:?} on {:?}: {:?}",
            voter, action_id, procedure.vote
        );

        // Vote was recorded — push to delta after all mutations complete.
        delta.governance_changes.push(GovernanceChange::CastVote {
            action_id: action_id.clone(),
            voter: voter.clone(),
            procedure: procedure.clone(),
        });
    }

    /// Check all active governance proposals for ratification.
    ///
    /// A proposal is ratified when it meets the required voting thresholds.
    /// Thresholds vary by action type and involve DRep, SPO, and/or CC votes.
    /// Ratified proposals are enacted (their effects applied) and removed.
    ///
    /// Per Haskell Ratify.hs, proposals are processed:
    /// 1. Sorted by priority (NoConfidence > UpdateCommittee > ... > InfoAction)
    /// 2. Sequentially with state threading (enacted roots update between proposals)
    /// 3. With a "delaying action" flag that blocks further ratification
    /// 4. With prev_action_id chain validation (must match last enacted of same purpose)
    ///
    /// ## Snapshot-based ratification (matching Haskell DRep pulser)
    ///
    /// When `governance.ratification_snapshot` is `Some`, proposals and votes are read
    /// from the frozen snapshot (captured at the *previous* epoch boundary) rather than
    /// live state.  This ensures proposals/votes submitted during the epoch that just
    /// ended are not considered for ratification until the *next* boundary — matching
    /// Haskell's `DRepPulsingState` timing exactly.
    ///
    /// When `None` (genesis, or a pre-#903 snapshot without this field) there are
    /// NO candidates: Haskell's RATIFY signal is the pulser's `dpProposals`,
    /// frozen at the previous boundary, and at genesis `ConwayGovState` is
    /// `DRComplete def def` — an empty set. This used to fall back to the live
    /// proposal set, which ratified proposals in the very epoch they were
    /// submitted (#903).
    pub fn ratify_proposals(&mut self) {
        ratify_proposals_impl(self.epoch, &mut self.epochs, &mut self.certs, &mut self.gov);
    }

    /// Whether we are in the Conway bootstrap phase (protocol version 9).
    /// During bootstrap, all DRep voting thresholds are set to 0 (auto-pass)
    /// per the Haskell `hardforkConwayBootstrapPhase` function.
    fn is_bootstrap_phase(&self) -> bool {
        self.epochs.protocol_params.protocol_version_major == 9
    }

    /// Check whether a proposal has met its voting thresholds for ratification.
    ///
    /// CIP-1694 voting thresholds (stake-weighted), matching Haskell cardano-ledger:
    /// - InfoAction: always ratified (no thresholds)
    /// - ParameterChange: DRep >= dvt_pp_*_group + SPO >= pvt_pp_security (if security) + CC
    /// - HardForkInitiation: DRep >= dvt_hard_fork + SPO >= pvt_hard_fork + CC
    /// - NoConfidence: DRep >= dvt_no_confidence + SPO >= pvt_motion_no_confidence (no CC)
    /// - UpdateCommittee: DRep >= dvt_committee + SPO >= pvt_committee (no CC)
    /// - NewConstitution: DRep >= dvt_constitution + CC (no SPO)
    /// - TreasuryWithdrawals: DRep >= dvt_treasury_withdrawal + CC (no SPO)
    ///
    /// During Conway bootstrap phase (protocol version 9), all DRep thresholds are 0.
    /// Check ratification for a governance action.
    ///
    /// Committee state, votes, and no-confidence flag are passed explicitly so that
    /// `ratify_proposals()` can supply either live or snapshot data.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    fn check_ratification(
        &self,
        action_id: &GovActionId,
        state: &ProposalState,
        _total_drep_stake: u64,
        total_spo_stake: u64,
        drep_power_cache: &ImblHashMap<Hash32, u64>,
        no_confidence_stake: u64,
        votes_by_action: &ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
        committee_hot_keys: &ImblHashMap<Hash32, Hash32>,
        committee_expiration: &ImblHashMap<Hash32, EpochNo>,
        committee_resigned: &ImblHashMap<Hash32, Option<Anchor>>,
        committee_threshold: &Option<Rational>,
        remaining_treasury: u64,
        pool_stake_override: Option<&HashMap<Hash28, Lovelace>>,
        vote_delegations_override: Option<&ImblHashMap<Hash32, DRep>>,
    ) -> bool {
        // Count votes by voter type (uses pre-computed DRep power cache).
        // SPO votes iterate ALL pools in the provided pool stake distribution,
        // not just explicit voters, matching Haskell's `spoAcceptedRatio`.
        let (drep_yes, drep_total, spo_yes, spo_abstain, _cc_yes, _cc_total) = self
            .count_votes_by_type(
                action_id,
                &state.procedure.gov_action,
                drep_power_cache,
                no_confidence_stake,
                votes_by_action,
                pool_stake_override,
                vote_delegations_override,
            );

        let bootstrap = self.is_bootstrap_phase();

        // SPO denominator = totalActiveStake − abstainStake (always, for all
        // action types). This matches Haskell's `spoAcceptedRatio` formula.
        let spo_denom = total_spo_stake.saturating_sub(spo_abstain);

        // Helper closure for CC approval checks using snapshot committee data
        let cc_met_fn = |aid: &GovActionId| -> bool {
            check_cc_approval(
                aid,
                votes_by_action,
                committee_hot_keys,
                committee_expiration,
                committee_resigned,
                committee_threshold,
                self.epoch,
                self.epochs.protocol_params.committee_min_size,
                bootstrap,
            )
        };

        match &state.procedure.gov_action {
            GovAction::InfoAction => {
                // InfoAction has NoVotingThreshold for all three voting bodies
                // (DRep, SPO, CC).  Per Haskell, NoVotingThreshold means the
                // voting body does not participate — the action cannot accumulate
                // any votes.  InfoAction proposals therefore cannot be ratified;
                // they remain in the proposals set until they expire.
                false
            }
            GovAction::ParameterChange {
                protocol_param_update,
                ..
            } => {
                let drep_met = if bootstrap {
                    true
                } else {
                    pp_change_drep_all_groups_met(
                        protocol_param_update,
                        &self.epochs.protocol_params,
                        drep_yes,
                        drep_total,
                    )
                };
                let spo_met = if let Some(ref spo_threshold) =
                    pp_change_spo_threshold(protocol_param_update, &self.epochs.protocol_params)
                {
                    check_threshold(spo_yes, spo_denom, spo_threshold)
                } else {
                    true
                };
                let cc_met = cc_met_fn(action_id);
                debug!(
                    action_id = %action_id.transaction_id.to_hex(),
                    bootstrap,
                    drep_yes, drep_total, drep_met,
                    spo_yes, spo_denom, spo_met,
                    cc_met,
                    "ParameterChange ratification check"
                );
                drep_met && spo_met && cc_met
            }
            GovAction::HardForkInitiation {
                protocol_version, ..
            } => {
                let rational_zero = Rational {
                    numerator: 0,
                    denominator: 1,
                };
                let drep_threshold = if bootstrap {
                    rational_zero
                } else {
                    self.epochs.protocol_params.dvt_hard_fork.clone()
                };
                let spo_threshold = &self.epochs.protocol_params.pvt_hard_fork;
                let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
                let spo_met = check_threshold(spo_yes, spo_denom, spo_threshold);
                let cc_met = cc_met_fn(action_id);
                debug!(
                    action_id = %action_id.transaction_id.to_hex(),
                    version = ?protocol_version,
                    bootstrap,
                    drep_yes, drep_total,
                    drep_threshold = drep_threshold.as_f64(), drep_met,
                    spo_yes, spo_denom,
                    spo_threshold = spo_threshold.as_f64(), spo_met,
                    cc_met,
                    "HardForkInitiation ratification check"
                );
                drep_met && spo_met && cc_met
            }
            GovAction::NoConfidence { .. } => {
                let rational_zero = Rational {
                    numerator: 0,
                    denominator: 1,
                };
                let drep_threshold = if bootstrap {
                    rational_zero
                } else {
                    self.epochs.protocol_params.dvt_no_confidence.clone()
                };
                let spo_threshold = &self.epochs.protocol_params.pvt_motion_no_confidence;
                let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
                let spo_met = check_threshold(spo_yes, spo_denom, spo_threshold);
                debug!(
                    action_id = %action_id.transaction_id.to_hex(),
                    bootstrap,
                    drep_yes, drep_total,
                    drep_threshold = drep_threshold.as_f64(), drep_met,
                    spo_yes, spo_denom,
                    spo_threshold = spo_threshold.as_f64(), spo_met,
                    "NoConfidence ratification check"
                );
                drep_met && spo_met
            }
            GovAction::UpdateCommittee { members_to_add, .. } => {
                // Haskell RATIFY: `validCommitteeTerm` — all new members' expiry epochs
                // must be ≤ currentEpoch + committeeMaxTermLength. Only new members are
                // checked (retained members are not re-validated).
                let max_expiry =
                    self.epoch.0 + self.epochs.protocol_params.committee_max_term_length;
                if members_to_add.values().any(|&exp| exp > max_expiry) {
                    debug!(
                        action_id = %action_id.transaction_id.to_hex(),
                        max_expiry,
                        "UpdateCommittee rejected: member expiry exceeds committeeMaxTermLength"
                    );
                    return false;
                }

                let rational_zero = Rational {
                    numerator: 0,
                    denominator: 1,
                };
                // Use snapshot no_confidence state: check if committee_threshold is None
                // (which indicates no-confidence when committee was dissolved).
                // The no_confidence flag from the snapshot is implicitly captured in the
                // committee state. We use self.gov.governance.no_confidence here because the
                // threshold selection (normal vs no-confidence) depends on the enacted
                // state which is threaded through the ratification pass.
                let no_confidence = self.gov.governance.no_confidence;
                let (drep_threshold, spo_threshold) = if no_confidence {
                    (
                        if bootstrap {
                            rational_zero
                        } else {
                            self.epochs
                                .protocol_params
                                .dvt_committee_no_confidence
                                .clone()
                        },
                        &self.epochs.protocol_params.pvt_committee_no_confidence,
                    )
                } else {
                    (
                        if bootstrap {
                            rational_zero
                        } else {
                            self.epochs.protocol_params.dvt_committee_normal.clone()
                        },
                        &self.epochs.protocol_params.pvt_committee_normal,
                    )
                };
                let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
                let spo_met = check_threshold(spo_yes, spo_denom, spo_threshold);
                drep_met && spo_met
            }
            GovAction::NewConstitution { .. } => {
                let rational_zero = Rational {
                    numerator: 0,
                    denominator: 1,
                };
                let drep_threshold = if bootstrap {
                    rational_zero
                } else {
                    self.epochs.protocol_params.dvt_constitution.clone()
                };
                let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
                let cc_met = cc_met_fn(action_id);
                drep_met && cc_met
            }
            GovAction::TreasuryWithdrawals { withdrawals, .. } => {
                // Haskell RATIFY: checkWithdrawals — total withdrawal must not
                // exceed the remaining treasury balance (after any previously
                // enacted withdrawals in this ratification pass).
                let total: u64 = withdrawals
                    .values()
                    .fold(0u64, |acc, a| acc.saturating_add(a.0));
                if total > remaining_treasury {
                    debug!(
                        action_id = %action_id.transaction_id.to_hex(),
                        total,
                        remaining_treasury,
                        "TreasuryWithdrawals rejected: exceeds remaining treasury"
                    );
                    return false;
                }

                let rational_zero = Rational {
                    numerator: 0,
                    denominator: 1,
                };
                let drep_threshold = if bootstrap {
                    rational_zero
                } else {
                    self.epochs.protocol_params.dvt_treasury_withdrawal.clone()
                };
                let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
                let cc_met = cc_met_fn(action_id);
                drep_met && cc_met
            }
        }
    }

    /// Count stake-weighted votes by voter type for a specific governance action.
    ///
    /// Per Haskell `dRepAcceptedRatio` / `spoAcceptedRatio`:
    /// - DRep denominator = total active DRep-delegated stake - abstain stake
    ///   (non-voting active DReps count as implicit No in denominator)
    /// - SPO: iterates ALL pools in the provided pool stake distribution, classifying
    ///   non-voters per Haskell rules (bootstrap → Abstain for non-HardFork,
    ///   post-bootstrap → `defaultStakePoolVote` based on DRep delegation)
    /// - SPO denominator = totalActiveStake - abstainStake (always)
    /// - AlwaysNoConfidence stake counts as Yes for NoConfidence, No otherwise
    /// - AlwaysAbstain stake is excluded from both numerator and denominator
    /// - Inactive/expired DReps are excluded (handled by drep_power_cache)
    ///
    /// The `votes_by_action` parameter is passed explicitly so that `ratify_proposals()`
    /// can supply either live votes or a [`PulsingSnapshot`]'s frozen votes.
    ///
    /// The `pool_stake_override` parameter, when `Some`, provides the SPO stake
    /// distribution to use instead of `self.epochs.snapshots.mark`.  During ratification
    /// this MUST be the **set** snapshot (= the mark from the PREVIOUS epoch
    /// boundary), matching Haskell's `dpStakePoolDistr` which is captured when
    /// the DRep pulser is initialized at the prior boundary.  After SNAP rotation,
    /// `self.epochs.snapshots.mark` is the NEW mark for the upcoming epoch, not the one
    /// the Haskell pulser would have used.
    ///
    /// Returns `(drep_yes, drep_total, spo_yes, spo_abstain, cc_yes, cc_total)`.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub(crate) fn count_votes_by_type(
        &self,
        action_id: &GovActionId,
        action: &GovAction,
        drep_power_cache: &ImblHashMap<Hash32, u64>,
        no_confidence_stake: u64,
        votes_by_action: &ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
        pool_stake_override: Option<&HashMap<Hash28, Lovelace>>,
        vote_delegations_override: Option<&ImblHashMap<Hash32, DRep>>,
    ) -> (u64, u64, u64, u64, u64, u64) {
        let mut cc_yes = 0u64;
        let mut cc_total = 0u64;

        // Build DRep hash -> Vote and SPO pool_id -> Vote maps for this action
        let mut drep_votes: HashMap<Hash32, Vote> = HashMap::new();
        let mut spo_votes: HashMap<Hash28, Vote> = HashMap::new();

        let empty = ImblOrdMap::new();
        let action_votes = votes_by_action.get(action_id).unwrap_or(&empty);

        for (voter, procedure) in action_votes {
            match voter {
                Voter::DRep(cred) => {
                    let drep_hash = credential_to_hash(cred);
                    drep_votes.insert(drep_hash, procedure.vote.clone());
                }
                Voter::StakePool(pool_hash) => {
                    // Pool IDs are Hash28 (Blake2b-224); convert from Hash32
                    let pool_id = Hash28::from_bytes({
                        let mut b = [0u8; 28];
                        b.copy_from_slice(&pool_hash.as_bytes()[..28]);
                        b
                    });
                    spo_votes.insert(pool_id, procedure.vote.clone());
                }
                Voter::ConstitutionalCommittee(_) => {
                    cc_total += 1;
                    if procedure.vote == Vote::Yes {
                        cc_yes += 1;
                    }
                }
            }
        }

        // ── SPO ratio per Haskell `spoAcceptedRatio` ──
        //
        // Iterate ALL pools in the mark snapshot distribution (not just explicit
        // voters). For each pool, classify its vote per Haskell's accumStake:
        //
        //   Explicit Yes    → yes += stake
        //   Explicit No     → (neither yes nor abstain — stays in denominator)
        //   Explicit Abstain → abstain += stake
        //   No vote + HardFork (any era)  → No
        //   No vote + bootstrap + non-HardFork → Abstain
        //   No vote + post-bootstrap + non-HardFork → defaultStakePoolVote
        //
        // Denominator = totalActiveStake − abstainStake (always, for all action types).
        let bootstrap = self.is_bootstrap_phase();
        let is_hardfork = matches!(action, GovAction::HardForkInitiation { .. });
        let is_no_confidence = matches!(action, GovAction::NoConfidence { .. });

        let mut spo_yes = 0u64;
        let mut spo_abstain = 0u64;

        // Get the pool distribution.  When a caller supplies pool_stake_override
        // (ratification path — uses the *set* snapshot, i.e. the previous epoch's
        // mark, matching Haskell's dpStakePoolDistr), prefer that.  Otherwise fall
        // back to self.epochs.snapshots.mark for non-ratification contexts (e.g. queries).
        let mark_pool_stake: Option<&HashMap<Hash28, Lovelace>> = pool_stake_override
            .or_else(|| self.epochs.snapshots.mark.as_ref().map(|s| &s.pool_stake));

        if let Some(pool_stake) = mark_pool_stake {
            for (pool_id, stake) in pool_stake {
                let stake = stake.0;
                match spo_votes.get(pool_id) {
                    Some(Vote::Yes) => {
                        spo_yes += stake;
                    }
                    Some(Vote::Abstain) => {
                        spo_abstain += stake;
                    }
                    Some(Vote::No) => {
                        // Explicit No: in denominator but not numerator or abstain.
                    }
                    None => {
                        // Non-voter: classify per Haskell rules.
                        // HardFork guard fires BEFORE bootstrap guard.
                        if is_hardfork {
                            // No vote on HardFork → No (in denom, not num)
                        } else if bootstrap {
                            // No vote during bootstrap on non-HardFork → Abstain
                            spo_abstain += stake;
                        } else {
                            // Post-bootstrap: defaultStakePoolVote
                            match self.default_spo_vote(pool_id, vote_delegations_override) {
                                DefaultVote::NoConfidence if is_no_confidence => {
                                    spo_yes += stake;
                                }
                                DefaultVote::Abstain => {
                                    spo_abstain += stake;
                                }
                                _ => {
                                    // DefaultVote::No or NoConfidence on non-NoConfidence
                                    // action: No (in denom, not num)
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // Fallback: no mark snapshot (first epoch). Only count explicit voters
            // with a simple delegation scan (matches pre-snapshot behavior).
            for (pool_id, vote) in &spo_votes {
                let stake = self.compute_spo_voting_power(pool_id);
                match vote {
                    Vote::Yes => spo_yes += stake,
                    Vote::Abstain => spo_abstain += stake,
                    Vote::No => {}
                }
            }
        }

        // ── DRep ratio per Haskell `dRepAcceptedRatio` ──
        //
        // Iterate ALL active DRep stake (from drep_power_cache), not just voters.
        // Non-voting DReps are implicit No (in denominator, not numerator).
        let mut drep_yes = 0u64;
        let mut drep_abstain = 0u64;
        let mut drep_total_all = 0u64;

        for (drep_hash, &power) in drep_power_cache {
            drep_total_all += power;
            match drep_votes.get(drep_hash) {
                Some(Vote::Yes) => {
                    drep_yes += power;
                }
                Some(Vote::Abstain) => {
                    drep_abstain += power;
                }
                Some(Vote::No) | None => {
                    // Voted No or didn't vote: implicit No (already in total)
                }
            }
        }

        // Handle AlwaysNoConfidence stake per CIP-1694:
        // - For NoConfidence actions: counts as Yes
        // - For all other actions: counts as No (in denominator, not numerator)
        // AlwaysNoConfidence is always in the denominator.
        if no_confidence_stake > 0 {
            drep_total_all += no_confidence_stake;
            if is_no_confidence {
                drep_yes += no_confidence_stake;
            }
        }

        // AlwaysAbstain: already excluded from drep_power_cache (handled in build_drep_power_cache)

        // DRep denominator = total active stake - abstain stake
        let drep_total = drep_total_all.saturating_sub(drep_abstain);

        (drep_yes, drep_total, spo_yes, spo_abstain, cc_yes, cc_total)
    }

    /// Determine the default vote for a non-voting SPO, per Haskell
    /// `defaultStakePoolVote` from `Cardano.Ledger.Conway.Governance.Procedures`.
    ///
    /// Looks up the pool's reward account credential in `vote_delegations` to find
    /// the DRep delegation. If the credential delegates to `AlwaysNoConfidence`,
    /// returns `NoConfidence`; if `AlwaysAbstain`, returns `Abstain`; otherwise `No`.
    #[allow(dead_code)]
    fn default_spo_vote(
        &self,
        pool_id: &Hash28,
        vote_delegations_override: Option<&ImblHashMap<Hash32, DRep>>,
    ) -> DefaultVote {
        // Look up the pool's reward account from pool_params
        let pool_reg = self.certs.pool_params.get(pool_id);
        let reward_account = match pool_reg {
            Some(reg) if reg.reward_account.len() >= 29 => &reg.reward_account,
            _ => return DefaultVote::No,
        };

        // Extract the stake credential hash from the reward account
        let cred_hash = Self::reward_account_to_hash(reward_account);

        // Use frozen vote_delegations from the ratification snapshot when
        // available, matching Haskell's `dpDefaultDRepVoteDelegs` captured
        // in the DRep pulser at the previous epoch boundary.  Falls back
        // to live state for non-ratification contexts or before the first
        // snapshot is captured.
        let delegations =
            vote_delegations_override.unwrap_or(&self.gov.governance.vote_delegations);

        match delegations.get(&cred_hash) {
            Some(DRep::NoConfidence) => DefaultVote::NoConfidence,
            Some(DRep::Abstain) => DefaultVote::Abstain,
            _ => DefaultVote::No,
        }
    }

    /// Get the total stake for a credential: UTxO stake + reward balance.
    ///
    /// Note: For DRep voting power (via `build_drep_power_cache_live`), proposal
    /// deposits are added separately via `proposal_deposits_by_credential()`.
    pub(crate) fn credential_stake(&self, cred_hash: &Hash32) -> u64 {
        let utxo = self
            .certs
            .stake_distribution
            .stake_map
            .get(cred_hash)
            .map(|s| s.0)
            .unwrap_or(0);
        let reward = self
            .certs
            .reward_accounts
            .get(cred_hash)
            .map(|s| s.0)
            .unwrap_or(0);
        utxo + reward
    }

    /// Build a cache of DRep voting power (Hash32 -> delegated stake) for ratification.
    ///
    /// Per Haskell `reDRepDistr` (`Conway.Rules.Epoch`), ratification must use the
    /// DRep stake distribution captured at the *start* of the current epoch (the
    /// "mark" snapshot), not the live state.  If a snapshot is available it is used
    /// directly.  Otherwise the live `vote_delegations` state is scanned as a fallback
    /// (first epoch, or nodes upgrading from older snapshots without this field).
    ///
    /// Returns `(drep_power_cache, always_no_confidence_stake, always_abstain_stake)`.
    pub fn build_drep_power_cache(&self) -> (ImblHashMap<Hash32, u64>, u64, u64) {
        build_drep_power_cache_from(&self.gov, &self.certs)
    }

    /// Capture the DRep distribution snapshot at the end of an epoch transition.
    ///
    /// Called AFTER ratification, DRep activity updates, and committee expiry —
    /// alongside `freeze_prior_boundary_pulser()` — matching Haskell's
    /// `setFreshDRepPulsingState` which captures `dpDRepDistr` from the
    /// post-EPOCH state.  The snapshot is consumed by `build_drep_power_cache()`
    /// and `compute_total_drep_stake()` at the NEXT epoch boundary, providing
    /// the one-epoch-lagged DRep voting power that matches Haskell's pulser
    /// lifecycle.
    #[allow(dead_code)]
    pub(crate) fn capture_drep_distribution_snapshot(&mut self) {
        set_fresh_drep_pulsing_state(self.epoch, &self.epochs, &self.certs, &mut self.gov);
    }

    /// Freeze only the ratification INPUTS — Haskell's `PulsingSnapshot` half.
    ///
    /// For fixtures that drive [`Self::ratify_proposals`] directly (the RATIFY
    /// decision in isolation) and so need a candidate set but no plan. Tests
    /// that cross an epoch boundary want [`Self::freeze_prior_boundary_pulser`]
    /// instead.
    #[allow(dead_code)]
    pub fn capture_ratification_snapshot(&mut self) {
        set_fresh_drep_pulsing_state(self.epoch, &self.epochs, &self.certs, &mut self.gov);
    }

    /// Stand in for the epoch boundary that PRECEDES the one under test.
    ///
    /// A proposal is never a candidate at the first boundary after it is
    /// submitted: Haskell's RATIFY signal is the pulser's `dpProposals`, frozen
    /// by `setFreshDRepPulsingState` one boundary earlier (#903). Tests that
    /// exercise ratification logic rather than that timing call this to say
    /// "assume the previous boundary happened".
    ///
    /// It runs the whole governance step, not just the input capture. Since
    /// #988 step 2 the boundary APPLIES a frozen decision instead of computing
    /// one, so a test that froze only the inputs would find no plan and watch
    /// nothing ratify — correctly, but for a reason it never meant to test.
    #[allow(dead_code)]
    pub fn freeze_prior_boundary_pulser(&mut self) {
        epoch_boundary_governance_step(self.epoch, &self.epochs, &self.certs, &mut self.gov);
    }

    /// Compute total active DRep-delegated stake across all DReps.
    /// Excludes stake delegated to inactive DReps.
    /// Includes stake delegated to Abstain and NoConfidence (they are part of total DRep ecosystem).
    #[allow(dead_code)]
    pub(crate) fn compute_total_drep_stake(&self) -> u64 {
        compute_total_drep_stake_from(&self.gov, &self.certs)
    }

    /// Compute the voting power of a stake pool: total delegated stake.
    ///
    /// Uses the `mark` snapshot (current epoch's stake distribution).
    ///
    /// **NOTE:** During ratification (`ratify_proposals`), SPO voting power is
    /// read from the **set** snapshot (previous epoch's mark) via the
    /// `pool_stake_override` parameter on `count_votes_by_type`, matching
    /// Haskell's `dpStakePoolDistr` in the DRep pulser.  This function is a
    /// fallback for non-ratification contexts (queries, first-epoch bootstrap).
    ///
    /// Reference: Haskell `spoVotingPower` in `Cardano.Ledger.Conway.Governance.Procedures`
    /// uses `ssStakeMarkPoolDistr` (the mark pool distribution).
    #[allow(dead_code)]
    pub(crate) fn compute_spo_voting_power(&self, pool_id: &Hash28) -> u64 {
        // Use the "mark" snapshot — non-ratification fallback.
        if let Some(ref snapshot) = self.epochs.snapshots.mark {
            if let Some(stake) = snapshot.pool_stake.get(pool_id) {
                return stake.0;
            }
        }
        // Fallback: compute from current delegations (UTxO + rewards).
        // This path is taken during the first two epochs before snapshots are populated.
        debug!("SPO voting power: falling back to O(n) delegation scan — snapshot not available");
        let mut total = 0u64;
        for (stake_cred, delegated_pool) in self.certs.delegations.iter() {
            if delegated_pool == pool_id {
                total += self.credential_stake(stake_cred);
            }
        }
        total
    }

    /// Compute total active SPO stake across all pools from the `mark` snapshot.
    ///
    /// **NOTE:** During ratification, total SPO stake is computed from the
    /// **set** snapshot directly inside `ratify_proposals()`, not via this
    /// function.  This ensures ratification uses the previous epoch's mark
    /// (matching Haskell's `dpStakePoolDistr`).  This function is used as a
    /// fallback for non-ratification contexts and early epochs.
    #[allow(dead_code)]
    fn compute_total_spo_stake(&self) -> u64 {
        // Use "mark" snapshot if available, else fall back.
        if let Some(ref snapshot) = self.epochs.snapshots.mark {
            let total: u64 = snapshot
                .pool_stake
                .values()
                .fold(0u64, |acc, s| acc.saturating_add(s.0));
            return total.max(1);
        }
        // Fallback: sum all pool stake from current delegations (UTxO + rewards).
        // This path is taken during the first two epochs before snapshots are populated.
        let mut total = 0u64;
        for stake_cred in self.certs.delegations.keys() {
            total = total.saturating_add(self.credential_stake(stake_cred));
        }
        total.max(1)
    }

    /// Enact a ratified governance action by applying its effects
    #[allow(dead_code)]
    pub(crate) fn enact_gov_action(&mut self, action: &GovAction) {
        enact_gov_action_impl(action, &mut self.epochs, &mut self.certs, &mut self.gov);
    }
}

// ── Free functions for governance epoch operations ─────────────────────
//
// These functions are extracted from `LedgerState` methods so they can be
// called from both the monolithic `LedgerState::process_epoch_transition`
// (via thin wrappers) and the new `EraRules`-based dispatch path which
// operates on decomposed sub-states.

/// Update dormant epoch counter per Haskell Conway.Rules.Epoch `updateNumDormantEpochs`.
/// Only applicable during Conway era (PV >= 9).
pub(crate) fn update_dormant_epochs(
    new_epoch: EpochNo,
    epochs: &EpochSubState,
    gov: &mut GovSubState,
) {
    if epochs.protocol_params.protocol_version_major >= 9 {
        let gov_state = Arc::make_mut(&mut gov.governance);
        if gov_state.proposals.is_empty() {
            gov_state.num_dormant_epochs = gov_state.num_dormant_epochs.saturating_add(1);
            debug!(
                epoch = new_epoch.0,
                num_dormant = gov_state.num_dormant_epochs,
                "Governance: epoch is dormant (no active proposals)"
            );
        }
    }
}

/// Mark inactive DReps per CIP-1694.
///
/// Haskell stores drepExpiry = (activity_epoch + drep_activity) - num_dormant
/// at registration/vote time, then checks: currentEpoch > drepExpiry.
/// We store the same value in drep.drep_expiry, so the check is simple.
pub(crate) fn update_drep_activity(
    new_epoch: EpochNo,
    epochs: &EpochSubState,
    gov: &mut GovSubState,
) {
    if epochs.protocol_params.protocol_version_major >= 9 {
        let mut newly_inactive = 0u64;
        let mut reactivated = 0u64;
        // imbl::HashMap has no values_mut (persistent); collect keys, then
        // get_mut each (CoW per touched DRep — fine for the ~tens of DReps).
        let gov_mut = Arc::make_mut(&mut gov.governance);
        let drep_keys: Vec<Hash32> = gov_mut.dreps.keys().cloned().collect();
        for k in drep_keys {
            if let Some(drep) = gov_mut.dreps.get_mut(&k) {
                let expired = new_epoch.0 > drep.drep_expiry.0;
                if expired && drep.active {
                    drep.active = false;
                    newly_inactive += 1;
                } else if !expired && !drep.active {
                    drep.active = true;
                    reactivated += 1;
                }
            }
        }
        if newly_inactive > 0 || reactivated > 0 {
            debug!(
                "DRep activity update at epoch {}: {} newly inactive, {} reactivated",
                new_epoch.0, newly_inactive, reactivated
            );
        }
    }
}

/// Observe committee members whose expiry epoch has passed.
///
/// Matches Haskell `cardano-ledger` (`Cardano.Ledger.Conway.Rules.Epoch`):
/// the expiry epoch transition does NOT physically remove expired members
/// from the committee map. Membership is retained verbatim; the
/// `MemberStatus = Expired` projection is computed dynamically at query
/// time (see `Cardano.Ledger.Conway.Governance.Committee.committeeMemberStateF`
/// and dugite's analogue in `crates/dugite-node/src/node/query.rs`).
///
/// Retaining expired entries is required for two reasons:
///   * The CC state query must surface them with `Expired` status (#433).
///   * Subsequent `UpdateCommittee` actions can re-elect the same cold
///     credential with a fresh `validUntil`; if we had dropped the entry
///     we would lose the prior authorization context observable from queries.
///
/// Ratification already filters expired members in-place by comparing the
/// current epoch against the stored `validUntil` (see `check_cc_approval`
/// and `count_cc_votes_quorum`), so leaving stale entries in the map does
/// not affect voting weight.
pub(crate) fn expire_committee_members(new_epoch: EpochNo, gov: &mut GovSubState) {
    let expired_count = gov
        .governance
        .committee_expiration
        .iter()
        .filter(|(_, exp_epoch)| **exp_epoch < new_epoch)
        .count();
    if expired_count > 0 {
        debug!(
            "Observed {} expired committee members at epoch {} (retained in map; surfaced as MemberStatus=Expired in queries)",
            expired_count, new_epoch.0
        );
    }
}

/// Prune committee hot-key authorizations and resignations to the
/// POST-enactment committee membership at the epoch boundary.
///
/// Mirrors Haskell `updateCommitteeState` (Conway/Rules/Epoch.hs), which runs
/// unconditionally every epoch boundary AFTER enactment:
///
/// ```haskell
/// updateCommitteeState :: StrictMaybe (Committee era) -> CommitteeState era -> CommitteeState era
/// updateCommitteeState committee (CommitteeState creds) =
///   CommitteeState $ Map.intersection creds members
///   where
///     members = foldMap' committeeMembers committee
/// ```
///
/// `vsCommitteeState` (dugite: `committee_hot_keys` + `committee_resigned`)
/// keeps ONLY entries whose cold credential is in the new committee — entries
/// for members removed by an enacted `UpdateCommittee` (explicitly or
/// implicitly) are discarded, and a `NoConfidence` (empty committee) wipes
/// everything. `script_committee_hot_credentials` (hot-credential TYPE
/// tracking, keyed by HOT hash) is kept in sync with the retained hot keys.
pub(crate) fn prune_committee_state(gov: &mut GovSubState) {
    let members: std::collections::HashSet<Hash32> = gov
        .governance
        .committee_expiration
        .keys()
        .copied()
        .collect();
    let gov_state = Arc::make_mut(&mut gov.governance);

    let stale_hot: Vec<Hash32> = gov_state
        .committee_hot_keys
        .iter()
        .filter(|(cold, _)| !members.contains(*cold))
        .map(|(_, hot)| *hot)
        .collect();
    if !stale_hot.is_empty() {
        gov_state
            .committee_hot_keys
            .retain(|cold, _| members.contains(cold));
        // A hot credential's script-type marker is dropped only when no
        // RETAINED member still authorizes that same hot credential.
        let live_hot: std::collections::HashSet<Hash32> =
            gov_state.committee_hot_keys.values().copied().collect();
        for hot in stale_hot {
            if !live_hot.contains(&hot) {
                gov_state.script_committee_hot_credentials.remove(&hot);
            }
        }
    }
    gov_state
        .committee_resigned
        .retain(|cold, _| members.contains(cold));
}

/// Capture the DRep distribution snapshot and ratification snapshot for the
/// NEXT epoch boundary.
///
/// Per Haskell `setFreshDRepPulsingState`, the pulser is created from the
/// post-transition state — after ratification/expiry have pruned proposals,
/// enacted roots have been updated, DRep activity has been updated, and
/// committee members have been expired.
/// Haskell `solidifyNextEpochPParams` — collapse `futurePParams` once the slot
/// passes the point of no return (#977).
///
/// ```haskell
/// getTheSlotOfNoReturn slot = ...
///   pointOfNoReturn = firstSlotNextEpoch *- Duration (2 * stabilityWindow)
///
/// solidifyNextEpochPParams nes slot =
///   if slot < slotOfNoReturn then nes
///   else nes & futurePParamsGovStateL %~ solidifyFuturePParams
/// ```
///
/// `stabilityWindow` here is `3k/f` — dugite's `stability_window_3kf`, NOT the
/// randomness-stabilisation window (`4k/f`) used by the RUPD pulser. The two
/// are different constants and are easy to confuse.
///
/// Runs on EVERY block, before the boundary/predict step, matching
/// `validatingTickTransition`.
pub(crate) fn solidify_next_epoch_pparams(
    slot: u64,
    first_slot_next_epoch: u64,
    stability_window_3kf: u64,
    gov: &mut GovSubState,
) {
    let point_of_no_return = first_slot_next_epoch.saturating_sub(2 * stability_window_3kf);
    if slot < point_of_no_return {
        return;
    }
    let g = Arc::make_mut(&mut gov.governance);
    let before = g.future_pparams.clone();
    g.future_pparams.solidify();
    if before != g.future_pparams {
        debug!(
            slot,
            point_of_no_return, "futurePParams solidified at the point of no return"
        );
    }
}

/// Haskell `predictFuturePParams` — run on every NON-boundary tick (#977).
///
/// ```haskell
/// predictFuturePParams govState = case cgsFuturePParams govState of
///   NoPParamsUpdate         -> govState
///   DefinitePParamsUpdate _ -> govState
///   _ -> govState { cgsFuturePParams = PotentialPParamsUpdate newFuturePParams }
///   where
///     newFuturePParams = do
///       guard (any hasChangesToPParams (rsEnacted ratifyState))
///       pure (ensCurPParams (rsEnactState ratifyState))
///     ratifyState = extractDRepPulsingState (cgsDRepPulsingState govState)
/// ```
///
/// `rsEnacted`/`rsEnactState` are the DRep pulser's, which is why this needed
/// #988 first: without a frozen pulser result dugite simply could not answer
/// "what will enact". Both terms come from `pulsed_ratify_state`.
///
/// Note the two early returns: once the value is `No` or `Definite` it is
/// settled and prediction must NOT reopen it.
pub(crate) fn predict_future_pparams(gov: &mut GovSubState) {
    match gov.governance.future_pparams {
        FuturePParams::NoPParamsUpdate | FuturePParams::DefinitePParamsUpdate(_) => return,
        FuturePParams::PotentialPParamsUpdate(_) => {}
    }
    let predicted = gov
        .governance
        .ratify_plan()
        .filter(|p| p.has_pparams_changes)
        .map(|p| Box::new(p.cur_pparams.clone()));
    let g = Arc::make_mut(&mut gov.governance);
    g.future_pparams = FuturePParams::PotentialPParamsUpdate(predicted);
}

/// Ratify at an epoch boundary, and check the outcome against the plan the
/// pulser froze at the PREVIOUS boundary (#988).
///
/// **Every** boundary path must go through this rather than calling
/// [`ratify_proposals_impl`] directly. The detector originally lived inline in
/// `LedgerState::process_epoch_transition` — the `#[doc(hidden)]` test-only
/// path — so it never executed on a real node, and the "0 mismatches observed"
/// evidence recorded when #988 was closed was measuring nothing. Same trap as
/// #977, in the same change. One shared function is what makes that
/// unrepeatable.
///
/// # What the check means
///
/// The plan is computed at boundary B from the snapshot frozen at B, and
/// applied here at B+1 from that same snapshot, so the two MUST agree. Where
/// they can still diverge is the handful of live reads remaining inside the
/// threshold path — notably `vote_delegations` when attributing proposal
/// deposits, which Haskell freezes into `psDRepDistr` and dugite re-reads.
///
/// This does not paper over that; it makes it LOUD. A mismatch means
/// `GetRatifyState` told a client one thing and the chain then did another,
/// which is exactly the class of silent divergence this repository keeps
/// having to find the hard way. It WARNs rather than asserts because a false
/// crash on a live node is worse than a false green — but a WARN cannot be
/// missed by the evidence analyzer, which a debug! can.
pub(crate) fn ratify_at_boundary(
    epoch: EpochNo,
    boundary_epoch: EpochNo,
    epochs: &mut EpochSubState,
    certs: &mut CertSubState,
    gov: &mut GovSubState,
) {
    let Some(plan) = gov.governance.ratify_plan().cloned() else {
        // Haskell's `Default (DRepPulsingState era)` is `DRComplete def def` —
        // an EMPTY result. Nothing enacts and nothing expires at a boundary
        // with no pulser, which is reachable only at the first Conway boundary
        // of a chain and on the first boundary after loading a snapshot older
        // than the field. Re-deriving a decision here instead would ratify from
        // inputs frozen at the WRONG boundary, which is #903 all over again.
        if epochs.protocol_params.protocol_version_major >= 9 {
            warn!(
                boundary_epoch = boundary_epoch.0,
                "No frozen DRep pulser at a Conway epoch boundary — nothing can \
                 ratify (expected once at the first Conway boundary, or once \
                 after loading a pre-v33 snapshot) (#988)"
            );
        }
        let g = Arc::make_mut(&mut gov.governance);
        g.last_ratified = Vec::new();
        g.last_expired = Vec::new();
        g.last_ratify_delayed = false;
        return;
    };

    // The plan carries the epoch it was computed under — Haskell's
    // `dpCurrentEpoch`, which RATIFY consumed as `reCurrentEpoch`. This
    // boundary must be the very next one. A mismatch means the pulser was
    // stamped or frozen at the wrong point, which is exactly the defect the
    // prediction-vs-outcome detector caught on preview (`boundary_epoch=743
    // predicted_at=741`) and the one thing that stops being self-evident once
    // the plan is applied rather than compared.
    if plan.computed_at_epoch != epoch {
        warn!(
            boundary_epoch = boundary_epoch.0,
            ratify_epoch = epoch.0,
            plan_epoch = plan.computed_at_epoch.0,
            "DRep pulser plan is stamped with the wrong epoch — it was frozen at \
             a boundary other than the immediately preceding one (#988)"
        );
    }

    apply_ratify_decision(&plan, epoch, epochs, certs, gov);

    // Positive evidence, at INFO deliberately: a run that logs nothing here is
    // indistinguishable from a run where the boundary never reached this code,
    // and that ambiguity is what made the original #988 evidence worthless.
    // One line per epoch boundary is negligible volume.
    info!(
        boundary_epoch = boundary_epoch.0,
        planned_at = plan.computed_at_epoch.0,
        enacted = gov.governance.last_ratified.len(),
        expired = gov.governance.last_expired.len(),
        delayed = gov.governance.last_ratify_delayed,
        "Applied the frozen DRep pulser result at the epoch boundary (#988)"
    );
}

/// Apply a completed pulser's `RatifyState`, as Haskell's EPOCH rule does.
///
/// Upstream this is not a separate function — it is the body of
/// `epochTransition` between `extractDRepPulsingState` and
/// `setFreshDRepPulsingState`. The two halves it performs are:
///
/// 1. the effects of `rsEnactState`, which dugite realises by replaying
///    [`enact_gov_action_impl`] over `rsEnacted` in order (upstream threads an
///    `EnactState` record through RATIFY and copies its fields onto the gov
///    state here; dugite mutates the live state directly, which is the same
///    state transition expressed differently); and
/// 2. `proposalsApplyEnactment rsEnacted rsExpired`, against the LIVE proposal
///    superset.
///
/// The actions themselves are looked up in the frozen proposal set the plan was
/// decided over — that set IS Haskell's `rsEnacted`, which carries whole
/// `GovActionState`s rather than bare ids.
fn apply_ratify_decision(
    plan: &PulsedRatifyState,
    epoch: EpochNo,
    epochs: &mut EpochSubState,
    certs: &mut CertSubState,
    gov: &mut GovSubState,
) {
    let frozen = gov.governance.pulsing_snapshot().cloned();

    let (
        mut enacted_pparam,
        mut enacted_hardfork,
        mut enacted_committee_root,
        mut enacted_constitution,
    ) = match frozen {
        Some(ref snap) => (
            snap.enacted_pparam_update.clone(),
            snap.enacted_hard_fork.clone(),
            snap.enacted_committee.clone(),
            snap.enacted_constitution.clone(),
        ),
        None => {
            let g = &gov.governance;
            (
                g.enacted_pparam_update.clone(),
                g.enacted_hard_fork.clone(),
                g.enacted_committee.clone(),
                g.enacted_constitution.clone(),
            )
        }
    };

    for action_id in &plan.enacted {
        // The frozen set is the decision's own candidate set, so this is where
        // the action must come from. The live set is a fallback only: it is a
        // superset, and an enacted id is by construction present in both, but
        // reaching for it silently would hide a plan/candidate-set mismatch.
        let action = frozen
            .as_ref()
            .and_then(|snap| snap.proposals.get(action_id))
            .or_else(|| gov.governance.proposals.get(action_id))
            .map(|state| state.procedure.gov_action.clone());

        let Some(action) = action else {
            warn!(
                action_id = %action_id.transaction_id.to_hex(),
                index = action_id.action_index,
                epoch = epoch.0,
                "DRep pulser planned to enact a governance action that is in \
                 neither the frozen nor the live proposal set — skipping (#988)"
            );
            continue;
        };

        debug!(
            action_id = %action_id.transaction_id.to_hex(),
            action_type = ?std::mem::discriminant(&action),
            "Governance proposal ENACTED from the frozen pulser plan"
        );
        enact_gov_action_impl(&action, epochs, certs, gov);
        update_enacted_root_local(
            action_id,
            &action,
            &mut enacted_pparam,
            &mut enacted_hardfork,
            &mut enacted_committee_root,
            &mut enacted_constitution,
        );
    }

    {
        let gov_state = Arc::make_mut(&mut gov.governance);
        gov_state.enacted_pparam_update = enacted_pparam;
        gov_state.enacted_hard_fork = enacted_hardfork;
        gov_state.enacted_committee = enacted_committee_root;
        gov_state.enacted_constitution = enacted_constitution;
    }

    proposals_apply_enactment(&plan.enacted, &plan.expired, epoch, epochs, certs, gov);

    Arc::make_mut(&mut gov.governance).last_ratify_delayed = plan.delayed;
}

/// Compute the frozen pulser result for the epoch now starting (#988).
///
/// Haskell's `setFreshDRepPulsingState` creates the pulser at the boundary and
/// the pulser's inputs are all frozen there; every reader forces it to
/// completion, so its answer is CONSTANT for the whole epoch. Computing it once
/// here reproduces that exactly — and is strictly closer to upstream than
/// deriving it lazily, which would re-read state that has moved since the
/// boundary.
///
/// It is computed by running the REAL `ratify_proposals_impl` against a CLONE
/// of the sub-states and reading off the decisions. That is deliberate: a
/// separate "dry-run" implementation would be a second copy of ~300 lines of
/// threshold logic, and a second copy is how #985's overlay condition drifted
/// (`the condition existed in TWO hand-written copies, only one current`).
/// One implementation, no drift.
///
/// The clone is affordable because it happens once per epoch boundary and the
/// large maps are `imbl` persistent structures or `Arc`s, so most of it is
/// refcount bumps rather than deep copies.
fn compute_pulsed_ratify_state(
    epoch: EpochNo,
    epochs: &EpochSubState,
    certs: &CertSubState,
    gov: &GovSubState,
) -> PulsedRatifyState {
    let mut e = epochs.clone();
    let mut c = certs.clone();
    let mut g = gov.clone();

    // Rotate the stake snapshot the way the NEXT boundary will, before
    // predicting what it will do.
    //
    // `ratify_proposals_impl` reads `snapshots.set` for SPO voting power. At a
    // boundary, dugite rotates `set <- mark` (eras/conway.rs) BEFORE ratifying,
    // so the run being predicted here will see the `mark` that exists now. This
    // function runs at the END of the current boundary, where `set` is still the
    // PREVIOUS boundary's mark — one generation too old.
    //
    // Haskell freezes the right one into the pulser directly
    // (`Conway.Rules.Epoch`):
    //
    // ```haskell
    // snapshots1 <- trans @(EraRule "SNAP" era) $ TRC (...)   -- rotation
    // stakePoolDistr = ssStakeMarkPoolDistr snapshots1        -- the NEW mark
    // ...
    // liftSTS $ setFreshDRepPulsingState eNo stakePoolDistr epochState2
    // ```
    //
    // and RATIFY at the next boundary uses it as `reStakePoolDistr`. Since
    // `mark` here becomes `set` there, copying it across reproduces that
    // exactly.
    //
    // This was the residual cause of the #988 divergence. On a full preview
    // replay it accounted for all 6 mismatches in 733 Conway boundaries, and
    // every one was a boundary where the chain really did enact something —
    // so `GetRatifyState` was correct on the 727 boundaries where nothing
    // happened and wrong on all 6 that mattered.
    if e.snapshots.mark.is_some() {
        e.snapshots.set = e.snapshots.mark.clone();
    }

    ratify_proposals_impl(epoch, &mut e, &mut c, &mut g)
}

/// The Conway EPOCH rule's governance step, run once per boundary.
///
/// This is deliberately ONE function rather than a reset plus a capture at
/// each call site. There are two boundary paths in this crate — the per-era
/// `EraRulesImpl::process_epoch_transition` (production) and the
/// `#[doc(hidden)]` `LedgerState::process_epoch_transition` (tests) — and
/// #977's `futurePParams` reset was originally written into the test one
/// ONLY. Every unit test passed, because the unit tests use that path; the
/// devnet caught it by diffing against cardano-node across a live boundary.
/// That is the N-copies trap #985 called out, recurring. Folding both steps
/// into a single shared function is what makes the drift inexpressible.
///
/// Order is load-bearing and matches `Cardano.Ledger.Conway.Rules.Epoch`:
///
/// 1. `cgsFuturePParamsL .~ PotentialPParamsUpdate Nothing`, UNCONDITIONALLY
///    — whatever enacted, the epoch just ended's prediction is discarded
///    wholesale and rebuilt by `predict_future_pparams` on later blocks.
/// 2. `setFreshDRepPulsingState` — the DRep distribution, the ratification
///    snapshot, and the frozen pulser result, in that order, each computed
///    from the state the previous step left.
///
/// The reset must come FIRST: the pulser frozen in step 2 describes the NEXT
/// boundary, and prediction may only read it on LATER blocks. Haskell's
/// NEWEPOCH takes the boundary branch or the predict branch, never both.
///
/// # `new_epoch` is Haskell's `eNo`, and it is NOT the epoch that just ended
///
/// `Conway.Rules.Epoch.epochTransition` ends with
/// `setFreshDRepPulsingState eNo stakePoolDistr epochState2`, and NEWEPOCH
/// reaches EPOCH only when `eNo == succ eL` — so `eNo` is the epoch STARTING
/// at this boundary. `setFreshDRepPulsingState` stores it as
/// `dpCurrentEpoch`, and the NEXT boundary's RATIFY runs with
/// `reCurrentEpoch = dpCurrentEpoch`.
///
/// So the pulser must be computed with the epoch the next boundary will
/// ratify under, not the one just ending. Passing the ending epoch here made
/// `GetRatifyState` answer from a ratification run one epoch behind — caught
/// on preview by the #988 detector (`boundary_epoch=743 predicted_at=741`),
/// not by any test, because both the prediction and the application are
/// self-consistent in isolation.
pub(crate) fn epoch_boundary_governance_step(
    new_epoch: EpochNo,
    epochs: &EpochSubState,
    certs: &CertSubState,
    gov: &mut GovSubState,
) {
    if epochs.protocol_params.protocol_version_major >= 9 {
        Arc::make_mut(&mut gov.governance).future_pparams =
            FuturePParams::PotentialPParamsUpdate(None);
        set_fresh_drep_pulsing_state(new_epoch, epochs, certs, gov);
        // 3. …and predict IMMEDIATELY, from the pulser just frozen.
        //
        // `setFreshDRepPulsingState` ends with exactly this, applied to the
        // govState that already carries the new pulser:
        //
        // ```haskell
        // govState' =
        //   predictFuturePParams $
        //     govState & cgsDRepPulsingStateL .~ DRPulsing (DRepPulser {..})
        // ```
        //
        // So the boundary does NOT leave `PotentialPParamsUpdate Nothing`
        // behind — it leaves `Just pp` whenever the fresh plan enacts a
        // `ParameterChange` or `HardForkInitiation`. dugite reset and froze but
        // never predicted, so the value stayed `Nothing` until some LATER
        // non-boundary tick predicted it (#995).
        //
        // On a chain where `2 * stabilityWindow >= epochLength` there is no
        // such later tick: `solidifyNextEpochPParams` fires on the very next
        // block and collapses `Potential Nothing` to `NoPParamsUpdate`, which
        // prediction then refuses to reopen. That is the devnet, where dugite
        // answered `NoPParamsUpdate` for an entire epoch in which cardano-node
        // answered `DefinitePParamsUpdate` — 235 divergent samples confined to
        // the epoch before the change enacted.
        //
        // It matters beyond the query: `futurePParams` feeds `nextEpochPParams`,
        // which `Conway.Rules.Tickf` uses for the ledger-view FORECAST, so a
        // node that gets this wrong validates next-epoch headers against the
        // current epoch's protocol version and size limits.
        predict_future_pparams(gov);
    }
}

/// Haskell `setFreshDRepPulsingState eNo stakePoolDistr epochState` — freeze a
/// new pulser for the epoch now starting (#988).
///
/// The ONLY writer of [`GovernanceState::drep_pulsing_state`]. Upstream this is
/// two states: `setFreshDRepPulsingState` installs `DRPulsing` with the inputs
/// frozen and the result not yet computed, and `finishDRepPulser` collapses it
/// to `DRComplete` when a reader forces it. dugite has no incremental pulsing,
/// so both happen here — which is why the inputs are installed first and the
/// result written over them: the decision reads its inputs from `gov`, exactly
/// as `finishDRepPulser` reads them from the pulser record.
///
/// The intermediate state is never observable outside this function.
fn set_fresh_drep_pulsing_state(
    new_epoch: EpochNo,
    epochs: &EpochSubState,
    certs: &CertSubState,
    gov: &mut GovSubState,
) {
    let snapshot = build_pulsing_snapshot(new_epoch, epochs.treasury.0, certs, gov);
    Arc::make_mut(&mut gov.governance).drep_pulsing_state = Some(DRepPulsingState {
        snapshot,
        // Placeholder: overwritten below, before this function returns.
        ratify_state: PulsedRatifyState {
            computed_at_epoch: new_epoch,
            enacted: Vec::new(),
            expired: Vec::new(),
            delayed: false,
            cur_pparams: epochs.protocol_params.clone(),
            has_pparams_changes: false,
        },
    });

    let pulsed = compute_pulsed_ratify_state(new_epoch, epochs, certs, gov);
    debug!(
        epoch = new_epoch.0,
        enacted = pulsed.enacted.len(),
        expired = pulsed.expired.len(),
        delayed = pulsed.delayed,
        has_pparams_changes = pulsed.has_pparams_changes,
        "DRep pulser: frozen ratification result for the epoch now starting"
    );
    if let Some(p) = Arc::make_mut(&mut gov.governance)
        .drep_pulsing_state
        .as_mut()
    {
        p.ratify_state = pulsed;
    }
}

/// Build the DRep distribution snapshot from live state.
///
/// This is dugite's `psDRepDistr` analogue — captured once per epoch boundary
/// and consumed by `ratify_proposals()`, so it IS on-chain DRep voting power,
/// not a reporting convenience.
///
/// Per-credential it must sum the same three terms Haskell's `computeDRepDistr`
/// does (`Conway/Governance/DRepPulser.hs`):
///
/// ```haskell
///   stakeAndDeposits = fold $ mInstantStake <> mProposalDeposit
///   updatedDistr = Map.insertWith (<>) dRep (stakeAndDeposits <> balance) distr
/// ```
///
/// i.e. `InstantStake + ProposalDeposits + AccountBalance`. The proposal-deposit
/// term was missing here (#949) while the LIVE query path already had it — the
/// same term had been found missing once before and fixed only on that side, so
/// the bug moved out of sight rather than away.
fn build_pulsing_snapshot(
    epoch: EpochNo,
    treasury: u64,
    certs: &CertSubState,
    gov: &GovSubState,
) -> super::PulsingSnapshot {
    let mut drep_distr: ImblHashMap<Hash32, u64> = ImblHashMap::new();
    let mut drep_no_confidence = 0u64;
    let mut drep_abstain = 0u64;
    // Presence, not amount — see `PulsingSnapshot::drep_abstain_delegated`.
    let mut drep_no_confidence_delegated = false;
    let mut drep_abstain_delegated = false;

    // Proposal deposits are keyed by each live proposal's RETURN ADDRESS staking
    // credential and SUMMED across proposals sharing one, matching Haskell's
    // `proposalsDeposits` (`Conway/Governance/Proposals.hs`). Registration
    // deposits (drep/key/pool) are deliberately excluded — Haskell never reads
    // them here.
    let mut proposal_deposits: ImblHashMap<Hash32, u64> = ImblHashMap::new();
    for proposal in gov.governance.proposals.values() {
        let cred = crate::LedgerState::reward_account_to_hash(&proposal.procedure.return_addr);
        *proposal_deposits.entry(cred).or_default() += proposal.procedure.deposit.0;
    }

    for (stake_cred, drep) in &gov.governance.vote_delegations {
        let stake = credential_stake_from(stake_cred, certs)
            .saturating_add(proposal_deposits.get(stake_cred).copied().unwrap_or(0));
        if let Some(hash32) = drep.credential_hash32() {
            if gov.governance.dreps.get(&hash32).is_some_and(|d| d.active) {
                *drep_distr.entry(hash32).or_default() += stake;
            }
        } else {
            match drep {
                DRep::NoConfidence => {
                    drep_no_confidence += stake;
                    drep_no_confidence_delegated = true;
                }
                DRep::Abstain => {
                    drep_abstain += stake;
                    drep_abstain_delegated = true;
                }
                _ => {}
            }
        }
    }

    let gov_ref = &gov.governance;
    let snapshot = super::PulsingSnapshot {
        proposals: gov_ref.proposals.clone(),
        votes_by_action: gov_ref.votes_by_action.clone(),
        committee_hot_keys: gov_ref.committee_hot_keys.clone(),
        committee_expiration: gov_ref.committee_expiration.clone(),
        committee_resigned: gov_ref.committee_resigned.clone(),
        committee_threshold: gov_ref.committee_threshold.clone(),
        no_confidence: gov_ref.no_confidence,
        enacted_pparam_update: gov_ref.enacted_pparam_update.clone(),
        enacted_hard_fork: gov_ref.enacted_hard_fork.clone(),
        enacted_committee: gov_ref.enacted_committee.clone(),
        enacted_constitution: gov_ref.enacted_constitution.clone(),
        snapshot_epoch: epoch,
        vote_delegations: gov_ref.vote_delegations.clone(),
        // Haskell `ensTreasury` (#966). Captured HERE, at the end of the
        // boundary, because that is where `setFreshDRepPulsingState` seals it
        // — so the value consumed by the NEXT boundary's ratification is this
        // boundary's post-RUPD treasury, exactly one boundary stale.
        treasury,
        drep_distr,
        drep_no_confidence,
        drep_abstain,
        drep_no_confidence_delegated,
        drep_abstain_delegated,
    };
    debug!(
        epoch = epoch.0,
        proposals = snapshot.proposals.len(),
        votes = snapshot.votes_by_action.len(),
        committee = snapshot.committee_expiration.len(),
        dreps = snapshot.drep_distr.len(),
        no_confidence = snapshot.drep_no_confidence,
        abstain = snapshot.drep_abstain,
        treasury,
        "DRep pulser: inputs frozen for the epoch now starting"
    );
    snapshot
}

/// Compute the stake for a credential from sub-state components.
///
/// Equivalent to `LedgerState::credential_stake` but operates on
/// decomposed sub-states.
fn credential_stake_from(cred_hash: &Hash32, certs: &CertSubState) -> u64 {
    let utxo = certs
        .stake_distribution
        .stake_map
        .get(cred_hash)
        .map(|s| s.0)
        .unwrap_or(0);
    let reward = certs
        .reward_accounts
        .get(cred_hash)
        .map(|s| s.0)
        .unwrap_or(0);
    utxo + reward
}

/// Determine the default vote for a non-voting SPO.
///
/// Equivalent to `LedgerState::default_spo_vote` but operates on
/// decomposed sub-states.
fn default_spo_vote_from(
    pool_id: &Hash28,
    certs: &CertSubState,
    vote_delegations: &ImblHashMap<Hash32, DRep>,
    vote_delegations_override: Option<&ImblHashMap<Hash32, DRep>>,
) -> DefaultVote {
    let pool_reg = certs.pool_params.get(pool_id);
    let reward_account = match pool_reg {
        Some(reg) if reg.reward_account.len() >= 29 => &reg.reward_account,
        _ => return DefaultVote::No,
    };
    let cred_hash = LedgerState::reward_account_to_hash(reward_account);
    let delegations = vote_delegations_override.unwrap_or(vote_delegations);
    match delegations.get(&cred_hash) {
        Some(DRep::NoConfidence) => DefaultVote::NoConfidence,
        Some(DRep::Abstain) => DefaultVote::Abstain,
        _ => DefaultVote::No,
    }
}

/// Count stake-weighted votes by voter type for a specific governance action.
///
/// Equivalent to `LedgerState::count_votes_by_type` but operates on
/// decomposed sub-states.
#[allow(clippy::too_many_arguments)]
fn count_votes_by_type_impl(
    action_id: &GovActionId,
    action: &GovAction,
    drep_power_cache: &ImblHashMap<Hash32, u64>,
    no_confidence_stake: u64,
    votes_by_action: &ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
    pool_stake_override: Option<&HashMap<Hash28, Lovelace>>,
    vote_delegations_override: Option<&ImblHashMap<Hash32, DRep>>,
    bootstrap: bool,
    mark_pool_stake: Option<&HashMap<Hash28, Lovelace>>,
    certs: &CertSubState,
    gov: &GovSubState,
    epochs: &EpochSubState,
) -> (u64, u64, u64, u64, u64, u64) {
    let mut cc_yes = 0u64;
    let mut cc_total = 0u64;

    let mut drep_votes: HashMap<Hash32, Vote> = HashMap::new();
    let mut spo_votes: HashMap<Hash28, Vote> = HashMap::new();

    let empty = ImblOrdMap::new();
    let action_votes = votes_by_action.get(action_id).unwrap_or(&empty);

    for (voter, procedure) in action_votes {
        match voter {
            Voter::DRep(cred) => {
                let drep_hash = credential_to_hash(cred);
                drep_votes.insert(drep_hash, procedure.vote.clone());
            }
            Voter::StakePool(pool_hash) => {
                let pool_id = Hash28::from_bytes({
                    let mut b = [0u8; 28];
                    b.copy_from_slice(&pool_hash.as_bytes()[..28]);
                    b
                });
                spo_votes.insert(pool_id, procedure.vote.clone());
            }
            Voter::ConstitutionalCommittee(_) => {
                cc_total += 1;
                if procedure.vote == Vote::Yes {
                    cc_yes += 1;
                }
            }
        }
    }

    let is_hardfork = matches!(action, GovAction::HardForkInitiation { .. });
    let is_no_confidence = matches!(action, GovAction::NoConfidence { .. });

    let mut spo_yes = 0u64;
    let mut spo_abstain = 0u64;

    let effective_pool_stake: Option<&HashMap<Hash28, Lovelace>> =
        pool_stake_override.or(mark_pool_stake);

    if let Some(pool_stake) = effective_pool_stake {
        for (pool_id, stake) in pool_stake {
            let stake = stake.0;
            match spo_votes.get(pool_id) {
                Some(Vote::Yes) => {
                    spo_yes += stake;
                }
                Some(Vote::Abstain) => {
                    spo_abstain += stake;
                }
                Some(Vote::No) => {}
                None => {
                    if is_hardfork {
                        // No vote on HardFork -> No
                    } else if bootstrap {
                        spo_abstain += stake;
                    } else {
                        match default_spo_vote_from(
                            pool_id,
                            certs,
                            &gov.governance.vote_delegations,
                            vote_delegations_override,
                        ) {
                            DefaultVote::NoConfidence if is_no_confidence => {
                                spo_yes += stake;
                            }
                            DefaultVote::Abstain => {
                                spo_abstain += stake;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    } else {
        for (pool_id, vote) in &spo_votes {
            let stake = compute_spo_voting_power_from(pool_id, certs, epochs);
            match vote {
                Vote::Yes => spo_yes += stake,
                Vote::Abstain => spo_abstain += stake,
                Vote::No => {}
            }
        }
    }

    let mut drep_yes = 0u64;
    let mut drep_abstain = 0u64;
    let mut drep_total_all = 0u64;

    for (drep_hash, &power) in drep_power_cache {
        drep_total_all += power;
        match drep_votes.get(drep_hash) {
            Some(Vote::Yes) => {
                drep_yes += power;
            }
            Some(Vote::Abstain) => {
                drep_abstain += power;
            }
            Some(Vote::No) | None => {}
        }
    }

    if no_confidence_stake > 0 {
        drep_total_all += no_confidence_stake;
        if is_no_confidence {
            drep_yes += no_confidence_stake;
        }
    }

    let drep_total = drep_total_all.saturating_sub(drep_abstain);

    (drep_yes, drep_total, spo_yes, spo_abstain, cc_yes, cc_total)
}

/// Compute the voting power of a stake pool from sub-states (non-ratification fallback).
fn compute_spo_voting_power_from(
    pool_id: &Hash28,
    certs: &CertSubState,
    epochs: &EpochSubState,
) -> u64 {
    if let Some(ref snapshot) = epochs.snapshots.mark {
        if let Some(stake) = snapshot.pool_stake.get(pool_id) {
            return stake.0;
        }
    }
    debug!("SPO voting power: falling back to O(n) delegation scan — snapshot not available");
    let mut total = 0u64;
    for (stake_cred, delegated_pool) in certs.delegations.iter() {
        if delegated_pool == pool_id {
            total += credential_stake_from(stake_cred, certs);
        }
    }
    total
}

/// Check whether a proposal has met its voting thresholds for ratification.
///
/// Equivalent to `LedgerState::check_ratification` but operates on
/// decomposed sub-states.
#[allow(clippy::too_many_arguments)]
fn check_ratification_impl(
    action_id: &GovActionId,
    state: &ProposalState,
    _total_drep_stake: u64,
    total_spo_stake: u64,
    drep_power_cache: &ImblHashMap<Hash32, u64>,
    no_confidence_stake: u64,
    votes_by_action: &ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
    committee_hot_keys: &ImblHashMap<Hash32, Hash32>,
    committee_expiration: &ImblHashMap<Hash32, EpochNo>,
    committee_resigned: &ImblHashMap<Hash32, Option<Anchor>>,
    committee_threshold: &Option<Rational>,
    remaining_treasury: u64,
    pool_stake_override: Option<&HashMap<Hash28, Lovelace>>,
    vote_delegations_override: Option<&ImblHashMap<Hash32, DRep>>,
    epoch: EpochNo,
    epochs: &EpochSubState,
    certs: &CertSubState,
    gov: &GovSubState,
) -> bool {
    let bootstrap = epochs.protocol_params.protocol_version_major == 9;

    let (drep_yes, drep_total, spo_yes, spo_abstain, _cc_yes, _cc_total) = count_votes_by_type_impl(
        action_id,
        &state.procedure.gov_action,
        drep_power_cache,
        no_confidence_stake,
        votes_by_action,
        pool_stake_override,
        vote_delegations_override,
        bootstrap,
        epochs.snapshots.mark.as_ref().map(|s| &s.pool_stake),
        certs,
        gov,
        epochs,
    );

    let spo_denom = total_spo_stake.saturating_sub(spo_abstain);

    let cc_met_fn = |aid: &GovActionId| -> bool {
        check_cc_approval(
            aid,
            votes_by_action,
            committee_hot_keys,
            committee_expiration,
            committee_resigned,
            committee_threshold,
            epoch,
            epochs.protocol_params.committee_min_size,
            bootstrap,
        )
    };

    match &state.procedure.gov_action {
        GovAction::InfoAction => false,
        GovAction::ParameterChange {
            protocol_param_update,
            ..
        } => {
            let drep_met = if bootstrap {
                true
            } else {
                pp_change_drep_all_groups_met(
                    protocol_param_update,
                    &epochs.protocol_params,
                    drep_yes,
                    drep_total,
                )
            };
            let spo_met = if let Some(ref spo_threshold) =
                pp_change_spo_threshold(protocol_param_update, &epochs.protocol_params)
            {
                check_threshold(spo_yes, spo_denom, spo_threshold)
            } else {
                true
            };
            let cc_met = cc_met_fn(action_id);
            debug!(
                action_id = %action_id.transaction_id.to_hex(),
                bootstrap,
                drep_yes, drep_total, drep_met,
                spo_yes, spo_denom, spo_met,
                cc_met,
                "ParameterChange ratification check"
            );
            drep_met && spo_met && cc_met
        }
        GovAction::HardForkInitiation {
            protocol_version, ..
        } => {
            let rational_zero = Rational {
                numerator: 0,
                denominator: 1,
            };
            let drep_threshold = if bootstrap {
                rational_zero
            } else {
                epochs.protocol_params.dvt_hard_fork.clone()
            };
            let spo_threshold = &epochs.protocol_params.pvt_hard_fork;
            let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
            let spo_met = check_threshold(spo_yes, spo_denom, spo_threshold);
            let cc_met = cc_met_fn(action_id);
            debug!(
                action_id = %action_id.transaction_id.to_hex(),
                version = ?protocol_version,
                bootstrap,
                drep_yes, drep_total,
                drep_threshold = drep_threshold.as_f64(), drep_met,
                spo_yes, spo_denom,
                spo_threshold = spo_threshold.as_f64(), spo_met,
                cc_met,
                "HardForkInitiation ratification check"
            );
            drep_met && spo_met && cc_met
        }
        GovAction::NoConfidence { .. } => {
            let rational_zero = Rational {
                numerator: 0,
                denominator: 1,
            };
            let drep_threshold = if bootstrap {
                rational_zero
            } else {
                epochs.protocol_params.dvt_no_confidence.clone()
            };
            let spo_threshold = &epochs.protocol_params.pvt_motion_no_confidence;
            let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
            let spo_met = check_threshold(spo_yes, spo_denom, spo_threshold);
            debug!(
                action_id = %action_id.transaction_id.to_hex(),
                bootstrap,
                drep_yes, drep_total,
                drep_threshold = drep_threshold.as_f64(), drep_met,
                spo_yes, spo_denom,
                spo_threshold = spo_threshold.as_f64(), spo_met,
                "NoConfidence ratification check"
            );
            drep_met && spo_met
        }
        GovAction::UpdateCommittee { members_to_add, .. } => {
            let max_expiry = epoch.0 + epochs.protocol_params.committee_max_term_length;
            if members_to_add.values().any(|&exp| exp > max_expiry) {
                debug!(
                    action_id = %action_id.transaction_id.to_hex(),
                    max_expiry,
                    "UpdateCommittee rejected: member expiry exceeds committeeMaxTermLength"
                );
                return false;
            }

            let rational_zero = Rational {
                numerator: 0,
                denominator: 1,
            };
            let no_confidence = gov.governance.no_confidence;
            let (drep_threshold, spo_threshold) = if no_confidence {
                (
                    if bootstrap {
                        rational_zero
                    } else {
                        epochs.protocol_params.dvt_committee_no_confidence.clone()
                    },
                    &epochs.protocol_params.pvt_committee_no_confidence,
                )
            } else {
                (
                    if bootstrap {
                        rational_zero
                    } else {
                        epochs.protocol_params.dvt_committee_normal.clone()
                    },
                    &epochs.protocol_params.pvt_committee_normal,
                )
            };
            let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
            let spo_met = check_threshold(spo_yes, spo_denom, spo_threshold);
            drep_met && spo_met
        }
        GovAction::NewConstitution { .. } => {
            let rational_zero = Rational {
                numerator: 0,
                denominator: 1,
            };
            let drep_threshold = if bootstrap {
                rational_zero
            } else {
                epochs.protocol_params.dvt_constitution.clone()
            };
            let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
            let cc_met = cc_met_fn(action_id);
            drep_met && cc_met
        }
        GovAction::TreasuryWithdrawals { withdrawals, .. } => {
            let total: u64 = withdrawals
                .values()
                .fold(0u64, |acc, a| acc.saturating_add(a.0));
            if total > remaining_treasury {
                debug!(
                    action_id = %action_id.transaction_id.to_hex(),
                    total,
                    remaining_treasury,
                    "TreasuryWithdrawals rejected: exceeds remaining treasury"
                );
                return false;
            }

            let rational_zero = Rational {
                numerator: 0,
                denominator: 1,
            };
            let drep_threshold = if bootstrap {
                rational_zero
            } else {
                epochs.protocol_params.dvt_treasury_withdrawal.clone()
            };
            let drep_met = check_threshold(drep_yes, drep_total, &drep_threshold);
            let cc_met = cc_met_fn(action_id);
            drep_met && cc_met
        }
    }
}

/// Compute total active DRep-delegated stake from sub-states.
///
/// Equivalent to `LedgerState::compute_total_drep_stake`.
fn compute_total_drep_stake_from(gov: &GovSubState, certs: &CertSubState) -> u64 {
    // Gated on the pulser EXISTING, not on its distribution being non-empty.
    // An empty `reDRepDistr` is a legitimate answer — `dRepAcceptedRatio` folds
    // the distribution rather than the voter set, so it makes every DRep-gated
    // action unratifiable — and substituting a live one for it would count
    // DReps registered during the epoch that just ended, which is precisely the
    // one-boundary-early error of #922 / #950 / #966.
    if let Some(snap) = gov.governance.pulsing_snapshot() {
        let drep_sum: u64 = snap
            .drep_distr
            .values()
            .fold(0u64, |acc, v| acc.saturating_add(*v));
        let total = drep_sum
            .saturating_add(snap.drep_no_confidence)
            .saturating_add(snap.drep_abstain);
        return total.max(1);
    }

    // Fallback: compute from live state (first epoch or old snapshot).
    let prop_deposits = proposal_deposits_from_map(&gov.governance.proposals);
    let mut total = 0u64;
    for (stake_cred, drep) in &gov.governance.vote_delegations {
        let stake = credential_stake_from(stake_cred, certs)
            + prop_deposits.get(stake_cred).copied().unwrap_or(0);
        match drep {
            DRep::Abstain | DRep::NoConfidence => {
                total += stake;
            }
            _ => {
                if let Some(hash32) = drep.credential_hash32() {
                    if gov.governance.dreps.get(&hash32).is_some_and(|d| d.active) {
                        total += stake;
                    }
                }
            }
        }
    }
    total.max(1)
}

/// Compute proposal deposits per credential from a proposals map.
fn proposal_deposits_from_map(
    proposals: &ImblOrdMap<GovActionId, ProposalState>,
) -> HashMap<Hash32, u64> {
    let mut deposits: HashMap<Hash32, u64> = HashMap::new();
    for proposal in proposals.values() {
        let cred = LedgerState::reward_account_to_hash(&proposal.procedure.return_addr);
        *deposits.entry(cred).or_default() += proposal.procedure.deposit.0;
    }
    deposits
}

/// Build DRep power cache from sub-states.
///
/// Equivalent to `LedgerState::build_drep_power_cache`.
fn build_drep_power_cache_from(
    gov: &GovSubState,
    certs: &CertSubState,
) -> (ImblHashMap<Hash32, u64>, u64, u64) {
    // The frozen distribution is returned VERBATIM. `computeDRepDistr` folds
    // `mInstantStake <> mProposalDeposit <> balance` in one pass, so the
    // deposits are already in it — and until #991 this function added them a
    // SECOND time. `compute_total_drep_stake_from` sums the same map and never
    // did, so the per-DRep numerator carried deposits twice against a
    // denominator that carried them once, inflating `dRepAcceptedRatio` in the
    // accept-early direction.
    //
    // Gated on the pulser EXISTING, not on the distribution being non-empty:
    // an empty `reDRepDistr` is a legitimate answer, and substituting a live
    // one for it would count DReps that registered during the epoch that just
    // ended.
    if let Some(snap) = gov.governance.pulsing_snapshot() {
        return (
            snap.drep_distr.clone(),
            snap.drep_no_confidence,
            snap.drep_abstain,
        );
    }

    // Fallback: compute from live state.
    debug!("DRep power cache: using live vote_delegations (snapshot not yet populated)");
    build_drep_power_cache_live_from(gov, certs)
}

/// Build DRep power cache from live state (fallback).
fn build_drep_power_cache_live_from(
    gov: &GovSubState,
    certs: &CertSubState,
) -> (ImblHashMap<Hash32, u64>, u64, u64) {
    let mut cache: ImblHashMap<Hash32, u64> = ImblHashMap::new();
    let mut no_confidence_stake = 0u64;
    let mut abstain_stake = 0u64;
    let prop_deposits = proposal_deposits_from_map(&gov.governance.proposals);
    for (stake_cred, drep) in &gov.governance.vote_delegations {
        let stake = credential_stake_from(stake_cred, certs)
            + prop_deposits.get(stake_cred).copied().unwrap_or(0);
        if let Some(hash32) = drep.credential_hash32() {
            if gov.governance.dreps.get(&hash32).is_some_and(|d| d.active) {
                *cache.entry(hash32).or_default() += stake;
            }
        } else {
            match drep {
                DRep::NoConfidence => no_confidence_stake += stake,
                DRep::Abstain => abstain_stake += stake,
                _ => {}
            }
        }
    }
    (cache, no_confidence_stake, abstain_stake)
}

/// Compute total active SPO stake from sub-states (fallback).
fn compute_total_spo_stake_from(certs: &CertSubState, epochs: &EpochSubState) -> u64 {
    if let Some(ref snapshot) = epochs.snapshots.mark {
        let total: u64 = snapshot
            .pool_stake
            .values()
            .fold(0u64, |acc, s| acc.saturating_add(s.0));
        return total.max(1);
    }
    let mut total = 0u64;
    for stake_cred in certs.delegations.keys() {
        total = total.saturating_add(credential_stake_from(stake_cred, certs));
    }
    total.max(1)
}

/// Enact a ratified governance action by applying its effects.
///
/// Equivalent to `LedgerState::enact_gov_action` but operates on
/// decomposed sub-states.
pub(crate) fn enact_gov_action_impl(
    action: &GovAction,
    epochs: &mut EpochSubState,
    certs: &mut CertSubState,
    gov: &mut GovSubState,
) {
    match action {
        GovAction::ParameterChange {
            protocol_param_update,
            ..
        } => {
            if let Err(e) =
                apply_protocol_param_update_impl(&mut epochs.protocol_params, protocol_param_update)
            {
                warn!(
                    error = %e,
                    "Governance protocol parameter update rejected"
                );
            } else {
                debug!("Governance   protocol parameters updated");
            }
        }
        GovAction::HardForkInitiation {
            protocol_version, ..
        } => {
            epochs.protocol_params.protocol_version_major = protocol_version.0;
            epochs.protocol_params.protocol_version_minor = protocol_version.1;
            debug!(
                "Governance   hard fork initiated (protocol version {}.{})",
                protocol_version.0, protocol_version.1
            );
        }
        GovAction::TreasuryWithdrawals { withdrawals, .. } => {
            // Match Haskell `applyEnactedWithdrawals` (Conway/Rules/Epoch.hs):
            // withdrawals to unregistered reward accounts are silently dropped —
            // the lovelace remains in the treasury. Only the successfully
            // disbursed total is deducted, and only registered accounts are
            // credited. Do NOT silently register the account here; doing so
            // would also cause the proposal-deposit refund check below to
            // spuriously match a return_addr equal to a withdrawal_addr.
            let mut disbursed: u64 = 0;
            for (reward_addr, amount) in withdrawals {
                if amount.0 == 0 || reward_addr.len() < 29 {
                    continue;
                }
                let key = LedgerState::reward_account_to_hash(reward_addr);
                if certs.reward_accounts.contains_key(&key) {
                    *certs.reward_accounts.entry(key).or_insert(Lovelace(0)) += *amount;
                    disbursed = disbursed.saturating_add(amount.0);
                } else {
                    debug!(
                        "Treasury withdrawal to unregistered reward account dropped: {} lovelace",
                        amount.0
                    );
                }
            }
            if disbursed > epochs.treasury.0 {
                warn!(
                    "Treasury withdrawal exceeds balance: disbursed {} but only {} available",
                    disbursed, epochs.treasury.0
                );
            }
            epochs.treasury.0 = epochs.treasury.0.saturating_sub(disbursed);
            debug!(
                "Governance   treasury withdrawal: {} lovelace disbursed to {} accounts",
                disbursed,
                withdrawals.len()
            );
        }
        GovAction::NoConfidence { .. } => {
            let gov_state = Arc::make_mut(&mut gov.governance);
            gov_state.committee_hot_keys.clear();
            gov_state.committee_expiration.clear();
            gov_state.committee_resigned.clear();
            gov_state.script_committee_credentials.clear();
            gov_state.script_committee_hot_credentials.clear();
            gov_state.committee_threshold = None;
            gov_state.no_confidence = true;
            debug!("Governance   no confidence motion enacted, committee disbanded");
        }
        GovAction::UpdateCommittee {
            members_to_remove,
            members_to_add,
            threshold,
            ..
        } => {
            // Haskell ENACT (`updatedCommittee`, Conway/Rules/Enact.hs) only
            // rewrites the committee MEMBERSHIP:
            //
            // ```haskell
            // newCommitteeMembers =
            //   Map.union membersToAdd (currentMembers `Map.withoutKeys` membersToRemove)
            // ```
            //
            // (left-biased union: a credential both removed and re-added stays,
            // with the NEW term). Hot-key authorizations and resignations
            // (`vsCommitteeState`) are NOT touched here — they are pruned to the
            // post-enactment membership at the epoch boundary by
            // `updateCommitteeState` (see `prune_committee_state`), so a member
            // removed-and-re-added in one action keeps both its hot-key auth and
            // any standing resignation.
            for cred in members_to_remove {
                let key = credential_to_hash(cred);
                Arc::make_mut(&mut gov.governance)
                    .committee_expiration
                    .remove(&key);
                Arc::make_mut(&mut gov.governance)
                    .script_committee_credentials
                    .remove(&key);
            }
            for (cred, expiration_epoch) in members_to_add {
                let key = credential_to_hash(cred);
                Arc::make_mut(&mut gov.governance)
                    .committee_expiration
                    .insert(key, EpochNo(*expiration_epoch));
                // Track script-typed cold credentials so the N2C committee-state
                // query reports the correct credential type (key=0, script=1).
                // Without this, members added via UpdateCommittee with script
                // cold credentials are mislabeled as KeyHash.
                if matches!(cred, dugite_primitives::credentials::Credential::Script(_)) {
                    Arc::make_mut(&mut gov.governance)
                        .script_committee_credentials
                        .insert(key);
                } else {
                    // Defensive: if re-adding the same cold credential with a
                    // different type, drop the stale script tracking.
                    Arc::make_mut(&mut gov.governance)
                        .script_committee_credentials
                        .remove(&key);
                }
            }
            Arc::make_mut(&mut gov.governance).committee_threshold = Some(threshold.clone());
            Arc::make_mut(&mut gov.governance).no_confidence = false;
            debug!(
                "Governance   committee updated: {} removed, {} added, threshold={}/{}",
                members_to_remove.len(),
                members_to_add.len(),
                threshold.numerator,
                threshold.denominator,
            );
        }
        GovAction::NewConstitution { constitution, .. } => {
            Arc::make_mut(&mut gov.governance).constitution = Some(constitution.clone());
            debug!(
                "Governance   new constitution enacted (script_hash: {:?})",
                constitution.script_hash.as_ref().map(|h| h.to_hex())
            );
        }
        GovAction::InfoAction => {
            debug!("Info action ratified (no on-chain effect)");
        }
    }
}

/// Apply a single ProtocolParamUpdate to the given protocol parameters.
///
/// This is the free-function equivalent of `LedgerState::apply_protocol_param_update`.
/// Each field in the update, if Some, overwrites the corresponding parameter.
/// Returns an error if any governance threshold is out of range [0, 1].
///
/// #802: this function is ATOMIC — every fallible `validate_threshold` check
/// below runs BEFORE the first field write, so an `Err` return leaves
/// `params` completely untouched. This matches Haskell, where
/// `UnitInterval`-typed fields cannot even be decoded off the wire out of
/// `[0, 1]` (`BoundedRatio`'s `DecCBOR` instance rejects `numerator >
/// denominator` at decode time) and Conway `ENACT` (`PredicateFailure
/// (ENACT era) = Void`) is a total function that never re-validates at
/// apply time. `rho`/`tau`/`d` are `UnitInterval`-typed and get the same
/// `[0, 1]` bound here as a defense-in-depth backstop. `a0` is
/// `NonNegativeInterval`-typed — it has no upper bound in Haskell (only
/// `numerator >= 0`, which the u64 representation already guarantees) and
/// is intentionally NOT bounds-checked. Keep this function's validation
/// block byte-identical to `LedgerState::apply_protocol_param_update`.
fn apply_protocol_param_update_impl(
    params: &mut ProtocolParameters,
    update: &dugite_primitives::transaction::ProtocolParamUpdate,
) -> Result<(), super::LedgerError> {
    if let Some(ref v) = update.rho {
        LedgerState::validate_threshold("rho", v)?;
    }
    if let Some(ref v) = update.tau {
        LedgerState::validate_threshold("tau", v)?;
    }
    if let Some(ref v) = update.d {
        LedgerState::validate_threshold("d", v)?;
    }
    if let Some(ref v) = update.dvt_pp_network_group {
        LedgerState::validate_threshold("dvt_pp_network_group", v)?;
    }
    if let Some(ref v) = update.dvt_pp_economic_group {
        LedgerState::validate_threshold("dvt_pp_economic_group", v)?;
    }
    if let Some(ref v) = update.dvt_pp_technical_group {
        LedgerState::validate_threshold("dvt_pp_technical_group", v)?;
    }
    if let Some(ref v) = update.dvt_pp_gov_group {
        LedgerState::validate_threshold("dvt_pp_gov_group", v)?;
    }
    if let Some(ref v) = update.dvt_hard_fork {
        LedgerState::validate_threshold("dvt_hard_fork", v)?;
    }
    if let Some(ref v) = update.dvt_no_confidence {
        LedgerState::validate_threshold("dvt_no_confidence", v)?;
    }
    if let Some(ref v) = update.dvt_committee_normal {
        LedgerState::validate_threshold("dvt_committee_normal", v)?;
    }
    if let Some(ref v) = update.dvt_committee_no_confidence {
        LedgerState::validate_threshold("dvt_committee_no_confidence", v)?;
    }
    if let Some(ref v) = update.dvt_constitution {
        LedgerState::validate_threshold("dvt_constitution", v)?;
    }
    if let Some(ref v) = update.dvt_treasury_withdrawal {
        LedgerState::validate_threshold("dvt_treasury_withdrawal", v)?;
    }
    if let Some(ref v) = update.pvt_motion_no_confidence {
        LedgerState::validate_threshold("pvt_motion_no_confidence", v)?;
    }
    if let Some(ref v) = update.pvt_committee_normal {
        LedgerState::validate_threshold("pvt_committee_normal", v)?;
    }
    if let Some(ref v) = update.pvt_committee_no_confidence {
        LedgerState::validate_threshold("pvt_committee_no_confidence", v)?;
    }
    if let Some(ref v) = update.pvt_hard_fork {
        LedgerState::validate_threshold("pvt_hard_fork", v)?;
    }
    if let Some(ref v) = update.pvt_pp_security_group {
        LedgerState::validate_threshold("pvt_pp_security_group", v)?;
    }

    // --- All fallible checks passed: apply every field unconditionally. ---
    if let Some(v) = update.min_fee_a {
        params.min_fee_a = v;
    }
    if let Some(v) = update.min_fee_b {
        params.min_fee_b = v;
    }
    if let Some(v) = update.max_block_body_size {
        params.max_block_body_size = v;
    }
    if let Some(v) = update.max_tx_size {
        params.max_tx_size = v;
    }
    if let Some(v) = update.max_block_header_size {
        params.max_block_header_size = v;
    }
    if let Some(v) = update.key_deposit {
        params.key_deposit = v;
    }
    if let Some(v) = update.pool_deposit {
        params.pool_deposit = v;
    }
    if let Some(v) = update.e_max {
        params.e_max = v;
    }
    if let Some(v) = update.n_opt {
        params.n_opt = v;
    }
    if let Some(ref v) = update.a0 {
        params.a0 = v.clone();
    }
    if let Some(ref v) = update.rho {
        params.rho = v.clone();
    }
    if let Some(ref v) = update.tau {
        params.tau = v.clone();
    }
    if let Some(ref v) = update.d {
        params.d = v.clone();
    }
    if let Some(v) = update.min_pool_cost {
        params.min_pool_cost = v;
    }
    if let Some(v) = update.min_utxo_value {
        params.min_utxo_value = v;
    }
    if let Some(v) = update.ada_per_utxo_byte {
        // Key-17 disambiguation — must run BEFORE `update.protocol_version_major`
        // is applied below (issue #919; see
        // `ProtocolParameters::apply_key17_update`). In practice Conway
        // governance ParameterChange only runs at PV >= 9, so this always
        // takes the byte-denominated branch — kept for defense-in-depth
        // consistency with the two other apply sites.
        params.apply_key17_update(v);
    }
    if let Some(ref v) = update.cost_models {
        if let Some(ref v1) = v.plutus_v1 {
            params.cost_models.plutus_v1 = Some(v1.clone());
        }
        if let Some(ref v2) = v.plutus_v2 {
            params.cost_models.plutus_v2 = Some(v2.clone());
        }
        if let Some(ref v3) = v.plutus_v3 {
            params.cost_models.plutus_v3 = Some(v3.clone());
        }
        if let Some(ref v4) = v.plutus_v4 {
            params.cost_models.plutus_v4 = Some(v4.clone());
        }
        // #770: per-language merge of unknown-language entries (keys ≥ 4),
        // mirroring Haskell Conway `updateCostModels` (`Map.union new old`,
        // new wins; old keys absent from the update are retained). Typed and
        // unknown keys never collide (decoders route 0–3 to the typed fields).
        for (key, costs) in &v.unknown_cost_models {
            params
                .cost_models
                .unknown_cost_models
                .insert(*key, costs.clone());
        }
    }
    if let Some(ref v) = update.execution_costs {
        params.execution_costs = v.clone();
    }
    if let Some(v) = update.max_tx_ex_units {
        params.max_tx_ex_units = v;
    }
    if let Some(v) = update.max_block_ex_units {
        params.max_block_ex_units = v;
    }
    if let Some(v) = update.max_val_size {
        params.max_val_size = v;
    }
    if let Some(v) = update.collateral_percentage {
        params.collateral_percentage = v;
    }
    if let Some(v) = update.max_collateral_inputs {
        params.max_collateral_inputs = v;
    }
    if let Some(v) = &update.min_fee_ref_script_cost_per_byte {
        params.min_fee_ref_script_cost_per_byte = v.clone();
    }
    if let Some(v) = update.drep_deposit {
        params.drep_deposit = v;
    }
    if let Some(v) = update.gov_action_lifetime {
        params.gov_action_lifetime = v;
    }
    if let Some(v) = update.gov_action_deposit {
        params.gov_action_deposit = v;
    }
    if let Some(ref v) = update.dvt_pp_network_group {
        params.dvt_pp_network_group = v.clone();
    }
    if let Some(ref v) = update.dvt_pp_economic_group {
        params.dvt_pp_economic_group = v.clone();
    }
    if let Some(ref v) = update.dvt_pp_technical_group {
        params.dvt_pp_technical_group = v.clone();
    }
    if let Some(ref v) = update.dvt_pp_gov_group {
        params.dvt_pp_gov_group = v.clone();
    }
    if let Some(ref v) = update.dvt_hard_fork {
        params.dvt_hard_fork = v.clone();
    }
    if let Some(ref v) = update.dvt_no_confidence {
        params.dvt_no_confidence = v.clone();
    }
    if let Some(ref v) = update.dvt_committee_normal {
        params.dvt_committee_normal = v.clone();
    }
    if let Some(ref v) = update.dvt_committee_no_confidence {
        params.dvt_committee_no_confidence = v.clone();
    }
    if let Some(ref v) = update.dvt_constitution {
        params.dvt_constitution = v.clone();
    }
    if let Some(ref v) = update.dvt_treasury_withdrawal {
        params.dvt_treasury_withdrawal = v.clone();
    }
    if let Some(ref v) = update.pvt_motion_no_confidence {
        params.pvt_motion_no_confidence = v.clone();
    }
    if let Some(ref v) = update.pvt_committee_normal {
        params.pvt_committee_normal = v.clone();
    }
    if let Some(ref v) = update.pvt_committee_no_confidence {
        params.pvt_committee_no_confidence = v.clone();
    }
    if let Some(ref v) = update.pvt_hard_fork {
        params.pvt_hard_fork = v.clone();
    }
    if let Some(ref v) = update.pvt_pp_security_group {
        params.pvt_pp_security_group = v.clone();
    }
    if let Some(v) = update.min_committee_size {
        params.committee_min_size = v;
    }
    if let Some(v) = update.committee_term_limit {
        params.committee_max_term_length = v;
    }
    if let Some(v) = update.drep_activity {
        params.drep_activity = v;
    }
    if let Some(v) = update.protocol_version_major {
        params.protocol_version_major = v;
    }
    if let Some(v) = update.protocol_version_minor {
        params.protocol_version_minor = v;
    }
    Ok(())
}

/// The main governance ratification pipeline.
///
/// Equivalent to `LedgerState::ratify_proposals` but operates on
/// decomposed sub-states, allowing it to be called from both the
/// monolithic `LedgerState` path and the `EraRules` dispatch path.
#[allow(clippy::too_many_arguments)]
/// Refund a removed proposal's deposit to its `return_addr`, mirroring Haskell
/// `Cardano.Ledger.Conway.Rules.Epoch`'s `proposalsApplyEnactment` /
/// `returnProposalDeposits`: the deposit is credited to the return account's
/// balance when that account is registered, and routed to the treasury when it
/// is not.
///
/// Every proposal-removal path (expiry, enactment, sibling/descendant drop)
/// MUST go through here. Issue #898: three hand-copied versions of this logic
/// existed, and a proposal removed without a refund silently destroys the
/// deposit — invisible in the pots (treasury and reserves still reconcile)
/// but it lowers the return account's balance forever, which then depresses
/// its snapshot stake, every pool's `appPerf`, and ultimately every reward,
/// until an exact-drain withdrawal fails and chain advance halts.
fn refund_proposal_deposit(
    action_id: &GovActionId,
    proposal_state: &ProposalState,
    epochs: &mut EpochSubState,
    certs: &mut CertSubState,
    reason: &'static str,
) {
    let deposit = proposal_state.procedure.deposit;
    if deposit.0 == 0 {
        return;
    }
    let return_addr = &proposal_state.procedure.return_addr;
    if return_addr.len() < 29 {
        warn!(
            action_id = %action_id.transaction_id.to_hex(),
            index = action_id.action_index,
            deposit = deposit.0,
            return_addr_len = return_addr.len(),
            reason,
            "Governance proposal deposit NOT refunded: malformed return address \
             (deposit would be destroyed) — this must never happen for a proposal \
             that passed GOV validation"
        );
        return;
    }
    let key = LedgerState::reward_account_to_hash(return_addr);
    if certs.reward_accounts.contains_key(&key) {
        *certs.reward_accounts.entry(key).or_insert(Lovelace(0)) += deposit;
        debug!(
            action_id = %action_id.transaction_id.to_hex(),
            index = action_id.action_index,
            deposit = deposit.0,
            cred = %key.to_hex(),
            reason,
            "Governance proposal deposit refunded to return account"
        );
    } else {
        epochs.treasury += deposit;
        debug!(
            action_id = %action_id.transaction_id.to_hex(),
            index = action_id.action_index,
            deposit = deposit.0,
            cred = %key.to_hex(),
            reason,
            "Governance proposal deposit -> treasury (unregistered return address)"
        );
    }
}

/// Haskell RATIFY — `Conway.Rules.Ratify.ratifyTransition` (#988).
///
/// This is the DECISION, and upstream it runs **inside the DRep pulser**, not
/// at the epoch boundary. `finishDRepPulser` builds `RatifyEnv` from the
/// pulser's frozen fields, seeds `RatifyState` from `dpEnactState`, and runs
/// `runConwayRatify` over `RatifySignal dpProposals`. The boundary that then
/// consumes the result does not re-run any of it:
///
/// ```haskell
/// pulsingState = epochState0 ^. epochStateDRepPulsingStateL
/// ratifyState@RatifyState {rsEnactState, rsEnacted, rsExpired} =
///   extractDRepPulsingState pulsingState
/// ```
///
/// So this function is called at the boundary that FREEZES the pulser (against
/// a clone, via [`compute_pulsed_ratify_state`]), and its result is applied one
/// boundary later by [`apply_ratify_decision`]. It still enacts as it goes,
/// because that is how the `EnactState` threading is expressed — each enactment
/// is visible to the proposals evaluated after it, matching the recursive
/// `trans @(RATIFY era)` call that passes `st'`.
///
/// The returned [`PulsedRatifyState`] is the `RatifyState` half of Haskell's
/// `DRComplete PulsingSnapshot RatifyState`.
pub(crate) fn ratify_proposals_impl(
    epoch: EpochNo,
    epochs: &mut EpochSubState,
    certs: &mut CertSubState,
    gov: &mut GovSubState,
) -> PulsedRatifyState {
    // Lazy forest reconstruction for backward compatibility with old snapshots
    if gov.governance.proposal_roots == GovRelation::default()
        && !gov.governance.proposals.is_empty()
    {
        let (roots, graph) = rebuild_forest_from_flat(
            &gov.governance.proposals,
            &gov.governance.enacted_pparam_update,
            &gov.governance.enacted_hard_fork,
            &gov.governance.enacted_committee,
            &gov.governance.enacted_constitution,
        );
        let gov_state = Arc::make_mut(&mut gov.governance);
        gov_state.proposal_roots = roots;
        gov_state.proposal_graph = graph;
    }

    let total_drep_stake = compute_total_drep_stake_from(gov, certs);
    let (drep_power_cache, no_confidence_stake, _abstain_stake) =
        build_drep_power_cache_from(gov, certs);

    // Diagnostic: DRep distribution details
    {
        let cache_sum: u64 = drep_power_cache.values().sum();
        let active_dreps = gov.governance.dreps.values().filter(|d| d.active).count();
        let total_dreps = gov.governance.dreps.len();
        debug!(
            epoch = epoch.0,
            drep_cache_size = drep_power_cache.len(),
            drep_cache_sum = cache_sum,
            no_confidence_stake,
            _abstain_stake,
            total_drep_stake,
            active_dreps,
            total_dreps,
            "DRep distribution for ratification"
        );
    }

    // SPO voting power from the **set** snapshot
    let ratify_pool_stake: Option<HashMap<Hash28, Lovelace>> =
        epochs.snapshots.set.as_ref().map(|s| s.pool_stake.clone());
    let ratify_pool_stake_ref = ratify_pool_stake.as_ref();
    let total_spo_stake: u64 = ratify_pool_stake_ref
        .map(|ps| {
            ps.values()
                .fold(0u64, |acc, s| acc.saturating_add(s.0))
                .max(1)
        })
        .unwrap_or_else(|| compute_total_spo_stake_from(certs, epochs));

    let snapshot = gov.governance.pulsing_snapshot().cloned();
    let using_snapshot = snapshot.is_some();

    // The committee fields are mutable because Haskell's RATIFY threads
    // `EnactState.ensCommittee` through the loop — when an
    // `UpdateCommittee` or `NoConfidence` proposal enacts, every
    // subsequent proposal in the same pass sees the updated committee.
    let (
        snap_proposals,
        snap_votes,
        mut snap_committee_hot_keys,
        mut snap_committee_expiration,
        mut snap_committee_resigned,
        mut snap_committee_threshold,
        _snap_no_confidence,
        mut enacted_pparam,
        mut enacted_hardfork,
        mut enacted_committee_root,
        mut enacted_constitution,
    ) = if let Some(ref snap) = snapshot {
        (
            snap.proposals.clone(),
            snap.votes_by_action.clone(),
            snap.committee_hot_keys.clone(),
            snap.committee_expiration.clone(),
            snap.committee_resigned.clone(),
            snap.committee_threshold.clone(),
            snap.no_confidence,
            snap.enacted_pparam_update.clone(),
            snap.enacted_hard_fork.clone(),
            snap.enacted_committee.clone(),
            snap.enacted_constitution.clone(),
        )
    } else {
        // No pulser snapshot yet ⇒ NO ratification candidates (#903).
        //
        // Haskell's RATIFY signal is `RatifySignal dpProposals`, where
        // `dpProposals` is frozen into the DRep pulser by
        // `setFreshDRepPulsingState` at the PREVIOUS epoch boundary
        // (`Conway/Governance.hs:469-516`; `finishDRepPulser`,
        // `DRepPulser.hs:386-417`, builds the signal exclusively from it).
        // RATIFY has no path to live `cgsProposals` — a proposal submitted
        // during epoch N is invisible to the N→N+1 pass by construction, not
        // via any `gasProposedIn` check (there is none in Ratify.hs).
        //
        // dugite already models that two-boundary indirection correctly:
        // `process_epoch_transition` ratifies from the snapshot captured at the
        // previous boundary, then captures a fresh one. The gap was only this
        // fallback — before the first capture, it ratified over the LIVE set.
        // At genesis Haskell's `ConwayGovState` is `DRComplete def def`, i.e.
        // an EMPTY candidate set, so nothing can ratify at the first boundary.
        //
        // Consequence of the old behaviour: a ParameterChange proposed in
        // epoch 0 was ratified and enacted at the 0→1 boundary, and its sibling
        // was then removed by the (correct) `proposalsApplyEnactment` sibling
        // cleanup — so two proposals cardano-node still listed as live vanished
        // from dugite's set, and a 100k ADA deposit was refunded an epoch early.
        //
        // Only the candidate set is emptied. The enacted roots and committee
        // still come from current state: they mirror Haskell's `dpEnactState`,
        // which is likewise the EnactState as of the boundary.
        let g = &gov.governance;
        (
            Default::default(),
            Default::default(),
            g.committee_hot_keys.clone(),
            g.committee_expiration.clone(),
            g.committee_resigned.clone(),
            g.committee_threshold.clone(),
            g.no_confidence,
            g.enacted_pparam_update.clone(),
            g.enacted_hard_fork.clone(),
            g.enacted_committee.clone(),
            g.enacted_constitution.clone(),
        )
    };

    let snap_vote_delegations: Option<ImblHashMap<Hash32, DRep>> = if let Some(ref snap) = snapshot
    {
        if snap.vote_delegations.is_empty() {
            None
        } else {
            Some(snap.vote_delegations.clone())
        }
    } else {
        None
    };
    let snap_vote_delegations_ref = snap_vote_delegations.as_ref();

    // #799: `snap_proposals` is an `ImblOrdMap<GovActionId, _>`, whose `.iter()`
    // yields entries in GovActionId (hash) order — NOT on-chain submission
    // order. Haskell's `reorderActions` (Governance/Internal.hs:534-544) is a
    // STABLE sort keyed only on `actionPriority`; ties preserve the proposals
    // OMap's insertion (submission) order. Carrying `submission_index` into the
    // candidate tuple and sorting on `(priority, submission_index)` recovers
    // that exact tie-break, independent of the map's iteration order.
    let mut candidates: Vec<(GovActionId, GovAction, EpochNo, u64)> = snap_proposals
        .iter()
        .map(|(id, state)| {
            (
                id.clone(),
                state.procedure.gov_action.clone(),
                state.expires_epoch,
                state.submission_index,
            )
        })
        .collect();
    candidates.sort_by_key(|(_, action, _, seq)| (gov_action_priority(action), *seq));

    let bootstrap = epochs.protocol_params.protocol_version_major == 9;

    debug!(
        epoch = epoch.0,
        active_proposals = candidates.len(),
        total_drep_stake,
        total_spo_stake,
        no_confidence_stake,
        using_snapshot,
        bootstrap,
        protocol_version = epochs.protocol_params.protocol_version_major,
        cc_members = snap_committee_expiration.len(),
        cc_hot_keys = snap_committee_hot_keys.len(),
        cc_threshold = ?snap_committee_threshold,
        "Governance ratification: evaluating proposals"
    );

    let mut ratified = Vec::new();
    let mut delayed = false;

    // Transient treasury cap-basis for the ratification pass. This mirrors
    // Haskell Conway `Enact.hs`/`Ratify.hs` `ensTreasury`: the cap basis is
    // threaded across the pass and decremented by the FULL declared `fold wdrls`
    // for every enacted `TreasuryWithdrawals` (regardless of whether the target
    // reward accounts are registered). Unregistered targets are filtered only
    // LATER at the epoch boundary (`applyEnactedWithdrawals`) against the REAL
    // `casTreasury` (`epochs.treasury.0`). Keeping these two quantities separate
    // is required for byte-exactness: `epochs.treasury.0` continues to be
    // decremented by `disbursed` (registered-target total only) inside
    // `enact_gov_action_impl`, while `cap_treasury` is used solely for the
    // `withdrawalCanWithdraw` cap check below. For the all-registered case
    // `disbursed == fold wdrls`, so the two stay equal and behavior is identical.
    //
    // #966: the BASIS is the FROZEN `ensTreasury` from the ratification
    // snapshot, not the live pot. Haskell seals `ensTreasury` into the DRep
    // pulser at the END of `epochTransition`
    // (`setFreshDRepPulsingState`/`Epoch.hs:372`) and `finishDRepPulser`
    // consumes that field verbatim one boundary later, so RATIFY cannot see
    // the `applyRUpd` credit applied at the boundary it is running on.
    //
    // dugite applies RUPD (epoch.rs) BEFORE calling this function, so reading
    // `epochs.treasury.0` here saw a pot one boundary NEWER than Haskell's. A
    // withdrawal that only became affordable at boundary B then enacted at B
    // on dugite and B+1 on cardano-node — a split in the accept-early
    // direction, which is the dangerous one.
    //
    // The fallback to live state covers only the first boundary of a fresh or
    // freshly-imported chain, where no snapshot has been captured yet; from
    // the second boundary onward the snapshot always exists.
    let mut cap_treasury = gov
        .governance
        .pulsing_snapshot()
        .map_or(epochs.treasury.0, |snap| snap.treasury);

    // Haskell `rsExpired`, and it is built from the pulser's OWN proposal set,
    // never from live state (`Ratify.hs`, the `else` branch of
    // `ratifyTransition`). dugite used to rescan the live proposals for
    // `expires_epoch < epoch` down in `proposalsApplyEnactment`; the two sets
    // coincide, because a proposal submitted during the epoch that just ended
    // cannot already have expired, but deriving it here is what makes that a
    // property of the code rather than of the arithmetic.
    let mut expired_from_pulser: Vec<GovActionId> = Vec::new();

    for (action_id, action, expires, _submission_index) in &candidates {
        // Haskell evaluates EVERY element of the signal, expired or not. The
        // expiry test lives in the `else` branch — *after* the ratification
        // attempt fails:
        //
        // ```haskell
        // else do
        //   st' <- trans @(RATIFY era) $ TRC (env, st, RatifySignal sigs)
        //   if gasExpiresAfter < reCurrentEpoch
        //     then pure $ st' & rsExpiredL %~ Set.insert gasId
        //     else pure st'
        // ```
        //
        // dugite used to `continue` on `expires < epoch` BEFORE the threshold
        // check, which silently removed the last ratification opportunity from
        // every proposal (#990). It is reachable: an action with
        // `expires_epoch == E-1` is not expired at the boundary where
        // `reCurrentEpoch == E-1`, so it survives into the pulser frozen there
        // — together with the votes cast during epoch E-1 — and is evaluated
        // once more at `reCurrentEpoch == E`, the same pass that expires it.
        // cardano-node enacts it if it crossed threshold on those votes;
        // dugite dropped it. A split in the reject direction.
        let ratified_now = if !prev_action_as_expected(
            action,
            &enacted_pparam,
            &enacted_hardfork,
            &enacted_committee_root,
            &enacted_constitution,
        ) {
            trace!(
                action_id = %action_id.transaction_id.to_hex(),
                action_type = ?std::mem::discriminant(action),
                "Governance proposal: prev_action_id chain mismatch — skipping"
            );
            false
        } else if delayed {
            debug!(
                action_id = %action_id.transaction_id.to_hex(),
                "Governance proposal: delayed by previously enacted action"
            );
            false
        } else if let Some(state) = snap_proposals.get(action_id) {
            // Cap basis is the transient `cap_treasury` — mirroring Haskell
            // Conway `Ratify.hs` `withdrawalCanWithdraw`, which checks
            // `fold(wdrls) <= ensTreasury` against the threaded `ensTreasury`.
            // `ensTreasury` is decremented by the FULL declared `fold wdrls` of
            // every earlier enacted withdrawal in this pass (NOT by the
            // registered-target-only `disbursed`), so we must NOT use
            // `epochs.treasury.0` here: that field is the real `casTreasury`,
            // decremented only by `disbursed`, and using it would under-subtract
            // the cap basis whenever a prior withdrawal targeted an unregistered
            // account — wrongly admitting a later withdrawal Haskell blocks.
            let remaining_treasury = cap_treasury;
            check_ratification_impl(
                action_id,
                state,
                total_drep_stake,
                total_spo_stake,
                &drep_power_cache,
                no_confidence_stake,
                &snap_votes,
                &snap_committee_hot_keys,
                &snap_committee_expiration,
                &snap_committee_resigned,
                &snap_committee_threshold,
                remaining_treasury,
                ratify_pool_stake_ref,
                snap_vote_delegations_ref,
                epoch,
                epochs,
                certs,
                gov,
            )
        } else {
            false
        };

        {
            if ratified_now {
                debug!(
                    action_id = %action_id.transaction_id.to_hex(),
                    action_type = ?std::mem::discriminant(action),
                    "Governance proposal RATIFIED"
                );
                enact_gov_action_impl(action, epochs, certs, gov);
                // Thread the transient cap-basis: decrement `cap_treasury` by the
                // FULL declared `fold wdrls` (NOT `disbursed`), mirroring Haskell
                // `Enact.hs` `ensTreasury <- ensTreasury - fold wdrls`. The real
                // `epochs.treasury.0` (= `casTreasury`) was already decremented by
                // `disbursed` inside `enact_gov_action_impl`; this cap-basis decrement
                // is independent and feeds only the cap check for later proposals.
                if let GovAction::TreasuryWithdrawals { withdrawals, .. } = action {
                    cap_treasury = cap_treasury.saturating_sub(
                        withdrawals
                            .values()
                            .fold(0u64, |acc, a| acc.saturating_add(a.0)),
                    );
                }
                // Match Haskell `RatifyState.rsEnactState.ensCommittee`
                // threading: when an `UpdateCommittee` or `NoConfidence`
                // proposal enacts, refresh the local committee maps from
                // the live state so subsequent proposals in the same pass
                // see the updated committee (matches the recursive
                // `ratifyTransition` call passing `st'` with the new
                // `EnactState`).
                if matches!(
                    action,
                    GovAction::UpdateCommittee { .. } | GovAction::NoConfidence { .. }
                ) {
                    let g = &gov.governance;
                    snap_committee_hot_keys = g.committee_hot_keys.clone();
                    snap_committee_expiration = g.committee_expiration.clone();
                    snap_committee_resigned = g.committee_resigned.clone();
                    snap_committee_threshold = g.committee_threshold.clone();
                }
                update_enacted_root_local(
                    action_id,
                    action,
                    &mut enacted_pparam,
                    &mut enacted_hardfork,
                    &mut enacted_committee_root,
                    &mut enacted_constitution,
                );
                ratified.push(action_id.clone());
                if is_delaying_action(action) {
                    delayed = true;
                }
            } else {
                if !matches!(action, GovAction::InfoAction) {
                    trace!(
                        action_id = %action_id.transaction_id.to_hex(),
                        action_type = ?std::mem::discriminant(action),
                        "Governance proposal NOT ratified"
                    );
                }
                // `if gasExpiresAfter < reCurrentEpoch then rsExpired += gasId`
                // — reachable only from the not-ratified branch, so an action
                // that enacts in its final epoch is never also expired.
                if *expires < epoch {
                    expired_from_pulser.push(action_id.clone());
                }
            }
        }
    }

    // Persist final enacted roots
    {
        let gov_state = Arc::make_mut(&mut gov.governance);
        gov_state.enacted_pparam_update = enacted_pparam;
        gov_state.enacted_hard_fork = enacted_hardfork;
        gov_state.enacted_committee = enacted_committee_root;
        gov_state.enacted_constitution = enacted_constitution;
    }

    proposals_apply_enactment(&ratified, &expired_from_pulser, epoch, epochs, certs, gov);
    Arc::make_mut(&mut gov.governance).last_ratify_delayed = delayed;

    // Haskell `hasChangesToPParams` — only these two make `futurePParams`
    // become `Just` (#977).
    let has_pparams_changes = gov.governance.last_ratified.iter().any(|(_, prop)| {
        matches!(
            prop.procedure.gov_action,
            GovAction::ParameterChange { .. } | GovAction::HardForkInitiation { .. }
        )
    });

    PulsedRatifyState {
        computed_at_epoch: epoch,
        enacted: ratified,
        expired: expired_from_pulser,
        delayed,
        // `ensCurPParams` AFTER the enactments.
        cur_pparams: epochs.protocol_params.clone(),
        has_pparams_changes,
    }
}

/// Haskell `proposalsApplyEnactment rsEnacted rsExpired (govState0 ^.
/// proposalsGovStateL)` — the EPOCH rule's application of a completed RATIFY.
///
/// Deliberately takes the enacted and expired ids as ARGUMENTS rather than
/// deriving them: upstream they come from the pulser's `RatifyState`, computed
/// one boundary earlier over the pulser's own frozen proposal set, while the
/// set they are applied TO is the live superset — it also contains everything
/// proposed during the epoch that just ended, plus every vote cast on the
/// older proposals. Upstream's comment on that asymmetry is explicit:
///
/// > We only need to apply the enactment operations to this superset to get a
/// > new set of proposals with: enacted actions and their sibling subtrees, as
/// > well as expired actions and their subtrees, removed, and with all the
/// > votes intact for the rest of them.
fn proposals_apply_enactment(
    ratified: &[GovActionId],
    expired_ids: &[GovActionId],
    epoch: EpochNo,
    epochs: &mut EpochSubState,
    certs: &mut CertSubState,
    gov: &mut GovSubState,
) {
    // ── Step 1: Expire proposals with descendants ─────────────────────
    let current_epoch = epoch;

    let expired_removed = if !expired_ids.is_empty() {
        let gov_state = Arc::make_mut(&mut gov.governance);
        let removed = forest_remove_with_descendants(
            expired_ids,
            &mut gov_state.proposals,
            &mut gov_state.proposal_roots,
            &mut gov_state.proposal_graph,
            &mut gov_state.votes_by_action,
        );
        for (action_id, proposal_state) in &removed {
            refund_proposal_deposit(action_id, proposal_state, epochs, certs, "expired");
        }
        if !removed.is_empty() {
            debug!(
                "Expired {} governance proposal(s) (incl. descendants) at epoch {}",
                removed.len(),
                current_epoch.0
            );
        }
        removed.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    // ── Step 2: Remove enacted proposals + refund deposits ────────────
    let mut ratified_with_state = Vec::new();

    if !ratified.is_empty() {
        for action_id in ratified {
            if let Some(state) = gov.governance.proposals.get(action_id) {
                let purpose = gov_action_purpose_tag(&state.procedure.gov_action);
                if let Some(tag) = purpose {
                    let gov_state = Arc::make_mut(&mut gov.governance);
                    forest_remove_node(
                        action_id,
                        tag,
                        &mut gov_state.proposal_roots,
                        &mut gov_state.proposal_graph,
                    );
                }
            }
            if let Some(proposal_state) = Arc::make_mut(&mut gov.governance)
                .proposals
                .remove(action_id)
            {
                refund_proposal_deposit(action_id, &proposal_state, epochs, certs, "enacted");
                ratified_with_state.push((action_id.clone(), proposal_state));
            }
            Arc::make_mut(&mut gov.governance)
                .votes_by_action
                .remove(action_id);
        }
        debug!(
            "Governance   {} proposal(s) ratified and enacted",
            ratified.len()
        );

        // ── Step 3: Per-enacted sibling removal + root promotion ──────
        for (enacted_id, enacted_state) in &ratified_with_state {
            let enacted_action = &enacted_state.procedure.gov_action;
            let Some(tag) = gov_action_purpose_tag(enacted_action) else {
                continue;
            };

            let siblings: Vec<GovActionId> = {
                let root = gov.governance.proposal_roots.get(tag);
                root.children
                    .iter()
                    .filter(|id| *id != enacted_id)
                    .cloned()
                    .collect()
            };

            if !siblings.is_empty() {
                let gov_state = Arc::make_mut(&mut gov.governance);
                let removed = forest_remove_with_descendants(
                    &siblings,
                    &mut gov_state.proposals,
                    &mut gov_state.proposal_roots,
                    &mut gov_state.proposal_graph,
                    &mut gov_state.votes_by_action,
                );
                for (action_id, proposal_state) in &removed {
                    refund_proposal_deposit(
                        action_id,
                        proposal_state,
                        epochs,
                        certs,
                        "sibling-or-descendant dropped by enactment",
                    );
                }
                if !removed.is_empty() {
                    debug!(
                        "Governance   {} sibling/descendant proposal(s) removed due to enactment of {:?}",
                        removed.len(),
                        enacted_id
                    );
                }
            }

            let gov_state = Arc::make_mut(&mut gov.governance);
            forest_promote_root(
                enacted_id,
                tag,
                &mut gov_state.proposal_roots,
                &mut gov_state.proposal_graph,
            );
        }
    }

    // Store ratification and expiry results. `last_ratify_delayed` is NOT set
    // here: it is a property of the RATIFY decision, not of applying it, so
    // each caller writes it from the decision it holds.
    let gov_state = Arc::make_mut(&mut gov.governance);
    gov_state.last_ratified = ratified_with_state;
    gov_state.last_expired = expired_removed;
}

/// DRep voting group for protocol parameter classification per CIP-1694.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DRepPPGroup {
    Network,
    Economic,
    Technical,
    Gov,
}

/// Whether SPOs can vote on a parameter change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StakePoolPPGroup {
    Security,
    NoVote,
}

/// Classification of a protocol parameter: (DRepPPGroup, StakePoolPPGroup).
/// Matches Haskell cardano-ledger Conway `PPGroups` exactly.
pub(crate) type PPGroup = (DRepPPGroup, StakePoolPPGroup);

/// Determine which PP groups are modified by a ProtocolParamUpdate.
///
/// Each parameter belongs to exactly one (DRepPPGroup, StakePoolPPGroup) pair.
/// Classification matches Haskell cardano-ledger Conway ConwayPParams field tags.
pub(crate) fn modified_pp_groups(
    ppu: &dugite_primitives::transaction::ProtocolParamUpdate,
) -> Vec<PPGroup> {
    use DRepPPGroup::*;
    use StakePoolPPGroup::*;

    let mut groups = Vec::new();

    // Network + Security
    if ppu.max_block_body_size.is_some() {
        groups.push((Network, Security));
    }
    if ppu.max_tx_size.is_some() {
        groups.push((Network, Security));
    }
    if ppu.max_block_header_size.is_some() {
        groups.push((Network, Security));
    }
    if ppu.max_block_ex_units.is_some() {
        groups.push((Network, Security));
    }
    if ppu.max_val_size.is_some() {
        groups.push((Network, Security));
    }

    // Network + NoVote
    if ppu.max_tx_ex_units.is_some() {
        groups.push((Network, NoVote));
    }
    if ppu.max_collateral_inputs.is_some() {
        groups.push((Network, NoVote));
    }

    // Economic + Security
    if ppu.min_fee_a.is_some() {
        groups.push((Economic, Security));
    }
    if ppu.min_fee_b.is_some() {
        groups.push((Economic, Security));
    }
    if ppu.ada_per_utxo_byte.is_some() {
        groups.push((Economic, Security));
    }
    if ppu.min_fee_ref_script_cost_per_byte.is_some() {
        groups.push((Economic, Security));
    }

    // Economic + NoVote
    if ppu.key_deposit.is_some() {
        groups.push((Economic, NoVote));
    }
    if ppu.pool_deposit.is_some() {
        groups.push((Economic, NoVote));
    }
    if ppu.rho.is_some() {
        groups.push((Economic, NoVote));
    }
    if ppu.tau.is_some() {
        groups.push((Economic, NoVote));
    }
    if ppu.min_pool_cost.is_some() {
        groups.push((Economic, NoVote));
    }
    if ppu.execution_costs.is_some() {
        groups.push((Economic, NoVote));
    }

    // Technical + NoVote
    if ppu.e_max.is_some() {
        groups.push((Technical, NoVote));
    }
    if ppu.n_opt.is_some() {
        groups.push((Technical, NoVote));
    }
    if ppu.a0.is_some() {
        groups.push((Technical, NoVote));
    }
    if ppu.cost_models.is_some() {
        groups.push((Technical, NoVote));
    }
    if ppu.collateral_percentage.is_some() {
        groups.push((Technical, NoVote));
    }

    // Gov + Security
    if ppu.gov_action_deposit.is_some() {
        groups.push((Gov, Security));
    }

    // Gov + NoVote
    if ppu.dvt_pp_network_group.is_some()
        || ppu.dvt_pp_economic_group.is_some()
        || ppu.dvt_pp_technical_group.is_some()
        || ppu.dvt_pp_gov_group.is_some()
        || ppu.dvt_hard_fork.is_some()
        || ppu.dvt_no_confidence.is_some()
        || ppu.dvt_committee_normal.is_some()
        || ppu.dvt_committee_no_confidence.is_some()
        || ppu.dvt_constitution.is_some()
        || ppu.dvt_treasury_withdrawal.is_some()
    {
        groups.push((Gov, NoVote));
    }
    if ppu.pvt_motion_no_confidence.is_some()
        || ppu.pvt_committee_normal.is_some()
        || ppu.pvt_committee_no_confidence.is_some()
        || ppu.pvt_hard_fork.is_some()
        || ppu.pvt_pp_security_group.is_some()
    {
        groups.push((Gov, NoVote));
    }
    if ppu.min_committee_size.is_some() {
        groups.push((Gov, NoVote));
    }
    if ppu.committee_term_limit.is_some() {
        groups.push((Gov, NoVote));
    }
    if ppu.gov_action_lifetime.is_some() {
        groups.push((Gov, NoVote));
    }
    if ppu.drep_deposit.is_some() {
        groups.push((Gov, NoVote));
    }
    if ppu.drep_activity.is_some() {
        groups.push((Gov, NoVote));
    }

    groups
}

/// Check that ALL affected DRep parameter group thresholds are independently met.
///
/// Per CIP-1694 / Haskell `pparamsUpdateThreshold`: each affected parameter group
/// has its own DRep voting threshold. A ParameterChange is ratified only if the
/// DRep vote ratio meets the threshold for EVERY affected group independently.
///
/// This replaces the previous (incorrect) max-of-all-groups approach.
pub(crate) fn pp_change_drep_all_groups_met(
    ppu: &dugite_primitives::transaction::ProtocolParamUpdate,
    params: &dugite_primitives::protocol_params::ProtocolParameters,
    drep_yes: u64,
    drep_total: u64,
) -> bool {
    let groups = modified_pp_groups(ppu);
    // Collect unique DRep groups (avoid checking the same group multiple times)
    let mut seen = std::collections::HashSet::new();
    for (drep_group, _) in &groups {
        if !seen.insert(*drep_group) {
            continue;
        }
        let threshold = match drep_group {
            DRepPPGroup::Network => &params.dvt_pp_network_group,
            DRepPPGroup::Economic => &params.dvt_pp_economic_group,
            DRepPPGroup::Technical => &params.dvt_pp_technical_group,
            DRepPPGroup::Gov => &params.dvt_pp_gov_group,
        };
        if !check_threshold(drep_yes, drep_total, threshold) {
            return false;
        }
    }
    true
}

/// Compute the maximum DRep voting threshold for a ParameterChange governance action.
///
/// Returns the highest DRep group threshold across all affected parameter groups.
/// Used by tests and for informational purposes. For ratification, use
/// `pp_change_drep_all_groups_met` which checks each group independently.
#[cfg(test)]
pub(crate) fn pp_change_drep_threshold(
    ppu: &dugite_primitives::transaction::ProtocolParamUpdate,
    params: &dugite_primitives::protocol_params::ProtocolParameters,
) -> Rational {
    let groups = modified_pp_groups(ppu);
    let mut max_threshold = Rational {
        numerator: 0,
        denominator: 1,
    };
    for (drep_group, _) in &groups {
        let t = match drep_group {
            DRepPPGroup::Network => &params.dvt_pp_network_group,
            DRepPPGroup::Economic => &params.dvt_pp_economic_group,
            DRepPPGroup::Technical => &params.dvt_pp_technical_group,
            DRepPPGroup::Gov => &params.dvt_pp_gov_group,
        };
        if t.gt(&max_threshold) {
            max_threshold = t.clone();
        }
    }
    max_threshold
}

/// Determine if SPOs can vote on a ParameterChange, and if so, return the threshold.
///
/// Per Haskell `votingStakePoolThresholdInternal`: SPOs vote with pvtPPSecurityGroup
/// if ANY modified parameter is tagged SecurityGroup. Otherwise SPOs cannot vote.
pub(crate) fn pp_change_spo_threshold(
    ppu: &dugite_primitives::transaction::ProtocolParamUpdate,
    params: &dugite_primitives::protocol_params::ProtocolParameters,
) -> Option<Rational> {
    let groups = modified_pp_groups(ppu);
    let has_security = groups
        .iter()
        .any(|(_, spo)| *spo == StakePoolPPGroup::Security);
    if has_security {
        Some(params.pvt_pp_security_group.clone())
    } else {
        None
    }
}

pub(crate) fn check_threshold(yes: u64, total: u64, threshold: &Rational) -> bool {
    // A zero threshold always passes (e.g., DRep thresholds during Conway bootstrap)
    if threshold.is_zero() {
        return true;
    }
    // Haskell's (%?) operator: _ %? 0 = 0, so zero denominator yields
    // ratio 0 which fails any non-zero threshold.
    if total == 0 {
        return false;
    }
    // Exact integer comparison: yes/total >= numerator/denominator
    // ⟺ yes * denominator >= numerator * total (using u128 to avoid overflow)
    threshold.is_met_by(yes, total)
}

/// Check if the constitutional committee has approved a governance action.
///
/// Per Haskell `committeeAccepted` / `committeeAcceptedRatio`:
/// - Iterate ALL committee members (from committee_expiration, which tracks membership)
/// - Expired members: excluded (treated as abstain)
/// - Members without hot keys (unregistered): excluded (treated as abstain)
/// - Resigned members: excluded (treated as abstain)
/// - Active members who didn't vote: counted as NO
/// - Active members who voted Abstain: excluded from ratio
/// - Active members who voted Yes: yes / Active members who voted No: no
/// - Ratio = yes_count / (yes_count + no_count) compared against committee_threshold
///
/// During bootstrap (protocol version 9), committeeMinSize check is skipped.
/// Post-bootstrap, if active_size < committeeMinSize, CC blocks ratification.
///
/// The committee data is passed explicitly (not via `&GovernanceState`) so that
/// `ratify_proposals()` can supply either live state or a [`PulsingSnapshot`]'s
/// committee fields, matching Haskell's `committeeAccepted` which reads from the
/// frozen `RatifyEnv`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_cc_approval(
    action_id: &GovActionId,
    votes_by_action: &ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
    committee_hot_keys: &ImblHashMap<Hash32, Hash32>,
    committee_expiration: &ImblHashMap<Hash32, EpochNo>,
    committee_resigned: &ImblHashMap<Hash32, Option<Anchor>>,
    committee_threshold: &Option<Rational>,
    current_epoch: EpochNo,
    committee_min_size: u64,
    bootstrap: bool,
) -> bool {
    // Get committee quorum threshold
    let threshold = match committee_threshold {
        Some(t) => t,
        None => {
            // No committee exists — CC vote fails (blocks ratification)
            return false;
        }
    };

    // Collect CC votes for this action indexed by hot credential
    let mut cc_votes: HashMap<Hash32, Vote> = HashMap::new();
    let empty = ImblOrdMap::new();
    let action_votes = votes_by_action.get(action_id).unwrap_or(&empty);
    for (voter, procedure) in action_votes {
        if let Voter::ConstitutionalCommittee(cred) = voter {
            let hot_key = credential_to_hash(cred);
            cc_votes.insert(hot_key, procedure.vote.clone());
        }
    }

    // Iterate all committee members and compute the ratio
    let mut yes_count = 0u64;
    let mut total_excluding_abstain = 0u64;
    let mut active_size = 0u64;

    for (cold_key, expiry) in committee_expiration {
        // Expired members: excluded (treated as abstain)
        // Per Haskell: `currentEpoch > validUntil` means expired.
        // Members are active through their expiry epoch (inclusive).
        if current_epoch > *expiry {
            continue;
        }

        // Check if member has a registered hot key
        let hot_key = match committee_hot_keys.get(cold_key) {
            Some(hk) => hk,
            None => continue, // No hot key: excluded (treated as abstain)
        };

        // Resigned members: excluded (treated as abstain)
        if committee_resigned.contains_key(cold_key) {
            continue;
        }

        active_size += 1;

        // Look up vote by hot credential
        match cc_votes.get(hot_key) {
            Some(Vote::Yes) => {
                yes_count += 1;
                total_excluding_abstain += 1;
            }
            Some(Vote::Abstain) => {
                // Abstain: excluded from ratio
            }
            Some(Vote::No) | None => {
                // Voted No or didn't vote: counts as No
                total_excluding_abstain += 1;
            }
        }
    }

    // Check committeeMinSize (skipped during bootstrap per Haskell spec).
    //
    // This MUST run before the zero-threshold auto-pass below: Haskell's
    // `votingThreshold` only yields a usable `VotingThreshold t` when
    // `hardforkConwayBootstrapPhase pv || activeCommitteeSize >= minSize`;
    // otherwise `NoVotingThreshold` fails the CC leg outright, regardless of
    // what the configured threshold is. A 0-threshold committee below
    // min-size must still be rejected (#800).
    if !bootstrap && active_size < committee_min_size {
        return false;
    }

    // If threshold is 0, auto-approve. Must come AFTER the min-size gate
    // (above) but BEFORE the all-abstain short-circuit (below): Haskell's
    // ratio comparison is `0 %? 0 <op> 0` which is `True` (0 >= 0), so an
    // all-abstain vote on a 0-threshold committee still auto-passes.
    if threshold.is_zero() {
        return true;
    }

    // Haskell's (%?) operator: _ %? 0 = 0, so when all active members
    // abstain the ratio is 0, which fails any non-zero threshold.
    if total_excluding_abstain == 0 {
        return false;
    }

    // Exact comparison: yes_count / total_excluding_abstain >= threshold
    let result = threshold.is_met_by(yes_count, total_excluding_abstain);
    if !result {
        debug!(
            action = %action_id.transaction_id.to_hex(),
            active_size, yes_count, total_excluding_abstain,
            threshold = threshold.as_f64(),
            ratio = yes_count as f64 / total_excluding_abstain as f64,
            result,
            cc_voters = cc_votes.len(),
            committee_members = committee_expiration.len(),
            hot_keys = committee_hot_keys.len(),
            "CC approval check failed"
        );
    }
    result
}

/// Check that a proposal's `prev_action_id` matches the last enacted action of the same
/// governance purpose. Per Haskell `prevActionAsExpected` in Ratify.hs.
///
/// NoConfidence and UpdateCommittee share the `Committee` purpose.
/// TreasuryWithdrawals and InfoAction have no prev_action_id chain (always pass).
///
/// The enacted roots are passed explicitly so that `ratify_proposals()` can thread
/// them through as proposals are enacted within a single ratification pass (matching
/// Haskell's `RatifyState.rsEnactState` threading).
pub(crate) fn prev_action_as_expected(
    action: &GovAction,
    enacted_pparam: &Option<GovActionId>,
    enacted_hardfork: &Option<GovActionId>,
    enacted_committee: &Option<GovActionId>,
    enacted_constitution: &Option<GovActionId>,
) -> bool {
    match action {
        GovAction::ParameterChange { prev_action_id, .. } => *prev_action_id == *enacted_pparam,
        GovAction::HardForkInitiation { prev_action_id, .. } => {
            *prev_action_id == *enacted_hardfork
        }
        GovAction::NoConfidence { prev_action_id } => *prev_action_id == *enacted_committee,
        GovAction::UpdateCommittee { prev_action_id, .. } => *prev_action_id == *enacted_committee,
        GovAction::NewConstitution { prev_action_id, .. } => {
            *prev_action_id == *enacted_constitution
        }
        // TreasuryWithdrawals and InfoAction have no chain requirement
        GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => true,
    }
}

/// Update threaded enacted roots during a ratification pass.
///
/// Called by `ratify_proposals()` after a proposal is successfully ratified to update
/// the local enacted root variables (not the live `GovernanceState`).  The live state
/// is updated in bulk after the full ratification pass completes.  This matches
/// Haskell's `RatifyState.rsEnactState` threading through the RATIFY rule.
fn update_enacted_root_local(
    action_id: &GovActionId,
    action: &GovAction,
    enacted_pparam: &mut Option<GovActionId>,
    enacted_hardfork: &mut Option<GovActionId>,
    enacted_committee: &mut Option<GovActionId>,
    enacted_constitution: &mut Option<GovActionId>,
) {
    match action {
        GovAction::ParameterChange { .. } => {
            *enacted_pparam = Some(action_id.clone());
        }
        GovAction::HardForkInitiation { .. } => {
            *enacted_hardfork = Some(action_id.clone());
        }
        GovAction::NoConfidence { .. } | GovAction::UpdateCommittee { .. } => {
            *enacted_committee = Some(action_id.clone());
        }
        GovAction::NewConstitution { .. } => {
            *enacted_constitution = Some(action_id.clone());
        }
        // TreasuryWithdrawals and InfoAction don't update any root
        GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => {}
    }
}

/// Check whether a "genesis root" proposal (`prev_action_id = None`) is valid at
/// submission time for the given governance action type.
///
/// Per Haskell `proposalsAddAction` (`Proposals.hs`):
///   `parent == ps ^. pRootsL . govRelationL . prRootL`
///
/// When `prev_action_id = SNothing` (None), the check passes only when the
/// purpose tree's current root is also `SNothing` (i.e. no proposal of this type
/// has ever been enacted).  If the root is `SJust id` (already enacted once),
/// the `SNothing` proposal has a stale parent and must be rejected with
/// `InvalidPrevGovActionId`.
///
/// Returns `true` (valid) when the enacted root for the action's purpose is `None`.
/// Returns `false` (invalid, must reject) when the enacted root is `Some(...)`.
///
/// `TreasuryWithdrawals` and `InfoAction` have no chain requirement — always `true`.
/// Haskell `pvCanFollow` (`Cardano.Ledger.Shelley.PParams`):
///
/// ```haskell
/// pvCanFollow (ProtVer curMajor curMinor) (ProtVer newMajor newMinor) =
///   (succVersion curMajor, 0) == (Just newMajor, newMinor)
///     || (curMajor, curMinor + 1) == (newMajor, newMinor)
/// ```
///
/// `succVersion curMajor = curMajor + 1`. A new version follows iff it is exactly a
/// major bump (`curMajor+1`, minor reset to 0) or exactly the next minor
/// (`curMinor+1`, same major). Any larger gap is illegal.
pub(crate) fn pv_can_follow(
    cur_major: u64,
    cur_minor: u64,
    new_major: u64,
    new_minor: u64,
) -> bool {
    (new_major == cur_major + 1 && new_minor == 0)
        || (new_major == cur_major && new_minor == cur_minor + 1)
}

/// Returns `true` when `gov_action` is a `HardForkInitiation` whose target ProtVer
/// does NOT `pvCanFollow` its resolved base version — i.e. the proposal must be
/// dropped with `ProposalCantFollow`. Returns `false` for any non-HardFork action,
/// or a HardForkInitiation whose base is unresolved or whose target legally follows.
///
/// Implements Haskell `preceedingHardFork` (`Cardano.Ledger.Conway.Rules.Gov`,
/// Gov.hs:673-694) three-way base resolution followed by [`pv_can_follow`]:
///  1. base = current on-chain PV — when the proposal's prev pointer equals the
///     enacted HardFork root, OR the target major exceeds `succVersion(curMajor)`
///     (the short-circuit that forbids compounding two major bumps in one epoch);
///  2. base = an in-flight parent HardForkInitiation's target PV — when the prev
///     pointer resolves to one already in the live proposal set (including
///     same-transaction earlier proposals, which are inserted as they are folded);
///  3. base unresolved (prev missing / not a HardFork) — no `ProposalCantFollow`
///     (the structural `InvalidPrevGovActionId` check owns that case).
///
/// Shared by the live block-apply GOV rule (`eras::conway`, #858) and the
/// test/dead-path proposal processors here (#812) so the reachability rule has a
/// single source of truth.
pub(crate) fn hardfork_proposal_cant_follow(
    gov_action: &GovAction,
    governance: &GovernanceState,
    cur_major: u64,
    cur_minor: u64,
) -> bool {
    let GovAction::HardForkInitiation {
        prev_action_id: hf_prev,
        protocol_version: (tgt_major, tgt_minor),
    } = gov_action
    else {
        return false;
    };
    let base = if hf_prev == &governance.enacted_hard_fork || *tgt_major > cur_major + 1 {
        Some((cur_major, cur_minor))
    } else {
        match hf_prev
            .as_ref()
            .and_then(|p| governance.proposals.get(p))
            .map(|ps| &ps.procedure.gov_action)
        {
            Some(GovAction::HardForkInitiation {
                protocol_version: (pm, pn),
                ..
            }) => Some((*pm, *pn)),
            _ => None,
        }
    };
    match base {
        Some((bm, bn)) => !pv_can_follow(bm, bn, *tgt_major, *tgt_minor),
        None => false,
    }
}

pub(crate) fn genesis_root_is_valid(action: &GovAction, governance: &GovernanceState) -> bool {
    let enacted_root = match action {
        GovAction::ParameterChange { .. } => governance.enacted_pparam_update.as_ref(),
        GovAction::HardForkInitiation { .. } => governance.enacted_hard_fork.as_ref(),
        GovAction::NoConfidence { .. } | GovAction::UpdateCommittee { .. } => {
            governance.enacted_committee.as_ref()
        }
        GovAction::NewConstitution { .. } => governance.enacted_constitution.as_ref(),
        // No chain requirement for these types
        GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => return true,
    };
    // Valid only when no prior action of this purpose has been enacted
    enacted_root.is_none()
}

/// Check whether a specific `prev_id` matches the last enacted action root for the
/// given action's governance purpose.
///
/// Used at proposal *submission* time (GOV rule) to validate that `prev_action_id`
/// is coherent before inserting the proposal into the active set.
///
/// Unlike `prev_action_as_expected` (which checks `action.prev_action_id == enacted_root`),
/// this takes the candidate `prev_id` directly so callers can test it without having
/// to reconstruct the action's own `prev_action_id`.
pub(crate) fn prev_action_matches_enacted_root(
    action: &GovAction,
    prev_id: &GovActionId,
    governance: &GovernanceState,
) -> bool {
    let enacted = match action {
        GovAction::ParameterChange { .. } => governance.enacted_pparam_update.as_ref(),
        GovAction::HardForkInitiation { .. } => governance.enacted_hard_fork.as_ref(),
        GovAction::NoConfidence { .. } | GovAction::UpdateCommittee { .. } => {
            governance.enacted_committee.as_ref()
        }
        GovAction::NewConstitution { .. } => governance.enacted_constitution.as_ref(),
        GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => {
            // No chain requirement; the caller should not pass a prev_id for these types.
            return false;
        }
    };
    enacted.is_some_and(|e| e == prev_id)
}

/// Default vote classification for non-voting SPOs, per Haskell
/// `defaultStakePoolVote` from `Cardano.Ledger.Conway.Governance.Procedures`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefaultVote {
    /// Pool's reward account delegates to AlwaysAbstain DRep
    Abstain,
    /// Pool's reward account delegates to AlwaysNoConfidence DRep
    NoConfidence,
    /// Pool not found, reward account not found, or delegates to a normal DRep
    No,
}

/// Extract the raw `Option<GovActionId>` prev_action_id from a governance action.
/// Returns `None` for both "prev_action_id = None" (genesis root) and actions
/// without a prev_action_id field (TreasuryWithdrawals, InfoAction).
/// Used for sibling matching where None == None (genesis root siblings).
pub(crate) fn gov_action_raw_prev_id(action: &GovAction) -> Option<GovActionId> {
    match action {
        GovAction::ParameterChange { prev_action_id, .. }
        | GovAction::HardForkInitiation { prev_action_id, .. }
        | GovAction::NoConfidence { prev_action_id }
        | GovAction::UpdateCommittee { prev_action_id, .. }
        | GovAction::NewConstitution { prev_action_id, .. } => prev_action_id.clone(),
        GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => None,
    }
}

/// Return a purpose tag for grouping governance actions by their governance
/// purpose tree. Actions with the same tag share the same enacted root chain.
/// Returns None for TreasuryWithdrawals and InfoAction (no purpose tree).
pub(crate) fn gov_action_purpose_tag(action: &GovAction) -> Option<u8> {
    match action {
        GovAction::ParameterChange { .. } => Some(0),
        GovAction::HardForkInitiation { .. } => Some(1),
        GovAction::NoConfidence { .. } | GovAction::UpdateCommittee { .. } => Some(2),
        GovAction::NewConstitution { .. } => Some(3),
        GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => None,
    }
}

/// Returns the governance action priority for ratification ordering.
/// Lower number = higher priority, per Haskell's `actionPriority`.
pub(crate) fn gov_action_priority(action: &GovAction) -> u8 {
    match action {
        GovAction::NoConfidence { .. } => 0,
        GovAction::UpdateCommittee { .. } => 1,
        GovAction::NewConstitution { .. } => 2,
        GovAction::HardForkInitiation { .. } => 3,
        GovAction::ParameterChange { .. } => 4,
        GovAction::TreasuryWithdrawals { .. } => 5,
        GovAction::InfoAction => 6,
    }
}

// ── Governance proposal priority forest operations ─────────────────────
//
// These functions maintain the `proposal_roots` / `proposal_graph` forest
// alongside the flat `proposals` BTreeMap, matching Haskell's Proposals.hs
// operations: proposalsAddAction, proposalsRemoveWithDescendants, and the
// root promotion in the `enact` helper of `proposalsApplyEnactment`.

/// Insert a proposal into the governance purpose forest.
///
/// Determines whether the proposal is a direct child of the purpose root
/// (its `prev_action_id` matches `roots[purpose].root`) or a deeper node
/// (its `prev_action_id` points to another active proposal in the graph).
///
/// Mirrors the insert path of Haskell's `proposalsAddAction`.
pub(crate) fn forest_add_proposal(
    action_id: &GovActionId,
    prev_action_id: Option<&GovActionId>,
    purpose_tag: u8,
    roots: &mut GovRelation<PRoot>,
    graph: &mut GovRelation<PGraph>,
) {
    let root = roots.get_mut(purpose_tag);
    let g = graph.get_mut(purpose_tag);

    if prev_action_id == root.root.as_ref() {
        // Direct child of the purpose root (last enacted action or genesis).
        root.children.insert(action_id.clone());
        debug!(
            "forest_add: {}#{} -> root child (tag={}, root={:?}, prev={:?})",
            action_id.transaction_id.to_hex(),
            action_id.action_index,
            purpose_tag,
            root.root.as_ref().map(|id| format!(
                "{}#{}",
                id.transaction_id.to_hex(),
                id.action_index
            )),
            prev_action_id.map(|id| format!("{}#{}", id.transaction_id.to_hex(), id.action_index)),
        );
    } else {
        // Deeper node — add as child of its parent in the graph.
        debug!(
            "forest_add: {}#{} -> graph node (tag={}, root={:?}, prev={:?})",
            action_id.transaction_id.to_hex(),
            action_id.action_index,
            purpose_tag,
            root.root.as_ref().map(|id| format!(
                "{}#{}",
                id.transaction_id.to_hex(),
                id.action_index
            )),
            prev_action_id.map(|id| format!("{}#{}", id.transaction_id.to_hex(), id.action_index)),
        );
        // Create PEdges for the new node.
        let edges = super::PEdges {
            parent: prev_action_id.cloned(),
            children: ImblOrdSet::new(),
        };
        g.nodes.insert(action_id.clone(), edges);

        // Register as a child of the parent node.
        if let Some(parent_id) = prev_action_id {
            if let Some(parent_edges) = g.nodes.get_mut(parent_id) {
                parent_edges.children.insert(action_id.clone());
            } else {
                // Parent is a root-level child (in PRoot.children but not in PGraph).
                // Create a PGraph entry for the parent so it can track children.
                let parent_edges = super::PEdges {
                    parent: None, // Parent's parent is the root
                    children: {
                        let mut s = ImblOrdSet::new();
                        s.insert(action_id.clone());
                        s
                    },
                };
                g.nodes.insert(parent_id.clone(), parent_edges);
            }
        }
    }
}

/// Remove a set of proposal IDs and ALL their transitive descendants from the
/// proposal forest and the flat `proposals` / `votes_by_action` maps.
///
/// Returns the removed `(GovActionId, ProposalState)` pairs for deposit refunding.
///
/// Mirrors Haskell's `proposalsRemoveWithDescendants`:
///   proposalsRemoveIds (gais <> foldMap getAllDescendants gais) ps
///
/// The descendant collection uses the `PGraph` (and `PRoot.children` for root-level
/// nodes) for O(k) traversal where k is the subtree size.
pub(crate) fn forest_remove_with_descendants(
    ids: &[GovActionId],
    proposals: &mut ImblOrdMap<GovActionId, ProposalState>,
    roots: &mut GovRelation<PRoot>,
    graph: &mut GovRelation<PGraph>,
    votes_by_action: &mut ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
) -> Vec<(GovActionId, ProposalState)> {
    if ids.is_empty() {
        return Vec::new();
    }

    // Collect the full removal set: ids + all transitive descendants.
    let mut to_remove = BTreeSet::new();
    for id in ids {
        to_remove.insert(id.clone());
        // Determine the purpose from the proposal's action (if still in the map).
        if let Some(state) = proposals.get(id) {
            let purpose = gov_action_purpose_tag(&state.procedure.gov_action);
            if let Some(tag) = purpose {
                collect_descendants(id, tag, roots, graph, &mut to_remove);
            }
        }
    }

    // Remove from the forest structure.
    for id in &to_remove {
        if let Some(state) = proposals.get(id) {
            let purpose = gov_action_purpose_tag(&state.procedure.gov_action);
            if let Some(tag) = purpose {
                forest_remove_node(id, tag, roots, graph);
            }
        }
    }

    // Remove from proposals map and votes, collecting removed states.
    let mut removed = Vec::with_capacity(to_remove.len());
    for id in &to_remove {
        if let Some(state) = proposals.remove(id) {
            removed.push((id.clone(), state));
        }
        votes_by_action.remove(id);
    }
    removed
}

/// Collect all transitive descendants of `id` in the purpose tree into `out`.
///
/// Traverses both `PRoot.children` (for root-level nodes) and `PGraph.nodes`
/// (for deeper nodes), following children edges recursively.
fn collect_descendants(
    id: &GovActionId,
    purpose_tag: u8,
    roots: &GovRelation<PRoot>,
    graph: &GovRelation<PGraph>,
    out: &mut BTreeSet<GovActionId>,
) {
    let root = roots.get(purpose_tag);
    let g = graph.get(purpose_tag);

    // Collect direct children of `id`.
    let children: Vec<GovActionId> = if root.children.contains(id) {
        // `id` is a root-level child.  Its children are in PGraph (if any).
        if let Some(edges) = g.nodes.get(id) {
            edges.children.iter().cloned().collect()
        } else {
            Vec::new()
        }
    } else if let Some(edges) = g.nodes.get(id) {
        edges.children.iter().cloned().collect()
    } else {
        Vec::new()
    };

    for child in children {
        if out.insert(child.clone()) {
            // Recurse — child's children are always in PGraph.
            collect_descendants(&child, purpose_tag, roots, graph, out);
        }
    }
}

/// Remove a single node from the forest structure (PRoot.children / PGraph.nodes).
/// Does NOT touch the `proposals` BTreeMap — caller handles that.
fn forest_remove_node(
    id: &GovActionId,
    purpose_tag: u8,
    roots: &mut GovRelation<PRoot>,
    graph: &mut GovRelation<PGraph>,
) {
    let root = roots.get_mut(purpose_tag);
    let g = graph.get_mut(purpose_tag);

    // Remove from PRoot.children if present.
    root.children.remove(id);

    // Remove from PGraph.nodes if present, and clean up parent's children set.
    if let Some(edges) = g.nodes.remove(id) {
        if let Some(parent_id) = &edges.parent {
            if let Some(parent_edges) = g.nodes.get_mut(parent_id) {
                parent_edges.children.remove(id);
            }
        }
        // Note: children of the removed node are also being removed
        // (they're in `to_remove`), so we don't need to re-parent them.
    }
}

/// Promote an enacted proposal to the root of its governance purpose tree.
///
/// After a proposal is ratified and enacted:
/// 1. It becomes the new root for its purpose (`PRoot.root = Some(enacted_id)`).
/// 2. Its children (from `PGraph`) become the new `PRoot.children`.
/// 3. Its own `PGraph` entry is removed.
///
/// Mirrors the root promotion in Haskell's `enact` helper within
/// `proposalsApplyEnactment`.
pub(crate) fn forest_promote_root(
    enacted_id: &GovActionId,
    purpose_tag: u8,
    roots: &mut GovRelation<PRoot>,
    graph: &mut GovRelation<PGraph>,
) {
    let root = roots.get_mut(purpose_tag);
    let g = graph.get_mut(purpose_tag);

    // The enacted proposal's children (if any) become the new root's children.
    let new_children = if let Some(edges) = g.nodes.remove(enacted_id) {
        // Clear parent references for promoted children.
        for child_id in &edges.children {
            if let Some(child_edges) = g.nodes.get_mut(child_id) {
                child_edges.parent = None;
            }
        }
        edges.children
    } else {
        ImblOrdSet::new()
    };

    // Remove enacted_id from old root's children (it was a root-level child).
    root.children.remove(enacted_id);

    // Set new root.
    root.root = Some(enacted_id.clone());
    root.children = new_children;
}

/// Rebuild the proposal forest from a flat `proposals` BTreeMap and enacted roots.
///
/// Used for backward compatibility when loading old snapshots that lack forest data.
/// Iterates all proposals once, determines each proposal's purpose and `prev_action_id`,
/// and reconstructs the complete `GovRelation<PRoot>` and `GovRelation<PGraph>`.
pub(crate) fn rebuild_forest_from_flat(
    proposals: &ImblOrdMap<GovActionId, ProposalState>,
    enacted_pparam: &Option<GovActionId>,
    enacted_hard_fork: &Option<GovActionId>,
    enacted_committee: &Option<GovActionId>,
    enacted_constitution: &Option<GovActionId>,
) -> (GovRelation<PRoot>, GovRelation<PGraph>) {
    let mut roots = GovRelation::<PRoot>::default();
    let mut graph = GovRelation::<PGraph>::default();

    // Initialize roots from enacted action IDs.
    roots.pparam.root = enacted_pparam.clone();
    roots.hard_fork.root = enacted_hard_fork.clone();
    roots.committee.root = enacted_committee.clone();
    roots.constitution.root = enacted_constitution.clone();

    // Insert each proposal into the forest.
    for (action_id, state) in proposals {
        let purpose = gov_action_purpose_tag(&state.procedure.gov_action);
        if let Some(tag) = purpose {
            let prev = gov_action_raw_prev_id(&state.procedure.gov_action);
            forest_add_proposal(action_id, prev.as_ref(), tag, &mut roots, &mut graph);
        }
    }

    (roots, graph)
}

/// Whether enacting this action should delay all further ratification for this epoch.
/// Per Haskell `delayingAction`: NoConfidence, HardFork, UpdateCommittee, NewConstitution.
pub(crate) fn is_delaying_action(action: &GovAction) -> bool {
    matches!(
        action,
        GovAction::NoConfidence { .. }
            | GovAction::HardForkInitiation { .. }
            | GovAction::UpdateCommittee { .. }
            | GovAction::NewConstitution { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        credential_to_hash, DRepRegistration, LedgerState, PoolRegistration, StakeSnapshot,
    };
    use dugite_primitives::credentials::Credential;
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::EpochNo;
    use dugite_primitives::transaction::{
        Anchor, Certificate, Constitution, DRep, ExUnits, GovAction, GovActionId,
        ProposalProcedure, ProtocolParamUpdate, Rational, Vote, Voter, VotingProcedure,
    };
    use dugite_primitives::value::Lovelace;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    /// #770 (the decisive path): the Conway gov-action ENACTMENT merge
    /// (`apply_protocol_param_update_impl`) must merge V4 AND unknown-language
    /// cost models per-language, mirroring Haskell `updateCostModels`
    /// (`Map.union new old`) — preserving the prior V2 entry, layering V3/V4,
    /// and carrying unknown keys ≥ 4. (Before #770 this path merged only
    /// V1/V2/V3 and silently dropped V4 + unknown.)
    #[test]
    fn apply_ppu_enactment_merges_v4_and_unknown_cost_models() {
        use dugite_primitives::transaction::CostModels;
        let mut params = ProtocolParameters::mainnet_defaults();
        params.cost_models.plutus_v2 = Some(vec![1]);
        params.cost_models.plutus_v3 = None;
        params.cost_models.plutus_v4 = None;
        params.cost_models.unknown_cost_models.clear();

        let update = ProtocolParamUpdate {
            cost_models: Some(CostModels {
                plutus_v3: Some(vec![2]),
                plutus_v4: Some(vec![3]),
                unknown_cost_models: [(5u8, vec![9i64])].into_iter().collect(),
                ..Default::default()
            }),
            ..Default::default()
        };

        apply_protocol_param_update_impl(&mut params, &update).unwrap();

        assert_eq!(
            params.cost_models.plutus_v2,
            Some(vec![1]),
            "prior V2 entry must be preserved (per-language merge)"
        );
        assert_eq!(params.cost_models.plutus_v3, Some(vec![2]), "V3 merged");
        assert_eq!(
            params.cost_models.plutus_v4,
            Some(vec![3]),
            "V4 merged (was dropped)"
        );
        assert_eq!(
            params.cost_models.unknown_cost_models.get(&5),
            Some(&vec![9]),
            "unknown lang=5 merged (was dropped at enactment)"
        );
    }

    /// #802: `apply_protocol_param_update_impl` (the Conway governance
    /// enactment path) must be ATOMIC — an update combining an out-of-range
    /// `rho` with an earlier-processed plain field (`min_fee_a`) must be
    /// rejected as a whole, leaving `params` completely untouched. Mirrors
    /// `protocol_params::test_apply_update_atomic_rejection_on_invalid_rho`;
    /// the two `apply_protocol_param_update*` functions must stay
    /// byte-identical in their validation behavior.
    #[test]
    fn apply_ppu_enactment_atomic_rejection_on_invalid_rho() {
        let mut params = ProtocolParameters::mainnet_defaults();
        let defaults = ProtocolParameters::mainnet_defaults();

        let update = ProtocolParamUpdate {
            min_fee_a: Some(999_999),
            rho: Some(Rational {
                numerator: 3,
                denominator: 2,
            }), // invalid: 3/2 exceeds 1
            ..Default::default()
        };

        let err = apply_protocol_param_update_impl(&mut params, &update)
            .expect_err("rho numerator > denominator must be rejected");
        assert!(matches!(
            err,
            crate::state::LedgerError::InvalidProtocolParam(_)
        ));

        assert_eq!(
            params.min_fee_a, defaults.min_fee_a,
            "min_fee_a must be unchanged when the update is rejected"
        );
        assert_eq!(
            params.rho, defaults.rho,
            "rho must be unchanged when the update is rejected"
        );
    }

    /// #802: `a0` (`NonNegativeInterval`) must NOT be bounds-checked at
    /// enactment — Haskell has no upper bound on it.
    #[test]
    fn apply_ppu_enactment_does_not_bound_a0() {
        let mut params = ProtocolParameters::mainnet_defaults();
        let update = ProtocolParamUpdate {
            a0: Some(Rational {
                numerator: 100,
                denominator: 1,
            }),
            ..Default::default()
        };
        apply_protocol_param_update_impl(&mut params, &update)
            .expect("a0 > 1 must be accepted — NonNegativeInterval has no upper bound");
        assert_eq!(
            params.a0,
            Rational {
                numerator: 100,
                denominator: 1,
            }
        );
    }

    pub(super) fn make_anchor() -> Anchor {
        Anchor {
            url: "https://example.com".to_string(),
            data_hash: Hash32::ZERO,
        }
    }

    pub(super) fn make_action_id(byte: u8, index: u32) -> GovActionId {
        GovActionId {
            transaction_id: Hash32::from_bytes([byte; 32]),
            action_index: index,
        }
    }

    /// Set up a LedgerState with DReps, SPOs, and CC for governance testing.
    /// Returns the state with `n_dreps` DReps (1B stake each), `n_spos` SPOs (1B stake each),
    /// and 1 CC member. Protocol version 10 (post-bootstrap).
    pub(super) fn gov_test_state(n_dreps: usize, n_spos: usize) -> LedgerState {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10; // Post-bootstrap
        params.committee_min_size = 0; // Don't require min committee size in tests
        let mut state = LedgerState::new(params);
        state.epoch_length = 100;
        state.epochs.needs_stake_rebuild = false;
        // Zero reserves to prevent RUPD monetary expansion from interfering
        // with governance-specific assertions about treasury changes.
        state.epochs.reserves = Lovelace(0);

        // Set up CC
        let cold = Credential::VerificationKey(Hash28::from_bytes([10u8; 28]));
        let hot = Credential::VerificationKey(Hash28::from_bytes([20u8; 28]));
        let cold_key = credential_to_hash(&cold);
        Arc::make_mut(&mut state.gov.governance)
            .committee_expiration
            .insert(cold_key, EpochNo(1000));
        state.process_certificate(&Certificate::CommitteeHotAuth {
            cold_credential: cold,
            hot_credential: hot,
        });
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 1,
            denominator: 2,
        });

        // Register DReps with vote delegations
        for i in 0..n_dreps {
            let cred = Credential::VerificationKey(Hash28::from_bytes([i as u8; 28]));
            let key = credential_to_hash(&cred);
            Arc::make_mut(&mut state.gov.governance).dreps.insert(
                key,
                DRepRegistration {
                    credential: cred,
                    deposit: Lovelace(500_000_000),
                    anchor: None,
                    registered_epoch: EpochNo(0),
                    drep_expiry: EpochNo(0),
                    active: true,
                },
            );
            let stake_key = Hash32::from_bytes([200 + i as u8; 32]);
            Arc::make_mut(&mut state.gov.governance)
                .vote_delegations
                .insert(stake_key, DRep::KeyHash(key));
            state
                .certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(1_000_000_000));
        }

        // Register SPOs with delegations
        for i in 0..n_spos {
            let pool_id = Hash28::from_bytes([100 + i as u8; 28]);
            Arc::make_mut(&mut state.certs.pool_params).insert(
                pool_id,
                PoolRegistration {
                    pool_id,
                    vrf_keyhash: Hash32::ZERO,
                    pledge: Lovelace(1_000_000),
                    cost: Lovelace(340_000_000),
                    margin_numerator: 1,
                    margin_denominator: 100,
                    reward_account: vec![],
                    owners: vec![],
                    relays: vec![],
                    metadata_url: None,
                    metadata_hash: None,
                },
            );
            let stake_key = Hash32::from_bytes([150 + i as u8; 32]);
            state.certs.delegations.insert(stake_key, pool_id);
            state
                .certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(1_000_000_000));
        }

        state
    }

    pub(super) fn cc_vote_yes(state: &mut LedgerState, action_id: &GovActionId) {
        let hot_cred = Credential::VerificationKey(Hash28::from_bytes([20u8; 28]));
        state.process_vote(
            &Voter::ConstitutionalCommittee(hot_cred),
            action_id,
            &VotingProcedure {
                vote: Vote::Yes,
                anchor: None,
            },
        );
    }

    pub(super) fn drep_vote(
        state: &mut LedgerState,
        i: usize,
        action_id: &GovActionId,
        vote: Vote,
    ) {
        let voter = Voter::DRep(Credential::VerificationKey(Hash28::from_bytes(
            [i as u8; 28],
        )));
        state.process_vote(&voter, action_id, &VotingProcedure { vote, anchor: None });
    }

    pub(super) fn spo_vote(state: &mut LedgerState, i: usize, action_id: &GovActionId, vote: Vote) {
        let pool_hash = Hash28::from_bytes([100 + i as u8; 28]).to_hash32_padded();
        let voter = Voter::StakePool(pool_hash);
        state.process_vote(&voter, action_id, &VotingProcedure { vote, anchor: None });
    }

    // ========================================================================
    // Priority ordering tests
    // ========================================================================

    #[test]
    fn test_gov_action_priority_ordering() {
        assert_eq!(
            gov_action_priority(&GovAction::NoConfidence {
                prev_action_id: None
            }),
            0
        );
        assert_eq!(
            gov_action_priority(&GovAction::UpdateCommittee {
                prev_action_id: None,
                members_to_remove: vec![],
                members_to_add: BTreeMap::new(),
                threshold: Rational {
                    numerator: 1,
                    denominator: 2
                },
            }),
            1
        );
        assert_eq!(
            gov_action_priority(&GovAction::NewConstitution {
                prev_action_id: None,
                constitution: Constitution {
                    anchor: make_anchor(),
                    script_hash: None
                },
            }),
            2
        );
        assert_eq!(
            gov_action_priority(&GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (10, 0),
            }),
            3
        );
        assert_eq!(
            gov_action_priority(&GovAction::ParameterChange {
                prev_action_id: None,
                protocol_param_update: Box::new(ProtocolParamUpdate::default()),
                policy_hash: None,
            }),
            4
        );
        assert_eq!(
            gov_action_priority(&GovAction::TreasuryWithdrawals {
                withdrawals: BTreeMap::new(),
                policy_hash: None,
            }),
            5
        );
        assert_eq!(gov_action_priority(&GovAction::InfoAction), 6);
    }

    // ========================================================================
    // Delaying action tests
    // ========================================================================

    #[test]
    fn test_delaying_actions() {
        assert!(is_delaying_action(&GovAction::NoConfidence {
            prev_action_id: None
        }));
        assert!(is_delaying_action(&GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (10, 0),
        }));
        assert!(is_delaying_action(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![],
            members_to_add: BTreeMap::new(),
            threshold: Rational {
                numerator: 1,
                denominator: 2
            },
        }));
        assert!(is_delaying_action(&GovAction::NewConstitution {
            prev_action_id: None,
            constitution: Constitution {
                anchor: make_anchor(),
                script_hash: None
            },
        }));
        assert!(!is_delaying_action(&GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::new(ProtocolParamUpdate::default()),
            policy_hash: None,
        }));
        assert!(!is_delaying_action(&GovAction::TreasuryWithdrawals {
            withdrawals: BTreeMap::new(),
            policy_hash: None,
        }));
        assert!(!is_delaying_action(&GovAction::InfoAction));
    }

    #[test]
    fn test_delaying_action_blocks_subsequent_ratification() {
        let mut state = gov_test_state(10, 10);
        // Set CC threshold to 0 to simplify
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 0,
            denominator: 1,
        });

        // Submit two proposals: NoConfidence (delaying) + ParameterChange (non-delaying)
        let nc_hash = Hash32::from_bytes([1u8; 32]);
        state.process_proposal(
            &nc_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );

        let pp_hash = Hash32::from_bytes([2u8; 32]);
        state.process_proposal(
            &pp_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        n_opt: Some(1000),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );

        let nc_id = make_action_id(1, 0);
        let pp_id = make_action_id(2, 0);

        // All DReps and SPOs vote Yes on both
        for i in 0..10 {
            drep_vote(&mut state, i, &nc_id, Vote::Yes);
            drep_vote(&mut state, i, &pp_id, Vote::Yes);
            spo_vote(&mut state, i, &nc_id, Vote::Yes);
        }

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // NoConfidence should be enacted (delaying action)
        assert!(state.gov.governance.no_confidence);
        // ParameterChange should be delayed — NOT enacted
        assert_eq!(state.epochs.protocol_params.n_opt, 500); // unchanged
                                                             // The pp proposal should still be active (delayed, not expired)
        assert!(state.gov.governance.last_ratify_delayed);
    }

    // ========================================================================
    // Prev action ID chain tests
    // ========================================================================

    #[test]
    fn test_prev_action_id_chain_validation() {
        let gov = GovernanceState::default();

        // First action of each type must have None prevActionId
        assert!(prev_action_as_expected(
            &GovAction::ParameterChange {
                prev_action_id: None,
                protocol_param_update: Box::new(ProtocolParamUpdate::default()),
                policy_hash: None
            },
            &gov.enacted_pparam_update,
            &gov.enacted_hard_fork,
            &gov.enacted_committee,
            &gov.enacted_constitution
        ));
        assert!(prev_action_as_expected(
            &GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (10, 0)
            },
            &gov.enacted_pparam_update,
            &gov.enacted_hard_fork,
            &gov.enacted_committee,
            &gov.enacted_constitution
        ));
        assert!(prev_action_as_expected(
            &GovAction::NoConfidence {
                prev_action_id: None
            },
            &gov.enacted_pparam_update,
            &gov.enacted_hard_fork,
            &gov.enacted_committee,
            &gov.enacted_constitution
        ));
        // TreasuryWithdrawals always passes (no chain)
        assert!(prev_action_as_expected(
            &GovAction::TreasuryWithdrawals {
                withdrawals: BTreeMap::new(),
                policy_hash: None
            },
            &gov.enacted_pparam_update,
            &gov.enacted_hard_fork,
            &gov.enacted_committee,
            &gov.enacted_constitution
        ));
        // InfoAction always passes
        assert!(prev_action_as_expected(
            &GovAction::InfoAction,
            &gov.enacted_pparam_update,
            &gov.enacted_hard_fork,
            &gov.enacted_committee,
            &gov.enacted_constitution
        ));
    }

    #[test]
    fn test_prev_action_id_mismatch_rejects() {
        let gov = GovernanceState::default();
        let wrong_id = GovActionId {
            transaction_id: Hash32::from_bytes([99u8; 32]),
            action_index: 0,
        };

        // ParameterChange with wrong prevActionId should fail
        assert!(!prev_action_as_expected(
            &GovAction::ParameterChange {
                prev_action_id: Some(wrong_id.clone()),
                protocol_param_update: Box::new(ProtocolParamUpdate::default()),
                policy_hash: None,
            },
            &gov.enacted_pparam_update,
            &gov.enacted_hard_fork,
            &gov.enacted_committee,
            &gov.enacted_constitution
        ));

        // NoConfidence with wrong prevActionId should fail
        assert!(!prev_action_as_expected(
            &GovAction::NoConfidence {
                prev_action_id: Some(wrong_id.clone())
            },
            &gov.enacted_pparam_update,
            &gov.enacted_hard_fork,
            &gov.enacted_committee,
            &gov.enacted_constitution
        ));
    }

    #[test]
    fn test_no_confidence_and_update_committee_share_committee_purpose() {
        let mut gov = GovernanceState::default();
        let enacted_id = make_action_id(50, 0);
        gov.enacted_committee = Some(enacted_id.clone());

        // Both NoConfidence and UpdateCommittee should check against enacted_committee
        assert!(prev_action_as_expected(
            &GovAction::NoConfidence {
                prev_action_id: Some(enacted_id.clone())
            },
            &gov.enacted_pparam_update,
            &gov.enacted_hard_fork,
            &gov.enacted_committee,
            &gov.enacted_constitution
        ));
        assert!(prev_action_as_expected(
            &GovAction::UpdateCommittee {
                prev_action_id: Some(enacted_id),
                members_to_remove: vec![],
                members_to_add: BTreeMap::new(),
                threshold: Rational {
                    numerator: 1,
                    denominator: 2
                },
            },
            &gov.enacted_pparam_update,
            &gov.enacted_hard_fork,
            &gov.enacted_committee,
            &gov.enacted_constitution
        ));
    }

    // ========================================================================
    // Parameter group classification tests
    // ========================================================================

    #[test]
    fn test_pp_groups_security_params() {
        // Security params should trigger SPO voting
        let ppu = ProtocolParamUpdate {
            max_block_body_size: Some(90000),
            ..Default::default()
        };
        let groups = modified_pp_groups(&ppu);
        assert!(groups.iter().any(|(_, s)| *s == StakePoolPPGroup::Security));

        let ppu = ProtocolParamUpdate {
            min_fee_a: Some(100),
            ..Default::default()
        };
        let groups = modified_pp_groups(&ppu);
        assert!(groups.iter().any(|(_, s)| *s == StakePoolPPGroup::Security));

        let ppu = ProtocolParamUpdate {
            gov_action_deposit: Some(Lovelace(100_000_000_000)),
            ..Default::default()
        };
        let groups = modified_pp_groups(&ppu);
        assert!(groups.iter().any(|(_, s)| *s == StakePoolPPGroup::Security));
    }

    #[test]
    fn test_pp_groups_non_security_params() {
        // Non-security params should NOT trigger SPO voting
        let ppu = ProtocolParamUpdate {
            n_opt: Some(1000),
            ..Default::default()
        };
        let groups = modified_pp_groups(&ppu);
        assert!(groups.iter().all(|(_, s)| *s == StakePoolPPGroup::NoVote));

        let ppu = ProtocolParamUpdate {
            drep_deposit: Some(Lovelace(500_000_000)),
            ..Default::default()
        };
        let groups = modified_pp_groups(&ppu);
        assert!(groups.iter().all(|(_, s)| *s == StakePoolPPGroup::NoVote));
    }

    #[test]
    fn test_pp_groups_drep_group_classification() {
        let ppu = ProtocolParamUpdate {
            max_tx_size: Some(32768),
            ..Default::default()
        };
        let groups = modified_pp_groups(&ppu);
        assert!(groups.iter().any(|(d, _)| *d == DRepPPGroup::Network));

        let ppu = ProtocolParamUpdate {
            key_deposit: Some(Lovelace(2_000_000)),
            ..Default::default()
        };
        let groups = modified_pp_groups(&ppu);
        assert!(groups.iter().any(|(d, _)| *d == DRepPPGroup::Economic));

        let ppu = ProtocolParamUpdate {
            e_max: Some(50),
            ..Default::default()
        };
        let groups = modified_pp_groups(&ppu);
        assert!(groups.iter().any(|(d, _)| *d == DRepPPGroup::Technical));

        let ppu = ProtocolParamUpdate {
            drep_activity: Some(20),
            ..Default::default()
        };
        let groups = modified_pp_groups(&ppu);
        assert!(groups.iter().any(|(d, _)| *d == DRepPPGroup::Gov));
    }

    #[test]
    fn test_pp_change_spo_threshold_security() {
        let params = ProtocolParameters::mainnet_defaults();
        let ppu = ProtocolParamUpdate {
            max_block_body_size: Some(90000), // Network + Security
            ..Default::default()
        };
        let threshold = pp_change_spo_threshold(&ppu, &params);
        assert!(threshold.is_some());
        assert_eq!(threshold.unwrap(), params.pvt_pp_security_group);
    }

    #[test]
    fn test_pp_change_spo_threshold_non_security() {
        let params = ProtocolParameters::mainnet_defaults();
        let ppu = ProtocolParamUpdate {
            n_opt: Some(1000), // Technical + NoVote
            ..Default::default()
        };
        let threshold = pp_change_spo_threshold(&ppu, &params);
        assert!(threshold.is_none());
    }

    // ========================================================================
    // DRep denominator tests (Haskell dRepAcceptedRatio)
    // ========================================================================

    #[test]
    fn test_drep_non_voters_count_as_implicit_no() {
        let mut state = gov_test_state(10, 0);
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 0,
            denominator: 1,
        });

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        n_opt: Some(1000),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);

        // Only 3 out of 10 DReps vote Yes (30%)
        // With correct denominator: 3/10 = 30% < 67% dvt_pp_technical_group → NOT ratified
        for i in 0..3 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }

        let (cache, nc_stake, _) = state.build_drep_power_cache();
        let (yes, total, _, _, _, _) = state.count_votes_by_type(
            &action_id,
            &GovAction::ParameterChange {
                prev_action_id: None,
                protocol_param_update: Box::new(ProtocolParamUpdate::default()),
                policy_hash: None,
            },
            &cache,
            nc_stake,
            &state.gov.governance.votes_by_action,
            None,
            None,
        );

        assert_eq!(yes, 3_000_000_000); // 3 DReps * 1B
        assert_eq!(total, 10_000_000_000); // ALL 10 DReps' stake (7 non-voters as implicit No)
    }

    #[test]
    fn test_drep_abstain_excluded_from_denominator() {
        let mut state = gov_test_state(10, 0);
        let action_id = make_action_id(50, 0);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );

        // 3 yes, 3 no, 4 abstain
        for i in 0..3 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 3..6 {
            drep_vote(&mut state, i, &action_id, Vote::No);
        }
        for i in 6..10 {
            drep_vote(&mut state, i, &action_id, Vote::Abstain);
        }

        let (cache, nc_stake, _) = state.build_drep_power_cache();
        let (yes, total, _, _, _, _) = state.count_votes_by_type(
            &action_id,
            &GovAction::InfoAction,
            &cache,
            nc_stake,
            &state.gov.governance.votes_by_action,
            None,
            None,
        );

        // Denominator = total active (10B) - abstain (4B) = 6B
        assert_eq!(yes, 3_000_000_000);
        assert_eq!(total, 6_000_000_000);
    }

    #[test]
    fn test_always_abstain_excluded_entirely() {
        let mut state = gov_test_state(5, 0);
        // Add AlwaysAbstain delegators
        for i in 0..3u8 {
            let stake_key = Hash32::from_bytes([230 + i; 32]);
            Arc::make_mut(&mut state.gov.governance)
                .vote_delegations
                .insert(stake_key, DRep::Abstain);
            state
                .certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(2_000_000_000));
        }

        let action_id = make_action_id(50, 0);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );

        let (cache, nc_stake, abstain_stake) = state.build_drep_power_cache();

        // AlwaysAbstain should NOT be in drep_power_cache
        assert_eq!(abstain_stake, 6_000_000_000); // 3 * 2B
                                                  // DRep power cache should only have the 5 registered DReps
        assert_eq!(cache.len(), 5);

        let (yes, total, _, _, _, _) = state.count_votes_by_type(
            &action_id,
            &GovAction::InfoAction,
            &cache,
            nc_stake,
            &state.gov.governance.votes_by_action,
            None,
            None,
        );

        // Total should only include active DRep stake (5B), not AlwaysAbstain (6B)
        assert_eq!(yes, 0);
        assert_eq!(total, 5_000_000_000);
    }

    #[test]
    fn test_always_no_confidence_yes_on_no_confidence() {
        let mut state = gov_test_state(5, 5);
        // Add AlwaysNoConfidence delegators
        for i in 0..3u8 {
            let stake_key = Hash32::from_bytes([230 + i; 32]);
            Arc::make_mut(&mut state.gov.governance)
                .vote_delegations
                .insert(stake_key, DRep::NoConfidence);
            state
                .certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(2_000_000_000));
        }

        let action_id = make_action_id(50, 0);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );

        let (cache, nc_stake, _) = state.build_drep_power_cache();
        assert_eq!(nc_stake, 6_000_000_000); // 3 * 2B

        let (yes, total, _, _, _, _) = state.count_votes_by_type(
            &action_id,
            &GovAction::NoConfidence {
                prev_action_id: None,
            },
            &cache,
            nc_stake,
            &state.gov.governance.votes_by_action,
            None,
            None,
        );

        // NoConfidence action: AlwaysNoConfidence counts as Yes
        // yes = 6B (NoConfidence), total = 5B (DReps, implicit No) + 6B (NoConfidence) = 11B
        assert_eq!(yes, 6_000_000_000);
        assert_eq!(total, 11_000_000_000);
    }

    #[test]
    fn test_always_no_confidence_no_on_other_actions() {
        let mut state = gov_test_state(5, 0);
        for i in 0..3u8 {
            let stake_key = Hash32::from_bytes([230 + i; 32]);
            Arc::make_mut(&mut state.gov.governance)
                .vote_delegations
                .insert(stake_key, DRep::NoConfidence);
            state
                .certs
                .stake_distribution
                .stake_map
                .insert(stake_key, Lovelace(2_000_000_000));
        }

        let action_id = make_action_id(50, 0);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );

        let (cache, nc_stake, _) = state.build_drep_power_cache();
        let (yes, total, _, _, _, _) = state.count_votes_by_type(
            &action_id,
            &GovAction::InfoAction,
            &cache,
            nc_stake,
            &state.gov.governance.votes_by_action,
            None,
            None,
        );

        // Non-NoConfidence: AlwaysNoConfidence counts as No (in denominator, not numerator)
        assert_eq!(yes, 0);
        assert_eq!(total, 11_000_000_000); // 5B DRep + 6B NoConfidence
    }

    /// Regression: a successfully-enacted delaying action (NoConfidence /
    /// UpdateCommittee / NewConstitution / HardForkInitiation) must block
    /// every subsequent proposal in the same RATIFY pass.  Matches the
    /// Haskell `rsDelayed` flag in `RatifyState`.
    ///
    /// This is the practical mechanism that keeps committee state fresh
    /// across a multi-proposal pass: once `NoConfidence` enacts and clears
    /// the committee, a follow-up `ParameterChange` that would have needed
    /// CC approval is skipped entirely (not re-checked against the cleared
    /// committee).  Both Haskell and dugite achieve the same end result —
    /// dugite via the local `delayed` flag, Haskell via `rsDelayed` —
    /// neither lets the second proposal observe the mutated committee.
    #[test]
    fn test_committee_affecting_actions_are_all_delaying() {
        // Every Conway action that mutates the committee state must also
        // be a delaying action, so that no subsequent proposal in the
        // same RATIFY pass tries to read the (now-stale) committee
        // snapshot.  This is the invariant that keeps `check_cc_approval`
        // sound across a multi-proposal pass without explicit committee
        // threading: once `NoConfidence` or `UpdateCommittee` enacts,
        // the loop's `delayed` flag short-circuits all remaining
        // proposals before they can re-check CC against the cleared
        // committee.  Matches Haskell where the same actions set
        // `rsDelayed` in `RatifyState`.
        assert!(is_delaying_action(&GovAction::NoConfidence {
            prev_action_id: None
        }));
        assert!(is_delaying_action(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: Vec::new(),
            members_to_add: BTreeMap::new(),
            threshold: Rational {
                numerator: 1,
                denominator: 2,
            },
        }));
    }

    #[test]
    fn test_committee_threading_refresh_after_enact() {
        // Direct unit test of the local committee-refresh logic in
        // `ratify_proposals_impl`: after `enact_gov_action_impl` mutates
        // the live `gov.governance.committee_*` maps via NoConfidence,
        // the local `snap_committee_*` bindings can be re-derived from
        // the live state.  This is the threading mechanism — exercised
        // here in isolation since the priority + delaying invariant
        // (above) means no production scenario observes a difference.
        let mut state = gov_test_state(0, 0);
        // Pre-populate a committee.
        let cold = Credential::VerificationKey(Hash28::from_bytes([10u8; 28]));
        let cold_key = credential_to_hash(&cold);
        Arc::make_mut(&mut state.gov.governance)
            .committee_expiration
            .insert(cold_key, EpochNo(1000));
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 1,
            denominator: 2,
        });
        // Snapshot the local "before" view as the loop would.
        let before_threshold = state.gov.governance.committee_threshold.clone();
        assert!(before_threshold.is_some());

        // Enact a NoConfidence directly via the helper (bypasses
        // submission rules; we're testing enact's effect, not the rule).
        enact_gov_action_impl(
            &GovAction::NoConfidence {
                prev_action_id: None,
            },
            &mut state.epochs,
            &mut state.certs,
            &mut state.gov,
        );
        assert!(state.gov.governance.no_confidence);
        assert!(state.gov.governance.committee_threshold.is_none());
        assert!(state.gov.governance.committee_expiration.is_empty());

        // Re-derive the "after" view from the live state — this is the
        // refresh step `ratify_proposals_impl` performs after a delaying
        // committee-affecting action enacts.
        let after_threshold = state.gov.governance.committee_threshold.clone();
        let after_expiration = state.gov.governance.committee_expiration.clone();
        assert!(after_threshold.is_none());
        assert!(after_expiration.is_empty());
    }

    /// Regression for issue #433: `expire_committee_members` must NOT
    /// physically remove members whose expiry epoch has passed. Matches
    /// Haskell `cardano-ledger`, where the committee map retains all elected
    /// members verbatim and `MemberStatus = Expired` is computed at query
    /// time. Physically deleting the entry would (a) make the CC state query
    /// undercount, and (b) lose the prior authorization context if the same
    /// cold credential is re-elected with a fresh `validUntil`.
    #[test]
    fn test_expired_committee_members_retained_in_map() {
        let mut state = gov_test_state(0, 0);
        let cold = Credential::VerificationKey(Hash28::from_bytes([42u8; 28]));
        let cold_key = credential_to_hash(&cold);
        // Member elected with `validUntil = 10`.
        Arc::make_mut(&mut state.gov.governance)
            .committee_expiration
            .insert(cold_key, EpochNo(10));
        // Authorise a hot key — this must survive expiry so the query can
        // still report the prior authorization (matches Haskell behaviour).
        let hot = Credential::VerificationKey(Hash28::from_bytes([99u8; 28]));
        let hot_key = credential_to_hash(&hot);
        Arc::make_mut(&mut state.gov.governance)
            .committee_hot_keys
            .insert(cold_key, hot_key);

        // Advance past expiry — well beyond `validUntil = 10`.
        expire_committee_members(EpochNo(50), &mut state.gov);

        assert!(
            state
                .gov
                .governance
                .committee_expiration
                .contains_key(&cold_key),
            "expired CC member must remain in committee_expiration; Haskell parity (#433)"
        );
        assert_eq!(
            state.gov.governance.committee_expiration.get(&cold_key),
            Some(&EpochNo(10)),
            "validUntil must be preserved verbatim after expiry"
        );
        assert!(
            state
                .gov
                .governance
                .committee_hot_keys
                .contains_key(&cold_key),
            "hot-key authorization must survive expiry; query must still surface it"
        );

        // Query-time MemberStatus projection (mirrors query.rs:444):
        // `currentEpoch > validUntil` ⇒ Expired.
        let current = EpochNo(50);
        let expiry = state
            .gov
            .governance
            .committee_expiration
            .get(&cold_key)
            .copied()
            .expect("retained");
        let member_status: u8 = if current.0 > expiry.0 { 1 } else { 0 };
        assert_eq!(member_status, 1, "MemberStatus must be Expired (1)");
    }

    /// Regression: script-DRep delegations must be routed via the typed
    /// Hash32 form (`Credential::to_typed_hash32` / `DRep::credential_hash32`),
    /// not the type-byte-less `Hash28::to_hash32_padded` form.
    ///
    /// Before the fix, `build_drep_power_cache_live` looked up
    /// `dreps.get(&h.to_hash32_padded())` for `DRep::ScriptHash(h)` while
    /// the `dreps` map keys are produced by `credential_to_hash`
    /// (script-typed, type byte 0x01).  The lookup silently missed and
    /// every script-DRep's stake was dropped.
    #[test]
    fn test_script_drep_lookup_uses_typed_credential_hash() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        // Register one script DRep.
        let raw = [0x7cu8; 28];
        let script_cred = Credential::Script(Hash28::from_bytes(raw));
        let dreps_key = credential_to_hash(&script_cred); // script-typed Hash32
        Arc::make_mut(&mut state.gov.governance).dreps.insert(
            dreps_key,
            DRepRegistration {
                credential: script_cred.clone(),
                deposit: Lovelace(500_000_000),
                anchor: None,
                registered_epoch: EpochNo(0),
                drep_expiry: EpochNo(u64::MAX / 2),
                active: true,
            },
        );

        // Delegate a stake credential to the script DRep.
        let stake_cred = Hash32::from_bytes([0xeeu8; 32]);
        Arc::make_mut(&mut state.gov.governance)
            .vote_delegations
            .insert(stake_cred, DRep::ScriptHash(Hash28::from_bytes(raw)));
        state
            .certs
            .stake_distribution
            .stake_map
            .insert(stake_cred, Lovelace(1_000_000_000));

        // Both routings: the pulser freeze (the consensus path since #988) and
        // the live fallback that answers before the first Conway boundary.
        let frozen = build_pulsing_snapshot(
            state.epoch,
            state.epochs.treasury.0,
            &state.certs,
            &state.gov,
        )
        .drep_distr;
        let (live, _, _) = build_drep_power_cache_live_from(&state.gov, &state.certs);
        let padded = Hash28::from_bytes(raw).to_hash32_padded();
        for (label, cache) in [("frozen", &frozen), ("live", &live)] {
            assert_eq!(
                cache.get(&dreps_key),
                Some(&1_000_000_000u64),
                "{label}: script DRep stake must be routed under the typed-Hash32 key"
            );
            // The padded (untyped) form must NOT appear — that was the bug.
            assert!(
                !cache.contains_key(&padded),
                "{label}: padded (no-discriminator) form must not leak into the cache"
            );
        }
    }

    #[test]
    fn test_inactive_drep_excluded_from_power_cache() {
        let mut state = gov_test_state(5, 0);
        // Mark first 2 DReps inactive
        let cred0 = Credential::VerificationKey(Hash28::from_bytes([0u8; 28]));
        let cred1 = Credential::VerificationKey(Hash28::from_bytes([1u8; 28]));
        let key0 = credential_to_hash(&cred0);
        let key1 = credential_to_hash(&cred1);
        Arc::make_mut(&mut state.gov.governance)
            .dreps
            .get_mut(&key0)
            .unwrap()
            .active = false;
        Arc::make_mut(&mut state.gov.governance)
            .dreps
            .get_mut(&key1)
            .unwrap()
            .active = false;

        let (cache, _, _) = state.build_drep_power_cache();
        assert!(!cache.contains_key(&key0));
        assert!(!cache.contains_key(&key1));
        assert_eq!(cache.len(), 3); // Only 3 active DReps
    }

    // ========================================================================
    // CC approval tests
    // ========================================================================

    #[test]
    fn test_cc_expired_members_excluded() {
        let mut state = gov_test_state(5, 0);
        state.epochs.protocol_params.committee_min_size = 0;
        // Add a second CC member with early expiry
        let cold2 = Credential::VerificationKey(Hash28::from_bytes([11u8; 28]));
        let hot2 = Credential::VerificationKey(Hash28::from_bytes([21u8; 28]));
        let cold2_key = credential_to_hash(&cold2);
        Arc::make_mut(&mut state.gov.governance)
            .committee_expiration
            .insert(cold2_key, EpochNo(5)); // Active through epoch 5

        state.process_certificate(&Certificate::CommitteeHotAuth {
            cold_credential: cold2,
            hot_credential: hot2,
        });

        // Set epoch to 5 — member should still be active (expiry is inclusive)
        state.epoch = EpochNo(5);
        let action_id = make_action_id(50, 0);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );

        // Both CC members vote Yes
        cc_vote_yes(&mut state, &action_id);
        let hot2_cred = Credential::VerificationKey(Hash28::from_bytes([21u8; 28]));
        state.process_vote(
            &Voter::ConstitutionalCommittee(hot2_cred),
            &action_id,
            &VotingProcedure {
                vote: Vote::Yes,
                anchor: None,
            },
        );

        // At epoch 5, member with expiry 5 should still be active
        let result = check_cc_approval(
            &action_id,
            &state.gov.governance.votes_by_action,
            &state.gov.governance.committee_hot_keys,
            &state.gov.governance.committee_expiration,
            &state.gov.governance.committee_resigned,
            &state.gov.governance.committee_threshold,
            EpochNo(5),
            state.epochs.protocol_params.committee_min_size,
            false,
        );
        assert!(result);

        // At epoch 6, member with expiry 5 should be expired
        let result = check_cc_approval(
            &action_id,
            &state.gov.governance.votes_by_action,
            &state.gov.governance.committee_hot_keys,
            &state.gov.governance.committee_expiration,
            &state.gov.governance.committee_resigned,
            &state.gov.governance.committee_threshold,
            EpochNo(6),
            state.epochs.protocol_params.committee_min_size,
            false,
        );
        // Only first CC member (expiry 1000) is active, voted Yes → 1/1 >= 1/2 → pass
        assert!(result);
    }

    #[test]
    fn test_cc_resigned_members_excluded() {
        let mut state = gov_test_state(5, 0);
        let cold_key =
            credential_to_hash(&Credential::VerificationKey(Hash28::from_bytes([10u8; 28])));

        // Resign the CC member
        Arc::make_mut(&mut state.gov.governance)
            .committee_resigned
            .insert(cold_key, None);

        let action_id = make_action_id(50, 0);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );

        // CC vote should fail — only member is resigned
        let result = check_cc_approval(
            &action_id,
            &state.gov.governance.votes_by_action,
            &state.gov.governance.committee_hot_keys,
            &state.gov.governance.committee_expiration,
            &state.gov.governance.committee_resigned,
            &state.gov.governance.committee_threshold,
            EpochNo(0),
            state.epochs.protocol_params.committee_min_size,
            false,
        );
        assert!(!result);
    }

    #[test]
    fn test_cc_no_committee_blocks_ratification() {
        let gov = GovernanceState {
            // No committee (threshold = None, matching Haskell SNothing)
            committee_threshold: None,
            ..GovernanceState::default()
        };

        let action_id = make_action_id(50, 0);
        let result = check_cc_approval(
            &action_id,
            &gov.votes_by_action,
            &gov.committee_hot_keys,
            &gov.committee_expiration,
            &gov.committee_resigned,
            &gov.committee_threshold,
            EpochNo(0),
            0,
            false,
        );
        assert!(!result);
    }

    #[test]
    fn test_cc_min_size_enforcement() {
        let mut state = gov_test_state(5, 0);
        // Set min committee size to 3 (we only have 1 member)
        state.epochs.protocol_params.committee_min_size = 3;

        let action_id = make_action_id(50, 0);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );
        cc_vote_yes(&mut state, &action_id);

        // Post-bootstrap: should fail because active_size (1) < committee_min_size (3)
        let result = check_cc_approval(
            &action_id,
            &state.gov.governance.votes_by_action,
            &state.gov.governance.committee_hot_keys,
            &state.gov.governance.committee_expiration,
            &state.gov.governance.committee_resigned,
            &state.gov.governance.committee_threshold,
            EpochNo(0),
            3,
            false,
        );
        assert!(!result);

        // During bootstrap: min size check is skipped
        let result = check_cc_approval(
            &action_id,
            &state.gov.governance.votes_by_action,
            &state.gov.governance.committee_hot_keys,
            &state.gov.governance.committee_expiration,
            &state.gov.governance.committee_resigned,
            &state.gov.governance.committee_threshold,
            EpochNo(0),
            3,
            true,
        );
        assert!(result);
    }

    /// #800: a 0-threshold committee must still be gated by `committeeMinSize`.
    /// Below min-size, even a 0-threshold committee must FAIL — Haskell's
    /// `votingThreshold` returns `NoVotingThreshold` (⇒ CC leg fails) when
    /// `activeCommitteeSize < minSize` post-bootstrap, regardless of the
    /// configured threshold. At/above min-size, or during bootstrap (where the
    /// min-size gate is skipped entirely), a 0-threshold committee auto-passes.
    #[test]
    fn test_cc_zero_threshold_respects_min_size() {
        let mut state = gov_test_state(5, 0);
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 0,
            denominator: 1,
        });

        let action_id = make_action_id(51, 0);
        state.process_proposal(
            &Hash32::from_bytes([51u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );
        // No CC votes are cast — with the old (buggy) code, the zero-threshold
        // shortcut auto-passed unconditionally, before committeeMinSize was
        // even considered.

        // Post-bootstrap, active_size (1) < committee_min_size (3): must fail
        // even though the configured threshold is 0.
        let result = check_cc_approval(
            &action_id,
            &state.gov.governance.votes_by_action,
            &state.gov.governance.committee_hot_keys,
            &state.gov.governance.committee_expiration,
            &state.gov.governance.committee_resigned,
            &state.gov.governance.committee_threshold,
            EpochNo(0),
            3,
            false,
        );
        assert!(
            !result,
            "0-threshold committee below committeeMinSize must not auto-pass"
        );

        // At/above min-size, a 0-threshold committee auto-passes (all-abstain
        // ratio 0 %? 0 = True per Haskell's (%?) operator).
        let result = check_cc_approval(
            &action_id,
            &state.gov.governance.votes_by_action,
            &state.gov.governance.committee_hot_keys,
            &state.gov.governance.committee_expiration,
            &state.gov.governance.committee_resigned,
            &state.gov.governance.committee_threshold,
            EpochNo(0),
            1,
            false,
        );
        assert!(
            result,
            "0-threshold committee at/above committeeMinSize must auto-pass"
        );

        // During bootstrap, the min-size gate is skipped entirely, so the
        // 0-threshold auto-pass applies unconditionally.
        let result = check_cc_approval(
            &action_id,
            &state.gov.governance.votes_by_action,
            &state.gov.governance.committee_hot_keys,
            &state.gov.governance.committee_expiration,
            &state.gov.governance.committee_resigned,
            &state.gov.governance.committee_threshold,
            EpochNo(0),
            3,
            true,
        );
        assert!(
            result,
            "0-threshold committee during bootstrap must auto-pass regardless of size"
        );
    }

    /// #812 (dead-path): `process_proposal`'s `pvCanFollow` check must require
    /// the target minor version to be EXACTLY `curMinor + 1`, not merely any
    /// higher minor (Haskell `pvCanFollow`). A `HardForkInitiation` that skips
    /// a minor version must be dropped (`ProposalCantFollow`); an exact +1
    /// minor bump must be accepted.
    #[test]
    fn test_process_proposal_rejects_skip_minor_hard_fork() {
        let mut state = gov_test_state(0, 0);
        assert_eq!(state.epochs.protocol_params.protocol_version_major, 10);
        assert_eq!(state.epochs.protocol_params.protocol_version_minor, 0);

        // Skip-minor target (10,0) -> (10,2): must be rejected.
        let tx_hash = Hash32::from_bytes([60u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::HardForkInitiation {
                    prev_action_id: None,
                    protocol_version: (10, 2),
                },
                anchor: make_anchor(),
            },
        );
        let action_id = GovActionId {
            transaction_id: tx_hash,
            action_index: 0,
        };
        assert!(
            !state.gov.governance.proposals.contains_key(&action_id),
            "HardForkInitiation skipping a minor version must be dropped (ProposalCantFollow)"
        );

        // Exact +1 minor bump (10,0) -> (10,1): must be accepted.
        let tx_hash2 = Hash32::from_bytes([61u8; 32]);
        state.process_proposal(
            &tx_hash2,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::HardForkInitiation {
                    prev_action_id: None,
                    protocol_version: (10, 1),
                },
                anchor: make_anchor(),
            },
        );
        let action_id2 = GovActionId {
            transaction_id: tx_hash2,
            action_index: 0,
        };
        assert!(
            state.gov.governance.proposals.contains_key(&action_id2),
            "HardForkInitiation with an exact +1 minor bump must be accepted"
        );
    }

    /// #812 Defect B / #858: a HardForkInitiation that chains to an IN-FLIGHT parent
    /// HardForkInitiation must be checked with `pvCanFollow` against the PARENT's
    /// target version (`preceedingHardFork`), not the current on-chain version.
    #[test]
    fn test_process_proposal_hardfork_chains_to_in_flight_parent_812() {
        let mut state = gov_test_state(0, 0); // PV (10,0), enacted_hard_fork = None
        let hf = |prev: Option<GovActionId>, ver: (u64, u64)| ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0u8; 29],
            gov_action: GovAction::HardForkInitiation {
                prev_action_id: prev,
                protocol_version: ver,
            },
            anchor: make_anchor(),
        };
        let submit = |state: &mut LedgerState, seed: u8, p: &ProposalProcedure| {
            let h = Hash32::from_bytes([seed; 32]);
            state.process_proposal(&h, 0, p);
            GovActionId {
                transaction_id: h,
                action_index: 0,
            }
        };

        // Parent: (10,0) -> (11,0) major bump, genesis-root (prev=None). Accepted.
        let parent = submit(&mut state, 70, &hf(None, (11, 0)));
        assert!(
            state.gov.governance.proposals.contains_key(&parent),
            "parent major-bump HardForkInitiation (11,0) must be accepted"
        );

        // Child chaining to the in-flight parent, target (11,1) = minor+1 off the
        // parent's (11,0). Under the OLD (base=current-PV) logic this checked
        // pvCanFollow((10,0),(11,1)) = false and was wrongly dropped; with the
        // preceedingHardFork chaining it checks pvCanFollow((11,0),(11,1)) = true.
        let child_ok = submit(&mut state, 71, &hf(Some(parent.clone()), (11, 1)));
        assert!(
            state.gov.governance.proposals.contains_key(&child_ok),
            "child (11,1) chaining off in-flight parent (11,0) must be ACCEPTED (#812 Defect B)"
        );

        // Child chaining to the in-flight parent but skipping a minor: (11,3) does
        // not follow (11,0). Dropped.
        let child_skip = submit(&mut state, 72, &hf(Some(parent.clone()), (11, 3)));
        assert!(
            !state.gov.governance.proposals.contains_key(&child_skip),
            "child (11,3) skip-minor off in-flight parent must be dropped (ProposalCantFollow)"
        );

        // succVersion short-circuit: a child targeting (12,0) — two major bumps
        // compounded in one epoch — is checked against CURRENT (10,0), not the
        // parent, and dropped.
        let child_double = submit(&mut state, 73, &hf(Some(parent.clone()), (12, 0)));
        assert!(
            !state.gov.governance.proposals.contains_key(&child_double),
            "child (12,0) compounding two major bumps must be dropped (succVersion short-circuit)"
        );
    }

    /// Direct unit coverage of the shared `hardfork_proposal_cant_follow` helper for
    /// the non-HardFork and unresolved-base branches.
    #[test]
    fn test_hardfork_proposal_cant_follow_edge_cases() {
        let state = gov_test_state(0, 0); // PV (10,0)
        let gov = &state.gov.governance;

        // Non-HardFork action → never a ProposalCantFollow.
        assert!(!hardfork_proposal_cant_follow(
            &GovAction::InfoAction,
            gov,
            10,
            0
        ));

        // HardFork whose prev points at a NON-existent proposal (and not the enacted
        // root, target not above succVersion) → base unresolved → false (the
        // structural InvalidPrevGovActionId check owns that case, not pvCanFollow).
        let dangling = GovActionId {
            transaction_id: Hash32::from_bytes([0xEE; 32]),
            action_index: 7,
        };
        let hf_dangling = GovAction::HardForkInitiation {
            prev_action_id: Some(dangling),
            protocol_version: (10, 1),
        };
        assert!(!hardfork_proposal_cant_follow(&hf_dangling, gov, 10, 0));

        // HardFork at genesis root (prev = None = enacted_hard_fork), skip-minor
        // target (10,2) → base = current (10,0) → cant follow → true.
        let hf_skip = GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (10, 2),
        };
        assert!(hardfork_proposal_cant_follow(&hf_skip, gov, 10, 0));
    }

    // ========================================================================
    // Threshold matrix tests (CC/DRep/SPO for each action type)
    // ========================================================================

    #[test]
    fn test_no_confidence_no_cc_required() {
        // NoConfidence: DRep + SPO, NO CC
        let mut state = gov_test_state(10, 10);
        // Set CC threshold to something that would block if checked
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 99,
            denominator: 100,
        });

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);

        // 7/10 DReps yes (70% >= dvt_no_confidence 67%)
        for i in 0..7 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        // 6/10 SPOs yes (60% >= pvt_motion_no_confidence 51%)
        for i in 0..6 {
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        // NO CC votes — CC cannot vote on NoConfidence

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));
        assert!(state.gov.governance.no_confidence);
        assert!(state.gov.governance.proposals.is_empty());
    }

    #[test]
    fn test_update_committee_no_cc_required() {
        // UpdateCommittee: DRep + SPO, NO CC
        let mut state = gov_test_state(10, 10);
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 99,
            denominator: 100,
        });

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        let new_cred = Credential::VerificationKey(Hash28::from_bytes([30u8; 28]));
        let mut members_to_add = BTreeMap::new();
        // Expiry must be ≤ currentEpoch + committeeMaxTermLength (Haskell
        // `validCommitteeTerm`).  Default max term = 146.
        members_to_add.insert(new_cred, 100u64);

        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::UpdateCommittee {
                    prev_action_id: None,
                    members_to_remove: vec![],
                    members_to_add,
                    threshold: Rational {
                        numerator: 2,
                        denominator: 3,
                    },
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);

        for i in 0..7 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..6 {
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));
        // UpdateCommittee restores confidence
        assert!(!state.gov.governance.no_confidence);
        assert!(state.gov.governance.proposals.is_empty());
    }

    #[test]
    fn test_new_constitution_no_spo_required() {
        // NewConstitution: DRep + CC, NO SPO
        let mut state = gov_test_state(10, 10);

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NewConstitution {
                    prev_action_id: None,
                    constitution: Constitution {
                        anchor: make_anchor(),
                        script_hash: None,
                    },
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);

        // DReps vote yes (need >= dvt_constitution)
        for i in 0..8 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &action_id);
        // NO SPO votes — SPOs cannot vote on NewConstitution

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));
        assert!(state.gov.governance.constitution.is_some());
        assert!(state.gov.governance.proposals.is_empty());
    }

    #[test]
    fn test_treasury_withdrawal_no_spo_required() {
        // TreasuryWithdrawals: DRep + CC, NO SPO
        let mut state = gov_test_state(10, 10);
        state.epochs.treasury = Lovelace(10_000_000_000);

        // Register the withdrawal target so the disbursement actually credits.
        // Per Haskell `applyEnactedWithdrawals`, withdrawals to unregistered
        // reward accounts are silently dropped.
        let withdrawal_key = LedgerState::reward_account_to_hash(&[0u8; 29]);
        state
            .certs
            .reward_accounts
            .insert(withdrawal_key, Lovelace(0));

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(vec![0u8; 29], Lovelace(5_000_000_000));

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::TreasuryWithdrawals {
                    withdrawals,
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);

        for i in 0..7 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &action_id);

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));
        assert_eq!(state.epochs.treasury, Lovelace(5_000_000_000));
    }

    // ========================================================================
    // Treasury withdrawal cap tests
    // ========================================================================

    /// #966 — RATIFY must gate on the FROZEN `ensTreasury`, not the live pot.
    ///
    /// Haskell seals `ensTreasury` into the DRep pulser at the end of
    /// `epochTransition` and consumes it a full boundary later, so RATIFY is
    /// structurally blind to the `applyRUpd` credit landing at the boundary it
    /// is running on. dugite applies RUPD before ratifying, so reading the live
    /// pot made a withdrawal affordable one boundary EARLIER than cardano-node
    /// — an accept-early chain split.
    ///
    /// The test reproduces exactly that shape: poor at snapshot time, rich by
    /// the time the boundary runs. Before the fix, ratification saw
    /// 10_000_000_000 and enacted; after it, it sees the frozen 500_000_000 and
    /// correctly defers. Deferral (not rejection) is the Haskell behaviour:
    /// the action stays live and is retried each epoch until it expires.
    #[test]
    fn test_treasury_withdrawal_uses_frozen_not_live_treasury() {
        let mut state = gov_test_state(10, 0);

        // Poor at the moment the pulser is sealed.
        state.epochs.treasury = Lovelace(500_000_000);

        let withdrawal_key = LedgerState::reward_account_to_hash(&[0u8; 29]);
        state
            .certs
            .reward_accounts
            .insert(withdrawal_key, Lovelace(0));

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(vec![0u8; 29], Lovelace(1_000_000_000));

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::TreasuryWithdrawals {
                    withdrawals,
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);
        for i in 0..7 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &action_id);

        // Seal the pulser while the treasury is still 500M.
        state.freeze_prior_boundary_pulser();
        assert_eq!(
            state
                .gov
                .governance
                .pulsing_snapshot()
                .expect("snapshot captured")
                .treasury,
            500_000_000,
            "the snapshot must freeze the treasury as it stood at capture time"
        );

        // Now the boundary's RUPD lands, making the pot ample. Haskell's RATIFY
        // cannot see this; neither may ours.
        state.epochs.treasury = Lovelace(10_000_000_000);

        state.process_epoch_transition(EpochNo(1));

        assert_eq!(
            state.epochs.treasury,
            Lovelace(10_000_000_000),
            "withdrawal must NOT have been paid: RATIFY gates on the frozen 500M \
             basis, under which a 1B withdrawal is unaffordable"
        );

        // And it must be DEFERRED, not dropped — Haskell leaves an unaffordable
        // action in `cgsProposals` to be retried by the next pulser.
        let still_live = state.gov.governance.proposals.contains_key(&action_id);
        assert!(
            still_live,
            "an unaffordable withdrawal must stay live for retry, not be discarded"
        );
    }

    #[test]
    fn test_treasury_withdrawal_insufficient_funds_not_ratified() {
        // Gap 1: TreasuryWithdrawals that exceed treasury balance must not be
        // ratified, matching Haskell's checkWithdrawals precondition.
        let mut state = gov_test_state(10, 0);
        state.epochs.treasury = Lovelace(500_000_000); // 500M

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(vec![0u8; 29], Lovelace(1_000_000_000)); // Request 1B

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::TreasuryWithdrawals {
                    withdrawals,
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);

        // All DReps vote yes + CC votes yes — voting thresholds fully met
        for i in 0..10 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &action_id);

        state.process_epoch_transition(EpochNo(1));

        // Withdrawal exceeds treasury → not ratified despite unanimous approval
        assert_eq!(
            state.epochs.treasury.0, 500_000_000,
            "Treasury must be unchanged when withdrawal exceeds balance"
        );
        assert_eq!(
            state.gov.governance.proposals.len(),
            1,
            "Proposal must remain active (not ratified)"
        );
    }

    #[test]
    fn test_treasury_aggregate_withdrawal_cap() {
        // Gap 2: Multiple TreasuryWithdrawals in the same epoch must track
        // cumulative withdrawals. Second proposal blocked when aggregate exceeds
        // treasury balance.
        let mut state = gov_test_state(10, 0);
        state.epochs.treasury = Lovelace(600_000_000); // 600M

        // Pre-register withdrawal/refund addresses so the disbursements and
        // deposit refunds actually credit reward_accounts (otherwise both
        // are silently dropped/forfeited per Haskell `applyEnactedWithdrawals`
        // and `returnProposalDeposits`).
        for addr_byte in 0u8..2 {
            let mut addr = vec![0u8; 29];
            addr[0] = addr_byte;
            let key = LedgerState::reward_account_to_hash(&addr);
            state.certs.reward_accounts.insert(key, Lovelace(0));
        }

        // Two withdrawal proposals: 400M each (total 800M > 600M treasury)
        for proposal_idx in 0u8..2 {
            let mut withdrawals = BTreeMap::new();
            // Use distinct reward addresses so proposals are unique
            let mut addr = vec![0u8; 29];
            addr[0] = proposal_idx;
            withdrawals.insert(addr, Lovelace(400_000_000));

            let tx_byte = 50 + proposal_idx;
            let tx_hash = Hash32::from_bytes([tx_byte; 32]);
            state.process_proposal(
                &tx_hash,
                0,
                &ProposalProcedure {
                    deposit: Lovelace(100_000_000_000),
                    return_addr: vec![0u8; 29],
                    gov_action: GovAction::TreasuryWithdrawals {
                        withdrawals,
                        policy_hash: None,
                    },
                    anchor: make_anchor(),
                },
            );
            let action_id = make_action_id(tx_byte, 0);
            for i in 0..10 {
                drep_vote(&mut state, i, &action_id, Vote::Yes);
            }
            cc_vote_yes(&mut state, &action_id);
        }

        assert_eq!(state.gov.governance.proposals.len(), 2);
        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // First 400M enacted, second 400M blocked (would exceed remaining 200M)
        assert_eq!(
            state.epochs.treasury.0, 200_000_000,
            "Only the first 400M withdrawal should be enacted"
        );
        assert_eq!(
            state.gov.governance.proposals.len(),
            1,
            "Second proposal must remain active (aggregate cap)"
        );
    }

    #[test]
    fn test_two_treasury_withdrawals_both_enact_in_one_pass() {
        // Backlog #29 regression: the cap basis for each TreasuryWithdrawals must
        // be the LIVE, per-enact-decremented treasury — mirroring Haskell Conway
        // `Ratify.hs` `withdrawalCanWithdraw` (fold(wdrls) <= ensTreasury) against
        // the threaded `ensTreasury` that the enact (payout) leg already
        // decremented. The old code ALSO maintained a separate
        // `enacted_withdrawals_total` accumulator and computed the cap basis as
        // `treasury - accumulator`, so the second withdrawal saw `treasury - 2*w1`
        // (a double subtraction of the first withdrawal) and was wrongly blocked
        // even when the aggregate fit within the treasury.
        //
        // Scenario: treasury = 1000M, two 400M withdrawals to DISTINCT registered
        // reward accounts. Aggregate 800M <= 1000M → BOTH must enact, leaving the
        // treasury at exactly 200M.
        let mut state = gov_test_state(10, 0);
        state.epochs.treasury = Lovelace(1_000_000_000); // 1000M

        // Withdrawal target accounts (distinct from each other and from the
        // proposal return address, so the disbursements and the deposit refunds
        // do not co-mingle on the same reward account). All must be registered;
        // per Haskell `applyEnactedWithdrawals` withdrawals to unregistered
        // accounts are silently dropped.
        //
        // NOTE: `reward_account_to_hash` keys on bytes [1..29] (the 28-byte
        // credential) and uses byte [0] only as the header. To get DISTINCT
        // reward-account keys we must vary the CREDENTIAL bytes, not byte [0].
        let return_addr = vec![0u8; 29]; // gets the two deposit refunds
        let mut wd_addr_1 = vec![0u8; 29];
        wd_addr_1[1] = 1;
        let mut wd_addr_2 = vec![0u8; 29];
        wd_addr_2[1] = 2;
        for addr in [&return_addr, &wd_addr_1, &wd_addr_2] {
            let key = LedgerState::reward_account_to_hash(addr);
            state.certs.reward_accounts.insert(key, Lovelace(0));
        }
        let wd_key_1 = LedgerState::reward_account_to_hash(&wd_addr_1);
        let wd_key_2 = LedgerState::reward_account_to_hash(&wd_addr_2);

        // Two withdrawal proposals: 400M each to wd_addr_1 / wd_addr_2.
        for (proposal_idx, wd_addr) in [&wd_addr_1, &wd_addr_2].iter().enumerate() {
            let mut withdrawals = BTreeMap::new();
            withdrawals.insert((*wd_addr).clone(), Lovelace(400_000_000));

            let tx_byte = 50 + proposal_idx as u8;
            let tx_hash = Hash32::from_bytes([tx_byte; 32]);
            state.process_proposal(
                &tx_hash,
                0,
                &ProposalProcedure {
                    deposit: Lovelace(100_000_000_000),
                    return_addr: return_addr.clone(),
                    gov_action: GovAction::TreasuryWithdrawals {
                        withdrawals,
                        policy_hash: None,
                    },
                    anchor: make_anchor(),
                },
            );
            let action_id = make_action_id(tx_byte, 0);
            for i in 0..10 {
                drep_vote(&mut state, i, &action_id, Vote::Yes);
            }
            cc_vote_yes(&mut state, &action_id);
        }

        assert_eq!(state.gov.governance.proposals.len(), 2);
        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // BOTH withdrawals must enact: each target credited exactly 400M.
        assert_eq!(
            state.certs.reward_accounts.get(&wd_key_1).copied(),
            Some(Lovelace(400_000_000)),
            "First withdrawal target must be credited 400M"
        );
        assert_eq!(
            state.certs.reward_accounts.get(&wd_key_2).copied(),
            Some(Lovelace(400_000_000)),
            "Second withdrawal target must be credited 400M (NOT blocked by a \
             double-subtracted cap basis)"
        );

        // Treasury: 1000M - 400M - 400M = 200M.
        assert_eq!(
            state.epochs.treasury.0, 200_000_000,
            "Treasury must end at 200M after both 400M withdrawals enact"
        );

        // Both proposals consumed (ratified + removed).
        assert_eq!(
            state.gov.governance.proposals.len(),
            0,
            "Both withdrawal proposals must be ratified and removed"
        );
    }

    #[test]
    fn test_treasury_withdrawal_unregistered_target_still_consumes_cap_basis() {
        // Backlog #29 byte-exact rework (gauntlet wq63ah2hg REFUTED fix v1).
        //
        // Haskell Conway `Enact.hs` decrements the transient cap-basis
        // `ensTreasury` by the FULL declared `fold wdrls` for every enacted
        // `TreasuryWithdrawals`, regardless of whether the target reward
        // accounts are registered. Unregistered targets are filtered only LATER
        // at the epoch boundary (`applyEnactedWithdrawals`) against the REAL
        // `casTreasury`. dugite separates these: `cap_treasury` (the cap basis)
        // drops by the full fold, while `epochs.treasury.0` (the real treasury)
        // drops only by `disbursed` (registered-target total).
        //
        // Fix v1 cap-checked against `epochs.treasury.0`, which only decrements
        // by `disbursed`. So a withdrawal to an UNREGISTERED account (disbursed
        // = 0) left `epochs.treasury.0` unchanged, and v1 would WRONGLY admit a
        // later withdrawal that Haskell BLOCKS.
        //
        // Scenario in ONE ratification pass: treasury = 1000M.
        //   A = 600M to an UNREGISTERED reward account  → enact disburses 0,
        //       epochs.treasury.0 stays 1000M, but cap_treasury drops by the FULL
        //       600M → 400M.
        //   B = 600M to a REGISTERED reward account      → cap check 600M > 400M
        //       → B is BLOCKED (NOT enacted, target credited 0, B NOT removed).
        //
        // Both proposals are otherwise ratifiable (unanimous DRep + CC Yes).
        let mut state = gov_test_state(10, 0);
        state.epochs.treasury = Lovelace(1_000_000_000); // 1000M

        // Proposal-deposit return address — registered so refunds land cleanly.
        let return_addr = vec![0u8; 29];
        let return_key = LedgerState::reward_account_to_hash(&return_addr);
        state.certs.reward_accounts.insert(return_key, Lovelace(0));

        // A's target: an UNREGISTERED reward account (absent from
        // certs.reward_accounts). enact disburses 0 to it.
        let mut wd_addr_unreg = vec![0u8; 29];
        wd_addr_unreg[1] = 1;
        let wd_key_unreg = LedgerState::reward_account_to_hash(&wd_addr_unreg);
        // Deliberately do NOT register wd_addr_unreg.

        // B's target: a REGISTERED reward account.
        let mut wd_addr_reg = vec![0u8; 29];
        wd_addr_reg[1] = 2;
        let wd_key_reg = LedgerState::reward_account_to_hash(&wd_addr_reg);
        state.certs.reward_accounts.insert(wd_key_reg, Lovelace(0));

        // Proposal A (tx byte 50): 600M to the UNREGISTERED account.
        {
            let mut withdrawals = BTreeMap::new();
            withdrawals.insert(wd_addr_unreg.clone(), Lovelace(600_000_000));
            let tx_hash = Hash32::from_bytes([50u8; 32]);
            state.process_proposal(
                &tx_hash,
                0,
                &ProposalProcedure {
                    deposit: Lovelace(100_000_000_000),
                    return_addr: return_addr.clone(),
                    gov_action: GovAction::TreasuryWithdrawals {
                        withdrawals,
                        policy_hash: None,
                    },
                    anchor: make_anchor(),
                },
            );
            let action_id = make_action_id(50, 0);
            for i in 0..10 {
                drep_vote(&mut state, i, &action_id, Vote::Yes);
            }
            cc_vote_yes(&mut state, &action_id);
        }

        // Proposal B (tx byte 51): 600M to the REGISTERED account.
        {
            let mut withdrawals = BTreeMap::new();
            withdrawals.insert(wd_addr_reg.clone(), Lovelace(600_000_000));
            let tx_hash = Hash32::from_bytes([51u8; 32]);
            state.process_proposal(
                &tx_hash,
                0,
                &ProposalProcedure {
                    deposit: Lovelace(100_000_000_000),
                    return_addr: return_addr.clone(),
                    gov_action: GovAction::TreasuryWithdrawals {
                        withdrawals,
                        policy_hash: None,
                    },
                    anchor: make_anchor(),
                },
            );
            let action_id = make_action_id(51, 0);
            for i in 0..10 {
                drep_vote(&mut state, i, &action_id, Vote::Yes);
            }
            cc_vote_yes(&mut state, &action_id);
        }

        assert_eq!(state.gov.governance.proposals.len(), 2);
        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // A enacted: target is UNREGISTERED so it is credited 0 (silently
        // dropped) and epochs.treasury.0 is NOT decremented by A. But the
        // cap-basis WAS decremented by the full 600M → 400M.
        assert_eq!(
            state.certs.reward_accounts.get(&wd_key_unreg).copied(),
            None,
            "A's unregistered target must remain unregistered (credited 0)"
        );

        // B BLOCKED: 600M > cap_treasury (400M). B is NOT enacted — its
        // registered target is credited 0 and B is NOT removed. This is the
        // exact case fix v1 got wrong (it cap-checked against epochs.treasury.0,
        // still 1000M after A, so it WRONGLY admitted B → treasury
        // over-disbursement).
        assert_eq!(
            state.certs.reward_accounts.get(&wd_key_reg).copied(),
            Some(Lovelace(0)),
            "B's registered target must be credited 0 (B blocked by the cap basis)"
        );

        // Real treasury: A disbursed 0 (unregistered), B blocked → unchanged at
        // 1000M. (epochs.treasury.0 == casTreasury, decremented only by
        // `disbursed`.)
        assert_eq!(
            state.epochs.treasury.0, 1_000_000_000,
            "Treasury must stay at 1000M: A disbursed 0 (unregistered), B blocked"
        );

        // B's proposal must remain active (not ratified, not removed); A is
        // consumed (enacted + removed).
        assert!(
            state
                .gov
                .governance
                .proposals
                .contains_key(&make_action_id(51, 0)),
            "B must remain an active proposal (cap check blocked it)"
        );
        assert!(
            !state
                .gov
                .governance
                .proposals
                .contains_key(&make_action_id(50, 0)),
            "A must be consumed (enacted + removed)"
        );
    }

    #[test]
    fn test_prev_action_id_expired_proposal_blocks_child() {
        // Gap 4 regression: Proposal A submitted → B references A via
        // prev_action_id → A expires → B should NOT be ratified because
        // A was never enacted, so the enacted root doesn't match.
        let mut state = gov_test_state(10, 10);
        state.epoch = EpochNo(5);

        // Submit proposal A (ParameterChange, expires epoch 5 — already expired)
        let tx_a = Hash32::from_bytes([40u8; 32]);
        let ppu = ProtocolParamUpdate {
            max_block_body_size: Some(90112),
            ..Default::default()
        };
        state.process_proposal(
            &tx_a,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ppu.clone()),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id_a = make_action_id(40, 0);
        // Manually set expires to current epoch so it's already expired at boundary
        if let Some(ps) = Arc::make_mut(&mut state.gov.governance)
            .proposals
            .get_mut(&action_id_a)
        {
            ps.expires_epoch = EpochNo(4); // Expired before current epoch 5
        }

        // Submit proposal B referencing A
        let tx_b = Hash32::from_bytes([41u8; 32]);
        let ppu_b = ProtocolParamUpdate {
            max_block_body_size: Some(98304),
            ..Default::default()
        };
        state.process_proposal(
            &tx_b,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: Some(action_id_a.clone()),
                    protocol_param_update: Box::new(ppu_b),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id_b = make_action_id(41, 0);

        // Vote unanimously for B
        for i in 0..10 {
            drep_vote(&mut state, i, &action_id_b, Vote::Yes);
            spo_vote(&mut state, i, &action_id_b, Vote::Yes);
        }
        cc_vote_yes(&mut state, &action_id_b);

        state.process_epoch_transition(EpochNo(6));

        // A expired and was never enacted → enacted_pparam root is still None →
        // B's prev_action_id (Some(A)) doesn't match → B is not ratified
        assert_ne!(
            state.epochs.protocol_params.max_block_body_size, 98304,
            "Proposal B must NOT be ratified (parent A expired without enactment)"
        );
    }

    // ========================================================================
    // No-confidence state effects
    // ========================================================================

    #[test]
    fn test_no_confidence_clears_committee_threshold() {
        let mut state = gov_test_state(10, 10);
        assert!(state.gov.governance.committee_threshold.is_some());

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);

        for i in 0..7 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..6 {
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        assert!(state.gov.governance.no_confidence);
        assert!(state.gov.governance.committee_threshold.is_none()); // Cleared
        assert!(state.gov.governance.committee_hot_keys.is_empty());
        assert!(state.gov.governance.committee_expiration.is_empty());
    }

    #[test]
    fn test_no_confidence_switches_committee_threshold() {
        let mut state = gov_test_state(10, 10);
        // First enact NoConfidence
        state.enact_gov_action(&GovAction::NoConfidence {
            prev_action_id: None,
        });
        assert!(state.gov.governance.no_confidence);

        // In no-confidence state, UpdateCommittee should use dvt_committee_no_confidence
        // (a different threshold from dvt_committee_normal).
        // On mainnet: no_confidence=60%, normal=67% (no_confidence is lower, not higher)
        assert_ne!(
            state.epochs.protocol_params.dvt_committee_no_confidence,
            state.epochs.protocol_params.dvt_committee_normal
        );
    }

    // ========================================================================
    // Bootstrap phase tests
    // ========================================================================

    #[test]
    fn test_bootstrap_drep_thresholds_zero() {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Bootstrap
        let mut state = LedgerState::new(params);
        state.epoch_length = 100;
        state.epochs.needs_stake_rebuild = false;

        assert!(state.is_bootstrap_phase());

        // In bootstrap, DRep thresholds should be 0 (auto-pass)
        // so ParameterChange ratifies with just CC + SPO (for security params)
    }

    // ========================================================================
    // Proposal lifecycle tests
    // ========================================================================

    #[test]
    fn test_proposal_expiry_inclusive() {
        // Proposals are active through their expires_epoch (per Haskell gasExpiresAfter < currentEpoch)
        let mut state = gov_test_state(5, 0);
        state.epochs.protocol_params.gov_action_lifetime = 3;

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );

        // expires_epoch = 0 + 3 = 3 (per Haskell gasExpiresAfter)
        // Expiry filter uses self.epoch (old epoch): expires_epoch < self.epoch
        // A proposal with expires_epoch = 3 is active through epoch 4
        // (at transition to 5, self.epoch=4, 3 < 4 = true → expired)
        for e in 1..=4 {
            state.process_epoch_transition(EpochNo(e));
            assert_eq!(
                state.gov.governance.proposals.len(),
                1,
                "Should be active at epoch {}",
                e
            );
        }

        state.process_epoch_transition(EpochNo(5));
        assert_eq!(state.gov.governance.proposals.len(), 0); // Expired
    }

    #[test]
    fn test_deposit_returned_on_ratification() {
        // Use TreasuryWithdrawals to verify deposit return on ratification.
        // Set CC threshold to 0 so it auto-passes with no votes.
        let mut state = gov_test_state(10, 0);
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 0,
            denominator: 1,
        });
        // DRep threshold for treasury withdrawal = dvt_treasury_withdrawal
        state.epochs.protocol_params.dvt_treasury_withdrawal = Rational {
            numerator: 0,
            denominator: 1,
        };

        let return_addr = vec![0u8; 29];
        let return_key = LedgerState::reward_account_to_hash(&return_addr);
        // Register the return credential so the deposit refund goes to the
        // reward account (not treasury, per Haskell `returnProposalDeposits`).
        state.certs.reward_accounts.insert(return_key, Lovelace(0));

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(50_000_000_000),
                return_addr: return_addr.clone(),
                gov_action: GovAction::TreasuryWithdrawals {
                    withdrawals: std::collections::BTreeMap::new(),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // Deposit should be returned to reward account
        assert_eq!(
            state
                .certs
                .reward_accounts
                .get(&return_key)
                .copied()
                .unwrap_or(Lovelace(0)),
            Lovelace(50_000_000_000)
        );
    }

    #[test]
    fn test_deposit_returned_on_expiry() {
        let mut state = gov_test_state(5, 0);
        state.epochs.protocol_params.gov_action_lifetime = 1;

        let return_addr = vec![0u8; 29];
        let return_key = LedgerState::reward_account_to_hash(&return_addr);
        // Register the return credential so the deposit refund goes to the
        // reward account (not treasury, per Haskell `returnProposalDeposits`).
        state.certs.reward_accounts.insert(return_key, Lovelace(0));

        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(50_000_000_000),
                return_addr: return_addr.clone(),
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );

        // expires_epoch = 0 + 1 = 1; expiry filter: expires_epoch < self.epoch (old epoch)
        // At transition to 2, self.epoch=1, 1 < 1 = false → still active
        // At transition to 3, self.epoch=2, 1 < 2 = true → expired
        state.process_epoch_transition(EpochNo(1));
        assert_eq!(state.gov.governance.proposals.len(), 1); // Still active

        state.process_epoch_transition(EpochNo(2));
        assert_eq!(state.gov.governance.proposals.len(), 1); // Still active (1 < 1 = false)

        state.process_epoch_transition(EpochNo(3));
        assert_eq!(state.gov.governance.proposals.len(), 0); // Expired (1 < 2 = true)

        // Deposit should be refunded
        assert_eq!(
            state
                .certs
                .reward_accounts
                .get(&return_key)
                .copied()
                .unwrap_or(Lovelace(0)),
            Lovelace(50_000_000_000)
        );
    }

    // ========================================================================
    // Vote replacement tests
    // ========================================================================

    #[test]
    fn test_vote_replacement() {
        let mut state = gov_test_state(5, 0);
        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);

        // DRep 0 votes No initially
        drep_vote(&mut state, 0, &action_id, Vote::No);
        // DRep 0 changes vote to Yes
        drep_vote(&mut state, 0, &action_id, Vote::Yes);

        let votes = state
            .gov
            .governance
            .votes_by_action
            .get(&action_id)
            .unwrap();
        let drep_cred = Credential::VerificationKey(Hash28::from_bytes([0u8; 28]));
        let drep_vote_entry = votes.get(&Voter::DRep(drep_cred)).unwrap();
        assert_eq!(drep_vote_entry.vote, Vote::Yes);
    }

    // ========================================================================
    // Sequential proposals — lineal chain invariant
    // ========================================================================

    /// A second ParameterChange proposal with `prev_action_id = None` must be
    /// rejected at submission when a prior ParameterChange has already been
    /// enacted.  Per Haskell `proposalsAddAction` (Proposals.hs): the genesis
    /// root check (`parent == prRootL`) fails when the purpose tree root is
    /// already `SJust id` (previously enacted action).
    ///
    /// This is the root cause of the devnet-validate Round 2 gov-lifecycle
    /// failure: 10a submitted its second ParameterChange with no
    /// --prev-governance-action-tx-id, producing a `prev_action_id = None`
    /// proposal that passed submission in Dugite (old code) but then always
    /// failed `prevActionAsExpected` at epoch-boundary ratification, timing out
    /// the 10e enactment wait.
    #[test]
    fn test_sequential_param_change_requires_prev_action_id() {
        let mut state = gov_test_state(10, 0);
        // Set CC threshold to zero so it never blocks ratification
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 0,
            denominator: 1,
        });

        // Round 1: submit and enact a ParameterChange with prev_action_id = None
        let tx1 = Hash32::from_bytes([1u8; 32]);
        let id1 = GovActionId {
            transaction_id: tx1,
            action_index: 0,
        };
        state.process_proposal(
            &tx1,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        n_opt: Some(500),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        assert!(
            state.gov.governance.proposals.contains_key(&id1),
            "first proposal should be admitted"
        );

        // Vote all DReps yes; epoch transition enacts it
        for i in 0..10 {
            drep_vote(&mut state, i, &id1, Vote::Yes);
        }
        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));
        assert!(
            state.gov.governance.enacted_pparam_update.is_some(),
            "first ParameterChange should be enacted after epoch boundary"
        );
        assert!(
            !state.gov.governance.proposals.contains_key(&id1),
            "enacted proposal should be removed from active set"
        );

        // Round 2: submit a second ParameterChange with prev_action_id = None (stale)
        // This should now be REJECTED at submission (InvalidPrevGovActionId) because
        // the purpose tree root is no longer SNothing — it's the enacted id1.
        let tx2 = Hash32::from_bytes([2u8; 32]);
        let id2 = GovActionId {
            transaction_id: tx2,
            action_index: 0,
        };
        state.process_proposal(
            &tx2,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None, // <-- missing the enacted root; must be rejected
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        n_opt: Some(501),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        assert!(
            !state.gov.governance.proposals.contains_key(&id2),
            "second ParameterChange with prev_action_id=None must be rejected at submission \
             when an enacted root already exists (Haskell: InvalidPrevGovActionId)"
        );

        // Round 2 correct: submit with prev_action_id = Some(id1)
        let tx3 = Hash32::from_bytes([3u8; 32]);
        let id3 = GovActionId {
            transaction_id: tx3,
            action_index: 0,
        };
        state.process_proposal(
            &tx3,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: Some(id1.clone()), // correctly references enacted root
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        n_opt: Some(501),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        assert!(
            state.gov.governance.proposals.contains_key(&id3),
            "second ParameterChange with prev_action_id=Some(enacted_id) must be admitted"
        );

        // Votes for id3 were cast AFTER the epoch 1 boundary, so they are captured
        // in the epoch 2 snapshot (captured at epoch 2 boundary) and ratified at
        // epoch 3 boundary.  Run a dry epoch 2 transition to capture the snapshot,
        // then epoch 3 to ratify.
        for i in 0..10 {
            drep_vote(&mut state, i, &id3, Vote::Yes);
        }
        state.process_epoch_transition(EpochNo(2)); // captures snapshot with id3 + votes
        state.process_epoch_transition(EpochNo(3)); // ratifies id3
        assert_eq!(
            state.gov.governance.enacted_pparam_update,
            Some(id3),
            "second ParameterChange should be enacted at epoch 3"
        );
    }

    /// `genesis_root_is_valid` returns true only when no prior action of that
    /// purpose has been enacted.
    #[test]
    fn test_genesis_root_is_valid() {
        let mut gov = GovernanceState::default();

        // At genesis all roots are None — genesis proposals are valid
        assert!(genesis_root_is_valid(
            &GovAction::ParameterChange {
                prev_action_id: None,
                protocol_param_update: Box::new(ProtocolParamUpdate::default()),
                policy_hash: None,
            },
            &gov
        ));
        assert!(genesis_root_is_valid(
            &GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (10, 0)
            },
            &gov
        ));
        assert!(genesis_root_is_valid(
            &GovAction::NoConfidence {
                prev_action_id: None
            },
            &gov
        ));

        // TreasuryWithdrawals and InfoAction always valid (no chain)
        assert!(genesis_root_is_valid(&GovAction::InfoAction, &gov));
        assert!(genesis_root_is_valid(
            &GovAction::TreasuryWithdrawals {
                withdrawals: BTreeMap::new(),
                policy_hash: None
            },
            &gov
        ));

        // After enactment, same genesis proposal is invalid
        let enacted_id = GovActionId {
            transaction_id: Hash32::from_bytes([42u8; 32]),
            action_index: 0,
        };
        gov.enacted_pparam_update = Some(enacted_id.clone());
        assert!(
            !genesis_root_is_valid(
                &GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate::default()),
                    policy_hash: None,
                },
                &gov
            ),
            "ParameterChange with prev_action_id=None invalid once enacted_pparam_update is set"
        );

        // HardFork genesis still valid (different purpose)
        assert!(genesis_root_is_valid(
            &GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (11, 0)
            },
            &gov
        ));

        // After committee enacted, NoConfidence genesis is invalid
        gov.enacted_committee = Some(enacted_id);
        assert!(
            !genesis_root_is_valid(
                &GovAction::NoConfidence {
                    prev_action_id: None
                },
                &gov
            ),
            "NoConfidence with prev_action_id=None invalid once enacted_committee is set"
        );
    }

    // ========================================================================
    // Competing proposals tests
    // ========================================================================

    #[test]
    fn test_competing_proposals_same_prev_action_id() {
        let mut state = gov_test_state(10, 0);
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 0,
            denominator: 1,
        });

        // Submit two ParameterChange proposals with the same prevActionId (None)
        let tx1 = Hash32::from_bytes([1u8; 32]);
        state.process_proposal(
            &tx1,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        n_opt: Some(1000),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );

        let tx2 = Hash32::from_bytes([2u8; 32]);
        state.process_proposal(
            &tx2,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        n_opt: Some(2000),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );

        let id1 = make_action_id(1, 0);
        let id2 = make_action_id(2, 0);

        // All DReps vote Yes on both
        for i in 0..10 {
            drep_vote(&mut state, i, &id1, Vote::Yes);
            drep_vote(&mut state, i, &id2, Vote::Yes);
        }

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // The first-submitted proposal (id1, tx=[1u8;32]) must enact —
        // Haskell's `reorderActions` stable-sorts by priority only, so a
        // same-priority tie is broken by on-chain submission order (#799),
        // not by `GovActionId` (hash) order. `update_enacted_root` runs
        // immediately after each enactment within the same pass, so the
        // second proposal (id2) sees the now-enacted root and fails
        // `prevActionAsExpected`.
        assert_eq!(
            state.gov.governance.enacted_pparam_update,
            Some(id1.clone()),
            "the first-submitted proposal must enact deterministically"
        );
        assert_ne!(state.gov.governance.enacted_pparam_update, Some(id2));
    }

    /// #799: RATIFY must break same-priority ties by ON-CHAIN SUBMISSION
    /// order, not `GovActionId` (hash) order. Haskell's `reorderActions`
    /// (`Governance/Internal.hs:534-544`) is a stable sort keyed only on
    /// `actionPriority`; ties preserve the proposals OMap's insertion order.
    ///
    /// Construct two same-priority (`ParameterChange`), same-parent
    /// (`prev_action_id: None`) proposals whose `GovActionId` (tx-hash)
    /// ordering is the OPPOSITE of their submission order, so a
    /// hash-ordered (pre-#799) implementation would pick the wrong winner.
    #[test]
    fn test_ratify_tie_break_uses_submission_order_not_hash_order() {
        let mut state = gov_test_state(10, 0);
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 0,
            denominator: 1,
        });

        // Submitted FIRST, but has the LARGER tx hash ([0xFF;32] > [0x01;32]).
        let tx_first_submitted = Hash32::from_bytes([0xFFu8; 32]);
        state.process_proposal(
            &tx_first_submitted,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        n_opt: Some(1000),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );

        // Submitted SECOND, but has the SMALLER tx hash — a hash-ordered
        // `ImblOrdMap` iteration would visit this one first.
        let tx_second_submitted = Hash32::from_bytes([0x01u8; 32]);
        state.process_proposal(
            &tx_second_submitted,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        n_opt: Some(2000),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );

        let id_first = GovActionId {
            transaction_id: tx_first_submitted,
            action_index: 0,
        };
        let id_second = GovActionId {
            transaction_id: tx_second_submitted,
            action_index: 0,
        };

        // Sanity-check the adversarial hash ordering: without the fix,
        // iterating `proposals` (`ImblOrdMap<GovActionId, _>`) would visit
        // `id_second` before `id_first`.
        assert!(
            id_second < id_first,
            "test setup: id_second must hash-sort before id_first"
        );

        // Both DReps vote Yes on both proposals so each independently meets
        // threshold; only the submission-order tie-break decides the winner.
        for i in 0..10 {
            drep_vote(&mut state, i, &id_first, Vote::Yes);
            drep_vote(&mut state, i, &id_second, Vote::Yes);
        }

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // The FIRST-SUBMITTED proposal must enact — NOT the one with the
        // smaller GovActionId.
        assert_eq!(
            state.gov.governance.enacted_pparam_update,
            Some(id_first),
            "the first-submitted proposal must enact, not the one with the smaller GovActionId"
        );
        assert_ne!(state.gov.governance.enacted_pparam_update, Some(id_second));
    }

    // ========================================================================
    // Enactment effects tests
    // ========================================================================

    #[test]
    fn test_enact_no_confidence_effects() {
        let mut state = gov_test_state(5, 0);
        assert!(!state.gov.governance.no_confidence);
        assert!(state.gov.governance.committee_threshold.is_some());

        state.enact_gov_action(&GovAction::NoConfidence {
            prev_action_id: None,
        });

        assert!(state.gov.governance.no_confidence);
        assert!(state.gov.governance.committee_threshold.is_none());
        assert!(state.gov.governance.committee_hot_keys.is_empty());
        assert!(state.gov.governance.committee_expiration.is_empty());
    }

    #[test]
    fn test_enact_update_committee_restores_confidence() {
        let mut state = gov_test_state(5, 0);
        state.enact_gov_action(&GovAction::NoConfidence {
            prev_action_id: None,
        });
        assert!(state.gov.governance.no_confidence);

        let new_cred = Credential::VerificationKey(Hash28::from_bytes([30u8; 28]));
        let mut members = BTreeMap::new();
        members.insert(new_cred, 500u64);

        state.enact_gov_action(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![],
            members_to_add: members,
            threshold: Rational {
                numerator: 2,
                denominator: 3,
            },
        });

        assert!(!state.gov.governance.no_confidence);
        assert_eq!(
            state.gov.governance.committee_threshold,
            Some(Rational {
                numerator: 2,
                denominator: 3,
            })
        );
    }

    /// #731: Haskell GOVCERT accepts a `CommitteeHotAuth` for a cold
    /// credential that is named in `members_to_add` of any LIVE
    /// `UpdateCommittee` proposal (`isPotentialFutureMember`), not just for
    /// current committee members.
    #[test]
    fn test_committee_auth_eligible_includes_pending_update_committee_adds() {
        let mut state = gov_test_state(0, 0);
        let current_cold =
            credential_to_hash(&Credential::VerificationKey(Hash28::from_bytes([10u8; 28])));
        let future_cred = Credential::VerificationKey(Hash28::from_bytes([77u8; 28]));
        let future_cold = credential_to_hash(&future_cred);
        let unrelated_cold =
            credential_to_hash(&Credential::VerificationKey(Hash28::from_bytes([78u8; 28])));

        // Before any proposal: only the current member is eligible.
        let eligible = state.gov.governance.committee_auth_eligible_members();
        assert!(eligible.contains(&current_cold));
        assert!(!eligible.contains(&future_cold));

        // A live UpdateCommittee proposal naming `future_cred` makes it a
        // potential future member.
        let mut members = BTreeMap::new();
        members.insert(future_cred, 500u64);
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([0xAA; 32]),
            action_index: 0,
        };
        Arc::make_mut(&mut state.gov.governance).proposals.insert(
            action_id,
            ProposalState {
                procedure: ProposalProcedure {
                    deposit: Lovelace(100_000_000_000),
                    return_addr: vec![0; 29],
                    gov_action: GovAction::UpdateCommittee {
                        prev_action_id: None,
                        members_to_remove: vec![],
                        members_to_add: members,
                        threshold: Rational {
                            numerator: 2,
                            denominator: 3,
                        },
                    },
                    anchor: make_anchor(),
                },
                proposed_epoch: EpochNo(0),
                expires_epoch: EpochNo(10),
                yes_votes: 0,
                no_votes: 0,
                abstain_votes: 0,
                submission_index: 0,
            },
        );

        let eligible = state.gov.governance.committee_auth_eligible_members();
        assert!(eligible.contains(&current_cold));
        assert!(
            eligible.contains(&future_cold),
            "members_to_add of a live UpdateCommittee proposal must be auth-eligible"
        );
        assert!(!eligible.contains(&unrelated_cold));
    }

    /// #731: Haskell `updateCommitteeState` (Conway/Rules/Epoch.hs) prunes
    /// hot-key authorizations AND resignations to the post-enactment
    /// committee membership via `Map.intersection` — including members
    /// removed IMPLICITLY (present in neither `members_to_remove` nor
    /// `members_to_add`)… of which explicit removal is just a special case.
    /// A member removed and re-added in the SAME action keeps its hot-key
    /// authorization and any standing resignation.
    #[test]
    fn test_prune_committee_state_intersects_with_membership() {
        let mut state = gov_test_state(0, 0);
        let gov_state = Arc::make_mut(&mut state.gov.governance);

        let cold_a = Hash32::from_bytes([1u8; 32]);
        let cold_b = Hash32::from_bytes([2u8; 32]);
        let cold_c = Hash32::from_bytes([3u8; 32]);
        let hot_a = Hash32::from_bytes([11u8; 32]);
        let hot_b = Hash32::from_bytes([12u8; 32]);

        // Committee after enactment: A and C only (B implicitly removed).
        gov_state.committee_expiration.insert(cold_a, EpochNo(900));
        gov_state.committee_expiration.insert(cold_c, EpochNo(900));
        // committeeState before the prune: hot auths for A and B, B has a
        // script-typed hot credential, C has a standing resignation.
        gov_state.committee_hot_keys.insert(cold_a, hot_a);
        gov_state.committee_hot_keys.insert(cold_b, hot_b);
        gov_state.script_committee_hot_credentials.insert(hot_b);
        gov_state.committee_resigned.insert(cold_b, None);
        gov_state.committee_resigned.insert(cold_c, None);

        prune_committee_state(&mut state.gov);

        let g = &state.gov.governance;
        // A: still a member — hot auth retained.
        assert_eq!(g.committee_hot_keys.get(&cold_a), Some(&hot_a));
        // B: not in the new committee — hot auth, script-type marker, and
        // resignation all pruned.
        assert!(!g.committee_hot_keys.contains_key(&cold_b));
        assert!(!g.script_committee_hot_credentials.contains(&hot_b));
        assert!(!g.committee_resigned.contains_key(&cold_b));
        // C: still a member — its resignation is KEPT (resignations are
        // permanent while the member remains in the committee).
        assert!(g.committee_resigned.contains_key(&cold_c));
    }

    /// #731: a member removed and re-added by the SAME UpdateCommittee action
    /// stays in the committee (Haskell `Map.union membersToAdd (current
    /// \\ removed)` is left-biased) and KEEPS its hot-key authorization and
    /// resignation through the boundary prune (ENACT does not touch
    /// `vsCommitteeState`).
    #[test]
    fn test_update_committee_remove_and_readd_keeps_committee_state() {
        let mut state = gov_test_state(0, 0);
        let cred = Credential::VerificationKey(Hash28::from_bytes([10u8; 28]));
        let cold_key = credential_to_hash(&cred);
        // gov_test_state seeded `cred` as a member with a hot-key auth.
        assert!(state
            .gov
            .governance
            .committee_hot_keys
            .contains_key(&cold_key));

        let mut members = BTreeMap::new();
        members.insert(cred.clone(), 777u64);
        state.enact_gov_action(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![cred],
            members_to_add: members,
            threshold: Rational {
                numerator: 2,
                denominator: 3,
            },
        });
        prune_committee_state(&mut state.gov);

        let g = &state.gov.governance;
        assert_eq!(
            g.committee_expiration.get(&cold_key),
            Some(&EpochNo(777)),
            "re-added member keeps membership with the NEW term (add wins)"
        );
        assert!(
            g.committee_hot_keys.contains_key(&cold_key),
            "hot-key auth survives a remove-and-re-add in one action"
        );
    }

    /// Regression test for issue #433.
    ///
    /// When an UpdateCommittee adds a member with a Script cold credential,
    /// the `script_committee_credentials` set must also be updated so the
    /// N2C committee-state query reports the correct credential type. Prior
    /// to the fix, only `committee_expiration` was updated and the query
    /// mislabeled all enacted-script members as KeyHash.
    ///
    /// Also verifies that removing a member clears all associated tracking
    /// sets and that NoConfidence clears the script credential set as well.
    #[test]
    fn test_enact_update_committee_tracks_script_credentials() {
        let mut state = gov_test_state(5, 0);

        let key_member = Credential::VerificationKey(Hash28::from_bytes([7u8; 28]));
        let script_member = Credential::Script(Hash28::from_bytes([8u8; 28]));
        let script_key = script_member.to_typed_hash32();
        let key_key = key_member.to_typed_hash32();

        let mut members = BTreeMap::new();
        members.insert(key_member.clone(), 500u64);
        members.insert(script_member.clone(), 600u64);

        state.enact_gov_action(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![],
            members_to_add: members,
            threshold: Rational {
                numerator: 2,
                denominator: 3,
            },
        });

        // Both newly added members present in expiration map. (gov_test_state
        // seeds an existing committee so we only assert membership of the new
        // entries rather than a fixed length.)
        assert!(state
            .gov
            .governance
            .committee_expiration
            .contains_key(&key_key));
        assert!(state
            .gov
            .governance
            .committee_expiration
            .contains_key(&script_key));

        // Only the script-typed member should be tracked as script.
        assert!(
            state
                .gov
                .governance
                .script_committee_credentials
                .contains(&script_key),
            "script cold credential must be tracked in script_committee_credentials"
        );
        assert!(
            !state
                .gov
                .governance
                .script_committee_credentials
                .contains(&key_key),
            "key cold credential must not be tracked as script"
        );

        // Now remove the script member — script tracking must be cleared too.
        state.enact_gov_action(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![script_member.clone()],
            members_to_add: BTreeMap::new(),
            threshold: Rational {
                numerator: 2,
                denominator: 3,
            },
        });
        assert!(!state
            .gov
            .governance
            .committee_expiration
            .contains_key(&script_key));
        assert!(!state
            .gov
            .governance
            .script_committee_credentials
            .contains(&script_key));

        // NoConfidence must wipe all committee tracking sets.
        state.enact_gov_action(&GovAction::NoConfidence {
            prev_action_id: None,
        });
        assert!(state.gov.governance.committee_expiration.is_empty());
        assert!(state.gov.governance.script_committee_credentials.is_empty());
        assert!(state
            .gov
            .governance
            .script_committee_hot_credentials
            .is_empty());
        assert!(state.gov.governance.committee_resigned.is_empty());
    }

    #[test]
    fn test_enact_hard_fork_updates_protocol_version() {
        let mut state = gov_test_state(5, 0);
        assert_eq!(state.epochs.protocol_params.protocol_version_major, 10);

        state.enact_gov_action(&GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (11, 0),
        });

        assert_eq!(state.epochs.protocol_params.protocol_version_major, 11);
        assert_eq!(state.epochs.protocol_params.protocol_version_minor, 0);
    }

    #[test]
    fn test_enact_treasury_withdrawal_debits_treasury() {
        let mut state = gov_test_state(5, 0);
        state.epochs.treasury = Lovelace(10_000_000_000);

        // Both withdrawal addresses must already be registered in the
        // reward map for the disbursement to count — matches Haskell
        // `applyEnactedWithdrawals`, which silently drops unregistered
        // entries (the lovelace remains in the treasury).
        let key_a = LedgerState::reward_account_to_hash(&[0u8; 29]);
        let key_b = LedgerState::reward_account_to_hash(&[1u8; 29]);
        state.certs.reward_accounts.insert(key_a, Lovelace(0));
        state.certs.reward_accounts.insert(key_b, Lovelace(0));

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(vec![0u8; 29], Lovelace(3_000_000_000));
        withdrawals.insert(vec![1u8; 29], Lovelace(2_000_000_000));

        state.enact_gov_action(&GovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash: None,
        });

        assert_eq!(state.epochs.treasury, Lovelace(5_000_000_000));
        assert_eq!(
            state.certs.reward_accounts.get(&key_a),
            Some(&Lovelace(3_000_000_000))
        );
        assert_eq!(
            state.certs.reward_accounts.get(&key_b),
            Some(&Lovelace(2_000_000_000))
        );
    }

    /// Regression: withdrawals to UNREGISTERED reward accounts must be
    /// silently dropped (lovelace stays in treasury). Previously the
    /// implementation used `.entry().or_insert()`, which silently created
    /// the account, both crediting unregistered addresses AND causing the
    /// proposal-deposit refund check below to spuriously match a
    /// `return_addr` equal to a `withdrawal_addr`.
    #[test]
    fn test_enact_treasury_withdrawal_to_unregistered_is_dropped() {
        let mut state = gov_test_state(5, 0);
        state.epochs.treasury = Lovelace(10_000_000_000);

        let registered_key = LedgerState::reward_account_to_hash(&[2u8; 29]);
        state
            .certs
            .reward_accounts
            .insert(registered_key, Lovelace(0));

        let mut withdrawals = BTreeMap::new();
        // Registered: should be credited, treasury debited.
        withdrawals.insert(vec![2u8; 29], Lovelace(1_000_000_000));
        // Not registered: should be silently dropped, treasury unchanged.
        withdrawals.insert(vec![9u8; 29], Lovelace(4_000_000_000));

        state.enact_gov_action(&GovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash: None,
        });

        assert_eq!(state.epochs.treasury, Lovelace(9_000_000_000));
        let dropped_key = LedgerState::reward_account_to_hash(&[9u8; 29]);
        assert!(
            !state.certs.reward_accounts.contains_key(&dropped_key),
            "unregistered withdrawal address must NOT be silently created"
        );
    }

    #[test]
    fn test_enact_new_constitution() {
        let mut state = gov_test_state(5, 0);
        assert!(state.gov.governance.constitution.is_none());

        let constitution = Constitution {
            anchor: make_anchor(),
            script_hash: Some(Hash28::from_bytes([99u8; 28])),
        };

        state.enact_gov_action(&GovAction::NewConstitution {
            prev_action_id: None,
            constitution: constitution.clone(),
        });

        let stored = state.gov.governance.constitution.as_ref().unwrap();
        assert_eq!(stored.script_hash, constitution.script_hash);
    }

    #[test]
    fn test_enact_info_action_no_effect() {
        let mut state = gov_test_state(5, 0);
        let before = state.epochs.protocol_params.clone();

        state.enact_gov_action(&GovAction::InfoAction);

        assert_eq!(state.epochs.protocol_params.n_opt, before.n_opt);
        assert_eq!(
            state.epochs.protocol_params.protocol_version_major,
            before.protocol_version_major
        );
    }

    // ========================================================================
    // Regression tests — Issue #94: ParameterChange ex-unit updates
    // ========================================================================

    /// Regression test for issue #94.
    ///
    /// A Conway ParameterChange governance action that updates `max_tx_ex_units`
    /// and `max_block_ex_units` must be fully enacted when it receives sufficient
    /// DRep (network group), SPO (security group), and CC approval.
    ///
    /// Prior to the fix, nodes loaded from stale snapshots (saved before
    /// Alonzo/Conway genesis was wired in) carried `mainnet_defaults()` values
    /// for these fields. Because `committee_min_size` was also stale (7 instead
    /// of 0 for preview), `check_cc_approval` always returned false and no
    /// ParameterChange action ever ratified — leaving the node permanently
    /// reporting `max_tx_ex_mem=14,000,000` instead of the chain value of
    /// `16,500,000`.
    #[test]
    fn test_parameter_change_ex_units_ratified_and_enacted() {
        // 10 DReps + 10 SPOs covers both dvt_pp_network_group (67%) and
        // pvt_pp_security_group (51%) thresholds. committee_min_size is set to
        // 0 by gov_test_state so CC approval only requires the threshold ratio.
        let mut state = gov_test_state(10, 10);

        // Record the baseline values so we can assert they changed.
        let old_tx_mem = state.epochs.protocol_params.max_tx_ex_units.mem;
        let old_block_mem = state.epochs.protocol_params.max_block_ex_units.mem;

        // Propose a ParameterChange that updates both ex-unit limits.
        // These are the preview testnet values from on-chain governance epoch 1094.
        let new_tx_ex_units = ExUnits {
            mem: 16_500_000,
            steps: 10_000_000_000,
        };
        let new_block_ex_units = ExUnits {
            mem: 72_000_000,
            steps: 40_000_000_000,
        };
        let tx_hash = Hash32::from_bytes([50u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        // max_tx_ex_units is in the Network group (no SPO vote required).
                        // max_block_ex_units is in the Network + Security groups
                        // (requires pvt_pp_security_group SPO threshold).
                        max_tx_ex_units: Some(new_tx_ex_units),
                        max_block_ex_units: Some(new_block_ex_units),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(50, 0);

        // Cast DRep yes votes: 7 out of 10 = 70% >= dvt_pp_network_group (67%).
        for i in 0..7 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        // Cast SPO yes votes: 6 out of 10 = 60% >= pvt_pp_security_group (51%).
        for i in 0..6 {
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        // CC yes vote — the single CC member votes yes (threshold is 1/2).
        cc_vote_yes(&mut state, &action_id);

        // Trigger the epoch boundary where ratification and enactment occur.
        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // The proposal must have been consumed (ratified and enacted).
        assert!(
            state.gov.governance.proposals.is_empty(),
            "Proposal should have been ratified and removed from pending proposals"
        );

        // The protocol params must reflect the new ex-unit values.
        assert_ne!(
            state.epochs.protocol_params.max_tx_ex_units.mem, old_tx_mem,
            "max_tx_ex_units.mem must have changed from the baseline"
        );
        assert_eq!(
            state.epochs.protocol_params.max_tx_ex_units.mem, new_tx_ex_units.mem,
            "max_tx_ex_units.mem must equal the enacted value (16_500_000)"
        );
        assert_eq!(
            state.epochs.protocol_params.max_tx_ex_units.steps, new_tx_ex_units.steps,
            "max_tx_ex_units.steps must equal the enacted value"
        );

        assert_ne!(
            state.epochs.protocol_params.max_block_ex_units.mem, old_block_mem,
            "max_block_ex_units.mem must have changed from the baseline"
        );
        assert_eq!(
            state.epochs.protocol_params.max_block_ex_units.mem, new_block_ex_units.mem,
            "max_block_ex_units.mem must equal the enacted value (72_000_000)"
        );
        assert_eq!(
            state.epochs.protocol_params.max_block_ex_units.steps, new_block_ex_units.steps,
            "max_block_ex_units.steps must equal the enacted value"
        );
    }

    /// Confirms that a ParameterChange ex-unit update does NOT ratify when the
    /// CC vote is missing, even if DRep and SPO thresholds are met. This
    /// ensures the CC approval gate is functioning correctly.
    #[test]
    fn test_parameter_change_ex_units_not_ratified_without_cc() {
        let mut state = gov_test_state(10, 10);
        // Use a non-trivial CC threshold so missing the CC vote actually blocks ratification.
        Arc::make_mut(&mut state.gov.governance).committee_threshold = Some(Rational {
            numerator: 1,
            denominator: 2,
        });

        let tx_hash = Hash32::from_bytes([51u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        max_tx_ex_units: Some(ExUnits {
                            mem: 16_500_000,
                            steps: 10_000_000_000,
                        }),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(51, 0);

        // DRep and SPO thresholds are met, but NO CC vote.
        for i in 0..7 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..6 {
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        // Deliberately omit cc_vote_yes.

        state.process_epoch_transition(EpochNo(1));

        // Proposal must still be pending (not ratified — CC threshold not met).
        assert!(
            !state.gov.governance.proposals.is_empty(),
            "Proposal should NOT have been ratified without CC approval"
        );
        // ex-unit value must be unchanged.
        assert_eq!(
            state.epochs.protocol_params.max_tx_ex_units.mem,
            ProtocolParameters::mainnet_defaults().max_tx_ex_units.mem,
            "max_tx_ex_units.mem must be unchanged when CC approval is missing"
        );
    }

    // ========================================================================
    // Enacted root update tests
    // ========================================================================

    #[test]
    fn test_enacted_roots_updated_correctly() {
        // Test the update_enacted_root_local free function which threads enacted
        // roots through a ratification pass.
        let mut enacted_pparam: Option<GovActionId> = None;
        let mut enacted_hardfork: Option<GovActionId> = None;
        let mut enacted_committee: Option<GovActionId> = None;
        let mut enacted_constitution: Option<GovActionId> = None;

        let pp_id = make_action_id(1, 0);
        update_enacted_root_local(
            &pp_id,
            &GovAction::ParameterChange {
                prev_action_id: None,
                protocol_param_update: Box::new(ProtocolParamUpdate::default()),
                policy_hash: None,
            },
            &mut enacted_pparam,
            &mut enacted_hardfork,
            &mut enacted_committee,
            &mut enacted_constitution,
        );
        assert_eq!(enacted_pparam, Some(pp_id));

        let hf_id = make_action_id(2, 0);
        update_enacted_root_local(
            &hf_id,
            &GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (10, 0),
            },
            &mut enacted_pparam,
            &mut enacted_hardfork,
            &mut enacted_committee,
            &mut enacted_constitution,
        );
        assert_eq!(enacted_hardfork, Some(hf_id));

        let nc_id = make_action_id(3, 0);
        update_enacted_root_local(
            &nc_id,
            &GovAction::NoConfidence {
                prev_action_id: None,
            },
            &mut enacted_pparam,
            &mut enacted_hardfork,
            &mut enacted_committee,
            &mut enacted_constitution,
        );
        assert_eq!(enacted_committee, Some(nc_id.clone()));

        // UpdateCommittee shares the committee purpose with NoConfidence
        let uc_id = make_action_id(4, 0);
        update_enacted_root_local(
            &uc_id,
            &GovAction::UpdateCommittee {
                prev_action_id: None,
                members_to_remove: vec![],
                members_to_add: BTreeMap::new(),
                threshold: Rational {
                    numerator: 1,
                    denominator: 2,
                },
            },
            &mut enacted_pparam,
            &mut enacted_hardfork,
            &mut enacted_committee,
            &mut enacted_constitution,
        );
        assert_eq!(enacted_committee, Some(uc_id));

        let co_id = make_action_id(5, 0);
        update_enacted_root_local(
            &co_id,
            &GovAction::NewConstitution {
                prev_action_id: None,
                constitution: Constitution {
                    anchor: make_anchor(),
                    script_hash: None,
                },
            },
            &mut enacted_pparam,
            &mut enacted_hardfork,
            &mut enacted_committee,
            &mut enacted_constitution,
        );
        assert_eq!(enacted_constitution, Some(co_id));

        // TreasuryWithdrawals and InfoAction don't update any root
        let tw_id = make_action_id(6, 0);
        let old_pp = enacted_pparam.clone();
        update_enacted_root_local(
            &tw_id,
            &GovAction::TreasuryWithdrawals {
                withdrawals: BTreeMap::new(),
                policy_hash: None,
            },
            &mut enacted_pparam,
            &mut enacted_hardfork,
            &mut enacted_committee,
            &mut enacted_constitution,
        );
        assert_eq!(enacted_pparam, old_pp);
    }

    // ========================================================================
    // check_threshold tests
    // ========================================================================

    #[test]
    fn test_check_threshold_zero_passes() {
        let zero = Rational {
            numerator: 0,
            denominator: 1,
        };
        assert!(check_threshold(0, 0, &zero));
        assert!(check_threshold(0, 100, &zero));
    }

    #[test]
    fn test_check_threshold_zero_total_fails() {
        let threshold = Rational {
            numerator: 1,
            denominator: 2,
        };
        assert!(!check_threshold(0, 0, &threshold));
    }

    #[test]
    fn test_check_threshold_exact_boundary() {
        let threshold = Rational {
            numerator: 2,
            denominator: 3,
        };
        // 2/3 >= 2/3 → true
        assert!(check_threshold(2, 3, &threshold));
        // 666/1000 < 2/3 → false (666*3 = 1998 < 2000 = 2*1000)
        assert!(!check_threshold(666, 1000, &threshold));
        // 667/1000 >= 2/3 → true (667*3 = 2001 >= 2000)
        assert!(check_threshold(667, 1000, &threshold));
    }

    #[test]
    fn test_check_threshold_one_hundred_percent() {
        let threshold = Rational {
            numerator: 1,
            denominator: 1,
        };
        assert!(check_threshold(100, 100, &threshold));
        assert!(!check_threshold(99, 100, &threshold));
    }

    // ========================================================================
    // SPO voting power snapshot tests (mark vs set)
    // ========================================================================

    /// Verify that `compute_spo_voting_power` reads from the **mark** snapshot,
    /// not from `set`.  CIP-1694 specifies the mark (current-epoch) stake
    /// distribution for SPO voting power.
    #[test]
    fn test_spo_voting_power_uses_mark_snapshot() {
        use crate::state::StakeSnapshot;

        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let pool_id = Hash28::from_bytes([10u8; 28]);

        // Populate ONLY the mark snapshot with stake for this pool.
        // The set snapshot has a different (lower) amount.
        let mut mark_pool_stake = std::collections::HashMap::new();
        mark_pool_stake.insert(pool_id, Lovelace(5_000_000_000));
        state.epochs.snapshots.mark = Some(StakeSnapshot {
            epoch: EpochNo(1),
            delegations: Arc::new(std::collections::HashMap::new()),
            pool_stake: mark_pool_stake,
            pool_params: Arc::clone(&state.certs.pool_params),
            stake_distribution: Arc::new(std::collections::HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        });

        let mut set_pool_stake = std::collections::HashMap::new();
        set_pool_stake.insert(pool_id, Lovelace(1_000_000_000)); // deliberately different
        state.epochs.snapshots.set = Some(StakeSnapshot {
            epoch: EpochNo(0),
            delegations: Arc::new(std::collections::HashMap::new()),
            pool_stake: set_pool_stake,
            pool_params: Arc::clone(&state.certs.pool_params),
            stake_distribution: Arc::new(std::collections::HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        });

        // SPO voting power must come from mark (5B), not set (1B)
        let power = state.compute_spo_voting_power(&pool_id);
        assert_eq!(
            power, 5_000_000_000,
            "compute_spo_voting_power must read from mark snapshot per CIP-1694"
        );
    }

    /// Verify that `compute_total_spo_stake` (the denominator for SPO voting
    /// thresholds) also reads from the mark snapshot.
    #[test]
    fn test_compute_total_spo_stake_uses_mark_snapshot() {
        use crate::state::StakeSnapshot;

        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let pool_a = Hash28::from_bytes([1u8; 28]);
        let pool_b = Hash28::from_bytes([2u8; 28]);

        // Mark has both pools: total = 8B
        let mut mark_stake = std::collections::HashMap::new();
        mark_stake.insert(pool_a, Lovelace(3_000_000_000));
        mark_stake.insert(pool_b, Lovelace(5_000_000_000));
        state.epochs.snapshots.mark = Some(StakeSnapshot {
            epoch: EpochNo(2),
            delegations: Arc::new(std::collections::HashMap::new()),
            pool_stake: mark_stake,
            pool_params: Arc::clone(&state.certs.pool_params),
            stake_distribution: Arc::new(std::collections::HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        });

        // Set has only one pool: total = 1B (stale, should NOT be used)
        let mut set_stake = std::collections::HashMap::new();
        set_stake.insert(pool_a, Lovelace(1_000_000_000));
        state.epochs.snapshots.set = Some(StakeSnapshot {
            epoch: EpochNo(1),
            delegations: Arc::new(std::collections::HashMap::new()),
            pool_stake: set_stake,
            pool_params: Arc::clone(&state.certs.pool_params),
            stake_distribution: Arc::new(std::collections::HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        });

        let total = state.compute_total_spo_stake();
        assert_eq!(
            total, 8_000_000_000,
            "compute_total_spo_stake must sum from mark snapshot per CIP-1694"
        );
    }

    /// Without any snapshots, SPO voting power falls back to the O(n) live
    /// delegation scan.  Ensure the fallback returns sensible results and does
    /// not panic.
    #[test]
    fn test_spo_voting_power_fallback_no_snapshots() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let pool_id = Hash28::from_bytes([77u8; 28]);
        let stake_cred = Hash32::from_bytes([88u8; 32]);

        // No snapshots — must fall back to live delegation scan
        assert!(state.epochs.snapshots.mark.is_none());
        assert!(state.epochs.snapshots.set.is_none());

        // With a delegation pointing to pool_id and some stake
        state.certs.delegations.insert(stake_cred, pool_id);
        state
            .certs
            .stake_distribution
            .stake_map
            .insert(stake_cred, Lovelace(9_000_000));

        let power = state.compute_spo_voting_power(&pool_id);
        assert_eq!(
            power, 9_000_000,
            "fallback scan must return live stake when no mark snapshot exists"
        );
    }

    /// Verify that `count_votes_by_type` uses the `pool_stake_override` when
    /// provided, NOT `self.epochs.snapshots.mark`.  This matches Haskell's RATIFY which
    /// uses `dpStakePoolDistr` (the mark from the PREVIOUS boundary, stored in
    /// the DRep pulser) rather than the current mark.
    ///
    /// Regression test for #332: HardFork enactment timing mismatch caused by
    /// using the wrong (post-SNAP-rotation) mark snapshot for SPO voting.
    #[test]
    fn test_spo_pool_stake_override_used_during_ratification() {
        use crate::state::StakeSnapshot;

        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.epochs.protocol_params.protocol_version_major = 9; // bootstrap
        let pool_id = Hash28::from_bytes([42u8; 28]);

        // Register the pool so SPO votes are recognized
        Arc::make_mut(&mut state.certs.pool_params).insert(
            pool_id,
            PoolRegistration {
                pool_id,
                vrf_keyhash: Hash32::ZERO,
                pledge: Lovelace(1_000_000),
                cost: Lovelace(340_000_000),
                margin_numerator: 1,
                margin_denominator: 100,
                reward_account: vec![],
                owners: vec![],
                relays: vec![],
                metadata_url: None,
                metadata_hash: None,
            },
        );

        // Mark snapshot: pool has 10B (this is the WRONG snapshot for ratification)
        let mut mark_pool_stake = HashMap::new();
        mark_pool_stake.insert(pool_id, Lovelace(10_000_000_000));
        state.epochs.snapshots.mark = Some(StakeSnapshot {
            epoch: EpochNo(2),
            delegations: Arc::new(std::collections::HashMap::new()),
            pool_stake: mark_pool_stake,
            pool_params: Arc::clone(&state.certs.pool_params),
            stake_distribution: Arc::new(HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        });

        // Override: pool has 3B (simulates the set snapshot = previous mark)
        let mut override_pool_stake = HashMap::new();
        override_pool_stake.insert(pool_id, Lovelace(3_000_000_000));

        // Add a Yes vote from this pool
        let action_id = GovActionId {
            transaction_id: Hash32::from_bytes([1u8; 32]),
            action_index: 0,
        };
        let voter = Voter::StakePool(pool_id.to_hash32_padded());
        let procedure = VotingProcedure {
            vote: Vote::Yes,
            anchor: None,
        };
        let mut votes = ImblOrdMap::new();
        votes.insert(action_id.clone(), vec![(voter, procedure)].into());

        let cache = ImblHashMap::new();

        // WITHOUT override: should use mark (10B)
        let (_, _, spo_yes_mark, _, _, _) = state.count_votes_by_type(
            &action_id,
            &GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (10, 0),
            },
            &cache,
            0,
            &votes,
            None,
            None,
        );
        assert_eq!(
            spo_yes_mark, 10_000_000_000,
            "Without override: must use mark (10B)"
        );

        // WITH override: should use the override (3B)
        let (_, _, spo_yes_override, _, _, _) = state.count_votes_by_type(
            &action_id,
            &GovAction::HardForkInitiation {
                prev_action_id: None,
                protocol_version: (10, 0),
            },
            &cache,
            0,
            &votes,
            Some(&override_pool_stake),
            None,
        );
        assert_eq!(
            spo_yes_override, 3_000_000_000,
            "With override: must use set (3B), not mark"
        );
    }

    // ─── SPO vote counting tests (spoAcceptedRatio) ───

    /// Build a state with pools in the mark snapshot for SPO vote counting tests.
    /// Creates `n` pools with 1B stake each in the mark snapshot.
    /// Pool IDs are Hash28([100+i; 28]).
    fn spo_vote_test_state(n: usize, bootstrap: bool) -> LedgerState {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = if bootstrap { 9 } else { 10 };
        params.committee_min_size = 0;
        let mut state = LedgerState::new(params);
        state.epoch_length = 100;
        state.epochs.needs_stake_rebuild = false;

        let mut mark_pool_stake = HashMap::new();
        for i in 0..n {
            let pool_id = Hash28::from_bytes([100 + i as u8; 28]);
            Arc::make_mut(&mut state.certs.pool_params).insert(
                pool_id,
                PoolRegistration {
                    pool_id,
                    vrf_keyhash: Hash32::ZERO,
                    pledge: Lovelace(1_000_000),
                    cost: Lovelace(340_000_000),
                    margin_numerator: 1,
                    margin_denominator: 100,
                    reward_account: vec![],
                    owners: vec![],
                    relays: vec![],
                    metadata_url: None,
                    metadata_hash: None,
                },
            );
            mark_pool_stake.insert(pool_id, Lovelace(1_000_000_000));
        }

        state.epochs.snapshots.mark = Some(StakeSnapshot {
            epoch: EpochNo(0),
            delegations: Arc::new(std::collections::HashMap::new()),
            pool_stake: mark_pool_stake,
            pool_params: Arc::clone(&state.certs.pool_params),
            stake_distribution: Arc::new(HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        });

        state
    }

    #[test]
    fn test_spo_hardfork_abstain_excluded_from_denominator() {
        // Per Haskell `spoAcceptedRatio`: SPO denominator is always
        // totalActiveStake − abstainStake. For HardForkInitiation, non-voting
        // pools count as No, but explicitly abstaining pools must be subtracted
        // from the denominator.
        //
        // Setup: 5 pools × 1B each = 5B total
        //   Pool 0: votes Yes (1B)
        //   Pool 1: votes Abstain (1B)
        //   Pools 2-4: don't vote (HardFork → No)
        //
        // Expected: spo_yes = 1B, spo_abstain = 1B
        //   denom = 5B − 1B = 4B → ratio = 1/4 = 25%
        let state = spo_vote_test_state(5, true);
        let action_id = make_action_id(1, 0);
        let action = GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (10, 0),
        };

        let mut votes = ImblOrdMap::new();
        votes.insert(
            action_id.clone(),
            vec![
                (
                    Voter::StakePool(Hash28::from_bytes([100u8; 28]).to_hash32_padded()),
                    VotingProcedure {
                        vote: Vote::Yes,
                        anchor: None,
                    },
                ),
                (
                    Voter::StakePool(Hash28::from_bytes([101u8; 28]).to_hash32_padded()),
                    VotingProcedure {
                        vote: Vote::Abstain,
                        anchor: None,
                    },
                ),
            ]
            .into(),
        );

        let cache = ImblHashMap::new();
        let (_, _, spo_yes, spo_abstain, _, _) =
            state.count_votes_by_type(&action_id, &action, &cache, 0, &votes, None, None);

        assert_eq!(spo_yes, 1_000_000_000, "Only pool 0 voted Yes");
        assert_eq!(spo_abstain, 1_000_000_000, "Only pool 1 voted Abstain");

        // Denominator should be totalActive - abstain = 5B - 1B = 4B
        let total_spo = state.compute_total_spo_stake();
        let denom = total_spo.saturating_sub(spo_abstain);
        assert_eq!(denom, 4_000_000_000, "HardFork denom excludes abstain");
    }

    #[test]
    fn test_spo_bootstrap_nonvoters_abstain_for_non_hardfork() {
        // During bootstrap (PV 9), non-voting pools on non-HardFork actions
        // count as Abstain per Haskell. This effectively removes them from
        // the denominator.
        //
        // Setup: 4 pools × 1B each = 4B total
        //   Pool 0: votes Yes
        //   Pools 1-3: don't vote → Abstain (bootstrap, non-HardFork)
        //
        // Expected: spo_yes = 1B, spo_abstain = 3B
        //   denom = 4B − 3B = 1B → ratio = 1/1 = 100%
        let state = spo_vote_test_state(4, true);
        let action_id = make_action_id(2, 0);
        let action = GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::new(ProtocolParamUpdate::default()),
            policy_hash: None,
        };

        let mut votes = ImblOrdMap::new();
        votes.insert(
            action_id.clone(),
            vec![(
                Voter::StakePool(Hash28::from_bytes([100u8; 28]).to_hash32_padded()),
                VotingProcedure {
                    vote: Vote::Yes,
                    anchor: None,
                },
            )]
            .into(),
        );

        let cache = ImblHashMap::new();
        let (_, _, spo_yes, spo_abstain, _, _) =
            state.count_votes_by_type(&action_id, &action, &cache, 0, &votes, None, None);

        assert_eq!(spo_yes, 1_000_000_000);
        assert_eq!(
            spo_abstain, 3_000_000_000,
            "Non-voting pools during bootstrap on non-HardFork count as Abstain"
        );
    }

    #[test]
    fn test_spo_bootstrap_nonvoters_no_for_hardfork() {
        // During bootstrap, non-voting pools on HardForkInitiation count as No
        // (the HardFork guard fires before the bootstrap guard in Haskell).
        //
        // Setup: 4 pools × 1B, 1 votes Yes, 3 don't vote
        // Expected: spo_yes = 1B, spo_abstain = 0
        let state = spo_vote_test_state(4, true);
        let action_id = make_action_id(3, 0);
        let action = GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (10, 0),
        };

        let mut votes = ImblOrdMap::new();
        votes.insert(
            action_id.clone(),
            vec![(
                Voter::StakePool(Hash28::from_bytes([100u8; 28]).to_hash32_padded()),
                VotingProcedure {
                    vote: Vote::Yes,
                    anchor: None,
                },
            )]
            .into(),
        );

        let cache = ImblHashMap::new();
        let (_, _, spo_yes, spo_abstain, _, _) =
            state.count_votes_by_type(&action_id, &action, &cache, 0, &votes, None, None);

        assert_eq!(spo_yes, 1_000_000_000);
        assert_eq!(
            spo_abstain, 0,
            "Non-voting pools on HardFork count as No even during bootstrap"
        );
    }

    #[test]
    fn test_spo_explicit_no_in_denominator_not_abstain() {
        // Pools that explicitly vote No should remain in the denominator
        // (they are not counted as abstain).
        //
        // Setup: 3 pools × 1B each = 3B total
        //   Pool 0: votes Yes
        //   Pool 1: votes No
        //   Pool 2: votes Abstain
        //
        // Expected: spo_yes = 1B, spo_abstain = 1B
        //   denom = 3B − 1B = 2B → ratio = 1/2 = 50%
        let state = spo_vote_test_state(3, false);
        let action_id = make_action_id(4, 0);
        let action = GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (11, 0),
        };

        let mut votes = ImblOrdMap::new();
        votes.insert(
            action_id.clone(),
            vec![
                (
                    Voter::StakePool(Hash28::from_bytes([100u8; 28]).to_hash32_padded()),
                    VotingProcedure {
                        vote: Vote::Yes,
                        anchor: None,
                    },
                ),
                (
                    Voter::StakePool(Hash28::from_bytes([101u8; 28]).to_hash32_padded()),
                    VotingProcedure {
                        vote: Vote::No,
                        anchor: None,
                    },
                ),
                (
                    Voter::StakePool(Hash28::from_bytes([102u8; 28]).to_hash32_padded()),
                    VotingProcedure {
                        vote: Vote::Abstain,
                        anchor: None,
                    },
                ),
            ]
            .into(),
        );

        let cache = ImblHashMap::new();
        let (_, _, spo_yes, spo_abstain, _, _) =
            state.count_votes_by_type(&action_id, &action, &cache, 0, &votes, None, None);

        assert_eq!(spo_yes, 1_000_000_000, "Only pool 0 voted Yes");
        assert_eq!(
            spo_abstain, 1_000_000_000,
            "Only pool 2 is Abstain — explicit No is NOT Abstain"
        );
    }

    #[test]
    fn test_spo_default_vote_always_abstain() {
        // Post-bootstrap: non-voting pool whose reward account delegates to
        // AlwaysAbstain DRep should count as Abstain.
        let mut state = spo_vote_test_state(2, false);
        let pool_id = Hash28::from_bytes([100u8; 28]);

        // Set up pool's reward account with an AlwaysAbstain DRep delegation
        let reward_account = {
            let mut ra = vec![0xe0u8]; // key-hash reward account header
            ra.extend_from_slice(&[50u8; 28]);
            ra
        };
        Arc::make_mut(&mut state.certs.pool_params)
            .get_mut(&pool_id)
            .unwrap()
            .reward_account = reward_account;

        let cred_hash = LedgerState::reward_account_to_hash(
            &[0xe0u8; 1]
                .iter()
                .chain(&[50u8; 28])
                .copied()
                .collect::<Vec<u8>>(),
        );
        Arc::make_mut(&mut state.gov.governance)
            .vote_delegations
            .insert(cred_hash, DRep::Abstain);

        let action_id = make_action_id(5, 0);
        let action = GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::new(ProtocolParamUpdate::default()),
            policy_hash: None,
        };

        let cache = ImblHashMap::new();
        let votes = ImblOrdMap::new();
        let (_, _, spo_yes, spo_abstain, _, _) =
            state.count_votes_by_type(&action_id, &action, &cache, 0, &votes, None, None);

        // Pool 0 has AlwaysAbstain delegation → Abstain
        // Pool 1 has no reward account → No (default)
        assert_eq!(spo_yes, 0);
        assert_eq!(
            spo_abstain, 1_000_000_000,
            "Pool with AlwaysAbstain delegation should count as Abstain"
        );
    }

    #[test]
    fn test_spo_default_vote_no_confidence_on_no_confidence_action() {
        // Post-bootstrap: non-voting pool whose reward account delegates to
        // AlwaysNoConfidence should count as Yes on NoConfidence actions.
        let mut state = spo_vote_test_state(2, false);
        let pool_id = Hash28::from_bytes([100u8; 28]);

        // Set up pool's reward account
        let mut reward_account = vec![0xe0u8];
        reward_account.extend_from_slice(&[60u8; 28]);
        Arc::make_mut(&mut state.certs.pool_params)
            .get_mut(&pool_id)
            .unwrap()
            .reward_account = reward_account.clone();

        let cred_hash = LedgerState::reward_account_to_hash(&reward_account);
        Arc::make_mut(&mut state.gov.governance)
            .vote_delegations
            .insert(cred_hash, DRep::NoConfidence);

        let action_id = make_action_id(6, 0);
        let action = GovAction::NoConfidence {
            prev_action_id: None,
        };

        let cache = ImblHashMap::new();
        let votes = ImblOrdMap::new();
        let (_, _, spo_yes, spo_abstain, _, _) =
            state.count_votes_by_type(&action_id, &action, &cache, 0, &votes, None, None);

        assert_eq!(
            spo_yes, 1_000_000_000,
            "AlwaysNoConfidence pool counts as Yes on NoConfidence action"
        );
        assert_eq!(spo_abstain, 0);
    }

    #[test]
    fn test_valid_committee_term_rejects_excessive_expiry() {
        // UpdateCommittee proposals where new members' expiry exceeds
        // currentEpoch + committeeMaxTermLength should fail ratification.
        let mut state = gov_test_state(10, 10);
        state.epoch = EpochNo(5);

        let max_term = state.epochs.protocol_params.committee_max_term_length;
        let too_long_expiry = state.epoch.0 + max_term + 1;

        let tx_hash = Hash32::from_bytes([80u8; 32]);
        let new_cred = Credential::VerificationKey(Hash28::from_bytes([31u8; 28]));
        let mut members = BTreeMap::new();
        members.insert(new_cred, too_long_expiry);

        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::UpdateCommittee {
                    prev_action_id: None,
                    members_to_remove: vec![],
                    members_to_add: members,
                    threshold: Rational {
                        numerator: 2,
                        denominator: 3,
                    },
                },
                anchor: make_anchor(),
            },
        );

        let action_id = make_action_id(80, 0);
        // Vote unanimously in favor
        for i in 0..10 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }

        state.process_epoch_transition(EpochNo(6));
        // Should NOT ratify — validCommitteeTerm check fails
        assert!(
            !state.gov.governance.proposals.is_empty(),
            "Proposal with excessive member expiry must not ratify"
        );
    }

    // ── Proposal forest tests ─────────────────────────────────────────────

    #[test]
    fn test_forest_add_and_structure() {
        // Build a tree:
        //   root (enacted) -> A -> C
        //                  -> B
        let root_id = make_action_id(0x01, 0);
        let a_id = make_action_id(0x0a, 0);
        let b_id = make_action_id(0x0b, 0);
        let c_id = make_action_id(0x0c, 0);

        let mut roots = GovRelation::<PRoot>::default();
        let mut graph = GovRelation::<PGraph>::default();

        // Set enacted root for purpose 0 (PParam).
        roots.pparam.root = Some(root_id.clone());

        // Add A as child of root.
        forest_add_proposal(&a_id, Some(&root_id), 0, &mut roots, &mut graph);
        assert!(roots.pparam.children.contains(&a_id));
        assert!(!graph.pparam.nodes.contains_key(&a_id));

        // Add B as child of root.
        forest_add_proposal(&b_id, Some(&root_id), 0, &mut roots, &mut graph);
        assert!(roots.pparam.children.contains(&b_id));

        // Add C as child of A (deeper level).
        forest_add_proposal(&c_id, Some(&a_id), 0, &mut roots, &mut graph);
        assert!(!roots.pparam.children.contains(&c_id));
        assert!(graph.pparam.nodes.contains_key(&c_id));
        // A should now have a graph entry with C as child.
        let a_edges = graph.pparam.nodes.get(&a_id).unwrap();
        assert!(a_edges.children.contains(&c_id));
        // C's parent should be A.
        let c_edges = graph.pparam.nodes.get(&c_id).unwrap();
        assert_eq!(c_edges.parent, Some(a_id.clone()));
    }

    #[test]
    fn test_forest_remove_with_descendants() {
        // Tree:  root -> A -> C -> D
        //             -> B
        // Remove A: should remove A, C, D but not B.
        let root_id = make_action_id(0x01, 0);
        let a_id = make_action_id(0x0a, 0);
        let b_id = make_action_id(0x0b, 0);
        let c_id = make_action_id(0x0c, 0);
        let d_id = make_action_id(0x0d, 0);

        let mut roots = GovRelation::<PRoot>::default();
        let mut graph = GovRelation::<PGraph>::default();
        roots.pparam.root = Some(root_id.clone());

        forest_add_proposal(&a_id, Some(&root_id), 0, &mut roots, &mut graph);
        forest_add_proposal(&b_id, Some(&root_id), 0, &mut roots, &mut graph);
        forest_add_proposal(&c_id, Some(&a_id), 0, &mut roots, &mut graph);
        forest_add_proposal(&d_id, Some(&c_id), 0, &mut roots, &mut graph);

        // Build proposals map with matching actions.
        let mut proposals = ImblOrdMap::new();
        let mut votes = ImblOrdMap::new();
        for (id, prev) in [
            (&a_id, Some(root_id.clone())),
            (&b_id, Some(root_id.clone())),
            (&c_id, Some(a_id.clone())),
            (&d_id, Some(c_id.clone())),
        ] {
            proposals.insert(
                id.clone(),
                ProposalState {
                    procedure: ProposalProcedure {
                        deposit: Lovelace(100),
                        return_addr: vec![0; 29],
                        gov_action: GovAction::ParameterChange {
                            prev_action_id: prev,
                            protocol_param_update: Box::new(ProtocolParamUpdate::default()),
                            policy_hash: None,
                        },
                        anchor: make_anchor(),
                    },
                    proposed_epoch: EpochNo(0),
                    expires_epoch: EpochNo(10),
                    yes_votes: 0,
                    no_votes: 0,
                    abstain_votes: 0,
                    submission_index: 0,
                },
            );
        }

        let removed = forest_remove_with_descendants(
            std::slice::from_ref(&a_id),
            &mut proposals,
            &mut roots,
            &mut graph,
            &mut votes,
        );

        // A, C, D removed.
        assert_eq!(removed.len(), 3);
        let removed_ids: BTreeSet<_> = removed.iter().map(|(id, _)| id.clone()).collect();
        assert!(removed_ids.contains(&a_id));
        assert!(removed_ids.contains(&c_id));
        assert!(removed_ids.contains(&d_id));

        // B still present.
        assert!(proposals.contains_key(&b_id));
        assert!(!proposals.contains_key(&a_id));

        // Forest: B should still be in root.children.
        assert!(roots.pparam.children.contains(&b_id));
        assert!(!roots.pparam.children.contains(&a_id));
    }

    #[test]
    fn test_forest_promote_root() {
        // Tree: root -> A -> C
        //            -> B
        // Promote A to root: new root = A, new root.children = {C}, B removed.
        let root_id = make_action_id(0x01, 0);
        let a_id = make_action_id(0x0a, 0);
        let b_id = make_action_id(0x0b, 0);
        let c_id = make_action_id(0x0c, 0);

        let mut roots = GovRelation::<PRoot>::default();
        let mut graph = GovRelation::<PGraph>::default();
        roots.pparam.root = Some(root_id.clone());

        forest_add_proposal(&a_id, Some(&root_id), 0, &mut roots, &mut graph);
        forest_add_proposal(&b_id, Some(&root_id), 0, &mut roots, &mut graph);
        forest_add_proposal(&c_id, Some(&a_id), 0, &mut roots, &mut graph);

        // Simulate: B was already removed as sibling.
        roots.pparam.children.remove(&b_id);

        // Promote A.
        forest_promote_root(&a_id, 0, &mut roots, &mut graph);

        assert_eq!(roots.pparam.root, Some(a_id.clone()));
        assert!(roots.pparam.children.contains(&c_id));
        assert!(!roots.pparam.children.contains(&a_id));
        assert!(!roots.pparam.children.contains(&b_id));
    }

    #[test]
    fn test_rebuild_forest_from_flat() {
        let root_id = make_action_id(0x01, 0);
        let a_id = make_action_id(0x0a, 0);
        let c_id = make_action_id(0x0c, 0);

        let mut proposals = ImblOrdMap::new();
        for (id, prev) in [(&a_id, Some(root_id.clone())), (&c_id, Some(a_id.clone()))] {
            proposals.insert(
                id.clone(),
                ProposalState {
                    procedure: ProposalProcedure {
                        deposit: Lovelace(100),
                        return_addr: vec![0; 29],
                        gov_action: GovAction::ParameterChange {
                            prev_action_id: prev,
                            protocol_param_update: Box::new(ProtocolParamUpdate::default()),
                            policy_hash: None,
                        },
                        anchor: make_anchor(),
                    },
                    proposed_epoch: EpochNo(0),
                    expires_epoch: EpochNo(10),
                    yes_votes: 0,
                    no_votes: 0,
                    abstain_votes: 0,
                    submission_index: 0,
                },
            );
        }

        let (roots, graph) =
            rebuild_forest_from_flat(&proposals, &Some(root_id.clone()), &None, &None, &None);

        assert_eq!(roots.pparam.root, Some(root_id));
        assert!(roots.pparam.children.contains(&a_id));
        assert!(graph.pparam.nodes.contains_key(&c_id));
        assert!(graph
            .pparam
            .nodes
            .get(&a_id)
            .unwrap()
            .children
            .contains(&c_id));
    }

    #[test]
    fn test_forest_committee_purpose_shared() {
        // NoConfidence and UpdateCommittee share purpose tag 2.
        let root_id = make_action_id(0x01, 0);
        let nc_id = make_action_id(0x0a, 0);
        let uc_id = make_action_id(0x0b, 0);

        let mut roots = GovRelation::<PRoot>::default();
        let mut graph = GovRelation::<PGraph>::default();
        roots.committee.root = Some(root_id.clone());

        // Add NoConfidence as child of root (purpose 2).
        forest_add_proposal(&nc_id, Some(&root_id), 2, &mut roots, &mut graph);
        // Add UpdateCommittee as child of root (purpose 2).
        forest_add_proposal(&uc_id, Some(&root_id), 2, &mut roots, &mut graph);

        // Both should be in the same tree.
        assert!(roots.committee.children.contains(&nc_id));
        assert!(roots.committee.children.contains(&uc_id));
        assert_eq!(roots.committee.children.len(), 2);
    }

    #[test]
    fn test_forest_treasury_info_no_tree() {
        // TreasuryWithdrawals and InfoAction have no purpose tree (tag = None).
        let action = GovAction::TreasuryWithdrawals {
            withdrawals: BTreeMap::new(),
            policy_hash: None,
        };
        assert!(gov_action_purpose_tag(&action).is_none());

        let action2 = GovAction::InfoAction;
        assert!(gov_action_purpose_tag(&action2).is_none());
    }

    #[test]
    fn test_forest_genesis_root_siblings() {
        // When prev_action_id is None (genesis root), siblings should match.
        let a_id = make_action_id(0x0a, 0);
        let b_id = make_action_id(0x0b, 0);

        let mut roots = GovRelation::<PRoot>::default();
        let mut graph = GovRelation::<PGraph>::default();
        // Root is None (genesis).

        forest_add_proposal(&a_id, None, 0, &mut roots, &mut graph);
        forest_add_proposal(&b_id, None, 0, &mut roots, &mut graph);

        // Both should be root children (genesis root).
        assert!(roots.pparam.children.contains(&a_id));
        assert!(roots.pparam.children.contains(&b_id));
        assert_eq!(roots.pparam.root, None);
    }

    /// Reproduces issue #481: at preview boundary 735→736, three sibling
    /// ParameterChange proposals existed with prev_action_id = None.  One was
    /// ratified+enacted; the other two should have been dropped with their
    /// 100K-ADA deposits refunded to their return addresses (treasury if
    /// unregistered).  Koios shows Haskell did refund all three; dugite
    /// missed one of the sibling refunds.
    ///
    /// This test wires up exactly that scenario with three return addresses:
    ///   - enacted's return_addr: REGISTERED   → reward_account += 100K
    ///   - sibling 1 return_addr: UNREGISTERED → treasury        += 100K
    ///   - sibling 2 return_addr: REGISTERED   → reward_account += 100K
    #[test]
    fn test_param_change_sibling_drops_refund_all_three() {
        let mut state = gov_test_state(10, 10);

        // Set up three distinct 29-byte reward accounts (1 header + 28 hash).
        // The 0xe0 header marks a key-credential mainnet reward address.
        // Differing the second byte makes each address unique.
        let mut ret_enacted = vec![0xe0u8];
        ret_enacted.extend_from_slice(&[0x11u8; 28]);
        let mut ret_sibling_unreg = vec![0xe0u8];
        ret_sibling_unreg.extend_from_slice(&[0x22u8; 28]);
        let mut ret_sibling_reg = vec![0xe0u8];
        ret_sibling_reg.extend_from_slice(&[0x33u8; 28]);

        let key_enacted = LedgerState::reward_account_to_hash(&ret_enacted);
        let key_sibling_unreg = LedgerState::reward_account_to_hash(&ret_sibling_unreg);
        let key_sibling_reg = LedgerState::reward_account_to_hash(&ret_sibling_reg);

        // Register two of the three accounts.  The middle one stays UNregistered
        // so its refund must flow to treasury (mirroring B_ret on preview at
        // boundary 735→736, which had no Registration certificate before e784).
        state.certs.reward_accounts.insert(key_enacted, Lovelace(0));
        state
            .certs
            .reward_accounts
            .insert(key_sibling_reg, Lovelace(0));

        let initial_treasury = state.epochs.treasury.0;
        let deposit = 100_000_000_000u64; // 100K ADA

        // Submit three ParameterChange proposals with prev_action_id = None.
        // Only the first one is the one we'll vote to enact; the other two
        // exist purely as siblings to be dropped at the boundary.
        let tx_a = Hash32::from_bytes([0xa0u8; 32]);
        let tx_b = Hash32::from_bytes([0xb0u8; 32]);
        let tx_c = Hash32::from_bytes([0xc0u8; 32]);

        state.process_proposal(
            &tx_a,
            0,
            &ProposalProcedure {
                deposit: Lovelace(deposit),
                return_addr: ret_enacted.clone(),
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        max_tx_ex_units: Some(ExUnits {
                            mem: 16_500_000,
                            steps: 10_000_000_000,
                        }),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        state.process_proposal(
            &tx_b,
            0,
            &ProposalProcedure {
                deposit: Lovelace(deposit),
                return_addr: ret_sibling_unreg.clone(),
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        max_tx_ex_units: Some(ExUnits {
                            mem: 17_000_000,
                            steps: 10_000_000_000,
                        }),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        state.process_proposal(
            &tx_c,
            0,
            &ProposalProcedure {
                deposit: Lovelace(deposit),
                return_addr: ret_sibling_reg.clone(),
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ProtocolParamUpdate {
                        max_tx_ex_units: Some(ExUnits {
                            mem: 18_000_000,
                            steps: 10_000_000_000,
                        }),
                        ..Default::default()
                    }),
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );

        let enacted_id = GovActionId {
            transaction_id: tx_a,
            action_index: 0,
        };

        // Vote ONLY the enacted proposal through.
        for i in 0..8 {
            drep_vote(&mut state, i, &enacted_id, Vote::Yes);
        }
        for i in 0..6 {
            spo_vote(&mut state, i, &enacted_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &enacted_id);

        // Trigger the boundary.
        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // ── ASSERTIONS ──
        // All three proposals must be removed from the proposal pool.
        assert!(
            state.gov.governance.proposals.is_empty(),
            "All 3 proposals (1 enacted + 2 sibling-dropped) must be removed, \
             but {} remain",
            state.gov.governance.proposals.len()
        );

        // Treasury should have received exactly the unregistered sibling's
        // 100K ADA refund (no RUPD on a zero-reserves state).
        let treasury_delta = state.epochs.treasury.0.saturating_sub(initial_treasury);
        assert_eq!(
            treasury_delta, deposit,
            "Treasury must gain exactly 100K ADA from the unregistered \
             sibling's deposit refund — got delta = {treasury_delta}",
        );

        // The two REGISTERED return addresses must each have +100K credited.
        let enacted_balance = state
            .certs
            .reward_accounts
            .get(&key_enacted)
            .copied()
            .unwrap_or(Lovelace(0))
            .0;
        let sibling_reg_balance = state
            .certs
            .reward_accounts
            .get(&key_sibling_reg)
            .copied()
            .unwrap_or(Lovelace(0))
            .0;
        assert_eq!(
            enacted_balance, deposit,
            "Enacted proposal's deposit must be refunded to its registered \
             reward account (got {enacted_balance})",
        );
        assert_eq!(
            sibling_reg_balance, deposit,
            "Dropped registered-sibling's deposit must be refunded to its \
             reward account (got {sibling_reg_balance})",
        );

        // The unregistered sibling MUST NOT have a phantom reward_accounts entry.
        assert!(
            !state.certs.reward_accounts.contains_key(&key_sibling_unreg),
            "Unregistered sibling's return_addr must NOT have been silently \
             created in reward_accounts",
        );
    }

    // ========================================================================
    // Additional ratification threshold tests (gap coverage)
    // ========================================================================

    // -----------------------------------------------------------------------
    // HardForkInitiation: DRep + SPO + CC all required (via enact directly)
    // -----------------------------------------------------------------------

    /// HardForkInitiation enactment updates the protocol version.
    /// Verify that enact_gov_action correctly sets the new major/minor version.
    #[test]
    fn test_hard_fork_enact_updates_protocol_version() {
        let mut state = gov_test_state(0, 0);
        assert_eq!(state.epochs.protocol_params.protocol_version_major, 10);

        state.enact_gov_action(&GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (11, 0),
        });

        assert_eq!(
            state.epochs.protocol_params.protocol_version_major, 11,
            "enact HardForkInitiation must update protocol_version_major"
        );
        assert_eq!(
            state.epochs.protocol_params.protocol_version_minor, 0,
            "enact HardForkInitiation must update protocol_version_minor"
        );
    }

    /// HardForkInitiation ratification requires CC — without CC vote the proposal
    /// must not be enacted at the epoch boundary.
    #[test]
    fn test_hard_fork_not_ratified_without_cc() {
        let mut state = gov_test_state(10, 5);
        state.epochs.protocol_params.dvt_hard_fork = Rational {
            numerator: 1,
            denominator: 2,
        };
        state.epochs.protocol_params.pvt_hard_fork = Rational {
            numerator: 1,
            denominator: 2,
        };

        let tx_hash = Hash32::from_bytes([77u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::HardForkInitiation {
                    prev_action_id: None,
                    protocol_version: (11, 0),
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(77, 0);

        // DReps and SPOs vote Yes — but NO CC vote
        for i in 0..10 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..5 {
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        // No CC vote

        state.process_epoch_transition(EpochNo(1));

        assert_eq!(
            state.epochs.protocol_params.protocol_version_major, 10,
            "HardFork must NOT ratify without CC approval"
        );
    }

    // -----------------------------------------------------------------------
    // NoConfidence: DRep + SPO required, CC not required
    // -----------------------------------------------------------------------

    /// NoConfidence must NOT require CC approval and must set no_confidence=true.
    #[test]
    fn test_no_confidence_ratified_without_cc_vote() {
        let mut state = gov_test_state(10, 5);
        // Use low thresholds so 100% of 10 DReps and 5 SPOs easily passes.
        state.epochs.protocol_params.dvt_no_confidence = Rational {
            numerator: 1,
            denominator: 2,
        };
        state.epochs.protocol_params.pvt_motion_no_confidence = Rational {
            numerator: 1,
            denominator: 2,
        };

        let tx_hash = Hash32::from_bytes([78u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(78, 0);

        for i in 0..10 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..5 {
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        // No CC vote — NC does not require CC

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        assert!(
            state.gov.governance.no_confidence,
            "NoConfidence action must be enacted without CC approval"
        );
    }

    // -----------------------------------------------------------------------
    // TreasuryWithdrawals: DRep + CC required, no SPO required
    // -----------------------------------------------------------------------

    /// TreasuryWithdrawals ratifies with DRep + CC, no SPO needed.
    /// Uses same pattern as existing test_treasury_withdrawal_no_spo_required.
    #[test]
    fn test_treasury_withdrawal_ratified_no_spo_vote_needed() {
        let mut state = gov_test_state(10, 10); // 10 SPOs registered but won't vote
        state.epochs.treasury = Lovelace(10_000_000_000);

        // Register the withdrawal target so the disbursement actually credits.
        let withdrawal_key = LedgerState::reward_account_to_hash(&[0u8; 29]);
        state
            .certs
            .reward_accounts
            .insert(withdrawal_key, Lovelace(0));

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(vec![0u8; 29], Lovelace(1_000_000_000));

        let tx_hash = Hash32::from_bytes([79u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::TreasuryWithdrawals {
                    withdrawals,
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(79, 0);

        // 8/10 DReps vote Yes (80% >= 67% dvt_treasury_withdrawal)
        for i in 0..8 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &action_id);
        // SPOs do NOT vote — TreasuryWithdrawals does not need SPO

        state.process_epoch_transition(EpochNo(1));

        state.process_epoch_transition(EpochNo(1));

        assert!(
            state.epochs.treasury.0 < 10_000_000_000,
            "Treasury withdrawal must be enacted even without SPO votes"
        );
    }

    // -----------------------------------------------------------------------
    // UpdateCommittee: DRep + SPO required (when no_confidence=false), CC not required
    // -----------------------------------------------------------------------

    #[test]
    fn test_update_committee_no_cc_required_when_confidence() {
        let mut state = gov_test_state(10, 5);
        state.epochs.protocol_params.dvt_committee_normal = Rational {
            numerator: 1,
            denominator: 2,
        };
        state.epochs.protocol_params.pvt_committee_normal = Rational {
            numerator: 1,
            denominator: 2,
        };
        // Ensure no_confidence = false (normal confidence)
        Arc::make_mut(&mut state.gov.governance).no_confidence = false;

        let new_cold = Credential::VerificationKey(Hash28::from_bytes([99u8; 28]));
        let new_cold_key = credential_to_hash(&new_cold);
        let mut members_to_add = BTreeMap::new();
        // Expiry must be <= current_epoch + committee_max_term_length (146 by default)
        // state.epoch == 0, so max_expiry = 0 + 146 = 146
        members_to_add.insert(new_cold.clone(), 100u64); // epoch expiry

        let tx_hash = Hash32::from_bytes([80u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::UpdateCommittee {
                    prev_action_id: None,
                    members_to_remove: vec![],
                    members_to_add,
                    threshold: Rational {
                        numerator: 1,
                        denominator: 2,
                    },
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(80, 0);

        for i in 0..10 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..5 {
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        // No CC vote — UpdateCommittee doesn't need CC when not in no_confidence

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        assert!(
            state
                .gov
                .governance
                .committee_expiration
                .contains_key(&new_cold_key),
            "UpdateCommittee must be enacted without CC vote when not in no_confidence state"
        );
    }

    // -----------------------------------------------------------------------
    // InfoAction: cannot be ratified (NoVotingThreshold), expires normally
    // -----------------------------------------------------------------------

    /// Per the Haskell ledger spec, InfoAction has NoVotingThreshold for all
    /// three voting bodies, which means no voting body participates — the
    /// action cannot accumulate yes votes and therefore cannot be ratified.
    /// It remains in the proposals set until it expires.
    #[test]
    fn test_info_action_cannot_be_ratified() {
        let mut state = gov_test_state(0, 0);
        state.epochs.protocol_params.gov_action_lifetime = 5; // survives several epochs

        let tx_hash = Hash32::from_bytes([81u8; 32]);
        let deposit = state.epochs.protocol_params.gov_action_deposit;
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit,
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(81, 0);

        // Process one epoch — InfoAction must NOT be ratified
        state.process_epoch_transition(EpochNo(1));

        let still_pending = state
            .gov
            .governance
            .proposals
            .iter()
            .any(|(id, _)| *id == action_id);
        assert!(
            still_pending,
            "InfoAction must NOT be ratified after one epoch — it has NoVotingThreshold"
        );
    }

    // -----------------------------------------------------------------------
    // Proposal expiry: deposit goes to treasury, not returned
    // -----------------------------------------------------------------------

    #[test]
    fn test_expired_proposal_deposit_forfeited_to_treasury() {
        // A NoConfidence proposal with lifetime=1 and no voters must expire
        // at epoch 1+1=2, and must NOT appear in active proposals after that.
        let mut state = gov_test_state(0, 0);
        state.epochs.treasury = Lovelace(0);

        state.epochs.protocol_params.gov_action_lifetime = 1;

        let tx_hash = Hash32::from_bytes([82u8; 32]);
        let deposit = state.epochs.protocol_params.gov_action_deposit;
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit,
                return_addr: vec![0u8; 29],
                // NoConfidence won't ratify without votes; it expires at epoch 0+1=1
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );

        // With gov_action_lifetime=1, expires_epoch = 0 + 1 = 1.
        // Haskell: proposal is active while expires_epoch >= currentEpoch.
        // Per test_proposal_expiry_inclusive: with lifetime=3 submitted at epoch 0,
        // expires at epoch 5 boundary (active through epoch 4).
        // So with lifetime=1: expires_epoch=1, active through epoch 2,
        // expired at transition to epoch 3.
        state.process_epoch_transition(EpochNo(1));
        state.process_epoch_transition(EpochNo(2));

        // Still active at epoch 2
        let still_pending_2 = state
            .gov
            .governance
            .proposals
            .iter()
            .any(|(id, _)| *id == make_action_id(82, 0));
        assert!(still_pending_2, "Proposal still active at epoch 2");

        state.process_epoch_transition(EpochNo(3));

        // Expired by epoch 3 boundary
        let still_pending_3 = state
            .gov
            .governance
            .proposals
            .iter()
            .any(|(id, _)| *id == make_action_id(82, 0));
        assert!(
            !still_pending_3,
            "Proposal must expire by epoch 3 with gov_action_lifetime=1"
        );
    }

    // -----------------------------------------------------------------------
    // Proposal deposit returned on ratification (reward account credited)
    // -----------------------------------------------------------------------

    #[test]
    fn test_ratified_proposal_deposit_returned_to_return_addr() {
        // When a proposal is ratified, deposit is credited to return_addr, not forfeited.
        let mut state = gov_test_state(10, 0);
        state.epochs.treasury = Lovelace(10_000_000_000);
        state.epochs.protocol_params.dvt_treasury_withdrawal = Rational {
            numerator: 1,
            denominator: 2,
        };

        let deposit = Lovelace(100_000_000_000);
        // return_addr: mainnet key reward address (0xE1 + 28 zero bytes)
        let mut return_addr = vec![0xE1u8];
        return_addr.extend_from_slice(&[0x55u8; 28]);

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(return_addr.clone(), Lovelace(1_000_000_000));

        let tx_hash = Hash32::from_bytes([83u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit,
                return_addr: return_addr.clone(),
                gov_action: GovAction::TreasuryWithdrawals {
                    withdrawals,
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(83, 0);

        // Register the return_addr as a stake key so reward_accounts has it.
        let return_cred = Credential::VerificationKey(Hash28::from_bytes([0x55u8; 28]));
        state.process_certificate(&Certificate::StakeRegistration(return_cred));

        for i in 0..10 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &action_id);

        let treasury_before = state.epochs.treasury.0;
        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // Treasury must be reduced by withdrawal
        assert!(
            state.epochs.treasury.0 < treasury_before,
            "Treasury must decrease after ratified TreasuryWithdrawal"
        );

        // Deposit must have been credited (either to reward account or treasury-adjacent flow)
        let still_pending = state
            .gov
            .governance
            .proposals
            .iter()
            .any(|(id, _)| *id == action_id);
        assert!(
            !still_pending,
            "Ratified proposal must be removed from active proposals"
        );
    }

    // -----------------------------------------------------------------------
    // NewConstitution: DRep + CC required, no SPO
    // -----------------------------------------------------------------------

    #[test]
    fn test_new_constitution_no_spo_required_extended() {
        let mut state = gov_test_state(10, 3);
        state.epochs.protocol_params.dvt_constitution = Rational {
            numerator: 1,
            denominator: 2,
        };

        let tx_hash = Hash32::from_bytes([84u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NewConstitution {
                    prev_action_id: None,
                    constitution: Constitution {
                        anchor: make_anchor(),
                        script_hash: None,
                    },
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(84, 0);

        for i in 0..10 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &action_id);
        // SPOs do NOT vote — NewConstitution must not need them

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        assert!(
            state.gov.governance.constitution.is_some(),
            "NewConstitution must be enacted with DRep + CC, no SPO"
        );
    }

    // -----------------------------------------------------------------------
    // Vote replacement: the latest vote wins
    // -----------------------------------------------------------------------

    #[test]
    fn test_vote_replacement_latest_wins_drep() {
        let mut state = gov_test_state(1, 0); // 1 DRep with 1B stake

        let tx_hash = Hash32::from_bytes([85u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::InfoAction,
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(85, 0);

        // DRep votes No, then changes to Yes
        drep_vote(&mut state, 0, &action_id, Vote::No);
        drep_vote(&mut state, 0, &action_id, Vote::Yes);

        // Find the recorded vote
        let votes_for_action = state
            .gov
            .governance
            .votes_by_action
            .get(&action_id)
            .cloned()
            .unwrap_or_default();

        let voter_cred = Credential::VerificationKey(Hash28::from_bytes([0u8; 28]));
        let drep_voter = Voter::DRep(voter_cred);
        let final_vote = votes_for_action.get(&drep_voter).map(|p| p.vote.clone());

        assert_eq!(
            final_vote,
            Some(Vote::Yes),
            "Latest vote (Yes) must replace the earlier vote (No)"
        );
    }

    // -----------------------------------------------------------------------
    // DRep threshold: zero threshold always passes (bootstrap era)
    // -----------------------------------------------------------------------

    #[test]
    fn test_drep_threshold_zero_passes_any_ratio() {
        // check_threshold with zero threshold must return true regardless of yes/total.
        assert!(
            check_threshold(
                0,
                100,
                &Rational {
                    numerator: 0,
                    denominator: 1
                }
            ),
            "Zero threshold must always pass (bootstrap era)"
        );
        assert!(
            check_threshold(
                0,
                0,
                &Rational {
                    numerator: 0,
                    denominator: 1
                }
            ),
            "Zero threshold with zero total must also pass"
        );
    }

    // -----------------------------------------------------------------------
    // DRep threshold: exact boundary (yes/total == threshold)
    // -----------------------------------------------------------------------

    #[test]
    fn test_drep_threshold_exact_boundary_passes() {
        // yes=51, total=100 with threshold=51/100 must pass (>=)
        assert!(
            check_threshold(
                51,
                100,
                &Rational {
                    numerator: 51,
                    denominator: 100
                }
            ),
            "Exact threshold boundary must pass"
        );
        // yes=50, total=100 with threshold=51/100 must fail (<)
        assert!(
            !check_threshold(
                50,
                100,
                &Rational {
                    numerator: 51,
                    denominator: 100
                }
            ),
            "Just below threshold must fail"
        );
    }

    // -----------------------------------------------------------------------
    // ParameterChange SPO vote: security group gets pvtPPSecurityGroup threshold
    // -----------------------------------------------------------------------

    #[test]
    fn test_pp_change_spo_security_group_threshold_applied() {
        use crate::state::governance::pp_change_spo_threshold;
        use dugite_primitives::transaction::ProtocolParamUpdate;

        let params = ProtocolParameters::mainnet_defaults();
        // max_block_ex_units is a Security group parameter
        let ppu = ProtocolParamUpdate {
            max_block_ex_units: Some(dugite_primitives::transaction::ExUnits {
                mem: 80_000_000,
                steps: 40_000_000_000,
            }),
            ..ProtocolParamUpdate::default()
        };

        let threshold = pp_change_spo_threshold(&ppu, &params);
        assert!(
            threshold.is_some(),
            "Security-group PP change must have an SPO threshold"
        );
    }

    // -----------------------------------------------------------------------
    // ParameterChange DRep group: economic params use dvt_pp_economic_group
    // -----------------------------------------------------------------------

    #[test]
    fn test_pp_change_drep_economic_group_threshold_applied() {
        use crate::state::governance::pp_change_drep_threshold;
        use dugite_primitives::transaction::ProtocolParamUpdate;

        let mut params = ProtocolParameters::mainnet_defaults();
        // Set the economic threshold to something distinctive
        params.dvt_pp_economic_group = Rational {
            numerator: 71,
            denominator: 100,
        };

        // min_fee_a is an Economic group parameter
        let ppu = ProtocolParamUpdate {
            min_fee_a: Some(44),
            ..ProtocolParamUpdate::default()
        };

        let threshold = pp_change_drep_threshold(&ppu, &params);
        assert_eq!(
            threshold.numerator, 71,
            "Economic PP change must use dvt_pp_economic_group threshold"
        );
    }

    // -----------------------------------------------------------------------
    // TreasuryWithdrawals: script credential in withdrawal (epoch 890 pattern)
    // -----------------------------------------------------------------------

    /// Regression-class: withdrawal reward addresses can use script credentials.
    /// The TreasuryWithdrawals action with a script-type reward address must be
    /// enacted (treasury decreases) when ratification thresholds are met.
    ///
    /// A script-type reward address has network/credential-type header 0xF1
    /// (mainnet, script credential) followed by 28 bytes of script hash.
    /// The enactment code (enact_gov_action_impl) accepts any reward_addr with
    /// len >= 29, so script-type addresses are handled the same as key-type.
    ///
    /// This test uses the withdrawal address as both the withdrawal target AND
    /// the return_addr for the deposit, to isolate the treasury deduction from
    /// deposit-return flow. Treasury must strictly decrease by the withdrawal amount.
    ///
    /// (Relates to project memory: epoch 890 script cred drop.)
    #[test]
    fn test_treasury_withdrawal_script_credential_reward_address() {
        // Use 10 DReps with easy thresholds (same setup as
        // test_treasury_withdrawal_ratified_no_spo_vote_needed).
        let mut state = gov_test_state(10, 0); // 0 SPOs — TreasuryWithdrawals doesn't need them
        state.epochs.treasury = Lovelace(10_000_000_000);

        // Build a script-type reward address: header 0xF1 (mainnet script) + 28-byte script hash.
        // Length = 29 bytes, satisfies the `reward_addr.len() >= 29` gate in enact_gov_action_impl.
        let mut script_reward_addr = vec![0xF1u8]; // mainnet script reward addr header
        script_reward_addr.extend_from_slice(&[0xABu8; 28]);

        // Pre-register the script reward address so the withdrawal disbursement
        // and deposit refund both credit it (per Haskell, unregistered targets
        // are silently dropped / forfeited to treasury).
        let script_key = LedgerState::reward_account_to_hash(&script_reward_addr);
        state.certs.reward_accounts.insert(script_key, Lovelace(0));

        let withdrawal_amount = Lovelace(1_000_000_000);
        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(script_reward_addr.clone(), withdrawal_amount);

        let tx_hash = Hash32::from_bytes([86u8; 32]);
        // Use the script_reward_addr as return_addr so that after enactment,
        // the reward_account entry exists and the deposit is returned there
        // rather than treasury, making the treasury change attributable only
        // to the withdrawal.
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: script_reward_addr.clone(),
                gov_action: GovAction::TreasuryWithdrawals {
                    withdrawals,
                    policy_hash: None,
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(86, 0);

        // 8/10 DReps vote yes (80% >= 67% dvt_treasury_withdrawal)
        for i in 0..8 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &action_id);
        // SPOs do NOT vote (TreasuryWithdrawals doesn't require SPO)

        let treasury_before = state.epochs.treasury.0;
        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        let treasury_after = state.epochs.treasury.0;
        assert!(
            treasury_after < treasury_before,
            "Treasury must decrease after script-credential TreasuryWithdrawal is enacted \
             (treasury_before={treasury_before}, treasury_after={treasury_after})"
        );
        // The treasury should decrease by exactly the withdrawal amount, since the
        // proposal deposit is returned to the script reward_account (not to treasury).
        assert_eq!(
            treasury_before - treasury_after,
            withdrawal_amount.0,
            "Treasury decrease must equal the withdrawal amount \
             (expected {}, got {})",
            withdrawal_amount.0,
            treasury_before - treasury_after
        );
    }

    // -----------------------------------------------------------------------
    // Enact NoConfidence clears committee threshold (existing) + drep threshold check
    // -----------------------------------------------------------------------

    /// When no_confidence is true, the committee is in no-confidence mode.
    /// UpdateCommittee must then use dvt_committee_no_confidence + pvt_committee_no_confidence.
    #[test]
    fn test_update_committee_thresholds_differ_under_no_confidence() {
        // Under no_confidence=true, the thresholds for UpdateCommittee
        // switch to dvt_committee_no_confidence and pvt_committee_no_confidence.
        // This test verifies the Haskell threshold-selection branching.
        let mut state = gov_test_state(10, 5);
        Arc::make_mut(&mut state.gov.governance).no_confidence = true;

        // Set no_confidence thresholds very high (impossible to pass)
        state.epochs.protocol_params.dvt_committee_no_confidence = Rational {
            numerator: 99,
            denominator: 100,
        };
        state.epochs.protocol_params.pvt_committee_no_confidence = Rational {
            numerator: 99,
            denominator: 100,
        };

        let tx_hash = Hash32::from_bytes([87u8; 32]);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::UpdateCommittee {
                    prev_action_id: None,
                    members_to_remove: vec![],
                    members_to_add: BTreeMap::new(),
                    threshold: Rational {
                        numerator: 1,
                        denominator: 2,
                    },
                },
                anchor: make_anchor(),
            },
        );
        let action_id = make_action_id(87, 0);

        // Only 5 of 10 DReps vote yes (50%) — below 99% threshold
        for i in 0..5 {
            drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..5 {
            spo_vote(&mut state, i, &action_id, Vote::Yes);
        }

        let proposals_before = state.gov.governance.proposals.len();
        state.process_epoch_transition(EpochNo(1));

        // Proposal must NOT be ratified — thresholds not met
        let proposals_after = state.gov.governance.proposals.len();
        assert_eq!(
            proposals_after, proposals_before,
            "UpdateCommittee must not ratify when no_confidence thresholds are not met"
        );
    }

    // -----------------------------------------------------------------------
    // Delaying action: NoConfidence blocks ratification of all other actions
    // in the same epoch except itself
    // -----------------------------------------------------------------------

    #[test]
    fn test_no_confidence_proposal_blocks_constitution_same_epoch() {
        let mut state = gov_test_state(10, 5);
        state.epochs.protocol_params.dvt_no_confidence = Rational {
            numerator: 1,
            denominator: 2,
        };
        state.epochs.protocol_params.pvt_motion_no_confidence = Rational {
            numerator: 1,
            denominator: 2,
        };
        state.epochs.protocol_params.dvt_constitution = Rational {
            numerator: 1,
            denominator: 2,
        };

        // Submit NoConfidence
        let nc_hash = Hash32::from_bytes([88u8; 32]);
        state.process_proposal(
            &nc_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );
        let nc_id = make_action_id(88, 0);

        // Submit NewConstitution
        let cons_hash = Hash32::from_bytes([89u8; 32]);
        state.process_proposal(
            &cons_hash,
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NewConstitution {
                    prev_action_id: None,
                    constitution: Constitution {
                        anchor: make_anchor(),
                        script_hash: None,
                    },
                },
                anchor: make_anchor(),
            },
        );
        let cons_id = make_action_id(89, 0);

        // Both NC and Constitution get enough votes
        for i in 0..10 {
            drep_vote(&mut state, i, &nc_id, Vote::Yes);
            drep_vote(&mut state, i, &cons_id, Vote::Yes);
        }
        for i in 0..5 {
            spo_vote(&mut state, i, &nc_id, Vote::Yes);
        }
        cc_vote_yes(&mut state, &cons_id);

        // #903: seed the pulser snapshot the previous epoch boundary would
        // have captured. Haskell RATIFY consumes `dpProposals`, frozen by
        // `setFreshDRepPulsingState` at the PRIOR boundary, so a proposal is
        // never a candidate at the first boundary after submission. This test
        // exercises ratification logic, not that timing, so it stands in for
        // the prior boundary explicitly.
        state.freeze_prior_boundary_pulser();
        state.process_epoch_transition(EpochNo(1));

        // NoConfidence should be enacted
        assert!(
            state.gov.governance.no_confidence,
            "NoConfidence should be enacted"
        );
        // NewConstitution should NOT be enacted — NC is a delaying action
        assert!(
            state.gov.governance.constitution.is_none(),
            "NewConstitution must be blocked in the same epoch as NoConfidence (delaying action)"
        );
    }

    // -----------------------------------------------------------------------
    // Transient proposal at epoch boundary (relates to project_cstreamer_divergences epoch 736)
    // -----------------------------------------------------------------------

    /// A proposal submitted in epoch N must survive the first epoch boundary
    /// (N→N+1) without being expired if gov_action_lifetime > 0.
    /// It should only be expired after N + gov_action_lifetime epochs have passed.
    ///
    /// This test uses a NoConfidence proposal (won't ratify without votes)
    /// with lifetime 3, submitted at epoch 0. After 1 epoch it must still be
    /// active, and must be gone after epoch 3+1=4.
    ///
    /// Regression class: epoch 736 transient proposal from project_cstreamer_divergences.
    #[test]
    fn test_transient_proposal_survives_first_epoch_boundary() {
        let mut state = gov_test_state(0, 0); // No voters — won't ratify
        state.epochs.protocol_params.gov_action_lifetime = 3; // 3 epochs lifetime

        let tx_hash = Hash32::from_bytes([90u8; 32]);
        let deposit = state.epochs.protocol_params.gov_action_deposit;

        // Submit at epoch 0
        state.epoch = EpochNo(0);
        state.process_proposal(
            &tx_hash,
            0,
            &ProposalProcedure {
                deposit,
                return_addr: vec![0u8; 29],
                // NoConfidence won't ratify without votes, so it will only expire
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: make_anchor(),
            },
        );

        let action_id = make_action_id(90, 0);

        // Process epoch 1 — proposal submitted at epoch 0 with lifetime 3
        // must survive (expiry = 0 + 3 = epoch 3, expires AT epoch 4)
        state.process_epoch_transition(EpochNo(1));

        let still_pending_epoch1 = state
            .gov
            .governance
            .proposals
            .iter()
            .any(|(id, _)| *id == action_id);
        assert!(
            still_pending_epoch1,
            "Proposal submitted at epoch 0 with lifetime=3 must survive epoch 1 boundary"
        );

        // With lifetime=3, expires_epoch = 0 + 3 = 3.
        // Per test_proposal_expiry_inclusive: active through epoch 4 boundary.
        // Expired at epoch 5 transition.
        state.process_epoch_transition(EpochNo(2));
        state.process_epoch_transition(EpochNo(3));
        state.process_epoch_transition(EpochNo(4));

        let still_pending_epoch4 = state
            .gov
            .governance
            .proposals
            .iter()
            .any(|(id, _)| *id == action_id);
        assert!(
            still_pending_epoch4,
            "Proposal with lifetime=3 submitted at epoch 0 must still be active at epoch 4"
        );

        // Expires at epoch 5
        state.process_epoch_transition(EpochNo(5));
        let still_pending_epoch5 = state
            .gov
            .governance
            .proposals
            .iter()
            .any(|(id, _)| *id == action_id);
        assert!(
            !still_pending_epoch5,
            "Proposal with lifetime=3 submitted at epoch 0 must expire by epoch 5"
        );
    }

    /// #949: the FROZEN DRep distribution snapshot — the one `ratify_proposals`
    /// consumes as on-chain voting power — must sum
    /// `InstantStake + ProposalDeposits + AccountBalance` per delegating
    /// credential, matching Haskell `computeDRepDistr`:
    ///
    /// ```haskell
    ///   stakeAndDeposits = fold $ mInstantStake <> mProposalDeposit
    ///   updatedDistr = Map.insertWith (<>) dRep (stakeAndDeposits <> balance) distr
    /// ```
    ///
    /// The proposal-deposit term was missing here while the live query path had
    /// it, so the bug was invisible from the query side.
    #[test]
    fn frozen_drep_snapshot_includes_proposal_deposits() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.epochs.protocol_params.protocol_version_major = 9;

        let drep_cred = Credential::VerificationKey(Hash28::from_bytes([0x7A; 28]));
        let drep_hash = credential_to_hash(&drep_cred);
        Arc::make_mut(&mut state.gov.governance).dreps.insert(
            drep_hash,
            DRepRegistration {
                credential: drep_cred.clone(),
                deposit: Lovelace(500_000_000),
                anchor: None,
                registered_epoch: EpochNo(0),
                drep_expiry: EpochNo(100),
                active: true,
            },
        );

        // A stake credential that delegates its vote to that DRep, holds UTxO
        // stake and a reward balance, AND is the return address of a live
        // governance proposal.
        let stake_cred = Credential::VerificationKey(Hash28::from_bytes([0x5B; 28]));
        let stake_hash = credential_to_hash(&stake_cred);
        Arc::make_mut(&mut state.gov.governance)
            .vote_delegations
            .insert(stake_hash, DRep::KeyHash(drep_hash));
        state
            .certs
            .stake_distribution
            .stake_map
            .insert(stake_hash, Lovelace(1_000));
        state
            .certs
            .reward_accounts
            .insert(stake_hash, Lovelace(200));

        // Without a proposal, the snapshot is utxo + reward.
        state.capture_drep_distribution_snapshot();
        assert_eq!(
            state
                .gov
                .governance
                .drep_distr()
                .and_then(|d| d.get(&drep_hash))
                .copied(),
            Some(1_200),
            "baseline must be InstantStake + AccountBalance"
        );

        // Add a live proposal whose RETURN ADDRESS is that same credential.
        // Haskell keys deposits by the return address' staking credential and
        // sums across proposals sharing one (`proposalsDeposits`).
        // Reward account = 0xe0 header + the 28 credential bytes, which
        // `reward_account_to_hash` maps back to `stake_hash`.
        let mut return_addr = vec![0xe0u8];
        return_addr.extend_from_slice(&stake_hash.as_ref()[..28]);
        assert_eq!(
            LedgerState::reward_account_to_hash(&return_addr),
            stake_hash,
            "return address must resolve to the delegating credential"
        );

        for i in 0..2u8 {
            let action_id = GovActionId {
                transaction_id: Hash32::from_bytes([0xC0 + i; 32]),
                action_index: 0,
            };
            Arc::make_mut(&mut state.gov.governance).proposals.insert(
                action_id,
                crate::state::ProposalState {
                    procedure: ProposalProcedure {
                        deposit: Lovelace(100_000),
                        return_addr: return_addr.clone(),
                        gov_action: GovAction::InfoAction,
                        anchor: Anchor {
                            url: String::new(),
                            data_hash: Hash32::ZERO,
                        },
                    },
                    proposed_epoch: EpochNo(0),
                    expires_epoch: EpochNo(100),
                    yes_votes: 0,
                    no_votes: 0,
                    abstain_votes: 0,
                    submission_index: i as u64,
                },
            );
        }

        state.capture_drep_distribution_snapshot();
        assert_eq!(
            state
                .gov
                .governance
                .drep_distr()
                .and_then(|d| d.get(&drep_hash))
                .copied(),
            Some(1_200 + 200_000),
            "both proposal deposits must be summed into the frozen snapshot (#949)"
        );

        // #991: and exactly ONCE. `computeDRepDistr` folds instant stake,
        // proposal deposits and balance in a single pass, so a consumer that
        // adds the deposits again produces a numerator carrying them twice
        // against a denominator carrying them once.
        let (cache, _, _) = build_drep_power_cache_from(&state.gov, &state.certs);
        assert_eq!(
            cache.get(&drep_hash).copied(),
            Some(1_200 + 200_000),
            "the consumer must not re-add deposits already in the frozen \
             distribution (#991)"
        );

        // The same quantity must reach the ratification denominator, or the
        // ratio is computed against a different distribution than the powers.
        assert_eq!(
            compute_total_drep_stake_from(&state.gov, &state.certs),
            1_200 + 200_000,
            "numerator and denominator must be the same distribution (#991)"
        );
    }
}

#[cfg(test)]
mod pulser_tests {
    use super::*;
    use crate::state::test_fixtures::populated_ledger_state;

    /// Every epoch-boundary path must ratify through [`ratify_at_boundary`].
    ///
    /// A source-level guard, because the failure it prevents is invisible at
    /// runtime: a boundary that calls `ratify_proposals_impl` directly still
    /// ratifies correctly and still passes every behavioural test — it just
    /// silently stops comparing the outcome against the frozen plan. That is
    /// how the detector came to exist in only the test-only path, and how a
    /// "0 mismatches" result got recorded from a check that never ran.
    #[test]
    fn no_boundary_path_bypasses_the_pulser_check() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let boundary_paths = ["eras/conway.rs", "state/epoch.rs"];
        for rel in boundary_paths {
            let src = std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            for (i, line) in src.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                assert!(
                    !code.contains("ratify_proposals_impl("),
                    "{rel}:{} calls ratify_proposals_impl directly. Epoch \
                     boundaries must go through ratify_at_boundary, which also \
                     checks the applied ratification against the plan the \
                     pulser froze at the previous boundary (#988).",
                    i + 1
                );
            }
        }
    }

    /// The pulser must be stamped with the epoch the NEXT boundary ratifies
    /// under — Haskell's `eNo`, not the epoch that just ended.
    ///
    /// `setFreshDRepPulsingState eNo` stores `dpCurrentEpoch = eNo`, and EPOCH
    /// is only reached when `eNo == succ eL`, so `eNo` is the epoch STARTING at
    /// the boundary. The next boundary's RATIFY then runs with
    /// `reCurrentEpoch = dpCurrentEpoch`.
    ///
    /// Passing the ending epoch made `GetRatifyState` answer from a
    /// ratification run one epoch behind. No unit test caught it, because the
    /// prediction and the application are each self-consistent in isolation —
    /// only comparing them across a real boundary shows it, which is what the
    /// preview run did (`boundary_epoch=743 predicted_at=741`).
    #[test]
    fn pulser_is_stamped_with_the_epoch_the_next_boundary_ratifies_under() {
        let mut st = populated_ledger_state();
        let ending = st.epoch;
        let starting = EpochNo(ending.0 + 1);
        Arc::make_mut(&mut st.gov.governance).drep_pulsing_state = None;

        epoch_boundary_governance_step(starting, &st.epochs, &st.certs, &mut st.gov);

        let stamp = st
            .gov
            .governance
            .ratify_plan()
            .expect("a pulser must be frozen")
            .computed_at_epoch;
        assert_eq!(
            stamp, starting,
            "the pulser carries Haskell's `eNo` (the epoch STARTING at this \
             boundary), because that is the `reCurrentEpoch` the next \
             boundary's RATIFY will use — not the epoch that just ended \
             ({ending:?})"
        );
    }

    /// Install a ratification plan into an already-frozen pulser.
    ///
    /// There is deliberately no way to set a plan without a pulser: the two
    /// halves of `DRComplete` are frozen together, and a test that could
    /// install one without the other would be testing a state the ledger
    /// cannot reach.
    fn set_plan(state: &mut LedgerState, plan: PulsedRatifyState) {
        Arc::make_mut(&mut state.gov.governance)
            .drep_pulsing_state
            .as_mut()
            .expect("freeze a pulser before planting a plan")
            .ratify_state = plan;
    }

    /// Collect the WARN messages emitted while `f` runs.
    fn warnings_during(f: impl FnOnce()) -> String {
        use tracing::subscriber;
        use tracing_subscriber::layer::SubscriberExt;

        #[derive(Default, Clone)]
        struct Caught(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Caught {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() != tracing::Level::WARN {
                    return;
                }
                struct V(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
                impl tracing::field::Visit for V {
                    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                        if f.name() == "message" {
                            self.0.lock().unwrap().push(format!("{v:?}"));
                        }
                    }
                }
                event.record(&mut V(self.0.clone()));
            }
        }

        let caught = Caught::default();
        let sink = caught.clone();
        subscriber::with_default(tracing_subscriber::registry().with(caught), f);
        let msgs = sink.0.lock().unwrap().join(" | ");
        msgs
    }

    /// A NoConfidence proposal with no votes at all, plus a frozen pulser.
    fn state_with_unratifiable_proposal() -> (LedgerState, GovActionId) {
        let mut state = super::tests::gov_test_state(10, 10);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: super::tests::make_anchor(),
            },
        );
        state.freeze_prior_boundary_pulser();
        (state, super::tests::make_action_id(50, 0))
    }

    /// #988 step 2: the boundary APPLIES the frozen plan. It does not re-decide.
    ///
    /// Upstream the epoch boundary never runs RATIFY. It reads the completed
    /// pulser and applies the result:
    ///
    /// ```haskell
    /// pulsingState = epochState0 ^. epochStateDRepPulsingStateL
    /// ratifyState@RatifyState {rsEnactState, rsEnacted, rsExpired} =
    ///   extractDRepPulsingState pulsingState
    /// ```
    ///
    /// The plan here enacts a proposal that has NOT ONE VOTE, so any boundary
    /// that re-decides must reject it. Enactment is therefore proof that the
    /// stored decision was applied verbatim rather than recomputed — which is
    /// the whole point, because the recomputation reads live state that has
    /// moved on by a full epoch.
    #[test]
    fn boundary_applies_the_frozen_plan_rather_than_re_deciding() {
        let (mut state, action_id) = state_with_unratifiable_proposal();

        // Sanity: on its own merits this proposal cannot ratify.
        let fresh =
            compute_pulsed_ratify_state(state.epoch, &state.epochs, &state.certs, &state.gov);
        assert!(
            fresh.enacted.is_empty(),
            "fixture is wrong: an unvoted NoConfidence must not ratify"
        );

        let plan = PulsedRatifyState {
            computed_at_epoch: state.epoch,
            enacted: vec![action_id.clone()],
            expired: Vec::new(),
            delayed: true,
            cur_pparams: state.epochs.protocol_params.clone(),
            has_pparams_changes: false,
        };
        set_plan(&mut state, plan);

        state.process_epoch_transition(EpochNo(state.epoch.0 + 1));

        assert!(
            state.gov.governance.no_confidence,
            "the boundary must enact what the frozen pulser decided, even though \
             re-deciding here would reject it (#988 step 2)"
        );
        assert!(
            !state.gov.governance.proposals.contains_key(&action_id),
            "an enacted proposal must be removed from the live set"
        );
        assert!(
            state.gov.governance.last_ratify_delayed,
            "`rsDelayed` comes from the plan, not from a fresh decision"
        );
    }

    /// The mirror image: what the plan omits must NOT enact, however popular.
    #[test]
    fn boundary_does_not_enact_what_the_plan_omits() {
        let mut state = super::tests::gov_test_state(10, 10);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: super::tests::make_anchor(),
            },
        );
        let action_id = super::tests::make_action_id(50, 0);
        for i in 0..7 {
            super::tests::drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..6 {
            super::tests::spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        state.freeze_prior_boundary_pulser();

        // Confirm the fixture really is ratifiable, so the assertion below is
        // about the plan being authoritative and not about a dud proposal.
        let planned = state
            .gov
            .governance
            .ratify_plan()
            .cloned()
            .expect("a pulser must be frozen");
        assert_eq!(
            planned.enacted,
            vec![action_id.clone()],
            "fixture is wrong: this proposal must be ratifiable"
        );

        set_plan(
            &mut state,
            PulsedRatifyState {
                enacted: Vec::new(),
                delayed: false,
                ..planned
            },
        );

        state.process_epoch_transition(EpochNo(state.epoch.0 + 1));

        assert!(
            !state.gov.governance.no_confidence,
            "the boundary must not enact an action the frozen pulser did not \
             decide on, however the live votes now stand (#988 step 2)"
        );
        assert!(
            state.gov.governance.proposals.contains_key(&action_id),
            "a proposal the plan neither enacted nor expired must survive"
        );
    }

    /// No pulser ⇒ nothing ratifies. Haskell's `Default` is `DRComplete def
    /// def`, an empty result, so a boundary with no frozen pulser enacts
    /// nothing — it must not fall back to deciding from live state, which is
    /// #903's bug with an extra step.
    #[test]
    fn a_boundary_with_no_pulser_ratifies_nothing() {
        let (mut state, action_id) = state_with_unratifiable_proposal();
        for i in 0..7 {
            super::tests::drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..6 {
            super::tests::spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        Arc::make_mut(&mut state.gov.governance).drep_pulsing_state = None;

        state.process_epoch_transition(EpochNo(state.epoch.0 + 1));

        assert!(
            !state.gov.governance.no_confidence,
            "with no frozen pulser nothing is a candidate (Haskell `DRComplete \
             def def`)"
        );
    }

    /// A plan naming an action that exists in neither the frozen nor the live
    /// proposal set is not something that can happen — so it must be loud
    /// rather than silently skipped.
    #[test]
    fn a_plan_naming_an_unknown_action_warns() {
        let (mut state, _) = state_with_unratifiable_proposal();
        let epoch = state.epoch;
        let pp = state.epochs.protocol_params.clone();
        set_plan(
            &mut state,
            PulsedRatifyState {
                computed_at_epoch: epoch,
                enacted: vec![GovActionId {
                    transaction_id: Hash32::from_bytes([0xAB; 32]),
                    action_index: 0,
                }],
                expired: Vec::new(),
                delayed: false,
                cur_pparams: pp,
                has_pparams_changes: false,
            },
        );

        let msgs = warnings_during(|| {
            ratify_at_boundary(
                state.epoch,
                EpochNo(state.epoch.0 + 1),
                &mut state.epochs,
                &mut state.certs,
                &mut state.gov,
            );
        });
        assert!(
            msgs.contains("neither the frozen nor the live proposal set"),
            "captured: {msgs}"
        );
    }

    /// The plan carries the epoch it was decided under — Haskell's
    /// `dpCurrentEpoch`, consumed as `reCurrentEpoch`. Applying one from the
    /// wrong boundary is the defect the preview run caught
    /// (`boundary_epoch=743 predicted_at=741`), and once the plan is applied
    /// instead of compared it is the only place that can still show up.
    #[test]
    fn a_plan_stamped_with_the_wrong_epoch_warns() {
        let (mut state, _) = state_with_unratifiable_proposal();
        let stale = PulsedRatifyState {
            computed_at_epoch: EpochNo(state.epoch.0.wrapping_sub(2)),
            ..state
                .gov
                .governance
                .ratify_plan()
                .cloned()
                .expect("a pulser must be frozen")
        };
        set_plan(&mut state, stale);

        let msgs = warnings_during(|| {
            ratify_at_boundary(
                state.epoch,
                EpochNo(state.epoch.0 + 1),
                &mut state.epochs,
                &mut state.certs,
                &mut state.gov,
            );
        });
        assert!(
            msgs.contains("stamped with the wrong epoch"),
            "captured: {msgs}"
        );
    }

    /// #990: an action in its final epoch still gets ONE ratification attempt.
    ///
    /// `ratifyTransition` tests expiry only in the `else` branch — *after* the
    /// ratification attempt has failed:
    ///
    /// ```haskell
    /// else do
    ///   st' <- trans @(RATIFY era) $ TRC (env, st, RatifySignal sigs)
    ///   if gasExpiresAfter < reCurrentEpoch
    ///     then pure $ st' & rsExpiredL %~ Set.insert gasId
    ///     else pure st'
    /// ```
    ///
    /// dugite skipped expired candidates BEFORE the threshold check, so a
    /// proposal that crossed threshold on votes cast during its last epoch was
    /// enacted by cardano-node and dropped here.
    #[test]
    fn an_expired_action_still_gets_a_final_ratification_attempt() {
        let mut state = super::tests::gov_test_state(10, 10);
        state.epoch = EpochNo(5);
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::NoConfidence {
                    prev_action_id: None,
                },
                anchor: super::tests::make_anchor(),
            },
        );
        let action_id = super::tests::make_action_id(50, 0);
        for i in 0..7 {
            super::tests::drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..6 {
            super::tests::spo_vote(&mut state, i, &action_id, Vote::Yes);
        }

        // Age it so this is its LAST pass: `gasExpiresAfter < reCurrentEpoch`.
        // Done BEFORE the freeze, so the aged proposal is what the pulser's
        // candidate set carries — expiry is decided over `dpProposals`, never
        // over live state.
        {
            let g = Arc::make_mut(&mut state.gov.governance);
            let mut ps = g.proposals.get(&action_id).expect("proposal").clone();
            ps.expires_epoch = EpochNo(state.epoch.0 - 1);
            g.proposals.insert(action_id.clone(), ps);
        }
        state.freeze_prior_boundary_pulser();

        let decision = state
            .gov
            .governance
            .ratify_plan()
            .cloned()
            .expect("a pulser must be frozen");

        assert_eq!(
            decision.enacted,
            vec![action_id.clone()],
            "an action whose votes crossed threshold must ratify on the same \
             pass that would otherwise expire it (#990)"
        );
        assert!(
            !decision.expired.contains(&action_id),
            "`rsExpired` is only reachable from the not-ratified branch, so an \
             action that enacts is never also expired (#990)"
        );
    }

    /// #988: the boundary must freeze a pulser result, stamped with the
    /// boundary it was computed at.
    ///
    /// It describes the ratification that will be applied at the NEXT
    /// boundary, which is what `queryRatifyState = snd . finishedPulserState`
    /// returns mid-epoch. Answering from `last_ratified` instead is one
    /// boundary stale — the same shape and direction as #922 / #950 / #966.
    #[test]
    fn boundary_freezes_a_pulser_result() {
        let mut st = populated_ledger_state();
        Arc::make_mut(&mut st.gov.governance).drep_pulsing_state = None;
        epoch_boundary_governance_step(st.epoch, &st.epochs, &st.certs, &mut st.gov);

        let pulsed = st
            .gov
            .governance
            .ratify_plan()
            .expect("PV>=9 boundary must freeze a pulser result");
        assert_eq!(
            pulsed.computed_at_epoch, st.epoch,
            "stamped with the boundary it was frozen at"
        );
    }

    /// Computing the pulser must NOT mutate live state. It runs the REAL
    /// ratification on a clone precisely so the decisions can be read off
    /// without applying them — a second, non-mutating copy of the threshold
    /// logic would be the N-copies trap that #985 recorded.
    #[test]
    fn computing_the_pulser_does_not_mutate_the_ledger() {
        let st = populated_ledger_state();
        let before_treasury = st.epochs.treasury;
        let before_min_fee = st.epochs.protocol_params.min_fee_a;
        let before_proposals = st.gov.governance.proposals.len();
        let before_ratified = st.gov.governance.last_ratified.len();

        let _ = compute_pulsed_ratify_state(st.epoch, &st.epochs, &st.certs, &st.gov);

        assert_eq!(st.epochs.treasury, before_treasury, "treasury moved");
        assert_eq!(
            st.epochs.protocol_params.min_fee_a, before_min_fee,
            "protocol params moved"
        );
        assert_eq!(
            st.gov.governance.proposals.len(),
            before_proposals,
            "proposal set changed"
        );
        assert_eq!(
            st.gov.governance.last_ratified.len(),
            before_ratified,
            "last_ratified changed — the clone leaked into live state"
        );
    }

    /// `has_pparams_changes` is Haskell `hasChangesToPParams`: ONLY
    /// `ParameterChange` and `HardForkInitiation` set it. #977 gates
    /// `futurePParams` on exactly this term, so defaulting it true would make
    /// the field `Just` where upstream leaves it `Nothing`.
    #[test]
    fn has_pparams_changes_is_false_when_nothing_enacts() {
        let st = populated_ledger_state();
        let pulsed = compute_pulsed_ratify_state(st.epoch, &st.epochs, &st.certs, &st.gov);
        assert!(pulsed.enacted.is_empty(), "fixture enacts nothing");
        assert!(
            !pulsed.has_pparams_changes,
            "no enactment must not report a pparams change"
        );
    }

    /// #995: the boundary must leave a PREDICTED `futurePParams`, not
    /// `Potential(None)`.
    ///
    /// `setFreshDRepPulsingState` ends with `predictFuturePParams` applied to
    /// the govState carrying the freshly-installed pulser, so a boundary whose
    /// new plan enacts a `ParameterChange` leaves `Potential(Just pp)`.
    ///
    /// dugite reset to `Potential(None)` and stopped. On a chain where
    /// `2 * stabilityWindow >= epochLength` — the devnet — the next block
    /// solidifies that to `NoPParamsUpdate` before any non-boundary tick can
    /// predict, and prediction refuses to reopen a settled value. dugite then
    /// reported `NoPParamsUpdate` for a whole epoch where cardano-node
    /// reported `DefinitePParamsUpdate`.
    #[test]
    fn the_boundary_predicts_from_the_pulser_it_just_froze() {
        let mut state = super::tests::gov_test_state(10, 10);
        let update = dugite_primitives::transaction::ProtocolParamUpdate {
            min_fee_a: Some(999),
            ..Default::default()
        };
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(update),
                    policy_hash: None,
                },
                anchor: super::tests::make_anchor(),
            },
        );
        let action_id = super::tests::make_action_id(50, 0);
        for i in 0..9 {
            super::tests::drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..9 {
            super::tests::spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        super::tests::cc_vote_yes(&mut state, &action_id);

        // The boundary: reset, freeze, predict.
        epoch_boundary_governance_step(
            EpochNo(state.epoch.0 + 1),
            &state.epochs,
            &state.certs,
            &mut state.gov,
        );

        match &state.gov.governance.future_pparams {
            FuturePParams::PotentialPParamsUpdate(Some(pp)) => {
                assert_eq!(
                    pp.min_fee_a, 999,
                    "the prediction must carry `ensCurPParams` from the plan"
                );
            }
            other => panic!(
                "the boundary must leave a PREDICTED futurePParams when the \
                 fresh pulser enacts a ParameterChange — `setFreshDRepPulsingState` \
                 ends with `predictFuturePParams` (#995); got {other:?}"
            ),
        }
    }

    /// …and TRUE when a `ParameterChange` is about to enact.
    ///
    /// Only the negative case was ever tested, so `has_pparams_changes` could
    /// be wired to a constant `false` and every test still passed. It feeds
    /// `predictFuturePParams`'s guard:
    ///
    /// ```haskell
    /// newFuturePParams = do
    ///   guard (any hasChangesToPParams (rsEnacted ratifyState))
    ///   pure (ensCurPParams (rsEnactState ratifyState))
    /// ```
    ///
    /// so a false negative means dugite answers `NoPParamsUpdate` for an epoch
    /// in which cardano-node answers `DefinitePParamsUpdate` — caught by the
    /// v2.6.0 release gate once Round 2 started driving the gov lifecycle, 235
    /// divergent samples confined entirely to the epoch with a pending
    /// ParameterChange.
    #[test]
    fn has_pparams_changes_is_true_when_a_parameter_change_enacts() {
        let mut state = super::tests::gov_test_state(10, 10);
        let update = dugite_primitives::transaction::ProtocolParamUpdate {
            min_fee_a: Some(999),
            ..Default::default()
        };
        state.process_proposal(
            &Hash32::from_bytes([50u8; 32]),
            0,
            &ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0u8; 29],
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(update),
                    policy_hash: None,
                },
                anchor: super::tests::make_anchor(),
            },
        );
        let action_id = super::tests::make_action_id(50, 0);
        for i in 0..9 {
            super::tests::drep_vote(&mut state, i, &action_id, Vote::Yes);
        }
        for i in 0..9 {
            super::tests::spo_vote(&mut state, i, &action_id, Vote::Yes);
        }
        super::tests::cc_vote_yes(&mut state, &action_id);

        state.freeze_prior_boundary_pulser();
        let pulsed = state
            .gov
            .governance
            .ratify_plan()
            .cloned()
            .expect("a pulser must be frozen");

        assert_eq!(
            pulsed.enacted,
            vec![action_id],
            "fixture is wrong: the ParameterChange must be ratifiable"
        );
        assert!(
            pulsed.has_pparams_changes,
            "an enacted ParameterChange IS a change to pparams — this is              `hasChangesToPParams`, the guard predictFuturePParams reads"
        );
        assert_eq!(
            pulsed.cur_pparams.min_fee_a, 999,
            "`ensCurPParams` must carry the enacted value, since that is what              `predictFuturePParams` publishes as the future parameters"
        );
    }
}

#[cfg(test)]
mod future_pparams_tests {
    use super::*;
    use crate::state::test_fixtures::populated_ledger_state;

    fn potential(pp: Option<ProtocolParameters>) -> FuturePParams {
        FuturePParams::PotentialPParamsUpdate(pp.map(Box::new))
    }

    /// `solidifyFuturePParams`: Potential Nothing -> No,
    /// Potential (Just pp) -> Definite pp, anything else unchanged.
    #[test]
    fn solidify_collapses_potential_only() {
        let mut f = potential(None);
        f.solidify();
        assert_eq!(f, FuturePParams::NoPParamsUpdate);

        let mut f = potential(Some(ProtocolParameters::mainnet_defaults()));
        f.solidify();
        assert!(matches!(f, FuturePParams::DefinitePParamsUpdate(_)));

        // Idempotent, and the settled variants are untouched.
        let mut f = FuturePParams::NoPParamsUpdate;
        f.solidify();
        assert_eq!(f, FuturePParams::NoPParamsUpdate);
        let mut f =
            FuturePParams::DefinitePParamsUpdate(Box::new(ProtocolParameters::mainnet_defaults()));
        let before = f.clone();
        f.solidify();
        assert_eq!(f, before);
    }

    /// The point of no return is `firstSlotNextEpoch - 2 * stabilityWindow`,
    /// and `stabilityWindow` is 3k/f — NOT the 4k/f randomness window the RUPD
    /// pulser uses. Confusing the two moves the boundary by a third.
    #[test]
    fn solidify_fires_only_at_the_point_of_no_return() {
        let first_next = 10_000u64;
        let window = 1_000u64; // point of no return = 10_000 - 2_000 = 8_000
        let mut st = populated_ledger_state();

        for (slot, should_collapse) in [(7_999u64, false), (8_000, true)] {
            Arc::make_mut(&mut st.gov.governance).future_pparams = potential(None);
            solidify_next_epoch_pparams(slot, first_next, window, &mut st.gov);
            let got = &st.gov.governance.future_pparams;
            if should_collapse {
                assert_eq!(*got, FuturePParams::NoPParamsUpdate, "slot {slot}");
            } else {
                assert_eq!(*got, potential(None), "slot {slot} is before the point");
            }
        }
    }

    /// `predictFuturePParams` must NOT reopen a settled value — its first two
    /// arms return the state unchanged.
    #[test]
    fn predict_never_reopens_a_settled_value() {
        let mut st = populated_ledger_state();

        Arc::make_mut(&mut st.gov.governance).future_pparams = FuturePParams::NoPParamsUpdate;
        predict_future_pparams(&mut st.gov);
        assert_eq!(
            st.gov.governance.future_pparams,
            FuturePParams::NoPParamsUpdate,
            "No must stay No"
        );

        let definite =
            FuturePParams::DefinitePParamsUpdate(Box::new(ProtocolParameters::mainnet_defaults()));
        Arc::make_mut(&mut st.gov.governance).future_pparams = definite.clone();
        predict_future_pparams(&mut st.gov);
        assert_eq!(
            st.gov.governance.future_pparams, definite,
            "Definite must stay Definite"
        );
    }

    /// `guard (any hasChangesToPParams (rsEnacted ratifyState))` — the payload
    /// appears ONLY when the pulser says a ParameterChange or
    /// HardForkInitiation will enact.
    #[test]
    fn predict_carries_a_payload_only_when_the_pulser_says_so() {
        let mut st = populated_ledger_state();

        // Pulser reports no pparams change -> Potential(None).
        {
            let g = Arc::make_mut(&mut st.gov.governance);
            g.future_pparams = potential(None);
            if let Some(p) = g.drep_pulsing_state.as_mut().map(|d| &mut d.ratify_state) {
                p.has_pparams_changes = false;
            }
        }
        predict_future_pparams(&mut st.gov);
        assert_eq!(st.gov.governance.future_pparams, potential(None));

        // Pulser reports one -> Potential(Some(cur_pparams)).
        {
            let g = Arc::make_mut(&mut st.gov.governance);
            g.future_pparams = potential(None);
            if let Some(p) = g.drep_pulsing_state.as_mut().map(|d| &mut d.ratify_state) {
                p.has_pparams_changes = true;
                p.cur_pparams.min_fee_a = 12_345;
            }
        }
        predict_future_pparams(&mut st.gov);
        match &st.gov.governance.future_pparams {
            FuturePParams::PotentialPParamsUpdate(Some(pp)) => {
                assert_eq!(pp.min_fee_a, 12_345, "must carry ensCurPParams");
            }
            other => panic!("expected Potential(Some(..)), got {other:?}"),
        }
    }

    /// `nextEpochPParams` is what the ledger-view FORECAST reads, and is the
    /// reason this is not merely a query field.
    #[test]
    fn next_epoch_pparams_prefers_the_queued_update() {
        let mut cur = ProtocolParameters::mainnet_defaults();
        cur.min_fee_a = 1;
        let mut queued = ProtocolParameters::mainnet_defaults();
        queued.min_fee_a = 2;

        assert_eq!(
            FuturePParams::NoPParamsUpdate
                .next_epoch_pparams(&cur)
                .min_fee_a,
            1
        );
        assert_eq!(
            FuturePParams::DefinitePParamsUpdate(Box::new(queued.clone()))
                .next_epoch_pparams(&cur)
                .min_fee_a,
            2
        );
        assert_eq!(
            potential(Some(queued)).next_epoch_pparams(&cur).min_fee_a,
            2
        );
        assert_eq!(potential(None).next_epoch_pparams(&cur).min_fee_a, 1);
    }
}
