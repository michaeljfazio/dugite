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
pub mod withdrawals;

#[cfg(test)]
mod tests;

pub use scripts::evaluate_native_script;
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

use std::collections::{HashMap, HashSet};

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::network::NetworkId;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{GovAction, GovActionId, Transaction, Voter};
use dugite_primitives::value::Lovelace;
use tracing::{debug, trace, warn};

use crate::plutus::{evaluate_plutus_scripts, SlotConfig};
use crate::utxo::UtxoLookup;

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

#[derive(Default)]
pub struct ValidationContext {
    pub registered_pools: Option<HashSet<Hash28>>,
    pub current_treasury: Option<u64>,
    pub reward_accounts: Option<HashMap<Hash32, Lovelace>>,
    pub current_epoch: Option<u64>,
    /// Set of registered DRep credentials, keyed by
    /// [`Credential::to_typed_hash32`].  Byte 28 of the key encodes the
    /// credential kind (`0x00` for `VerificationKey`, `0x01` for `Script`),
    /// mirroring Haskell's `Map (Credential 'DRepRole) DRepState` — a key
    /// credential and a script credential with the same 28-byte hash are
    /// treated as distinct DReps, matching the Haskell `KeyHashObj` /
    /// `ScriptHashObj` discrimination.
    pub registered_dreps: Option<HashSet<Hash32>>,
    pub registered_vrf_keys: Option<HashMap<Hash32, Hash28>>,
    pub node_network: Option<NetworkId>,
    pub committee_members: Option<HashSet<Hash32>>,
    pub committee_resigned: Option<HashSet<Hash32>>,
    pub stake_key_deposits: Option<HashMap<Hash32, u64>>,
    /// The constitution's guardrail script hash, if any.
    ///
    /// When `Some`, governance proposals of type `ParameterChange` or
    /// `TreasuryWithdrawals` must carry a matching `policy_hash`.  When `None`,
    /// the constitution policy-hash check is skipped.
    pub constitution_script_hash: Option<Hash28>,
    /// DRep vote delegations — keys are stake credential hashes of accounts
    /// that have delegated to any DRep (including AlwaysAbstain / AlwaysNoConfidence).
    pub vote_delegations: Option<HashSet<Hash32>>,
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
    pub active_proposals: Option<HashMap<GovActionId, ActiveProposal>>,
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
    pub committee_authorized_hot_keys: Option<HashSet<Hash32>>,
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
    pub accumulated_mir_balances: Option<HashMap<Hash32, i64>>,
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
    pub genesis_delegates: Option<HashSet<Hash28>>,
}

