//! Transaction validation — Phase-1 and Phase-2.
//!
//! This module is the public surface of the validation subsystem. It:
//! - Defines [`ValidationError`], the unified error type for all validation rules.
//! - Provides [`validate_transaction`] and [`validate_transaction_with_pools`] as
//!   the sole public entry points.
//! - Re-exports [`evaluate_native_script`] for callers that need to evaluate
//!   native scripts outside of full transaction validation (e.g. mempool admission).
//!
//! Internal rule logic is split across focused sub-modules:
//! - [`phase1`]    — Rules 1–10, 13–14 (structural/witness rules)
//! - [`collateral`] — Rules 11, 11b, 11c (collateral for Plutus transactions)
//! - [`scripts`]   — Rule 12 + script hash utilities + native script evaluation
//! - [`conway`]    — Era-gating checks + deposit/refund accounting

mod collateral;
mod conway;
mod datum;
pub mod mir;
mod phase1;
pub mod ppup;
mod scripts;
pub(crate) mod size_check;
pub mod withdrawals;

#[cfg(test)]
mod tests;

pub use scripts::evaluate_native_script;
// Dijkstra-aware native script evaluator + script-hash helper used by the
// Phase 3.5 guard-witness check in `eras::dijkstra`. Issue #475.
pub(crate) use scripts::{compute_script_ref_hash, evaluate_native_script_with_guards};
// Re-exported for use by the block-application layer (block-level ref script
// size check in state/apply.rs — Haskell's `conwayBbodyTransition`).
pub(crate) use scripts::script_ref_byte_size;
// Re-export the tier cap so apply.rs can reuse the same constant for the
// block-body check, keeping the tiered-fee short-circuit in sync.
#[allow(unused_imports)]
pub(crate) use scripts::MAX_REF_SCRIPT_SIZE_TIER_CAP;
// Re-exported for use by the block-application layer (per-transaction 200 KiB
// ref script size check — Haskell's `ppMaxRefScriptSizePerTxG` enforcement).
pub(crate) use scripts::calculate_ref_script_size;
// Re-exported for use by plutus.rs (V3 non-Unit return value check): maps
// script hashes to their language version so the evaluator can apply the
// correct success predicate per-result.
pub(crate) use collateral::plutus_script_version_map;
// Re-exported for use by plutus.rs (per-redeemer V3 Unit-return check): maps
// (redeemer_tag_byte, index) to the language version of the script that
// redeemer executes, allowing the Unit check to be applied only to V3 redeemers.
pub(crate) use collateral::redeemer_script_version_map;

use imbl::HashMap as ImblMap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::network::NetworkId;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{
    GovAction, GovActionId, ProposalProcedure, Transaction, Voter,
};
use dugite_primitives::value::Lovelace;
use std::cell::Cell;
use tracing::{debug, trace, warn};

use crate::plutus::{evaluate_plutus_scripts, SlotConfig};
use crate::utxo::UtxoLookup;

// ---------------------------------------------------------------------------
// Per-thread phase-2 skip gate
//
// When the block-apply path uses the deferred-parallel Phase-2 strategy
// (feature = "parallel-verification"), it sets this flag to `true` before
// calling `validate_transaction_with_context` so that the sequential phase-2
// eval inside `validate_transaction_with_pools` is suppressed. The parallel
// eval runs separately via `crate::plutus::run_phase2_parallel`.
//
// The flag is thread-local so it cannot leak across block-apply calls on
// different threads, and cannot interfere with the mempool path (which calls
// `validate_transaction*` on its own thread and never sets the flag).
// ---------------------------------------------------------------------------
thread_local! {
    static SKIP_PHASE2_EVAL: Cell<bool> = const { Cell::new(false) };
}

/// Set the thread-local phase-2 skip flag and return the previous value.
///
/// Used by the block-apply parallel-phase-2 path. Always restore the returned
/// value with [`restore_phase2_eval`] (even on error) to avoid leaking the
/// skip state across invocations.
pub(crate) fn suppress_phase2_eval() -> bool {
    SKIP_PHASE2_EVAL.with(|c| {
        let prev = c.get();
        c.set(true);
        prev
    })
}

/// Restore the thread-local phase-2 skip flag to a previously-saved value.
pub(crate) fn restore_phase2_eval(prev: bool) {
    SKIP_PHASE2_EVAL.with(|c| c.set(prev));
}

/// Re-validate an already-admitted mempool transaction against the current
/// ledger — dugite's equivalent of Haskell's `reapplyTx` (#996).
///
/// # Upstream semantics
///
/// `Ouroboros.Consensus.Mempool.Impl.Common.revalidateTxsFor` re-checks EVERY
/// remaining mempool transaction whenever the tip changes, via `reapplyTxs`
/// (not `applyTx`). At the ledger layer that is
/// `Cardano.Ledger.Shelley.API.Mempool`:
///
/// ```haskell
/// reapplyTx globals env state (Validated tx) =
///   fst <$> internalApplyTxWithValidation (ValidateSuchThat (notElem lblStatic)) globals env state tx
/// ```
///
/// `ValidateSuchThat (notElem lblStatic)` re-runs every **state-dependent**
/// predicate and skips only the **static** (context-free) ones. So a
/// transaction that was valid at admission and became invalid because a later
/// block changed the ledger IS dropped upstream —
/// `ConwayCommitteeHasPreviouslyResigned` is asserted with `failOnJust`, the
/// non-static form, and is therefore re-run.
///
/// dugite previously re-checked a hand-written list instead (consumed inputs,
/// TTL, missing UTxO, dangling gov-action votes), so every other predicate was
/// invisible after admission. That is #996: a `CommitteeHotAuth` for a cold
/// credential that resigned in an intervening block stayed in the mempool, was
/// forged, and cardano-node rejected the block permanently.
///
/// # What dugite skips
///
/// Phase-2 Plutus evaluation only — the static/context-free check, and the
/// expensive one. Everything else, including the witness checks Haskell also
/// labels static, is re-run: for a transaction whose inputs are unchanged
/// those are deterministic re-passes, so re-running them is a superset of
/// upstream's skip-set that cannot change any verdict.
///
/// # Errors
///
/// Returns the accumulated [`ValidationError`]s; any non-empty result means
/// the transaction must be evicted from the mempool.
pub fn reapply_tx_for_mempool(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    params: &ProtocolParameters,
    current_slot: u64,
    tx_size: u64,
    slot_config: Option<&SlotConfig>,
    context: ValidationContext,
) -> Result<(), Vec<ValidationError>> {
    let prev = suppress_phase2_eval();
    let result = validate_transaction_with_context(
        tx,
        utxo_set,
        params,
        current_slot,
        tx_size,
        slot_config,
        context,
    );
    restore_phase2_eval(prev);
    result
}

/// On-chain governance proposal record used by validation rules that need
/// access to a proposal's full state (not just the action itself).
///
/// This is the value type stored in [`ValidationContext::active_proposals`].
/// Future Conway GOV predicates need different fields:
/// - `DisallowedVoters` (Task 2): only `gov_action`.
/// - `VotingOnExpiredGovAction` (Task 4): `expires_after_epoch`.
/// - `ProposalReturnAccountDoesNotExist` (Task 5): `return_addr`.
///
/// `return_addr` is stored as raw bytes (`Vec<u8>`) to mirror the on-chain
/// `ProposalProcedure.return_addr` shape from `dugite-primitives`. Callers
/// performing the address-credential check must decode it themselves.
#[derive(Debug, Clone)]
pub struct ActiveProposal {
    /// The governance action being proposed.
    pub gov_action: GovAction,
    /// The reward address that receives the proposal deposit refund.
    /// Raw `ProposalProcedure.return_addr` bytes (header + 28-byte credential).
    pub return_addr: Vec<u8>,
    /// The proposal deposit (frozen at submission time).
    pub deposit: Lovelace,
    /// The last epoch in which votes are accepted (inclusive).
    pub expires_after_epoch: EpochNo,
    /// The epoch in which the proposal was submitted.
    pub proposed_in_epoch: EpochNo,
}

/// The last *enacted* governance action id for each lineal governance purpose —
/// Haskell's `Proposals.pRoots`, a `GovRelation` of `prRoot` values.
///
/// A `None` field means that purpose has never enacted an action, which is the
/// only state in which a proposal of that purpose may carry
/// `prev_action_id = None`.
///
/// `TreasuryWithdrawals` and `InfoAction` have no lineage and therefore no root.
///
/// This is the value type stored in [`ValidationContext::enacted_gov_roots`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnactedGovRoots {
    /// Root for `ParameterChange` proposals.
    pub pparam_update: Option<GovActionId>,
    /// Root for `HardForkInitiation` proposals.
    pub hard_fork: Option<GovActionId>,
    /// Root shared by `UpdateCommittee` AND `NoConfidence` — both act on the
    /// committee purpose and therefore share one lineage in Haskell
    /// (`grCommittee`).
    pub committee: Option<GovActionId>,
    /// Root for `NewConstitution` proposals.
    pub constitution: Option<GovActionId>,
}

impl EnactedGovRoots {
    /// The enacted root governing `action`'s purpose, or `None` when the action
    /// type has no lineage requirement (`TreasuryWithdrawals`, `InfoAction`).
    ///
    /// Mirrors the purpose selection in `genesis_root_is_valid` /
    /// `prev_action_matches_enacted_root` (`state/governance.rs`), which is the
    /// same mapping Haskell performs via `GovRelation` lenses.
    pub fn root_for(&self, action: &GovAction) -> Option<&GovActionId> {
        match action {
            GovAction::ParameterChange { .. } => self.pparam_update.as_ref(),
            GovAction::HardForkInitiation { .. } => self.hard_fork.as_ref(),
            GovAction::NoConfidence { .. } | GovAction::UpdateCommittee { .. } => {
                self.committee.as_ref()
            }
            GovAction::NewConstitution { .. } => self.constitution.as_ref(),
            GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => None,
        }
    }
}

/// Whether `action` participates in a lineal `prev_action_id` chain at all.
///
/// `TreasuryWithdrawals` and `InfoAction` are one-shot: Haskell's
/// `GovAction` has no `PrevGovActionId` field for them, so no ancestry check
/// applies and any `prev_action_id` is irrelevant.
fn gov_action_has_lineage(action: &GovAction) -> bool {
    matches!(
        action,
        GovAction::ParameterChange { .. }
            | GovAction::HardForkInitiation { .. }
            | GovAction::NoConfidence { .. }
            | GovAction::UpdateCommittee { .. }
            | GovAction::NewConstitution { .. }
    )
}

/// Short stable name for a `GovAction`, used in the
/// `InvalidPrevGovActionId` error message.
fn gov_action_type_name(action: &GovAction) -> &'static str {
    match action {
        GovAction::ParameterChange { .. } => "ParameterChange",
        GovAction::HardForkInitiation { .. } => "HardForkInitiation",
        GovAction::NoConfidence { .. } => "NoConfidence",
        GovAction::UpdateCommittee { .. } => "UpdateCommittee",
        GovAction::NewConstitution { .. } => "NewConstitution",
        GovAction::TreasuryWithdrawals { .. } => "TreasuryWithdrawals",
        GovAction::InfoAction => "InfoAction",
    }
}

/// Extract a proposal's `prev_action_id`, if its action type has one.
fn gov_action_prev_id(action: &GovAction) -> Option<&GovActionId> {
    match action {
        GovAction::ParameterChange { prev_action_id, .. }
        | GovAction::HardForkInitiation { prev_action_id, .. }
        | GovAction::NoConfidence { prev_action_id }
        | GovAction::UpdateCommittee { prev_action_id, .. }
        | GovAction::NewConstitution { prev_action_id, .. } => prev_action_id.as_ref(),
        GovAction::TreasuryWithdrawals { .. } | GovAction::InfoAction => None,
    }
}

/// `Clone` is cheap by construction: every heavyweight field is an [`Arc`] or
/// an `imbl` persistent map, so a clone is a refcount bump, not a deep copy.
/// Post-block mempool revalidation (#996) needs one context per transaction.
#[derive(Default, Clone)]
pub struct ValidationContext {
    pub registered_pools: Option<Arc<HashSet<Hash28>>>,
    pub current_treasury: Option<u64>,
    /// Reward account balances for validation (pre-block snapshot).
    ///
    /// Uses `imbl::HashMap` so that the per-block snapshot built in
    /// `apply_block` is an O(1) structural clone (not an O(N) deep copy).
    /// Apply-path mutations use the live `CertSubState.reward_accounts` imbl
    /// map independently — no Arc CoW contention, no per-block deep-clone.
    pub reward_accounts: Option<ImblMap<Hash32, Lovelace>>,
    pub current_epoch: Option<u64>,
    /// Set of registered DRep credentials, keyed by
    /// [`Credential::to_typed_hash32`].  Byte 28 of the key encodes the
    /// credential kind (`0x00` for `VerificationKey`, `0x01` for `Script`),
    /// mirroring Haskell's `Map (Credential 'DRepRole) DRepState` — a key
    /// credential and a script credential with the same 28-byte hash are
    /// treated as distinct DReps, matching the Haskell `KeyHashObj` /
    /// `ScriptHashObj` discrimination.
    pub registered_dreps: Option<Arc<HashSet<Hash32>>>,
    /// Per-DRep stored deposit amount, keyed identically to
    /// [`Self::registered_dreps`]. Required by the
    /// `ConwayDRepIncorrectRefund` predicate (Conway GOVCERT rule): an
    /// `UnregDRep` certificate carries an inline refund amount which must
    /// equal the deposit paid at registration time. When `None`, the
    /// refund-mismatch predicate is silently skipped (lenient default —
    /// matches the convention used by `stake_key_deposits`).
    pub drep_deposits: Option<Arc<HashMap<Hash32, u64>>>,
    pub registered_vrf_keys: Option<Arc<HashMap<Hash32, Hash28>>>,
    pub node_network: Option<NetworkId>,
    pub committee_members: Option<Arc<HashSet<Hash32>>>,
    pub committee_resigned: Option<Arc<HashSet<Hash32>>>,
    /// Per-credential stake key deposits.
    ///
    /// Uses `imbl::HashMap` for the same O(1) clone reason as `reward_accounts`.
    pub stake_key_deposits: Option<ImblMap<Hash32, u64>>,
    /// The constitution's guardrail script hash, if any.
    ///
    /// When `Some`, governance proposals of type `ParameterChange` or
    /// `TreasuryWithdrawals` must carry a matching `policy_hash`.  When `None`,
    /// the constitution policy-hash check is skipped.
    pub constitution_script_hash: Option<Hash28>,
    /// DRep vote delegations — keys are stake credential hashes of accounts
    /// that have delegated to any DRep (including AlwaysAbstain / AlwaysNoConfidence).
    pub vote_delegations: Option<Arc<HashSet<Hash32>>>,
    /// Map of currently active on-chain governance proposals, keyed by
    /// `GovActionId`.  When supplied, the validator uses this map to look up
    /// the [`ActiveProposal`] record for each `(voter, gov_action_id)` vote
    /// in `voting_procedures`, so that the `DisallowedVoters` predicate
    /// (Conway GOV) can reject votes whose voter type is not authorised for
    /// the action's type.  When `None`, only proposals submitted in the same
    /// transaction (`tx.body.proposal_procedures`) are checked.
    ///
    /// The value is an [`ActiveProposal`] (not a bare `GovAction`) because
    /// later GOV predicates (e.g. `VotingOnExpiredGovAction`,
    /// `ProposalReturnAccountDoesNotExist`) need the proposal's expiry
    /// epoch and return address, not just the action.
    pub active_proposals: Option<Arc<HashMap<GovActionId, ActiveProposal>>>,
    /// The last *enacted* governance action id per lineal purpose — Haskell's
    /// `Proposals.pRoots` (`GovRelation` of `prRoot`s).
    ///
    /// Required by the `InvalidPrevGovActionId` predicate to decide whether a
    /// proposal's `prev_action_id` chains correctly. `None` skips the predicate
    /// entirely (lenient default, matching `active_proposals`).
    ///
    /// Note these are the *enacted* roots, NOT the set of in-flight proposals —
    /// a proposal may also chain onto an active proposal, which is checked
    /// against [`Self::active_proposals`].
    pub enacted_gov_roots: Option<Arc<EnactedGovRoots>>,
    /// Hot credential hashes currently authorised by Constitutional Committee
    /// members (mirrors Haskell `authorizedHotCommitteeCredentials`).  Keys are
    /// stored as `credential.to_typed_hash32()` for symmetry with the
    /// other credential-keyed sets in this struct (`registered_dreps`,
    /// `committee_members`) — byte 28 disambiguates key (`0x00`) vs
    /// script (`0x01`) credentials, matching Haskell's
    /// `Credential 'HotCommitteeRole`.
    ///
    /// When `Some`, the `VotersDoNotExist` predicate (Conway GOV) rejects
    /// committee-voter votes whose hot credential is not in this set.  When
    /// `None`, the committee-hot-key membership check is skipped — i.e. a
    /// committee voter is treated as known.  This mirrors the lenient default
    /// used by `active_proposals`.
    pub committee_authorized_hot_keys: Option<Arc<HashSet<Hash32>>>,
    /// Subset of [`Self::committee_authorized_hot_keys`] whose backing cold
    /// credentials are in the **currently-enacted** committee
    /// (`committeeMembers electedCommittee` in Haskell).
    ///
    /// At PV >= 11, Conway's `UnelectedCommitteeVoters` predicate fires for
    /// any CC vote whose hot credential is in `committee_authorized_hot_keys`
    /// but NOT in this set — i.e. authorised by the registry but no longer
    /// backed by an elected cold credential.  When `None` the predicate is
    /// silently skipped (lenient default).
    ///
    /// Reference: Haskell `authorizedElectedHotCommitteeCredentials` in
    /// `eras/conway/impl/src/Cardano/Ledger/CertState.hs`.
    pub committee_authorized_elected_hot_keys: Option<Arc<HashSet<Hash32>>>,
    // ---------------------------------------------------------------------
    // MIR (Move Instantaneous Rewards) — Shelley–Babbage only.
    //
    // All four fields are optional so the lenient-default convention used
    // by every other context field (return false / no error when None) is
    // preserved.  See `validation/mir.rs` for the predicates.
    // ---------------------------------------------------------------------
    /// Treasury pot balance (in lovelace).  Used by the MIR predicates
    /// `InsufficientForInstantaneousRewards` /
    /// `InsufficientForTransferDELEG` when the source is `Treasury`.
    pub treasury: Option<Lovelace>,
    /// Reserves pot balance (in lovelace).  Used by the MIR predicates
    /// `InsufficientForInstantaneousRewards` /
    /// `InsufficientForTransferDELEG` when the source is `Reserves`.
    pub reserves: Option<Lovelace>,
    /// Number of slots per epoch (e.g. 432_000 for mainnet).  Required by
    /// the MIR `MIRCertificateTooLateInEpoch` predicate to compute the
    /// first slot of the next epoch.
    pub epoch_length: Option<u64>,
    /// Praos security parameter `k` (e.g. 2160 for mainnet, 432 for
    /// preview).  Used together with `active_slots_coeff` (already on
    /// `ProtocolParameters`) to compute `stabilityWindow = ceil(3k / f)`
    /// for the MIR `MIRCertificateTooLateInEpoch` predicate.
    pub security_param: Option<u64>,
    /// Snapshot of the per-credential accumulated MIR rewards for the
    /// current epoch (Haskell `dsIRewards`).  Used by the Alonzo+ MIR
    /// `MIRProducesNegativeUpdate` predicate to detect a delta that
    /// would push a recipient's balance below zero.
    ///
    /// Keys are credential hashes padded to `Hash32` (zero-extended) for
    /// symmetry with `reward_accounts` and `registered_dreps`.  When
    /// `None` the predicate is silently skipped (lenient default —
    /// callers without `dsIRewards` plumbing must accept this partial
    /// limitation).
    pub accumulated_mir_balances: Option<Arc<HashMap<Hash32, i64>>>,
    /// Set of currently registered genesis-delegate keys (`keysSet
    /// GenDelegs` in Haskell).  Used by the Shelley PPUP predicate
    /// `NonGenesisUpdatePPUP` to verify that every key in a pre-Conway
    /// `UpdateProposal.proposed_updates` map is a registered genesis
    /// delegate.
    ///
    /// In Haskell, `GenDelegs` maps `KeyHash 'Genesis -> (KeyHash
    /// 'GenesisDelegate, Hash VRF)`; here we only need the key set, so we
    /// store a `HashSet<Hash28>` of the genesis-key hashes.  When `None`,
    /// the `NonGenesisUpdatePPUP` predicate is silently skipped (lenient
    /// default — callers without genesis-delegate plumbing have no way to
    /// distinguish proposed-vs-genesis keys).
    ///
    /// Reference: Haskell `NonGenesisUpdatePPUP` in
    /// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`.
    pub genesis_delegates: Option<Arc<HashSet<Hash28>>>,
    /// Set of currently registered genesis-DELEGATE (hot/delegate) keys —
    /// `Map.elems (dsGenDelegs ds)`'s `genDelegKeyHash` values, i.e. the
    /// map's VALUES.  This is the OPPOSITE side from [`Self::genesis_delegates`]
    /// above (which holds the map's KEYS — the genesis/cold key hashes used
    /// by `NonGenesisUpdatePPUP`). Used by the Shelley UTXOW predicate
    /// `validateMIRInsufficientGenesisSigs` (#804): a transaction bearing a
    /// `MoveInstantaneousRewards` cert must be witnessed by at least
    /// `update_quorum` of these delegate keys.
    ///
    /// When `None`, `check_mir_genesis_quorum` is silently skipped (lenient
    /// default, matching [`Self::genesis_delegates`]'s convention).
    ///
    /// Reference: Haskell `validateMIRInsufficientGenesisSigs` in
    /// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxow.hs`.
    pub genesis_delegate_keys: Option<Arc<HashSet<Hash28>>>,
    /// Quorum of genesis-delegate signatures required on a MIR-bearing
    /// transaction (Haskell `Globals.quorum`, from genesis
    /// `sgUpdateQuorum` — mainnet value 5). Used together with
    /// [`Self::genesis_delegate_keys`] by `check_mir_genesis_quorum`.
    /// `None` skips the check (lenient default).
    pub update_quorum: Option<u64>,
}

impl ValidationContext {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Owned `with_X` builders ──────────────────────────────────────────
    //
    // These accept owned `HashSet` / `HashMap` values and wrap them in an
    // `Arc` internally.  They exist so the many call-sites in test code and
    // n2c handlers that build ad-hoc validation contexts from owned data do
    // not need to materialise Arcs themselves.  The hot path
    // (per-tx validation inside `state::apply::apply_block`) MUST use the
    // matching `with_X_arc` variants below, which take a pre-built `Arc`
    // so that per-tx context construction is O(1) reference-count bumps
    // instead of O(N) hash-table allocations + copies.
    //
    // Background: dugite previously rebuilt every registry set/map from
    // scratch and deep-cloned `reward_accounts` (already `Arc<HashMap>` on
    // the ledger state!) *for every transaction in every block*.  On
    // Babbage/Conway preview blocks with ~4 txs and registries holding
    // tens of thousands of reward accounts and DReps, that was ~400k
    // hash-table entries copied per block — 100-200 ms of pure memcpy,
    // dominating block apply throughput.

    pub fn with_pools(mut self, pools: HashSet<Hash28>) -> Self {
        self.registered_pools = Some(Arc::new(pools));
        self
    }

    pub fn with_pools_arc(mut self, pools: Arc<HashSet<Hash28>>) -> Self {
        self.registered_pools = Some(pools);
        self
    }

    pub fn with_treasury(mut self, treasury: u64) -> Self {
        self.current_treasury = Some(treasury);
        self
    }

    /// Set reward accounts from a std::HashMap (converts via collect — O(N)).
    /// Use `with_reward_accounts_imbl` for the O(1) imbl path.
    pub fn with_reward_accounts(mut self, accounts: HashMap<Hash32, Lovelace>) -> Self {
        self.reward_accounts = Some(accounts.into_iter().collect());
        self
    }

    /// Set reward accounts from an imbl::HashMap (O(1) clone).
    pub fn with_reward_accounts_imbl(mut self, accounts: ImblMap<Hash32, Lovelace>) -> Self {
        self.reward_accounts = Some(accounts);
        self
    }

