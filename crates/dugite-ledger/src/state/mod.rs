mod apply;
pub(crate) mod certificates;
mod epoch;
#[cfg(feature = "epoch-state-debug")]
pub mod epoch_state_debug;
pub(crate) mod governance;
pub mod non_myopic;
mod protocol_params;
#[cfg(feature = "reward-debug-dump")]
pub mod reward_debug;
pub mod reward_pulser;
mod rewards;
/// Phase 0 measurement of the boundary reward fold (see the module docs).
#[cfg(test)]
mod rupd_work_measurement;
mod snapshot;
pub mod snapshot_format;
pub mod substates;
/// Shared, fully-populated fixtures for the snapshot-layout guards (#967).
pub mod test_fixtures;

// Re-export the deferred Phase-2 fatality applier for the bulk-sync pooling
// path (apply_bench / node). Gated identically to its definition.
#[cfg(feature = "parallel-verification")]
pub use apply::apply_phase2_outcomes;

// Re-export governance free functions and types for use by tests
#[cfg(test)]
pub(crate) use governance::{
    check_cc_approval, check_threshold, gov_action_priority, is_delaying_action,
    modified_pp_groups, pp_change_drep_all_groups_met, pp_change_drep_threshold,
    pp_change_spo_threshold, prev_action_as_expected, DRepPPGroup, StakePoolPPGroup,
};
pub use non_myopic::{leader_probability, Likelihood, NonMyopic, DECAY_FACTOR, SAMPLE_SIZE};
pub use rewards::{compute_reward_update, forced_reward_update, ForcedRewardUpdate};
// Re-export for the RUPD-apply sites in `eras::shelley` / `eras::conway`,
// which are not descendants of `state` and cannot otherwise reach the
// private `rewards` submodule. See issue #796.
pub(crate) use rewards::apply_reserves_delta;
#[doc(hidden)]
pub use rewards::Rat;
pub use snapshot::{
    check_snapshot_backend_match, infer_backend_from_snapshot, BackendCheckResult, SnapshotBackend,
    SnapshotMeta,
};
pub use snapshot_format::LedgerStateSnapshot;
pub use substates::{CertSubState, ConsensusSubState, EpochSubState, GovSubState, UtxoSubState};

use crate::plutus::SlotConfig;
use crate::utxo::UtxoSet;
use crate::utxo_diff::DiffSeq;
#[cfg(test)]
use dugite_primitives::block::Block;
use dugite_primitives::block::{Point, Tip};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{BlockNo, EpochNo, SlotNo};
use dugite_primitives::transaction::{
    Anchor, Constitution, DRep, GovActionId, ProposalProcedure, Rational, Relay, Voter,
    VotingProcedure,
};
use dugite_primitives::value::Lovelace;
use imbl::HashMap as ImblHashMap;
use imbl::HashSet as ImblHashSet;
use imbl::OrdMap as ImblOrdMap;
use imbl::OrdSet as ImblOrdSet;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tracing::{debug, info, trace};

/// Total ADA supply (45 billion ADA = 45 * 10^15 lovelace)
pub const MAX_LOVELACE_SUPPLY: u64 = 45_000_000_000_000_000;

/// Maximum allowed snapshot file size (10 GiB).
/// Prevents OOM from loading maliciously crafted or corrupted snapshot files.
pub const MAX_SNAPSHOT_SIZE: usize = 10 * 1024 * 1024 * 1024;

/// Controls whether `apply_block()` re-evaluates Plutus scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockValidationMode {
    /// Full Phase-1 + Phase-2 Plutus evaluation for new network blocks.
    /// Rejects the block if the `is_valid` flag doesn't match the actual
    /// script evaluation result (`ValidationTagMismatch`).
    ValidateAll,
    /// Trust the block producer's `is_valid` flag without re-evaluating scripts.
    /// Used for ImmutableDB replay, Mithril import, rollback replay, and self-forged blocks.
    ApplyOnly,
}

fn default_update_quorum() -> u64 {
    5 // Mainnet default: 5 out of 7 genesis delegates
}

/// The complete ledger state, decomposed into component sub-states for granular borrowing.
///
/// Serialization goes through `LedgerStateSnapshot` (see `snapshot_format.rs`).
/// Do NOT derive Serialize/Deserialize on this struct directly.
///
/// Large collections (`delegations`, `pool_params`, `reward_accounts`,
/// `governance`, `epoch_blocks_by_pool`) are wrapped in `Arc` for
/// copy-on-write semantics.  Cloning a `LedgerState` is therefore cheap:
/// it only bumps reference counts instead of deep-copying megabytes of
/// data.  Mutations go through `Arc::make_mut()`, which clones the inner
/// collection only when there are other outstanding references.
/// Memoized large validation registries — bulk-sync apply-ceiling fix.
///
/// Phase-1/Phase-2 validation needs read-only derived views of
/// `certs.pool_params` and `gov.governance`.  Rebuilding all of them from
/// scratch on EVERY block (one `.keys().collect()` / `.values().collect()` per
/// registry) measured ~51% of `apply_block` wall time on preview Conway blocks
/// (`apply_bench --restore-lsm` + `DUGITE_BLOCK_APPLY_TIMING=1`).
///
/// This memoizes the THREE expensive registries — the registered pool-id set,
/// the VRF-key→pool map (both derived from `pool_params`), the DRep set, and the
/// vote-delegation set — each keyed on the structural identity of its OWN source
/// map, not on the whole `governance` Arc.  `pools`/`vrf_keys` use
/// `Arc::ptr_eq` on `certs.pool_params`; `dreps`/`vote_delegations` use
/// `imbl::HashMap::ptr_eq` on `gov.governance.{dreps,vote_delegations}`.  Keying
/// per-source means a block that merely casts a vote or advances a proposal
/// (which bumps the `governance` Arc and the `proposals` map but leaves `dreps`
/// and `vote_delegations` structurally identical) still HITS both big caches.
/// The remaining registries (committee sets, active proposals, constitution)
/// are small and rebuilt fresh every block.
///
/// Soundness: identical source pointer ⟹ identical contents ⟹ byte-exact reuse.
/// `Arc` and `imbl` collections copy-on-write on mutation, so any change
/// allocates a fresh root and the next block detects the miss.  Purely
/// transient — never serialized (snapshots use the separate
/// `LedgerStateSnapshot`) and never part of the ledger fingerprint.
#[derive(Debug, Clone)]
pub(crate) struct CachedValidationRegistry {
    /// `certs.pool_params` Arc the pool registries were derived from (ptr-eq key).
    pub pool_params_src: std::sync::Arc<std::collections::HashMap<Hash28, PoolRegistration>>,
    pub pools: std::sync::Arc<std::collections::HashSet<Hash28>>,
    /// `certs.vrf_key_hashes` the registry was derived from — a SECOND ptr-eq
    /// key, and it is not optional (#1085).
    ///
    /// The VRF registry depends on two sources, and they move independently: a
    /// re-registration updates `vrf_key_hashes` and `future_pool_params`
    /// while leaving `pool_params` untouched, so keying the cache on
    /// `pool_params` alone would serve a registry that is stale in exactly the
    /// direction the predicate cares about — the pending key would look
    /// unclaimed and a second pool could take it.
    pub vrf_key_hashes_src: ImblHashMap<Hash32, u64>,
    pub vrf_keys: std::sync::Arc<crate::validation::VrfKeyRegistry>,
    /// `gov.governance.dreps` imbl map the DRep set was derived from (ptr-eq key).
    pub dreps_src: ImblHashMap<Hash32, DRepRegistration>,
    pub dreps: std::sync::Arc<std::collections::HashSet<Hash32>>,
    /// `gov.governance.vote_delegations` imbl map the set was derived from (ptr-eq key).
    pub vote_delegations_src: ImblHashMap<Hash32, DRep>,
    pub vote_delegations: std::sync::Arc<std::collections::HashSet<Hash32>>,
}

#[derive(Debug, Clone)]
pub struct LedgerState {
    // ── Component sub-states (independently borrowable) ──────────────
    /// UTxO state: the unspent transaction output set, per-epoch fees, and UTxO diffs.
    pub utxo: UtxoSubState,
    /// Delegation and pool state: credentials, pool registrations, reward accounts.
    pub certs: CertSubState,
    /// Conway governance state: proposals, votes, DReps, constitutional committee.
    pub gov: GovSubState,
    /// Consensus-layer state: nonces, block production counters, opcert tracking.
    pub consensus: ConsensusSubState,
    /// Epoch-level state: snapshots, treasury/reserves, protocol parameters.
    pub epochs: EpochSubState,

    // ── Coordination (immutable config or cross-cutting bookkeeping) ──
    /// Current tip of the chain
    pub tip: Tip,
    /// Current era
    pub era: Era,
    /// Pending era transition detected from the block stream.
    /// Set when `block.era > self.era` during `apply_block`.
    /// Consumed by the node layer to update the consensus-level `EraHistory`.
    /// `(previous_era, new_era, transition_epoch)`.
    pub pending_era_transition: Option<(Era, Era, EpochNo)>,
    /// Current epoch
    pub epoch: EpochNo,
    /// Shelley epoch length in slots
    pub epoch_length: u64,
    /// Number of Byron epochs before the Shelley hard fork.
    /// Total Byron slots = byron_epoch_length * shelley_transition_epoch.
    pub shelley_transition_epoch: u64,
    /// Byron epoch length in slots (10 * k). 0 = mainnet default (21600).
    pub byron_epoch_length: u64,
    /// Slot configuration for Plutus time conversion
    pub slot_config: SlotConfig,
    /// Shelley genesis hash (used for initial nonce state)
    pub genesis_hash: Hash32,
    /// Genesis delegates: genesis_key_hash (28 bytes) -> (delegate_key_hash (28 bytes), vrf_key_hash (32 bytes)).
    ///
    /// Loaded from the Shelley genesis file and mutated by `Certificate::GenesisKeyDelegation`
    /// (Shelley-era only; Conway removed the cert type). Used for BFT overlay
    /// schedule validation during early Shelley era (when d > 0).
    pub genesis_delegates: HashMap<Hash28, (Hash28, Hash32)>,
    /// Pending (not-yet-matured) genesis-delegate changes: `(maturity_slot,
    /// genesis_key_hash)` -> `(delegate_key_hash, vrf_key_hash)`.
    ///
    /// Haskell's `dsFutureGenDelegs`: a `Certificate::GenesisKeyDelegation`
    /// does NOT update `genesis_delegates` immediately — it enqueues here
    /// with `maturity_slot = cert_slot + stability_window_3kf`
    /// (`stabilityWindow = ceil(3k/f)`, NOT doubled — see
    /// `eras::common::enqueue_genesis_key_delegations`). Entries are moved
    /// into `genesis_delegates` by `eras::common::adopt_matured_genesis_delegs`
    /// once `maturity_slot <= current_slot`, called every block (Haskell's
    /// `adoptGenesisDelegs` runs at TICK, not just epoch boundaries).
    /// See issue #804.
    pub future_gen_delegs: HashMap<(u64, Hash28), (Hash28, Hash32)>,
    /// Quorum for pre-Conway protocol parameter updates (from Shelley genesis)
    pub update_quorum: u64,
    /// The network this node is running on (mainnet, testnet, etc.).
    ///
    /// Used for unconditional output/withdrawal address network checks during
    /// Phase-1 validation (Haskell's `Globals.networkId`).  Not persisted in
    /// snapshots — set from genesis/config at node startup.
    pub node_network: Option<dugite_primitives::network::NetworkId>,
    /// Randomness stabilisation window: ceiling(4k/f) for Conway+.
    pub randomness_stabilisation_window: u64,
    /// Stability window: ceiling(3k/f) for Alonzo/Babbage (per Haskell erratum 17.3).
    pub stability_window_3kf: u64,
    /// Security parameter k — maximum rollback depth.
    /// Not persisted in snapshots; set from genesis config at startup.
    pub security_param: u64,
    /// Conway genesis initialization data (needed by era-transition rules).
    /// Populated from conway-genesis.json at node startup; not persisted in snapshots.
    pub conway_genesis_init: Option<crate::eras::ConwayGenesisInit>,
    /// Maximum total lovelace supply for the network (Haskell `Globals.maxLovelaceSupply`,
    /// from `ShelleyGenesis.sgMaxLovelaceSupply`). Mainnet/preview/preprod all use
    /// 45_000_000_000_000_000; custom devnets may differ. Used to (a) initialize
    /// reserves at genesis and (b) derive `total_stake = max_lovelace_supply − reserves`
    /// in the reward calculation.
    pub max_lovelace_supply: u64,
    /// One-shot phase-2 time-translation horizon for the NEXT `apply_block`
    /// call (#733 corrections 5/6): the conservative apply-time horizon
    /// (`EraHistory::phase2_apply_horizon_slot`) computed by the async
    /// caller at the PRE-block ledger tip, set under the same write lock as
    /// the apply (deterministic — never resolved via `try_read` inside
    /// apply). Consumed (`take()`) by `apply_block`. `None` ⇒ the
    /// apply-time horizon check is skipped (warn-only) — never a false
    /// fatality. Not persisted in snapshots (per-block transient).
    pub phase2_apply_horizon: Option<u64>,

    /// Memoized per-block validation registries — see
    /// [`CachedValidationRegistry`].  Reused across blocks while
    /// `certs.pool_params` and `gov.governance` stay pointer-identical, which
    /// eliminates the ~51%-of-apply registry rebuild on the ~95% of blocks that
    /// carry no pool/DRep/committee/governance certificate.  Transient: `None`
    /// on a freshly (re)constructed state forces a one-off rebuild; never
    /// serialized and never part of the fingerprint.
    pub(crate) cached_validation_registry: Option<CachedValidationRegistry>,
}

/// Pending reward update matching Haskell's RUPD structure.
///
/// Computed at one epoch boundary and applied at the next. Contains:
/// - Per-account rewards to credit
/// - Treasury increase (tau cut + undistributed)
/// - Reserves decrease (monetary expansion)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PendingRewardUpdate {
    /// Rewards to add to each registered stake credential's reward account.
    pub rewards: HashMap<Hash32, Lovelace>,
    /// Total treasury increase (tau cut + undistributed rewards).
    pub delta_treasury: u64,
    /// Signed reserves adjustment (Haskell `RewardUpdate.deltaR`, a signed
    /// `DeltaCoin`/`Integer`). Positive means reserves DECREASE (the normal
    /// monetary-expansion case); negative means reserves INCREASE — this
    /// happens in a degraded/low-block epoch where `epoch_fees` exceeds
    /// `treasury_cut + total_distributed`, and Haskell's `applyRUpdFiltered`
    /// credits the difference back to reserves via `addDeltaCoin`. See
    /// issue #796. `i128` (not `u64`) so the sign can be represented; the
    /// magnitude never exceeds the max lovelace supply, well within range.
    pub delta_reserves: i128,
    /// Haskell `RewardUpdate.nonMyopic` — the `NonMyopic` record this boundary
    /// produces, already folded through `updateNonMyopic` (decay the previous
    /// epoch's history, combine with this epoch's likelihoods, re-key to this
    /// epoch's pool set, and stamp the frozen reward pot).
    ///
    /// ```haskell
    /// data RewardUpdate = RewardUpdate
    ///   { deltaT :: !DeltaCoin, deltaR :: !DeltaCoin
    ///   , rs :: !(Map (Credential 'Staking) (Set Reward))
    ///   , deltaF :: !DeltaCoin, nonMyopic :: !NonMyopic }
    /// ```
    ///
    /// Carried on the reward update rather than written directly to the epoch
    /// state because upstream does the same: `startStep` computes
    /// `newLikelihoods` and stashes the OLD `nonMyopic` in the `RewardSnapShot`,
    /// and `completeRupd` produces the merged record. Both halves travel
    /// together to the point of application.
    pub non_myopic: non_myopic::NonMyopic,
}

// ── Governance proposal priority forest types ─────────────────────────
//
// Per Haskell `Cardano.Ledger.Conway.Governance.Proposals`:
//   Proposals { pProps, pRoots :: GovRelation PRoot, pGraph :: GovRelation PGraph }
//
// Each governance purpose (PParam, HardFork, Committee, Constitution) maintains
// a tree of proposals rooted at the last enacted action.  This enables O(k)
// descendant removal for both expiry and sibling cleanup after enactment.

/// Parent-child edges for a proposal node within a governance purpose tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PEdges {
    /// Parent proposal ID (None if this proposal is a direct child of the root).
    pub parent: Option<GovActionId>,
    /// Direct children — proposals whose `prev_action_id` points to this one.
    pub children: ImblOrdSet<GovActionId>,
}

/// Root of a governance purpose tree — tracks the last enacted action and its
/// direct children (proposals whose `prev_action_id` matches the root).
///
/// Matches Haskell's `PRoot { prRoot, prChildren }`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PRoot {
    /// Last enacted GovActionId for this purpose (None = genesis / no enactment yet).
    pub root: Option<GovActionId>,
    /// Direct children of the root (proposals whose `prev_action_id == root`).
    pub children: ImblOrdSet<GovActionId>,
}

/// Per-purpose DAG of proposal parent-child relationships for non-root proposals.
///
/// Matches Haskell's `PGraph { unPGraph :: Map (GovPurposeId p) PEdges }`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PGraph {
    /// Map from proposal ID to its edges (parent + children).
    pub nodes: ImblHashMap<GovActionId, PEdges>,
}

/// One value per governance purpose (4 purposes).
///
/// Mirrors Haskell's `GovRelation f` which holds one `f` per `GovActionPurpose`:
///   0 = PParamUpdate, 1 = HardForkInitiation, 2 = Committee (shared by
///   NoConfidence + UpdateCommittee), 3 = Constitution.
///
/// `TreasuryWithdrawals` and `InfoAction` have no purpose tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GovRelation<T: Default> {
    /// ParameterChange proposals.
    pub pparam: T,
    /// HardForkInitiation proposals.
    pub hard_fork: T,
    /// Committee-purpose proposals (NoConfidence + UpdateCommittee share this).
    pub committee: T,
    /// NewConstitution proposals.
    pub constitution: T,
}

impl<T: Default> GovRelation<T> {
    /// Access the value for a governance purpose by tag.
    ///
    /// Tags match `gov_action_purpose_tag()`: 0=PParam, 1=HardFork, 2=Committee, 3=Constitution.
    ///
    /// # Panics
    /// Panics if `purpose > 3`.
    pub fn get(&self, purpose: u8) -> &T {
        match purpose {
            0 => &self.pparam,
            1 => &self.hard_fork,
            2 => &self.committee,
            3 => &self.constitution,
            _ => panic!("invalid governance purpose tag: {purpose}"),
        }
    }