impl ValidationContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_pools(mut self, pools: HashSet<Hash28>) -> Self {
        self.registered_pools = Some(pools);
        self
    }

    pub fn with_treasury(mut self, treasury: u64) -> Self {
        self.current_treasury = Some(treasury);
        self
    }

    pub fn with_reward_accounts(mut self, accounts: HashMap<Hash32, Lovelace>) -> Self {
        self.reward_accounts = Some(accounts);
        self
    }

    pub fn with_epoch(mut self, epoch: u64) -> Self {
        self.current_epoch = Some(epoch);
        self
    }

    pub fn with_dreps(mut self, dreps: HashSet<Hash32>) -> Self {
        self.registered_dreps = Some(dreps);
        self
    }

    pub fn with_vrf_keys(mut self, keys: HashMap<Hash32, Hash28>) -> Self {
        self.registered_vrf_keys = Some(keys);
        self
    }

    pub fn with_network(mut self, network: NetworkId) -> Self {
        self.node_network = Some(network);
        self
    }

    pub fn with_committee_members(mut self, members: HashSet<Hash32>) -> Self {
        self.committee_members = Some(members);
        self
    }

    pub fn with_committee_resigned(mut self, resigned: HashSet<Hash32>) -> Self {
        self.committee_resigned = Some(resigned);
        self
    }

    pub fn with_stake_key_deposits(mut self, deposits: HashMap<Hash32, u64>) -> Self {
        self.stake_key_deposits = Some(deposits);
        self
    }

    pub fn with_constitution_script_hash(mut self, hash: Hash28) -> Self {
        self.constitution_script_hash = Some(hash);
        self
    }

    pub fn with_vote_delegations(mut self, delegations: HashSet<Hash32>) -> Self {
        self.vote_delegations = Some(delegations);
        self
    }

    pub fn with_active_proposals(
        mut self,
        proposals: HashMap<GovActionId, ActiveProposal>,
    ) -> Self {
        self.active_proposals = Some(proposals);
        self
    }

    pub fn with_committee_authorized_hot_keys(mut self, hot_keys: HashSet<Hash32>) -> Self {
        self.committee_authorized_hot_keys = Some(hot_keys);
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
            self.accumulated_mir_balances = Some(HashMap::new());
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
        self.accumulated_mir_balances = Some(snapshot);
        self
    }

    /// Set the set of registered genesis-delegate key hashes used by
    /// the Shelley PPUP `NonGenesisUpdatePPUP` predicate.
    pub fn with_genesis_delegates(mut self, keys: HashSet<Hash28>) -> Self {
        self.genesis_delegates = Some(keys);
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
        self.registered_pools = Some(pools);
        self.current_treasury = Some(treasury);
        self.reward_accounts = Some(accounts);
        self.current_epoch = Some(epoch);
        self.registered_dreps = Some(dreps);
        self.registered_vrf_keys = Some(vrf_keys);
        self.node_network = Some(network);
        self.committee_members = Some(committee_members);
        self.committee_resigned = Some(committee_resigned);
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
    /// Haskell `wdrlNotZero`: withdrawals with a zero amount are rejected.
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
    },
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
    AuxiliaryDataHashMismatch,
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
        context.registered_pools.as_ref(),
        context.current_treasury,
        context.reward_accounts.as_ref(),
        context.current_epoch,
        context.registered_dreps.as_ref(),
        context.registered_vrf_keys.as_ref(),
        context.node_network,
        context.committee_members.as_ref(),
        context.committee_resigned.as_ref(),
        context.stake_key_deposits.as_ref(),
        context.constitution_script_hash,
        context.vote_delegations.as_ref(),
    );

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

        // Aggregate every disallowed (voter, action_id) pair into one error,
        // mirroring Haskell's NonEmpty predicate-failure shape.
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
                    // not DisallowedVoters.  Skip silently here.
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
        // VotingOnExpiredGovAction: a vote against an action whose
        // `expires_after_epoch` is strictly less than `current_epoch` is
        // rejected.  Boundary case (`current_epoch == expires_after_epoch`)
        // is allowed — Haskell `checkVotesAreNotForExpiredActions`.
        //
        // Precedence (Haskell `internVoter` partitions unknown voters out
        // first; the authority and expiry checks then apply only to known
        // voters):
        //
        //   VotersDoNotExist  >  DisallowedVoters
        //                     >  VotingOnExpiredGovAction
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
    reward_accounts: Option<&HashMap<Hash32, Lovelace>>,
    current_epoch: Option<u64>,
    registered_dreps: Option<&HashSet<Hash32>>,
    registered_vrf_keys: Option<&HashMap<Hash32, Hash28>>,
    node_network: Option<dugite_primitives::network::NetworkId>,
    committee_members: Option<&HashSet<Hash32>>,
    committee_resigned: Option<&HashSet<Hash32>>,
    stake_key_deposits: Option<&HashMap<Hash32, u64>>,
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
    // Reference: Haskell `StakeKeyRegisteredDELEG` in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg`.
    // ------------------------------------------------------------------
    if let Some(accounts) = reward_accounts {
        for cert in &tx.body.certificates {
            let opt_cred: Option<&dugite_primitives::credentials::Credential> = match cert {
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
            if let Some(credential) = opt_cred {
                // Mirror `state::credential_to_hash` — `to_typed_hash32` so the
                // lookup key matches the kind-tagged storage form.
                let key = credential.to_typed_hash32();
                if accounts.contains_key(&key) {
                    errors.push(ValidationError::StakeKeyAlreadyRegistered {
                        credential_hash: key.to_hex(),
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Delegation to unregistered pool (Haskell `DelegateeStakePoolNotRegisteredDELEG`)
    //
    // A delegation certificate that targets a pool ID not currently registered
    // in `pool_params` is rejected. This covers all variants that carry a
    // target pool hash: `StakeDelegation` (tag 2), `RegStakeDeleg` (tag 11),
    // `StakeVoteDelegation` (tag 13), `RegStakeVoteDeleg` (tag 14).
    //
    // `VoteRegDeleg` (tag 15) does NOT include a pool delegation component —
    // it registers and sets a DRep vote delegation only — so it is excluded.
    //
    // This check is only enforced when `registered_pools` is provided.
    //
    // Reference: Haskell `DelegateeStakePoolNotRegisteredDELEG` in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Deleg`.
    // ------------------------------------------------------------------
    if let Some(pools) = registered_pools {
        for cert in &tx.body.certificates {
            let opt_pool: Option<Hash28> = match cert {
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
                _ => None,
            };
            if let Some(pool_id) = opt_pool {
                if !pools.contains(&pool_id) {
                    errors.push(ValidationError::DelegateePoolNotRegistered {
                        pool_id: pool_id.to_hex(),
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
    // From Conway (protocol >= 9), a pool registration certificate whose VRF
    // key hash is already registered to a DIFFERENT pool is rejected. A pool
    // re-registering its own parameters with the same VRF key is permitted (the
    // key already belongs to that pool, so the new registration is not a
    // collision).
    //
    // This check is only enforced when `registered_vrf_keys` is provided (block
    // validation mode). The map is keyed by VRF key hash (Hash32) and maps to
    // the pool ID (Hash28) that currently holds that key.
    //
    // Reference: Haskell `VRFKeyHashAlreadyRegistered` in
    // `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Pool`.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
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
    // Withdrawal validation (Haskell `wdrlNotZero` + balance check)
    //
    // - Zero-amount withdrawals are rejected in Shelley–Babbage (proto < 9).
    //   In Conway (proto >= 9), zero-amount withdrawals are valid — they
    //   allow "touching" a reward account for DRep activity tracking.
    // - When `reward_accounts` is provided (block application mode),
    //   each withdrawal amount must exactly match the on-chain reward
    //   balance, and the account must be registered.
    // ------------------------------------------------------------------
    let conway_or_later = params.protocol_version_major >= 9;
    for (reward_account_bytes, amount) in &tx.body.withdrawals {
        // Format the reward account as a hex string for error messages.
        let account_hex = reward_account_bytes.iter().fold(
            String::with_capacity(reward_account_bytes.len() * 2),
            |mut s, b| {
                use std::fmt::Write;
                let _ = write!(s, "{b:02x}");
                s
            },
        );
        // Conway relaxed the wdrlNotZero predicate — zero withdrawals are
        // now valid (used for DRep activity / reward account touching).
        if amount.0 == 0 && !conway_or_later {
            errors.push(ValidationError::ZeroWithdrawal {
                account: account_hex,
            });
        }
    }

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

    // ------------------------------------------------------------------
    // Rules 11, 11b, 11c, 12 — Plutus-transaction-specific checks
    //
    // These are only enforced when the transaction includes Plutus scripts
    // or redeemers. They are split into their own modules to keep the rule
    // logic focused and independently testable.
    // ------------------------------------------------------------------
    if scripts::has_plutus_scripts(tx) {
        // Rule 11: collateral inputs, percentage, net-ADA check, total_collateral
        // Rule 11b: redeemer index bounds
        collateral::check_collateral(tx, utxo_set, params, &mut errors);

        // Rule 11c: every script-locked input/withdrawal and every Plutus minting
        // policy must have a matching redeemer (Spend / Reward / Mint respectively).
        // Matches Haskell's `scriptsNeeded` check.
        collateral::check_script_redeemers(tx, utxo_set, &mut errors);

        // Alonzo UTXOW: every redeemer in the witness set must map to a valid
        // script purpose. Redeemers with no matching purpose are rejected.
        // Matches Haskell's `hasExactSetOfRedeemers` / `ExtraRedeemers`.
        collateral::check_extra_redeemers(tx, utxo_set, &mut errors);

        // Rule 12: script data hash (mkScriptIntegrity) — covers redeemers,
        // datums, cost models, and language versions.
        scripts::check_script_data_hash(tx, utxo_set, params, &mut errors);

        // Babbage/Conway UTXOW: scripts in the witness set that are not
        // needed by any script purpose are rejected as extraneous.
        // Matches Haskell's `ExtraneousScriptWitnessesUTXOW` /
        // `babbageMissingScripts` check.
        scripts::check_extraneous_script_witnesses(tx, utxo_set, &mut errors);

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

        // Phase-2 V3 TxInfo translation check (Haskell PR #5011):
        //
        // At PV >= 11, the phase-1 `BabbageNonDisjointRefInputs` check is
        // relaxed (see phase1.rs Rule 9).  An equivalent check is moved
        // into PlutusV3 `TxInfo` construction: if any redeemer executes a
        // V3 script AND `inputs ∩ reference_inputs` is non-empty, the
        // translation fails with
        // `ConwayContextError::ReferenceInputsNotDisjointFromInputs`.
        //
        // V1/V2/native scripts (and txs with NO V3 redeemer) are accepted
        // with overlap — this is the intended relaxation.
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
            let any_v3_executed = redeemer_versions.values().any(|&v| v == 3);
            if any_v3_executed {
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
                    );
                    if tx.is_valid {
                        // Tx claims scripts pass — reject if they actually fail.
                        if let Err(e) = eval_result {
                            errors.push(ValidationError::ScriptFailed(e.to_string()));
                        }
                    } else {
                        // Tx claims scripts fail (is_valid=false) — reject if they
                        // actually pass.  This is the "DoS-class" attack vector:
                        // an attacker marks is_valid=false on a tx whose Plutus
                        // script evaluates to True.  Cardano mempool (`applyTx`)
                        // catches this with `ValidationTagMismatch`; we mirror
                        // that check here so BPs never admit such txs.
                        // Haskell rule: `Cardano.Ledger.Conway.Rules.Utxos`
                        // predicate `ConwayUtxos`:
                        //   IsValid True vs eval False → `TagMismatch False`
                        //   IsValid False vs eval True → `TagMismatch True`
                        if eval_result.is_ok() {
                            errors.push(ValidationError::IsValidTagMismatch {
                                declared: false,
                                evaluated: true,
                            });
                        }
                        // If eval also fails (both agree scripts fail), the
                        // is_valid=false path is legitimate — collateral is
                        // consumed at block apply time. No error here.
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