    /// Legacy builder accepting Arc<HashMap>; converts to imbl (O(N)).
    /// Prefer `with_reward_accounts_imbl` for performance.
    #[allow(dead_code)]
    pub fn with_reward_accounts_arc(mut self, accounts: Arc<HashMap<Hash32, Lovelace>>) -> Self {
        self.reward_accounts = Some(accounts.iter().map(|(k, v)| (*k, *v)).collect());
        self
    }

    pub fn with_epoch(mut self, epoch: u64) -> Self {
        self.current_epoch = Some(epoch);
        self
    }

    pub fn with_dreps(mut self, dreps: HashSet<Hash32>) -> Self {
        self.registered_dreps = Some(Arc::new(dreps));
        self
    }

    pub fn with_dreps_arc(mut self, dreps: Arc<HashSet<Hash32>>) -> Self {
        self.registered_dreps = Some(dreps);
        self
    }

    pub fn with_vrf_keys(mut self, keys: HashMap<Hash32, Hash28>) -> Self {
        self.registered_vrf_keys = Some(Arc::new(keys));
        self
    }

    pub fn with_vrf_keys_arc(mut self, keys: Arc<HashMap<Hash32, Hash28>>) -> Self {
        self.registered_vrf_keys = Some(keys);
        self
    }

    pub fn with_network(mut self, network: NetworkId) -> Self {
        self.node_network = Some(network);
        self
    }

    pub fn with_committee_members(mut self, members: HashSet<Hash32>) -> Self {
        self.committee_members = Some(Arc::new(members));
        self
    }

    pub fn with_committee_members_arc(mut self, members: Arc<HashSet<Hash32>>) -> Self {
        self.committee_members = Some(members);
        self
    }

    pub fn with_committee_resigned(mut self, resigned: HashSet<Hash32>) -> Self {
        self.committee_resigned = Some(Arc::new(resigned));
        self
    }

    pub fn with_committee_resigned_arc(mut self, resigned: Arc<HashSet<Hash32>>) -> Self {
        self.committee_resigned = Some(resigned);
        self
    }

    /// Set stake_key_deposits from a std::HashMap (O(N) conversion).
    pub fn with_stake_key_deposits(mut self, deposits: HashMap<Hash32, u64>) -> Self {
        self.stake_key_deposits = Some(deposits.into_iter().collect());
        self
    }

    /// Set stake_key_deposits from an imbl::HashMap (O(1) clone).
    pub fn with_stake_key_deposits_imbl(mut self, deposits: ImblMap<Hash32, u64>) -> Self {
        self.stake_key_deposits = Some(deposits);
        self
    }

    /// Legacy builder accepting Arc<HashMap>; converts to imbl (O(N)).
    #[allow(dead_code)]
    pub fn with_stake_key_deposits_arc(mut self, deposits: Arc<HashMap<Hash32, u64>>) -> Self {
        self.stake_key_deposits = Some(deposits.iter().map(|(k, v)| (*k, *v)).collect());
        self
    }

    pub fn with_drep_deposits(mut self, deposits: HashMap<Hash32, u64>) -> Self {
        self.drep_deposits = Some(Arc::new(deposits));
        self
    }

    pub fn with_drep_deposits_arc(mut self, deposits: Arc<HashMap<Hash32, u64>>) -> Self {
        self.drep_deposits = Some(deposits);
        self
    }

    pub fn with_constitution_script_hash(mut self, hash: Hash28) -> Self {
        self.constitution_script_hash = Some(hash);
        self
    }

    pub fn with_vote_delegations(mut self, delegations: HashSet<Hash32>) -> Self {
        self.vote_delegations = Some(Arc::new(delegations));
        self
    }

    pub fn with_vote_delegations_arc(mut self, delegations: Arc<HashSet<Hash32>>) -> Self {
        self.vote_delegations = Some(delegations);
        self
    }

    pub fn with_active_proposals(
        mut self,
        proposals: HashMap<GovActionId, ActiveProposal>,
    ) -> Self {
        self.active_proposals = Some(Arc::new(proposals));
        self
    }

    pub fn with_active_proposals_arc(
        mut self,
        proposals: Arc<HashMap<GovActionId, ActiveProposal>>,
    ) -> Self {
        self.active_proposals = Some(proposals);
        self
    }

    /// Supply the enacted governance roots so the `InvalidPrevGovActionId`
    /// predicate runs. Without this the predicate is skipped entirely.
    pub fn with_enacted_gov_roots(mut self, roots: EnactedGovRoots) -> Self {
        self.enacted_gov_roots = Some(Arc::new(roots));
        self
    }

    /// `Arc` variant of [`Self::with_enacted_gov_roots`] for callers that
    /// snapshot the roots once per block.
    pub fn with_enacted_gov_roots_arc(mut self, roots: Arc<EnactedGovRoots>) -> Self {
        self.enacted_gov_roots = Some(roots);
        self
    }

    pub fn with_committee_authorized_hot_keys(mut self, hot_keys: HashSet<Hash32>) -> Self {
        self.committee_authorized_hot_keys = Some(Arc::new(hot_keys));
        self
    }

    pub fn with_committee_authorized_hot_keys_arc(
        mut self,
        hot_keys: Arc<HashSet<Hash32>>,
    ) -> Self {
        self.committee_authorized_hot_keys = Some(hot_keys);
        self
    }

    pub fn with_committee_authorized_elected_hot_keys(mut self, hot_keys: HashSet<Hash32>) -> Self {
        self.committee_authorized_elected_hot_keys = Some(Arc::new(hot_keys));
        self
    }

    pub fn with_committee_authorized_elected_hot_keys_arc(
        mut self,
        hot_keys: Arc<HashSet<Hash32>>,
    ) -> Self {
        self.committee_authorized_elected_hot_keys = Some(hot_keys);
        self
    }

    /// Set the Treasury and Reserves pot balances used by MIR predicates.
    pub fn with_pots(mut self, treasury: Lovelace, reserves: Lovelace) -> Self {
        self.treasury = Some(treasury);
        self.reserves = Some(reserves);
        self
    }

    /// Set the epoch length (slots per epoch) and Praos security parameter
    /// `k` used by the MIR `MIRCertificateTooLateInEpoch` predicate.
    pub fn with_epoch_geometry(mut self, epoch_length: u64, security_param: u64) -> Self {
        self.epoch_length = Some(epoch_length);
        self.security_param = Some(security_param);
        self
    }

    /// Set the per-credential accumulated MIR rewards snapshot used by
    /// the Alonzo+ MIR `MIRProducesNegativeUpdate` predicate.
    pub fn with_accumulated_mir_balances(mut self, balances: HashMap<Hash32, i64>) -> Self {
        self.accumulated_mir_balances = Some(Arc::new(balances));
        self
    }

    pub fn with_accumulated_mir_balances_arc(
        mut self,
        balances: Arc<HashMap<Hash32, i64>>,
    ) -> Self {
        self.accumulated_mir_balances = Some(balances);
        self
    }

    /// Populate `accumulated_mir_balances` from a [`crate::state::LedgerState`]
    /// snapshot.
    ///
    /// In Haskell, `dsIRewards` is the per-credential pending MIR delta map
    /// stored on `DState` for both Reserves and Treasury pots — it tracks
    /// MIR-cert deltas that have been *announced* but not yet credited (the
    /// drain happens at the epoch boundary).  Dugite does not yet maintain a
    /// separate pending-delta map: MIR distributions update
    /// `certs.reward_accounts` immediately on cert apply.
    ///
    /// This helper takes a *post-distribution* snapshot of `reward_accounts`
    /// and exposes it as `accumulated_mir_balances`, so the Alonzo+
    /// `MIRProducesNegativeUpdate` predicate can fire when a fresh negative
    /// delta would push a credential's recorded balance below zero.
    ///
    /// ## Bounded fidelity
    ///
    /// This is **not** a faithful Haskell `dsIRewards` reconstruction — it is
    /// the post-credit reward-accounts view, which is suitable for catching
    /// obvious negative updates in pre-Conway replay tests but **not** for
    /// exact byte-for-byte parity with Haskell on the Shelley–Babbage history.
    /// Use it for fixture-driven tests, not for live replay assertions that
    /// require strict parity.
    ///
    /// ## Mainnet impact: zero
    ///
    /// Mainnet has been Conway (PV ≥ 9) since September 2024.  MIR certs were
    /// removed at the era boundary; [`mir::validate_mir_cert`] short-circuits
    /// `Ok(())` for `pv >= 9`, so this helper is exercised only by pre-Conway
    /// fixtures and replay paths.  The Conway short-circuit here returns an
    /// empty accumulator, mirroring the live behaviour.
    pub fn with_accumulated_mir_balances_from_ledger(
        mut self,
        ledger: &crate::state::LedgerState,
    ) -> Self {
        // Conway+ has no MIR certs — accumulator is structurally empty.
        if ledger.epochs.protocol_params.protocol_version_major >= 9 {
            self.accumulated_mir_balances = Some(Arc::new(HashMap::new()));
            return self;
        }
        // Pre-Conway: snapshot reward_accounts as best-effort i64 deltas.
        // Lovelace is u64; clamp to i64::MAX on the (impossible-in-practice)
        // overflow so the predicate's signed arithmetic stays well-defined.
        let snapshot: HashMap<Hash32, i64> = ledger
            .certs
            .reward_accounts
            .iter()
            .map(|(cred_hash, lovelace)| {
                (*cred_hash, i64::try_from(lovelace.0).unwrap_or(i64::MAX))
            })
            .collect();
        self.accumulated_mir_balances = Some(Arc::new(snapshot));
        self
    }

    /// Set the set of registered genesis-delegate key hashes used by
    /// the Shelley PPUP `NonGenesisUpdatePPUP` predicate.
    pub fn with_genesis_delegates(mut self, keys: HashSet<Hash28>) -> Self {
        self.genesis_delegates = Some(Arc::new(keys));
        self
    }

    pub fn with_genesis_delegates_arc(mut self, keys: Arc<HashSet<Hash28>>) -> Self {
        self.genesis_delegates = Some(keys);
        self
    }

    /// Set the set of registered genesis-DELEGATE (hot/delegate) key hashes
    /// used by the Shelley UTXOW `validateMIRInsufficientGenesisSigs`
    /// predicate (#804). See [`ValidationContext::genesis_delegate_keys`]
    /// for the distinction from `genesis_delegates` above.
    pub fn with_genesis_delegate_keys(mut self, keys: HashSet<Hash28>) -> Self {
        self.genesis_delegate_keys = Some(Arc::new(keys));
        self
    }

    pub fn with_genesis_delegate_keys_arc(mut self, keys: Arc<HashSet<Hash28>>) -> Self {
        self.genesis_delegate_keys = Some(keys);
        self
    }

