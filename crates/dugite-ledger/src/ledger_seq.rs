//! LedgerSeq: Haskell-compatible anchored sequence of ledger state deltas.
//!
//! Matches Haskell's `LedgerDB.V2.LedgerSeq` — a single full anchor state at the
//! immutable tip plus a window of per-block deltas covering the volatile chain.
//! This enables O(1) rollback and O(checkpoint_interval) state reconstruction.
//!
//! # Architecture
//!
//! The design follows Haskell's V2 DiffTables approach because full `LedgerState`
//! clones are prohibitively large (~40–80 MB each for epoch snapshots alone).
//! Storing k full copies would require ~17–34 GB on preview and ~86–173 GB on
//! mainnet, which is infeasible.
//!
//! Instead:
//! - **Anchor**: One full `LedgerState` at the immutable tip.  Saved to disk on
//!   snapshot.  Rebuilt from the latest on-disk snapshot + ImmutableDB replay on
//!   restart.
//! - **Volatile deltas**: Per-block `LedgerDelta` recording ALL state changes,
//!   not just UTxO (also delegations, rewards, pools, governance, nonces, epoch
//!   transitions, protocol parameters).
//! - **Checkpoints**: Full `LedgerState` snapshots stored in memory every
//!   `checkpoint_interval` blocks (default 100).  Limits reconstruction cost to
//!   at most `checkpoint_interval` delta applications.
//!
//! # Memory budget (preview testnet, k=2160)
//!
//! | Component          | Count | Per-item size | Total     |
//! |--------------------|-------|---------------|-----------|
//! | Anchor             | 1     | ~80 MB        | ~80 MB    |
//! | Deltas             | 2160  | ~5–50 KB      | ~2–20 MB  |
//! | Checkpoints (k/100)| ~22   | ~80 MB        | ~1.76 GB  |
//!
//! Checkpoints dominate.  If memory pressure is a concern the checkpoint interval
//! can be increased (e.g. 500) at the cost of slower state reconstruction.
//!
//! # Rollback
//!
//! `rollback(n)` is O(1): it drops the trailing n deltas from the VecDeque and
//! removes any checkpoints that no longer have backing deltas.  The reconstructed
//! state for the new tip is obtained via `tip_state()`.
//!
//! # Haskell reference
//!
//! `ouroboros-consensus:LedgerDB/V2/LedgerSeq.hs` — `LedgerSeq`, `prune`,
//! `rollbackN`, `extend`.

use crate::state::{
    DRepRegistration, EpochSnapshots, GovSubState, LedgerState, PoolRegistration, ProposalState,
    StakeDistributionState,
};
use crate::utxo_diff::UtxoDiff;
use dugite_primitives::block::Point;
use dugite_primitives::credentials::Pointer;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{BlockNo, EpochNo, SlotNo};
use dugite_primitives::transaction::{
    Anchor, Constitution, DRep, GovActionId, ProtocolParamUpdate, Rational, Voter, VotingProcedure,
};
use dugite_primitives::value::Lovelace;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

// ─────────────────────────────────────────────────────────────────────────────
// LedgerDelta: all state changes produced by a single block
// ─────────────────────────────────────────────────────────────────────────────

/// All state changes produced by applying a single block to the ledger.
///
/// This must capture EVERY mutable field in `LedgerState` so that any
/// historical state within the volatile window can be exactly reconstructed
/// by replaying deltas forward from the nearest checkpoint.
///
/// `LedgerDelta` is intentionally flat — there is no nesting of "apply this
/// sub-delta then that one".  Each variant in each sub-list is self-contained.
///
/// # Forward-only semantics
///
/// Deltas are applied in the FORWARD direction (oldest → newest) during
/// reconstruction.  They are NOT unapplied during rollback — rollback simply
/// discards the trailing deltas and reconstructs the tip from scratch.  This
/// design avoids the complexity and subtle bugs of "inverse diff" logic.
#[derive(Debug, Clone)]
pub struct LedgerDelta {
    /// Block slot number.
    pub slot: SlotNo,
    /// Block header hash.
    pub hash: Hash32,
    /// Block number.
    pub block_no: BlockNo,

    /// UTxO changes: inserts (new outputs) and deletes (consumed outputs).
    pub utxo_diff: UtxoDiff,

    /// Delegation state changes produced by certificates in this block.
    pub delegation_changes: Vec<DelegationChange>,

    /// Pool state changes produced by certificates in this block.
    pub pool_changes: Vec<PoolChange>,

    /// Reward account state changes.
    pub reward_changes: Vec<RewardChange>,

    /// Governance state changes (DRep, vote delegation, proposals, votes,
    /// committee, ratification).
    pub governance_changes: Vec<GovernanceChange>,

    /// Epoch transition data if this block crossed an epoch boundary.
    /// Contains the full set of scalar field changes made during the
    /// transition (treasury, reserves, snapshots, protocol params, etc.).
    pub epoch_transition: Option<EpochTransitionDelta>,

    /// Scalar nonce / block production field updates for this block.
    pub block_fields: BlockFieldsDelta,

    /// Post-block snapshot of the directly-mutated `imbl` cert maps
    /// (`reward_accounts`, `delegations`, `stake_key_deposits`).
    ///
    /// These fields are mutated in place by `apply_block` (drain on withdrawal,
    /// epoch-boundary reward credit, registration/delegation certs) and were
    /// NOT captured by the `*_changes` delta vecs (which are never populated).
    /// Without a snapshot, `apply_delta_to_state` left them at the stale anchor
    /// value, so `rollback_via_seq` corrupted reward balances on a fork
    /// (preprod ep292 `WithdrawalAmountMismatch` halt). `imbl::HashMap` clones
    /// are O(1) (structural sharing), so snapshotting per block is cheap.
    ///
    /// `None` on deltas not produced by `apply_block_with_delta` (e.g. test
    /// fixtures) — `apply_delta_to_state` then leaves the fields untouched,
    /// preserving the previous behaviour for those paths.
    pub reward_accounts_snapshot: Option<imbl::HashMap<Hash32, Lovelace>>,
    /// See [`Self::reward_accounts_snapshot`].
    pub delegations_snapshot: Option<imbl::HashMap<Hash32, Hash28>>,
    /// See [`Self::reward_accounts_snapshot`].
    pub stake_key_deposits_snapshot: Option<imbl::HashMap<Hash32, u64>>,

    /// Post-block snapshot of pool-related cert state (`pool_params`,
    /// `future_pool_params`, `pending_retirements`, `pool_deposits`).
    ///
    /// These fields are mutated in place by `apply_block` when the block
    /// contains `PoolRegistration` or `PoolRetirement` certificates, and are
    /// NOT represented by the `pool_changes` delta vec (which is never
    /// populated). Without a snapshot, `apply_delta_to_state` would leave
    /// them at the stale anchor value so a fork rollback would resurrect
    /// retired pools or forget newly registered pools from the volatile window.
    ///
    /// `pool_params` is `Arc<HashMap>` so `Arc::clone()` is O(1) — just a
    /// refcount bump. `future_pool_params`, `pending_retirements`, and
    /// `pool_deposits` are small plain `HashMap`s (at most a few entries
    /// per block), so cloning them is negligible. Only emitted when
    /// `pool_params` was mutated (detected via `Arc::ptr_eq`). Blocks that
    /// contain no pool certs carry `None` and `apply_delta_to_state` leaves
    /// the fields untouched.
    pub pool_params_snapshot: Option<Arc<HashMap<Hash28, PoolRegistration>>>,
    /// See [`Self::pool_params_snapshot`].
    pub future_pool_params_snapshot: Option<HashMap<Hash28, PoolRegistration>>,
    /// See [`Self::pool_params_snapshot`].
    pub pending_retirements_snapshot: Option<HashMap<Hash28, EpochNo>>,
    /// See [`Self::pool_params_snapshot`].
    pub pool_deposits_snapshot: Option<HashMap<Hash28, u64>>,

    /// Post-block snapshot of the governance substate (`Arc<GovernanceState>`,
    /// so the clone is O(1)). Like the cert maps above, `gov` is mutated in
    /// place by `apply_block` (DRep registration, vote delegations, proposal
    /// ingestion, votes, ratification/enactment) and is NOT captured by
    /// `governance_changes` (which is never populated). Without this snapshot,
    /// `rollback_via_seq` (`self.gov = seq.tip_state().gov`) restored the stale
    /// anchor governance on every fork — wiping DReps/vote_delegations/proposals
    /// registered since the anchor. That zeroed DRep voting power, so real
    /// ParameterChanges never ratified (V3 cost model frozen at the genesis
    /// model → V3 script_data_hash divergence; `deposits_proposal: 0`). Same
    /// fork-reconstruction class as the reward-account fix.
    pub gov_snapshot: Option<GovSubState>,

    /// Post-block snapshot of the per-pool block-production counter
    /// (`consensus.epoch_blocks_by_pool`, `Arc<HashMap>` so the clone is O(1)).
    ///
    /// Unlike the other consensus scalars, this map is reconstructed during
    /// `apply_delta_to_state` via an INCREMENT (`block_fields.pool_block_increment`
    /// → `+= 1`), not an absolute assignment. An increment cannot recover blocks
    /// that were applied to the live `LedgerState` through a NO-DELTA path
    /// (gap-bridge advance in `handle_rollback_inner`, LSM/chunk replay,
    /// startup recovery) — those mutate `self.consensus.epoch_blocks_by_pool`
    /// directly but never push a `LedgerDelta`, leaving a hole in the delta
    /// chain. On the next `rollback_via_seq` (`self.consensus =
    /// seq.tip_state().consensus`) the reconstructed count is then SHORT by the
    /// number of hole blocks, silently under-counting `BlocksMade`. That drifts
    /// `eta = blocksMade / expectedBlocks` → `deltaR1 = floor(eta·rho·reserves)`
    /// too low → reserves drained too little + per-account reward skew (#763 —
    /// the bidirectional per-pool drift seen vs Koios, invisible to offline
    /// replay which has no holes). The absolute snapshot is authoritative and
    /// gap-robust, exactly mirroring the `reward_accounts`/`gov`/`pool_params`
    /// snapshot restores. `None` on deltas that did not change the map
    /// (overlay/Byron blocks) or non-`apply_block_with_delta` paths — then
    /// reconstruction carries the previous map forward (correct, unchanged).
    pub epoch_blocks_by_pool_snapshot: Option<Arc<HashMap<Hash28, u64>>>,

    // ── #782: fields omitted from the original delta allowlist ────────────
    //
    // The delta model above was built as an ALLOWLIST — only fields some
    // earlier bug report (#763, the ep292 reward corruption, etc.) actually
    // observed as stale got a snapshot. Every field below is a genuinely
    // mutable piece of `LedgerState` that had NO delta representation at
    // all, so a fork rollback silently regressed it to the anchor's frozen
    // value. See `_assert_ledger_state_fields_audited` at the bottom of this
    // file — it exists specifically to force a future field addition to
    // `LedgerState` through the same audit.
    /// Post-block snapshot of `certs.pointer_map` (Shelley-era pointer-address
    /// stake registrations). Mutated by `Certificate::StakeRegistration` (with
    /// a pointer) / deregistration in `state/certificates.rs`, which is NOT
    /// represented by the dead `delegation_changes` vec (see
    /// `reward_accounts_snapshot` docs). Content-diffed (not unconditional):
    /// the map only grows during early Shelley and is never bounded/cleared,
    /// so an unconditional clone would cost O(map size) forever; a
    /// content-diff still costs that on every block but at least skips the
    /// second `Some(...)` clone on the (overwhelming) common case of no
    /// pointer-cert in the block.
    pub pointer_map_snapshot: Option<HashMap<Pointer, Hash32>>,
    /// Post-block snapshot of `certs.script_stake_credentials`. Same
    /// mutation sites and rationale as [`Self::pointer_map_snapshot`].
    pub script_stake_credentials_snapshot: Option<HashSet<Hash32>>,
    /// Post-block snapshot of `certs.total_stake_key_deposits` (a plain
    /// `u64` scalar, so unconditional capture is free — mirrors
    /// `reward_accounts_snapshot`'s unconditional imbl clone).
    pub total_stake_key_deposits_snapshot: Option<u64>,
    /// Post-block snapshot of `certs.pending_mir_reserves` (Haskell
    /// `dsIRewards.irwdSrcReserves`). Pre-Conway only; drained to zero at
    /// every epoch boundary by `apply_pending_mir`. Content-diffed — MIR
    /// certs are rare, so this is `None` on almost every block.
    pub pending_mir_reserves_snapshot: Option<HashMap<Hash32, i128>>,
    /// Post-block snapshot of `certs.pending_mir_treasury`. See
    /// [`Self::pending_mir_reserves_snapshot`].
    pub pending_mir_treasury_snapshot: Option<HashMap<Hash32, i128>>,
    /// Post-block snapshot of `certs.pending_mir_delta_reserves` (pot-to-pot
    /// MIR transfer accumulator). See [`Self::pending_mir_reserves_snapshot`].
    pub pending_mir_delta_reserves_snapshot: Option<i128>,
    /// Post-block snapshot of `certs.pending_mir_delta_treasury`. See
    /// [`Self::pending_mir_reserves_snapshot`].
    pub pending_mir_delta_treasury_snapshot: Option<i128>,
    /// Post-block snapshot of the TOP-LEVEL `LedgerState.genesis_delegates`
    /// map (Shelley `Certificate::GenesisKeyDelegation`). This field lives
    /// directly on `LedgerState`, not inside any sub-state that
    /// `rollback_via_seq` wholesale-copies from the reconstructed tip, so it
    /// needs both this delta field AND an explicit copy-back in
    /// `rollback_via_seq` (state/mod.rs). Content-diffed — genesis key
    /// delegation is a rare bootstrap-era cert.
    pub genesis_delegates_snapshot: Option<HashMap<Hash28, (Hash28, Hash32)>>,
    /// Post-block ABSOLUTE snapshot of `epochs.pending_pp_updates`
    /// (pre-Conway PPUP proposals pending for the current/target epoch).
    /// Unconditional: this map holds only currently-active proposals (at
    /// most a handful of entries), so the clone is cheap every block. This
    /// supersedes the coarser `EpochTransitionDelta::pending_pp_updates_cleared`
    /// bool (which only handles the "both maps end up completely empty"
    /// case) — when present this snapshot MUST be applied AFTER the epoch
    /// transition's clear-if-empty step so the exact reconstruction wins.
    pub pending_pp_updates_snapshot: Option<BTreeMap<EpochNo, Vec<(Hash32, ProtocolParamUpdate)>>>,
    /// Post-block ABSOLUTE snapshot of `epochs.future_pp_updates`. See
    /// [`Self::pending_pp_updates_snapshot`].
    pub future_pp_updates_snapshot: Option<BTreeMap<EpochNo, Vec<(Hash32, ProtocolParamUpdate)>>>,
    /// Post-block snapshot of `epochs.rupd_addrs_rew` (the pv≤6 startStep
    /// reward-account prefilter; see the field docs on `EpochSubState`).
    /// Change-detected via `Arc::ptr_eq` on the inner `Arc` (both `None`s
    /// compare equal; a `None`↔`Some` transition or a pointer change counts
    /// as a change) — O(1) either way since cloning the outer `Option` is
    /// just an `Arc` refcount bump.
    pub rupd_addrs_rew_snapshot: Option<Option<Arc<HashSet<Hash32>>>>,
}