    /// Mutable access to the value for a governance purpose by tag.
    ///
    /// # Panics
    /// Panics if `purpose > 3`.
    pub fn get_mut(&mut self, purpose: u8) -> &mut T {
        match purpose {
            0 => &mut self.pparam,
            1 => &mut self.hard_fork,
            2 => &mut self.committee,
            3 => &mut self.constitution,
            _ => panic!("invalid governance purpose tag: {purpose}"),
        }
    }
}

/// Conway-era governance state (CIP-1694)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GovernanceState {
    /// Registered DReps: credential -> DRepState.
    ///
    /// This map contains ALL currently-registered DReps — entries are added by
    /// `RegDRep` certificates and removed by `UnregDRep` certificates.  It does
    /// NOT shrink when a DRep becomes inactive due to `drep_activity` expiry;
    /// inactive DReps are merely flagged `active = false` at each epoch boundary
    /// (matching Haskell's `vsDReps` map semantics).
    ///
    /// Use [`GovernanceState::active_drep_count`] to obtain the count of DReps
    /// whose activity flag is still `true` (i.e. those that contribute voting
    /// power and that external tools like Koios report as "registered").
    pub dreps: ImblHashMap<Hash32, DRepRegistration>,
    /// Vote delegations: stake credential hash -> DRep
    pub vote_delegations: ImblHashMap<Hash32, DRep>,
    /// Constitutional committee: cold credential -> hot credential
    pub committee_hot_keys: ImblHashMap<Hash32, Hash32>,
    /// Committee member expiration epochs (cold credential -> expiration epoch)
    pub committee_expiration: ImblHashMap<Hash32, EpochNo>,
    /// Resigned committee members
    pub committee_resigned: ImblHashMap<Hash32, Option<Anchor>>,
    /// Script-type cold committee credentials (credential_type = 1 for N2C queries).
    /// Populated from CommitteeHotAuth and CommitteeColdResign certificates when the cold
    /// credential is a Credential::Script variant.  Used to correctly set cold_credential_type
    /// in GetCommitteeState responses without changing the Hash32-keyed committee maps.
    pub script_committee_credentials: ImblHashSet<Hash32>,
    /// Script-type hot committee credentials (hot_credential_type = 1 for N2C queries).
    /// Populated from CommitteeHotAuth certificates when the hot credential is a
    /// Credential::Script variant.  Maps cold_credential_hash -> hot_credential_hash for
    /// script hot keys, so a re-authorization with a key hot key correctly removes the entry.
    /// Used to correctly set hot_credential_type in GetCommitteeState responses.
    pub script_committee_hot_credentials: ImblHashSet<Hash32>,
    /// Active governance proposals indexed by GovActionId
    pub proposals: ImblOrdMap<GovActionId, ProposalState>,
    /// Votes cast, indexed by action ID for efficient ratification lookup.
    ///
    /// Inner map is keyed by `Voter` (O(log n) insert, inherently last-vote-wins),
    /// matching Haskell's `Map voter Vote`. See #782-class perf note: this used to
    /// be a `Vec<(Voter, VotingProcedure)>` with a linear `find` for last-wins,
    /// which collapsed to O(n^2) on actions with hundreds of thousands of votes.
    pub votes_by_action: ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
    /// Proposal forest roots: last enacted action per governance purpose + direct children.
    ///
    /// Per Haskell `pRoots :: GovRelation PRoot`.  Each of the 4 purposes tracks the
    /// last enacted `GovActionId` (the root) and proposals whose `prev_action_id`
    /// matches that root.  Used for O(1) sibling lookups during enactment.
    pub proposal_roots: GovRelation<PRoot>,
    /// Proposal forest graph: parent-child edges per governance purpose for non-root proposals.
    ///
    /// Per Haskell `pGraph :: GovRelation PGraph`.  Proposals deeper than one level
    /// (i.e. their `prev_action_id` points to another proposal rather than the enacted
    /// root) are tracked here.  Used for O(k) descendant collection during removal.
    pub proposal_graph: GovRelation<PGraph>,
    /// Total DRep registrations count (including deregistered)
    pub drep_registration_count: u64,
    /// Total proposals submitted
    pub proposal_count: u64,
    /// Current constitution (set by NewConstitution governance action)
    pub constitution: Option<Constitution>,
    /// Whether the committee is in a no-confidence state (dissolved by NoConfidence action)
    pub no_confidence: bool,
    /// Committee quorum threshold (from genesis or UpdateCommittee action)
    /// This is the fraction of active CC members that must vote Yes to approve.
    pub committee_threshold: Option<Rational>,
    /// Last enacted governance action IDs per purpose (for prev_action_id chain validation).
    /// Matches Haskell's `GovRelation StrictMaybe` / `ensPrevGovActionIds`.
    pub enacted_pparam_update: Option<GovActionId>,
    pub enacted_hard_fork: Option<GovActionId>,
    pub enacted_committee: Option<GovActionId>,
    pub enacted_constitution: Option<GovActionId>,
    /// Last ratification results (from most recent epoch transition).
    /// Used by GetRatifyState (N2C query tag 32).
    pub last_ratified: Vec<(GovActionId, ProposalState)>,
    pub last_expired: Vec<GovActionId>,
    pub last_ratify_delayed: bool,
    /// Number of "dormant epochs" accumulated since the start of the Conway era.
    ///
    /// Per Haskell `vsNumDormantEpochs` (Conway.Rules.Epoch, `updateNumDormantEpochs`):
    /// an epoch is "dormant" if there were no active governance proposals at the epoch
    /// boundary (i.e. `proposals` was empty during that epoch).  The dormant count is
    /// baked into `DRepRegistration::drep_expiry` at registration/vote time via
    /// `compute_drep_expiry()`, so it is NOT subtracted again at activity-check time.
    ///
    pub num_dormant_epochs: u64,
    /// DRep voting power snapshot captured at each epoch boundary (the "mark" snapshot).
    ///
    /// Maps DRep credential hash → total delegated stake (lovelace).  Only active DReps
    /// (those whose `active` flag is `true`) appear in this map.
    ///
    /// Per Haskell `reDRepDistr` in `Conway.Rules.Epoch`, DRep voting power used during
    /// ratification is measured against the snapshot taken at the *start* of the current
    /// epoch, not the live state.  This prevents mid-epoch stake movements from
    /// affecting in-flight governance ratification.
    ///
    /// Haskell `cgsDRepPulsingState` — the frozen DRep pulser (#988).
    ///
    /// Captured at epoch boundary E and consumed at boundary E+1, so proposals
    /// and votes submitted during epoch E are not considered for ratification
    /// until E+1→E+2. It carries BOTH halves of `DRComplete`: the frozen inputs
    /// RATIFY ran over, and the decision it reached.
    ///
    /// `None` before the first Conway boundary of a chain. Since #988 step 2
    /// that means nothing ratifies, matching Haskell's `Default` of
    /// `DRComplete def def` — never a fall back to live state, which is #903's
    /// bug with an extra step.
    ///
    /// Replaced five separately-`Option`al fields; see [`DRepPulsingState`].
    pub drep_pulsing_state: Option<DRepPulsingState>,
    /// Haskell `cgsFuturePParams` (#977). See [`FuturePParams`] — this feeds
    /// the ledger-view FORECAST, not only the `GetGovState` query.
    pub future_pparams: FuturePParams,
}

/// Haskell `FuturePParams` — the protocol parameters that will be in force at
/// the next epoch boundary, insofar as they are yet known (#977).
///
/// Not a convenience cache, and not only a query field. Its two readers
/// upstream are `nextEpochPParams` at the boundary and — the reason this
/// matters — `Conway.Rules.Tickf`, ouroboros-consensus's ledger-view FORECAST
/// path used to validate headers AHEAD of the ledger tip:
///
/// ```haskell
/// pure $! nes {nesPd = pd'}
///   & newEpochStateGovStateL . curPParamsGovStateL  .~ nextEpochPParams govState
///   & newEpochStateGovStateL . prevPParamsGovStateL .~ (govState ^. curPParamsGovStateL)
///   & newEpochStateGovStateL . futurePParamsGovStateL .~ NoPParamsUpdate
/// ```
///
/// Praos's `LedgerView` is `{lvPoolDistr, lvMaxHeaderSize, lvMaxBodySize,
/// lvProtocolVersion}` and the last three come from those `curPParams`, so a
/// node that does not model this validates next-epoch headers against THIS
/// epoch's size limits and protocol version — divergent the moment a
/// `ParameterChange` or `HardForkInitiation` enacts.
///
/// # Lifecycle — three writers
///
/// * `EPOCH` sets `PotentialPParamsUpdate Nothing` **unconditionally** at every
///   boundary, whatever enacted.
/// * `predictFuturePParams` runs on every non-boundary tick and upgrades
///   `Nothing` to `Just pp` when the pulser's `rsEnacted` contains a
///   `ParameterChange` or `HardForkInitiation`.
/// * `solidifyFuturePParams` runs on every block from
///   `firstSlotNextEpoch - 2 * stabilityWindow` onward, collapsing
///   `Potential Nothing -> No` and `Potential (Just pp) -> Definite pp`. That
///   is deliberately early so, per upstream's comment, "HFC has the new
///   EnactState available 6k/f slots before the end of the epoch".
///
/// # Wire format
///
/// Verified against real preview epoch-1259 bytes by `dugite-serialization`'s
/// `decode_future_pparams`:
///
/// ```text
/// NoPParamsUpdate            -> array(1) [0]
/// DefinitePParamsUpdate pp   -> array(2) [1, pp]
/// PotentialPParamsUpdate m   -> array(2) [2, <array(0) | array(1) [pp]>]
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum FuturePParams {
    /// Nothing will change at the next boundary.
    #[default]
    NoPParamsUpdate,
    /// A change is GUARANTEED to be adopted. Only reachable after `solidify`,
    /// i.e. within `2 * stabilityWindow` of the epoch end.
    DefinitePParamsUpdate(Box<ProtocolParameters>),
    /// A change may still be adopted; `None` means "nothing proposed so far".
    /// This is the state for the early part of every epoch.
    PotentialPParamsUpdate(Option<Box<ProtocolParameters>>),
}

impl FuturePParams {
    /// Haskell `solidifyFuturePParams` — collapse a potential update into a
    /// definite one, or into nothing.
    ///
    /// ```haskell
    /// solidifyFuturePParams = \case
    ///   PotentialPParamsUpdate Nothing   -> NoPParamsUpdate
    ///   PotentialPParamsUpdate (Just pp) -> DefinitePParamsUpdate pp
    ///   fpp                              -> fpp
    /// ```
    pub fn solidify(&mut self) {
        *self = match std::mem::take(self) {
            FuturePParams::PotentialPParamsUpdate(None) => FuturePParams::NoPParamsUpdate,
            FuturePParams::PotentialPParamsUpdate(Some(pp)) => {
                FuturePParams::DefinitePParamsUpdate(pp)
            }
            other => other,
        };
    }

    /// Haskell `knownFuturePParams` — parameters only when their adoption is
    /// already guaranteed.
    pub fn known(&self) -> Option<&ProtocolParameters> {
        match self {
            FuturePParams::DefinitePParamsUpdate(pp) => Some(pp),
            _ => None,
        }
    }

    /// Haskell `nextEpochPParams govState` — the parameters that will be in
    /// force after the next boundary, falling back to the current ones.
    ///
    /// This is the term the ledger-view FORECAST reads.
    pub fn next_epoch_pparams<'a>(
        &'a self,
        current: &'a ProtocolParameters,
    ) -> &'a ProtocolParameters {
        match self {
            FuturePParams::DefinitePParamsUpdate(pp) => pp,
            FuturePParams::PotentialPParamsUpdate(Some(pp)) => pp,
            _ => current,
        }
    }
}

/// The **completed DRep pulser result** — Haskell `DRComplete (PulsingSnapshot,
/// RatifyState)` (#988).
///
/// # Why this exists
///
/// Haskell's `DRepPulsingState` is created fresh at each epoch boundary by
/// `setFreshDRepPulsingState`, computes incrementally through the epoch, and is
/// consumed by RATIFY at the NEXT boundary. dugite had the frozen *inputs*
/// (`PulsingSnapshot`, #903) but never the frozen *result*, which cost two
/// separate divergences:
///
/// * `GetRatifyState` (LSQ tag 32) is `queryRatifyState = snd .
///   finishedPulserState` upstream — the CURRENT epoch's pulser result, i.e.
///   what **will** enact at the next boundary. dugite answered with what
///   **did** enact at the last one: one boundary stale, the same shape and
///   direction as #922 / #950 / #966.
/// * `predictFuturePParams` (#977) reads `rsEnacted` and
///   `ensCurPParams (rsEnactState …)` from the pulser mid-epoch, which is
///   simply unavailable without it.
///
/// # dugite does NOT need incremental pulsing
///
/// Both `DRepPulsingState` constructors encode as `DRComplete`, and the
/// `DRPulsing` arm **forces `finishDRepPulser` before encoding**:
///
/// ```haskell
/// encCBOR (DRComplete x y) = encode (Rec DRComplete !> To x !> To y)
/// encCBOR x@(DRPulsing (DRepPulser {})) = encode (Rec DRComplete !> To snap !> To ratstate)
///   where (snap, ratstate) = finishDRepPulser x
/// ```
///
/// Every reader — `queryProposals`, `queryDRepStakeDistr`, `queryRatifyState`,
/// `predictFuturePParams` — goes through the same forcing, and `Default` is
/// `DRComplete def def`. A partially-pulsed state is therefore never
/// observable: the pulsing is a performance device that spreads an
/// O(accounts) computation across the epoch, carrying no semantics. What must
/// be modelled is the completed result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PulsedRatifyState {
    /// The boundary this was computed at — it describes the ratification that
    /// will be APPLIED at the following boundary.
    pub computed_at_epoch: EpochNo,
    /// `rsEnacted` — the actions that will enact, in enactment order.
    pub enacted: Vec<GovActionId>,
    /// `rsExpired`.
    pub expired: Vec<GovActionId>,
    /// `rsDelayed` — whether a delaying action is among them.
    pub delayed: bool,
    /// `ensCurPParams (rsEnactState …)` — the protocol parameters that result
    /// once `enacted` has been applied. This is the term `predictFuturePParams`
    /// needs (#977), and the reason the whole ratification is run rather than
    /// only its accept/reject verdicts.
    pub cur_pparams: ProtocolParameters,
    /// Whether any enacted action is a `ParameterChange` or
    /// `HardForkInitiation` — Haskell `hasChangesToPParams`, the guard that
    /// decides whether `futurePParams` becomes `Just` or stays `Nothing`.
    pub has_pparams_changes: bool,
    /// The remaining `rsEnactState` terms, AFTER this plan's enactments.
    ///
    /// See [`EnactedGovTerms`] — these are the same self-inclusive projection
    /// [`Self::cur_pparams`] already is, for the governance fields rather than
    /// the protocol parameters.
    pub enact_state: EnactedGovTerms,
}

/// `rsEnactState`'s governance terms as they stand AFTER the plan's own
/// enactments have been applied.
///
/// # Why these cannot be read from live state
///
/// RATIFY threads ONE `EnactState` through the ENACT rule and returns it inside
/// the `RatifyState` it produces (`Conway/Rules/Ratify.hs`, `ratifyTransition`):
///
/// ```haskell
/// newEnactState <- trans @(EraRule "ENACT" era) $
///                    TRC ((), rsEnactState, EnactSignal gasId govAction)
/// let st' = st & rsEnactStateL .~ newEnactState
///              & rsEnactedL %~ (Seq.:|> gas)
/// trans @(ConwayRATIFY era) $ TRC (env, st', RatifySignal sigs)
/// ```
///
/// so the returned `rsEnactState` is the pulser's seed PLUS the cumulative
/// effect of every action in its own `rsEnacted`. `ensCommittee`,
/// `ensConstitution` and `ensPrevGovActionIds` are therefore ONE BOUNDARY AHEAD
/// of the governance state they were seeded from: an action that enacts at the
/// E→E+1 boundary is already visible here during epoch E.
///
/// Measured on preprod, which is the reason this is a stored field and not a
/// live read at the point of use. `cardano-streamer` prints this record at the
/// first block of each epoch; its `prevGovActionIds` carries the `PParamUpdate`
/// root from epoch **179** and the `HardFork` root from epoch **180**, while the
/// live governance state only gains them at 180 and 181 respectively — the
/// hard fork's protocol version does not reach `cgsCurPParams` until 181.
/// Reading live state here would be correct in every epoch that enacts nothing
/// and wrong in every epoch that enacts something, which is the failure shape
/// of #977 and #1071: the interesting value occupies a phase, so a reader that
/// samples the boring one agrees almost always.
///
/// # Why it is captured rather than recomputed
///
/// It is filled in by `compute_pulsed_ratify_state`, from the state its own dry
/// run produced — the clone that `ratify_proposals_impl` has already applied
/// every enactment to. Nothing is re-derived and there is no second copy of
/// `enactGovAction` to drift (#985's N-copies trap). Recomputing at the point of
/// use would additionally be wrong: the dump point is one block past the
/// boundary, and the DRep and SPO terms that decide ratification have moved by
/// then.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnactedGovTerms {
    /// `ensCommittee`'s membership — cold credential → expiry epoch.
    ///
    /// Keyed by `Credential::to_typed_hash32`, so the credential's KIND is
    /// recoverable from the key itself: bytes `[..28]` are the hash and byte
    /// `[28]` is `0x01` for a script and `0x00` for a key. Consumers that
    /// render the credential (`keyHash-…` / `scriptHash-…`) must read that byte
    /// rather than consult a second set — which is the defect the `drepDistr`
    /// key carried until it was corrected against real oracle output.
    pub committee_expiration: ImblHashMap<Hash32, EpochNo>,
    /// `ensCommittee`'s quorum threshold. `None` is `SNothing` — no committee.
    pub committee_threshold: Option<Rational>,
    /// `ensConstitution`.
    pub constitution: Option<Constitution>,
    /// `ensPrevGovActionIds` — the four governance-purpose roots.
    pub prev_gov_action_ids: GovRelation<Option<GovActionId>>,
}