    /// Set the genesis-delegate signature quorum used by
    /// `check_mir_genesis_quorum` (#804).
    pub fn with_update_quorum(mut self, quorum: u64) -> Self {
        self.update_quorum = Some(quorum);
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_full_ledger_state(
        mut self,
        pools: HashSet<Hash28>,
        treasury: u64,
        accounts: HashMap<Hash32, Lovelace>,
        epoch: u64,
        dreps: HashSet<Hash32>,
        vrf_keys: HashMap<Hash32, Hash28>,
        network: NetworkId,
        committee_members: HashSet<Hash32>,
        committee_resigned: HashSet<Hash32>,
    ) -> Self {
        self.registered_pools = Some(Arc::new(pools));
        self.current_treasury = Some(treasury);
        self.reward_accounts = Some(accounts.into_iter().collect());
        self.current_epoch = Some(epoch);
        self.registered_dreps = Some(Arc::new(dreps));
        self.registered_vrf_keys = Some(Arc::new(vrf_keys));
        self.node_network = Some(network);
        self.committee_members = Some(Arc::new(committee_members));
        self.committee_resigned = Some(Arc::new(committee_resigned));
        self
    }
}

// ---------------------------------------------------------------------------
// Public error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("No inputs in transaction")]
    NoInputs,
    #[error("Input not found in UTxO set: {0}")]
    InputNotFound(String),
    #[error("Value not conserved: inputs={inputs}, outputs={outputs}, fee={fee}")]
    ValueNotConserved { inputs: u64, outputs: u64, fee: u64 },
    #[error("Fee too small: minimum={minimum}, actual={actual}")]
    FeeTooSmall { minimum: u64, actual: u64 },
    #[error("Output too small: minimum={minimum}, actual={actual}")]
    OutputTooSmall { minimum: u64, actual: u64 },
    #[error("Transaction too large: maximum={maximum}, actual={actual}")]
    TxTooLarge { maximum: u64, actual: u64 },
    #[error("Missing required signer: {0}")]
    MissingRequiredSigner(String),
    #[error("Missing witness for input: {0}")]
    MissingWitness(String),
    #[error("TTL expired: current_slot={current_slot}, ttl={ttl}")]
    TtlExpired { current_slot: u64, ttl: u64 },
    #[error("Transaction not yet valid: current_slot={current_slot}, valid_from={valid_from}")]
    NotYetValid { current_slot: u64, valid_from: u64 },
    #[error("Script validation failed: {0}")]
    ScriptFailed(String),
    #[error("Insufficient collateral")]
    InsufficientCollateral,
    #[error("Too many collateral inputs: max={max}, actual={actual}")]
    TooManyCollateralInputs { max: u64, actual: u64 },
    #[error("Collateral input not found in UTxO set: {0}")]
    CollateralNotFound(String),
    #[error("Collateral input contains tokens (must be pure ADA): {0}")]
    CollateralHasTokens(String),
    #[error("Collateral mismatch: total_collateral={declared}, effective={computed}")]
    CollateralMismatch { declared: u64, computed: u64 },
    #[error("Reference input not found in UTxO set: {0}")]
    ReferenceInputNotFound(String),
    #[error("Reference input overlaps with regular input: {0}")]
    ReferenceInputOverlapsInput(String),
    /// Phase-2 PlutusV3 `TxInfo` translation failure: `inputs ∩ reference_inputs`
    /// is non-empty.  Introduced by Haskell `cardano-ledger` PR #5011 at PV >= 11.
    /// Surfaces on the wire as a `BadTranslation` carrying
    /// `ConwayContextError::ReferenceInputsNotDisjointFromInputs` (CBOR tag 15).
    /// The payload lists the offending `TxIn`s in deterministic (sorted) order.
    #[error("PlutusV3 TxInfo translation: reference inputs not disjoint from inputs: {0:?}")]
    ReferenceInputsNotDisjointFromInputs(Vec<String>),
    #[error("Multi-asset not conserved for policy {policy}: inputs+mint={input_side}, outputs={output_side}")]
    MultiAssetNotConserved {
        policy: String,
        input_side: i128,
        output_side: i128,
    },
    #[error("Negative minting without policy script")]
    InvalidMint,
    #[error("Max execution units exceeded")]
    ExUnitsExceeded,
    #[error("Script data hash mismatch: expected {expected}, got {actual}")]
    ScriptDataHashMismatch { expected: String, actual: String },
    #[error("Script data hash present but no scripts or redeemers")]
    UnexpectedScriptDataHash,
    #[error("Missing script data hash (required when scripts/redeemers present)")]
    MissingScriptDataHash,
    #[error("Duplicate input in transaction: {0}")]
    DuplicateInput(String),
    #[error("Native script validation failed")]
    NativeScriptFailed,
    #[error("Witness signature verification failed for vkey: {0}")]
    InvalidWitnessSignature(String),
    #[error("Output address network mismatch: expected {expected:?}, got {actual:?}")]
    NetworkMismatch {
        expected: dugite_primitives::network::NetworkId,
        actual: dugite_primitives::network::NetworkId,
    },
    #[error("Auxiliary data hash declared but no auxiliary data present")]
    AuxiliaryDataHashWithoutData,
    #[error("Auxiliary data present but no auxiliary data hash in tx body")]
    AuxiliaryDataWithoutHash,
    #[error("Block execution units exceeded: {resource} limit={limit}, total={total}")]
    BlockExUnitsExceeded {
        resource: String,
        limit: u64,
        total: u64,
    },
    #[error("Output value too large: maximum={maximum}, actual={actual}")]
    OutputValueTooLarge { maximum: u64, actual: u64 },
    #[error("Plutus transaction missing raw CBOR for script evaluation")]
    MissingRawCbor,
    #[error("Plutus transaction missing slot configuration for script evaluation")]
    MissingSlotConfig,
    /// The tx body `is_valid` flag does not match the Phase-2 Plutus evaluation
    /// result.
    ///
    /// Mirrors Haskell `Cardano.Ledger.Conway.Rules.Utxos` predicate
    /// `ValidationTagMismatch`: a tx that declares `is_valid=false` but whose
    /// scripts all evaluate to `True` would allow the producer to steal
    /// collateral without a legitimate script failure (DoS vector).  Conversely,
    /// declaring `is_valid=true` while scripts actually fail would produce an
    /// invalid block.
    ///
    /// Cardano mempool (`applyTx`) enforces this at admission time; we mirror
    /// that check in `validate_transaction_with_context` so BPs never admit
    /// tag-mismatched txs.
    #[error("is_valid tag mismatch: declared={declared}, evaluated={evaluated}")]
    IsValidTagMismatch { declared: bool, evaluated: bool },
    /// Phase-2 **collection/context** error — the script context could not be
    /// built or its inputs collected: UTxO/cost-model decode failure, missing
    /// script or datum, or validity-interval time translation past the
    /// safe-zone horizon (`TimeTranslationPastHorizon`).
    ///
    /// Mirrors Haskell `UtxosFailure (CollectErrors …)` (Babbage/Conway),
    /// raised by `collectTwoPhaseScriptInputs` BEFORE script evaluation and
    /// rejecting the transaction **regardless** of the `is_valid` tag.
    /// Unlike `ScriptFailed`, this never legitimises `is_valid = false`
    /// (#733/#734).
    #[error("phase-2 collection error (UtxosFailure CollectErrors): {0}")]
    Phase2CollectError(String),
    /// The dugite CEK panicked (caught by `catch_unwind`). Rejects at
    /// ADMISSION (reject-by-default on adversarial input) but is NOT a
    /// Haskell CollectError: at block apply it must stay warn-and-trust —
    /// a Haskell-validated chain can contain scripts that panic dugite's
    /// evaluator (#733 correction 3).
    #[error("phase-2 evaluator panic: {0}")]
    Phase2EvalPanic(String),
    #[error("Script-locked input at index {index} has no matching Spend redeemer")]
    MissingSpendRedeemer { index: u32 },
    /// A script-locked withdrawal or Plutus minting policy has no matching
    /// redeemer of the required tag/index.
    ///
    /// Mirrors Haskell's `scriptsNeeded` check: every entry in the `Reward`
    /// and `Mint` buckets that corresponds to a Plutus script must have an
    /// explicit redeemer at the correct sorted position.
    #[error("Missing {tag} redeemer at index {index}")]
    MissingRedeemer { tag: String, index: u32 },
    #[error("Redeemer index out of range: tag={tag}, index={index}, max={max}")]
    RedeemerIndexOutOfRange { tag: String, index: u32, max: usize },
    #[error("Missing VKey witness for input credential: {0}")]
    MissingInputWitness(String),
    #[error("Missing script witness for script-locked input: {0}")]
    MissingScriptWitness(String),
    #[error("Missing VKey witness for withdrawal credential: {0}")]
    MissingWithdrawalWitness(String),
    #[error("Missing script witness for script-locked withdrawal: {0}")]
    MissingWithdrawalScriptWitness(String),
    #[error("Missing VKey witness for certificate credential: {0}")]
    MissingCertificateWitness(String),
    #[error("Missing script witness for script-credential certificate: {0}")]
    MissingCertificateScriptWitness(String),
    #[error("Value overflow in transaction accounting")]
    ValueOverflow,
    #[error("Era gating violation: {certificate_type} requires {required_era}, current era is {current_era}")]
    EraGatingViolation {
        certificate_type: String,
        required_era: String,
        current_era: String,
    },
    #[error("Governance feature requires Conway era (protocol >= 9), current protocol version is {current_version}")]
    GovernancePreConway { current_version: u64 },
    /// Conway LEDGERS rule: the block producer's declared treasury value in the
    /// transaction body (`currentTreasuryValue`, field 19) must match the
    /// ledger's tracked treasury balance exactly.
    ///
    /// Reference: Cardano Blueprint `LEDGERS` flowchart, "submittedTreasuryValue
    /// == currentTreasuryValue" predicate.
    #[error("Treasury value mismatch: tx declared {declared}, ledger has {actual}")]
    TreasuryValueMismatch { declared: u64, actual: u64 },
    /// Conway LEDGERS rule: the `CommitteeHotAuth` certificate's cold credential
    /// must correspond to a member currently elected to the constitutional
    /// committee (`committee_expiration` map).  Authorising a hot key for an
    /// unrecognised cold credential is rejected ("failOnNonEmpty unelected").
    ///
    /// Reference: Cardano ledger `conwayWitsVKeyNeeded` / `CERT` rule,
    /// "ccHotKeyOK" predicate from the Haskell implementation.
    #[error("CommitteeHotAuth cold credential is not a current CC member: {cold_credential_hash}")]
    UnelectedCommitteeMember { cold_credential_hash: String },
    /// Conway LEDGERS rule: the `CommitteeHotAuth` certificate's cold credential
    /// belongs to a committee member that has previously resigned via
    /// `CommitteeColdResign`.  Resigned members may not re-authorise hot keys
    /// until they are re-elected (the Haskell `CERT` rule predicate
    /// "membersResigned ∩ {coldKey} = ∅").
    ///
    /// Reference: Haskell `ConwayCommitteeHasPreviouslyResigned` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Cert`.
    #[error(
        "CommitteeHotAuth rejected: cold credential {cold_credential_hash} has previously \
         resigned (ConwayCommitteeHasPreviouslyResigned)"
    )]
    CommitteeHasPreviouslyResigned { cold_credential_hash: String },
    /// Alonzo/Conway Phase-1 rule: a script-locked spending input carries a
    /// `DatumHash` in its UTxO but no corresponding datum bytes were supplied
    /// in `tx.witness_set.plutus_data`.
    ///
    /// Per Haskell's `checkWitnessesShelley` / Alonzo `UTXOW` rule
    /// "witsVKeyNeeded" extended with "reqSignerHashes" — every non-inline
    /// datum referenced by a script-locked input MUST be provided as a witness.
    #[error("Missing datum witness for script-locked input: datum hash {0}")]
    MissingDatumWitness(String),
    /// Alonzo/Conway Phase-1 rule: a datum supplied in
    /// `tx.witness_set.plutus_data` is not needed by any script-locked input
    /// or referenced output, making the transaction malformed.
    ///
    /// Haskell rejects transactions with extraneous datums under the
    /// `UTXOW` predicate "allowedSupplementalDatums ⊇ suppliedDatums".
    #[error("Extra (unreferenced) datum witness in transaction: datum hash {0}")]
    ExtraDatumWitness(String),
    /// Alonzo UTXO rule: a script-locked spending input has no datum
    /// (OutputDatum::None) and the locking script is PlutusV1 or PlutusV2.
    /// PlutusV3 inputs are exempt per CIP-0069.
    ///
    /// Reference: Haskell `UnspendableUTxONoDatumHash` in
    /// `cardano-ledger-alonzo:Cardano.Ledger.Alonzo.Rules.Utxo`.
    #[error(
        "Script-locked input {input} has no datum (NoDatum) and locking script is {language} \
         (UnspendableUTxONoDatumHash — PlutusV3 exempt per CIP-0069)"
    )]
    UnspendableUTxONoDatumHash { input: String, language: String },
    /// Conway LEDGER rule (PV ≥ 10): a KeyHash reward account making a
    /// withdrawal must have an active DRep delegation (any delegation value
    /// including AlwaysAbstain/AlwaysNoConfidence satisfies this).
    ///
    /// Reference: Haskell `ConwayWdrlNotDelegatedToDRep` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Ledger`.
    #[error(
        "Withdrawal rejected: KeyHash reward account {credential_hash} has no DRep delegation \
         (ConwayWdrlNotDelegatedToDRep, requires PV >= 10)"
    )]
    WdrlNotDelegatedToDRep { credential_hash: String },
    /// Conway GOV rule: a `ParameterChange` proposal's `PParamsUpdate` is
    /// malformed — one or more fields fail the `ppuWellFormed` check.
    ///
    /// Reference: Haskell `MalformedProposal` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Gov`.
    #[error("Governance proposal rejected: malformed PParamsUpdate ({reason})")]
    MalformedProposal { reason: String },
    /// Conway GOV rule: a voter is not authorised to vote on this governance
    /// action type.
    ///
    /// Reference: Haskell `DisallowedVoters` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`.
    /// The voter × action authority matrix:
    ///   - `NoConfidence`: SPO yes, DRep yes, CC NO
    ///   - `UpdateCommittee`: SPO yes, DRep yes, CC NO
    ///   - `NewConstitution`: SPO NO, DRep yes, CC yes
    ///   - `HardForkInitiation`: SPO yes, DRep yes, CC yes
    ///   - `ParameterChange`: SPO only when SecurityGroup params, DRep yes, CC yes
    ///   - `TreasuryWithdrawals`: SPO NO, DRep yes, CC yes
    ///   - `InfoAction`: all yes (NoVotingThreshold)
    ///
    /// The payload aggregates **every** disallowed `(voter, gov_action_id)`
    /// pair in the transaction into a single error (mirroring Haskell's
    /// `NonEmpty` predicate-failure shape).
    #[error("DisallowedVoters: {violations:?}")]
    DisallowedVoters {
        violations: Vec<(Voter, GovActionId)>,
    },
    /// Conway bootstrap-phase (PV9) voting restriction.
    ///
    /// Per Haskell `checkBootstrapVotes` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
    /// lines 378-391:
    /// - DRepVoter may only vote on `InfoAction`.
    /// - CommitteeVoter / StakePoolVoter may only vote on
    ///   `ParameterChange`, `HardForkInitiation`, or `InfoAction`.
    ///
    /// Fires only when `pvMajor < 10`
    /// (`hardforkConwayBootstrapPhase`).
    #[error("DisallowedVotesDuringBootstrap: {violations:?}")]
    DisallowedVotesDuringBootstrap {
        violations: Vec<(Voter, GovActionId)>,
    },
    /// Conway GOV rule: every TreasuryWithdrawals destination address
    /// must be a registered staking credential.
    ///
    /// Per Haskell `processProposal` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
    /// lines 509-519. The lookup table is `DState.accountsL` (mapped
    /// here to `ValidationContext::reward_accounts`).
    #[error("TreasuryWithdrawalReturnAccountsDoNotExist: {bad_addrs:?}")]
    TreasuryWithdrawalReturnAccountsDoNotExist { bad_addrs: Vec<String> },
    /// Allegra+ Shelley UTXOW: a metadata leaf (`Bytes` or `Text`) at any
    /// depth exceeds 64 bytes. Haskell enforces this at CBOR-decode time
    /// (`decodeMetadatum` in `Cardano.Ledger.Metadata`), and the
    /// `InvalidMetadata` predicate is dead code on the validation side.
    /// dugite's decoder accepts arbitrary sizes, so we mirror Haskell's
    /// constraint at the validation step. PV >= 3 means Allegra+
    /// (decoder version > 2).
    ///
    /// Reference: Haskell `decodeMetadatum` in
    /// `libs/cardano-ledger-core/src/Cardano/Ledger/Metadata.hs`.
    #[error("InvalidMetadata (oversize leaf at labels {labels:?}; max 64 bytes per Bytes/Text)")]
    InvalidMetadata { labels: Vec<u64> },
    /// Conway GOV rule: one or more voters in the transaction's
    /// `voting_procedures` are not registered / authorised, and therefore
    /// cannot vote on any governance action:
    ///   - `DRepVoter` whose credential is not in `vsDReps`.
    ///   - `StakePoolVoter` whose pool ID is not in `psStakePools`.
    ///   - `CommitteeVoter` whose hot credential is not in
    ///     `authorizedHotCommitteeCredentials`.
    ///
    /// This predicate fires **before** [`ValidationError::DisallowedVoters`]
    /// (Haskell `internVoter` partitions unknown voters out of the voting set
    /// before the authority matrix is applied), so a single voter is never
    /// reported under both predicates.
    ///
    /// Reference: Haskell `VotersDoNotExist` /
    /// `internVoter` in `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`.
    /// All unknown voters are aggregated into a single predicate failure
    /// (mirroring Haskell's `NonEmpty` shape).
    #[error("VotersDoNotExist: {voters:?}")]
    VotersDoNotExist { voters: Vec<Voter> },
    /// Conway GOV rule: one or more votes in the transaction's
    /// `voting_procedures` reference a `GovActionId` that does not exist
    /// in the active-proposal set (either it was never proposed, already
    /// ratified+enacted, expired, or dropped at the prior epoch boundary).
    ///
    /// This is the "votes for ratified/expired/never-proposed actions"
    /// guard.  Without it dugite's mempool admits — and forge picks up —
    /// votes whose action has just been removed from the proposals
    /// registry at an epoch-boundary tick.  Haskell rejects the
    /// resulting block with the same `GovActionsDoNotExist` predicate,
    /// causing chain-divergence stalls between dugite forgers and
    /// cardano-node validators.
    ///
    /// Same-tx proposals are exempt: a vote inside a transaction that
    /// also carries the proposal procedure for the referenced id is
    /// valid by definition (the action enters the registry as part of
    /// the same apply step).
    ///
    /// Reference: Haskell `GovActionsDoNotExist` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`
    /// (`checkGovActionsExist`).
    #[error("GovActionsDoNotExist: {action_ids:?}")]
    GovActionsDoNotExist { action_ids: Vec<GovActionId> },
    /// Conway GOV rule: a `ProposalProcedure` whose `prev_action_id` does not
    /// chain correctly onto its governance purpose.
    ///
    /// A lineal-purpose action (ParameterChange, HardForkInitiation,
    /// NoConfidence, UpdateCommittee, NewConstitution) is admissible only when
    /// either
    ///   (a) `prev_action_id = None` AND that purpose has no enacted root, or
    ///   (b) `prev_action_id = Some(id)` AND `id` is that purpose's enacted
    ///       root OR an active in-flight proposal.
    /// TreasuryWithdrawals and InfoAction have no lineage requirement.
    ///
    /// This must FAIL THE TRANSACTION, not drop the proposal. Haskell's GOV
    /// rule is explicit (`Cardano.Ledger.Conway.Rules.Gov`):
    ///
    /// ```haskell
    /// case proposalsAddAction actionState proposals of
    ///   Just updatedProposals -> pure updatedProposals
    ///   Nothing -> proposals <$ failBecause (injectFailure $ InvalidPrevGovActionId proposal)
    /// ```
    ///
    /// `failBecause` registers a predicate failure, so the tx — and any block
    /// carrying it — is invalid. dugite previously only logged and dropped the
    /// proposal at apply time, on the mistaken assumption that Haskell dropped
    /// it too. The result was a P0 consensus divergence: dugite's forge
    /// admitted such a tx, minted it, and cardano-node rejected the block with
    /// `ConwayGovFailure (InvalidPrevGovActionId …)` and issued `ShutdownPeer`,
    /// splitting the chain. Observed on the local devnet at slot 1870 when a
    /// ParameterChange with `prev_action_id = None` was proposed after another
    /// ParameterChange had already been enacted.
    ///
    /// Skipped when `enacted_gov_roots` is `None` (the same lenient default
    /// used by `active_proposals` and `committee_authorized_hot_keys`).
    ///
    /// Reference: Haskell `InvalidPrevGovActionId` (GOV predicate tag 8) in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`.
    ///
    /// `proposal` carries the full offending `ProposalProcedure` — Haskell's
    /// payload for this predicate is the ENTIRE proposal, not just its
    /// lineage fields, so the N2C rejection encoder
    /// (`dugite-network::local_tx_submission::encode`) needs the whole
    /// value to emit a byte-exact `ConwayGovPredFailure` tag-8 frame instead
    /// of falling back to a generic rejection reason (dugite issue #915).
    /// Boxed because `ProposalProcedure` (which itself boxes
    /// `ProtocolParamUpdate` for `GovAction::ParameterChange`) would
    /// otherwise make this the largest `ValidationError` variant by far,
    /// bloating every `Vec<ValidationError>` on the hot rejection path.
    #[error("InvalidPrevGovActionId: proposal index {action_index} ({action_type})")]
    InvalidPrevGovActionId {
        action_index: u32,
        action_type: &'static str,
        prev_action_id: Option<GovActionId>,
        proposal: Box<ProposalProcedure>,
    },
    /// Conway GOV rule (PV >= 11 only): one or more `ConstitutionalCommittee`
    /// votes carry a hot credential whose backing cold credential is NOT in
    /// the currently-enacted committee.  At PV <= 10 this predicate is
    /// silenced — only `VotersDoNotExist` fires for unauthorised CC voters.
    ///
    /// Haskell defines this as: a CC vote is rejected when its hot credential
    /// is not in `authorizedElectedHotCommitteeCredentials = csCommitteeCreds
    /// ∩ keys (committeeMembers electedCommittee)`.  At PV >= 11 BOTH
    /// `UnelectedCommitteeVoters` AND `VotersDoNotExist` fire for the same
    /// voter (Haskell `GovSpec.hs:757-758`).
    ///
    /// All offending hot credentials are aggregated into a single
    /// `NonEmpty`-shaped error.
    ///
    /// Reference: Haskell `unelectedCommitteeVoters` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs` L649-L661.
    #[error("UnelectedCommitteeVoters: {hot_keys:?}")]
    UnelectedCommitteeVoters { hot_keys: Vec<Hash32> },
    /// A voter is voting against a governance action whose `expires_after_epoch`
    /// is strictly less than the current epoch.
    ///
    /// Reference: Haskell `VotingOnExpiredGovAction` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
    /// function `checkVotesAreNotForExpiredActions`. Vote is allowed
    /// when `current_epoch <= gasExpiresAfter` (boundary inclusive).
    ///
    /// This predicate is silently skipped if `ValidationContext::active_proposals`
    /// is `None` (lenient default for callers that don't yet plumb in the
    /// active-proposal map).
    ///
    /// Tuple shape: `(voter, gov_action_id, expires_after_epoch, current_epoch)`.
    #[error("VotingOnExpiredGovAction: {expired_votes:?}")]
    VotingOnExpiredGovAction {
        expired_votes: Vec<(Voter, GovActionId, u64, u64)>,
    },
    /// Conway GOV rule: one or more proposal procedures have a return address
    /// whose stake credential is not registered in the reward-accounts map.
    ///
    /// Per Haskell `processProposal` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`, every proposal
    /// procedure's `pProcReturnAddr` credential must be present in the on-chain
    /// `accounts` map (i.e. the stake credential is currently registered) so the
    /// proposal deposit can be refunded at expiry/enactment.  The check is
    /// **skipped during Conway bootstrap** (`pvMajor == 9`) per
    /// `hardforkConwayBootstrapPhase`, and runs from PV ≥ 10 onwards.
    ///
    /// This predicate is silently skipped if `ValidationContext::reward_accounts`
    /// is `None` (lenient default for callers that haven't plumbed in the
    /// reward-accounts state — same convention used by the other GOV predicates).
    ///
    /// Every offending proposal's raw `return_addr` (hex-encoded) is aggregated
    /// into a single predicate failure, mirroring Haskell's `NonEmpty`
    /// predicate-failure shape.
    #[error("ProposalReturnAccountDoesNotExist: {bad_addrs:?}")]
    ProposalReturnAccountDoesNotExist {
        /// Hex-encoded raw `return_addr` bytes (header + 28-byte credential)
        /// for every proposal whose return-address credential is unregistered.
        bad_addrs: Vec<String>,
    },
    /// Conway GOV rule: one or more proposal procedures have a return-address
    /// network id that does not match the node's configured network.
    ///
    /// Per Haskell `processProposal` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`, every
    /// proposal procedure's `pProcReturnAddr` must be on the same network as
    /// the node.  Bit 0 of the reward-account header byte encodes the network
    /// (`0` = testnet, `1` = mainnet).  Unlike
    /// [`ValidationError::ProposalReturnAccountDoesNotExist`], this check is
    /// **always enforced** — there is no Conway-bootstrap skip; the network
    /// id is a structural property of the proposal payload, not a
    /// post-bootstrap state lookup.
    ///
    /// This predicate is silently skipped if `ValidationContext::node_network`
    /// is `None` (lenient default for callers that haven't plumbed in the
    /// node network — same convention used by the other GOV predicates).
    ///
    /// Every offending proposal's raw `return_addr` (hex-encoded) and the
    /// actual mismatched network id (`0` testnet / `1` mainnet) are
    /// aggregated into a single predicate failure, mirroring Haskell's
    /// `NonEmpty` predicate-failure shape.
    #[error("ProposalProcedureNetworkIdMismatch: expected={expected}, mismatched={mismatched:?}")]
    ProposalProcedureNetworkIdMismatch {
        /// Expected network id (`0` testnet / `1` mainnet) — the node's
        /// configured network.
        expected: u8,
        /// `(hex-encoded return_addr, actual_network_id)` for every proposal
        /// whose return-address network does not match `expected`.
        mismatched: Vec<(String, u8)>,
    },
    /// Conway GOV rule: one or more `TreasuryWithdrawals` proposals carry a
    /// destination reward-address whose network id does not match the node's
    /// configured network.
    ///
    /// Per Haskell `processProposal` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`
    /// (`TreasuryWithdrawals` branch), every key in the withdrawals map is a
    /// reward address whose network id (bit 0 of the header byte;
    /// `0` = testnet, `1` = mainnet) must match the node's network.  Like
    /// [`ValidationError::ProposalProcedureNetworkIdMismatch`], this check is
    /// **always enforced** — there is no Conway-bootstrap skip; the network
    /// id is a structural property of the proposal payload.
    ///
    /// This predicate is silently skipped if `ValidationContext::node_network`
    /// is `None` (lenient default for callers that haven't plumbed in the
    /// node network — same convention used by the other GOV predicates).
    ///
    /// All mismatched destinations across all `TreasuryWithdrawals` proposals
    /// in the transaction are aggregated into a single predicate failure,
    /// mirroring Haskell's `NonEmpty` predicate-failure shape.
    #[error(
        "TreasuryWithdrawalsNetworkIdMismatch: expected={expected}, mismatched={mismatched:?}"
    )]
    TreasuryWithdrawalsNetworkIdMismatch {
        /// Expected network id (`0` testnet / `1` mainnet) — the node's
        /// configured network.
        expected: u8,
        /// `(hex-encoded reward_addr, actual_network_id)` for every TW
        /// destination address whose network id does not match `expected`.
        mismatched: Vec<(String, u8)>,
    },
    /// Conway GOV rule: one or more `TreasuryWithdrawals` proposals carry a
    /// total amount of zero (including the all-zero-entries case).
    ///
    /// Per Haskell `processProposal` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`
    /// (`TreasuryWithdrawals` branch), the sum of every withdrawal entry's
    /// `Coin` must be strictly positive — degenerate zero-sum proposals are
    /// rejected.
    ///
    /// This check is **skipped during Conway bootstrap** (`pvMajor == 9`)
    /// per `hardforkConwayBootstrapPhase`; it activates from PV ≥ 10.
    ///
    /// Every offending proposal is identified by a string descriptor
    /// (currently the proposal's hex-encoded `return_addr` to keep the
    /// payload stable) — the Haskell side aggregates the full `GovAction`
    /// payloads, but a list of identifiers is sufficient for diagnostics.
    /// All offending proposals across the transaction aggregate into a
    /// single predicate failure, mirroring Haskell's `NonEmpty`
    /// predicate-failure shape.
    #[error("ZeroTreasuryWithdrawals: {offending_proposals:?}")]
    ZeroTreasuryWithdrawals {
        /// Hex-encoded `return_addr` (or other stable identifier) of every
        /// offending TreasuryWithdrawals proposal in the transaction.
        offending_proposals: Vec<String>,
    },
    /// Conway GOV rule: one or more `UpdateCommittee` proposals whose
    /// add-set keys intersect the remove-set — the proposal both adds and
    /// removes the same Constitutional Committee credential.
    ///
    /// Per Haskell `processProposal` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`
    /// (`UpdateCommittee` branch):
    ///
    /// ```haskell
    /// let conflicting = Set.intersection (Map.keysSet membersToAdd) membersToRemove
    /// in unless (Set.null conflicting) (failBecause $ ConflictingCommitteeUpdate conflicting)
    /// ```
    ///
    /// This check is **always enforced** — there is no Conway-bootstrap
    /// skip; the conflict is a structural property of the action payload.
    ///
    /// Conflicting credentials across all `UpdateCommittee` proposals in
    /// the transaction are aggregated into a single predicate failure.
    /// Each entry is the typed-hash32 hex (byte 28 = `0x01` for scripts,
    /// `0x00` for keys) so callers can distinguish key- from script-
    /// credential conflicts — matching Haskell's `Credential` type.
    #[error("ConflictingCommitteeUpdate: {conflicts:?}")]
    ConflictingCommitteeUpdate {
        /// Hex-encoded typed-hash32 of every conflicting credential
        /// across all UpdateCommittee proposals in the transaction.
        conflicts: Vec<String>,
    },
    /// Conway GOV rule: one or more new members in an `UpdateCommittee`
    /// proposal carry a `validUntil` epoch that is not strictly greater
    /// than the current epoch — the member would expire on or before
    /// taking office.
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
    /// This check is **always enforced** — there is no Conway-bootstrap
    /// skip; the expiry-vs-current-epoch comparison is a structural
    /// property of the proposal payload combined with the live epoch.
    ///
    /// This predicate is silently skipped if `ValidationContext::current_epoch`
    /// is `None` (lenient default for callers that have not plumbed in
    /// epoch context — same convention used by other epoch-dependent GOV
    /// predicates).
    ///
    /// Every offending `(credential, validUntil)` pair across all
    /// `UpdateCommittee` proposals in the transaction is aggregated into
    /// a single predicate failure, mirroring Haskell's `NonEmpty`
    /// predicate-failure shape.  Each credential is the typed-hash32 hex
    /// (byte 28 = `0x01` for scripts, `0x00` for keys).
    #[error("ExpirationEpochTooSmall: {invalid_members:?}")]
    ExpirationEpochTooSmall {
        /// `(typed-hash32 hex of credential, bad validUntil epoch)` for
        /// every offending new member across all UpdateCommittee
        /// proposals in the transaction.
        invalid_members: Vec<(String, u64)>,
    },
    /// Alonzo UTXOW rule: a redeemer in the witness set has no matching
    /// script purpose (spending input, minting policy, withdrawal, cert, vote).
    ///
    /// Reference: Haskell `ExtraRedeemers` in
    /// `cardano-ledger-alonzo:Cardano.Ledger.Alonzo.Rules.Utxow`.
    #[error("Extra redeemer with no matching script purpose: tag={tag}, index={index}")]
    ExtraRedeemer { tag: String, index: u32 },
    /// Alonzo UTXO rule: collateral inputs must be at VKey (non-script)
    /// addresses. Script-locked UTxOs cannot serve as collateral.
    /// Byron/bootstrap addresses are accepted as collateral.
    ///
    /// Reference: Haskell `ScriptsNotPaidUTxO` in
    /// `cardano-ledger-alonzo:Cardano.Ledger.Alonzo.Rules.Utxo`.
    #[error("Collateral input(s) at script-locked addresses (ScriptsNotPaidUTxO): {inputs:?}")]
    ScriptLockedCollateral { inputs: Vec<String> },
    /// Babbage/Conway UTXOW rule: one or more scripts in the transaction
    /// witness set are not needed by any script purpose. Reference scripts
    /// do not count as "needed" for the witness check.
    ///
    /// Reference: Haskell `ExtraneousScriptWitnessesUTXOW` in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Utxow`.
    #[error("Extraneous script witness(es) not needed by transaction: {hashes:?}")]
    ExtraneousScriptWitness { hashes: Vec<String> },
    /// Babbage+ UTXOW rule: one or more scripts in the witness set are
    /// malformed for the current protocol version. Either the script's
    /// bytes do not decode (failed `decodePlutusRunnable`) or the script
    /// language is not yet supported at the current PV (e.g. PlutusV3 at
    /// PV < 9).
    ///
    /// Per Haskell `validScript` (`eras/alonzo/impl/.../Scripts.hs`, line
    /// 650):
    ///
    /// ```haskell
    /// validScript pv script =
    ///   case toPlutusScript script of
    ///     Just plutusScript -> isValidPlutusScript (pvMajor pv) plutusScript
    ///     Nothing -> case getNativeScript script of
    ///       Just timelockScript -> deepseq timelockScript True
    ///       Nothing -> error "Impossible"
    /// ```
    ///
    /// Reference: Haskell `MalformedScriptWitnesses` in
    /// `cardano-ledger-babbage:Cardano.Ledger.Babbage.Rules.Utxow`, line 260.
    #[error("Malformed script witness(es) (MalformedScriptWitnesses): {hashes:?}")]
    MalformedScriptWitnesses { hashes: Vec<String> },
    /// Babbage+ UTXOW rule: one or more reference scripts attached to an
    /// output PRODUCED by this transaction (`tx.body.outputs[].script_ref`
    /// or `tx.body.collateral_return.script_ref`) are malformed for the
    /// current protocol version. Same `validScript` predicate as
    /// `MalformedScriptWitnesses` — only the source of scripts differs.
    ///
    /// Reference: Haskell `MalformedReferenceScripts` in
    /// `cardano-ledger-babbage:Cardano.Ledger.Babbage.Rules.Utxow`, line 261.
    #[error("Malformed reference script(s) on tx outputs (MalformedReferenceScripts): {hashes:?}")]
    MalformedReferenceScripts { hashes: Vec<String> },
    /// Conway rule: the total byte size of all reference scripts reachable
    /// from a single transaction's inputs and reference inputs must not exceed
    /// 200 KiB (`ppMaxRefScriptSizePerTxG`).
    ///
    /// Source: Haskell `ppMaxRefScriptSizePerTxG = L.to . const $ 200 * 1024`
    /// (Conway PParams). This is hardcoded, not a governance-updateable parameter.
    #[error(
        "Transaction reference script size {actual} exceeds per-transaction limit \
         {limit} bytes (Conway ppMaxRefScriptSizePerTxG)"
    )]
    TxRefScriptSizeTooLarge { actual: u64, limit: u64 },
    /// Pool retirement epoch exceeds `current_epoch + e_max`.
    ///
    /// Per Haskell's POOL rule (Shelley spec, Figure 14): "The pool's announced
    /// retirement epoch must satisfy `e <= cepoch + emax`."
    #[error(
        "Pool retirement epoch {retirement_epoch} exceeds maximum allowed \
         {max_epoch} (current_epoch={current_epoch} + e_max={e_max})"
    )]
    PoolRetirementTooLate {
        retirement_epoch: u64,
        current_epoch: u64,
        e_max: u64,
        max_epoch: u64,
    },
    /// Pool retirement epoch is not strictly in the future.
    ///
    /// Per Haskell's POOL rule
    /// (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs`, lines
    /// 308-323), the retirement epoch must satisfy `currentEpoch < e`
    /// (STRICT lower bound). A retirement scheduled for the current epoch
    /// or earlier fires the first `Mismatch` arm of
    /// `StakePoolRetirementWrongEpochPOOL`.
    #[error(
        "Pool retirement epoch {retirement_epoch} must be strictly greater than \
         current_epoch={current_epoch} (StakePoolRetirementWrongEpochPOOL, RelGT arm)"
    )]
    PoolRetirementTooEarly {
        retirement_epoch: u64,
        current_epoch: u64,
    },
    /// Pool registration's reward account is on the wrong network.
    ///
    /// Per Haskell `ShelleyPoolPredFailure::WrongNetworkPOOL` in
    /// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs`,
    /// gated on `hardforkAlonzoValidatePoolAccountAddressNetID pv`
    /// (PV >= 5). The network ID encoded in the pool's reward (account)
    /// address must match the node's network ID.
    #[error(
        "Pool {pool_id} reward account on wrong network: expected {expected:?}, \
         got {actual:?} (WrongNetworkPOOL)"
    )]
    WrongNetworkPool {
        expected: dugite_primitives::network::NetworkId,
        actual: dugite_primitives::network::NetworkId,
        pool_id: String,
    },
    /// Conway `ConwayStakeRegistration` deposit does not match protocol parameter
    /// `key_deposit`.
    ///
    /// Per Haskell's Conway `DELEG` rule: "The deposit amount declared in the
    /// certificate must equal the current `keyDeposit` protocol parameter."
    #[error(
        "Conway stake registration deposit mismatch: declared={declared}, \
         expected key_deposit={expected}"
    )]
    StakeRegistrationDepositMismatch { declared: u64, expected: u64 },
    /// Retained for the N2C error-mapping API (`node/serve.rs`) but no longer
    /// constructed: cardano-ledger has no zero-amount withdrawal predicate in
    /// any era — a zero withdrawal of a registered zero-balance account is valid
    /// (`isSubmapOfUM` accepts `0 == 0`).
    #[error("Zero withdrawal amount for reward account: {account}")]
    ZeroWithdrawal { account: String },
    /// Combined CERTS-rule withdrawal failure (PV ≤ 10).
    ///
    /// Reference: Haskell `WithdrawalsNotInRewardsCERTS` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Certs.hs`.
    /// Bundles missing accounts (unregistered or wrong-network) AND
    /// incomplete withdrawals (amount ≠ balance) per the helper
    /// `withdrawalsThatDoNotDrainAccounts`. Active in Conway prior to
    /// the move-checks-to-LEDGER-rule hard fork (PV ≤ 10).
    #[error("WithdrawalsNotInRewardsCERTS: {bad:?}")]
    WithdrawalsNotInRewardsCERTS {
        /// `(addr_hex, supplied_amount)` pairs for every withdrawal whose
        /// reward account is missing OR whose amount mismatches the balance.
        bad: Vec<(String, u64)>,
    },
    /// Withdrawal references an unregistered or wrong-network reward account
    /// (PV ≥ 11).
    ///
    /// Reference: Haskell `ConwayWithdrawalsMissingAccounts` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs`,
    /// active after `hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule`.
    #[error("ConwayWithdrawalsMissingAccounts: {missing:?}")]
    ConwayWithdrawalsMissingAccounts {
        /// `(addr_hex, supplied_amount)` per missing-account withdrawal.
        missing: Vec<(String, u64)>,
    },
    /// Withdrawal amount does not match the registered account balance
    /// (PV ≥ 11).
    ///
    /// Reference: Haskell `ConwayIncompleteWithdrawals` in
    /// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs`.
    #[error("ConwayIncompleteWithdrawals: {incomplete:?}")]
    ConwayIncompleteWithdrawals {
        /// `(addr_hex, supplied_amount, expected_balance)` per mismatched
        /// withdrawal.
        incomplete: Vec<(String, u64, u64)>,
    },
    /// Haskell `StakeKeyHasNonZeroAccountBalanceDELEG`: a stake deregistration
    /// is rejected when the reward account holds a non-zero balance.
    ///
    /// Per the Cardano ledger spec (Shelley DELEG rule and Conway DELEG rule),
    /// deregistering a stake credential with a non-empty reward account is
    /// invalid — the delegator must first withdraw all rewards before
    /// deregistering. This prevents silent loss of on-chain rewards.
    ///
    /// Reference: Haskell `StakeKeyHasNonZeroAccountBalanceDELEG` predicate in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg`.
    #[error(
        "Stake deregistration rejected: reward account {credential_hash} has non-zero balance \
         ({balance} lovelace) — withdraw rewards before deregistering"
    )]
    StakeKeyHasNonZeroBalance {
        /// Hex-encoded credential hash (zero-padded to 32 bytes).
        credential_hash: String,
        /// Current reward balance in lovelace.
        balance: u64,
    },
    /// Conway `UnRegCert` (tag 8) declared refund does not match the current
    /// `key_deposit` protocol parameter.
    ///
    /// Per Haskell's Conway DELEG rule: the deposit amount carried in
    /// `ConwayStakeDeregistration` must equal the `keyDeposit` currently in
    /// effect. A mismatch means the transaction was constructed with stale
    /// protocol parameters and must be rejected.
    #[error(
        "Conway stake deregistration refund mismatch: declared={declared}, \
         expected key_deposit={expected}"
    )]
    StakeDeregistrationRefundMismatch { declared: u64, expected: u64 },
    /// Haskell `StakeKeyRegisteredDELEG`: a stake registration certificate
    /// names a credential that is already registered in the ledger.
    ///
    /// Both legacy `StakeRegistration` (tag 0) and Conway
    /// `ConwayStakeRegistration` (tag 7) are covered — Haskell enforces the
    /// same predicate for both certificate variants.
    ///
    /// Reference: Haskell `StakeKeyRegisteredDELEG` in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg`.
    #[error(
        "Stake registration rejected: credential {credential_hash} is already registered \
         (StakeKeyRegisteredDELEG)"
    )]
    StakeKeyAlreadyRegistered {
        /// Hex-encoded credential hash (zero-padded to 32 bytes).
        credential_hash: String,
    },
    /// Haskell `StakeKeyNotRegisteredDELEG` (dereg polarity): a stake
    /// DEREGISTRATION certificate names a credential that is not registered
    /// at that point in the left-to-right cert walk (neither pre-tx nor by a
    /// prior cert in the same tx).
    ///
    /// Both legacy `StakeDeregistration` (tag 1) and Conway
    /// `ConwayStakeDeregistration` (tag 8) are covered. Without this check a
    /// dugite BP could forge a Haskell-invalid block, and the apply path
    /// would refund a key deposit that was never paid (deposits-pot drift).
    ///
    /// Reference: Haskell `StakeKeyNotRegisteredDELEG` in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg`
    /// (ShelleyUnRegCert / ConwayUnRegCert predicate). dugite #748.
    #[error(
        "Stake deregistration rejected: credential {credential_hash} is not registered \
         (StakeKeyNotRegisteredDELEG)"
    )]
    StakeKeyNotRegisteredForDeregistration {
        /// Hex-encoded credential hash (zero-padded to 32 bytes).
        credential_hash: String,
    },
    /// Haskell `DelegateeStakePoolNotRegisteredDELEG`: a stake delegation
    /// certificate names a pool ID that is not currently registered.
    ///
    /// Covers all delegation certificate variants: `StakeDelegation` (tag 2),
    /// `RegStakeDeleg` (tag 11), `StakeVoteDelegation` (tag 13),
    /// `RegStakeVoteDeleg` (tag 14).
    ///
    /// Reference: Haskell `DelegateeStakePoolNotRegisteredDELEG` predicate in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg`.
    #[error(
        "Stake delegation rejected: target pool {pool_id} is not registered \
         (DelegateeStakePoolNotRegisteredDELEG)"
    )]
    DelegateePoolNotRegistered {
        /// Hex-encoded pool ID (Hash28).
        pool_id: String,
    },
    /// Haskell `DelegateeDRepNotRegisteredDELEG`: a vote-delegation certificate
    /// names a specific DRep credential that is not currently registered in
    /// `dsUnified` / `vState.dsDReps`.
    ///
    /// Covers all delegation certificate variants that delegate to a specific
    /// DRep (not AlwaysAbstain or AlwaysNoConfidence):
    ///   - `VoteDelegation`     (tag 9 )
    ///   - `StakeVoteDelegation`(tag 13)
    ///   - `RegStakeVoteDeleg`  (tag 14)
    ///   - `VoteRegDeleg`       (tag 15)
    ///
    /// `AlwaysAbstain` and `AlwaysNoConfidence` DRep targets are exempt —
    /// they are built-in synthetic DReps with no registry entry.
    ///
    /// Reference: Haskell `DelegateeDRepNotRegisteredDELEG` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Deleg`.
    #[error(
        "Vote delegation rejected: target DRep {drep_id} is not registered \
         (DelegateeDRepNotRegisteredDELEG)"
    )]
    DelegateeDRepNotRegistered {
        /// Hex-encoded DRep credential hash (typed-hash32, zero-padded to 32 bytes).
        drep_id: String,
    },
    /// Haskell `StakeKeyNotRegisteredDELEG`: a delegation certificate references
    /// a stake credential that is not registered in the ledger's reward-accounts
    /// map (`dsUnified`).
    ///
    /// Covers delegation certificate variants that require the stake credential
    /// to be registered BEFORE the cert is processed:
    ///   - `StakeDelegation`     (tag 2 ) — pure pool delegation
    ///   - `VoteDelegation`      (tag 9 ) — pure DRep delegation
    ///   - `StakeVoteDelegation` (tag 13) — pool + DRep delegation
    ///
    /// The combined registration+delegation certs (`RegStakeDeleg` tag 11,
    /// `RegStakeVoteDeleg` tag 14, `VoteRegDeleg` tag 15) are EXEMPT because
    /// they register the credential atomically as part of the same cert — they
    /// fire `StakeKeyRegisteredDELEG` if the key is ALREADY registered.
    ///
    /// Reference: Haskell `StakeKeyNotRegisteredDELEG` in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg`.
    #[error(
        "Delegation rejected: stake credential {credential_hash} is not registered \
         (StakeKeyNotRegisteredDELEG)"
    )]
    StakeKeyNotRegisteredForDelegation {
        /// Hex-encoded credential hash (typed-hash32, zero-padded to 32 bytes).
        credential_hash: String,
    },
    /// Haskell `ConwayDRepAlreadyRegistered`: a `RegDRep` certificate names a
    /// DRep credential that is already present in the DRep registry.
    ///
    /// Reference: Haskell `ConwayDRepAlreadyRegistered` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Deleg`.
    #[error(
        "DRep registration rejected: credential {credential_hash} is already registered \
         (ConwayDRepAlreadyRegistered)"
    )]
    DRepAlreadyRegistered {
        /// Hex-encoded DRep credential hash (zero-padded to 32 bytes).
        credential_hash: String,
    },
    /// Haskell `ConwayDRepIncorrectDeposit`: a `RegDRep` certificate declares a
    /// deposit amount that does not match the current `drep_deposit` protocol
    /// parameter.
    ///
    /// Reference: Haskell `ConwayDRepIncorrectDeposit` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.GovCert`.
    #[error(
        "DRep registration rejected: declared deposit {declared} does not match \
         drep_deposit parameter {expected} (ConwayDRepIncorrectDeposit)"
    )]
    DRepIncorrectDeposit {
        /// Deposit amount declared in the `RegDRep` certificate.
        declared: u64,
        /// Expected deposit from `drep_deposit` protocol parameter.
        expected: u64,
    },
    /// Haskell `ConwayDRepNotRegistered`: an `UnregDRep` certificate names a
    /// DRep credential that is not present in the DRep registry.
    ///
    /// Without this check a transaction can credit a DRep deposit refund for a
    /// credential that never registered — effectively minting ADA from nothing.
    ///
    /// Reference: Haskell `ConwayDRepNotRegistered` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.GovCert`.
    #[error(
        "DRep unregistration rejected: credential {credential_hash} is not registered \
         (ConwayDRepNotRegistered)"
    )]
    DRepNotRegistered {
        /// Hex-encoded DRep credential hash (zero-padded to 32 bytes).
        credential_hash: String,
    },
    /// Haskell `ConwayDRepIncorrectRefund`: an `UnregDRep` certificate
    /// carries a refund amount that does not match the deposit stored at
    /// registration time.
    ///
    /// Per Haskell `conwayGovCertTransition` (Conway GOVCERT rule):
    ///
    /// ```haskell
    /// ConwayUnRegDRep cred refund -> do
    ///   let mDRepState = Map.lookup cred (certState ^. certVStateL . vsDRepsL)
    ///       drepRefundMismatch = do
    ///         drepState <- mDRepState
    ///         let paidDeposit = drepState ^. drepDepositL
    ///         guard (refund /= paidDeposit)
    ///         pure paidDeposit
    ///   isJust mDRepState ?! (injectFailure . ConwayDRepNotRegistered) cred
    ///   failOnJust drepRefundMismatch $ injectFailure . ConwayDRepIncorrectRefund . Mismatch refund
    /// ```
    ///
    /// `paidDeposit = drepState ^. drepDepositL` — the compact coin stored
    /// when the DRep registered. Fires when `refund /= paidDeposit`.
    ///
    /// Reference: Haskell `ConwayDRepIncorrectRefund` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.GovCert`.
    #[error(
        "DRep unregistration rejected: declared refund {declared} does not match \
         stored deposit {expected} for credential {credential_hash} \
         (ConwayDRepIncorrectRefund)"
    )]
    DRepIncorrectRefund {
        /// Hex-encoded DRep credential hash (zero-padded to 32 bytes).
        credential_hash: String,
        /// Refund amount declared in the `UnregDRep` certificate.
        declared: u64,
        /// Expected refund (deposit paid at registration time).
        expected: u64,
    },
    /// Haskell `ProposalDepositIncorrect`: a governance proposal declares a
    /// deposit amount that does not match the current `gov_action_deposit`
    /// protocol parameter.
    ///
    /// Reference: Haskell `ProposalDepositIncorrect` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Gov`.
    #[error(
        "Governance proposal rejected: declared deposit {declared} does not match \
         gov_action_deposit parameter {expected} (ProposalDepositIncorrect)"
    )]
    ProposalDepositIncorrect {
        /// Deposit amount declared in the `ProposalProcedure`.
        declared: u64,
        /// Expected deposit from `gov_action_deposit` protocol parameter.
        expected: u64,
    },
    /// Conway+ POOL rule: a `PoolRegistration` certificate uses a VRF key hash
    /// that is already registered to a different pool.
    ///
    /// Enforced only when `protocol_version_major >= 9` (Conway). In earlier
    /// eras, multiple pools sharing a VRF key is theoretically possible (though
    /// inadvisable). From Conway onward, Haskell rejects duplicate VRF keys to
    /// prevent ambiguity in the VRF-based leader election.
    ///
    /// Reference: Haskell `VRFKeyHashAlreadyRegistered` in
    /// `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Pool`.
    #[error(
        "Pool registration rejected: VRF key {vrf_keyhash} is already registered to pool \
         {existing_pool_id} (VRFKeyHashAlreadyRegistered)"
    )]
    VrfKeyHashAlreadyRegistered {
        /// Hex-encoded VRF key hash (32 bytes).
        vrf_keyhash: String,
        /// Hex-encoded pool ID that currently holds the VRF key.
        existing_pool_id: String,
    },
    /// Shelley+ POOL rule: pool registration cost is below the minimum pool cost
    /// (`minPoolCost` / `min_pool_cost`) from the protocol parameters.
    ///
    /// Per Haskell's POOL rule (Shelley spec, Figure 14): "The declared pool cost
    /// must satisfy `poolCost >= minPoolCost`." This prevents pools from declaring
    /// artificially low costs to attract delegators at the expense of network
    /// sustainability.
    ///
    /// Reference: Haskell `StakePoolCostTooLowPOOL` in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Pool`.
    #[error(
        "Pool registration rejected: cost {actual} is below minimum pool cost {minimum} \
         (StakePoolCostTooLowPOOL)"
    )]
    StakePoolCostTooLow {
        /// Declared pool cost in lovelace.
        actual: u64,
        /// `minPoolCost` protocol parameter in lovelace.
        minimum: u64,
    },
    /// POOL rule: pool registration margin must be a valid rational in `[0, 1]`.
    ///
    /// Haskell's POOL rule enforces `0 ≤ margin ≤ 1` via `PoolMarginsInvalidPOOL`.
    /// Two conditions are rejected:
    ///   * `denominator == 0` — division by zero; would panic in reward calculation.
    ///   * `numerator > denominator` — margin > 100%; pool takes more than all rewards.
    ///
    /// The `u64` wire type already prevents negative numerator/denominator.
    ///
    /// Reference: Haskell `PoolMarginsInvalidPOOL` in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Pool`.
    #[error(
        "Pool registration rejected: margin {numerator}/{denominator} is not in [0,1] \
         (PoolMarginsInvalidPOOL)"
    )]
    PoolMarginInvalid {
        /// Declared margin numerator.
        numerator: u64,
        /// Declared margin denominator.
        denominator: u64,
    },
    /// Alonzo+ POOL rule: pool registration reward account network must match the
    /// network ID declared in the transaction body.
    ///
    /// When a transaction body carries a `network_id` field (Alonzo+), every pool
    /// registration certificate's reward account must be on the same network.
    /// Mixing networks (e.g., a testnet reward account in a mainnet transaction)
    /// is rejected as `WrongNetworkInTxBody`.
    ///
    /// Reference: Haskell `WrongNetworkInTxBody` in
    /// `cardano-ledger-alonzo:Cardano.Ledger.Alonzo.Rules.Utxo`.
    #[error(
        "Pool registration rejected: reward account network {actual:?} does not match \
         transaction network {expected:?} (WrongNetworkInTxBody)"
    )]
    PoolRewardAccountWrongNetwork {
        expected: dugite_primitives::network::NetworkId,
        actual: dugite_primitives::network::NetworkId,
        /// Hex-encoded 28-byte operator key hash of the offending pool.
        ///
        /// Carried because Haskell's counterpart is `WrongNetworkPOOL
        /// (Mismatch Network) (KeyHash StakePool)` — the pool id is part of
        /// the wire shape, so an error without it cannot be encoded (#979).
        pool_id: String,
    },
    /// Pool registration: reward account has wrong length.
    ///
    /// A pool reward account must be exactly 29 bytes: 1 header byte followed by
    /// a 28-byte credential hash (Blake2b-224). Any other length is rejected
    /// by Haskell's `checkPoolParams` which deserialises the address strictly.
    ///
    /// Finding D8 of security audit #544.
    #[error("Invalid pool reward account: {0}")]
    InvalidRewardAccount(String),
    /// Auxiliary data hash content mismatch.
    ///
    /// When both `auxiliary_data_hash` and `auxiliary_data` are present in a
    /// transaction, the declared hash must equal `blake2b_256(raw_aux_data_cbor)`.
    /// This check ensures the auxiliary data has not been altered after signing.
    ///
    /// Reference: Haskell `AuxiliaryDataHash` predicate in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Utxow`.
    #[error(
        "Auxiliary data hash mismatch: declared hash does not match blake2b_256 of aux data bytes \
         (AuxDataHashMismatch)"
    )]
    AuxiliaryDataHashMismatch {
        /// Hex-encoded auxiliary-data hash declared in the transaction body
        /// (Haskell `mismatchSupplied`).
        declared: String,
        /// Hex-encoded hash actually computed over the auxiliary data
        /// (Haskell `mismatchExpected`).
        computed: String,
    },
    /// Output address network does not match the node's configured network.
    ///
    /// Every transaction output address must be on the same network as the node.
    /// This is an unconditional check (unlike Rule 5b which only fires when the
    /// tx body carries a `network_id` field).
    ///
    /// Reference: Haskell `WrongNetwork` in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Utxo`.
    #[error(
        "Output address network {actual:?} does not match node network {expected:?} \
         (WrongNetworkInOutput)"
    )]
    WrongNetworkInOutput {
        expected: dugite_primitives::network::NetworkId,
        actual: dugite_primitives::network::NetworkId,
        /// Hex-encoded raw address bytes of EVERY offending output.
        ///
        /// Haskell's `WrongNetwork Network (Set Addr)` reports the whole set,
        /// not the first offender — so this collects them all (#979).
        addresses: Vec<String>,
    },
    /// Withdrawal reward address network does not match the node's configured network.
    ///
    /// Every withdrawal reward address must be on the same network as the node.
    ///
    /// Reference: Haskell `WrongNetworkWithdrawal` in
    /// `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Utxow`.
    #[error(
        "Withdrawal reward address network {actual:?} does not match node network {expected:?} \
         (WrongNetworkWithdrawal)"
    )]
    WrongNetworkWithdrawal {
        expected: dugite_primitives::network::NetworkId,
        actual: dugite_primitives::network::NetworkId,
        /// Hex-encoded reward-account bytes of EVERY offending withdrawal.
        ///
        /// Haskell's `WrongNetworkWithdrawal Network (Set RewardAccount)`
        /// reports the whole set (#979).
        accounts: Vec<String>,
    },
    /// Conway GOV rule: a `ParameterChange` or `TreasuryWithdrawals` proposal's
    /// `policy_hash` does not match the constitution's guardrail script hash.
    ///
    /// When the constitution carries a guardrail script, every governed proposal
    /// must include a `policy_hash` that equals the constitution's script hash.
    /// A mismatch or omission prevents the guardrail from being executed during
    /// Phase-2, bypassing the constitutionality check.
    ///
    /// Reference: Haskell `ConwayGovFailure` predicate —
    /// `GovActionsDoNotExist` / policy-hash mismatch in the GOV rule.
    #[error(
        "Governance proposal policy_hash mismatch: constitution requires {expected}, \
         proposal has {actual} (ConstitutionPolicyMismatch)"
    )]
    ConstitutionPolicyMismatch {
        /// Hex-encoded expected constitution script hash.
        expected: String,
        /// Hex-encoded provided policy hash, or "None" if absent.
        actual: String,
    },
    /// Pool metadata hash exceeds the 32-byte (Blake2b-256) cap.
    ///
    /// Reference: Haskell `PoolMedataHashTooBig` in
    /// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs`:
    ///
    /// ```haskell
    /// when (SoftForks.restrictPoolMetadataHash pv) $
    ///   forM_ sppMetadata $ \pmd ->
    ///     let s = sizeofByteArray $ pmHash pmd
    ///      in s <= fromIntegral (hashSize ([] @HASH))
    ///           ?! injectFailure (PoolMedataHashTooBig sppId s)
    /// ```
    ///
    /// Active since Alonzo (`pvMajor > 4`) per
    /// `SoftForks.restrictPoolMetadataHash`. `HASH = Blake2b_256`, so the
    /// cap is 32 bytes.
    ///
    /// In dugite, `PoolMetadata.hash` is structurally a `Hash32` (fixed
    /// 32 bytes), so this predicate is defensive against future
    /// wire-decode paths that might surface oversized values via a
    /// byte-slice route.
    #[error("PoolMedataHashTooBig: pool={pool}, hash_size={hash_size}")]
    PoolMedataHashTooBig {
        /// Hex-encoded 28-byte pool operator key hash.
        pool: String,
        /// Reported metadata hash size in bytes (> 32).
        hash_size: usize,
    },
    /// One or more transaction outputs use a Byron/bootstrap address whose
    /// serialized attributes exceed the 64-byte cap.
    ///
    /// Reference: Haskell `OutputBootAddrAttrsTooBig` /
    /// `validateOutputBootAddrAttrsTooBig` in
    /// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxo.hs`:
    ///
    /// ```text
    /// ∀ ( _ ↦ (a,_)) ∈ txoutstxb, a ∈ Addrbootstrap → bootstrapAttrsSize a ≤ 64
    /// ```
    ///
    /// Applies to all outputs in all eras Shelley+. Every offending output
    /// in the transaction aggregates into a single predicate failure with
    /// its zero-based index, mirroring Haskell's aggregation.
    #[error("OutputBootAddrAttrsTooBig: {oversized_outputs:?}")]
    OutputBootAddrAttrsTooBig {
        /// Zero-based output indices for every Byron/bootstrap output
        /// whose serialized attributes exceed 64 bytes.
        oversized_outputs: Vec<usize>,
    },
    // ---------------------------------------------------------------------
    // MIR (Move Instantaneous Rewards) predicate failures.
    //
    // Reference: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`.
    //
    // MIR certificates exist only in Shelley–Babbage (`AtMostEra "Babbage"`).
    // Conway has removed `MIRCert` entirely. All MIR predicates short-circuit
    // (no-op) at PV >= 9 (Conway). Several sub-rules are further gated by
    // `hardforkAlonzoAllowMIRTransfer = pvMajor pv > 4`.
    // ---------------------------------------------------------------------
    /// Shelley DELEG rule `MIRCertificateTooLateInEpoch`: MIR certificates
    /// submitted within `stabilityWindow` slots of the next epoch boundary
    /// are rejected — applying them mid-window risks the rewards landing in
    /// an epoch the proposer didn't intend.
    ///
    /// `tooLate = firstSlotOfNextEpoch - stabilityWindow` and
    /// `stabilityWindow = ceil(3 * k / f)` where `k = securityParam` and
    /// `f = activeSlotCoeff`.  Reject when `currentSlot >= tooLate`
    /// (boundary inclusive — Haskell uses `>=`).
    ///
    /// Reference: Haskell `checkSlotNotTooLate` in `Shelley.Rules.Deleg`.
    #[error("MIRCertificateTooLateInEpoch: current_slot={current_slot}, deadline={deadline}")]
    MIRCertificateTooLateInEpoch {
        /// The transaction's current slot.
        current_slot: u64,
        /// `firstSlotOfNextEpoch - stabilityWindow` — the earliest slot
        /// at which an MIR cert is too late.
        deadline: u64,
    },
    /// Shelley DELEG rule `InsufficientForInstantaneousRewards`: the sum of
    /// all delta values in a `StakeAddressesMIR` certificate exceeds the
    /// available pot balance (Reserves or Treasury).
    ///
    /// In Alonzo+ Haskell uses `availableAfterMIR` which considers existing
    /// `dsIRewards` accumulated during the same epoch; dugite uses the
    /// simpler `sum(deltas) > pot_balance` check (documented limitation —
    /// see `validation/mir.rs` doc comment).
    ///
    /// Reference: Haskell `checkStakeAddressesMIR` /
    /// `availableAfterMIR` in `Shelley.Rules.Deleg`.
    #[error(
        "InsufficientForInstantaneousRewards: pot={pot:?}, required={required}, \
         available={available}"
    )]
    InsufficientForInstantaneousRewards {
        /// The MIR source pot.
        pot: dugite_primitives::transaction::MIRSource,
        /// Sum of delta values requested.
        required: u64,
        /// Pot balance available.
        available: u64,
    },
    /// Shelley DELEG rule `MIRTransferNotCurrentlyAllowed`: pre-Alonzo
    /// (`pvMajor <= 4`), `OtherAccountingPot` (pot-to-pot transfer) MIR
    /// certificates are not allowed.  Only `StakeCredentials` distributions
    /// are accepted before Alonzo enables `hardforkAlonzoAllowMIRTransfer`.
    ///
    /// Reference: Haskell `checkSendToOppositePotMIR` (pre-Alonzo branch)
    /// in `Shelley.Rules.Deleg`.
    #[error("MIRTransferNotCurrentlyAllowed (pre-Alonzo MIR pot-to-pot transfer)")]
    MIRTransferNotCurrentlyAllowed,
    /// Shelley DELEG rule `MIRNegativesNotCurrentlyAllowed`: pre-Alonzo
    /// (`pvMajor <= 4`), every delta in a `StakeAddressesMIR` certificate
    /// must be non-negative.  Negative deltas (claw-back) are only allowed
    /// from Alonzo onward (`hardforkAlonzoAllowMIRTransfer`).
    ///
    /// Reference: Haskell `checkStakeAddressesMIR` (pre-Alonzo branch)
    /// in `Shelley.Rules.Deleg`.
    #[error("MIRNegativesNotCurrentlyAllowed (pre-Alonzo negative MIR delta)")]
    MIRNegativesNotCurrentlyAllowed,
    /// Shelley DELEG rule `MIRProducesNegativeUpdate`: in Alonzo+, after
    /// aggregating an MIR cert's deltas with the existing `dsIRewards`
    /// accumulator, no recipient may end up with a negative balance.
    ///
    /// Dugite implements this conservatively when an
    /// `accumulated_mir_balances` snapshot is supplied by the caller; when
    /// the snapshot is `None`, the check is silently skipped (documented
    /// limitation — full simulation requires `dsIRewards` plumbing).
    ///
    /// Reference: Haskell `checkStakeAddressesMIR` (Alonzo+ branch) in
    /// `Shelley.Rules.Deleg`.
    #[error("MIRProducesNegativeUpdate: credentials={credentials:?}")]
    MIRProducesNegativeUpdate {
        /// Hex-encoded credential hashes whose accumulated balance would
        /// become negative if the MIR cert were applied.
        credentials: Vec<String>,
    },
    /// Shelley DELEG rule `InsufficientForTransferDELEG`: in Alonzo+, an
    /// `OtherAccountingPot(coin)` transfer requests more lovelace than the
    /// source pot currently holds.
    ///
    /// Reference: Haskell `checkSendToOppositePotMIR` (Alonzo+ branch) in
    /// `Shelley.Rules.Deleg`.
    #[error(
        "InsufficientForTransferDELEG: pot={pot:?}, requested={requested}, \
         available={available}"
    )]
    InsufficientForTransferDELEG {
        /// The MIR source pot.
        pot: dugite_primitives::transaction::MIRSource,
        /// Lovelace requested in the transfer.
        requested: u64,
        /// Pot balance available.
        available: u64,
    },
    /// Shelley DELEG rule `MIRNegativeTransfer`: in Alonzo+, the `coin`
    /// field of an `OtherAccountingPot` transfer must be non-negative.
    ///
    /// In dugite, `MIRTarget::OtherAccountingPot(u64)` is structurally
    /// non-negative, so this predicate is unreachable via the public type
    /// system but kept for parity-completeness with Haskell's
    /// `DeltaCoin`-typed payload (which can encode negatives at the
    /// CBOR level on alternate decode paths).
    ///
    /// Reference: Haskell `checkSendToOppositePotMIR` (Alonzo+ branch) in
    /// `Shelley.Rules.Deleg`.
    #[error("MIRNegativeTransfer: pot={pot:?}, amount={amount}")]
    MIRNegativeTransfer {
        /// The MIR source pot.
        pot: dugite_primitives::transaction::MIRSource,
        /// Negative transfer amount.
        amount: i64,
    },
    /// Shelley UTXOW rule `MIRInsufficientGenesisSigsUTXOW`: a transaction
    /// containing at least one `MoveInstantaneousRewards` certificate must
    /// carry VKey witnesses for at least `update_quorum` of the CURRENT
    /// genesis-delegate keys (`Map.elems (dsGenDelegs ds)`'s `genDelegKeyHash`
    /// values — the delegate/hot keys, NOT the genesis/cold keys used by
    /// `NonGenesisUpdatePPUP`).
    ///
    /// Active in Shelley–Babbage (`AtMostEra "Babbage" era`); structurally
    /// impossible in Conway (MIR certs removed, `isInstantaneousRewards` is
    /// type-constrained to `AtMostEra "Babbage"`).
    ///
    /// Skipped silently (lenient default, matching every other predicate in
    /// this module) when [`ValidationContext::genesis_delegate_keys`] or
    /// [`ValidationContext::update_quorum`] is `None` — callers that have
    /// not plumbed in the genesis-delegate value-set have no way to
    /// evaluate the quorum.
    ///
    /// Reference: Haskell `validateMIRInsufficientGenesisSigs` in
    /// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxow.hs`.
    #[error(
        "MIRInsufficientGenesisSigsUTXOW: present={present}, required={required}, \
         signers={signers:?}"
    )]
    MIRInsufficientGenesisSigs {
        /// Number of distinct genesis-delegate (hot) keys that witnessed
        /// this transaction.
        present: usize,
        /// The quorum required (`update_quorum`, a genesis constant).
        required: u64,
        /// Hex-encoded genesis-delegate keys that DID witness the
        /// transaction (for diagnostic visibility).
        signers: Vec<String>,
    },
    // ---------------------------------------------------------------------
    // PPUP (pre-Conway protocol-parameter update) predicate failures.
    //
    // Reference: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`.
    //
    // PPUP is active in Shelley–Babbage (`AtMostEra "Babbage" era`).  Conway
    // replaces this rule with on-chain governance (CIP-1694).  All three
    // PPUP predicates short-circuit (no-op) at PV >= 9.
    // ---------------------------------------------------------------------
    /// Shelley PPUP rule `NonGenesisUpdatePPUP`: every key in the proposed
    /// update map must be a registered genesis-delegate key (i.e. a member
    /// of `keysSet GenDelegs`).
    ///
    /// Reference: Haskell `NonGenesisUpdatePPUP` in
    /// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`.
    /// Active in Shelley–Babbage (`AtMostEra "Babbage" era`); Conway+
    /// replaces this rule with on-chain governance (CIP-1694).
    ///
    /// Skipped silently (lenient default) when
    /// [`ValidationContext::genesis_delegates`] is `None` — callers that
    /// haven't plumbed in the genesis-delegate set have no way to tell
    /// proposed-vs-genesis apart.
    #[error(
        "NonGenesisUpdatePPUP: proposed_keys not subset of genesis_delegates \
         ({proposed:?} ∉ {genesis:?})"
    )]
    NonGenesisUpdatePPUP {
        /// Hex-encoded proposed update keys that are not registered genesis
        /// delegates.
        proposed: Vec<String>,
        /// Hex-encoded set of currently registered genesis delegate keys
        /// (for diagnostic visibility).
        genesis: Vec<String>,
    },
    /// Shelley PPUP rule `PPUpdateWrongEpoch`: an update proposal targets an
    /// epoch that is incompatible with the current slot's "voting period".
    ///
    /// `tooLate = firstSlotOfNextEpoch - 2 * stabilityWindow`.
    /// - When `current_slot < tooLate` ([`VotingPeriod::ForThisEpoch`]) the
    ///   target epoch must equal `current_epoch`.
    /// - When `current_slot >= tooLate` ([`VotingPeriod::ForNextEpoch`]) the
    ///   target epoch must equal `current_epoch + 1`.
    ///
    /// Reference: Haskell `PPUpdateWrongEpoch` in
    /// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`.
    ///
    /// Skipped silently when `epoch_length` or `security_param` are not
    /// supplied on the context (the predicate cannot fire without them —
    /// same lenient-default convention as the MIR predicates).
    #[error("PPUpdateWrongEpoch: current={current}, target={target}, period={period:?}")]
    PPUpdateWrongEpoch {
        /// Current epoch (derived from `current_slot`/`epoch_length` if not
        /// supplied directly).
        current: u64,
        /// The proposal's declared target epoch.
        target: u64,
        /// Whether the current slot is in the for-this-epoch or
        /// for-next-epoch voting period.
        period: VotingPeriod,
    },
    /// Shelley PPUP rule `PVCannotFollowPPUP`: the proposed protocol version
    /// does not validly follow the current one.  Only minor (`(major,
    /// minor+1)`) and major (`(major+1, 0)`) bumps are allowed; skipping
    /// versions or regressing is rejected.
    ///
    /// Reference: Haskell `PVCannotFollowPPUP` in
    /// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`.
    #[error("PVCannotFollowPPUP: bad_pv={bad_pv:?}")]
    PVCannotFollowPPUP {
        /// `(major, minor)` of the proposed protocol version that fails the
        /// `pvCanFollow` check.
        bad_pv: (u32, u32),
    },
}