impl LedgerDelta {
    /// Create an empty delta for the given block header.
    pub fn new(slot: SlotNo, hash: Hash32, block_no: BlockNo) -> Self {
        LedgerDelta {
            slot,
            hash,
            block_no,
            utxo_diff: UtxoDiff::new(),
            delegation_changes: Vec::new(),
            pool_changes: Vec::new(),
            reward_changes: Vec::new(),
            governance_changes: Vec::new(),
            epoch_transition: None,
            block_fields: BlockFieldsDelta::default(),
            reward_accounts_snapshot: None,
            delegations_snapshot: None,
            stake_key_deposits_snapshot: None,
            pool_params_snapshot: None,
            future_pool_params_snapshot: None,
            pending_retirements_snapshot: None,
            pool_deposits_snapshot: None,
            gov_snapshot: None,
            epoch_blocks_by_pool_snapshot: None,
            pointer_map_snapshot: None,
            script_stake_credentials_snapshot: None,
            total_stake_key_deposits_snapshot: None,
            pending_mir_reserves_snapshot: None,
            pending_mir_treasury_snapshot: None,
            pending_mir_delta_reserves_snapshot: None,
            pending_mir_delta_treasury_snapshot: None,
            genesis_delegates_snapshot: None,
            pending_pp_updates_snapshot: None,
            future_pp_updates_snapshot: None,
            rupd_addrs_rew_snapshot: None,
        }
    }
}

// ─── Delegation ────────────────────────────────────────────────────────────

/// A change to the delegation map or pointer map.
#[derive(Debug, Clone)]
pub enum DelegationChange {
    /// New stake credential registered (added to reward_accounts with deposit).
    /// `pointer` is `Some` when registered via a certificate that also creates
    /// a pointer entry (Shelley StakeRegistration at a specific (slot, tx, cert)).
    Register {
        credential_hash: Hash32,
        is_script: bool,
        pointer: Option<dugite_primitives::credentials::Pointer>,
    },
    /// Stake credential deregistered (removed from delegations, reward_accounts).
    Deregister {
        credential_hash: Hash32,
        pointer: Option<dugite_primitives::credentials::Pointer>,
    },
    /// Delegation set or updated (credential → pool).
    Delegate {
        credential_hash: Hash32,
        pool_id: Hash28,
    },
    /// Delegation removed (e.g. stake address deregistered without re-delegation).
    Undelegate { credential_hash: Hash32 },
}

// ─── Pool ──────────────────────────────────────────────────────────────────

/// A change to the pool registration or retirement state.
#[derive(Debug, Clone)]
pub enum PoolChange {
    /// New pool registered (first-time registration, takes effect at epoch N+2).
    Register { params: PoolRegistration },
    /// Existing pool re-registered (parameters queued as future_pool_params).
    Reregister { params: PoolRegistration },
    /// Pool retirement announced for a future epoch.
    Retire { pool_id: Hash28, epoch: EpochNo },
    /// Pending retirement cancelled (re-registration before retirement epoch).
    CancelRetirement { pool_id: Hash28 },
}

// ─── Rewards ───────────────────────────────────────────────────────────────

/// A change to a reward account balance.
#[derive(Debug, Clone)]
pub enum RewardChange {
    /// Fee credited to reward account (from withdrawal certificate or deposit refund).
    Credit {
        credential_hash: Hash32,
        amount: Lovelace,
    },
    /// Reward withdrawn (balance reduced by withdrawal amount).
    Withdraw {
        credential_hash: Hash32,
        amount: Lovelace,
    },
    /// Reward account created (deposit held, initial balance 0).
    Create { credential_hash: Hash32 },
    /// Reward account destroyed.
    Destroy { credential_hash: Hash32 },
}

// ─── Governance ────────────────────────────────────────────────────────────

/// A change to Conway governance state.
#[derive(Debug, Clone)]
pub enum GovernanceChange {
    // DRep lifecycle
    DRepRegister {
        credential_hash: Hash32,
        registration: DRepRegistration,
        is_script: bool,
    },
    DRepUpdate {
        credential_hash: Hash32,
        anchor: Option<Anchor>,
        drep_expiry: EpochNo,
    },
    DRepUnregister {
        credential_hash: Hash32,
    },

    // Vote delegation
    VoteDelegate {
        credential_hash: Hash32,
        drep: DRep,
    },
    VoteUndelegate {
        credential_hash: Hash32,
    },

    // Constitutional committee
    CommitteeHotAuth {
        cold_credential_hash: Hash32,
        hot_credential_hash: Hash32,
        cold_is_script: bool,
        hot_is_script: bool,
    },
    CommitteeResign {
        cold_credential_hash: Hash32,
        anchor: Option<Anchor>,
        is_script: bool,
    },

    // Governance proposals
    ProposeAction {
        action_id: GovActionId,
        proposal: ProposalState,
    },

    // Votes
    CastVote {
        action_id: GovActionId,
        voter: Voter,
        procedure: VotingProcedure,
    },

    // Ratification outcomes (applied at epoch boundary)
    Enacted {
        action_id: GovActionId,
        proposal: ProposalState,
    },
    Expired {
        action_id: GovActionId,
    },

    // Constitutional updates
    SetConstitution {
        constitution: Constitution,
    },
    SetNoConfidence {
        no_confidence: bool,
    },
    SetCommitteeThreshold {
        threshold: Option<Rational>,
    },

    // Governance action counters
    IncrementDRepCount,
    IncrementProposalCount,
}

// ─── Epoch transition ──────────────────────────────────────────────────────

/// All scalar and collection changes made during an epoch transition.
///
/// When `process_epoch_transition()` runs, every field it touches is captured
/// here so that the transition can be replayed during state reconstruction.
/// This avoids having to re-run the full epoch transition logic (which is
/// expensive and stateful) during delta application.
#[derive(Debug, Clone)]
pub struct EpochTransitionDelta {
    /// The epoch number after the transition.
    pub new_epoch: EpochNo,
    /// New treasury balance.
    pub treasury: Lovelace,
    /// New reserves balance.
    pub reserves: Lovelace,
    /// Updated epoch snapshots (mark/set/go rotation).
    pub snapshots: EpochSnapshots,
    /// New protocol parameters (after PPUP/governance ratification).
    pub protocol_params: ProtocolParameters,
    /// Previous protocol parameters (swap during PPUP).
    pub prev_protocol_params: ProtocolParameters,
    /// Updated prev_d value (exact `Rational`; issue #629).
    pub prev_d: dugite_primitives::transaction::Rational,
    /// Updated prev_protocol_version_major.
    pub prev_protocol_version_major: u64,
    /// Cleared pending PP updates (pre-Conway).
    pub pending_pp_updates_cleared: bool,
    /// Epoch nonce updated at the transition.
    pub epoch_nonce: Hash32,
    /// New last_epoch_block_nonce.
    pub last_epoch_block_nonce: Hash32,
    /// `consensus.extra_entropy` after this transition (Shelley `ppExtraEntropy`,
    /// set by a pre-Conway PP update and folded into the epoch nonce at TICKN).
    /// Only mutated inside `process_epoch_transition`, so an unconditional
    /// post-transition capture here is exact — #782 (previously omitted from
    /// the delta entirely, so a fork rollback across an extra-entropy change
    /// silently regressed it, corrupting the next epoch's nonce derivation).
    pub extra_entropy: Hash32,
    /// Reward credits applied to individual accounts.
    pub reward_credits: HashMap<Hash32, Lovelace>,
    /// Pool retirements processed: pools removed.
    pub pools_retired: Vec<Hash28>,
    /// Future pool params promoted to pool_params.
    pub future_params_promoted: Vec<(Hash28, PoolRegistration)>,
    /// DRep active flags updated (credential_hash → new active state).
    pub drep_activity_updates: HashMap<Hash32, bool>,
    /// Last ratified and expired proposals (for GetRatifyState).
    pub last_ratified: Vec<(GovActionId, ProposalState)>,
    pub last_expired: Vec<GovActionId>,
    pub last_ratify_delayed: bool,
    /// Constitution set during this transition.
    pub new_constitution: Option<Constitution>,
    /// No-confidence state updated.
    pub no_confidence: Option<bool>,
    /// Committee threshold updated.
    pub committee_threshold: Option<Option<Rational>>,
    /// Proposals enacted by governance actions: proposals removed from
    /// active set.
    pub proposals_enacted: Vec<GovActionId>,
    /// Proposals expired: removed from active set.
    pub proposals_expired: Vec<GovActionId>,
    /// Enacted protocol param update IDs.
    pub enacted_pparam_update: Option<Option<GovActionId>>,
    pub enacted_hard_fork: Option<Option<GovActionId>>,
    pub enacted_committee: Option<Option<GovActionId>>,
    pub enacted_constitution: Option<Option<GovActionId>>,
    /// Post-transition stake distribution rebuild result.
    pub stake_distribution: StakeDistributionState,
    /// Delegation changes during transition (e.g. retiring pool delegator moves).
    pub delegation_changes: Vec<DelegationChange>,
}

// ─── Per-block scalar fields ───────────────────────────────────────────────

/// Scalar and nonce fields updated by each individual block.
///
/// These fields are updated by every block (not just epoch transitions) and
/// must be captured so that the exact historical state can be reconstructed.
#[derive(Debug, Clone)]
pub struct BlockFieldsDelta {
    /// Fee accumulated in this block (added to epoch_fees).
    pub fees_collected: Lovelace,
    /// The pool that produced this block (pool_id whose block count to
    /// increment by 1).  `None` for Byron blocks / blocks with no VRF proof.
    pub pool_block_increment: Option<Hash28>,
    /// Total epoch_block_count after this block.
    pub epoch_block_count: u64,
    /// Updated evolving_nonce (post-block).
    pub evolving_nonce: Hash32,
    /// Updated candidate_nonce (post-block; may be same as pre-block if
    /// the randomness stabilisation window has passed).
    pub candidate_nonce: Hash32,
    /// Updated lab_nonce (= prev_hash of this block).
    pub lab_nonce: Hash32,
    /// epoch_fees running total after this block.
    pub epoch_fees: Lovelace,
    /// `utxo.pending_donations` running total after this block (Conway
    /// treasury donations, accumulated per-tx, flushed at the epoch
    /// boundary). Was NOT actually part of the delta prior to #782 despite
    /// a doc comment on `rollback_via_seq` claiming otherwise — a fork
    /// rollback across a donation-bearing block silently regressed it.
    pub pending_donations: Lovelace,
    /// `LedgerState.era` after this block. Omitted from the delta model
    /// entirely prior to #782: a rollback across an era boundary (e.g.
    /// Babbage→Conway) regressed `era`, causing the next block applied to
    /// RE-RUN `on_era_transition` (re-zeroing `pending_donations`,
    /// re-seeding Conway DReps/committee, re-signaling
    /// `pending_era_transition`).
    pub era: Era,
    /// `epochs.pending_avvm_return` after this block (AVVM coin returned to
    /// reserves at the Shelley→Allegra boundary; consumed by the next
    /// `compute_reward_update`). Plain `u64` scalar — unconditional capture
    /// is free.
    pub pending_avvm_return: u64,
    /// Per-block absolute update to `consensus.opcert_counters[pool_id]`
    /// (max operational-cert sequence number observed for that pool), if
    /// this block's header carried a non-empty `issuer_vkey`. `None` for
    /// Byron blocks / headers without an issuer.
    ///
    /// This is a targeted single-key delta rather than a full-map snapshot:
    /// `opcert_counters` is mutated for AT MOST ONE pool per block (the
    /// block's own producer, in `compute_shelley_nonce`) and is never
    /// bounded or cleared, so it grows for the lifetime of the chain. A
    /// full-map content-diff (clone + compare every block, forever) would
    /// be unbounded overhead for no extra correctness; capturing just the
    /// touched `(pool_id, seq)` pair is O(1) and exact.
    pub opcert_counter_update: Option<(Hash28, u64)>,
}