/// Frozen ratification inputs captured at epoch boundary E.
///
/// Consumed by `ratify_proposals()` at boundary E+1 so that proposals/votes
/// submitted during epoch E are not considered until the following boundary.
/// Analogous to Haskell's `DRepPulsingState` snapshot fields (`dpProposals`,
/// `dpCommitteeState`, `dpEnactState`, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PulsingSnapshot {
    /// Proposals active at snapshot time.
    pub proposals: ImblOrdMap<GovActionId, ProposalState>,
    /// Votes indexed by action ID at snapshot time.
    pub votes_by_action: ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
    /// Committee hot key authorizations (cold → hot) at snapshot time.
    pub committee_hot_keys: ImblHashMap<Hash32, Hash32>,
    /// Committee member expiration epochs at snapshot time.
    pub committee_expiration: ImblHashMap<Hash32, EpochNo>,
    /// Resigned committee members at snapshot time.
    pub committee_resigned: ImblHashMap<Hash32, Option<Anchor>>,
    /// Committee quorum threshold at snapshot time.
    pub committee_threshold: Option<Rational>,
    /// Whether the committee was in a no-confidence state at snapshot time.
    pub no_confidence: bool,
    /// Enacted governance action roots at snapshot time (starting point for
    /// `prev_action_id` chain validation during ratification).
    pub enacted_pparam_update: Option<GovActionId>,
    pub enacted_hard_fork: Option<GovActionId>,
    pub enacted_committee: Option<GovActionId>,
    pub enacted_constitution: Option<GovActionId>,
    /// The epoch when this snapshot was captured.
    pub snapshot_epoch: EpochNo,
    /// Treasury pot at snapshot time — Haskell `ensTreasury` (#966).
    ///
    /// `withdrawalCanWithdraw` gates a `TreasuryWithdrawals` action on this
    /// value, and Haskell reads it from the frozen pulser, NOT from live
    /// state. `setFreshDRepPulsingState` seals it at the end of
    /// `epochTransition`:
    ///
    /// ```haskell
    /// dpEnactState = mkEnactState govState & ensTreasuryL .~ epochState ^. treasuryL
    /// ```
    ///
    /// The pulser sealed at boundary N->N+1 is consumed at boundary
    /// N+1->N+2, so RATIFY is structurally blind to the `applyRUpd` credit
    /// landing at the boundary it is running on — it sees the treasury as of
    /// ONE BOUNDARY EARLIER.
    ///
    /// Before this field existed, ratification seeded its cap basis from the
    /// live `epochs.treasury`, which at that point already included the
    /// current boundary's RUPD. A withdrawal that only became affordable at
    /// boundary B would therefore enact at B on dugite and at B+1 on
    /// cardano-node — a chain split, in the accept-early direction.
    pub treasury: u64,
    /// Vote delegations (credential → DRep) at snapshot time.
    ///
    /// Used by `default_spo_vote()` during ratification to determine the
    /// implicit vote for non-voting SPOs, matching Haskell's
    /// `dpDefaultDRepVoteDelegs` captured in the DRep pulser.
    pub vote_delegations: ImblHashMap<Hash32, DRep>,
    /// `psDRepDistr` — DRep voting power, credential → stake.
    ///
    /// Haskell's `finishDRepPulser` computes this with `computeDRepDistr` over
    /// the pulser's own frozen `dpInstantStake` / `dpDRepState` /
    /// `dpProposalDeposits`, and RATIFY consumes it as `reDRepDistr`.
    ///
    /// EVERY REGISTERED DRep with delegated stake appears — `computeDRepDistr`
    /// applies exactly four predicates (`Conway/Governance/DRepPulser.hs`), and
    /// for a `DRepCredential` the only one is `Map.member cred regDReps`. There
    /// is no expiry, no vote and no `drepDelegs` condition. This doc previously
    /// said "only active DReps appear", and the code matched the doc: expiry was
    /// applied HERE instead of at the ratio, so dugite shed entries over time.
    /// Measured on preprod — exact through epoch 186, then 150 of 182 entries
    /// missing by 306, with a dropped DRep reappearing later carrying a LARGER
    /// value, i.e. filtered out rather than destroyed.
    pub drep_distr: ImblHashMap<Hash32, u64>,
    /// Frozen `drepExpiry` per registered DRep — Haskell's `dpDRepState`.
    ///
    /// Expiry enters the pipeline at exactly ONE point, `dRepAcceptedRatio`
    /// (`Conway/Rules/Ratify.hs`), which tests `reCurrentEpoch` against the
    /// expiry held in this FROZEN state. Both halves matter: the state is as of
    /// the freeze, the epoch is the one RATIFY runs in — one boundary later — so
    /// a boolean decided at capture cannot express it. A DRep whose expiry
    /// equals the capture epoch is counted at capture and excluded at
    /// consumption.
    ///
    /// An ORDERED map, unlike its siblings here, and deliberately: the snapshot
    /// serialiser writes map fields in iteration order, and `imbl::HashMap`
    /// iterates in hash order, which varies between processes. Adding this field
    /// as a hash map made `snapshot_format_hash_stability` produce a DIFFERENT
    /// digest on two runs of identical code. Every other map field in the
    /// fixture happens to hold at most one entry, so that check has never had
    /// the chance to observe the nondeterminism it would otherwise catch — see
    /// the issue filed alongside this change.
    pub drep_expiry: ImblOrdMap<Hash32, EpochNo>,
    /// Total stake delegated to `AlwaysNoConfidence` at freeze time.
    pub drep_no_confidence: u64,
    /// Total stake delegated to `AlwaysAbstain` at freeze time.
    pub drep_abstain: u64,
    /// Whether ANY account delegates to `AlwaysNoConfidence` / `AlwaysAbstain`.
    ///
    /// Upstream `psDRepDistr` is a single `Map DRep (CompactForm Coin)` in
    /// which the two predefined DReps are ordinary keys, created by
    /// `Map.insertWith` only when an account actually delegates to one — so an
    /// undelegated predefined DRep is ABSENT, not present-with-zero. dugite
    /// splits the map into a credential-keyed part plus two scalars, and a
    /// scalar cannot distinguish "absent" from "zero"; these two flags carry
    /// that bit so `GetDRepStakeDistr` can reproduce the key set exactly.
    ///
    /// Without them dugite padded every reply with `drep-alwaysAbstain: 0` and
    /// `drep-alwaysNoConfidence: 0` where cardano-node returned `{}` (#994).
    /// Gating on `stake > 0` instead would be wrong in the other direction: a
    /// zero-balance account that delegates its vote creates the key upstream
    /// with a value of zero.
    pub drep_no_confidence_delegated: bool,
    pub drep_abstain_delegated: bool,
}

impl PulsingSnapshot {
    /// Is this DRep expired as of the epoch RATIFY is running in?
    ///
    /// Haskell `dRepAcceptedRatio` (`Conway/Rules/Ratify.hs`) skips a DRep when
    /// its frozen `drepExpiry` is behind `reCurrentEpoch`. The pulser frozen at
    /// boundary N is consumed at boundary N+1, so the comparison epoch is
    /// `snapshot_epoch + 1` — NOT the capture epoch. A DRep whose expiry equals
    /// the capture epoch is therefore counted at capture and excluded here,
    /// which is exactly the drift a boolean decided at capture cannot express.
    ///
    /// A credential absent from `drep_expiry` is treated as NOT expired: absence
    /// means the frozen registry had no entry, and a DRep with no registration
    /// never reaches the distribution in the first place.
    pub fn drep_is_expired(&self, cred: &Hash32) -> bool {
        match self.drep_expiry.get(cred) {
            Some(expiry) => self.snapshot_epoch.0.saturating_add(1) > expiry.0,
            None => false,
        }
    }
}

/// Haskell `DRepPulsingState`, always in its `DRComplete` form (#988).
///
/// ```haskell
/// data DRepPulsingState era
///   = DRPulsing !(DRepPulser era Identity (RatifyState era))
///   | DRComplete !(PulsingSnapshot era) !(RatifyState era)
/// ```
///
/// dugite has no incremental pulsing and does not need it: both constructors
/// encode as `DRComplete`, the `DRPulsing` arm forcing `finishDRepPulser`
/// first, and every reader goes through the same forcing. The pulsing is a
/// performance device that spreads an O(accounts) computation across the epoch
/// and carries no semantics.
///
/// # Why one struct rather than five fields
///
/// dugite reached this shape by accretion — a separate frozen field added each
/// time a divergence was traced to reading live state: the proposal/vote set
/// (#903), the DRep distribution (#949/#950), `ensTreasury` (#966), and finally
/// the ratification result itself (#988). Five independently-`Option`al fields
/// admit two failure modes that upstream's single sum type does not:
///
/// * a **torn** pulser — inputs frozen at one boundary, result at another, or
///   one captured and the other not, which is only prevented by every write
///   path remembering to do all of them; and
/// * a reader picking the live equivalent of one term while its neighbours read
///   the frozen one, which is exactly how #949's proposal-deposit term came to
///   be fixed in the query path and left broken in the consensus path.
///
/// Neither is expressible now: there is one `Option`, written in one place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DRepPulsingState {
    /// `PulsingSnapshot` — the frozen INPUTS.
    pub snapshot: PulsingSnapshot,
    /// `RatifyState` — the frozen RESULT, decided over `snapshot`.
    pub ratify_state: PulsedRatifyState,
}

/// Registration state for a DRep
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DRepRegistration {
    pub credential: Credential,
    pub deposit: Lovelace,
    pub anchor: Option<Anchor>,
    pub registered_epoch: EpochNo,
    /// Absolute expiry epoch for this DRep, matching Haskell's `drepExpiry`.
    ///
    /// Computed at registration/vote/update time as:
    ///   PV >= 10: `(current_epoch + drep_activity) - num_dormant_epochs`
    ///   PV <  10: `current_epoch + drep_activity` (bootstrap, dormant ignored)
    ///
    /// A DRep is expired (inactive) when `current_epoch > drep_expiry`.
    pub drep_expiry: EpochNo,
    /// Whether this DRep is currently active (per CIP-1694 activity tracking).
    /// Inactive DReps remain registered but are excluded from voting power calculations.
    pub active: bool,
    /// `drepDelegs` — the staking credentials delegating their VOTE to this
    /// DRep. Haskell's `DRepState` carries it as a `Set (Credential 'Staking)`
    /// and it is the 4th field of its CBOR encoding.
    ///
    /// # It is not derivable from the forward map, and that is the whole point
    ///
    /// `ConwayUnRegDRep` has TWO state mutations, not one (`GovCert.hs`):
    ///
    /// ```haskell
    /// certState' = certState & certVStateL . vsDRepsL %~ Map.delete cred
    /// clearDRepDelegations delegs accountsMap =
    ///   foldr (Map.adjust (dRepDelegationAccountStateL .~ Nothing)) accountsMap delegs
    /// ```
    ///
    /// so deregistering a DRep ORPHANS its delegators — and since
    /// `ConwayRegDRep` starts `drepDelegs = mempty`, a dereg/re-reg cycle
    /// leaves the re-registered DRep with none of them. dugite tracked only the
    /// forward map and so kept counting them forever (#1084).
    ///
    /// At protocol version 10 and above this set happens to equal
    /// `{a : vote_delegations[a] == this}`, so a `retain` over the forward map
    /// would give the same answer. **Below 10 it does not**, and that is why
    /// the set is stored rather than derived: `processDelegationInternal`
    /// preserves ledger #4772 at PV9, leaving delegators who have already moved
    /// to another DRep in this set, whereupon this DRep's dereg collaterally
    /// clears a delegation that no longer points at it. Reproducing the number
    /// with a `retain` would be right on today's networks and wrong on the
    /// replay of their history.
    #[serde(default)]
    pub delegs: ImblHashSet<Hash32>,
}

impl GovernanceState {
    /// The frozen pulser's INPUTS, if a pulser exists.
    ///
    /// Every reader of frozen ratification inputs goes through this, so a
    /// caller cannot read one term from the pulser and its neighbour from live
    /// state — the mistake behind #949.
    pub fn pulsing_snapshot(&self) -> Option<&PulsingSnapshot> {
        self.drep_pulsing_state.as_ref().map(|p| &p.snapshot)
    }

    /// The frozen pulser's RESULT — Haskell `queryRatifyState = snd .
    /// finishedPulserState`. This is what WILL enact at the next boundary, and
    /// since #988 step 2 it is also what the boundary applies.
    pub fn ratify_plan(&self) -> Option<&PulsedRatifyState> {
        self.drep_pulsing_state.as_ref().map(|p| &p.ratify_state)
    }

    /// `reDRepDistr` — DRep voting power for ratification.
    ///
    /// Empty with no pulser, matching Haskell's `DRComplete def def`. It
    /// deliberately does NOT fall back to live delegations: `dRepAcceptedRatio`
    /// folds `reDRepDistr`, never the voter set, so an empty distribution makes
    /// every DRep-gated action unratifiable and that is the correct answer, not
    /// a reason to substitute a newer one.
    pub fn drep_distr(&self) -> Option<&ImblHashMap<Hash32, u64>> {
        self.drep_pulsing_state
            .as_ref()
            .map(|p| &p.snapshot.drep_distr)
    }

    /// Count of DReps whose `active` flag is currently `true`.
    ///
    /// This is the number that external tools (Koios, cardano-cli) report as
    /// "registered" DReps: all DReps that have registered and whose activity
    /// window has not yet expired.  It excludes:
    ///
    /// * DReps that became inactive due to `drep_activity` epoch inactivity
    ///   (they remain in `self.dreps` with `active = false` until explicitly
    ///   deregistered via `UnregDRep`).
    ///
    /// Per CIP-1694, inactive DReps still hold their deposit and can
    /// reactivate by voting or submitting an `UpdateDRep` certificate; they
    /// are simply excluded from voting power calculations.
    pub fn active_drep_count(&self) -> usize {
        self.dreps.values().filter(|d| d.active).count()
    }

    /// Cold credentials eligible to authorize a committee hot key: the
    /// CURRENT committee members plus every cold credential named in
    /// `members_to_add` of any LIVE (not yet enacted/expired)
    /// `UpdateCommittee` proposal.
    ///
    /// Mirrors the Haskell GOVCERT rule (Conway/Rules/GovCert.hs,
    /// `checkAndOverwriteCommitteeMemberState`):
    ///
    /// ```haskell
    /// let isCurrentMember =
    ///       strictMaybe False (Map.member coldCred . committeeMembers) cgceCurrentCommittee
    ///     committeeUpdateContainsColdCred GovActionState {gasProposalProcedure} =
    ///       case pProcGovAction gasProposalProcedure of
    ///         UpdateCommittee _ _ newMembers _ -> Map.member coldCred newMembers
    ///         _ -> False
    ///     isPotentialFutureMember =
    ///       any committeeUpdateContainsColdCred cgceCommitteeProposals
    /// isCurrentMember || isPotentialFutureMember
    ///   ?! (injectFailure . ConwayCommitteeIsUnknown) coldCred
    /// ```
    ///
    /// A `CommitteeHotAuth` is therefore valid not only for current members
    /// but also for incoming members of a pending committee-update proposal
    /// (they may pre-authorize their hot key before enactment).
    pub fn committee_auth_eligible_members(&self) -> std::collections::HashSet<Hash32> {
        let mut eligible: std::collections::HashSet<Hash32> =
            self.committee_expiration.keys().copied().collect();
        for proposal in self.proposals.values() {
            if let dugite_primitives::transaction::GovAction::UpdateCommittee {
                members_to_add,
                ..
            } = &proposal.procedure.gov_action
            {
                eligible.extend(members_to_add.keys().map(credential_to_hash));
            }
        }
        eligible
    }
}

/// State of a governance proposal
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalState {
    pub procedure: ProposalProcedure,
    pub proposed_epoch: EpochNo,
    pub expires_epoch: EpochNo,
    pub yes_votes: u64,
    pub no_votes: u64,
    pub abstain_votes: u64,
    /// Monotonic on-chain submission order (issue #799).
    ///
    /// Haskell's `reorderActions` (`Governance/Internal.hs:534-544`) stable-sorts
    /// active proposals by `actionPriority` only; ties preserve the proposals
    /// `OMap`'s insertion (on-chain submission) order — NEVER `GovActionId`
    /// (hash) order. `proposals` here is an `ImblOrdMap<GovActionId, _>`, whose
    /// natural iteration order is by key (hash), so this field is required to
    /// recover submission order for the ratification tie-break sort.
    ///
    /// Assigned from the monotonic `GovernanceState::proposal_count` counter
    /// (read BEFORE it is incremented) at every ingest site: `eras/conway.rs`
    /// (live block-apply GOV rule), `state/governance.rs` `process_proposal` /
    /// `process_proposal_with_delta` (test/dead-path), and reconstructed by
    /// enumeration order when loading a Haskell ledger-state dump
    /// (`state/mod.rs::from_haskell_snapshot`, which decodes proposals from an
    /// on-wire `StrictSeq` that preserves the OMap's insertion order).
    pub submission_index: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StakeDistributionState {
    pub stake_map: HashMap<Hash32, Lovelace>,
}

/// Cardano uses a "mark / set / go" snapshot model:
/// - "mark" is the snapshot taken at the current epoch boundary
/// - "set" is the snapshot from the previous epoch (used for leader election)
/// - "go" is the snapshot from two epochs ago (used for reward calculation)
///
/// Matches Haskell's `SnapShots` data type. All snapshots start as empty
/// (not None) — Haskell uses `emptySnapShots` at genesis. The `ss_fee`
/// field is separate from individual snapshots, matching Haskell's `ssFee`
/// which is set by the SNAP rule at each epoch boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpochSnapshots {
    /// Snapshot from the most recent epoch boundary ("mark")
    pub mark: Option<StakeSnapshot>,
    /// Snapshot from one epoch ago ("set") — used for leader election
    pub set: Option<StakeSnapshot>,
    /// Snapshot from two epochs ago ("go") — used for reward distribution
    pub go: Option<StakeSnapshot>,
    /// Fee pot for the next RUPD (Haskell's `ssFee`).
    pub ss_fee: Lovelace,
    /// Block production from the previous epoch (Haskell's `nesBprev`).
    ///
    /// At each NEWEPOCH boundary: `bprev = current epoch blocks`, then
    /// counters are reset. The RUPD uses bprev for pool reward allocation.
    /// Separate from the snapshot rotation (bprev is from 1 epoch ago,
    /// while GO stake data is from 2 epochs ago).
    pub bprev_block_count: u64,
    pub bprev_blocks_by_pool: Arc<HashMap<Hash28, u64>>,
    /// Legacy field — RUPD now fires unconditionally at every epoch
    /// boundary (Issue #438: Haskell's `applyRUpd` runs at boundary 0→1
    /// with `ssFee = 0` from `emptySnapShots`, draining the genesis
    /// monetary-expansion tau cut from reserves to treasury).  Kept for
    /// snapshot wire-format compatibility.
    pub rupd_ready: bool,
}