// ---------------------------------------------------------------------------
// PPUP supporting types
// ---------------------------------------------------------------------------

/// Voting period for a pre-Conway protocol-parameter update proposal.
///
/// Mirrors Haskell's `VotingPeriod` in
/// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`:
///
/// - [`VotingPeriod::ForThisEpoch`] — `current_slot < tooLate`; the
///   proposal's target must equal the current epoch.
/// - [`VotingPeriod::ForNextEpoch`] — `current_slot >= tooLate`; the
///   proposal's target must equal the next epoch.
///
/// Where `tooLate = firstSlotOfNextEpoch - 2 * stabilityWindow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VotingPeriod {
    ForThisEpoch,
    ForNextEpoch,
}

// ---------------------------------------------------------------------------
// Public validation entry points
// ---------------------------------------------------------------------------

/// Validate a transaction against the current UTxO set and protocol parameters.
///
/// This is a convenience wrapper around [`validate_transaction_with_pools`] that
/// treats all pool registrations as new (no re-registration discount).
///
/// The `utxo_set` parameter accepts anything that implements [`UtxoLookup`],
/// including the standard on-chain `&UtxoSet` and the composite
/// `CompositeUtxoView` used by the mempool validator for chained tx support.
/// Decide the phase-2 admission outcome from the `is_valid` tag and the
/// evaluator result, mirroring the Haskell UTXOS rule exactly (#733/#734):
///
/// * **Collection/context errors** (`CollectErrors`: decode failures, missing
///   script/datum, past-horizon time translation, missing CBOR
///   infrastructure) reject the tx for BOTH tag polarities — Haskell raises
///   `UtxosFailure (CollectErrors …)` before any script is evaluated.
/// * `is_valid = true` + script failure → `ScriptFailed`
///   (Haskell `ValidationTagMismatch … FailedUnexpectedly`).
/// * `is_valid = false` + all scripts pass → `IsValidTagMismatch`
///   (Haskell `ValidationTagMismatch … PassedUnexpectedly`, the #522 class).
/// * `is_valid = false` + genuine script failure → admitted (the legitimate
///   collateral-consuming path).
pub(crate) fn phase2_admission_error(
    is_valid: bool,
    eval_result: &Result<(), crate::plutus::PlutusError>,
) -> Option<ValidationError> {
    use crate::plutus::PlutusError;
    match eval_result {
        // Collection/context errors reject regardless of the tag.  The
        // match is exhaustive on purpose: a new PlutusError variant must
        // make a deliberate classification choice here.
        Err(
            e @ (PlutusError::CollectError(_)
            | PlutusError::MissingTxCbor
            | PlutusError::MissingOutputCbor(_)),
        ) => Some(ValidationError::Phase2CollectError(e.to_string())),
        // A dugite-CEK panic rejects at admission for BOTH polarities
        // (reject-by-default on adversarial input) but is deliberately NOT
        // a Haskell CollectError: at block apply it stays warn-and-trust —
        // a Haskell-validated chain can contain scripts that panic dugite's
        // evaluator (#733 correction 3).
        Err(e @ PlutusError::EvalPanic(_)) => Some(ValidationError::Phase2EvalPanic(e.to_string())),
        Err(e @ PlutusError::EvalFailed(_)) => {
            if is_valid {
                // Tx claims scripts pass — they failed.
                Some(ValidationError::ScriptFailed(e.to_string()))
            } else {
                // Both the tag and the evaluator agree the scripts fail —
                // the legitimate is_valid=false path (collateral consumed
                // at block apply).
                None
            }
        }
        Ok(()) => {
            if is_valid {
                None
            } else {
                // Tx claims scripts fail but they pass — the #522 "DoS
                // class" attack vector (`TagMismatch PassedUnexpectedly`).
                Some(ValidationError::IsValidTagMismatch {
                    declared: false,
                    evaluated: true,
                })
            }
        }
    }
}