impl Default for BlockFieldsDelta {
    fn default() -> Self {
        BlockFieldsDelta {
            fees_collected: Lovelace(0),
            pool_block_increment: None,
            epoch_block_count: 0,
            evolving_nonce: Hash32::ZERO,
            candidate_nonce: Hash32::ZERO,
            lab_nonce: Hash32::ZERO,
            epoch_fees: Lovelace(0),
            pending_donations: Lovelace(0),
            era: Era::Byron,
            pending_avvm_return: 0,
            opcert_counter_update: None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LedgerSeq
// ─────────────────────────────────────────────────────────────────────────────

/// Anchored sequence of ledger state deltas.
///
/// Implements Haskell's `LedgerDB.V2.LedgerSeq` (DiffTables variant):
/// one full anchor state at the immutable tip plus a window of per-block
/// deltas covering the volatile chain.
///
/// # Invariants
///
/// 1. `deltas.len() <= k` at all times (enforced by `push`).
/// 2. `checkpoints` keys are indices into the current `deltas` window, i.e.
///    every key `i` satisfies `i < deltas.len()`.
/// 3. The state at delta index `i` equals the anchor with deltas `[0..=i]`
///    applied.
/// 4. `anchor_point` matches `anchor.tip.point`.
pub struct LedgerSeq {
    /// Full ledger state at the immutable tip (the anchor point).
    ///
    /// This is the only full state ever saved to disk.  All volatile states
    /// are derived by applying deltas forward from the anchor.
    anchor: Box<LedgerState>,

    /// The chain point corresponding to the anchor state.
    anchor_point: Point,

    /// Per-block deltas in chronological order (oldest at front, newest at back).
    ///
    /// `deltas[0]` is the first block applied after the anchor.
    /// `deltas[deltas.len()-1]` is the tip delta.
    deltas: VecDeque<LedgerDelta>,

    /// Full `LedgerState` checkpoints stored in memory every
    /// `checkpoint_interval` deltas.
    ///
    /// Key: delta index after which the checkpoint was taken.  E.g. if
    /// `checkpoint_interval = 100` then checkpoints exist at indices
    /// 99, 199, 299, …  (the state produced by applying deltas `[0..=99]`
    /// is stored at key `99`).
    ///
    /// During reconstruction of the state at delta index `i`, the nearest
    /// checkpoint at index `j <= i` is loaded and then deltas `[j+1..=i]`
    /// are applied.
    checkpoints: BTreeMap<usize, Box<LedgerState>>,

    /// Number of deltas between consecutive checkpoints.
    checkpoint_interval: usize,

    /// Security parameter k: maximum number of volatile deltas retained.
    k: u64,

    /// When `true`, [`Self::push`] skips checkpoint creation.
    ///
    /// Checkpoints exist to accelerate rollback reconstruction — they hold
    /// Arc'd clones of the anchor's substates so a fork-switch can recover
    /// in O(checkpoint_interval) rather than O(k).  During catch-up sync
    /// the node never rolls back deeply (every block from BlockFetch is
    /// canonical-chain by the time it reaches `apply_block_with_delta`),
    /// so the checkpoint Arc clones are pure overhead: every shared Arc
    /// inflates the refcount, and the next `advance_anchor` →
    /// `apply_delta_to_state` → `Arc::make_mut` triggers a CoW deep clone
    /// of every mutated HashMap (10–30 MB each).  That was the ~480 ms
    /// per-block ceiling observed at preview epoch 25+ across multiple
    /// runs (#702).
    ///
    /// At-tip sets this to `false` so the original rollback acceleration
    /// is restored.  Default `false` so existing callers without an
    /// at-tip toggle keep their current behaviour.
    catchup_mode: bool,
}

impl LedgerSeq {
    /// Create a new `LedgerSeq` anchored at the given state.
    ///
    /// # Parameters
    ///
    /// - `anchor`: Full ledger state at the immutable tip.  Ownership is
    ///   transferred; the caller should not hold another copy.
    /// - `k`: Security parameter (number of blocks for rollback window).
    ///   The volatile window will hold at most `k` deltas before the anchor
    ///   is advanced.
    /// - `checkpoint_interval`: How often to store a full checkpoint in
    ///   memory.  Default 100; increasing reduces memory at the cost of
    ///   slower reconstruction.
    pub fn new(anchor: LedgerState, k: u64, checkpoint_interval: usize) -> Self {
        let anchor_point = anchor.tip.point.clone();
        LedgerSeq {
            anchor: Box::new(anchor),
            anchor_point,
            deltas: VecDeque::new(),
            checkpoints: BTreeMap::new(),
            checkpoint_interval,
            k,
            catchup_mode: false,
        }
    }

    /// Enable or disable catch-up mode.
    ///
    /// In catch-up mode, [`Self::push`] does not create checkpoints — the
    /// Arc-clone fan-out they create is wasted work because catch-up never
    /// triggers a deep rollback.  See the field comment for the rationale.
    /// Caller should flip this back to `false` once the node reaches tip.
    ///
    /// Returns `true` iff the mode actually changed.
    pub fn set_catchup_mode(&mut self, catchup: bool) -> bool {
        let changed = self.catchup_mode != catchup;
        self.catchup_mode = catchup;
        changed
    }

    /// Whether the sequence is in catch-up mode (checkpoints suppressed).
    pub fn is_catchup_mode(&self) -> bool {
        self.catchup_mode
    }

    /// Create a `LedgerSeq` with default settings (checkpoint every 100 blocks).
    pub fn with_defaults(anchor: LedgerState, k: u64) -> Self {
        Self::new(anchor, k, 100)
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    /// Current anchor point (immutable tip).
    pub fn anchor_point(&self) -> &Point {
        &self.anchor_point
    }

    /// Number of volatile deltas currently held.
    pub fn len(&self) -> usize {
        self.deltas.len()
    }

    /// Whether the volatile window is empty (chain tip == anchor).
    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty()
    }

    /// Maximum rollback depth: number of blocks that can be rolled back
    /// without losing state.  Equals `deltas.len()`.
    pub fn max_rollback(&self) -> usize {
        self.deltas.len()
    }

    /// The chain point at the current tip (newest delta, or anchor if empty).
    pub fn tip_point(&self) -> Point {
        if let Some(d) = self.deltas.back() {
            // Reconstruct a Specific point from the tip delta
            dugite_primitives::block::Point::Specific(d.slot, d.hash)
        } else {
            self.anchor_point.clone()
        }
    }

    /// Reference to the raw anchor state (the immutable tip).
    ///
    /// Callers needing the volatile-tip state should use `tip_state()`.
    pub fn anchor_state(&self) -> &LedgerState {
        &self.anchor
    }

    // ── State reconstruction ─────────────────────────────────────────────────

    /// Reconstruct the ledger state at the current tip by applying deltas
    /// from the nearest checkpoint.
    ///
    /// The returned state has an **incomplete UTxO set** — it contains only
    /// UTxOs that changed within the volatile window. N2C queries and mempool
    /// validation should use the live `ledger_state` (which has the LSM store),
    /// not this reconstructed state.
    ///
    /// Cost: O(`checkpoint_interval`) delta applications — at most
    /// `checkpoint_interval` blocks regardless of how many deltas exist.
    pub fn tip_state(&self) -> LedgerState {
        if self.deltas.is_empty() {
            return (*self.anchor).clone_without_utxos();
        }
        self.state_at_index(self.deltas.len() - 1)
    }

    /// Reconstruct the ledger state after applying the first `index + 1`
    /// deltas (0-indexed).
    ///
    /// The returned state has an **incomplete UTxO set** — only UTxOs that
    /// changed between the base state and the requested index are present.
    /// Non-UTxO state (delegations, rewards, governance, epochs) is complete.
    pub fn state_at_index(&self, index: usize) -> LedgerState {
        debug_assert!(
            index < self.deltas.len(),
            "state_at_index: index {} out of bounds (deltas.len()={})",
            index,
            self.deltas.len()
        );

        // Find the nearest checkpoint at or before `index`.
        // Checkpoints are lightweight (no UTxO data) — clone is cheap.
        let (start_index, base_state) = match self.checkpoints.range(..=index).next_back() {
            Some((&cp_idx, cp_state)) => (cp_idx + 1, (**cp_state).clone()),
            None => (0, (*self.anchor).clone_without_utxos()),
        };

        // Apply deltas [start_index..=index].
        let mut state = base_state;
        for i in start_index..=index {
            let delta = &self.deltas[i];
            apply_delta_to_state(&mut state, delta);
        }
        state
    }

    /// Reconstruct the ledger state at a specific chain point within the
    /// volatile window.
    ///
    /// Returns `None` if the point is not found in the volatile window.
    ///
    /// Cost: O(`checkpoint_interval`) delta applications.
    pub fn state_at(&self, slot: SlotNo, hash: &Hash32) -> Option<LedgerState> {
        // Search deltas from newest to oldest for a matching point.
        let idx = self
            .deltas
            .iter()
            .enumerate()
            .rev()
            .find(|(_, d)| d.slot == slot && &d.hash == hash)
            .map(|(i, _)| i)?;

        Some(self.state_at_index(idx))
    }

    // ── Mutation ────────────────────────────────────────────────────────────

    /// Push a new block's delta onto the volatile window.
    ///
    /// If the number of deltas would exceed `k`, `advance_anchor` is called
    /// first to move the oldest delta into the anchor.
    ///
    /// After appending, if the new delta's index is a multiple of
    /// `checkpoint_interval - 1` (i.e. every N blocks), a full checkpoint
    /// is stored.
    pub fn push(&mut self, delta: LedgerDelta) {
        // Enforce the k-block volatile window by advancing the anchor when full.
        while self.deltas.len() >= self.k as usize {
            self.advance_anchor();
        }

        self.deltas.push_back(delta);
        let new_idx = self.deltas.len() - 1;

        // Store a checkpoint every `checkpoint_interval` blocks.
        // Checkpoint is taken at indices checkpoint_interval-1, 2*(checkpoint_interval)-1, …
        //
        // Skipped during catch-up mode because every checkpoint Arc-clone
        // bumps the refcount on the anchor's substates, which forces the
        // next `advance_anchor` → `apply_delta_to_state` → `Arc::make_mut`
        // path to CoW-deep-clone any mutated HashMap.  See the field
        // comment on `catchup_mode` for the full rationale (#702).
        if !self.catchup_mode && (new_idx + 1).is_multiple_of(self.checkpoint_interval) {
            let cp = Box::new(self.state_at_index(new_idx));
            self.checkpoints.insert(new_idx, cp);
        }
    }

    /// Roll back `n` blocks.
    ///
    /// Removes the last `n` deltas from the volatile window and invalidates
    /// any checkpoints that pointed into the removed range.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `n > deltas.len()`.  In release builds
    /// it silently clamps to `deltas.len()`.
    ///
    /// Cost: O(n) for deque truncation + O(checkpoints pruned).
    pub fn rollback(&mut self, n: usize) {
        let n = n.min(self.deltas.len());
        let new_len = self.deltas.len() - n;

        // Trim deltas.
        self.deltas.truncate(new_len);

        // Drop checkpoints that pointed into the removed range.
        // A checkpoint at index `i` is valid only if `i < new_len`.
        self.checkpoints.retain(|&idx, _| idx < new_len);
    }

    /// Number of deltas to drop to reach `target_point`, or `None` if the
    /// point is not in the volatile window or is not the anchor.
    ///
    /// Used by callers that want to translate a `MsgRollBackward` chain
    /// point into a counted rollback for [`Self::rollback`].
    ///
    /// Matches Haskell's `LedgerDB.V2.rollbackToPoint`, which returns the
    /// number of blocks rolled back when the point is on the volatile
    /// chain and `Nothing` otherwise (the caller then falls back to a
    /// snapshot-driven recovery).
    ///
    /// # Cases
    ///
    /// - `target_point == anchor_point` → `Some(deltas.len())` (full rewind
    ///   to the immutable tip).
    /// - `target_point` matches some `deltas[i]` → `Some(deltas.len() - i - 1)`.
    /// - Otherwise → `None`.
    pub fn find_rollback_n(&self, target_point: &Point) -> Option<usize> {
        if &self.anchor_point == target_point {
            return Some(self.deltas.len());
        }
        for (i, delta) in self.deltas.iter().enumerate().rev() {
            let p = Point::Specific(delta.slot, delta.hash);
            if &p == target_point {
                return Some(self.deltas.len() - i - 1);
            }
        }
        None
    }

    /// Roll back to a specific chain point.  Returns the number of blocks
    /// rolled back, or `None` if the point is not in the volatile window
    /// (caller must fall back to snapshot-driven recovery).
    ///
    /// Cost: O(n) where `n` is the rollback distance.
    pub fn rollback_to_point(&mut self, target_point: &Point) -> Option<usize> {
        let n = self.find_rollback_n(target_point)?;
        self.rollback(n);
        Some(n)
    }

    /// Advance the anchor: apply the oldest delta to the anchor state, pop
    /// it from the deque, and re-index the remaining checkpoints.
    ///
    /// Called automatically by `push` when `deltas.len() >= k`, and
    /// explicitly when the immutable tip advances (copy-to-immutable).
    ///
    /// Cost: O(checkpoint_interval) for the anchor update + O(checkpoints)
    /// for re-indexing.
    pub fn advance_anchor(&mut self) {
        if self.deltas.is_empty() {
            return;
        }

        // Apply the oldest delta to the anchor.
        let oldest = self.deltas.pop_front().unwrap();
        apply_delta_to_state(&mut self.anchor, &oldest);
        self.anchor_point = dugite_primitives::block::Point::Specific(oldest.slot, oldest.hash);

        // Re-index checkpoints: every stored index shifts down by 1.
        // Checkpoints that were at index 0 (= the delta we just consumed)
        // are now part of the anchor — drop them.
        let old_checkpoints = std::mem::take(&mut self.checkpoints);
        self.checkpoints = old_checkpoints
            .into_iter()
            .filter_map(|(idx, state)| {
                if idx == 0 {
                    // This checkpoint was for the delta we just absorbed —
                    // it is now redundant (the anchor IS that state).
                    None
                } else {
                    Some((idx - 1, state))
                }
            })
            .collect();
    }

    /// Replace the anchor with a new full state (e.g. after loading a
    /// snapshot from disk).  Clears all volatile deltas and checkpoints.
    pub fn reset_anchor(&mut self, new_anchor: LedgerState) {
        self.anchor_point = new_anchor.tip.point.clone();
        *self.anchor = new_anchor;
        self.deltas.clear();
        self.checkpoints.clear();
    }

    /// Return a reference to all deltas (oldest first).  Used by the
    /// startup recovery path to replay volatile blocks.
    pub fn deltas(&self) -> &VecDeque<LedgerDelta> {
        &self.deltas
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Delta application
// ─────────────────────────────────────────────────────────────────────────────

/// Apply a single `LedgerDelta` to a `LedgerState` in-place.
///
/// This is the forward-direction application used during state reconstruction.
/// It is NOT used during rollback (rollback simply discards deltas).
///
/// Every field that can be modified by a block must be handled here.
/// If a new delta variant is added and this function is not updated, the
/// compiler will warn only if the enum is `#[non_exhaustive]` — therefore
/// reviewers MUST audit this function when adding new delta variants.
pub fn apply_delta_to_state(state: &mut LedgerState, delta: &LedgerDelta) {
    // ── 1. UTxO changes ─────────────────────────────────────────────────────
    apply_utxo_diff(state, &delta.utxo_diff);

    // ── 2. Delegation changes ────────────────────────────────────────────────
    for change in &delta.delegation_changes {
        apply_delegation_change(state, change);
    }

    // ── 3. Pool changes ──────────────────────────────────────────────────────
    for change in &delta.pool_changes {
        apply_pool_change(state, change);
    }

    // ── 4. Reward account changes ────────────────────────────────────────────
    for change in &delta.reward_changes {
        apply_reward_change(state, change);
    }

    // ── 4b. Directly-mutated imbl cert maps (snapshot restore) ────────────────
    //
    // `reward_accounts`, `delegations` and `stake_key_deposits` are mutated in
    // place by `apply_block` and are NOT represented by the `*_changes` vecs
    // above (those are never populated). Restore them from the post-block
    // snapshot captured in `apply_block_with_delta` so state reconstruction
    // (anchor advance, `state_at_index`, `rollback_via_seq`) is exact rather
    // than inheriting the stale anchor value. `imbl::HashMap` clone is O(1).
    if let Some(ra) = &delta.reward_accounts_snapshot {
        state.certs.reward_accounts = ra.clone();
    }
    if let Some(dl) = &delta.delegations_snapshot {
        state.certs.delegations = dl.clone();
    }
    if let Some(sk) = &delta.stake_key_deposits_snapshot {
        state.certs.stake_key_deposits = sk.clone();
    }
    // Pool state snapshots: pool_params (Arc clone = O(1)), and the small
    // plain-HashMap pool fields that change on pool cert blocks.
    if let Some(pp) = &delta.pool_params_snapshot {
        state.certs.pool_params = Arc::clone(pp);
    }
    if let Some(fpp) = &delta.future_pool_params_snapshot {
        state.certs.future_pool_params = fpp.clone();
    }
    if let Some(pr) = &delta.pending_retirements_snapshot {
        state.certs.pending_retirements = pr.clone();
    }
    if let Some(pd) = &delta.pool_deposits_snapshot {
        state.certs.pool_deposits = pd.clone();
    }
    if let Some(g) = &delta.gov_snapshot {
        state.gov = g.clone();
    }
    // #782: fields omitted from the original allowlist. See the field docs
    // on `LedgerDelta` for why each is (or isn't) content-diffed.
    if let Some(pm) = &delta.pointer_map_snapshot {
        state.certs.pointer_map = pm.clone();
    }
    if let Some(ssc) = &delta.script_stake_credentials_snapshot {
        state.certs.script_stake_credentials = ssc.clone();
    }
    if let Some(tskd) = &delta.total_stake_key_deposits_snapshot {
        state.certs.total_stake_key_deposits = *tskd;
    }
    if let Some(pmr) = &delta.pending_mir_reserves_snapshot {
        state.certs.pending_mir_reserves = pmr.clone();
    }
    if let Some(pmt) = &delta.pending_mir_treasury_snapshot {
        state.certs.pending_mir_treasury = pmt.clone();
    }
    if let Some(pmdr) = delta.pending_mir_delta_reserves_snapshot {
        state.certs.pending_mir_delta_reserves = pmdr;
    }
    if let Some(pmdt) = delta.pending_mir_delta_treasury_snapshot {
        state.certs.pending_mir_delta_treasury = pmdt;
    }
    // `genesis_delegates` lives directly on `LedgerState` (not a sub-state),
    // so unlike the fields above it also needs an explicit copy-back in
    // `rollback_via_seq` (state/mod.rs) — this restore alone only fixes
    // reconstruction via `state_at_index`/`advance_anchor`.
    if let Some(gd) = &delta.genesis_delegates_snapshot {
        state.genesis_delegates = gd.clone();
    }

    // ── 5. Governance changes ─────────────────────────────────────────────────
    for change in &delta.governance_changes {
        apply_governance_change(state, change);
    }

    // ── 6. Epoch transition ───────────────────────────────────────────────────
    if let Some(et) = &delta.epoch_transition {
        apply_epoch_transition_delta(state, et);
    }

    // ── 7. Per-block scalar / nonce updates ───────────────────────────────────
    apply_block_fields(state, &delta.block_fields);

    // ── 7b. Absolute restore of the per-pool block counter ────────────────────
    //
    // Authoritative override of the `pool_block_increment` reconstruction in
    // `apply_block_fields`. The increment cannot recover blocks applied through
    // a no-delta path (gap-bridge advance, LSM/chunk replay, startup recovery)
    // that mutated `self.consensus.epoch_blocks_by_pool` without pushing a
    // delta, so increment-only reconstruction under-counts `BlocksMade` and
    // drifts reserves/rewards (#763). When present, the post-block snapshot is
    // exact and gap-robust. MUST run AFTER step 6 (epoch transition clears the
    // map) and AFTER step 7 (the increment) so it has the final say. See the
    // field docs on `LedgerDelta::epoch_blocks_by_pool_snapshot`.
    if let Some(eb) = &delta.epoch_blocks_by_pool_snapshot {
        state.consensus.epoch_blocks_by_pool = Arc::clone(eb);
    }

    // ── 7c. Absolute restore of pre-Conway PPUP proposal maps + rupd prefilter ──
    //
    // Authoritative override of `EpochTransitionDelta::pending_pp_updates_cleared`
    // (step 6), which only models the coarse "both maps ended up completely
    // empty" case via `.clear()`. These unconditional per-block snapshots are
    // exact regardless of how partial/future the post-transition maps are —
    // MUST run after step 6 so they have the final say (#782).
    if let Some(ppu) = &delta.pending_pp_updates_snapshot {
        state.epochs.pending_pp_updates = ppu.clone();
    }
    if let Some(fppu) = &delta.future_pp_updates_snapshot {
        state.epochs.future_pp_updates = fppu.clone();
    }
    if let Some(rar) = &delta.rupd_addrs_rew_snapshot {
        state.epochs.rupd_addrs_rew = rar.clone();
    }

    // Update tip to reflect this block.
    state.tip = dugite_primitives::block::Tip {
        point: dugite_primitives::block::Point::Specific(delta.slot, delta.hash),
        block_number: delta.block_no,
    };
}

// ── UTxO ─────────────────────────────────────────────────────────────────────

fn apply_utxo_diff(state: &mut LedgerState, diff: &UtxoDiff) {
    use crate::state::{stake_routing, StakeRouting};
    use dugite_primitives::value::Lovelace;

    let ptr_stake_excluded = state.epochs.ptr_stake_excluded;

    // The incremental UTxO-stake (Haskell `InstantStake`) must be replayed here,
    // not just the UTxO set. `apply_delta_to_state` is the ONLY path that
    // reconstructs the live `certs` after a fork rollback (`rollback_via_seq`
    // assigns `self.certs = seq.tip_state().certs`) and during anchor advance.
    // Without mirroring the live `apply_utxo_changes` add/subtract on
    // `stake_distribution.stake_map` / `ptr_stake`, every per-credential UTxO
    // stake change accumulated since the anchor is silently dropped on rollback,
    // leaving a stake_map that no longer matches the (correctly inverted) UTxO
    // set. That add/subtract asymmetry compounds into reward/pool-stake
    // divergence (e.g. the exactly-5-ADA-per-credential preprod ep57 short on
    // pool1n84mel6, which then cascades to the ep181 WithdrawalAmountMismatch).
    //
    // `inserts` are newly-created outputs (ADD stake, mirrors Phase 5 of
    // `eras::common::apply_utxo_changes`); `deletes` are spent outputs (SUB
    // stake, mirrors Phase 2). The routing (`stake_routing`) is byte-for-byte
    // the same function the live path uses, so the keys are guaranteed
    // identical — the fix is symmetric by construction.

    // Inserts: new outputs add stake.
    for (input, output) in &diff.inserts {
        let coin = output.value.coin.0;
        match stake_routing(&output.address, ptr_stake_excluded) {
            StakeRouting::Credential(cred_hash) => {
                *state
                    .certs
                    .stake_distribution
                    .stake_map
                    .entry(cred_hash)
                    .or_insert(Lovelace(0)) += Lovelace(coin);
            }
            StakeRouting::Pointer(ptr) => {
                *state.epochs.ptr_stake.entry(ptr).or_insert(0) += coin;
            }
            StakeRouting::None => {}
        }
        state.utxo.utxo_set.insert(input.clone(), output.clone());
    }

    // Deletes: spent outputs subtract stake.
    for (input, output) in &diff.deletes {
        let coin = output.value.coin.0;
        match stake_routing(&output.address, ptr_stake_excluded) {
            StakeRouting::Credential(cred_hash) => {
                if let Some(stake) = state.certs.stake_distribution.stake_map.get_mut(&cred_hash) {
                    stake.0 = stake.0.saturating_sub(coin);
                }
            }
            StakeRouting::Pointer(ptr) => {
                if let Some(entry) = state.epochs.ptr_stake.get_mut(&ptr) {
                    *entry = entry.saturating_sub(coin);
                }
            }
            StakeRouting::None => {}
        }
        state.utxo.utxo_set.remove(input);
    }
}

// ── Delegation ───────────────────────────────────────────────────────────────

fn apply_delegation_change(state: &mut LedgerState, change: &DelegationChange) {
    match change {
        DelegationChange::Register {
            credential_hash,
            is_script,
            pointer,
        } => {
            // Ensure reward account exists (registered with 0 balance).
            state
                .certs
                .reward_accounts
                .entry(*credential_hash)
                .or_insert(Lovelace(0));
            if *is_script {
                state
                    .certs
                    .script_stake_credentials
                    .insert(*credential_hash);
            }
            if let Some(ptr) = pointer {
                state.certs.pointer_map.insert(*ptr, *credential_hash);
            }
        }
        DelegationChange::Deregister {
            credential_hash,
            pointer,
        } => {
            state.certs.delegations.remove(credential_hash);
            state.certs.reward_accounts.remove(credential_hash);
            state.certs.script_stake_credentials.remove(credential_hash);
            if let Some(ptr) = pointer {
                state.certs.pointer_map.remove(ptr);
            }
        }
        DelegationChange::Delegate {
            credential_hash,
            pool_id,
        } => {
            state.certs.delegations.insert(*credential_hash, *pool_id);
        }
        DelegationChange::Undelegate { credential_hash } => {
            state.certs.delegations.remove(credential_hash);
        }
    }
}

// ── Pool ─────────────────────────────────────────────────────────────────────

fn apply_pool_change(state: &mut LedgerState, change: &PoolChange) {
    match change {
        PoolChange::Register { params } => {
            Arc::make_mut(&mut state.certs.pool_params).insert(params.pool_id, params.clone());
        }
        PoolChange::Reregister { params } => {
            state
                .certs
                .future_pool_params
                .insert(params.pool_id, params.clone());
        }
        PoolChange::Retire { pool_id, epoch } => {
            state.certs.pending_retirements.insert(*pool_id, *epoch);
        }
        PoolChange::CancelRetirement { pool_id } => {
            state.certs.pending_retirements.remove(pool_id);
        }
    }
}

// ── Rewards ──────────────────────────────────────────────────────────────────

fn apply_reward_change(state: &mut LedgerState, change: &RewardChange) {
    match change {
        RewardChange::Credit {
            credential_hash,
            amount,
        } => {
            let accounts = &mut state.certs.reward_accounts;
            let entry = accounts.entry(*credential_hash).or_insert(Lovelace(0));
            entry.0 = entry.0.saturating_add(amount.0);
        }
        RewardChange::Withdraw {
            credential_hash,
            amount,
        } => {
            let accounts = &mut state.certs.reward_accounts;
            if let Some(bal) = accounts.get_mut(credential_hash) {
                bal.0 = bal.0.saturating_sub(amount.0);
            }
        }
        RewardChange::Create { credential_hash } => {
            state
                .certs
                .reward_accounts
                .entry(*credential_hash)
                .or_insert(Lovelace(0));
        }
        RewardChange::Destroy { credential_hash } => {
            state.certs.reward_accounts.remove(credential_hash);
        }
    }
}

// ── Governance ───────────────────────────────────────────────────────────────

fn apply_governance_change(state: &mut LedgerState, change: &GovernanceChange) {
    let gov = Arc::make_mut(&mut state.gov.governance);
    match change {
        GovernanceChange::DRepRegister {
            credential_hash,
            registration,
            is_script,
        } => {
            gov.dreps.insert(*credential_hash, registration.clone());
            if *is_script {
                // Script DReps don't have a separate tracking set in
                // GovernanceState currently, but we note this for future use.
            }
            gov.drep_registration_count += 1;
        }
        GovernanceChange::DRepUpdate {
            credential_hash,
            anchor,
            drep_expiry,
        } => {
            if let Some(drep) = gov.dreps.get_mut(credential_hash) {
                drep.anchor = anchor.clone();
                drep.drep_expiry = *drep_expiry;
                drep.active = true;
            }
        }
        GovernanceChange::DRepUnregister { credential_hash } => {
            gov.dreps.remove(credential_hash);
            gov.vote_delegations.retain(|_, d| {
                // Remove delegations to this DRep (key credential).
                // Note: DRep::KeyHash is matched by credential_hash.
                !matches!(d, DRep::KeyHash(h) if h == credential_hash)
            });
        }
        GovernanceChange::VoteDelegate {
            credential_hash,
            drep,
        } => {
            gov.vote_delegations.insert(*credential_hash, drep.clone());
        }
        GovernanceChange::VoteUndelegate { credential_hash } => {
            gov.vote_delegations.remove(credential_hash);
        }
        GovernanceChange::CommitteeHotAuth {
            cold_credential_hash,
            hot_credential_hash,
            cold_is_script,
            hot_is_script,
        } => {
            gov.committee_hot_keys
                .insert(*cold_credential_hash, *hot_credential_hash);
            gov.committee_resigned.remove(cold_credential_hash);
            if *cold_is_script {
                gov.script_committee_credentials
                    .insert(*cold_credential_hash);
            }
            if *hot_is_script {
                gov.script_committee_hot_credentials
                    .insert(*hot_credential_hash);
            }
        }
        GovernanceChange::CommitteeResign {
            cold_credential_hash,
            anchor,
            is_script,
        } => {
            gov.committee_resigned
                .insert(*cold_credential_hash, anchor.clone());
            gov.committee_hot_keys.remove(cold_credential_hash);
            if *is_script {
                gov.script_committee_credentials
                    .insert(*cold_credential_hash);
            }
        }
        GovernanceChange::ProposeAction {
            action_id,
            proposal,
        } => {
            gov.proposals.insert(action_id.clone(), proposal.clone());
            gov.proposal_count += 1;
        }
        GovernanceChange::CastVote {
            action_id,
            voter,
            procedure,
        } => {
            let votes = gov.votes_by_action.entry(action_id.clone()).or_default();
            // Replace existing vote from this voter or append.
            if let Some(entry) = votes.iter_mut().find(|(v, _)| v == voter) {
                entry.1 = procedure.clone();
            } else {
                votes.push((voter.clone(), procedure.clone()));
            }
        }
        GovernanceChange::Enacted {
            action_id,
            proposal,
        } => {
            gov.proposals.remove(action_id);
            gov.votes_by_action.remove(action_id);
            gov.last_ratified
                .push((action_id.clone(), proposal.clone()));
        }
        GovernanceChange::Expired { action_id } => {
            gov.proposals.remove(action_id);
            gov.votes_by_action.remove(action_id);
            gov.last_expired.push(action_id.clone());
        }
        GovernanceChange::SetConstitution { constitution } => {
            gov.constitution = Some(constitution.clone());
        }
        GovernanceChange::SetNoConfidence { no_confidence } => {
            gov.no_confidence = *no_confidence;
        }
        GovernanceChange::SetCommitteeThreshold { threshold } => {
            gov.committee_threshold = threshold.clone();
        }
        GovernanceChange::IncrementDRepCount => {
            gov.drep_registration_count += 1;
        }
        GovernanceChange::IncrementProposalCount => {
            gov.proposal_count += 1;
        }
    }
}

// ── Epoch transition ──────────────────────────────────────────────────────────

fn apply_epoch_transition_delta(state: &mut LedgerState, et: &EpochTransitionDelta) {
    state.epoch = et.new_epoch;
    state.epochs.treasury = et.treasury;
    state.epochs.reserves = et.reserves;
    state.epochs.snapshots = et.snapshots.clone();
    state.epochs.protocol_params = et.protocol_params.clone();
    state.epochs.prev_protocol_params = et.prev_protocol_params.clone();
    state.epochs.prev_d = et.prev_d.clone();
    state.epochs.prev_protocol_version_major = et.prev_protocol_version_major;
    state.consensus.epoch_nonce = et.epoch_nonce;
    state.consensus.last_epoch_block_nonce = et.last_epoch_block_nonce;
    state.consensus.extra_entropy = et.extra_entropy;
    state.certs.stake_distribution = et.stake_distribution.clone();

    if et.pending_pp_updates_cleared {
        state.epochs.pending_pp_updates.clear();
        state.epochs.future_pp_updates.clear();
    }

    // Apply reward credits.
    {
        let accounts = &mut state.certs.reward_accounts;
        for (cred, amount) in &et.reward_credits {
            let bal = accounts.entry(*cred).or_insert(Lovelace(0));
            bal.0 = bal.0.saturating_add(amount.0);
        }
    }

    // Remove retired pools.
    {
        let pools = Arc::make_mut(&mut state.certs.pool_params);
        for pool_id in &et.pools_retired {
            pools.remove(pool_id);
            state.certs.future_pool_params.remove(pool_id);
        }
    }
    // Clean up pending retirements for the epoch just processed.
    state
        .certs
        .pending_retirements
        .retain(|_, ep| *ep > et.new_epoch);

    // Promote future pool params.
    {
        let pools = Arc::make_mut(&mut state.certs.pool_params);
        for (pool_id, params) in &et.future_params_promoted {
            pools.insert(*pool_id, params.clone());
            state.certs.future_pool_params.remove(pool_id);
        }
    }

    // Update DRep activity flags.
    {
        let gov = Arc::make_mut(&mut state.gov.governance);
        for (cred, active) in &et.drep_activity_updates {
            if let Some(drep) = gov.dreps.get_mut(cred) {
                drep.active = *active;
            }
        }
        gov.last_ratified = et.last_ratified.clone();
        gov.last_expired = et.last_expired.clone();
        gov.last_ratify_delayed = et.last_ratify_delayed;

        if let Some(c) = &et.new_constitution {
            gov.constitution = Some(c.clone());
        }
        if let Some(nc) = et.no_confidence {
            gov.no_confidence = nc;
        }
        if let Some(thresh) = &et.committee_threshold {
            gov.committee_threshold = thresh.clone();
        }
        for action_id in &et.proposals_enacted {
            gov.proposals.remove(action_id);
            gov.votes_by_action.remove(action_id);
        }
        for action_id in &et.proposals_expired {
            gov.proposals.remove(action_id);
            gov.votes_by_action.remove(action_id);
        }
        if let Some(v) = &et.enacted_pparam_update {
            gov.enacted_pparam_update = v.clone();
        }
        if let Some(v) = &et.enacted_hard_fork {
            gov.enacted_hard_fork = v.clone();
        }
        if let Some(v) = &et.enacted_committee {
            gov.enacted_committee = v.clone();
        }
        if let Some(v) = &et.enacted_constitution {
            gov.enacted_constitution = v.clone();
        }
    }

    // Apply transition-level delegation changes (e.g. retiring pool movers).
    for change in &et.delegation_changes {
        apply_delegation_change(state, change);
    }

    // Reset per-epoch counters.
    state.utxo.epoch_fees = Lovelace(0);
    Arc::make_mut(&mut state.consensus.epoch_blocks_by_pool).clear();
    state.consensus.epoch_block_count = 0;
}

// ── Per-block scalar / nonce fields ──────────────────────────────────────────

fn apply_block_fields(state: &mut LedgerState, fields: &BlockFieldsDelta) {
    // epoch_fees and epoch_block_count are already in the delta as the
    // running totals AFTER this block so we can just assign.
    state.utxo.epoch_fees = fields.epoch_fees;
    state.consensus.epoch_block_count = fields.epoch_block_count;
    state.consensus.evolving_nonce = fields.evolving_nonce;
    state.consensus.candidate_nonce = fields.candidate_nonce;
    state.consensus.lab_nonce = fields.lab_nonce;
    // #782: era/pending_donations/pending_avvm_return are post-block absolute
    // scalars, exactly like epoch_fees above.
    state.utxo.pending_donations = fields.pending_donations;
    state.era = fields.era;
    state.epochs.pending_avvm_return = fields.pending_avvm_return;

    if let Some(pool_id) = fields.pool_block_increment {
        *Arc::make_mut(&mut state.consensus.epoch_blocks_by_pool)
            .entry(pool_id)
            .or_insert(0) += 1;
    }
    // #782: single-key absolute restore — see the field doc on
    // `BlockFieldsDelta::opcert_counter_update` for why this isn't a
    // full-map snapshot.
    if let Some((pool_id, seq)) = fields.opcert_counter_update {
        state.consensus.opcert_counters.insert(pool_id, seq);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// #782 compile-time field audit guard
// ─────────────────────────────────────────────────────────────────────────────

/// Never called. Exists solely so that adding a new field to `LedgerState`
/// without updating this function is a COMPILE ERROR (no `..` rest pattern
/// below), forcing the author to also consult:
///
/// - `apply_block_with_delta_impl` (`crates/dugite-ledger/src/state/apply.rs`)
///   — does the new field need a per-block capture into `LedgerDelta`?
/// - `apply_delta_to_state` (this file) — does the new field need a restore
///   arm during state reconstruction?
/// - `rollback_via_seq` (`crates/dugite-ledger/src/state/mod.rs`) — if the
///   field lives directly on `LedgerState` (not inside `certs` / `gov` /
///   `consensus` / `epochs`), does it need an explicit copy-back?
///
/// See #782: the delta model was an undocumented allowlist for years, so
/// fields added to `LedgerState` silently had no rollback coverage.
#[allow(dead_code)]
fn _assert_ledger_state_fields_audited(state: LedgerState) {
    let LedgerState {
        utxo: _,
        certs: _,
        gov: _,
        consensus: _,
        epochs: _,
        tip: _,
        era: _,
        pending_era_transition: _,
        epoch: _,
        epoch_length: _,
        shelley_transition_epoch: _,
        byron_epoch_length: _,
        slot_config: _,
        genesis_hash: _,
        genesis_delegates: _,
        update_quorum: _,
        node_network: _,
        randomness_stabilisation_window: _,
        stability_window_3kf: _,
        security_param: _,
        conway_genesis_init: _,
        max_lovelace_supply: _,
        phase2_apply_horizon: _,
        cached_validation_registry: _,
    } = state;
}

// ─────────────────────────────────────────────────────────────────────────────
// Snapshot helpers (stubs for Task 1.4 / 1.5)
// ─────────────────────────────────────────────────────────────────────────────

/// Reasons a `LedgerSeq` operation can fail.
#[derive(Debug)]
pub enum LedgerSeqError {
    /// Rollback depth exceeds the volatile window.
    RollbackExceedsWindow { requested: usize, available: usize },
    /// The given point is not in the volatile window.
    PointNotFound,
}

impl std::fmt::Display for LedgerSeqError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerSeqError::RollbackExceedsWindow {
                requested,
                available,
            } => write!(
                f,
                "rollback depth {requested} exceeds volatile window ({available} available)"
            ),
            LedgerSeqError::PointNotFound => write!(f, "point not found in volatile window"),
        }
    }
}

impl std::error::Error for LedgerSeqError {}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LedgerState;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::{BlockNo, SlotNo};
    use dugite_primitives::value::Lovelace;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_anchor() -> LedgerState {
        LedgerState::new(ProtocolParameters::mainnet_defaults())
    }

    fn make_hash(b: u8) -> Hash32 {
        Hash32::from_bytes([b; 32])
    }

    /// Build a minimal delta that records some fee collection so the
    /// resulting state is distinguishable from the anchor.
    fn make_delta(slot: u64, hash_byte: u8, fees: u64) -> LedgerDelta {
        let mut delta = LedgerDelta::new(SlotNo(slot), make_hash(hash_byte), BlockNo(slot));
        delta.block_fields = BlockFieldsDelta {
            fees_collected: Lovelace(fees),
            epoch_fees: Lovelace(fees), // Running total in this simple test
            epoch_block_count: slot,
            evolving_nonce: make_hash(hash_byte),
            candidate_nonce: make_hash(hash_byte),
            lab_nonce: make_hash(hash_byte),
            pool_block_increment: None,
            pending_donations: Lovelace(0),
            era: Era::Conway,
            pending_avvm_return: 0,
            opcert_counter_update: None,
        };
        delta
    }

    /// Regression for the preprod ep292 `WithdrawalAmountMismatch` halt: a
    /// rollback must restore `reward_accounts` from the per-block snapshot, NOT
    /// inherit the stale anchor value. The `reward_changes`/`delegation_changes`
    /// delta vecs are never populated, so before the `*_snapshot` fix
    /// `apply_delta_to_state` left reward_accounts at the anchor — and
    /// `rollback_via_seq` therefore corrupted reward balances on a fork.
    #[test]
    fn rollback_restores_reward_accounts_from_snapshot_not_stale_anchor() {
        let cred = Hash32::from_bytes([0xAB; 32]);
        // Anchor: account holds 100 (its balance at the last snapshot).
        let mut anchor = make_anchor();
        anchor.certs.reward_accounts.insert(cred, Lovelace(100));
        let mut seq = LedgerSeq::with_defaults(anchor, 2160);

        // Three blocks mutate the balance directly: credit→150, withdraw→0,
        // credit→50. Each carries the post-block snapshot the live apply path
        // (`apply_block_with_delta`) captures.
        for (slot, bal) in [(10u64, 150u64), (20, 0), (30, 50)] {
            let mut d = make_delta(slot, slot as u8, 0);
            let mut ra = imbl::HashMap::new();
            ra.insert(cred, Lovelace(bal));
            d.reward_accounts_snapshot = Some(ra);
            seq.push(d);
        }

        // Tip reflects the latest snapshot (50), not the anchor's 100.
        assert_eq!(
            seq.tip_state().certs.reward_accounts.get(&cred).copied(),
            Some(Lovelace(50)),
            "tip must reflect the latest per-block reward_accounts snapshot"
        );

        // Roll back one block → the withdrawal block, balance 0. Before the fix
        // this returned the stale anchor value (100) — the ep292 corruption.
        seq.rollback(1);
        assert_eq!(
            seq.tip_state().certs.reward_accounts.get(&cred).copied(),
            Some(Lovelace(0)),
            "after rollback the balance must be the per-block snapshot (0, \
             withdrawn), NOT the stale anchor value (100)"
        );
    }

    /// Regression for #763: the per-pool block counter must be reconstructed
    /// from the absolute per-block snapshot, NOT only the `pool_block_increment`.
    ///
    /// The live node applies some blocks through NO-DELTA paths (gap-bridge
    /// advance in `handle_rollback_inner`, LSM/chunk replay, startup recovery):
    /// those mutate `self.consensus.epoch_blocks_by_pool` directly but push no
    /// `LedgerDelta`, leaving a hole in the delta chain. The subsequent delta's
    /// `pool_block_increment` only adds +1, so increment-only reconstruction
    /// under-counts `BlocksMade` by the number of hole blocks. That short count
    /// then drifts `eta = blocksMade/expectedBlocks` → expansion → reserves and
    /// per-account rewards (the bidirectional Koios drift; halted preview ep1335
    /// with `WithdrawalAmountMismatch` +26). The absolute snapshot is exact.
    #[test]
    fn reconstruct_block_counter_from_absolute_snapshot_not_just_increment() {
        let pool = Hash28::from_bytes([0xCD; 28]);

        // Anchor counted 5 blocks for this pool.
        let mut anchor = make_anchor();
        Arc::make_mut(&mut anchor.consensus.epoch_blocks_by_pool).insert(pool, 5);
        let mut seq = LedgerSeq::with_defaults(anchor, 2160);

        // A gap-bridge advanced the LIVE state to count 40 blocks for this pool,
        // but pushed NO deltas for the 34 hole blocks. The next real block (the
        // ONLY delta the seq sees) carries pool_block_increment=Some (it would
        // reconstruct to just 5+1=6) but ALSO the absolute post-block snapshot
        // {pool: 40} that the live apply path captures.
        let mut d = make_delta(10, 0x10, 0);
        d.block_fields.pool_block_increment = Some(pool);
        let mut counts = HashMap::new();
        counts.insert(pool, 40u64);
        d.epoch_blocks_by_pool_snapshot = Some(Arc::new(counts));
        seq.push(d);

        // Increment-only reconstruction would give 6 (the #763 under-count).
        // The absolute snapshot must win: 40.
        assert_eq!(
            seq.tip_state()
                .consensus
                .epoch_blocks_by_pool
                .get(&pool)
                .copied(),
            Some(40),
            "block counter must be restored from the absolute snapshot (40), \
             NOT the increment-only reconstruction (6) that loses no-delta \
             (gap-bridge/replay) blocks — #763"
        );
    }

    /// Deltas WITHOUT a snapshot (e.g. overlay/Byron blocks that counted
    /// nothing, or non-`apply_block_with_delta` fixtures) must fall back to the
    /// `pool_block_increment` reconstruction and carry the map forward.
    #[test]
    fn block_counter_increment_fallback_when_no_snapshot() {
        let pool = Hash28::from_bytes([0xEF; 28]);
        let mut anchor = make_anchor();
        Arc::make_mut(&mut anchor.consensus.epoch_blocks_by_pool).insert(pool, 7);
        let mut seq = LedgerSeq::with_defaults(anchor, 2160);

        // No snapshot, just an increment → 7 + 1 = 8.
        let mut d = make_delta(10, 0x10, 0);
        d.block_fields.pool_block_increment = Some(pool);
        seq.push(d);

        assert_eq!(
            seq.tip_state()
                .consensus
                .epoch_blocks_by_pool
                .get(&pool)
                .copied(),
            Some(8),
            "without a snapshot the increment fallback must apply (7+1=8)"
        );
    }

    /// Regression for #22/#481: a rollback must restore the governance substate
    /// (DReps, vote delegations, proposals) from the per-block snapshot, NOT the
    /// stale anchor. Before the `gov_snapshot` fix, `rollback_via_seq` reset
    /// `self.gov` to the anchor governance on every fork — wiping DReps so DRep
    /// voting power went to 0, real ParameterChanges never ratified, and the V3
    /// cost model stayed frozen at the genesis model (V3 script_data_hash
    /// divergence; `deposits_proposal: 0`).
    #[test]
    fn rollback_restores_gov_from_snapshot_not_stale_anchor() {
        // Anchor governance has proposal_count = 0 (a stand-in for "empty gov").
        let mut seq = LedgerSeq::with_defaults(make_anchor(), 2160);

        // Two blocks mutate governance: proposal_count 5 then 9 (the live apply
        // path captures gov post-block via `gov_snapshot`).
        for pc in [5u64, 9] {
            let mut d = make_delta(pc * 10, pc as u8, 0);
            let mut gov = make_anchor().gov;
            Arc::make_mut(&mut gov.governance).proposal_count = pc;
            d.gov_snapshot = Some(gov);
            seq.push(d);
        }

        assert_eq!(
            seq.tip_state().gov.governance.proposal_count,
            9,
            "tip must reflect the latest gov snapshot"
        );

        // Roll back one block → proposal_count 5 (the snapshot), not 0 (anchor).
        seq.rollback(1);
        assert_eq!(
            seq.tip_state().gov.governance.proposal_count,
            5,
            "after rollback the governance must be the per-block snapshot (5), \
             NOT the stale anchor (0) — the #22/#481 fork-wipe bug"
        );
    }

    /// Regression for the preprod ep57 `pool1n84mel6` stake short (#634 class):
    /// `apply_delta_to_state` → `apply_utxo_diff` must replay the incremental
    /// UTxO stake (Haskell `InstantStake`) onto `stake_distribution.stake_map`,
    /// not just the UTxO set. Before the fix it only mutated the UTxO set, so a
    /// reconstructed `tip_state()` (and therefore `rollback_via_seq`, which sets
    /// `self.certs = seq.tip_state().certs`) dropped every per-credential stake
    /// add accumulated since the anchor. A spend recorded as a `delete` would
    /// then subtract from a credential that was never re-credited — the exact
    /// add/subtract asymmetry that left two delegators short by exactly 5 ADA.
    #[test]
    fn apply_utxo_diff_replays_credential_stake_not_just_utxo_set() {
        use dugite_primitives::address::{Address, BaseAddress};
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::network::NetworkId;
        use dugite_primitives::transaction::{TransactionInput, TransactionOutput};
        use dugite_primitives::value::Value;

        // A base address with a (key) stake credential — routes to stake_map.
        let stake_cred = Credential::VerificationKey(Hash28::from_bytes([0x7d; 28]));
        let cred_key = stake_cred.to_typed_hash32();
        let addr = Address::Base(BaseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(Hash28::from_bytes([0x11; 28])),
            stake: stake_cred,
        });
        let mk_output = |lovelace: u64| TransactionOutput {
            address: addr.clone(),
            value: Value::lovelace(lovelace),
            datum: dugite_primitives::transaction::OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let mk_input = |b: u8| TransactionInput {
            transaction_id: make_hash(b),
            index: 0,
        };

        let mut state = make_anchor();

        // Delta 1: create a 5-ADA output for the credential (an `insert`).
        let mut d1 = make_delta(10, 10, 0);
        d1.utxo_diff
            .record_insert(mk_input(0xA0), mk_output(5_000_000));
        apply_delta_to_state(&mut state, &d1);

        assert_eq!(
            state
                .certs
                .stake_distribution
                .stake_map
                .get(&cred_key)
                .copied(),
            Some(Lovelace(5_000_000)),
            "apply_utxo_diff must ADD the new output's coin to the credential's \
             stake_map — not silently drop it (the ep57 −5-ADA bug)"
        );

        // Delta 2: spend that output (a `delete`, carrying the original output).
        let mut d2 = make_delta(20, 20, 0);
        d2.utxo_diff
            .record_delete(mk_input(0xA0), mk_output(5_000_000));
        apply_delta_to_state(&mut state, &d2);

        // Add/subtract is symmetric: stake returns to 0, never goes negative,
        // and the UTxO set is empty.
        assert_eq!(
            state
                .certs
                .stake_distribution
                .stake_map
                .get(&cred_key)
                .copied(),
            Some(Lovelace(0)),
            "apply_utxo_diff must SUBTRACT the spent output's coin symmetrically"
        );
        assert!(
            state.utxo.utxo_set.lookup(&mk_input(0xA0)).is_none(),
            "the spent UTxO must be removed from the set"
        );
    }

    // ── Construction ──────────────────────────────────────────────────────────

    #[test]
    fn test_new_empty() {
        let anchor = make_anchor();
        let seq = LedgerSeq::with_defaults(anchor, 10);
        assert!(seq.is_empty());
        assert_eq!(seq.len(), 0);
        assert_eq!(seq.max_rollback(), 0);
    }

    // ── Push ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_push_single_delta() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        seq.push(make_delta(1, 1, 1_000_000));

        assert_eq!(seq.len(), 1);
        assert_eq!(seq.max_rollback(), 1);
    }

    #[test]
    fn test_push_multiple_deltas() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        for i in 1u8..=5 {
            seq.push(make_delta(i as u64, i, i as u64 * 1_000_000));
        }

        assert_eq!(seq.len(), 5);
        assert_eq!(seq.max_rollback(), 5);
    }

    #[test]
    fn test_push_beyond_k_advances_anchor() {
        let anchor = make_anchor();
        let k = 5u64;
        let mut seq = LedgerSeq::with_defaults(anchor, k);

        // Push k+2 deltas — the anchor should advance twice.
        for i in 1u8..=(k as u8 + 2) {
            seq.push(make_delta(i as u64, i, i as u64 * 1_000_000));
        }

        // volatile window should be exactly k
        assert_eq!(seq.len(), k as usize);
        // anchor point should have advanced to delta[1] (0-indexed)
        assert!(matches!(
            seq.anchor_point(),
            dugite_primitives::block::Point::Specific(_, _)
        ));
    }

    // ── Rollback ──────────────────────────────────────────────────────────────

    #[test]
    fn test_rollback_zero_is_noop() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);
        seq.push(make_delta(1, 1, 1_000_000));
        seq.push(make_delta(2, 2, 2_000_000));