impl Default for EpochSnapshots {
    fn default() -> Self {
        EpochSnapshots {
            mark: None,
            set: None,
            go: None,
            ss_fee: Lovelace(0),
            bprev_block_count: 0,
            bprev_blocks_by_pool: Arc::new(HashMap::new()),
            rupd_ready: false,
        }
    }
}

/// A snapshot of the stake distribution at an epoch boundary.
/// Uses `Arc` for large HashMaps to avoid deep-cloning during epoch rotation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StakeSnapshot {
    pub epoch: EpochNo,
    /// stake credential hash -> pool_id delegation
    pub delegations: Arc<HashMap<Hash32, Hash28>>,
    /// pool_id -> total active stake delegated to that pool
    pub pool_stake: HashMap<Hash28, Lovelace>,
    /// pool_id -> pool parameters at snapshot time
    pub pool_params: Arc<HashMap<Hash28, PoolRegistration>>,
    /// Individual stake per credential (for reward distribution and pledge verification)
    pub stake_distribution: Arc<HashMap<Hash32, Lovelace>>,
    /// Fee pot from the epoch this snapshot was captured (Haskell's _feeSS).
    /// Used by `calculate_rewards` (via the set snapshot) for RUPD deltaT1.
    pub epoch_fees: Lovelace,
    /// Total blocks produced in the epoch this snapshot was captured.
    /// Used for eta = actual_blocks / expected_blocks in reward calculation.
    pub epoch_block_count: u64,
    /// Per-pool block production in the epoch this snapshot was captured.
    /// Used for apparent performance in reward calculation.
    pub epoch_blocks_by_pool: Arc<HashMap<Hash28, u64>>,
}