/// The sorted set of executed Plutus language tags (1=V1, 2=V2, 3=V3, 4=V4) that
/// have NO cost model in `cost_models`. A non-empty result corresponds to Haskell
/// `collectPlutusScriptsWithContext` failing with `CollectErrors [NoCostModel lang]`
/// — the transaction must be rejected regardless of `isValid`, before any script is
/// evaluated. The lookup is keyed per-executed-script's own language (a tx with only
/// V2 scripts never observes a missing V1 cost model); native scripts and unknown
/// tags never need a cost model. Only TOTAL absence counts (a present-but-short cost
/// model is `maxBound`-padded upstream, not a `NoCostModel`). See #826 / #860.3.
pub(crate) fn missing_cost_model_languages(
    executed_langs: impl IntoIterator<Item = u8>,
    cost_models: &dugite_primitives::transaction::CostModels,
) -> Vec<u8> {
    executed_langs
        .into_iter()
        .collect::<std::collections::BTreeSet<u8>>()
        .into_iter()
        .filter(|&lang| !match lang {
            1 => cost_models.plutus_v1.is_some(),
            2 => cost_models.plutus_v2.is_some(),
            3 => cost_models.plutus_v3.is_some(),
            4 => cost_models.plutus_v4.is_some(),
            _ => true,
        })
        .collect()
}