        seq.rollback(0);
        assert_eq!(seq.len(), 2);
    }

    #[test]
    fn test_rollback_one() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);
        seq.push(make_delta(1, 1, 1_000_000));
        seq.push(make_delta(2, 2, 2_000_000));

        seq.rollback(1);
        assert_eq!(seq.len(), 1);
        assert_eq!(seq.max_rollback(), 1);
    }

    #[test]
    fn test_rollback_all() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);
        for i in 1u8..=5 {
            seq.push(make_delta(i as u64, i, 1_000_000));
        }

        seq.rollback(5);
        assert!(seq.is_empty());
        assert_eq!(seq.max_rollback(), 0);
    }

    #[test]
    fn test_rollback_clamps_to_available() {
        // rollback(n > len) should not panic — it clamps.
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);
        seq.push(make_delta(1, 1, 0));
        seq.push(make_delta(2, 2, 0));

        seq.rollback(100); // more than available
        assert!(seq.is_empty());
    }

    #[test]
    fn test_rollback_invalidates_checkpoints() {
        let anchor = make_anchor();
        // checkpoint_interval = 3 → checkpoint at index 2 (deltas[0..=2])
        let mut seq = LedgerSeq::new(anchor, 20, 3);

        for i in 1u8..=6 {
            seq.push(make_delta(i as u64, i, 0));
        }
        // Checkpoints should exist at indices 2 and 5.
        assert!(seq.checkpoints.contains_key(&2));
        assert!(seq.checkpoints.contains_key(&5));

        // Roll back to len=3 (remove deltas 3,4,5).
        seq.rollback(3);
        assert_eq!(seq.len(), 3);
        // Checkpoint at index 5 should be gone; checkpoint at index 2 remains.
        assert!(!seq.checkpoints.contains_key(&5));
        assert!(seq.checkpoints.contains_key(&2));
    }

    // ── Advance anchor ────────────────────────────────────────────────────────

    #[test]
    fn test_advance_anchor_empty_is_noop() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);
        seq.advance_anchor(); // should not panic
        assert!(seq.is_empty());
    }

    #[test]
    fn test_advance_anchor_updates_anchor_state() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        // Push one delta that changes epoch_fees.
        seq.push(make_delta(1, 1, 5_000_000));

        let pre_advance_tip = seq.tip_state();
        assert_eq!(pre_advance_tip.utxo.epoch_fees.0, 5_000_000);

        seq.advance_anchor();
        assert!(seq.is_empty());

        // Anchor itself should now reflect the fee change.
        assert_eq!(seq.anchor.utxo.epoch_fees.0, 5_000_000);
    }

    #[test]
    fn test_advance_anchor_reindexes_checkpoints() {
        let anchor = make_anchor();
        // checkpoint_interval = 2 → checkpoint at index 1, 3.
        let mut seq = LedgerSeq::new(anchor, 20, 2);

        for i in 1u8..=4 {
            seq.push(make_delta(i as u64, i, 0));
        }
        // Checkpoints at indices 1 and 3.
        assert!(seq.checkpoints.contains_key(&1));
        assert!(seq.checkpoints.contains_key(&3));

        // Advance anchor once — oldest delta is consumed, indices shift by 1.
        seq.advance_anchor();
        assert_eq!(seq.len(), 3);

        // Checkpoint that was at index 1 is now at index 0;
        // checkpoint that was at index 3 is now at index 2.
        assert!(seq.checkpoints.contains_key(&0));
        assert!(seq.checkpoints.contains_key(&2));
        // Old indices gone.
        assert!(!seq.checkpoints.contains_key(&1));
        assert!(!seq.checkpoints.contains_key(&3));
    }

    // ── max_rollback boundary ─────────────────────────────────────────────────

    #[test]
    fn test_max_rollback_boundary() {
        let anchor = make_anchor();
        let k = 5u64;
        let mut seq = LedgerSeq::with_defaults(anchor, k);

        // Fill to exactly k.
        for i in 1u8..=(k as u8) {
            seq.push(make_delta(i as u64, i, 0));
        }
        assert_eq!(seq.max_rollback(), k as usize);

        // Push one more — anchor advances, len stays at k.
        seq.push(make_delta(k + 1, (k + 1) as u8, 0));
        assert_eq!(seq.max_rollback(), k as usize);
    }

    // ── State reconstruction ──────────────────────────────────────────────────

    #[test]
    fn test_tip_state_matches_sequential_application() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor.clone(), 100);

        // Push 5 deltas, each adding 1_000_000 to epoch_fees.
        // After 5 deltas, running epoch_fees in BlockFieldsDelta = 5_000_000.
        let mut running_fees = 0u64;
        for i in 1u8..=5 {
            running_fees += 1_000_000;
            let mut delta = make_delta(i as u64, i, 1_000_000);
            delta.block_fields.epoch_fees = Lovelace(running_fees);
            seq.push(delta);
        }

        let tip = seq.tip_state();
        assert_eq!(tip.utxo.epoch_fees.0, 5_000_000);
    }

    #[test]
    fn test_state_at_returns_correct_intermediate_state() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 100);

        let mut running = 0u64;
        for i in 1u8..=5 {
            running += 1_000_000;
            let mut delta = make_delta(i as u64, i, 1_000_000);
            delta.block_fields.epoch_fees = Lovelace(running);
            seq.push(delta);
        }

        // State at slot=3 (hash=[3;32]) should have epoch_fees = 3_000_000.
        let state = seq
            .state_at(SlotNo(3), &make_hash(3))
            .expect("slot 3 should be in window");
        assert_eq!(state.utxo.epoch_fees.0, 3_000_000);
    }

    #[test]
    fn test_state_at_returns_none_for_unknown_point() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 100);
        seq.push(make_delta(1, 1, 0));

        let result = seq.state_at(SlotNo(99), &make_hash(99));
        assert!(result.is_none());
    }

    // ── Checkpoint creation ───────────────────────────────────────────────────

    #[test]
    fn test_checkpoint_created_at_correct_interval() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::new(anchor, 200, 5);

        // No checkpoints before we hit the interval.
        for i in 1u8..=4 {
            seq.push(make_delta(i as u64, i, 0));
        }
        assert!(seq.checkpoints.is_empty());

        // 5th push → checkpoint at index 4.
        seq.push(make_delta(5, 5, 0));
        assert_eq!(seq.checkpoints.len(), 1);
        assert!(seq.checkpoints.contains_key(&4));
    }

    #[test]
    fn test_checkpoint_reconstruction_consistent_with_sequential() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::new(anchor, 200, 3);

        // Push 9 deltas; checkpoints at index 2, 5, 8.
        let mut running = 0u64;
        for i in 1u8..=9 {
            running += 1_000_000;
            let mut delta = make_delta(i as u64, i, 1_000_000);
            delta.block_fields.epoch_fees = Lovelace(running);
            seq.push(delta);
        }

        // Verify checkpoint at index 2 has epoch_fees = 3_000_000.
        let cp_state = seq.checkpoints.get(&2).expect("checkpoint at 2");
        assert_eq!(cp_state.utxo.epoch_fees.0, 3_000_000);

        // Verify checkpoint at index 5 has epoch_fees = 6_000_000.
        let cp_state = seq.checkpoints.get(&5).expect("checkpoint at 5");
        assert_eq!(cp_state.utxo.epoch_fees.0, 6_000_000);

        // Verify tip (index 8) has epoch_fees = 9_000_000.
        let tip = seq.tip_state();
        assert_eq!(tip.utxo.epoch_fees.0, 9_000_000);
    }

    // ── Push / rollback cycle ─────────────────────────────────────────────────

    #[test]
    fn test_push_rollback_reapply_cycle() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 20);

        // Push 5 deltas.
        let mut running = 0u64;
        for i in 1u8..=5 {
            running += 1_000_000;
            let mut delta = make_delta(i as u64, i, 1_000_000);
            delta.block_fields.epoch_fees = Lovelace(running);
            seq.push(delta);
        }
        assert_eq!(seq.tip_state().utxo.epoch_fees.0, 5_000_000);

        // Roll back 2.
        seq.rollback(2);
        assert_eq!(seq.len(), 3);
        assert_eq!(seq.tip_state().utxo.epoch_fees.0, 3_000_000);

        // Reapply 3 different deltas (fork scenario).
        let mut running = 3_000_000u64;
        for i in 10u8..=12 {
            running += 500_000;
            let mut delta = make_delta(i as u64, i, 500_000);
            delta.block_fields.epoch_fees = Lovelace(running);
            seq.push(delta);
        }
        assert_eq!(seq.len(), 6);
        assert_eq!(seq.tip_state().utxo.epoch_fees.0, 4_500_000);
    }

    // ── Reset anchor ──────────────────────────────────────────────────────────

    #[test]
    fn test_reset_anchor_clears_volatile() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);
        for i in 1u8..=5 {
            seq.push(make_delta(i as u64, i, 0));
        }
        assert!(!seq.is_empty());

        let new_anchor = make_anchor();
        seq.reset_anchor(new_anchor);
        assert!(seq.is_empty());
        assert!(seq.checkpoints.is_empty());
    }

    // ── Lightweight checkpoints (#432) ───────────────────────────────────────

    #[test]
    fn test_checkpoint_has_empty_utxo_set() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::new(anchor, 300, 5);
        for i in 1u8..=5 {
            seq.push(make_delta(i as u64, i, (i as u64) * 1_000_000));
        }
        // Checkpoint created at index 4 (5th delta, interval=5)
        assert!(seq.checkpoints.contains_key(&4));
        let cp = &seq.checkpoints[&4];
        assert_eq!(cp.utxo.utxo_set.len(), 0);
    }

    #[test]
    fn test_lightweight_tip_state_preserves_epoch_fees() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);
        seq.push(make_delta(1, 1, 5_000_000));
        seq.push(make_delta(2, 2, 3_000_000));

        let tip = seq.tip_state();
        assert_eq!(tip.utxo.epoch_fees.0, 3_000_000);
    }

    #[test]
    fn test_state_at_index_reconstructs_non_utxo_state() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        for i in 1u8..=5 {
            seq.push(make_delta(i as u64, i, (i as u64) * 100));
        }

        let state_2 = seq.state_at_index(2);
        assert_eq!(state_2.utxo.epoch_fees.0, 300);

        let state_4 = seq.state_at_index(4);
        assert_eq!(state_4.utxo.epoch_fees.0, 500);
    }

    #[test]
    fn test_rollback_and_reconstruct_non_utxo_state() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        for i in 1u8..=5 {
            seq.push(make_delta(i as u64 * 10, i, (i as u64) * 1_000));
        }
        assert_eq!(seq.len(), 5);

        seq.rollback(2);
        assert_eq!(seq.len(), 3);

        let tip = seq.tip_state();
        assert_eq!(tip.utxo.epoch_fees.0, 3_000);
    }

    #[test]
    fn test_advance_anchor_preserves_non_utxo_state() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::new(anchor, 5, 100);

        for i in 1u8..=6 {
            seq.push(make_delta(i as u64, i, (i as u64) * 100));
        }
        // k=5, pushing 6th should advance anchor once
        assert_eq!(seq.len(), 5);

        let anchor_state = seq.anchor_state();
        assert_eq!(anchor_state.utxo.epoch_fees.0, 100);
    }

    #[test]
    fn test_rollback_deep_to_anchor() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        for i in 1u8..=5 {
            seq.push(make_delta(i as u64, i, (i as u64) * 1_000));
        }

        seq.rollback(5);
        assert!(seq.is_empty());
        let tip = seq.tip_state();
        assert_eq!(tip.utxo.epoch_fees.0, 0);
    }

    #[test]
    fn test_rollback_then_reapply() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        for i in 1u8..=3 {
            seq.push(make_delta(i as u64, i, (i as u64) * 1_000));
        }
        seq.rollback(2);
        assert_eq!(seq.len(), 1);

        seq.push(make_delta(10, 10, 99_000));
        seq.push(make_delta(20, 20, 88_000));

        let tip = seq.tip_state();
        assert_eq!(tip.utxo.epoch_fees.0, 88_000);
        assert_eq!(seq.len(), 3);
    }

    // ── Issue #728: snapshot-based cert/gov reconstruction ───────────────────
    //
    // These tests exercise the real bug: before the `*_snapshot` fix, every call
    // to `apply_delta_to_state` left `reward_accounts`, `delegations`,
    // `pool_params`, and `gov` at the stale anchor value because the
    // `*_changes` delta vecs are never populated.  A fork rollback therefore
    // silently reset all cert/gov state to the anchor, corrupting reward
    // balances (preprod ep292 `WithdrawalAmountMismatch`), losing pool
    // registrations, and wiping DRep state.

    /// Reward account credit + withdrawal within the volatile window must be
    /// visible after a rollback that lands AFTER those changes (#728 regression).
    #[test]
    fn rollback_mid_window_restores_reward_accounts_exactly() {
        let cred_a = Hash32::from_bytes([0x01; 32]);
        let cred_b = Hash32::from_bytes([0x02; 32]);

        // Anchor: both accounts have zero balance.
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        // Block 1: credit cred_a to 1000.
        {
            let mut d = make_delta(1, 1, 0);
            let mut ra = imbl::HashMap::new();
            ra.insert(cred_a, Lovelace(1000));
            d.reward_accounts_snapshot = Some(ra);
            seq.push(d);
        }
        // Block 2: cred_a withdraws all (→ 0), cred_b credited 500.
        {
            let mut d = make_delta(2, 2, 0);
            let mut ra = imbl::HashMap::new();
            ra.insert(cred_a, Lovelace(0));
            ra.insert(cred_b, Lovelace(500));
            d.reward_accounts_snapshot = Some(ra);
            seq.push(d);
        }
        // Block 3: cred_b credited further to 700.
        {
            let mut d = make_delta(3, 3, 0);
            let mut ra = imbl::HashMap::new();
            ra.insert(cred_a, Lovelace(0));
            ra.insert(cred_b, Lovelace(700));
            d.reward_accounts_snapshot = Some(ra);
            seq.push(d);
        }

        // Tip: cred_a=0, cred_b=700.
        {
            let tip = seq.tip_state();
            assert_eq!(
                tip.certs.reward_accounts.get(&cred_a).copied(),
                Some(Lovelace(0)),
                "tip: cred_a must be 0 after withdrawal"
            );
            assert_eq!(
                tip.certs.reward_accounts.get(&cred_b).copied(),
                Some(Lovelace(700)),
                "tip: cred_b must be 700"
            );
        }

        // Roll back to block 1 (remove blocks 2 and 3).
        seq.rollback(2);
        {
            let tip = seq.tip_state();
            assert_eq!(
                tip.certs.reward_accounts.get(&cred_a).copied(),
                Some(Lovelace(1000)),
                "after rollback to block 1: cred_a must be 1000 (pre-withdrawal), \
                 NOT the stale anchor value 0 (the #728 ep292 bug)"
            );
            assert_eq!(
                tip.certs.reward_accounts.get(&cred_b).copied(),
                None,
                "after rollback to block 1: cred_b must not exist yet"
            );
        }
    }

    /// Delegation map changes within the volatile window must survive rollback.
    #[test]
    fn rollback_restores_delegations_from_snapshot() {
        let cred = Hash32::from_bytes([0xCC; 32]);
        let pool_a = Hash28::from_bytes([0xAA; 28]);
        let pool_b = Hash28::from_bytes([0xBB; 28]);

        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        // Block 1: cred delegates to pool_a.
        {
            let mut d = make_delta(10, 1, 0);
            let mut dl = imbl::HashMap::new();
            dl.insert(cred, pool_a);
            d.delegations_snapshot = Some(dl);
            seq.push(d);
        }
        // Block 2: cred re-delegates to pool_b.
        {
            let mut d = make_delta(20, 2, 0);
            let mut dl = imbl::HashMap::new();
            dl.insert(cred, pool_b);
            d.delegations_snapshot = Some(dl);
            seq.push(d);
        }

        assert_eq!(
            seq.tip_state().certs.delegations.get(&cred).copied(),
            Some(pool_b),
            "tip must show delegation to pool_b"
        );

        seq.rollback(1);
        assert_eq!(
            seq.tip_state().certs.delegations.get(&cred).copied(),
            Some(pool_a),
            "after rollback delegation must revert to pool_a, not the anchor (None)"
        );
    }

    /// Pool registration and retirement within the volatile window must survive
    /// rollback (#728: pool_changes never populated, pool_params not snapshotted).
    #[test]
    fn rollback_restores_pool_params_from_snapshot() {
        let pool_id = Hash28::from_bytes([0xDE; 28]);
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        // Helper to build a minimal PoolRegistration for tests.
        fn make_pool_reg(pool_id: Hash28) -> PoolRegistration {
            PoolRegistration {
                pool_id,
                vrf_keyhash: Hash32::from_bytes([0x01; 32]),
                pledge: Lovelace(1_000_000_000),
                cost: Lovelace(340_000_000),
                margin_numerator: 1,
                margin_denominator: 20,
                reward_account: vec![0u8; 29],
                owners: Vec::new(),
                relays: Vec::new(),
                metadata_url: None,
                metadata_hash: None,
            }
        }

        // Block 1: pool registered (pool_params_snapshot captures it).
        {
            let mut d = make_delta(10, 1, 0);
            let mut pp = HashMap::new();
            pp.insert(pool_id, make_pool_reg(pool_id));
            d.pool_params_snapshot = Some(Arc::new(pp));
            d.future_pool_params_snapshot = Some(HashMap::new());
            d.pending_retirements_snapshot = Some(HashMap::new());
            d.pool_deposits_snapshot = Some(HashMap::new());
            seq.push(d);
        }
        // Block 2: pool scheduled for retirement.
        {
            let mut d = make_delta(20, 2, 0);
            let mut pp = HashMap::new();
            pp.insert(pool_id, make_pool_reg(pool_id));
            d.pool_params_snapshot = Some(Arc::new(pp));
            d.future_pool_params_snapshot = Some(HashMap::new());
            let mut pr = HashMap::new();
            pr.insert(pool_id, EpochNo(99));
            d.pending_retirements_snapshot = Some(pr);
            d.pool_deposits_snapshot = Some(HashMap::new());
            seq.push(d);
        }

        // Tip: pool exists in pool_params AND has a pending retirement.
        {
            let tip = seq.tip_state();
            assert!(
                tip.certs.pool_params.contains_key(&pool_id),
                "tip must contain the registered pool"
            );
            assert_eq!(
                tip.certs.pending_retirements.get(&pool_id).map(|e| e.0),
                Some(99),
                "tip must have the pending retirement at epoch 99"
            );
        }

        // Roll back block 2 (retirement block).
        seq.rollback(1);
        {
            let tip = seq.tip_state();
            assert!(
                tip.certs.pool_params.contains_key(&pool_id),
                "after rollback pool must still be registered (block 1 snapshot)"
            );
            assert!(
                !tip.certs.pending_retirements.contains_key(&pool_id),
                "after rollback the retirement must be gone (not in block 1 snapshot); \
                 stale anchor would have also had no retirement, but this confirms the \
                 block-1 snapshot is used, not the empty anchor"
            );
        }

        // Roll back block 1 (registration block) — tip is now the anchor.
        seq.rollback(1);
        {
            let tip = seq.tip_state();
            assert!(
                !tip.certs.pool_params.contains_key(&pool_id),
                "after full rollback pool must not exist (anchor had no pool)"
            );
        }
    }

    /// Governance state changes within the volatile window must survive rollback
    /// including a mid-window rollback that lands between two governance mutations.
    #[test]
    fn rollback_mid_window_restores_governance_exactly() {
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        // Block 1: proposal_count = 3.
        {
            let mut d = make_delta(10, 1, 0);
            let mut gov = make_anchor().gov;
            std::sync::Arc::make_mut(&mut gov.governance).proposal_count = 3;
            d.gov_snapshot = Some(gov);
            seq.push(d);
        }
        // Block 2: no gov change (no gov_snapshot).
        seq.push(make_delta(20, 2, 0));

        // Block 3: proposal_count = 7.
        {
            let mut d = make_delta(30, 3, 0);
            let mut gov = make_anchor().gov;
            std::sync::Arc::make_mut(&mut gov.governance).proposal_count = 7;
            d.gov_snapshot = Some(gov);
            seq.push(d);
        }

        assert_eq!(
            seq.tip_state().gov.governance.proposal_count,
            7,
            "tip must have proposal_count 7"
        );

        // Roll back block 3: tip is block 2 (no gov change) — gov should carry
        // forward from the most recent snapshot, which is block 1's snapshot (3).
        seq.rollback(1);
        assert_eq!(
            seq.tip_state().gov.governance.proposal_count,
            3,
            "after rollback of block 3, gov must revert to block 1's snapshot value (3), \
             not the stale anchor (0)"
        );
    }

    /// A stake_key_deposits change within the volatile window must survive rollback.
    #[test]
    fn rollback_restores_stake_key_deposits_from_snapshot() {
        let cred = Hash32::from_bytes([0xD1; 32]);
        let anchor = make_anchor();
        let mut seq = LedgerSeq::with_defaults(anchor, 10);

        // Block 1: register cred (deposit = 2_000_000).
        {
            let mut d = make_delta(5, 1, 0);
            let mut sk = imbl::HashMap::new();
            sk.insert(cred, 2_000_000u64);
            d.stake_key_deposits_snapshot = Some(sk);
            seq.push(d);
        }
        // Block 2: deregister cred (deposit = 0, entry removed).
        {
            let mut d = make_delta(10, 2, 0);
            d.stake_key_deposits_snapshot = Some(imbl::HashMap::new());
            seq.push(d);
        }

        assert_eq!(
            seq.tip_state().certs.stake_key_deposits.get(&cred).copied(),
            None,
            "tip: deposit must be gone after deregistration"
        );

        seq.rollback(1);
        assert_eq!(
            seq.tip_state().certs.stake_key_deposits.get(&cred).copied(),
            Some(2_000_000),
            "after rollback deposit must be restored from block 1 snapshot"
        );
    }

    /// `advance_anchor` must carry snapshot state into the anchor correctly so
    /// that the anchor itself reflects all cert changes from consumed deltas.
    #[test]
    fn advance_anchor_carries_reward_accounts_forward() {
        let cred = Hash32::from_bytes([0xE1; 32]);
        let anchor = make_anchor();
        let k = 3u64;
        let mut seq = LedgerSeq::with_defaults(anchor, k);

        // Push k+1 deltas — the first is consumed into the anchor via advance_anchor.
        // Delta 0 (slot 1): credit cred to 500.
        {
            let mut d = make_delta(1, 1, 0);
            let mut ra = imbl::HashMap::new();
            ra.insert(cred, Lovelace(500));
            d.reward_accounts_snapshot = Some(ra);
            seq.push(d);
        }
        // Delta 1 (slot 2): credit cred to 800.
        {
            let mut d = make_delta(2, 2, 0);
            let mut ra = imbl::HashMap::new();
            ra.insert(cred, Lovelace(800));
            d.reward_accounts_snapshot = Some(ra);
            seq.push(d);
        }
        // Delta 2 (slot 3): cred unchanged (no snapshot).
        seq.push(make_delta(3, 3, 0));
        // Delta 3 (slot 4): triggers advance_anchor (consumes delta 0).
        {
            let mut d = make_delta(4, 4, 0);
            let mut ra = imbl::HashMap::new();
            ra.insert(cred, Lovelace(800));
            d.reward_accounts_snapshot = Some(ra);
            seq.push(d);
        }

        // After advance_anchor, the anchor should reflect delta 0's reward
        // accounts (500). The volatile window holds deltas 1-3.
        assert_eq!(
            seq.anchor_state().certs.reward_accounts.get(&cred).copied(),
            Some(Lovelace(500)),
            "anchor must reflect the consumed delta's reward_accounts snapshot"
        );

        // tip_state must reflect the newest snapshot: 800.
        assert_eq!(
            seq.tip_state().certs.reward_accounts.get(&cred).copied(),
            Some(Lovelace(800)),
            "tip must reflect the most recent reward_accounts snapshot (800)"
        );
    }
}