impl StakeSnapshot {
    /// Create a default (empty) snapshot for use in struct update syntax.
    pub fn empty(epoch: EpochNo) -> Self {
        StakeSnapshot {
            epoch,
            delegations: Arc::new(HashMap::new()),
            pool_stake: HashMap::new(),
            pool_params: Arc::new(HashMap::new()),
            stake_distribution: Arc::new(HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolRegistration {
    pub pool_id: Hash28,
    pub vrf_keyhash: Hash32,
    pub pledge: Lovelace,
    pub cost: Lovelace,
    pub margin_numerator: u64,
    pub margin_denominator: u64,
    /// Reward account for pool operator rewards
    pub reward_account: Vec<u8>,
    /// Pool owner stake key hashes
    pub owners: Vec<Hash28>,
    /// Relay endpoints declared by the pool operator
    pub relays: Vec<Relay>,
    /// Pool metadata URL
    pub metadata_url: Option<String>,
    /// Pool metadata hash
    pub metadata_hash: Option<Hash32>,
}

impl LedgerState {
    // NOTE (#989): `reset_to_origin` used to live here. It reset `tip` and
    // `epoch` and nothing else, so a "forced re-replay" restarted at slot 0
    // carrying the snapshot's treasury, reserves, certificate state,
    // governance state, nonces and protocol parameters — #985's chimera
    // shape, ending with a snapshot of the chimera saved back to disk.
    //
    // It is deliberately NOT replaced with a fuller reset: genesis state
    // cannot be reconstructed from a `LedgerState` alone (it needs the
    // genesis UTxOs, delegates and parameters), so any in-place reset is
    // structurally incapable of being correct. The only caller now rebuilds
    // through `Node::init_fresh_ledger`, the same path a from-genesis start
    // takes, and wipes the UTxO store alongside it.

    /// Compute `drepExpiry` for a DRep whose last activity is the current epoch,
    /// matching Haskell's `computeDRepExpiryVersioned` / `computeDRepExpiry`.
    ///
    /// PV >= 10: `(current_epoch + drep_activity) - num_dormant_epochs`
    /// PV <  10: `current_epoch + drep_activity`  (bootstrap — dormant ignored)
    pub fn compute_drep_expiry(&self) -> EpochNo {
        let activity = self.epochs.protocol_params.drep_activity;
        let base = self.epoch.0 + activity;
        if self.epochs.protocol_params.protocol_version_major >= 10 {
            EpochNo(base.saturating_sub(self.gov.governance.num_dormant_epochs))
        } else {
            EpochNo(base)
        }
    }

    /// Snapshot the enacted governance roots (Haskell `Proposals.pRoots`) for
    /// the `InvalidPrevGovActionId` validation predicate.
    ///
    /// Single source of truth for every caller that builds a
    /// [`crate::validation::ValidationContext`] — block apply, N2C submission,
    /// and rollback revalidation — so the mempool and the block-apply path can
    /// never disagree about which proposals chain correctly.
    pub fn enacted_gov_roots(&self) -> crate::validation::EnactedGovRoots {
        crate::validation::EnactedGovRoots {
            pparam_update: self.gov.governance.enacted_pparam_update.clone(),
            hard_fork: self.gov.governance.enacted_hard_fork.clone(),
            committee: self.gov.governance.enacted_committee.clone(),
            constitution: self.gov.governance.enacted_constitution.clone(),
        }
    }

    /// The ledger-derived [`crate::validation::ValidationContext`] shared by
    /// every mempool path — N2C/N2N admission and post-block revalidation.
    ///
    /// # Why this exists (#996)
    ///
    /// Admission and block-apply each used to build their own context, and the
    /// admission one was a strict subset: it omitted `registered_vrf_keys`,
    /// `current_treasury`, `current_epoch`, `stake_key_deposits`,
    /// `vote_delegations`, `genesis_delegate_keys`, `update_quorum` and
    /// `constitution_script_hash`. Every omission is the same wedge — dugite
    /// admits a transaction the block-apply path (and therefore cardano-node)
    /// rejects, forges it, and every Haskell peer refuses the block forever.
    /// That is exactly how #996 presented: a `CommitteeHotAuth` for a resigned
    /// cold credential reached a forged block and permanently detached
    /// cardano-bp from the chain.
    ///
    /// It also unifies one live divergence: admission keyed the
    /// `CommitteeHotAuth` membership check off `committee_expiration` alone,
    /// while block-apply uses [`GovernanceState::committee_auth_eligible_members`],
    /// which additionally admits the `members_to_add` of any live
    /// `UpdateCommittee` proposal. Haskell's GOVCERT rule accepts a
    /// pre-authorization from such an incoming member
    /// (`isPotentialFutureMember`), so the narrower admission set was a false
    /// reject. The wider set is the correct one and is now the only one.
    ///
    /// Callers add their own non-ledger extras (network id, Plutus
    /// `SlotConfig`, mempool virtual-UTxO overlay) on top of this.
    pub fn mempool_validation_context(&self) -> crate::validation::ValidationContext {
        let gov = &self.gov.governance;

        let active_proposals: HashMap<
            dugite_primitives::transaction::GovActionId,
            crate::validation::ActiveProposal,
        > = gov
            .proposals
            .iter()
            .map(|(id, state)| {
                (
                    id.clone(),
                    crate::validation::ActiveProposal {
                        gov_action: state.procedure.gov_action.clone(),
                        return_addr: state.procedure.return_addr.clone(),
                        deposit: state.procedure.deposit,
                        expires_after_epoch: state.expires_epoch,
                        proposed_in_epoch: state.proposed_epoch,
                    },
                )
            })
            .collect();

        // `authorizedElectedHotCommitteeCredentials` in Haskell: the hot keys
        // of cold credentials that are in the ENACTED committee (drives the
        // PV >= 11 `UnelectedCommitteeVoters` predicate).
        let committee_authorized_elected_hot_keys: std::collections::HashSet<Hash32> = gov
            .committee_hot_keys
            .iter()
            .filter(|(cold, _)| gov.committee_expiration.contains_key(*cold))
            .map(|(_, hot)| *hot)
            .collect();

        let mut ctx = crate::validation::ValidationContext::new()
            .with_pools(self.certs.pool_params.keys().copied().collect())
            .with_dreps(gov.dreps.keys().copied().collect())
            // `psVRFKeyHashes` — the MAINTAINED registry, not a fold over the
            // live pool set (#1085). The derived version omitted pending
            // re-registrations, released a retiring pool's key early, and
            // could not represent a pre-PV11 duplicate; each of those is an
            // accept-where-Haskell-rejects.
            .with_vrf_key_registry(crate::validation::VrfKeyRegistry {
                occurrences: self
                    .certs
                    .vrf_key_hashes
                    .iter()
                    .map(|(h, c)| (*h, *c))
                    .collect(),
                pool_current_vrf: self
                    .certs
                    .pool_params
                    .values()
                    .map(|reg| (reg.pool_id, reg.vrf_keyhash))
                    .collect(),
            })
            .with_active_proposals(active_proposals)
            .with_enacted_gov_roots(self.enacted_gov_roots())
            .with_committee_authorized_hot_keys(gov.committee_hot_keys.values().copied().collect())
            .with_committee_authorized_elected_hot_keys(committee_authorized_elected_hot_keys)
            .with_committee_members(gov.committee_auth_eligible_members())
            .with_committee_resigned(gov.committee_resigned.keys().copied().collect())
            .with_vote_delegations(gov.vote_delegations.keys().copied().collect())
            .with_treasury(self.epochs.treasury.0)
            .with_epoch(self.epoch.0)
            // O(1) imbl structural clones, not deep copies.
            .with_reward_accounts_imbl(self.certs.reward_accounts.clone())
            .with_stake_key_deposits_imbl(self.certs.stake_key_deposits.clone())
            .with_genesis_delegate_keys(
                self.genesis_delegates
                    .values()
                    .map(|(delegate_hash, _)| *delegate_hash)
                    .collect(),
            )
            .with_update_quorum(self.update_quorum);

        if let Some(net) = self.node_network {
            ctx = ctx.with_network(net);
        }
        // #1028: pass the guardrail through INCLUDING its absence. If a
        // constitution is enacted, `c.script_hash` is authoritative — `None`
        // there means "no guardrail" (`SNothing`), which Haskell still enforces
        // equality against. Only the total absence of a constitution leaves the
        // context unset, which is the one case that skips the check.
        if let Some(c) = gov.constitution.as_ref() {
            ctx = ctx.with_constitution_guardrail(c.script_hash);
        }
        ctx
    }

    pub fn new(params: ProtocolParameters) -> Self {
        LedgerState {
            utxo: UtxoSubState {
                utxo_set: UtxoSet::new(),
                diff_seq: DiffSeq::new(),
                epoch_fees: Lovelace(0),
                pending_donations: Lovelace(0),
            },
            certs: CertSubState {
                delegations: ImblHashMap::new(),
                pool_params: Arc::new(HashMap::new()),
                future_pool_params: HashMap::new(),
                pending_retirements: HashMap::new(),
                vrf_key_hashes: ImblHashMap::new(),
                reward_accounts: ImblHashMap::new(),
                stake_key_deposits: ImblHashMap::new(),
                pool_deposits: HashMap::new(),
                total_stake_key_deposits: 0,
                pointer_map: HashMap::new(),
                stake_distribution: StakeDistributionState::default(),
                script_stake_credentials: std::collections::HashSet::new(),
                pending_mir_reserves: std::collections::HashMap::new(),
                pending_mir_treasury: std::collections::HashMap::new(),
                pending_mir_delta_reserves: 0,
                pending_mir_delta_treasury: 0,
            },
            gov: GovSubState {
                governance: Arc::new(GovernanceState::default()),
            },
            consensus: ConsensusSubState {
                evolving_nonce: Hash32::ZERO,
                candidate_nonce: Hash32::ZERO,
                epoch_nonce: Hash32::ZERO,
                previous_epoch_nonce: Hash32::ZERO,
                lab_nonce: Hash32::ZERO,
                last_epoch_block_nonce: Hash32::ZERO,
                extra_entropy: Hash32::ZERO,
                rolling_nonce: Hash32::ZERO,
                first_block_hash_of_epoch: None,
                prev_epoch_first_block_hash: None,
                epoch_blocks_by_pool: Arc::new(HashMap::new()),
                epoch_block_count: 0,
                opcert_counters: HashMap::new(),
            },
            epochs: EpochSubState {
                snapshots: EpochSnapshots::default(),
                treasury: Lovelace(0),
                reserves: Lovelace(MAX_LOVELACE_SUPPLY),
                pending_reward_update: None,
                // Haskell `emptyNonMyopic` at genesis — no pool has any history
                // yet and no RUPD has frozen a pot.
                non_myopic: non_myopic::NonMyopic::default(),
                last_applied_rupd: None,
                pending_pp_updates: BTreeMap::new(),
                future_pp_updates: BTreeMap::new(),
                needs_stake_rebuild: false,
                ptr_stake: HashMap::new(),
                ptr_stake_excluded: false,
                protocol_params: params.clone(),
                prev_protocol_params: params,
                prev_protocol_version_major: 6, // Genesis: Alonzo (proto 6)
                prev_d: dugite_primitives::transaction::Rational {
                    numerator: 1,
                    denominator: 1,
                }, // Genesis: d=1
                rupd_addrs_rew: None,           // #11: captured at startStep during apply
                rupd_pulser_started: false,
                rupd_monetary: None,
                rupd_fold: Default::default(),
                pending_avvm_return: 0,
            },
            tip: Tip::origin(),
            era: Era::Conway,
            pending_era_transition: None,
            epoch: EpochNo(0),
            epoch_length: 432000,          // mainnet default
            shelley_transition_epoch: 208, // mainnet default
            byron_epoch_length: 21600,     // mainnet default (10 * 2160)
            slot_config: SlotConfig::default(),
            genesis_hash: Hash32::ZERO,
            genesis_delegates: HashMap::new(),
            future_gen_delegs: HashMap::new(),
            update_quorum: default_update_quorum(),
            node_network: None,
            randomness_stabilisation_window: 172800, // 4k/f on mainnet: ceil(4*2160/0.05)
            stability_window_3kf: 129600,            // 3k/f on mainnet: ceil(3*2160/0.05)
            security_param: 2160,
            conway_genesis_init: None,
            max_lovelace_supply: MAX_LOVELACE_SUPPLY,
            phase2_apply_horizon: None,
            cached_validation_registry: None,
        }
    }

    /// Override the maximum lovelace supply (genesis `sgMaxLovelaceSupply`).
    ///
    /// Resets reserves to `max` so a subsequent `seed_genesis_utxos()` can
    /// safely deduct the initial-fund distribution. MUST be called before
    /// any UTxO seeding when the network uses a non-mainnet cap (devnets).
    /// No-op semantics for mainnet/preview/preprod (all use 45B).
    pub fn set_max_lovelace_supply(&mut self, max: u64) {
        self.max_lovelace_supply = max;
        self.epochs.reserves = Lovelace(max);
    }

    /// Create a `LedgerState` from a decoded Haskell `ExtLedgerState` snapshot.
    ///
    /// This is the core conversion used after Mithril ancillary import to restore
    /// a correct ledger state without replaying the entire chain from genesis.
    /// Every field is mapped from the Haskell structures; genesis-derived fields
    /// (`epoch_length`, `slot_config`, etc.) are applied by the caller afterward
    /// via the usual `set_epoch_length()` / `set_slot_config()` / etc. helpers.
    ///
    /// The UTxO set is NOT populated here — the caller must load UTxOs from the
    /// tvar file separately (they are too large to carry in the state struct).
    pub fn from_haskell_snapshot(
        hs: &dugite_serialization::haskell_snapshot::types::HaskellLedgerState,
    ) -> Self {
        use dugite_serialization::haskell_snapshot::types::*;

        // ── Tip ──────────────────────────────────────────────────────────
        let tip = Tip {
            point: Point::Specific(hs.tip_slot, hs.tip_hash),
            block_number: BlockNo(hs.tip_block_no),
        };

        // ── Protocol parameters ──────────────────────────────────────────
        let cur_pparams = hs.new_epoch_state.cur_pparams.clone();
        let prev_pparams = hs.new_epoch_state.prev_pparams.clone();
        // In Conway (proto >= 9), d = 0 (fully decentralized). The prev_d
        // field is a legacy cache; safe to set to 0/1 for Conway snapshots.
        let prev_d = dugite_primitives::transaction::Rational {
            numerator: 0,
            denominator: 1,
        };
        let prev_protocol_version_major = prev_pparams.protocol_version_major;

        // ── Delegations: (tag, Hash28) → Hash32 key, Hash28 pool value ──
        let mut delegations = HashMap::new();
        let mut reward_accounts = HashMap::new();
        let mut script_stake_credentials = std::collections::HashSet::new();
        let mut total_stake_key_deposits: u64 = 0;
        let mut stake_key_deposits = HashMap::new();
        let mut vote_delegations_map = HashMap::new();

        for ((tag, hash28), account) in &hs.new_epoch_state.cert_state.dstate.accounts {
            let cred_hash = haskell_credential_to_hash32(*tag, hash28);

            // Track script credentials
            if *tag == 1 {
                script_stake_credentials.insert(cred_hash);
            }

            // Delegation
            if let Some(pool_id) = &account.pool_delegation {
                delegations.insert(cred_hash, *pool_id);
            }

            // Reward balance (include zero-balance accounts — they are registered)
            reward_accounts.insert(cred_hash, Lovelace(account.balance));

            // Per-credential deposit tracking
            if account.deposit > 0 {
                total_stake_key_deposits += account.deposit;
                stake_key_deposits.insert(cred_hash, account.deposit);
            }

            // DRep vote delegation
            if let Some(drep) = &account.drep_delegation {
                let drep_native = convert_haskell_drep(drep);
                vote_delegations_map.insert(cred_hash, drep_native);
            }
        }

        // ── Pool registrations ───────────────────────────────────────────
        let mut pool_params_map = HashMap::new();
        let mut pool_deposits = HashMap::new();
        for (pool_id, pool) in &hs.new_epoch_state.cert_state.pstate.stake_pools {
            pool_params_map.insert(*pool_id, convert_pool_registration(*pool_id, pool));
            if pool.deposit > 0 {
                pool_deposits.insert(*pool_id, pool.deposit);
            }
        }

        // ── Future pool params ───────────────────────────────────────────
        let mut future_pool_params = HashMap::new();
        for (pool_id, pool) in &hs.new_epoch_state.cert_state.pstate.future_pool_params {
            future_pool_params.insert(*pool_id, convert_pool_registration(*pool_id, pool));
        }

        // ── Pending retirements ──────────────────────────────────────────
        let pending_retirements = hs.new_epoch_state.cert_state.pstate.retirements.clone();

        // ── Genesis delegates ────────────────────────────────────────────
        let genesis_delegates = hs
            .new_epoch_state
            .cert_state
            .dstate
            .genesis_delegates
            .clone();

        // ── Opcert counters ──────────────────────────────────────────────
        let opcert_counters = hs.praos_state.opcert_counters.clone();

        // ── Epoch block production ───────────────────────────────────────
        let epoch_blocks_by_pool = hs.new_epoch_state.blocks_made_cur.clone();
        let epoch_block_count: u64 = epoch_blocks_by_pool.values().sum();

        // ── Stake snapshots ──────────────────────────────────────────────
        //
        // Backfill snapshot pool_params owners/relays/metadata from the
        // live `psStakePools` registrations (`pool_params_map`).  cardano-node
        // ≥ 11.0.1 uses the `StakePoolSnapShot` wire layout that omits owners
        // entirely (they live in `psStakePools` per Haskell architecture);
        // the on-the-wire snapshot pool_params then carry empty owners which
        // fails dugite's pledge check at `compute_reward_update`, silently
        // dropping 99.9% of rewards.  Issue #668.
        let mark_snapshot = convert_stake_snapshot(
            &hs.new_epoch_state.snapshots.mark,
            hs.epoch,
            &pool_params_map,
        );
        let set_snapshot = convert_stake_snapshot(
            &hs.new_epoch_state.snapshots.set,
            EpochNo(hs.epoch.0.saturating_sub(1)),
            &pool_params_map,
        );
        let go_snapshot = convert_stake_snapshot(
            &hs.new_epoch_state.snapshots.go,
            EpochNo(hs.epoch.0.saturating_sub(2)),
            &pool_params_map,
        );

        // bprev = previous epoch's block production (used by RUPD)
        let bprev_blocks_by_pool = hs.new_epoch_state.blocks_made_prev.clone();
        let bprev_block_count: u64 = bprev_blocks_by_pool.values().sum();

        let snapshots = EpochSnapshots {
            mark: Some(mark_snapshot),
            set: Some(set_snapshot),
            go: Some(go_snapshot),
            ss_fee: Lovelace(hs.new_epoch_state.snapshots.fee),
            bprev_block_count,
            bprev_blocks_by_pool: Arc::new(bprev_blocks_by_pool),
            rupd_ready: true,
        };

        // ── Governance state ─────────────────────────────────────────────
        let mut gov = GovernanceState::default();

        // DRep registrations
        for ((tag, hash28), drep_state) in &hs.new_epoch_state.cert_state.vstate.dreps {
            let cred = if *tag == 0 {
                Credential::VerificationKey(*hash28)
            } else {
                Credential::Script(*hash28)
            };
            let cred_hash = cred.to_typed_hash32();
            let anchor = drep_state.anchor.as_ref().map(|(url, hash)| Anchor {
                url: url.clone(),
                data_hash: *hash,
            });
            gov.dreps.insert(
                cred_hash,
                DRepRegistration {
                    credential: cred,
                    deposit: Lovelace(drep_state.deposit),
                    anchor,
                    registered_epoch: EpochNo(0), // Not tracked in Haskell snapshot
                    drep_expiry: drep_state.expiry,
                    active: hs.epoch.0 <= drep_state.expiry.0,
                    // Filled in below, once the forward map is available.
                    delegs: Default::default(),
                },
            );
        }

        // Vote delegations
        gov.vote_delegations = vote_delegations_map.into_iter().collect();

        // `drepDelegs`, rebuilt from the forward map rather than decoded.
        //
        // The importer SKIPS `DRepState`'s 4th CBOR field (the delegator set)
        // and a comment claimed it was "reconstructed from DState accounts".
        // Nothing reconstructed it — a field with no writer, so every imported
        // node had an empty set and a later deregistration cleared nothing
        // (#1084, and #1067's shape: the import path silently dropping a record
        // while looking authoritative).
        //
        // Rebuilding is EXACT here, and only here. From protocol version 10 the
        // hard-fork rule `updateDRepDelegations` reindexes every set from the
        // accounts map, and PV10+ delegation processing keeps the two in step,
        // so `delegs` and the forward map agree by construction. Below PV10
        // they do NOT — ledger #4772 leaves stale entries — but a Mithril
        // snapshot tracks the live chain, and preprod and mainnet are both past
        // that fork. Decoding the set would be strictly better and is the right
        // change if a PV9 snapshot ever has to be imported.
        for (stake_cred, drep) in gov.vote_delegations.iter() {
            if let Some(target) = drep.credential_hash32() {
                if let Some(state) = gov.dreps.get_mut(&target) {
                    state.delegs.insert(*stake_cred);
                }
            }
        }

        // Committee state
        for ((tag, hash28), auth) in &hs.new_epoch_state.cert_state.vstate.committee_state {
            let cold_hash = haskell_credential_to_hash32(*tag, hash28);
            if *tag == 1 {
                gov.script_committee_credentials.insert(cold_hash);
            }
            match auth {
                HaskellCommitteeAuth::Hot(hot_tag, hot_hash) => {
                    let hot_h32 = haskell_credential_to_hash32(*hot_tag, hot_hash);
                    gov.committee_hot_keys.insert(cold_hash, hot_h32);
                    if *hot_tag == 1 {
                        gov.script_committee_hot_credentials.insert(hot_h32);
                    }
                }
                HaskellCommitteeAuth::Resigned(anchor) => {
                    let a = anchor.as_ref().map(|(url, hash)| Anchor {
                        url: url.clone(),
                        data_hash: *hash,
                    });
                    gov.committee_resigned.insert(cold_hash, a);
                }
            }
        }

        // Constitutional Committee membership + threshold.  The Haskell
        // snapshot stores the canonical `Committee` value (members map +
        // voting threshold) inside `cgsCommittee` in the gov-state CBOR;
        // upstream parsing captures it verbatim as `committee_raw` bytes.
        // Without decoding it here, `committee_expiration` would contain
        // only the Conway-genesis seeds — so any UpdateCommittee action
        // enacted before the snapshot anchor (e.g. preview tx ac99…
        // enacted at epoch 1011 adding 7 of 8 current members) would be
        // invisible to `query committee-state` and to CC quorum logic.
        // See issue #485 (P0).
        if let Some(raw) = &hs.new_epoch_state.gov_state.committee_raw {
            match dugite_serialization::haskell_snapshot::govstate::decode_committee(raw) {
                Ok((committee, _consumed)) => {
                    use dugite_primitives::transaction::Rational;
                    for ((cold_tag, cold_hash28), expiry) in &committee.members {
                        let cold = haskell_credential_to_hash32(*cold_tag, cold_hash28);
                        gov.committee_expiration.insert(cold, EpochNo(*expiry));
                        if *cold_tag == 1 {
                            gov.script_committee_credentials.insert(cold);
                        }
                    }
                    let (num, den) = committee.threshold;
                    gov.committee_threshold = Some(Rational {
                        numerator: num,
                        denominator: den,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to decode committee_raw from Haskell snapshot; \
                         committee_expiration left empty (governance queries \
                         and CC quorum checks will be incorrect until next \
                         from-genesis sync)"
                    );
                }
            }
        }

        // Dormant epochs
        gov.num_dormant_epochs = hs.new_epoch_state.cert_state.vstate.dormant_epochs;

        // Constitution
        if let Some(ref c) = hs.new_epoch_state.gov_state.constitution {
            gov.constitution = Some(Constitution {
                anchor: Anchor {
                    url: c.anchor_url.clone(),
                    data_hash: c.anchor_hash,
                },
                script_hash: c.script_hash,
            });
        }

        // ── Proposals (issue #670) ──────────────────────────────────────
        //
        // Haskell stores active governance proposals in `gov_state.proposals_raw`
        // as a CBOR list of `GovActionState` records. dugite mirrors this
        // verbatim in `gov.proposals` (an `OMap GovActionId ProposalState`).
        // Each `GovActionState` carries:
        //   * the full `ProposalProcedure` (deposit, return_addr, gov_action,
        //     anchor)
        //   * the per-voter `gas{Committee,DRep,StakePool}Votes` maps —
        //     tallied here into `(yes_votes, no_votes, abstain_votes)` and
        //     also expanded into `gov.votes_by_action` for parity with the
        //     from-genesis replay path.
        //   * `gasProposedIn` and `gasExpiresAfter` epoch numbers.
        //
        // Without this decode the imported ledger had `proposals.len() = 0`
        // and `governance` diverged from the from-genesis ledger by the
        // full proposal-state payload (~5.7 MB on preview epoch 1308).
        match dugite_serialization::haskell_snapshot::decode_proposals_with_roots(
            &hs.new_epoch_state.gov_state.proposals_raw,
        ) {
            Ok(decoded) => {
                use dugite_primitives::transaction::{Vote, Voter, VotingProcedure};
                use dugite_serialization::haskell_snapshot::types::HaskellVote;

                // ── Enacted roots (`pRoots` / `toPrevGovActionIds`) ──────
                //
                // #898: these MUST come from the snapshot. They record the
                // last *enacted* action per purpose and are not recoverable
                // from the active proposal set — a purpose's root is usually
                // far older than any in-flight proposal, and may have no live
                // descendants at all.
                //
                // Leaving them `None` makes the GOV rule (`proposalsAddAction`
                // / `prev_action_matches_enacted_root`) silently drop every
                // later proposal that legitimately chains onto a real root.
                // On preview that dropped an `UpdateCommittee` proposal, so
                // its 1000-ADA deposit was never refunded to the return
                // account; that account's snapshot stake stayed
                // 1_000_000_000 lovelace below Haskell's, which depressed
                // `totalActiveStake` → every pool's `appPerf` → every reward,
                // until an exact-drain withdrawal failed and chain advance
                // halted permanently.
                let to_gid =
                    |id: &dugite_serialization::haskell_snapshot::types::HaskellGovActionId| {
                        GovActionId {
                            transaction_id: id.tx_hash,
                            action_index: id.index as u32,
                        }
                    };
                gov.enacted_pparam_update = decoded.roots.pparam_update.as_ref().map(to_gid);
                gov.enacted_hard_fork = decoded.roots.hard_fork.as_ref().map(to_gid);
                gov.enacted_committee = decoded.roots.committee.as_ref().map(to_gid);
                gov.enacted_constitution = decoded.roots.constitution.as_ref().map(to_gid);
                info!(
                    pparam_update = ?gov.enacted_pparam_update.as_ref().map(|i| i.transaction_id.to_hex()),
                    hard_fork = ?gov.enacted_hard_fork.as_ref().map(|i| i.transaction_id.to_hex()),
                    committee = ?gov.enacted_committee.as_ref().map(|i| i.transaction_id.to_hex()),
                    constitution = ?gov.enacted_constitution.as_ref().map(|i| i.transaction_id.to_hex()),
                    "Loaded enacted governance roots from Haskell snapshot"
                );

                let haskell_proposals = decoded.actions;
                let proposal_count = haskell_proposals.len();
                for (submission_index, gas) in haskell_proposals.into_iter().enumerate() {
                    let action_id = GovActionId {
                        transaction_id: gas.gas_id.tx_hash,
                        action_index: gas.gas_id.index as u32,
                    };

                    // Tally votes + build votes_by_action.
                    let mut yes: u64 = 0;
                    let mut no: u64 = 0;
                    let mut abstain: u64 = 0;
                    let mut votes: ImblOrdMap<Voter, VotingProcedure> = ImblOrdMap::new();
                    let to_vote = |v: HaskellVote| match v {
                        HaskellVote::No => Vote::No,
                        HaskellVote::Yes => Vote::Yes,
                        HaskellVote::Abstain => Vote::Abstain,
                    };
                    let tally =
                        |v: HaskellVote, yes: &mut u64, no: &mut u64, abstain: &mut u64| match v {
                            HaskellVote::Yes => *yes += 1,
                            HaskellVote::No => *no += 1,
                            HaskellVote::Abstain => *abstain += 1,
                        };
                    let to_credential = |tag: u8, h: Hash28| -> Credential {
                        if tag == 0 {
                            Credential::VerificationKey(h)
                        } else {
                            Credential::Script(h)
                        }
                    };
                    for ((tag, hash28), v) in &gas.committee_votes {
                        tally(*v, &mut yes, &mut no, &mut abstain);
                        votes.insert(
                            Voter::ConstitutionalCommittee(to_credential(*tag, *hash28)),
                            VotingProcedure {
                                vote: to_vote(*v),
                                anchor: None,
                            },
                        );
                    }
                    for ((tag, hash28), v) in &gas.drep_votes {
                        tally(*v, &mut yes, &mut no, &mut abstain);
                        votes.insert(
                            Voter::DRep(to_credential(*tag, *hash28)),
                            VotingProcedure {
                                vote: to_vote(*v),
                                anchor: None,
                            },
                        );
                    }
                    for (pool_id, v) in &gas.pool_votes {
                        tally(*v, &mut yes, &mut no, &mut abstain);
                        // Voter::StakePool key is the 32-byte typed credential
                        // (28-byte key hash zero-padded to 32 with type-byte 0
                        // at position 28). Pool keys are key credentials, so
                        // the type-byte stays 0 and `to_hash32_padded` suffices.
                        votes.insert(
                            Voter::StakePool(pool_id.to_hash32_padded()),
                            VotingProcedure {
                                vote: to_vote(*v),
                                anchor: None,
                            },
                        );
                    }

                    let state = ProposalState {
                        procedure: gas.procedure,
                        proposed_epoch: gas.proposed_in,
                        expires_epoch: gas.expires_after,
                        yes_votes: yes,
                        no_votes: no,
                        abstain_votes: abstain,
                        // #799: `decode_proposals_with_roots` returns entries in the order
                        // they appear on the wire, which is a `StrictSeq` that
                        // preserves the Haskell `Proposals` OMap's insertion
                        // (on-chain submission) order — see the doc comment on
                        // `decode_proposals_with_roots`. Enumeration index is therefore a
                        // faithful reconstruction of `submission_index`.
                        submission_index: submission_index as u64,
                    };
                    gov.proposals.insert(action_id.clone(), state);
                    if !votes.is_empty() {
                        gov.votes_by_action.insert(action_id, votes);
                    }
                }
                // #799: seed the monotonic submission counter past every
                // reconstructed proposal so that proposals submitted AFTER
                // this snapshot import get strictly higher `submission_index`
                // values than all pre-existing ones (correct relative
                // ordering for the ratification tie-break sort). Without
                // this, `gov.proposal_count` stays at its `Default` (0),
                // colliding with `submission_index: 0` assigned above to the
                // first reconstructed proposal.
                gov.proposal_count = gov.proposal_count.max(proposal_count as u64);
                info!(
                    proposal_count,
                    "Loaded active governance proposals from Haskell snapshot"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to decode proposals_raw from Haskell snapshot; \
                     gov.proposals left empty (ratification/enactment will be \
                     incorrect until the next from-genesis sync)"
                );
            }
        }

        // ── Build stake distribution from mark snapshot ──────────────────
        // The "instant stake" from Haskell is the authoritative source.
        let mut stake_map = HashMap::new();
        for ((tag, hash28), lovelace) in &hs.new_epoch_state.instant_stake {
            let cred_hash = haskell_credential_to_hash32(*tag, hash28);
            stake_map.insert(cred_hash, Lovelace(*lovelace));
        }

        info!(
            epoch = hs.epoch.0,
            tip_slot = hs.tip_slot.0,
            tip_block = hs.tip_block_no,
            treasury = hs.new_epoch_state.treasury,
            reserves = hs.new_epoch_state.reserves,
            delegations = delegations.len(),
            pools = pool_params_map.len(),
            reward_accounts = reward_accounts.len(),
            dreps = gov.dreps.len(),
            stake_keys = stake_map.len(),
            "Building LedgerState from Haskell snapshot"
        );

        LedgerState {
            utxo: UtxoSubState {
                utxo_set: UtxoSet::new(),
                diff_seq: DiffSeq::new(),
                epoch_fees: Lovelace(hs.new_epoch_state.fees),
                pending_donations: Lovelace(hs.new_epoch_state.donation),
            },
            certs: CertSubState {
                // Convert std::HashMap (built above) → imbl::HashMap for CertSubState.
                delegations: delegations.into_iter().collect::<ImblHashMap<_, _>>(),
                pool_params: Arc::new(pool_params_map),
                future_pool_params,
                pending_retirements,
                // `psVRFKeyHashes`, taken from the snapshot rather than
                // recomputed. The decoder has always read PState's field [0]
                // (`map(bytes(32) -> uint)`) — it simply had no destination
                // until #1085 gave it one.
                //
                // Deriving it from the pool set here would be wrong for the
                // reason documented at `CertSubState::vrf_key_hashes`: after a
                // POOLREAP that deletes a superseded key still held by another
                // pool, upstream's map and any derived count disagree. A
                // Mithril import lands at PV11 on every live network, which is
                // exactly where the rule is enforced.
                vrf_key_hashes: hs
                    .new_epoch_state
                    .cert_state
                    .pstate
                    .vrf_key_hashes
                    .iter()
                    .map(|(h, c)| (*h, *c))
                    .collect::<ImblHashMap<_, _>>(),
                reward_accounts: reward_accounts.into_iter().collect::<ImblHashMap<_, _>>(),
                stake_key_deposits: stake_key_deposits
                    .into_iter()
                    .collect::<ImblHashMap<_, _>>(),
                pool_deposits,
                total_stake_key_deposits,
                pointer_map: HashMap::new(), // Conway era: pointers excluded
                stake_distribution: StakeDistributionState { stake_map },
                script_stake_credentials,
                pending_mir_reserves: HashMap::new(),
                pending_mir_treasury: HashMap::new(),
                pending_mir_delta_reserves: 0,
                pending_mir_delta_treasury: 0,
            },
            gov: GovSubState {
                governance: Arc::new(gov),
            },
            consensus: ConsensusSubState {
                evolving_nonce: hs.praos_state.evolving_nonce,
                candidate_nonce: hs.praos_state.candidate_nonce,
                epoch_nonce: hs.praos_state.epoch_nonce,
                previous_epoch_nonce: hs.praos_state.previous_epoch_nonce,
                lab_nonce: hs.praos_state.lab_nonce,
                last_epoch_block_nonce: hs.praos_state.last_epoch_block_nonce,
                // Haskell snapshots are imported for current-era (Conway) state
                // where ppExtraEntropy was removed → always NeutralNonce.
                extra_entropy: Hash32::ZERO,
                rolling_nonce: Hash32::ZERO,
                first_block_hash_of_epoch: None,
                prev_epoch_first_block_hash: None,
                epoch_blocks_by_pool: Arc::new(epoch_blocks_by_pool),
                epoch_block_count,
                opcert_counters,
            },
            epochs: EpochSubState {
                snapshots,
                treasury: Lovelace(hs.new_epoch_state.treasury),
                reserves: Lovelace(hs.new_epoch_state.reserves),
                pending_reward_update: None,
                // Carried across from the imported snapshot rather than reset.
                // The likelihoods are a 0.9-decayed accumulator over every past
                // epoch, so a fresh node that drops them needs ~20 epochs to
                // converge and reports plausible-but-wrong rankings the whole
                // time, with nothing to signal that it is wrong.
                non_myopic: non_myopic::NonMyopic::from_haskell_snapshot(
                    &hs.new_epoch_state.non_myopic,
                ),
                last_applied_rupd: None,
                pending_pp_updates: BTreeMap::new(),
                future_pp_updates: BTreeMap::new(),
                needs_stake_rebuild: false,
                ptr_stake: HashMap::new(), // Conway: pointers excluded
                ptr_stake_excluded: true,  // Conway: already excluded
                protocol_params: cur_pparams,
                prev_protocol_params: prev_pparams,
                prev_protocol_version_major,
                prev_d,
                rupd_addrs_rew: None, // #11: captured at startStep during apply
                rupd_pulser_started: false,
                rupd_monetary: None,
                rupd_fold: Default::default(),
                pending_avvm_return: 0,
            },
            tip,
            era: Era::Conway,
            pending_era_transition: None,
            epoch: hs.epoch,
            // Genesis-derived fields — caller applies via set_epoch_length() etc.
            epoch_length: 432000,
            shelley_transition_epoch: 0,
            byron_epoch_length: 0,
            slot_config: SlotConfig::default(), // Will be set by set_slot_config()
            genesis_hash: Hash32::ZERO,         // Will be set by set_genesis_hash()
            genesis_delegates,
            // Haskell snapshots are imported at a Conway-era tip, where the
            // GenesisKeyDelegation/FutureGenDeleg mechanism is retired
            // (AtMostEra "Babbage") — there is no live queue to decode, and
            // the Haskell snapshot decoder does not carry `dsFutureGenDelegs`
            // in the first place. Empty is correct here.
            future_gen_delegs: HashMap::new(),
            update_quorum: 5,
            node_network: None, // Will be set by caller
            // Will be recalculated by set_epoch_length()
            randomness_stabilisation_window: 0,
            stability_window_3kf: 0,
            security_param: 0,         // Will be set by set_epoch_length()
            conway_genesis_init: None, // Will be set by caller
            max_lovelace_supply: MAX_LOVELACE_SUPPLY,
            phase2_apply_horizon: None,
            cached_validation_registry: None,
        }
    }

    /// Set the slot configuration for Plutus time conversion
    pub fn set_slot_config(&mut self, slot_config: SlotConfig) {
        self.slot_config = slot_config;
        debug!(
            "Ledger: slot config (zero_time={}, zero_slot={}, slot_length={})",
            slot_config.zero_time, slot_config.zero_slot, slot_config.slot_length,
        );
    }

    /// Clone without UTxO data — for LedgerSeq checkpoints.
    ///
    /// Returns a LedgerState with an empty UtxoSet and DiffSeq. All non-UTxO
    /// state (delegations, pools, rewards, governance, epochs, consensus) is
    /// cloned normally. UTxO state is reconstructed from diffs during
    /// `LedgerSeq::state_at_index()`.
    pub fn clone_without_utxos(&self) -> Self {
        LedgerState {
            utxo: UtxoSubState {
                utxo_set: UtxoSet::new(),
                diff_seq: DiffSeq::new(),
                epoch_fees: self.utxo.epoch_fees,
                pending_donations: self.utxo.pending_donations,
            },
            certs: self.certs.clone(),
            gov: self.gov.clone(),
            consensus: self.consensus.clone(),
            epochs: self.epochs.clone(),
            tip: self.tip.clone(),
            era: self.era,
            pending_era_transition: self.pending_era_transition,
            epoch: self.epoch,
            epoch_length: self.epoch_length,
            shelley_transition_epoch: self.shelley_transition_epoch,
            byron_epoch_length: self.byron_epoch_length,
            slot_config: self.slot_config,
            genesis_hash: self.genesis_hash,
            genesis_delegates: self.genesis_delegates.clone(),
            future_gen_delegs: self.future_gen_delegs.clone(),
            update_quorum: self.update_quorum,
            node_network: self.node_network,
            randomness_stabilisation_window: self.randomness_stabilisation_window,
            stability_window_3kf: self.stability_window_3kf,
            security_param: self.security_param,
            conway_genesis_init: self.conway_genesis_init.clone(),
            max_lovelace_supply: self.max_lovelace_supply,
            phase2_apply_horizon: None,
            cached_validation_registry: None,
        }
    }

    /// Configure the epoch length (from Shelley genesis)
    /// Re-derive the stability windows and the RUPD pulser flag after genesis
    /// protocol parameters have been applied (#1072).
    ///
    /// # Why this is separate from [`Self::set_epoch_length`]
    ///
    /// `set_epoch_length` computes `4k/f` from
    /// `active_slot_coeff_rational()`, but the Mithril / Haskell-snapshot
    /// import path calls it BEFORE it copies `active_slots_coeff` out of
    /// genesis — the decoded `array(31)` PParams carry no `f`, so the default
    /// `0.05` is used. That was harmless while the window fed only the pv<=6
    /// reward prefilter; it is not harmless now that the window decides whether
    /// a reward update happens AT ALL, in every era. On an `f = 0.5` network
    /// the window would be 10x too wide.
    ///
    /// It also derives `rupd_pulser_started`. An imported ledger is NOT a fresh
    /// one: if its tip slot is already past `epoch_first + 4k/f` then the chain
    /// has seen a qualifying block and Haskell's imported `NewEpochState`
    /// carries `nesRu = SJust`. Starting at `false` would skip a reward update
    /// Haskell applies — the same permanent divergence #1072 is about, arriving
    /// through the import path instead.
    ///
    /// Call AFTER genesis params are in `epochs.protocol_params`.
    pub fn finalise_genesis_derived_windows(&mut self) {
        self.set_epoch_length(self.epoch_length, self.security_param);

        let epoch_first = self.first_slot_of_epoch(self.epoch.0);
        let window = crate::state::reward_pulser::RewardWindow::new(
            epoch_first,
            self.randomness_stabilisation_window,
        );
        let tip_slot = self
            .tip
            .point
            .slot()
            .unwrap_or(dugite_primitives::time::SlotNo(0));
        self.epochs.rupd_pulser_started =
            window.classify(tip_slot) != crate::state::reward_pulser::RewardTiming::TooEarly;
    }

    pub fn set_epoch_length(&mut self, epoch_length: u64, security_param: u64) {
        self.epoch_length = epoch_length;
        self.security_param = security_param;
        // Compute BOTH stability windows:
        //   randomness_stabilisation_window = ceiling(4k/f) — Conway+ candidate freeze
        //   stability_window_3kf            = ceiling(3k/f) — Alonzo/Babbage candidate freeze
        let (f_num, f_den) = self.epochs.protocol_params.active_slot_coeff_rational();
        self.randomness_stabilisation_window =
            dugite_primitives::protocol_params::ceiling_div_by_rational(
                4,
                security_param,
                f_num,
                f_den,
            );
        self.stability_window_3kf = dugite_primitives::protocol_params::ceiling_div_by_rational(
            3,
            security_param,
            f_num,
            f_den,
        );
        debug!(
            "Ledger: epoch length={}, rsw_4kf={}, sw_3kf={}, k={}",
            epoch_length,
            self.randomness_stabilisation_window,
            self.stability_window_3kf,
            security_param,
        );
    }

    /// Configure the Byron→Shelley hard fork boundary.
    ///
    /// `shelley_transition_epoch` is the number of Byron epochs before
    /// Shelley starts (e.g. mainnet=208, guild=2, preview=0).
    /// `byron_epoch_length` is 10*k in Byron slots.
    pub fn set_shelley_transition(
        &mut self,
        shelley_transition_epoch: u64,
        byron_epoch_length: u64,
    ) {
        self.shelley_transition_epoch = shelley_transition_epoch;
        self.byron_epoch_length = byron_epoch_length;
        debug!(
            "Ledger: Shelley transition at epoch {}, byron_epoch_len={}",
            shelley_transition_epoch, byron_epoch_length,
        );
    }

    /// Compute the HFC epoch number for a given absolute slot.
    ///
    /// Uses the CNCLI formula: for slots in the Shelley era,
    /// epoch = shelley_transition_epoch + (slot - byron_slots) / epoch_length
    /// where byron_slots = byron_epoch_length * shelley_transition_epoch.
    pub fn epoch_of_slot(&self, slot: u64) -> u64 {
        let byron_slots = self
            .byron_epoch_length
            .saturating_mul(self.shelley_transition_epoch);
        if slot < byron_slots {
            // Still in Byron era
            slot.checked_div(self.byron_epoch_length).unwrap_or(0)
        } else {
            // Shelley era
            let shelley_slots = slot - byron_slots;
            self.shelley_transition_epoch
                .saturating_add(shelley_slots / self.epoch_length)
        }
    }

    /// Compute the first slot of the epoch that contains the given slot.
    /// Uses saturating arithmetic to prevent u64 overflow with extreme values.
    pub fn first_slot_of_epoch(&self, epoch: u64) -> u64 {
        if epoch < self.shelley_transition_epoch {
            // Byron epoch
            epoch.saturating_mul(self.byron_epoch_length)
        } else {
            // Shelley epoch
            let byron_slots = self
                .byron_epoch_length
                .saturating_mul(self.shelley_transition_epoch);
            byron_slots.saturating_add(
                (epoch - self.shelley_transition_epoch).saturating_mul(self.epoch_length),
            )
        }
    }

    /// Return the epoch nonce that should be used to verify a block at `slot`.
    ///
    /// During normal (non-epoch-crossing) processing, this is `self.epoch_nonce`.
    /// When a block is the first block of the NEXT epoch (i.e. the epoch-transition
    /// block), its VRF proof was generated with the *new* epoch's nonce, which is
    /// computed by the TICKN rule at the epoch boundary:
    ///
    ///   epochNonce' = candidateNonce ⭒ lastEpochBlockNonce
    ///
    /// The ledger does not advance the epoch nonce until `apply_block` fires
    /// `process_epoch_transition`.  Therefore, if validation runs before apply
    /// (as in the batch-then-apply pattern in `process_forward_blocks`), the
    /// first block of a new epoch would be validated against the *old* nonce,
    /// causing a spurious VRF failure that then blocks the epoch transition from
    /// ever firing — permanently stalling the node.
    ///
    /// This function pre-computes the TICKN nonce for `slot` without mutating any
    /// state, so the validation loop can inject the correct nonce before calling
    /// `validate_header_full`.
    ///
    /// For blocks in the same epoch as the current ledger state, the existing
    /// `self.epoch_nonce` is returned directly.  For blocks exactly one epoch
    /// ahead, the next-epoch nonce is computed from `candidate_nonce` and
    /// `last_epoch_block_nonce`.  For blocks more than one epoch ahead (which
    /// should not occur at tip), `self.epoch_nonce` is returned as a fallback
    /// (VRF verification will fail non-fatally in non-strict mode, or produce
    /// an informative error in strict mode).
    /// Forecast the pool distribution `(pool_stake, total_active_stake)` for
    /// the leader VRF check at the given slot, mirroring Haskell's
    /// `protocolLedgerView` over a possibly-ticked ledger state
    /// (`Tickf.hs:nesPd = ssStakeMarkPoolDistr nes.esSnapshots`).
    ///
    /// At a normal forge — `slot`'s epoch == `self.epoch` — this returns the
    /// values from `snapshots.set` exactly as before. The "set" snapshot was
    /// rotated from "mark" at the most recent NEWEPOCH and is the canonical
    /// pool distribution for leader checks in the current epoch.
    ///
    /// At an epoch-boundary forge — `slot`'s epoch == `self.epoch + 1`, the
    /// case where our wall-clock slot has crossed into a new epoch but no
    /// peer block of the new epoch has applied yet — `snapshots.set` still
    /// holds the *previous* epoch's pool distribution (it has not been
    /// rotated yet because `process_epoch_transition` only fires on apply).
    /// Haskell handles this via a `forecastFor` / TICKF that effectively
    /// performs the rotation on the fly: the new `nesPd` is taken from
    /// `ssStakeMarkPoolDistr` of the pre-NEWEPOCH state — which is exactly
    /// `snapshots.mark` in dugite's data model. That is the value that would
    /// become the new `set` after NEWEPOCH runs.
    ///
    /// Returns `(0, 0)` when the requested snapshot is unavailable (e.g.
    /// genesis bootstrap before any snapshot has been computed) — the
    /// caller treats that as "no stake, can't be leader" which matches the
    /// Haskell behaviour in the same boundary case.
    ///
    /// Forecasting more than one epoch ahead is unsupported (the intermediate
    /// epoch's mark would itself depend on a future NEWEPOCH that hasn't
    /// run); we return the current-epoch values as a fallback, which the
    /// stability-window gate (`TraceNoLedgerView`) will normally have caught
    /// before this point.
    pub fn pool_distribution_for_slot(
        &self,
        slot: u64,
        pool_id: &dugite_primitives::hash::Hash28,
    ) -> (u64, u64) {
        let block_epoch = self.epoch_of_slot(slot);
        let cur_epoch = self.epoch.0;
        let snapshot = if block_epoch == cur_epoch.saturating_add(1) {
            // Epoch-boundary forecast: read pre-rotation `mark`, which becomes
            // the new `set` (= new `nesPd`) after NEWEPOCH.
            self.epochs.snapshots.mark.as_ref()
        } else {
            // Same epoch (or behind, which should not happen at tip): the
            // post-rotation `set` is the active pool distribution.
            self.epochs.snapshots.set.as_ref()
        };
        match snapshot {
            Some(s) => {
                let total: u64 = s.pool_stake.values().map(|stake| stake.0).sum();
                let pool = s.pool_stake.get(pool_id).map(|stake| stake.0).unwrap_or(0);
                (pool, total)
            }
            None => {
                // Genesis / cold-start fallback: no SNAP rotation has populated
                // `set`/`mark` yet. Compute pool distribution from live
                // delegations + stake/reward balances. This mirrors Haskell's
                // genesis `nesPd` (computed from `sgsPools`) which is a
                // SEPARATE field from `esSnapshots` and is used for leader
                // election in epoch 0 even though `ssStakeGo/Set/Mark` are
                // all `mempty`. Without this fallback, leaving the snapshots
                // unseeded at genesis would silence forge-eligibility in
                // epoch 0 — but pre-seeding them (the prior approach) leaks
                // the genesis stake into `ssStakeGo` at boundary 1→2 via
                // SNAP rotation and causes the RUPD per-pool distribution to
                // mis-fire ~22 ADA at boundary 1→2 vs Haskell's 0.
                let mut pool_stake: HashMap<dugite_primitives::hash::Hash28, u64> =
                    HashMap::with_capacity(self.certs.pool_params.len());
                for (cred_hash, delegated_pool) in self.certs.delegations.iter() {
                    if !self.certs.pool_params.contains_key(delegated_pool) {
                        continue;
                    }
                    let utxo_stake = self
                        .certs
                        .stake_distribution
                        .stake_map
                        .get(cred_hash)
                        .map(|l| l.0)
                        .unwrap_or(0);
                    let reward_balance = self
                        .certs
                        .reward_accounts
                        .get(cred_hash)
                        .map(|l| l.0)
                        .unwrap_or(0);
                    let total_for_cred = utxo_stake.saturating_add(reward_balance);
                    if total_for_cred > 0 {
                        *pool_stake.entry(*delegated_pool).or_insert(0) += total_for_cred;
                    }
                }
                let total: u64 = pool_stake.values().sum();
                let pool = pool_stake.get(pool_id).copied().unwrap_or(0);
                (pool, total)
            }
        }
    }

    pub fn epoch_nonce_for_slot(&self, slot: u64) -> Hash32 {
        let block_epoch = self.epoch_of_slot(slot);
        if block_epoch <= self.epoch.0 {
            // Same epoch (or behind, should not happen at tip): use current nonce.
            return self.consensus.epoch_nonce;
        }
        if block_epoch == self.epoch.0.saturating_add(1) {
            // Block is in the immediately following epoch.  Pre-compute the TICKN
            // nonce: epochNonce' = candidate ⭒ lastEpochBlockNonce ⭒ extraEntropy.
            // This mirrors process_epoch_transition Step 1 exactly — including
            // the extraEntropy term, which must be FORECAST from the pending PP
            // update that will be enacted at the boundary (mainnet's one-time
            // epoch-259 entropy is set this way). Omitting it would reject every
            // valid first-of-epoch block whose VRF seed uses the new nonce.
            let candidate = self.consensus.candidate_nonce;
            let prev_hash_nonce = self.consensus.last_epoch_block_nonce;
            let extra = self.forecast_extra_entropy_for_epoch(block_epoch);
            return crate::eras::common::combine_nonce(
                crate::eras::common::combine_nonce(candidate, prev_hash_nonce),
                extra,
            );
        }
        // Block is more than one epoch ahead.  We cannot pre-compute the nonce
        // because the intermediate epochs' VRF contributions are unknown.
        // Return the current nonce; validation will fail non-fatally (or produce
        // an informative error), and the node will retry after catching up.
        self.consensus.epoch_nonce
    }

    /// Forecast the decentralisation parameter `d` for `target_epoch` while the
    /// ledger is still in the current epoch.
    ///
    /// Header validation of a block in epoch N+1 must use epoch N+1's `d`
    /// (Haskell TICKF -> UPEC forecast: the LedgerView's `lvD` comes from the
    /// TICKed `curPParams` after the pending protocol-parameter update is
    /// enacted at the epoch boundary), NOT the un-ticked current `d`. The overlay
    /// (OBFT) schedule that decides whether a slot is an overlay/silent/Praos
    /// slot depends on `d`; using the wrong (higher, pre-decrease) `d` mis-counts
    /// overlay slots and rejects the first valid Praos block of the new epoch.
    ///
    /// This applies the SAME enactment as `process_epoch_transition`
    /// (proposals keyed by `target_epoch - 1`, requiring a quorum of
    /// genesis delegates to have voted the byte-identical value — see
    /// [`crate::validation::ppup::voted_future_pparams`], issue #784) but
    /// without mutating state. Only forecasts ONE epoch ahead; for the
    /// same epoch or anything further out it returns the current `d`.
    pub fn forecast_d_for_epoch(
        &self,
        target_epoch: u64,
    ) -> dugite_primitives::transaction::Rational {
        let current_d = self.epochs.protocol_params.d.clone();
        if target_epoch != self.epoch.0.saturating_add(1) {
            return current_d;
        }
        let lookup_epoch = EpochNo(target_epoch.saturating_sub(1));
        let Some(proposals) = self.epochs.pending_pp_updates.get(&lookup_epoch) else {
            return current_d;
        };
        let proposal_map = crate::validation::ppup::fold_pp_proposals(proposals);
        let Some(winner) = crate::validation::ppup::voted_future_pparams(
            &proposal_map,
            self.update_quorum,
            &self.epochs.protocol_params,
        ) else {
            return current_d;
        };
        winner.d.unwrap_or(current_d)
    }

    /// Forecast the active extra entropy (Shelley `ppExtraEntropy`) for
    /// `target_epoch` while the ledger is still in the current epoch.
    ///
    /// Header validation of a block in epoch N+1 derives its VRF seed from
    /// epoch N+1's nonce, which folds in the extraEntropy that the boundary
    /// PPUP will enact. This mirrors `process_epoch_transition`'s enactment
    /// (proposals keyed by `target_epoch - 1`, requiring a quorum of genesis
    /// delegates to have voted the byte-identical value — see
    /// [`crate::validation::ppup::voted_future_pparams`], issue #784, sticky)
    /// without mutating state. Returns the current (sticky) value for the
    /// same epoch, anything further out, or when no proposal changes it.
    pub fn forecast_extra_entropy_for_epoch(&self, target_epoch: u64) -> Hash32 {
        let current = self.consensus.extra_entropy;
        if target_epoch != self.epoch.0.saturating_add(1) {
            return current;
        }
        let lookup_epoch = EpochNo(target_epoch.saturating_sub(1));
        let Some(proposals) = self.epochs.pending_pp_updates.get(&lookup_epoch) else {
            return current;
        };
        let proposal_map = crate::validation::ppup::fold_pp_proposals(proposals);
        let Some(winner) = crate::validation::ppup::voted_future_pparams(
            &proposal_map,
            self.update_quorum,
            &self.epochs.protocol_params,
        ) else {
            return current;
        };
        winner.extra_entropy.unwrap_or(current)
    }

    /// Forecast the active `maxBlockBodySize` for `target_epoch` while the
    /// ledger is still in the current epoch.
    ///
    /// The Praos/TPraos envelope check (`maxBlockBodySize`) for the FIRST block
    /// of epoch N+1 must use epoch N+1's value — the boundary PPUP that raises
    /// it has *conceptually* already been enacted by the TICK that precedes
    /// header/body validation in the Haskell reference (chainChecks runs over
    /// the ticked ledger view). dugite validates the header before the epoch
    /// transition mutates state, so without this forecast it would check the
    /// boundary block against the OLD epoch's limit and wrongly reject it.
    /// Concretely: mainnet raised `maxBlockBodySize` 65536→73728 at the
    /// 305→306 boundary; the first epoch-306 block (6573513) has a 71271-byte
    /// body and is valid only under 73728. Mirrors
    /// [`Self::forecast_d_for_epoch`] (proposals keyed by `target_epoch - 1`,
    /// requiring a quorum of genesis delegates to have voted the
    /// byte-identical value — see
    /// [`crate::validation::ppup::voted_future_pparams`], issue #784),
    /// without mutating state.
    pub fn forecast_max_block_body_size_for_epoch(&self, target_epoch: u64) -> u64 {
        let current = self.epochs.protocol_params.max_block_body_size;
        if target_epoch != self.epoch.0.saturating_add(1) {
            return current;
        }
        let lookup_epoch = EpochNo(target_epoch.saturating_sub(1));
        let Some(proposals) = self.epochs.pending_pp_updates.get(&lookup_epoch) else {
            return current;
        };
        let proposal_map = crate::validation::ppup::fold_pp_proposals(proposals);
        let Some(winner) = crate::validation::ppup::voted_future_pparams(
            &proposal_map,
            self.update_quorum,
            &self.epochs.protocol_params,
        ) else {
            return current;
        };
        winner.max_block_body_size.unwrap_or(current)
    }

    /// Set the Shelley genesis hash.
    ///
    /// Initializes the Praos nonce state machine per Haskell's initialChainDepState
    /// (cardano-protocol-tpraos/API.hs) and translateChainDepStateByronToShelley:
    ///
    ///   evolvingNonce       = initNonce  (= Blake2b_256 of genesis file)
    ///   candidateNonce      = initNonce
    ///   epochNonce          = initNonce
    ///   labNonce            = NeutralNonce
    ///   lastEpochBlockNonce = NeutralNonce
    ///
    /// At the first epoch boundary, the Nonce combine with NeutralNonce is identity:
    ///   epochNonce' = candidateNonce ⭒ NeutralNonce = candidateNonce
    /// This means the first epoch transition preserves the candidate nonce directly
    /// rather than hashing it with a non-zero lastEpochBlockNonce.
    pub fn set_genesis_hash(&mut self, hash: Hash32) {
        self.genesis_hash = hash;
        // evolving/candidate/epoch all start from the genesis file hash
        self.consensus.evolving_nonce = hash;
        self.consensus.candidate_nonce = hash;
        self.consensus.epoch_nonce = hash;
        // lab and lastEpochBlockNonce start as NeutralNonce (ZERO)
        // This is critical: at the first epoch boundary, NeutralNonce identity
        // means epochNonce = candidateNonce (not hash(candidate || genesisHash))
        self.consensus.lab_nonce = Hash32::ZERO;
        self.consensus.last_epoch_block_nonce = Hash32::ZERO;
        info!(
            epoch_nonce = %hash.to_hex(),
            evolving = %hash.to_hex(),
            candidate = %hash.to_hex(),
            lab = "NeutralNonce (ZERO)",
            last_epoch_block = "NeutralNonce (ZERO)",
            "Ledger: Praos nonce state initialized from genesis hash"
        );
    }

    /// Set the update quorum threshold (from Shelley genesis)
    pub fn set_update_quorum(&mut self, quorum: u64) {
        self.update_quorum = quorum;
        debug!("Ledger: update quorum={quorum}");
    }

    /// Load genesis delegates from Shelley genesis data.
    ///
    /// Each entry is (genesis_key_hash_28, delegate_key_hash_28, vrf_key_hash_32)
    /// as raw bytes. Called during node initialization from `ShelleyGenesis::gen_delegs_entries()`.
    pub fn set_genesis_delegates(&mut self, entries: &[(Vec<u8>, Vec<u8>, Vec<u8>)]) {
        self.genesis_delegates.clear();
        for (genesis_hash, delegate_hash, vrf_hash) in entries {
            if genesis_hash.len() == 28 && delegate_hash.len() == 28 && vrf_hash.len() == 32 {
                let gkey = Hash28::from_bytes({
                    let mut buf = [0u8; 28];
                    buf.copy_from_slice(genesis_hash);
                    buf
                });
                let dkey = Hash28::from_bytes({
                    let mut buf = [0u8; 28];
                    buf.copy_from_slice(delegate_hash);
                    buf
                });
                let vrf = Hash32::from_bytes({
                    let mut buf = [0u8; 32];
                    buf.copy_from_slice(vrf_hash);
                    buf
                });
                self.genesis_delegates.insert(gkey, (dkey, vrf));
            }
        }
    }

    /// Seed the UTxO set with genesis UTxOs (from Byron genesis nonAvvmBalances).
    ///
    /// Each genesis UTxO is assigned a deterministic transaction hash derived from
    /// blake2b-256 of the address bytes, with sequential output indices.
    /// This MUST be called before replaying blocks from genesis.
    pub fn seed_genesis_utxos(&mut self, entries: &[(Vec<u8>, u64)]) {
        let mut seeded = 0u64;
        let mut total_lovelace = 0u64;

        for (address, lovelace) in entries {
            // No zero-value skip. This was the SECOND copy of that filter — the
            // genesis parser had one too — and removing only the parser's left
            // the count unchanged, which is how a duplicated guard hides a fix.
            //
            // cardano-ledger keeps zero-value genesis entries:
            // `genesisUtxo = fromBalances (avvmBalances <> nonAvvmBalances)` is a
            // plain `M.toList` with no filter, `mkLovelace` bounds only from
            // above, and `fromTxOut` keys each TxIn on the output ADDRESS
            // (`Cardano/Chain/UTxO/GenesisUTxO.hs`, `UTxO.hs`,
            // `Common/Lovelace.hs`). Shelley's `genesisUTxO` is the same shape.
            //
            // Measured on preprod, whose Byron genesis has 8 `nonAvvmBalances`
            // entries of which SEVEN are zero: cardano-node reports UTxO count 8
            // at Byron epochs 1-3, dugite reported 1, and the BALANCES agreed to
            // the lovelace — which is why every check that sums was blind.
            //
            // False reject on block validity: a transaction spending one of
            // those outputs is accepted by cardano-node and was rejected here
            // with `InputNotFound`.

            // Derive a deterministic tx hash from the address (matches Byron genesis UTxO format)
            let tx_hash = dugite_primitives::hash::blake2b_256(address);

            let input = dugite_primitives::transaction::TransactionInput {
                transaction_id: tx_hash,
                index: 0,
            };

            // Parse the address bytes, or fall back to Byron if parsing fails
            let addr = dugite_primitives::Address::from_bytes(address).unwrap_or(
                dugite_primitives::Address::Byron(dugite_primitives::address::ByronAddress {
                    payload: address.clone(),
                }),
            );

            let output = dugite_primitives::transaction::TransactionOutput {
                address: addr,
                value: dugite_primitives::value::Value {
                    coin: Lovelace(*lovelace),
                    multi_asset: std::collections::BTreeMap::new(),
                },
                datum: dugite_primitives::transaction::OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            };

            self.utxo.utxo_set.insert(input, output);
            seeded += 1;
            total_lovelace += lovelace;
        }

        // Deduct seeded lovelace from reserves per Shelley spec:
        // reserves = maxLovelaceSupply - totalBalance(initialUTxO)
        // Without this, monetary expansion (rho * reserves) is computed on too
        // large a reserves value, draining reserves too fast and overfilling
        // the treasury.
        self.epochs.reserves.0 = self
            .epochs
            .reserves
            .0
            .checked_sub(total_lovelace)
            .expect("genesis UTxO total exceeds maxLovelaceSupply — invariant broken");

        debug!(
            "Ledger: seeded {} genesis UTxOs ({} lovelace, reserves now {})",
            seeded, total_lovelace, self.epochs.reserves.0
        );
    }

    /// Seed a genesis pool registration into the ledger state.
    ///
    /// Inserts the pool registration into `pool_params` and registers
    /// the reward account with zero balance.
    pub fn seed_genesis_pool(&mut self, registration: PoolRegistration) {
        let pool_id = registration.pool_id;
        let reward_account = registration.reward_account.clone();

        let pool_params = Arc::make_mut(&mut self.certs.pool_params);
        pool_params.insert(pool_id, registration);

        // Register reward account with zero balance if not already present
        if reward_account.len() >= 29 {
            // Extract the 28-byte credential hash from the reward address
            // (byte 0 is the header, bytes 1-28 are the credential)
            let mut cred = [0u8; 32];
            cred[..28].copy_from_slice(&reward_account[1..29]);
            let cred_hash = Hash32::from_bytes(cred);
            self.certs
                .reward_accounts
                .entry(cred_hash)
                .or_insert(Lovelace(0));
        }

        debug!("Ledger: seeded genesis pool {}", pool_id.to_hex());
    }

    /// Seed a genesis stake delegation into the ledger state.
    ///
    /// Maps a stake credential (as padded Hash32) to a pool ID (Hash28).
    /// Registers the credential in reward accounts with zero balance.
    pub fn seed_genesis_delegation(&mut self, stake_credential: Hash32, pool_id: Hash28) {
        self.certs.delegations.insert(stake_credential, pool_id);
        // Register stake credential in reward accounts if not present.
        self.certs
            .reward_accounts
            .entry(stake_credential)
            .or_insert(Lovelace(0));
    }

    /// Finalize genesis state for cold-start block production.
    ///
    /// Mirrors Haskell's `resetStakeDistribution` (cardano-ledger
    /// `Shelley/Transition.hs`): after `seed_genesis_utxos`, `seed_genesis_pool`,
    /// and `seed_genesis_delegation` have been called, builds the initial
    /// stake/pool distribution and pre-populates the `mark` and `set`
    /// snapshots so that Praos leader election works from slot 0.
    ///
    /// In Haskell, leader election reads `nesPd` (active pool distribution),
    /// which `resetStakeDistribution` fills with the post-genesis pool stake.
    /// Dugite's forge path uses `snapshots.set` for the same purpose, so we
    /// populate both `mark` and `set` with the same genesis-derived data —
    /// the first SNAP rotation at epoch 0→1 preserves the same pool stake
    /// into `go`, matching Haskell's observable behaviour on a quiet devnet.
    ///
    /// **Conway-from-genesis caveat**: on chains that boot directly in
    /// Conway (e.g. the local devnet at PV10+), this pre-fill is wrong: the
    /// genesis snapshot rotates into `ssStakeGo` at boundary 0→1 and causes
    /// the RUPD per-pool distribution at boundary 1→2 to mis-fire. The
    /// genesis bootstrap in `dugite-node::main` therefore clears these
    /// snapshots back to `None` for Conway-from-genesis chains, after which
    /// the forge falls back to `pool_distribution_for_slot`'s live-state
    /// branch.
    ///
    /// No-op on Mithril-restored state, where snapshots are already loaded
    /// from the Haskell snapshot file.
    pub fn finalize_genesis_state(&mut self) {
        use tracing::info;

        // If a snapshot is already present (Mithril restore path), do nothing.
        if self.epochs.snapshots.set.is_some() || self.epochs.snapshots.mark.is_some() {
            return;
        }

        // Build stake_map from seeded UTxOs so pool_stake can be computed.
        self.rebuild_stake_distribution();

        // Build pool_stake and snapshot_stake exactly as the SNAP rule does
        // for the mark snapshot at an epoch boundary.
        let mut pool_stake: HashMap<Hash28, Lovelace> =
            HashMap::with_capacity(self.certs.pool_params.len());
        let mut snapshot_stake: HashMap<Hash32, Lovelace> =
            HashMap::with_capacity(self.certs.delegations.len());
        for (cred_hash, pool_id) in self.certs.delegations.iter() {
            let utxo_stake = self
                .certs
                .stake_distribution
                .stake_map
                .get(cred_hash)
                .copied()
                .unwrap_or(Lovelace(0));
            let reward_balance = self
                .certs
                .reward_accounts
                .get(cred_hash)
                .copied()
                .unwrap_or(Lovelace(0));
            let total = Lovelace(utxo_stake.0.saturating_add(reward_balance.0));
            if total.0 > 0 {
                snapshot_stake.insert(*cred_hash, total);
                *pool_stake.entry(*pool_id).or_insert(Lovelace(0)) += total;
            }
        }

        if pool_stake.is_empty() {
            // No pools registered or no delegated stake yet — nothing to snapshot.
            return;
        }

        let total_pool_stake: u64 = pool_stake
            .values()
            .fold(0u64, |acc, l| acc.saturating_add(l.0));
        info!(
            pools = pool_stake.len(),
            delegations = self.certs.delegations.len(),
            total_pool_stake_ada = total_pool_stake / 1_000_000,
            "Genesis: seeded initial stake/pool snapshot for cold-start leader election"
        );

        // Convert imbl::HashMap delegations → Arc<std::HashMap> for StakeSnapshot.
        let genesis_delegations = Arc::new(
            self.certs
                .delegations
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect::<HashMap<_, _>>(),
        );
        let snap = StakeSnapshot {
            epoch: self.epoch,
            delegations: genesis_delegations,
            pool_stake,
            pool_params: Arc::clone(&self.certs.pool_params),
            stake_distribution: Arc::new(snapshot_stake),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        };

        self.epochs.snapshots.mark = Some(snap.clone());
        self.epochs.snapshots.set = Some(snap);
    }

    /// Advance the ledger tip through a Byron Epoch Boundary Block (EBB).
    ///
    /// EBBs carry no transactions and do not mutate the UTxO set, stake
    /// distribution, or any other ledger data.  They exist solely so that
    /// the next real Byron block can reference them via `prev_hash`, forming
    /// an unbroken hash chain across epoch boundaries.
    ///
    /// This method advances `self.tip` so that the EBB hash becomes the
    /// current tip hash, allowing the subsequent block's `prev_hash` check
    /// to pass.  The slot is preserved from the previous real block because
    /// EBBs do not occupy slots — this prevents incorrect "block already
    /// applied" skips in the sync loop which compares `block.slot <= ledger_slot`.
    ///
    /// # Errors
    /// Returns `LedgerError::EpochTransition` if called outside the Byron era,
    /// since EBBs do not exist in Shelley or later eras.
    pub fn advance_past_ebb(&mut self, ebb_hash: Hash32) -> Result<(), LedgerError> {
        use dugite_primitives::era::Era;

        // EBBs only exist in the Byron era.  Calling this in Shelley+ is a programming error.
        if self.era != Era::Byron {
            return Err(LedgerError::EpochTransition(format!(
                "EBB advance called in non-Byron era {:?}; EBBs do not exist after Byron",
                self.era
            )));
        }

        // Preserve the slot of the current tip.  The EBB has no slot of its
        // own; by keeping the previous slot we ensure the next real block's slot
        // satisfies `slot > ledger_slot` so it is not incorrectly skipped.
        let preserved_slot = self.tip.point.slot().unwrap_or(SlotNo(0));

        trace!(
            ebb_hash = %ebb_hash.to_hex(),
            preserved_slot = preserved_slot.0,
            current_tip = %self.tip.point,
            "Ledger: advancing tip through EBB"
        );

        // Advance the tip hash to the EBB hash while keeping the slot from the
        // previous block.  Block number is also preserved since EBBs do not
        // increment the block counter.
        self.tip = Tip {
            point: Point::Specific(preserved_slot, ebb_hash),
            block_number: self.tip.block_number,
        };

        Ok(())
    }

    pub fn current_slot(&self) -> Option<SlotNo> {
        self.tip.point.slot()
    }

    pub fn current_block_number(&self) -> BlockNo {
        self.tip.block_number
    }

    /// Roll back the entire ledger state to a chain point using the
    /// in-memory [`LedgerSeq`] as the source of truth for non-UTxO state.
    ///
    /// This is the primary rollback path for in-volatile-window targets
    /// (Subsystem 4 — supersedes the snapshot-reload-and-replay approach
    /// for any target whose hash matches a delta in the seq's volatile
    /// window).  It is O(n) where `n` is the rollback distance — never
    /// triggers a full ledger replay.
    ///
    /// # Steps
    ///
    /// 1. Compute `n` from the target point via [`LedgerSeq::find_rollback_n`].
    ///    Returns `None` if the point is outside the volatile window — the
    ///    caller must fall back to snapshot-driven recovery.
    /// 2. Reverse-apply the trailing `n` UTxO diffs to the live UTxO set
    ///    (both the in-memory map and the LSM-backed store stay in sync
    ///    via the `UtxoSet::insert/remove` write-through).
    /// 3. Truncate the live `DiffSeq` by the same `n` entries.
    /// 4. Roll back the [`LedgerSeq`] (drops trailing deltas + invalidates
    ///    checkpoints that pointed into the removed range).
    /// 5. Replace every non-UTxO field on `self` with the seq's
    ///    reconstructed tip state — including `epoch_fees` / `pending_donations`
    ///    (the two UTxO-adjacent scalars the seq tracks via `BlockFieldsDelta`,
    ///    since #782) and `genesis_delegates` (a top-level field the seq
    ///    tracks via a dedicated delta snapshot, also since #782 — it is not
    ///    covered by the wholesale `certs`/`gov`/`consensus`/`epochs`
    ///    sub-state copies below because it lives directly on `LedgerState`).
    ///
    /// The LSM UTxO store stays attached the whole time: the `UtxoSet`
    /// already write-throughs to the LSM store on `insert`/`remove`, so
    /// the reverse-apply at step 2 leaves the on-disk store in the
    /// rolled-back state without a detach/re-attach dance (which would
    /// risk re-migrating reconstruction-window UTxOs over correct LSM
    /// entries via [`Self::attach_utxo_store`]).
    ///
    /// Static configuration fields (`epoch_length`, `slot_config`,
    /// `byron_epoch_length`, etc.) are unchanged by rollback and are
    /// left untouched on `self`.
    ///
    /// # Returns
    ///
    /// The number of blocks rolled back (`Some(n)`), or `None` if the
    /// target was outside the volatile window.
    ///
    /// Matches Haskell's `LedgerDB.V2.LedgerSeq.rollbackToPoint` followed
    /// by a single in-memory commit.
    pub fn rollback_via_seq(
        &mut self,
        seq: &mut crate::ledger_seq::LedgerSeq,
        target_point: &Point,
    ) -> Option<usize> {
        let n = seq.find_rollback_n(target_point)?;
        if n == 0 {
            return Some(0);
        }

        // #806 DEFECT A: `save_ledger_snapshot` clears `utxo.diff_seq` (to
        // reclaim memory — diffs are `#[serde(skip)]`, not persisted) but
        // does NOT clear `seq` (the `LedgerSeq`). If a rollback target lands
        // further back than the diffs retained since the last snapshot,
        // `diff_seq.len() < n` and reverse-applying only `diff_seq.len()`
        // UTxO diffs while restoring non-UTxO state `n` blocks back would
        // silently desync the UTxO set from the rest of ledger state. Must
        // be checked BEFORE any mutation below (`diff_seq.rollback` /
        // `seq.rollback` have not run yet at this point), so returning
        // `None` here is a clean bail-out: the caller's existing snapshot
        // reload fallback recovers safely.
        if n > self.utxo.diff_seq.len() {
            tracing::warn!(
                n,
                have = self.utxo.diff_seq.len(),
                "LedgerSeq/DiffSeq desync: DiffSeq too short to cover rollback; falling back to snapshot recovery"
            );
            return None;
        }

        // Step 1+2: pop the trailing n UTxO diffs and invert each one on the
        // live store.  `DiffSeq::rollback` removes them from the seq; the
        // returned `Vec` is consumed here for the reverse-apply.
        let diffs = self.utxo.diff_seq.rollback(n);
        for (_slot, _hash, diff) in &diffs {
            // Invert `apply` exactly. Forward apply is inserts-then-deletes
            // (`ledger_seq.rs` flattened path), so the inverse must re-insert
            // `deletes` FIRST, then remove `inserts`. Within one block's merged
            // diff, an output created by tx_i and spent by tx_j (j>i) appears in
            // BOTH `inserts` and `deletes` (`UtxoDiff::merge` appends, no
            // cancellation). Removing inserts first then re-inserting deletes
            // would materialize that phantom UTxO (a spent-in-block output). The
            // opposite overlap (TxIn deleted then re-created in one block) is
            // impossible — recreating `(txhash, ix)` needs a duplicate tx hash —
            // so this ordering is the exact inverse in all cases. (#781)
            for (input, output) in &diff.deletes {
                self.utxo.utxo_set.insert(input.clone(), output.clone());
            }
            for (input, _output) in &diff.inserts {
                self.utxo.utxo_set.remove(input);
            }
        }

        // Step 3: roll back the seq.
        seq.rollback(n);

        // Step 4: replace non-UTxO state from the seq's reconstructed tip.
        let new_state = seq.tip_state();
        self.certs = new_state.certs;
        self.gov = new_state.gov;
        self.consensus = new_state.consensus;
        self.epochs = new_state.epochs;
        self.tip = new_state.tip;
        self.era = new_state.era;
        self.epoch = new_state.epoch;
        self.pending_era_transition = new_state.pending_era_transition;
        self.utxo.epoch_fees = new_state.utxo.epoch_fees;
        self.utxo.pending_donations = new_state.utxo.pending_donations;
        // #782: top-level field, not covered by the sub-state copies above.
        self.genesis_delegates = new_state.genesis_delegates;
        // #804: same rationale — `future_gen_delegs` lives directly on
        // `LedgerState`, tracked via its own delta snapshot.
        self.future_gen_delegs = new_state.future_gen_delegs;

        Some(n)
    }
}

// ── Haskell snapshot conversion helpers ─────────��────────────────────────────

/// Convert a Haskell credential `(tag, Hash28)` to dugite's `Hash32` key format.
///
/// Matches `Credential::to_typed_hash32()`: the 28-byte hash occupies bytes [0..28],
/// byte 28 is `0x01` for script credentials (tag=1), `0x00` for key credentials (tag=0).
fn haskell_credential_to_hash32(tag: u8, hash: &Hash28) -> Hash32 {
    let mut bytes = [0u8; 32];
    bytes[..28].copy_from_slice(hash.as_bytes());
    if tag == 1 {
        bytes[28] = 0x01;
    }
    Hash32::from_bytes(bytes)
}

/// Convert a Haskell `HaskellStakePoolState` to dugite's `PoolRegistration`.
fn convert_pool_registration(
    pool_id: Hash28,
    pool: &dugite_serialization::haskell_snapshot::types::HaskellStakePoolState,
) -> PoolRegistration {
    use dugite_primitives::transaction::Relay;
    use dugite_serialization::haskell_snapshot::types::HaskellRelay;

    let relays: Vec<Relay> = pool
        .relays
        .iter()
        .map(|r| match r {
            HaskellRelay::SingleHostAddr(port, ipv4, ipv6) => Relay::SingleHostAddr {
                port: *port,
                ipv4: *ipv4,
                ipv6: *ipv6,
            },
            HaskellRelay::SingleHostName(port, dns) => Relay::SingleHostName {
                port: *port,
                dns_name: dns.clone(),
            },
            HaskellRelay::MultiHostName(dns) => Relay::MultiHostName {
                dns_name: dns.clone(),
            },
        })
        .collect();

    let (metadata_url, metadata_hash) = match &pool.metadata {
        Some((url, hash)) => (Some(url.clone()), Some(*hash)),
        None => (None, None),
    };

    PoolRegistration {
        pool_id,
        vrf_keyhash: pool.vrf_hash,
        pledge: Lovelace(pool.pledge),
        cost: Lovelace(pool.cost),
        margin_numerator: pool.margin_num,
        margin_denominator: pool.margin_den,
        reward_account: pool.reward_account.clone(),
        owners: pool.owners.clone(),
        relays,
        metadata_url,
        metadata_hash,
    }
}

/// Convert a Haskell `HaskellSnapShot` to dugite's `StakeSnapshot`.
///
/// `live_pool_params` is the live `psStakePools` registry, used to backfill
/// owners / relays / metadata that the cardano-node ≥ 11.0.1
/// `StakePoolSnapShot` wire format omits.  Per Haskell architecture the
/// snapshot's `_poolParams` carries the FULL `PoolParams` (owners included);
/// the wire encoding drops fields that are identical to `psStakePools` at
/// capture time, on the understanding that they are restored on load.  Issue
/// #668 — without this restore the pledge check at
/// `compute_reward_update` fails for every pool and ~99.9% of rewards are
/// silently routed to reserves/treasury instead of credited.
fn convert_stake_snapshot(
    snap: &dugite_serialization::haskell_snapshot::types::HaskellSnapShot,
    epoch: EpochNo,
    live_pool_params: &HashMap<Hash28, PoolRegistration>,
) -> StakeSnapshot {
    let mut delegations = HashMap::new();
    let mut stake_distribution = HashMap::new();
    let mut pool_stake: HashMap<Hash28, Lovelace> = HashMap::new();

    // Convert delegations and per-credential stake
    for ((tag, hash28), pool_id) in &snap.delegations {
        let cred_hash = haskell_credential_to_hash32(*tag, hash28);
        delegations.insert(cred_hash, *pool_id);
    }
    for ((tag, hash28), lovelace) in &snap.stake {
        let cred_hash = haskell_credential_to_hash32(*tag, hash28);
        stake_distribution.insert(cred_hash, Lovelace(*lovelace));

        // Accumulate pool stake from delegations
        if let Some(pool_id) = delegations.get(&cred_hash) {
            *pool_stake.entry(*pool_id).or_insert(Lovelace(0)) += Lovelace(*lovelace);
        }
    }

    // Convert pool params within the snapshot, restoring owners / relays /
    // metadata from `live_pool_params` when the wire encoding omitted them.
    let mut snapshot_pool_params = HashMap::new();
    for (pool_id, pool) in &snap.pool_params {
        let mut reg = convert_snapshot_pool_registration(*pool_id, pool);
        if reg.owners.is_empty() {
            if let Some(live) = live_pool_params.get(pool_id) {
                reg.owners = live.owners.clone();
                if reg.relays.is_empty() {
                    reg.relays = live.relays.clone();
                }
                if reg.metadata_url.is_none() {
                    reg.metadata_url = live.metadata_url.clone();
                    reg.metadata_hash = live.metadata_hash;
                }
            }
        }
        snapshot_pool_params.insert(*pool_id, reg);
    }

    StakeSnapshot {
        epoch,
        delegations: Arc::new(delegations),
        pool_stake,
        pool_params: Arc::new(snapshot_pool_params),
        stake_distribution: Arc::new(stake_distribution),
        epoch_fees: Lovelace(0), // Not stored per-snapshot in Haskell
        epoch_block_count: 0,    // Not stored per-snapshot in Haskell
        epoch_blocks_by_pool: Arc::new(HashMap::new()),
    }
}

/// Convert a Haskell `HaskellSnapShotPool` to dugite's `PoolRegistration`.
fn convert_snapshot_pool_registration(
    pool_id: Hash28,
    pool: &dugite_serialization::haskell_snapshot::types::HaskellSnapShotPool,
) -> PoolRegistration {
    use dugite_primitives::transaction::Relay;
    use dugite_serialization::haskell_snapshot::types::HaskellRelay;

    let relays: Vec<Relay> = pool
        .relays
        .iter()
        .map(|r| match r {
            HaskellRelay::SingleHostAddr(port, ipv4, ipv6) => Relay::SingleHostAddr {
                port: *port,
                ipv4: *ipv4,
                ipv6: *ipv6,
            },
            HaskellRelay::SingleHostName(port, dns) => Relay::SingleHostName {
                port: *port,
                dns_name: dns.clone(),
            },
            HaskellRelay::MultiHostName(dns) => Relay::MultiHostName {
                dns_name: dns.clone(),
            },
        })
        .collect();

    let (metadata_url, metadata_hash) = match &pool.metadata {
        Some((url, hash)) => (Some(url.clone()), Some(*hash)),
        None => (None, None),
    };

    PoolRegistration {
        pool_id,
        vrf_keyhash: pool.vrf_hash,
        pledge: Lovelace(pool.pledge),
        cost: Lovelace(pool.cost),
        margin_numerator: pool.margin_num,
        margin_denominator: pool.margin_den,
        reward_account: pool.reward_account.clone(),
        owners: pool.owners.clone(),
        relays,
        metadata_url,
        metadata_hash,
    }
}

/// Convert a Haskell `HaskellDRep` to dugite's native `DRep`.
fn convert_haskell_drep(drep: &dugite_serialization::haskell_snapshot::types::HaskellDRep) -> DRep {
    use dugite_serialization::haskell_snapshot::types::HaskellDRep;
    match drep {
        HaskellDRep::KeyHash(h) => DRep::KeyHash(h.to_hash32_padded()),
        HaskellDRep::ScriptHash(h) => DRep::ScriptHash(*h),
        HaskellDRep::AlwaysAbstain => DRep::Abstain,
        HaskellDRep::AlwaysNoConfidence => DRep::NoConfidence,
    }
}

/// Extract a Hash32 from a Credential for use as a map key.
///
/// Uses `to_typed_hash32()` which encodes the credential TYPE (key vs script)
/// in byte 28 of the padding. This ensures key and script credentials with
/// the same 28-byte hash are stored as separate entries, matching Haskell's
/// `KeyHashObj` / `ScriptHashObj` distinction.
fn credential_to_hash(credential: &Credential) -> Hash32 {
    credential.to_typed_hash32()
}

/// Extract the staking credential hash from an address.
///
/// Handles Base addresses (embedded credential), Reward addresses, and
/// Pointer addresses (resolved via the pointer_map, matching Haskell's
/// DState ptrs). Returns None for Enterprise and Byron addresses.
///
/// In Conway (protocol version >= 9), pointer addresses are excluded from the
/// stake distribution — Haskell's `ConwayInstantStake` has no `sisPtrStake`
/// field and `addConwayInstantStake` returns `ans` unchanged for pointer
/// addresses.  When `exclude_ptrs` is true, pointer addresses return `None`.
/// The stake routing outcome for a UTxO output address.
///
/// Haskell's `ShelleyInstantStake` tracks pointer-addressed UTxO coins separately
/// in `sisPtrStake` and defers their resolution to SNAP time.  Base/Reward addresses
/// go directly into `sisCredentialStake` (our `stake_map`).  In Conway,
/// `ConwayInstantStake` omits pointer stake entirely.
pub(crate) enum StakeRouting {
    /// Credential hash — route coins to `stake_distribution.stake_map`.
    Credential(Hash32),
    /// Pointer key — route coins to `ptr_stake` (deferred resolution at SNAP time).
    Pointer(dugite_primitives::credentials::Pointer),
    /// No stake routing (Enterprise / Byron / unknown).
    None,
}

/// Classify a UTxO address into its stake-routing bucket.
///
/// * Base / Reward  → `StakeRouting::Credential` (eager resolution)
/// * Pointer        → `StakeRouting::Pointer` (deferred — key stored in `ptr_stake`)
/// * Everything else → `StakeRouting::None`
///
/// When `exclude_ptrs` is true (Conway era), pointer addresses return
/// `StakeRouting::None` — they are silently excluded as in `ConwayInstantStake`.
pub(crate) fn stake_routing(
    address: &dugite_primitives::address::Address,
    exclude_ptrs: bool,
) -> StakeRouting {
    use dugite_primitives::address::Address;
    match address {
        Address::Base(base) => StakeRouting::Credential(credential_to_hash(&base.stake)),
        Address::Reward(reward) => StakeRouting::Credential(credential_to_hash(&reward.stake)),
        Address::Pointer(ptr_addr) => {
            if exclude_ptrs {
                StakeRouting::None
            } else {
                StakeRouting::Pointer(ptr_addr.pointer)
            }
        }
        _ => StakeRouting::None,
    }
}

/// Legacy: Extract staking credential hash without pointer resolution.
/// Used in contexts where the pointer_map isn't available.
#[cfg(test)]
fn stake_credential_hash(address: &dugite_primitives::address::Address) -> Option<Hash32> {
    use dugite_primitives::address::Address;
    match address {
        Address::Base(base) => Some(credential_to_hash(&base.stake)),
        Address::Reward(reward) => Some(credential_to_hash(&reward.stake)),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("Block does not connect to tip: expected {expected}, got {got}")]
    BlockDoesNotConnect { expected: String, got: String },
    #[error("UTxO error: {0}")]
    UtxoError(String),
    #[error("Invalid transaction: {0}")]
    InvalidTransaction(String),
    #[error("Epoch transition error: {0}")]
    EpochTransition(String),
    #[error("Invalid protocol parameter: {0}")]
    InvalidProtocolParam(String),
    #[error("Validation tag mismatch for tx {tx_hash}: block flag is_valid={block_flag} but evaluation result is_valid={eval_result}")]
    ValidationTagMismatch {
        tx_hash: String,
        block_flag: bool,
        eval_result: bool,
    },
    #[error("Transaction validation failed at slot {slot} tx {tx_hash}: {errors}")]
    BlockTxValidationFailed {
        slot: u64,
        tx_hash: String,
        errors: String,
    },
    #[error("Block body size mismatch: actual serialized size {actual} != header claimed size {claimed} (WrongBlockBodySizeBBODY)")]
    WrongBlockBodySize { actual: u64, claimed: u64 },
    /// Phase-2 collection/context error on a block transaction — Haskell
    /// `UtxosFailure (CollectErrors …)` (Babbage+), raised regardless of
    /// the `is_valid` tag. Block-fatal at apply (#733): a block containing
    /// such a tx is invalid on every honest Haskell node.
    #[error(
        "Phase-2 collection error at slot {slot} tx {tx_hash}: {error} \
         (UtxosFailure CollectErrors — block invalid regardless of is_valid)"
    )]
    Phase2CollectErrors {
        slot: u64,
        tx_hash: String,
        error: String,
    },
}

#[cfg(test)]
mod tests;