pub fn validate_transaction(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    params: &ProtocolParameters,
    current_slot: u64,
    tx_size: u64,
    slot_config: Option<&SlotConfig>,
) -> Result<(), Vec<ValidationError>> {
    validate_transaction_with_pools(
        tx,
        utxo_set,
        params,
        current_slot,
        tx_size,
        slot_config,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// Validate a transaction using a [`ValidationContext`] struct.
///
/// This is the preferred entry point for validation with full ledger state,
/// replacing the many-parameter [`validate_transaction_with_pools`] function.
///
/// # Example
///
/// ```rust,ignore
/// use dugite_ledger::validation::{ValidationContext, validate_transaction_with_context};
///
/// let context = ValidationContext::new()
///     .with_pools(pool_ids)
///     .with_treasury(treasury)
///     .with_reward_accounts(accounts)
///     .with_epoch(epoch)
///     .with_dreps(drep_ids)
///     .with_network(NetworkId::Mainnet);
///
/// let result = validate_transaction_with_context(
///     &tx,
///     &utxo_set,
///     &params,
///     current_slot,
///     tx_size,
///     slot_config,
///     context,
/// );
/// ```
pub fn validate_transaction_with_context(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    params: &ProtocolParameters,
    current_slot: u64,
    tx_size: u64,
    slot_config: Option<&SlotConfig>,
    context: ValidationContext,
) -> Result<(), Vec<ValidationError>> {
    let pools_result = validate_transaction_with_pools(
        tx,
        utxo_set,
        params,
        current_slot,
        tx_size,
        slot_config,
        context.registered_pools.as_deref(),
        context.current_treasury,
        context.reward_accounts.as_ref(),
        context.current_epoch,
        context.registered_dreps.as_deref(),
        context.registered_vrf_keys.as_deref(),
        context.node_network,
        context.committee_members.as_deref(),
        context.committee_resigned.as_deref(),
        context.stake_key_deposits.as_ref(),
        context.constitution_script_hash,
        context.vote_delegations.as_deref(),
    );

    // ------------------------------------------------------------------
    // Conway GOVCERT: ConwayDRepIncorrectRefund
    //
    // An `UnregDRep` certificate carries an inline `refund` amount which
    // must exactly equal the deposit paid at registration time (stored
    // per-credential in [`ValidationContext::drep_deposits`]). Without this
    // check, a malicious tx could withdraw arbitrary lovelace from the
    // deposit pot by declaring a refund larger than the original deposit.
    //
    // Per Haskell `conwayGovCertTransition` (GOVCERT sub-rule of CERTS):
    //
    // ```haskell
    // ConwayUnRegDRep cred refund -> do
    //   let mDRepState = Map.lookup cred (certState ^. certVStateL . vsDRepsL)
    //       drepRefundMismatch = do
    //         drepState <- mDRepState
    //         let paidDeposit = drepState ^. drepDepositL
    //         guard (refund /= paidDeposit)
    //         pure paidDeposit
    //   isJust mDRepState ?! (injectFailure . ConwayDRepNotRegistered) cred
    //   failOnJust drepRefundMismatch $
    //     injectFailure . ConwayDRepIncorrectRefund . Mismatch refund
    // ```
    //
    // We skip the refund check when the DRep is unknown (the
    // `ConwayDRepNotRegistered` predicate has already fired in the inner
    // `validate_transaction_with_pools` for that case). Silently skipped
    // when `drep_deposits` is `None` (lenient default — mirrors the
    // `stake_key_deposits` convention).
    //
    // Reference: Haskell `ConwayDRepIncorrectRefund` in
    // `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.GovCert`.
    // ------------------------------------------------------------------
    let mut extra_errors_pre: Vec<ValidationError> = Vec::new();
    if let (Some(dreps), Some(deposits)) = (
        context.registered_dreps.as_deref(),
        context.drep_deposits.as_deref(),
    ) {
        for cert in &tx.body.certificates {
            if let dugite_primitives::transaction::Certificate::UnregDRep { credential, refund } =
                cert
            {
                let key = credential.to_typed_hash32();
                if !dreps.contains(&key) {
                    // Already flagged inside as DRepNotRegistered; skip
                    // refund check (Haskell's `?!` short-circuit).
                    continue;
                }
                if let Some(stored_deposit) = deposits.get(&key) {
                    if refund.0 != *stored_deposit {
                        extra_errors_pre.push(ValidationError::DRepIncorrectRefund {
                            credential_hash: key.to_hex(),
                            declared: refund.0,
                            expected: *stored_deposit,
                        });
                    }
                }
            }
        }
    }

    // Conway GOV `VotersDoNotExist` and `DisallowedVoters` predicates.
    //
    // Both are PV >= 9 only and operate on `tx.body.voting_procedures`.
    //
    // Per Haskell `conwayGovTransition` (`internVoter`), unknown voters are
    // partitioned out of the voting set BEFORE the authority check runs — i.e.
    // `VotersDoNotExist` takes precedence over `DisallowedVoters`, and a single
    // voter is never reported under both.  We implement this by collecting the
    // unknown voters first into a `HashSet` and skipping them when the
    // `DisallowedVoters` loop iterates.
    let mut extra_errors: Vec<ValidationError> = Vec::new();
    if params.protocol_version_major >= 9 && !tx.body.voting_procedures.is_empty() {
        // -------------------------------------------------------------------
        // UnelectedCommitteeVoters (PV >= 11): every CC vote whose hot
        // credential is NOT in `authorizedElectedHotCommitteeCredentials`
        // is reported here, BEFORE `VotersDoNotExist`. At PV >= 11 BOTH
        // predicates fire for the same voter when the cold credential
        // was authorised but its backing election has been removed.
        //
        // Reference: Haskell `unelectedCommitteeVoters` in
        // `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`.
        // -------------------------------------------------------------------
        if params.protocol_version_major >= 11 {
            if let Some(elected) = context.committee_authorized_elected_hot_keys.as_ref() {
                let mut unelected_hot: Vec<Hash32> = Vec::new();
                let mut seen: HashSet<Hash32> = HashSet::new();
                for (voter, vp_map) in tx.body.voting_procedures.iter() {
                    if vp_map.is_empty() {
                        continue;
                    }
                    if let Voter::ConstitutionalCommittee(hot) = voter {
                        let hot_key = hot.to_typed_hash32();
                        if seen.contains(&hot_key) {
                            continue;
                        }
                        if !elected.contains(&hot_key) {
                            unelected_hot.push(hot_key);
                            seen.insert(hot_key);
                        }
                    }
                }
                if !unelected_hot.is_empty() {
                    extra_errors.push(ValidationError::UnelectedCommitteeVoters {
                        hot_keys: unelected_hot,
                    });
                }
            }
        }

        // -------------------------------------------------------------------
        // VotersDoNotExist: every voter whose credential / pool ID is not in
        // the corresponding registry.  Empty `vp_map`s are skipped — Haskell
        // does the same partition over the keys of the voting-procedures map,
        // and an empty inner map is unreachable in practice (CBOR decoders
        // reject it).
        // -------------------------------------------------------------------
        // Two collections, one purpose: `unknown_voters` preserves the order
        // of voters as they appear in `voting_procedures` so the resulting
        // `VotersDoNotExist` payload is deterministic; `unknown_voter_set`
        // gives O(1) skip-membership lookup for the precedence loops below
        // (DisallowedVoters / VotingOnExpiredGovAction must not double-fire
        // on a voter that's already in `VotersDoNotExist`).
        let mut unknown_voters: Vec<Voter> = Vec::new();
        let mut unknown_voter_set: HashSet<Voter> = HashSet::new();
        for (voter, vp_map) in tx.body.voting_procedures.iter() {
            if !vp_map.is_empty() && conway::is_voter_unknown(voter, &context) {
                unknown_voters.push(voter.clone());
                unknown_voter_set.insert(voter.clone());
            }
        }
        if !unknown_voters.is_empty() {
            extra_errors.push(ValidationError::VotersDoNotExist {
                voters: unknown_voters,
            });
        }

        // -------------------------------------------------------------------
        // GovActionsDoNotExist: every vote must reference a `GovActionId`
        // that exists either in the same tx's `proposal_procedures` (a
        // local proposal — admissible by definition) or in the
        // `active_proposals` map captured from current ledger state.
        //
        // Without this check dugite admits votes whose action has just
        // been ratified-and-removed at the previous epoch boundary; the
        // forge picks them up, and Haskell rejects the resulting block
        // with `ConwayGovFailure (GovActionsDoNotExist …)` — exactly the
        // chain-stall we observed at slot 800 on the devnet.
        //
        // The check is silently skipped when `active_proposals` is None
        // (the same lenient default we use for `committee_authorized_hot_keys`
        // etc.) so callers without ledger plumbing don't see false
        // positives.  Votes that reference voters already flagged as
        // unknown skip this check — Haskell partitions unknown voters
        // out before the action-existence check, matching the
        // VotersDoNotExist > GovActionsDoNotExist precedence.
        // -------------------------------------------------------------------
        let mut local_action_ids: HashSet<GovActionId> = HashSet::new();
        for (idx, _proposal) in tx.body.proposal_procedures.iter().enumerate() {
            local_action_ids.insert(GovActionId {
                transaction_id: tx.hash,
                action_index: idx as u32,
            });
        }
        if let Some(active) = context.active_proposals.as_ref() {
            let mut missing: Vec<GovActionId> = Vec::new();
            let mut seen: HashSet<GovActionId> = HashSet::new();
            for (voter, votes) in &tx.body.voting_procedures {
                if unknown_voter_set.contains(voter) {
                    continue;
                }
                for action_id in votes.keys() {
                    if seen.contains(action_id) {
                        continue;
                    }
                    if !local_action_ids.contains(action_id) && !active.contains_key(action_id) {
                        missing.push(action_id.clone());
                        seen.insert(action_id.clone());
                    }
                }
            }
            if !missing.is_empty() {
                extra_errors.push(ValidationError::GovActionsDoNotExist {
                    action_ids: missing,
                });
            }
        }

        // -------------------------------------------------------------------
        // DisallowedVoters: voter type is not authorised for the action type.
        //
        // For every (voter, gov_action_id) pair in `voting_procedures`, look up
        // the referenced GovAction and reject the vote if the voter type is
        // not authorised for that action type (Haskell `checkVotersAreValid` /
        // `is{Committee,DRep,StakePool}VotingAllowed`).
        //
        // The GovAction is looked up first against proposals submitted in the
        // same transaction, then against the optional `active_proposals` map
        // provided by the caller (typically the on-chain governance state).
        // Votes that do not resolve to any known action are ignored here —
        // that's a different predicate (`GovActionsDoNotExist`) handled
        // elsewhere.
        //
        // Voters already in `unknown_voter_set` are skipped so they don't
        // appear in BOTH `VotersDoNotExist` and `DisallowedVoters` (Haskell
        // partitions unknowns out before the authority check).
        // -------------------------------------------------------------------
        let mut local_proposals: HashMap<GovActionId, &GovAction> = HashMap::new();
        for (idx, proposal) in tx.body.proposal_procedures.iter().enumerate() {
            let id = GovActionId {
                transaction_id: tx.hash,
                action_index: idx as u32,
            };
            local_proposals.insert(id, &proposal.gov_action);
        }

        // -------------------------------------------------------------------
        // VotingOnExpiredGovAction: a vote against an action whose
        // `expires_after_epoch` is strictly less than `current_epoch` is
        // rejected.  Boundary case (`current_epoch == expires_after_epoch`)
        // is allowed — Haskell `checkVotesAreNotForExpiredActions`.
        //
        // Precedence (matches Haskell `conwayGovTransition` step ordering —
        // see `Cardano.Ledger.Conway.Rules.Gov`):
        //
        //   VotersDoNotExist  >  GovActionsDoNotExist
        //                     >  VotingOnExpiredGovAction
        //                     >  DisallowedVoters
        //
        // The dugite ordering previously placed `DisallowedVoters` *before*
        // `VotingOnExpiredGovAction`, which is the opposite of Haskell's
        // canonical ordering and would surface the wrong predicate first
        // when both fire on the same (voter, action) pair.  Swapped to
        // match cardano-haskell-oracle deep-dive findings (D-2).
        //
        // Concretely: we skip voters already in `unknown_voter_set` so a
        // single voter is never reported under multiple predicates here.
        //
        // Same-tx proposals (`local_proposals`) are skipped because a
        // proposal that was just submitted in this tx cannot have expired.
        // This matches Haskell's `proposals` look-up: only the on-chain
        // active-proposal map carries an `expiresAfter` field.
        // -------------------------------------------------------------------
        let mut expired_votes: Vec<(Voter, GovActionId, u64, u64)> = Vec::new();
        if let Some(current_epoch) = context.current_epoch {
            for (voter, votes) in &tx.body.voting_procedures {
                if unknown_voter_set.contains(voter) {
                    continue;
                }
                for action_id in votes.keys() {
                    // Same-tx proposals are never expired (they were just
                    // submitted), so skip them here.  This also prevents
                    // double-firing when a proposal happens to share a
                    // GovActionId with an active one (impossible in practice
                    // but the local-tx branch wins).
                    if local_proposals.contains_key(action_id) {
                        continue;
                    }
                    if conway::is_vote_on_expired_action(action_id, &context) {
                        // SAFETY: is_vote_on_expired_action returned true, so
                        // both `active_proposals` and the action_id entry exist.
                        let expires = context
                            .active_proposals
                            .as_ref()
                            .and_then(|m| m.get(action_id))
                            .map(|p| p.expires_after_epoch.0)
                            .expect("predicate true implies active proposal exists");
                        expired_votes.push((
                            voter.clone(),
                            action_id.clone(),
                            expires,
                            current_epoch,
                        ));
                    }
                }
            }
        }
        if !expired_votes.is_empty() {
            extra_errors.push(ValidationError::VotingOnExpiredGovAction { expired_votes });
        }

        // -------------------------------------------------------------------
        // DisallowedVoters: voter type is not authorised for the action type.
        //
        // For every (voter, gov_action_id) pair in `voting_procedures`, look up
        // the referenced GovAction and reject the vote if the voter type is
        // not authorised for that action type (Haskell `checkVotersAreValid` /
        // `is{Committee,DRep,StakePool}VotingAllowed`).
        //
        // The GovAction is looked up first against proposals submitted in the
        // same transaction, then against the optional `active_proposals` map
        // provided by the caller (typically the on-chain governance state).
        // Votes that do not resolve to any known action are ignored here —
        // that's a different predicate (`GovActionsDoNotExist`) handled above.
        //
        // Voters already in `unknown_voter_set` are skipped so they don't
        // appear in BOTH `VotersDoNotExist` and `DisallowedVoters` (Haskell
        // partitions unknowns out before the authority check).
        // -------------------------------------------------------------------
        let mut violations: Vec<(Voter, GovActionId)> = Vec::new();
        for (voter, votes) in &tx.body.voting_procedures {
            if unknown_voter_set.contains(voter) {
                continue;
            }
            for action_id in votes.keys() {
                let action: Option<&GovAction> =
                    local_proposals.get(action_id).copied().or_else(|| {
                        context
                            .active_proposals
                            .as_ref()
                            .and_then(|m| m.get(action_id))
                            .map(|ap| &ap.gov_action)
                    });

                let Some(action) = action else {
                    // Vote references an unknown GovAction; this is a
                    // different predicate failure (GovActionsDoNotExist),
                    // emitted above.  Skip silently here.
                    continue;
                };

                if conway::is_voter_disallowed(voter, action) {
                    violations.push((voter.clone(), action_id.clone()));
                }
            }
        }
        if !violations.is_empty() {
            extra_errors.push(ValidationError::DisallowedVoters { violations });
        }

        // -------------------------------------------------------------------
        // DisallowedVotesDuringBootstrap (Conway GOV rule, PV9 only).
        //
        // During the Conway bootstrap phase (`pvMajor < 10`), Haskell
        // restricts voter types to a subset of governance actions:
        // - DRepVoter: only `InfoAction`
        // - CommitteeVoter / StakePoolVoter: only ParameterChange,
        //   HardForkInitiation, or InfoAction (the "bootstrap actions").
        //
        // The check is gated on `params.protocol_version_major == 9` (PV10+
        // ratifies all governance action types). Outside the bootstrap
        // window the Haskell predicate is a `pure ()` no-op.
        //
        // Reference: Haskell `checkBootstrapVotes` in
        // `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
        // lines 378-391.
        // -------------------------------------------------------------------
        if params.protocol_version_major == 9 {
            let mut bootstrap_violations: Vec<(Voter, GovActionId)> = Vec::new();
            for (voter, votes) in tx.body.voting_procedures.iter() {
                if unknown_voter_set.contains(voter) {
                    continue;
                }
                for action_id in votes.keys() {
                    let action: Option<&GovAction> =
                        local_proposals.get(action_id).copied().or_else(|| {
                            context
                                .active_proposals
                                .as_ref()
                                .and_then(|m| m.get(action_id))
                                .map(|ap| &ap.gov_action)
                        });
                    let Some(action) = action else { continue };
                    if conway::is_bootstrap_vote_disallowed(voter, action) {
                        bootstrap_violations.push((voter.clone(), action_id.clone()));
                    }
                }
            }
            if !bootstrap_violations.is_empty() {
                extra_errors.push(ValidationError::DisallowedVotesDuringBootstrap {
                    violations: bootstrap_violations,
                });
            }
        }
    }

    // -------------------------------------------------------------------
    // ProposalReturnAccountDoesNotExist: every proposal procedure's
    // `return_addr` must reference a registered stake credential so the
    // deposit can be refunded.  Per Haskell `processProposal` in
    // `Conway.Rules.Gov`, this check is **skipped during Conway bootstrap**
    // (`pvMajor == 9`) — bootstrap gating is inside the predicate, so the
    // wiring just iterates and aggregates.
    //
    // Runs only when the transaction submits at least one proposal.  This
    // mirrors `Conway.Rules.Gov.processProposal`, which is invoked once per
    // proposal in `tx.body.proposal_procedures` (and so does nothing for
    // a tx with no proposals).
    // -------------------------------------------------------------------
    if params.protocol_version_major >= 9 && !tx.body.proposal_procedures.is_empty() {
        // -------------------------------------------------------------------
        // InvalidPrevGovActionId: every lineal-purpose proposal must chain
        // correctly onto its purpose.
        //
        // Haskell (`Conway.Rules.Gov`) FAILS THE TRANSACTION here:
        //
        //   case proposalsAddAction actionState proposals of
        //     Just updatedProposals -> pure updatedProposals
        //     Nothing -> proposals <$ failBecause (injectFailure $
        //                  InvalidPrevGovActionId proposal)
        //
        // dugite used to only WARN and drop the proposal at apply time,
        // believing Haskell dropped it too. It does not. That gap let dugite's
        // forge mint a block cardano-node considered invalid, which answered
        // `ShutdownPeer` and stalled the chain — reproduced on the devnet at
        // slot 1870 (a ParameterChange with prev_action_id=None proposed after
        // one had already been enacted).
        //
        // Proposals earlier in the SAME tx are valid parents: Haskell folds
        // `processProposal` over the tx's proposals in order, so proposal N may
        // chain onto proposal N-1 of the same tx. Only strictly-earlier indices
        // count — a forward or self reference is not yet in `proposals`.
        //
        // Skipped entirely when `enacted_gov_roots` is None (lenient default).
        // -------------------------------------------------------------------
        if let Some(roots) = context.enacted_gov_roots.as_ref() {
            for (idx, proposal) in tx.body.proposal_procedures.iter().enumerate() {
                let action = &proposal.gov_action;
                if !gov_action_has_lineage(action) {
                    continue;
                }
                let prev_id = gov_action_prev_id(action);
                let valid = match prev_id {
                    // Case (a): genesis root — only when nothing of this
                    // purpose has ever been enacted.
                    None => roots.root_for(action).is_none(),
                    // Case (b): must be this purpose's enacted root, an active
                    // in-flight proposal, or an earlier proposal in this tx.
                    Some(prev) => {
                        let matches_root = roots.root_for(action) == Some(prev);
                        let in_flight = context
                            .active_proposals
                            .as_ref()
                            .is_some_and(|active| active.contains_key(prev));
                        let earlier_in_tx =
                            prev.transaction_id == tx.hash && (prev.action_index as usize) < idx;
                        matches_root || in_flight || earlier_in_tx
                    }
                };
                if !valid {
                    extra_errors.push(ValidationError::InvalidPrevGovActionId {
                        action_index: idx as u32,
                        action_type: gov_action_type_name(action),
                        prev_action_id: prev_id.cloned(),
                        proposal: Box::new(proposal.clone()),
                    });
                }
            }
        }
    }

    if params.protocol_version_major >= 9 && !tx.body.proposal_procedures.is_empty() {
        let mut bad_addrs: Vec<String> = Vec::new();
        for proposal in &tx.body.proposal_procedures {
            if conway::is_proposal_return_account_unregistered(proposal, params, &context) {
                // Hex-encode the raw return_addr bytes for the diagnostic.
                // Matches the fold-based encoding used for withdrawal account
                // hex strings above so error formatting stays consistent
                // across this module without adding a new dependency.
                let addr_hex = proposal.return_addr.iter().fold(
                    String::with_capacity(proposal.return_addr.len() * 2),
                    |mut s, b| {
                        use std::fmt::Write;
                        let _ = write!(s, "{b:02x}");
                        s
                    },
                );
                bad_addrs.push(addr_hex);
            }
        }
        if !bad_addrs.is_empty() {
            extra_errors.push(ValidationError::ProposalReturnAccountDoesNotExist { bad_addrs });
        }

        // -------------------------------------------------------------------
        // TreasuryWithdrawalReturnAccountsDoNotExist (Conway GOV rule).
        //
        // For every TreasuryWithdrawals proposal action, each withdrawal
        // DESTINATION address (the receiver of the lovelace, the keys of
        // the withdrawals map) must have its credential registered in the
        // DState accounts map. Unknown destinations would otherwise allow
        // arbitrary lovelace to be drained from treasury to an unaccountable
        // sink.
        //
        // Per Haskell `processProposal` in
        // `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
        // lines 509-519:
        //
        // ```haskell
        // TreasuryWithdrawals withdrawals _ -> do
        //   let nonRegisteredAccounts =
        //         flip Map.filterWithKey withdrawals $ \withdrawalAddress _ ->
        //           not $
        //             isAccountRegistered
        //               (withdrawalAddress ^. accountAddressCredentialL)
        //               (certDState ^. accountsL)
        //   failOnNonEmpty (Map.keys nonRegisteredAccounts)
        //     (injectFailure . TreasuryWithdrawalReturnAccountsDoNotExist)
        // ```
        //
        // The lookup table on dugite's side is
        // `ValidationContext::reward_accounts` (Haskell `accountsL` —
        // staking credential → account info). When `reward_accounts` is
        // `None`, the predicate is silently skipped (lenient default).
        // -------------------------------------------------------------------
        if let Some(reward_accounts) = context.reward_accounts.as_ref() {
            let mut bad_withdrawal_addrs: Vec<String> = Vec::new();
            for proposal in &tx.body.proposal_procedures {
                if let GovAction::TreasuryWithdrawals { withdrawals, .. } = &proposal.gov_action {
                    for withdrawal_addr in withdrawals.keys() {
                        // Reward addresses are 29 bytes: 1-byte header +
                        // 28-byte credential hash. Header bit 4 (0x10):
                        // 0=key credential, 1=script credential. We use
                        // the 28-byte hash plus the type tag to form the
                        // `Hash32` key used by reward_accounts (mirrors
                        // `Credential::to_typed_hash32`).
                        if withdrawal_addr.len() < 29 {
                            // Malformed addr — leave to other predicates.
                            continue;
                        }
                        let is_script = (withdrawal_addr[0] & 0x10) != 0;
                        let tag: u8 = if is_script { 0x01 } else { 0x00 };
                        let mut key_bytes = [0u8; 32];
                        key_bytes[..28].copy_from_slice(&withdrawal_addr[1..29]);
                        key_bytes[28] = tag;
                        let key = dugite_primitives::hash::Hash::<32>::from_bytes(key_bytes);
                        if !reward_accounts.contains_key(&key) {
                            let addr_hex = withdrawal_addr.iter().fold(
                                String::with_capacity(withdrawal_addr.len() * 2),
                                |mut s, b| {
                                    use std::fmt::Write;
                                    let _ = write!(s, "{b:02x}");
                                    s
                                },
                            );
                            bad_withdrawal_addrs.push(addr_hex);
                        }
                    }
                }
            }
            if !bad_withdrawal_addrs.is_empty() {
                extra_errors.push(
                    ValidationError::TreasuryWithdrawalReturnAccountsDoNotExist {
                        bad_addrs: bad_withdrawal_addrs,
                    },
                );
            }
        }
    }

    // -------------------------------------------------------------------
    // ProposalProcedureNetworkIdMismatch: every proposal procedure's
    // `return_addr` must be on the same network as the node.  Per Haskell
    // `processProposal` in `Conway.Rules.Gov`, this check is **always
    // enforced** (no Conway-bootstrap skip — the network id is a
    // structural property of the proposal, not a post-bootstrap state
    // lookup).
    //
    // Runs only when the transaction submits at least one proposal,
    // mirroring `processProposal`'s per-proposal invocation.
    // -------------------------------------------------------------------
    if params.protocol_version_major >= 9 && !tx.body.proposal_procedures.is_empty() {
        let mut mismatched: Vec<(String, u8)> = Vec::new();
        for proposal in &tx.body.proposal_procedures {
            if let Some(actual_net) =
                conway::is_proposal_return_addr_wrong_network(proposal, &context)
            {
                let addr_hex = proposal.return_addr.iter().fold(
                    String::with_capacity(proposal.return_addr.len() * 2),
                    |mut s, b| {
                        use std::fmt::Write;
                        let _ = write!(s, "{b:02x}");
                        s
                    },
                );
                mismatched.push((addr_hex, actual_net));
            }
        }
        if !mismatched.is_empty() {
            // SAFETY: predicate fired -> ctx.node_network must be Some
            // (the predicate returns None when node_network is None).
            let expected = context
                .node_network
                .expect("predicate fired implies node_network is Some")
                .to_u8();
            extra_errors.push(ValidationError::ProposalProcedureNetworkIdMismatch {
                expected,
                mismatched,
            });
        }
    }

    // -------------------------------------------------------------------
    // TreasuryWithdrawalsNetworkIdMismatch: every destination reward
    // address in a `TreasuryWithdrawals` proposal must be on the same
    // network as the node.  Per Haskell `processProposal` in
    // `Conway.Rules.Gov` (`TreasuryWithdrawals` branch), this check is
    // **always enforced** (no Conway-bootstrap skip — the network id is a
    // structural property of the proposal, not a post-bootstrap state
    // lookup).  All mismatched destinations across all TreasuryWithdrawals
    // proposals are aggregated into a single error, mirroring Haskell's
    // `NonEmpty` predicate-failure shape.
    //
    // Runs only when the transaction submits at least one proposal,
    // mirroring `processProposal`'s per-proposal invocation.
    // -------------------------------------------------------------------
    if params.protocol_version_major >= 9 && !tx.body.proposal_procedures.is_empty() {
        let mut mismatched: Vec<(String, u8)> = Vec::new();
        for proposal in &tx.body.proposal_procedures {
            mismatched.extend(conway::treasury_withdrawal_network_mismatches(
                proposal, &context,
            ));
        }
        if !mismatched.is_empty() {
            // SAFETY: predicate fired -> ctx.node_network must be Some
            // (the predicate returns an empty vec when node_network is None).
            let expected = context
                .node_network
                .expect("predicate fired implies node_network is Some")
                .to_u8();
            extra_errors.push(ValidationError::TreasuryWithdrawalsNetworkIdMismatch {
                expected,
                mismatched,
            });
        }
    }

    // -------------------------------------------------------------------
    // ZeroTreasuryWithdrawals: every `TreasuryWithdrawals` proposal must
    // carry a strictly positive total amount.  Per Haskell `processProposal`
    // in `Conway.Rules.Gov` this check is **skipped during Conway
    // bootstrap** (PV == 9) per `hardforkConwayBootstrapPhase`.
    //
    // Runs only when the transaction submits at least one proposal,
    // mirroring `processProposal`'s per-proposal invocation.  The bootstrap
    // gate is encoded inside the predicate itself (`is_treasury_withdrawals_zero_sum`),
    // so the wiring here is straightforward.
    // -------------------------------------------------------------------
    if params.protocol_version_major >= 9 && !tx.body.proposal_procedures.is_empty() {
        let mut offending: Vec<String> = Vec::new();
        for proposal in &tx.body.proposal_procedures {
            if conway::is_treasury_withdrawals_zero_sum(proposal, params) {
                let id_hex = proposal.return_addr.iter().fold(
                    String::with_capacity(proposal.return_addr.len() * 2),
                    |mut s, b| {
                        use std::fmt::Write;
                        let _ = write!(s, "{b:02x}");
                        s
                    },
                );
                offending.push(id_hex);
            }
        }
        if !offending.is_empty() {
            extra_errors.push(ValidationError::ZeroTreasuryWithdrawals {
                offending_proposals: offending,
            });
        }
    }

    // -------------------------------------------------------------------
    // ConflictingCommitteeUpdate: every `UpdateCommittee` proposal must
    // have an empty intersection between its add-set keys and its
    // remove-set.  Per Haskell `processProposal` in `Conway.Rules.Gov`,
    // this check is **always enforced** (no Conway-bootstrap skip — the
    // add/remove conflict is a structural property of the action payload).
    //
    // Runs only when the transaction submits at least one proposal,
    // mirroring `processProposal`'s per-proposal invocation.
    // -------------------------------------------------------------------
    if params.protocol_version_major >= 9 && !tx.body.proposal_procedures.is_empty() {
        let mut conflicts: Vec<String> = Vec::new();
        for proposal in &tx.body.proposal_procedures {
            conflicts.extend(conway::committee_update_conflicts(proposal));
        }
        if !conflicts.is_empty() {
            extra_errors.push(ValidationError::ConflictingCommitteeUpdate { conflicts });
        }
    }

    // -------------------------------------------------------------------
    // ExpirationEpochTooSmall: every new committee member added by an
    // `UpdateCommittee` proposal must have a `validUntil` epoch strictly
    // greater than the current epoch.  Per Haskell `processProposal` in
    // `Conway.Rules.Gov`, this check is **always enforced** (no
    // Conway-bootstrap skip).  When `ctx.current_epoch` is `None`, the
    // predicate is silently lenient (returns the empty vec) so callers
    // that have not plumbed in epoch context don't get spurious failures.
    //
    // Runs only when the transaction submits at least one proposal,
    // mirroring `processProposal`'s per-proposal invocation.
    // -------------------------------------------------------------------
    if params.protocol_version_major >= 9 && !tx.body.proposal_procedures.is_empty() {
        let mut invalid_members: Vec<(String, u64)> = Vec::new();
        for proposal in &tx.body.proposal_procedures {
            invalid_members.extend(conway::committee_update_invalid_expiries(
                proposal, &context,
            ));
        }
        if !invalid_members.is_empty() {
            extra_errors.push(ValidationError::ExpirationEpochTooSmall { invalid_members });
        }
    }

    // -------------------------------------------------------------------
    // MIR (Move Instantaneous Rewards) — Shelley–Babbage only.
    //
    // Each MIR certificate in `tx.body.certificates` is validated against
    // the 7 predicates in `validation::mir`.  At PV >= 9 (Conway) the
    // entry point is a no-op so the wiring layer also short-circuits to
    // avoid an unnecessary scan of the cert vec.
    //
    // Reference: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`.
    // -------------------------------------------------------------------
    if params.protocol_version_major < 9 {
        for cert in &tx.body.certificates {
            if let Err(errs) = mir::validate_mir_cert(cert, params, current_slot, &context) {
                extra_errors.extend(errs);
            }
        }
    }

    // -------------------------------------------------------------------
    // MIR genesis-delegate quorum — Shelley–Babbage only, WHOLE-TRANSACTION
    // (UTXOW, not DELEG — see `mir::check_mir_genesis_quorum` doc comment).
    // Guarded identically to the per-cert MIR loop above (#804).
    // -------------------------------------------------------------------
    if params.protocol_version_major < 9 {
        mir::check_mir_genesis_quorum(tx, params, &context, &mut extra_errors);
    }

    // -------------------------------------------------------------------
    // PPUP — pre-Conway protocol-parameter update proposal.
    //
    // Active in Shelley–Babbage (`AtMostEra "Babbage" era`); Conway
    // replaces this rule with on-chain governance.  The entry point is a
    // no-op at PV >= 9 so the wiring layer also short-circuits to avoid
    // touching `tx.body.update` at all in the Conway-only path.
    //
    // Reference: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`.
    // -------------------------------------------------------------------
    if params.protocol_version_major < 9 {
        if let Err(errs) =
            ppup::validate_ppup(tx.body.update.as_ref(), params, current_slot, &context)
        {
            extra_errors.extend(errs);
        }
    }

    // Fold extra_errors_pre into extra_errors so the final match handles
    // all three error sources uniformly.
    extra_errors.append(&mut extra_errors_pre);

    match (pools_result, extra_errors.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Ok(()), false) => Err(extra_errors),
        (Err(errs), true) => Err(errs),
        (Err(mut errs), false) => {
            errs.append(&mut extra_errors);
            Err(errs)
        }
    }
}

/// Validate a transaction with an optional set of registered pools.
///
/// When `registered_pools` is `Some`, pool re-registrations (updating an existing
/// pool's parameters) do not charge an additional deposit — only new pool
/// registrations do. When `None`, all pool registrations are treated as new
/// (deposit always charged).
///
/// When `registered_dreps` is `Some`, duplicate DRep registration certificates
/// (`RegDRep`) are rejected with [`ValidationError::DRepAlreadyRegistered`].
/// When `None`, the DRep re-registration check is skipped.
///
/// When `registered_vrf_keys` is `Some`, pool registration certificates that
/// declare a VRF key hash already held by another pool are rejected with
/// [`ValidationError::VrfKeyHashAlreadyRegistered`] (Conway+ only).
/// When `None`, the VRF key deduplication check is skipped.
///
/// When `committee_members` is `Some`, `CommitteeHotAuth` certificates for cold
/// credentials NOT present in the committee are rejected with
/// [`ValidationError::UnelectedCommitteeMember`] (Conway+ only).
/// When `None`, the committee membership check is skipped.
///
/// When `committee_resigned` is `Some`, `CommitteeHotAuth` certificates for cold
/// credentials that have previously resigned are rejected with
/// [`ValidationError::CommitteeHasPreviouslyResigned`] (Conway+ only).
/// When `None`, the resigned-member check is skipped.
///
/// The `utxo_set` parameter accepts anything that implements [`UtxoLookup`],
/// including the standard on-chain `&UtxoSet` and the composite
/// `CompositeUtxoView` used by the mempool validator for chained tx support.
///
/// The validation pipeline is:
/// 1. Phase-1 structural rules (Rules 1–10, 13–14) via [`phase1::run_phase1_rules`].
/// 2. For Plutus transactions: collateral rules (Rules 11, 11b, 11c) and
///    script data hash (Rule 12).
/// 3. Phase-2 Plutus script execution when all Phase-1 checks pass and redeemers
///    are present.
#[allow(clippy::too_many_arguments)] // validation entry point legitimately needs all context parameters
pub fn validate_transaction_with_pools(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    params: &ProtocolParameters,
    current_slot: u64,
    tx_size: u64,
    slot_config: Option<&SlotConfig>,
    registered_pools: Option<&HashSet<Hash28>>,
    current_treasury: Option<u64>,
    reward_accounts: Option<&ImblMap<Hash32, Lovelace>>,
    current_epoch: Option<u64>,
    registered_dreps: Option<&HashSet<Hash32>>,
    registered_vrf_keys: Option<&HashMap<Hash32, Hash28>>,
    node_network: Option<dugite_primitives::network::NetworkId>,
    committee_members: Option<&HashSet<Hash32>>,
    committee_resigned: Option<&HashSet<Hash32>>,
    stake_key_deposits: Option<&ImblMap<Hash32, u64>>,
    constitution_script_hash: Option<Hash28>,
    vote_delegations: Option<&HashSet<Hash32>>,
) -> Result<(), Vec<ValidationError>> {
    trace!(
        tx_hash = %tx.hash.to_hex(),
        inputs = tx.body.inputs.len(),
        outputs = tx.body.outputs.len(),
        fee = tx.body.fee.0,
        tx_size,
        current_slot,
        "Validation: validating transaction"
    );

    let mut errors = Vec::new();

    // ------------------------------------------------------------------
    // Phase-1 structural rules (Rules 1–10, 13–14)
    // ------------------------------------------------------------------
    phase1::run_phase1_rules(
        tx,
        utxo_set,
        params,
        current_slot,
        tx_size,
        registered_pools,
        current_epoch,
        node_network,
        stake_key_deposits,
        &mut errors,
    );

    // ------------------------------------------------------------------
    // Stake deregistration: non-zero reward account balance check
    //
    // Haskell `StakeKeyHasNonZeroAccountBalanceDELEG` (Shelley DELEG rule and
    // Conway DELEG rule): a stake credential may not be deregistered while its
    // reward account holds any lovelace. The delegator must withdraw rewards
    // before deregistering.
    //
    // This check is only enforced when `reward_accounts` is provided (i.e.,
    // during block validation or mempool admission with ledger context). During
    // simple structural validation where the caller supplies `None`, the balance
    // check is skipped to match the withdrawal-amount check pattern above.
    //
    // Both legacy `StakeDeregistration` (tag 1) and Conway
    // `ConwayStakeDeregistration` (tag 8) are covered — Haskell enforces the
    // same predicate for both certificate variants.
    // ------------------------------------------------------------------
    if let Some(accounts) = reward_accounts {
        // The LEDGER rule applies the same-tx withdrawal drain to the account
        // map BEFORE the DELEG/CERT sub-rules run, so a stake credential whose
        // reward account is fully withdrawn in the SAME tx has a zero balance by
        // the time the dereg `rewardCoin == Just mempty` predicate is evaluated.
        // A withdrawal MUST drain its account to exactly zero (Haskell
        // `testIncompleteAndMissingWithdrawals`), so any credential appearing in
        // this tx's withdrawals is zero-balance for the dereg check. This holds
        // in ALL eras: Shelley `LEDGER` drains before `DELEGS`
        // (`certState & certDStateL.accountsL %~ drainAccounts withdrawals`);
        // Conway drains in `Certs.hs` (PV9/10) and in `Ledger.hs` (PV11+), both
        // before `ConwayUnRegCert` reads the (already-drained) accounts map.
        // Without this, a same-tx withdraw+deregister (legacy tag-1 or Conway
        // tag-8) is wrongly rejected (mainnet Alonzo tx ca2f5ba3… class).
        let withdrawn_in_tx: HashSet<Hash32> = tx
            .body
            .withdrawals
            .keys()
            .filter_map(|ra| phase1::extract_reward_credential(ra))
            .map(|cred| cred.to_typed_hash32())
            .collect();
        for cert in &tx.body.certificates {
            let opt_credential: Option<&dugite_primitives::credentials::Credential> = match cert {
                dugite_primitives::transaction::Certificate::StakeDeregistration(cred) => {
                    Some(cred)
                }
                dugite_primitives::transaction::Certificate::ConwayStakeDeregistration {
                    credential,
                    ..
                } => Some(credential),
                _ => None,
            };
            if let Some(credential) = opt_credential {
                // Match the producer-side keying in `state::credential_to_hash`,
                // which uses `Credential::to_typed_hash32` so script and key
                // credentials with the same 28-byte hash do not collide
                // (Haskell `Credential 'Staking`).
                let key = credential.to_typed_hash32();
                if withdrawn_in_tx.contains(&key) {
                    // Same-tx withdrawal already drained this account to zero.
                    continue;
                }
                if let Some(balance) = accounts.get(&key) {
                    if balance.0 > 0 {
                        errors.push(ValidationError::StakeKeyHasNonZeroBalance {
                            credential_hash: key.to_hex(),
                            balance: balance.0,
                        });
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Stake key already registered check (Haskell `StakeKeyRegisteredDELEG`)
    //
    // A StakeRegistration or ConwayStakeRegistration certificate is rejected
    // when the named credential is already present in the reward accounts map
    // (i.e., the key has previously registered and not yet deregistered).
    //
    // This check is only enforced when `reward_accounts` is provided (block
    // validation mode). When `None`, the check is skipped to match the
    // pattern of other ledger-state-dependent checks (e.g. the balance check
    // above). Both the pre-Conway `StakeRegistration` (tag 0) and the Conway
    // `ConwayStakeRegistration` (tag 7) variants are covered.
    //
    // IMPORTANT — sequential / intra-tx overlay semantics:
    // Haskell applies certificates left-to-right against the EVOLVING
    // ledger state. A [dereg(C), reg(C)] sequence in the same transaction
    // must be accepted because the dereg clears the key before the re-reg.
    // Conversely, [reg(C), reg(C)] must be rejected even when C is not
    // pre-registered, because the first cert registers it intra-tx and the
    // second cert sees it as already registered.
    //
    // We track two overlay sets that shadow the `accounts` map:
    //   `in_tx_deregistered` — credentials deregistered by a prior cert in
    //     this tx; they are treated as absent from `accounts`.
    //   `in_tx_registered`   — credentials registered by a prior cert in
    //     this tx; they are treated as present even if absent from `accounts`.
    //
    // Reference: Haskell `StakeKeyRegisteredDELEG` /
    // `ConwayDRepAlreadyRegistered` in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg` and
    // `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Deleg`.
    // ------------------------------------------------------------------
    if let Some(accounts) = reward_accounts {
        let mut in_tx_deregistered: std::collections::HashSet<dugite_primitives::hash::Hash32> =
            std::collections::HashSet::new();
        let mut in_tx_registered: std::collections::HashSet<dugite_primitives::hash::Hash32> =
            std::collections::HashSet::new();

        for cert in &tx.body.certificates {
            // ----------------------------------------------------------
            // Track deregistrations: clear from in_tx_registered, add to
            // in_tx_deregistered. This makes the key "invisible" to
            // subsequent registration certs in the same tx.
            // ----------------------------------------------------------
            let opt_dereg_cred: Option<&dugite_primitives::credentials::Credential> = match cert {
                dugite_primitives::transaction::Certificate::StakeDeregistration(cred) => {
                    Some(cred)
                }
                dugite_primitives::transaction::Certificate::ConwayStakeDeregistration {
                    credential,
                    ..
                } => Some(credential),
                _ => None,
            };
            if let Some(credential) = opt_dereg_cred {
                let key = credential.to_typed_hash32();
                // #748: a deregistration of a credential that is NOT
                // registered at this point in the left-to-right walk is
                // rejected by Haskell (`StakeKeyNotRegisteredDELEG`,
                // ShelleyUnRegCert/ConwayUnRegCert). Without this, dugite's
                // mempool admits the tx, a dugite BP forges a Haskell-invalid
                // block, and the apply path refunds a deposit never paid.
                let is_currently_registered = (accounts.contains_key(&key)
                    && !in_tx_deregistered.contains(&key))
                    || in_tx_registered.contains(&key);
                if !is_currently_registered {
                    errors.push(ValidationError::StakeKeyNotRegisteredForDeregistration {
                        credential_hash: key.to_hex(),
                    });
                }
                in_tx_deregistered.insert(key);
                in_tx_registered.remove(&key);
                continue;
            }

            // ----------------------------------------------------------
            // Check and track registrations.
            // ----------------------------------------------------------
            let opt_reg_cred: Option<&dugite_primitives::credentials::Credential> = match cert {
                dugite_primitives::transaction::Certificate::StakeRegistration(cred) => Some(cred),
                dugite_primitives::transaction::Certificate::ConwayStakeRegistration {
                    credential: cred,
                    ..
                } => Some(cred),
                // Combined registration certificates also register a stake key
                // and must be rejected if the credential is already registered.
                // Reference: Haskell `AlreadyRegisteredKey` in Conway DELEG rule.
                dugite_primitives::transaction::Certificate::RegStakeDeleg {
                    credential: cred,
                    ..
                } => Some(cred),
                dugite_primitives::transaction::Certificate::VoteRegDeleg {
                    credential: cred,
                    ..
                } => Some(cred),
                dugite_primitives::transaction::Certificate::RegStakeVoteDeleg {
                    credential: cred,
                    ..
                } => Some(cred),
                _ => None,
            };
            if let Some(credential) = opt_reg_cred {
                // Mirror `state::credential_to_hash` — `to_typed_hash32` so the
                // lookup key matches the kind-tagged storage form.
                let key = credential.to_typed_hash32();

                // A credential is "currently registered" if:
                //   (a) it is in the pre-tx accounts map AND was not deregistered
                //       by a prior cert in this tx, OR
                //   (b) it was registered by a prior cert in this tx.
                let is_currently_registered = (accounts.contains_key(&key)
                    && !in_tx_deregistered.contains(&key))
                    || in_tx_registered.contains(&key);

                if is_currently_registered {
                    errors.push(ValidationError::StakeKeyAlreadyRegistered {
                        credential_hash: key.to_hex(),
                    });
                }

                // Record that this cert registers the key for subsequent certs.
                in_tx_registered.insert(key);
                in_tx_deregistered.remove(&key);
            }
        }
    }

    // ------------------------------------------------------------------
    // Delegation to unregistered pool (Haskell `DelegateeStakePoolNotRegisteredDELEG`)
    // + Retirement of unregistered pool (Haskell `StakePoolNotRegisteredOnKeyPOOL`)
    //
    // Each certificate in a Conway transaction is processed sequentially by
    // the ledger CERTS rule: every cert sees the **evolving** pool registry,
    // not the pre-tx snapshot. The standard idiom is one tx that contains a
    // `PoolRegistration` followed by a `StakeDelegation` to the just-
    // registered pool — `cardano-cli stake-pool registration-certificate`
    // emits exactly this pair. Checking against the static pre-tx
    // `registered_pools` set would reject that legitimate use case and let
    // illegitimate "retire a never-registered pool" txs through; we mirror
    // the Haskell sequential semantics here so dugite admission matches
    // what cardano-node accepts and rejects.
    //
    // Covered cert variants and the predicate they fire:
    //   * `StakeDelegation`     (tag 2 ) → `DelegateeStakePoolNotRegisteredDELEG`
    //   * `RegStakeDeleg`       (tag 11) → same
    //   * `StakeVoteDelegation` (tag 13) → same
    //   * `RegStakeVoteDeleg`   (tag 14) → same
    //   * `PoolRetirement`              → `StakePoolNotRegisteredOnKeyPOOL`
    //
    // `VoteRegDeleg` (tag 15) does NOT include a pool delegation component —
    // it registers and sets a DRep vote delegation only — so it is excluded.
    //
    // This check is only enforced when `registered_pools` is provided.
    //
    // Reference: Haskell `DelegateeStakePoolNotRegisteredDELEG` in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg` and
    // `StakePoolNotRegisteredOnKeyPOOL` in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Pool`.
    // ------------------------------------------------------------------
    if let Some(pools) = registered_pools {
        // Previously this cloned the entire `pools` set (~683 entries on
        // preview at epoch 1309) on every transaction — a deep allocation
        // + memcpy in the per-tx validation hot path.  Track only the
        // delta: pool IDs newly registered within THIS tx.  A typical tx
        // has zero pool-registration certs, so this set is almost always
        // empty.  Membership check becomes
        // `pools.contains(target) || new_pools.contains(target)` —
        // O(1) on both sides.
        let mut new_pools: std::collections::HashSet<Hash28> = std::collections::HashSet::new();
        for cert in &tx.body.certificates {
            // Pool registration adds to the per-tx delta BEFORE we check
            // subsequent delegations/retirements in this tx.
            if let dugite_primitives::transaction::Certificate::PoolRegistration(params) = cert {
                new_pools.insert(params.operator);
                continue;
            }

            let opt_target: Option<Hash28> = match cert {
                dugite_primitives::transaction::Certificate::StakeDelegation {
                    pool_hash, ..
                } => Some(*pool_hash),
                dugite_primitives::transaction::Certificate::RegStakeDeleg {
                    pool_hash, ..
                } => Some(*pool_hash),
                dugite_primitives::transaction::Certificate::StakeVoteDelegation {
                    pool_hash,
                    ..
                } => Some(*pool_hash),
                dugite_primitives::transaction::Certificate::RegStakeVoteDeleg {
                    pool_hash,
                    ..
                } => Some(*pool_hash),
                dugite_primitives::transaction::Certificate::PoolRetirement {
                    pool_hash, ..
                } => Some(*pool_hash),
                _ => None,
            };
            if let Some(pool_id) = opt_target {
                if !pools.contains(&pool_id) && !new_pools.contains(&pool_id) {
                    errors.push(ValidationError::DelegateePoolNotRegistered {
                        pool_id: pool_id.to_hex(),
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // DelegateeDRepNotRegisteredDELEG (Conway DELEG rule, PV >= 10)
    //
    // A vote-delegation certificate that points to a specific DRep
    // (KeyHash or ScriptHash) is rejected when that DRep credential is
    // not present in `vState.dsDReps`.  The synthetic targets
    // `AlwaysAbstain` and `AlwaysNoConfidence` are exempt — they are
    // built-in and have no registry entry.
    //
    // This check applies to certs that include a DRep delegation target:
    //   * `VoteDelegation`      (tag  9)
    //   * `StakeVoteDelegation` (tag 13)
    //   * `RegStakeVoteDeleg`   (tag 14)
    //   * `VoteRegDeleg`        (tag 15)
    //
    // Haskell semantics (Conway DELEG rule): certs are applied left-to-right
    // against the evolving CertState.  A `RegDRep` cert earlier in the same
    // tx inserts the credential into `vsDReps` before the delegation cert is
    // evaluated, so the checks below honour the same sequential semantics
    // (tracking a per-tx `new_dreps` delta set).
    //
    // This check is SKIPPED during the PV9 Conway bootstrap phase and only
    // enforced at protocol >= 10 (when `registered_dreps` is provided). Haskell
    // `Cardano.Ledger.Conway.Rules.Deleg.checkDRepRegistered`:
    //   unless (hardforkConwayBootstrapPhase pv) $
    //     targetDRep `Map.member` dReps ?! DelegateeDRepNotRegisteredDELEG
    // with `hardforkConwayBootstrapPhase pv = pvMajor pv == 9`. So during PV9 a
    // vote-delegation to a not-yet-registered DRep — including the on-chain
    // self-register-then-self-delegate pattern (VoteDelegation cert[0] +
    // RegDRep cert[1] in one tx, which the left-to-right new_dreps tracking
    // below would otherwise reject) — is ACCEPTED. Gating this at >= 9 wrongly
    // rejected 26 such mainnet txs at PV9 (epoch 507+).
    //
    // Reference: Haskell `DelegateeDRepNotRegisteredDELEG` in
    // `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Deleg`.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 10 {
        if let Some(dreps) = registered_dreps {
            // Track DRep credentials registered within THIS tx (RegDRep cert
            // preceding a VoteDelegation in the same tx is valid).
            let mut new_dreps: std::collections::HashSet<dugite_primitives::hash::Hash32> =
                std::collections::HashSet::new();
            for cert in &tx.body.certificates {
                // A RegDRep cert expands the per-tx set before delegation checks.
                if let dugite_primitives::transaction::Certificate::RegDRep { credential, .. } =
                    cert
                {
                    new_dreps.insert(credential.to_typed_hash32());
                    continue;
                }
                // Extract the DRep target from certs that carry one.
                let opt_drep: Option<&dugite_primitives::transaction::DRep> = match cert {
                    dugite_primitives::transaction::Certificate::VoteDelegation {
                        drep, ..
                    } => Some(drep),
                    dugite_primitives::transaction::Certificate::StakeVoteDelegation {
                        drep,
                        ..
                    } => Some(drep),
                    dugite_primitives::transaction::Certificate::RegStakeVoteDeleg {
                        drep, ..
                    } => Some(drep),
                    dugite_primitives::transaction::Certificate::VoteRegDeleg { drep, .. } => {
                        Some(drep)
                    }
                    _ => None,
                };
                if let Some(drep_target) = opt_drep {
                    // `credential_hash32()` returns None for the synthetic
                    // pseudo-DReps (Abstain, NoConfidence) and Some for
                    // KeyHash/ScriptHash — exactly the split we need.
                    if let Some(drep_key) = drep_target.credential_hash32() {
                        if !dreps.contains(&drep_key) && !new_dreps.contains(&drep_key) {
                            errors.push(ValidationError::DelegateeDRepNotRegistered {
                                drep_id: drep_key.to_hex(),
                            });
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // StakeKeyNotRegisteredDELEG (Shelley DELEG rule, all eras)
    //
    // A delegation certificate that delegates on behalf of a stake
    // credential requires that credential to be registered in the ledger
    // (i.e. present in the reward-accounts map / `dsUnified`).  Without
    // this check, a transaction can nominate an arbitrary stake credential
    // as the delegator — the CERTS rule would fail to find the account,
    // but dugite would silently skip the error and forge the block anyway.
    //
    // Covered cert variants (pure delegation — stake key must already exist):
    //   * `StakeDelegation`     (tag  2) — pool delegation
    //   * `VoteDelegation`      (tag  9) — DRep delegation
    //   * `StakeVoteDelegation` (tag 13) — pool + DRep delegation
    //
    // Combined registration+delegation certs (`RegStakeDeleg` tag 11,
    // `RegStakeVoteDeleg` tag 14, `VoteRegDeleg` tag 15) are EXEMPT because
    // they register the credential atomically — they fire
    // `StakeKeyRegisteredDELEG` (above) if the key is ALREADY registered.
    //
    // This check is only enforced when `reward_accounts` is provided
    // (block-apply / mempool context with full ledger state).
    //
    // Reference: Haskell `StakeKeyNotRegisteredDELEG` in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg`.
    // ------------------------------------------------------------------
    if let Some(accounts) = reward_accounts {
        // Track stake credentials registered within THIS tx via
        // StakeRegistration / ConwayStakeRegistration / Reg* combo certs,
        // so that a same-tx register-then-delegate sequence is accepted.
        //
        // Intra-tx DELEGS sequencing (#746 follow-up): a same-tx
        // DEREGISTRATION must also make the credential invisible to LATER
        // delegation certs — Haskell applies certs left-to-right against
        // evolving state, so [dereg(C), delegate(C)] fires
        // `StakeKeyNotRegisteredDELEG` even when C was registered pre-tx,
        // and [reg(C), dereg(C), voteDeleg(C)] likewise. Track deregs in
        // `dropped_stake_keys` and prune `new_stake_keys` symmetrically.
        let mut new_stake_keys: std::collections::HashSet<dugite_primitives::hash::Hash32> =
            std::collections::HashSet::new();
        let mut dropped_stake_keys: std::collections::HashSet<dugite_primitives::hash::Hash32> =
            std::collections::HashSet::new();
        for cert in &tx.body.certificates {
            // Track deregistrations FIRST (left-to-right evolution): a dereg
            // removes the credential from the evolving registered set.
            let opt_dereg_cred: Option<&dugite_primitives::credentials::Credential> = match cert {
                dugite_primitives::transaction::Certificate::StakeDeregistration(c) => Some(c),
                dugite_primitives::transaction::Certificate::ConwayStakeDeregistration {
                    credential: c,
                    ..
                } => Some(c),
                _ => None,
            };
            if let Some(cred) = opt_dereg_cred {
                let key = cred.to_typed_hash32();
                dropped_stake_keys.insert(key);
                new_stake_keys.remove(&key);
                continue;
            }

            // Collect credentials registered by pure or combo registration certs.
            let opt_reg_cred: Option<&dugite_primitives::credentials::Credential> = match cert {
                dugite_primitives::transaction::Certificate::StakeRegistration(c) => Some(c),
                dugite_primitives::transaction::Certificate::ConwayStakeRegistration {
                    credential: c,
                    ..
                } => Some(c),
                // Reg* combo certs register + delegate atomically, so they
                // also expand the per-tx registered-key set.
                dugite_primitives::transaction::Certificate::RegStakeDeleg {
                    credential: c,
                    ..
                } => Some(c),
                dugite_primitives::transaction::Certificate::RegStakeVoteDeleg {
                    credential: c,
                    ..
                } => Some(c),
                dugite_primitives::transaction::Certificate::VoteRegDeleg {
                    credential: c, ..
                } => Some(c),
                _ => None,
            };
            if let Some(cred) = opt_reg_cred {
                let key = cred.to_typed_hash32();
                new_stake_keys.insert(key);
                // A re-registration after a same-tx dereg makes the
                // credential visible again ([dereg, reg, delegate] is the
                // legal mainnet pattern).
                dropped_stake_keys.remove(&key);
                // Don't `continue` here — we still need to check delegation
                // certs later in the loop.
            }

            // Extract the stake credential from pure-delegation certs.
            let opt_deleg_cred: Option<&dugite_primitives::credentials::Credential> = match cert {
                dugite_primitives::transaction::Certificate::StakeDelegation {
                    credential, ..
                } => Some(credential),
                dugite_primitives::transaction::Certificate::VoteDelegation {
                    credential, ..
                } => Some(credential),
                dugite_primitives::transaction::Certificate::StakeVoteDelegation {
                    credential,
                    ..
                } => Some(credential),
                _ => None,
            };
            if let Some(credential) = opt_deleg_cred {
                let key = credential.to_typed_hash32();
                // Registered at THIS point in the left-to-right cert walk:
                // pre-tx registered AND not since deregistered in this tx,
                // OR registered earlier in this tx (and not re-dropped —
                // `new_stake_keys` is pruned by the dereg arm above).
                let is_registered = (accounts.contains_key(&key)
                    && !dropped_stake_keys.contains(&key))
                    || new_stake_keys.contains(&key);
                if !is_registered {
                    errors.push(ValidationError::StakeKeyNotRegisteredForDelegation {
                        credential_hash: key.to_hex(),
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // DRep already registered check (Haskell `ConwayDRepAlreadyRegistered`)
    //
    // A `RegDRep` certificate is rejected when the named DRep credential is
    // already present in the DRep registry. This check is only enforced in
    // Conway (protocol >= 9) when `registered_dreps` is provided.
    //
    // Reference: Haskell `ConwayDRepAlreadyRegistered` in
    // `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Deleg`.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
        if let Some(dreps) = registered_dreps {
            for cert in &tx.body.certificates {
                if let dugite_primitives::transaction::Certificate::RegDRep { credential, .. } =
                    cert
                {
                    // `to_typed_hash32` matches the kind-tagged keys used by
                    // the DRep registry — without this, a script DRep with the
                    // same 28-byte hash as a key DRep would falsely collide.
                    let key = credential.to_typed_hash32();
                    if dreps.contains(&key) {
                        errors.push(ValidationError::DRepAlreadyRegistered {
                            credential_hash: key.to_hex(),
                        });
                    }
                }
            }
        }

        // ------------------------------------------------------------------
        // DRep not registered check (Haskell `ConwayDRepNotRegistered`)
        //
        // An `UnregDRep` certificate is rejected when the named DRep credential
        // is NOT present in the DRep registry.  Without this check, the deposit
        // accounting in `calculate_deposits_and_refunds` would credit the
        // `refund` amount from the certificate even though no deposit was ever
        // made — effectively minting ADA from nothing.
        //
        // This check is symmetric to the `ConwayDRepAlreadyRegistered` check
        // above (RegDRep must NOT be registered; UnregDRep MUST be registered).
        //
        // Reference: Haskell `ConwayDRepNotRegistered` in
        // `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.GovCert`.
        // ------------------------------------------------------------------
        if let Some(dreps) = registered_dreps {
            for cert in &tx.body.certificates {
                if let dugite_primitives::transaction::Certificate::UnregDRep {
                    credential, ..
                } = cert
                {
                    let key = credential.to_typed_hash32();
                    if !dreps.contains(&key) {
                        errors.push(ValidationError::DRepNotRegistered {
                            credential_hash: key.to_hex(),
                        });
                    }
                }
            }
        }

        // (ConwayDRepIncorrectRefund predicate is enforced in
        //  `validate_transaction_with_context` where `drep_deposits` is in
        //  scope — see the comment block there.)

        // ------------------------------------------------------------------
        // DRep deposit amount validation (Haskell `ConwayDRepIncorrectDeposit`)
        //
        // Each `RegDRep` certificate's inline deposit must exactly match the
        // current `drep_deposit` protocol parameter. Value conservation uses
        // the declared deposit for accounting, but the GOVCERT rule separately
        // validates that it equals the parameter.
        //
        // Reference: Haskell `ConwayDRepIncorrectDeposit` in
        // `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.GovCert`.
        // ------------------------------------------------------------------
        for cert in &tx.body.certificates {
            if let dugite_primitives::transaction::Certificate::RegDRep { deposit, .. } = cert {
                if *deposit != params.drep_deposit {
                    errors.push(ValidationError::DRepIncorrectDeposit {
                        declared: deposit.0,
                        expected: params.drep_deposit.0,
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // VRF key deduplication (Haskell `VRFKeyHashAlreadyRegistered`, Conway+)
    //
    // From protocol version 11 onward, a pool registration certificate whose
    // VRF key hash is already registered to a DIFFERENT pool is rejected. A pool
    // re-registering its own parameters with the same VRF key is permitted (the
    // key already belongs to that pool, so the new registration is not a
    // collision).
    //
    // The gate is PV >= 11, NOT >= 9: Haskell
    //   hardforkConwayDisallowDuplicatedVRFKeys pv = pvMajor pv > natVersion @10
    // (eras/shelley/impl/src/Cardano/Ledger/Shelley/Era.hs), consulted in the
    // RegPool branch of Shelley.Rules.Pool. The check is INACTIVE at PV 9 and
    // PV 10 — the whole Conway bootstrap + current mainnet — so two genuinely
    // different pools (e.g. same operator) may legitimately share a VRF key
    // until PV 11. Gating at >= 9 falsely rejected mainnet tx 054c270b… at
    // epoch 523 (PV 9.0), which cardano-node accepts.
    //
    // This check is only enforced when `registered_vrf_keys` is provided (block
    // validation mode). The map is keyed by VRF key hash (Hash32) and maps to
    // the pool ID (Hash28) that currently holds that key. (NOTE: at PV 11 the
    // Haskell `psVRFKeyHashes` is a refcount map and a retiring pool keeps its
    // key until POOLREAP — a refcount model will be needed then; mainnet is not
    // yet at PV 11.)
    //
    // Reference: Haskell `VRFKeyHashAlreadyRegistered` in
    // `cardano-ledger:Cardano.Ledger.Shelley.Rules.Pool` (reused by Conway).
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 11 {
        if let Some(vrf_keys) = registered_vrf_keys {
            for cert in &tx.body.certificates {
                if let dugite_primitives::transaction::Certificate::PoolRegistration(pool_params) =
                    cert
                {
                    // Check if this VRF key is held by a different pool.
                    // Same pool re-registering with the same key is fine.
                    if let Some(&existing_pool) = vrf_keys.get(&pool_params.vrf_keyhash) {
                        if existing_pool != pool_params.operator {
                            errors.push(ValidationError::VrfKeyHashAlreadyRegistered {
                                vrf_keyhash: pool_params.vrf_keyhash.to_hex(),
                                existing_pool_id: existing_pool.to_hex(),
                            });
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // CommitteeHotAuth: elected-member and non-resigned checks (Conway+)
    //
    // Haskell `CERT` rule predicates in
    // `Cardano.Ledger.Conway.Rules.Cert`:
    //
    //   1. "failOnNonEmpty unelected": every cold credential in a
    //      CommitteeHotAuth certificate must appear in the current
    //      committee (committee_expiration / committee_members map).
    //      → `ValidationError::UnelectedCommitteeMember`
    //
    //   2. "membersResigned ∩ {coldKey} = ∅": a cold credential that has
    //      previously resigned via CommitteeColdResign may not re-authorize
    //      a hot key without being re-elected.
    //      → `ValidationError::CommitteeHasPreviouslyResigned`
    //
    // Both checks are only enforced in Conway (protocol >= 9) and only
    // when the relevant state is provided (block application mode).
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
        for cert in &tx.body.certificates {
            if let dugite_primitives::transaction::Certificate::CommitteeHotAuth {
                cold_credential,
                ..
            } = cert
            {
                // Match the producer-side keying for `committee_expiration` /
                // `committee_resigned`, which use `Credential::to_typed_hash32`.
                let cold_key = cold_credential.to_typed_hash32();

                // Check 1: cold credential must be a current CC member.
                if let Some(members) = committee_members {
                    if !members.contains(&cold_key) {
                        errors.push(ValidationError::UnelectedCommitteeMember {
                            cold_credential_hash: cold_key.to_hex(),
                        });
                    }
                }

                // Check 2: cold credential must not have previously resigned.
                if let Some(resigned) = committee_resigned {
                    if resigned.contains(&cold_key) {
                        errors.push(ValidationError::CommitteeHasPreviouslyResigned {
                            cold_credential_hash: cold_key.to_hex(),
                        });
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Withdrawal validation.
    //
    // There is NO "zero-amount withdrawal is invalid" predicate in
    // cardano-ledger in ANY era. The ONLY withdrawal balance rule is that each
    // withdrawal amount must EQUAL the account's current reward balance, so a
    // zero withdrawal of a registered zero-balance account is valid in every
    // era. The previous `wdrlNotZero` gate (reject amount==0 pre-Conway) was
    // fabricated and wrongly rejected on-chain Alonzo txs (mainnet tx
    // fc7ca745… class).
    //
    // Haskell, DELEGS `Empty` case
    // (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Delegs.hs`):
    //   validateZeroRewards dState (Withdrawals wdrls) network =
    //     failureUnless (isSubmapOfUM wdrls (rewards dState)) ...
    // `isSubmapOfUM` is pure amount==balance equality (`coin1 == fromCompact
    // coin2`), which accepts a 0==0 pair. The amount-equals-balance /
    // account-registered check below (`withdrawals_that_do_not_drain_accounts`,
    // the `isSubmapOfUM` equivalent, block-application mode only) is the
    // complete and correct, era-invariant rule.
    //
    // (`ValidationError::ZeroWithdrawal` is retained for the N2C
    // `serve.rs` error-mapping API but is no longer constructed.)
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // Conway `WithdrawalsNotInRewardsCERTS` (PV ≤ 10) — split into
    // `ConwayWithdrawalsMissingAccounts` + `ConwayIncompleteWithdrawals`
    // for PV ≥ 11.
    //
    // Reference (PV ≤ 10): Haskell
    //   `Cardano.Ledger.Conway.Rules.Certs.conwayCertsTransition` /
    //   `withdrawalsThatDoNotDrainAccounts`.
    // Reference (PV ≥ 11): Haskell
    //   `Cardano.Ledger.Conway.Rules.Ledger.testIncompleteAndMissingWithdrawals`
    //   after `hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule`.
    //
    // Only enforced when both `node_network` and `reward_accounts` are
    // available (block-application context).
    // ------------------------------------------------------------------
    if let (Some(net), Some(accounts)) = (node_network, reward_accounts) {
        if let Some(split) = withdrawals::withdrawals_that_do_not_drain_accounts(
            &tx.body.withdrawals,
            net.to_u8(),
            accounts,
        ) {
            if params.protocol_version_major <= 10 {
                let mut bad = split.missing.clone();
                bad.extend(split.incomplete.iter().map(|(a, v, _)| (a.clone(), *v)));
                errors.push(ValidationError::WithdrawalsNotInRewardsCERTS { bad });
            } else {
                if !split.missing.is_empty() {
                    errors.push(ValidationError::ConwayWithdrawalsMissingAccounts {
                        missing: split.missing,
                    });
                }
                if !split.incomplete.is_empty() {
                    errors.push(ValidationError::ConwayIncompleteWithdrawals {
                        incomplete: split.incomplete,
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Conway LEDGER rule: ConwayWdrlNotDelegatedToDRep (PV >= 10)
    //
    // Every KeyHash reward account making a withdrawal must have a DRep
    // delegation. Script-credential accounts are exempt. Any delegation
    // value (including AlwaysAbstain/AlwaysNoConfidence) satisfies the check.
    // Uses the certState BEFORE the current tx's certificates are applied.
    //
    // Reference: Haskell `validateWithdrawalsDelegated` in
    // `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Ledger`.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 10 {
        if let Some(delegations) = vote_delegations {
            for reward_addr in tx.body.withdrawals.keys() {
                if reward_addr.len() < 29 {
                    continue;
                }
                let header = reward_addr[0];
                // Script-credential reward accounts (header bit 4 set) are exempt
                let is_script = (header & 0x10) != 0;
                if is_script {
                    continue;
                }
                // KeyHash credential — must have DRep delegation
                if let Ok(cred_hash) = Hash28::try_from(&reward_addr[1..29]) {
                    let key = cred_hash.to_hash32_padded();
                    if !delegations.contains(&key) {
                        errors.push(ValidationError::WdrlNotDelegatedToDRep {
                            credential_hash: key.to_hex(),
                        });
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Conway LEDGER rule: currentTreasuryValue must match ledger treasury.
    // This prevents mempool admission of transactions with stale/wrong
    // treasury assertions, which would cause forged blocks to be rejected.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
        if let (Some(declared), Some(actual)) = (tx.body.treasury_value.as_ref(), current_treasury)
        {
            if declared.0 != actual {
                errors.push(ValidationError::TreasuryValueMismatch {
                    declared: declared.0,
                    actual,
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // Conway GOV rule: constitution guardrail policy_hash validation.
    //
    // ParameterChange and TreasuryWithdrawals proposals must carry a
    // `policy_hash` matching the constitution's guardrail script hash.
    // Without this check, a transaction could reference an arbitrary script
    // (or omit the policy_hash entirely), bypassing the guardrail.
    //
    // Reference: Haskell GOV rule — policy hash must match the constitution's
    // script hash for governed governance actions.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
        if let Some(required_hash) = constitution_script_hash {
            for (idx, proposal) in tx.body.proposal_procedures.iter().enumerate() {
                let policy_hash = match &proposal.gov_action {
                    GovAction::ParameterChange { policy_hash, .. }
                    | GovAction::TreasuryWithdrawals { policy_hash, .. } => policy_hash.as_ref(),
                    _ => continue,
                };
                match policy_hash {
                    Some(provided) if *provided == required_hash => {
                        // Valid — policy hash matches constitution guardrail
                    }
                    Some(provided) => {
                        errors.push(ValidationError::ConstitutionPolicyMismatch {
                            expected: required_hash.to_hex(),
                            actual: provided.to_hex(),
                        });
                    }
                    None => {
                        errors.push(ValidationError::ConstitutionPolicyMismatch {
                            expected: required_hash.to_hex(),
                            actual: format!("None (proposal index {idx})"),
                        });
                    }
                }
            }
        }
    }

    // ppuWellFormed check for ParameterChange proposals (Conway GOV rule)
    conway::check_pparam_update_well_formed(params, &tx.body, &mut errors);

    // Babbage+ UTXOW: every reference script attached to an output PRODUCED
    // by this transaction must pass `validScript pv` — Plutus scripts must
    // decode and their language must be supported at the current PV. This
    // check runs UNCONDITIONALLY (not gated on `has_plutus_scripts`) because
    // a tx can attach a malformed Plutus ref-script to an output without
    // carrying any witness Plutus script of its own. Matches Haskell's
    // `MalformedReferenceScripts` predicate.
    scripts::check_malformed_reference_scripts(tx, params, &mut errors);

    // Rule 12: script data hash (mkScriptIntegrity) — covers redeemers,
    // datums, cost models, and language versions. Runs UNCONDITIONALLY
    // (not gated on `has_plutus_scripts`, issue #790): a tx with ONLY
    // supplemental witness datums (no witness Plutus scripts, no
    // redeemers) still needs `script_data_hash = hash(dats)` validated,
    // and `has_plutus_scripts` does not count `plutus_data`. The function
    // itself already self-gates on `has_redeemers || has_datums` /
    // `body.script_data_hash.is_some()`, so this is a no-op for ordinary
    // vkey-only txs.
    scripts::check_script_data_hash(tx, utxo_set, params, &mut errors);

    // Babbage/Conway UTXOW: scripts in the witness set that are not needed
    // by any script purpose are rejected as extraneous. Runs
    // UNCONDITIONALLY (not gated on `has_plutus_scripts`, issue #791):
    // Haskell's `ExtraneousScriptWitnessesUTXOW` covers ALL witness
    // scripts, native included, so a tx with only an unneeded native
    // witness script (no Plutus content at all) must still be checked.
    // The function self-gates on "has any witness script" internally.
    scripts::check_extraneous_script_witnesses(tx, utxo_set, &mut errors);

    // ------------------------------------------------------------------
    // Rules 11, 11b, 11c — Plutus-transaction-specific checks
    //
    // These are only enforced when the transaction includes Plutus scripts
    // or redeemers. They are split into their own modules to keep the rule
    // logic focused and independently testable.
    // ------------------------------------------------------------------
    if scripts::has_plutus_scripts(tx) {
        // Rule 11: collateral inputs, percentage, net-ADA check, total_collateral
        // Rule 11b: redeemer index bounds
        collateral::check_collateral(tx, utxo_set, params, &mut errors);

        // Rule 11c: every Plutus-script-locked input/withdrawal and every Plutus
        // minting policy must have a matching redeemer (Spend/Reward/Mint).
        // Native-script-locked inputs are exempt.  Matches Haskell's
        // `hasExactSetOfRedeemers` / `neededPlutusSet` filter.
        let script_versions_for_redeemers = collateral::plutus_script_version_map(tx, utxo_set);
        collateral::check_script_redeemers(
            tx,
            utxo_set,
            &script_versions_for_redeemers,
            &mut errors,
        );

        // Alonzo UTXOW: every redeemer in the witness set must map to a valid
        // script purpose. Redeemers with no matching purpose are rejected.
        // Matches Haskell's `hasExactSetOfRedeemers` / `ExtraRedeemers`.
        collateral::check_extra_redeemers(
            tx,
            utxo_set,
            &script_versions_for_redeemers,
            &mut errors,
        );

        // Babbage+ UTXOW: every script in the witness set must pass
        // `validScript pv script` — Plutus scripts must decode and their
        // language must be supported at the current PV; native scripts are
        // trivially OK once decoded. Matches Haskell's
        // `MalformedScriptWitnesses` predicate.
        scripts::check_malformed_script_witnesses(tx, params, &mut errors);

        // (MalformedReferenceScripts, check_script_data_hash, and
        //  check_extraneous_script_witnesses are all enforced
        //  unconditionally above, not just when the tx carries witness
        //  Plutus scripts / redeemers.)

        // ------------------------------------------------------------------
        // Phase-2: Execute Plutus scripts when redeemers are present.
        //
        // `slot_config` is required (Plutus time conversion needs the
        // network's epoch zero anchors).  `raw_cbor` is preferred but no
        // longer required: `evaluate_plutus_scripts` falls back to a
        // deterministic re-encoding of the in-memory `Transaction` for
        // locally-built txs (see `plutus::evaluate_plutus_scripts`).  The
        // caller therefore only fails fast on a missing slot config.
        // ------------------------------------------------------------------
        let has_redeemers = !tx.witness_set.redeemers.is_empty();

        // Phase-2 V3/V4 TxInfo translation check (Haskell PR #5011, and
        // issue #1000 for the V4 half):
        //
        // At PV >= 11, the phase-1 `BabbageNonDisjointRefInputs` check is
        // relaxed (see phase1.rs Rule 9).  An equivalent check is moved
        // into PlutusV3 `TxInfo` construction: if any redeemer executes a
        // V3 (or, from Dijkstra, V4 — see below) script AND
        // `inputs ∩ reference_inputs` is non-empty, the translation fails
        // with `ConwayContextError::ReferenceInputsNotDisjointFromInputs`.
        //
        // V1/V2/native scripts (and txs with NO V3/V4 redeemer) are
        // accepted with overlap — this is the intended relaxation.
        //
        // PlutusV4 inclusion is oracle-verified, not inferred from
        // `ScriptLanguage`'s "V4 is V3 semantics" doc comment alone:
        // `IntersectMBO/cardano-ledger` @
        // `4849c13d6f70e5ab46add9af6e0ec5c537b61f69`,
        // `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/TxInfo.hs`, BOTH
        // `instance EraPlutusTxInfo 'PlutusV3 DijkstraEra` (line 391) and
        // `instance EraPlutusTxInfo 'PlutusV4 DijkstraEra` (line 519,
        // `mkAnyLevelTxInfo`, line 566) call
        // `Conway.checkReferenceInputsNotDisjointFromInputs txBody`
        // unconditionally (no `when (pvMajor >= 11)` guard — Dijkstra is
        // always PV >= 12, so the Conway-era guard is always true there and
        // was simplified away). This holds even though
        // `EraPlutusTxInfo 'PlutusV4 DijkstraEra`'s `toPlutusScriptPurpose`
        // is still `error "stub: PlutusV4 not yet implemented"` upstream —
        // the disjointness check runs inside `toPlutusTxInfo`, which builds
        // `TxInfo` BEFORE any per-redeemer `toPlutusScriptPurpose` call, so
        // it is reached (and would fire) independently of that stub.
        //
        // This check is independent of other phase-1 errors (it is a pure
        // structural property of `inputs` vs `reference_inputs`), so we run
        // it before the `errors.is_empty()` gate that guards the uplc
        // evaluator (which depends on a clean phase-1 state).
        if has_redeemers
            && params.protocol_version_major >= 11
            && !tx.body.reference_inputs.is_empty()
        {
            let version_map = crate::validation::plutus_script_version_map(tx, utxo_set);
            let redeemer_versions =
                crate::validation::redeemer_script_version_map(tx, utxo_set, &version_map);
            let any_v3_or_v4_executed = redeemer_versions.values().any(|&v| v == 3 || v == 4);
            if any_v3_or_v4_executed {
                let input_set: std::collections::HashSet<_> = tx.body.inputs.iter().collect();
                let mut common: Vec<&dugite_primitives::transaction::TransactionInput> = tx
                    .body
                    .reference_inputs
                    .iter()
                    .filter(|r| input_set.contains(*r))
                    .collect();
                if !common.is_empty() {
                    // Deterministic order for the surfaced TxIn list:
                    // sort by (transaction_id, index) — matches the Haskell
                    // `Set.intersection` traversal order.
                    common.sort_by(|a, b| {
                        a.transaction_id
                            .cmp(&b.transaction_id)
                            .then(a.index.cmp(&b.index))
                    });
                    let offenders: Vec<String> = common.iter().map(|i| i.to_string()).collect();
                    errors.push(ValidationError::ReferenceInputsNotDisjointFromInputs(
                        offenders,
                    ));
                }
            }
        }

        if errors.is_empty() && has_redeemers {
            // ── #826 / #860.3: NoCostModel collection check ───────────────
            //
            // Haskell `collectPlutusScriptsWithContext` fails with
            // `CollectErrors [NoCostModel lang]` — rejecting the transaction
            // REGARDLESS of `isValid`, before any script is evaluated — when a
            // Plutus script that is actually EXECUTED (has a matching redeemer)
            // uses a language that has no cost model in the protocol parameters.
            // dugite previously fell back silently to the uplc-side reference
            // cost model, accepting a transaction cardano-node rejects. This is a
            // *collection* error, so it runs before the (possibly-skipped) eval.
            {
                let version_map = crate::validation::plutus_script_version_map(tx, utxo_set);
                let executed =
                    crate::validation::redeemer_script_version_map(tx, utxo_set, &version_map);
                let missing =
                    missing_cost_model_languages(executed.values().copied(), &params.cost_models);
                if !missing.is_empty() {
                    errors.push(ValidationError::Phase2CollectError(format!(
                        "NoCostModel: executed Plutus language(s) {missing:?} have no cost model \
                         in the protocol parameters"
                    )));
                }
            }

            // ── Parallel-phase-2 gate ─────────────────────────────────────
            //
            // When the block-apply path uses deferred-parallel Phase-2
            // (feature = "parallel-verification"), it sets the thread-local
            // `SKIP_PHASE2_EVAL` flag before calling validation so that the
            // expensive `eval_phase_two_raw` is suppressed here. The actual
            // evaluation runs in parallel via
            // `crate::plutus::run_phase2_parallel` after the sequential loop.
            //
            // For all other callers (mempool admission, `validate_transaction`,
            // `validate_transaction_with_pools`, tests), the flag is `false`
            // (its default) and this branch executes normally.
            let skip_phase2 = SKIP_PHASE2_EVAL.with(|c| c.get());
            if !skip_phase2 {
                if slot_config.is_none() {
                    debug!(
                        tx_hash = %tx.hash.to_hex(),
                        "Plutus transaction missing slot configuration for script evaluation"
                    );
                    errors.push(ValidationError::MissingSlotConfig);
                }
                if errors.is_empty() {
                    if let Some(sc) = slot_config {
                        let cost_models_cbor = params.cost_models.to_cbor();
                        // uplc::tx::eval_phase_two_raw expects initial_budget as (cpu_steps, mem_units).
                        // Our ExUnits struct uses { mem, steps } where mem=memory_units and steps=cpu_steps.
                        // Swap the fields to match the uplc convention: (steps, mem) = (cpu, mem).
                        let max_ex = (params.max_tx_ex_units.steps, params.max_tx_ex_units.mem);
                        let eval_result = evaluate_plutus_scripts(
                            tx,
                            utxo_set,
                            cost_models_cbor.as_deref(),
                            max_ex,
                            sc,
                            params.protocol_version_major as u32,
                        );
                        // Decide the admission outcome from the (is_valid,
                        // eval_result) matrix per the Haskell UTXOS rule —
                        // collection errors reject for BOTH polarities,
                        // only a genuine script failure legitimises
                        // is_valid=false (#733/#734).  See
                        // `phase2_admission_error` for the full matrix.
                        if let Some(e) = phase2_admission_error(tx.is_valid, &eval_result) {
                            errors.push(e);
                        }
                    }
                }
            }
        }
    }

    if errors.is_empty() {
        debug!(tx_hash = %tx.hash.to_hex(), "Validation: transaction valid");
        Ok(())
    } else {
        warn!(
            tx_hash = %tx.hash.to_hex(),
            error_count = errors.len(),
            errors = ?errors,
            "Validation: transaction rejected"
        );
        Err(errors)
    }
}
