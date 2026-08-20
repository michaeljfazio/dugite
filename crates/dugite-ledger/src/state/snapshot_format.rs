//! Flat wire format for LedgerState snapshot serialization.
//!
//! `LedgerStateSnapshot` is the bincode wire view of the runtime
//! `LedgerState`.  Bincode is positional: it encodes/decodes fields in
//! declaration order with no field names.  Any change to the struct's
//! field types or ordering is a breaking format change — bump
//! `LedgerState::SNAPSHOT_VERSION` so the loader rejects the old format
//! and operators re-sync from chain.
//!
//! There is no migration path between versions.  Pre-1.0 dugite makes no
//! snapshot back-compat guarantee.
//!
//! # Map/set ordering (#1088)
//!
//! Bincode writes a map or set field in whatever order its type iterates.
//! `std::collections::HashMap`/`HashSet` and `imbl::HashMap`/`HashSet` both
//! default to a randomized hasher, so for any collection with 2+ entries two
//! processes holding byte-for-byte identical logical state serialize
//! DIFFERENT bytes — and the same process can differ across two runs.  Every
//! collection reachable from [`LedgerStateSnapshot`] is therefore either
//! already backed by an inherently-ordered type (`BTreeMap`/`BTreeSet`,
//! `imbl::OrdMap`/`OrdSet`) or is converted to one AT THE SERIALIZATION
//! BOUNDARY — i.e. inside this module's `From` impls, never by changing the
//! live `LedgerState` field's own type.
//!
//! `LedgerState`'s live collections stay `HashMap`/`imbl::HashMap` on
//! purpose: the ordering fix costs nothing per block (the conversion runs
//! once, at snapshot-write time, on data that is already being cloned), while
//! switching the LIVE per-block hot-path types to an ordered container would
//! cost real throughput (`imbl::HashMap`'s HAMT is ~log32(N) deep vs.
//! `OrdMap`'s B-tree at ~log2(N), and `reward_accounts` alone is ~784K
//! entries on mainnet) for a property only the WIRE format needs. Two of the
//! affected types (`GovernanceState`, `StakeSnapshot`, `NonMyopic`,
//! `PendingRewardUpdate`, …) are shared between live state and the snapshot,
//! so a `*Wire` mirror struct exists for each: field-for-field identical
//! except every `HashMap`/`HashSet` becomes a `BTreeMap`/`BTreeSet`.  A field
//! already backed by `imbl::OrdMap`/`OrdSet` (`proposals`, `votes_by_action`,
//! `drep_expiry`, the `PRoot`/`PEdges` trees, …) is left untouched — it
//! already iterates in key order.

use imbl::HashMap as ImblHashMap;
use imbl::HashSet as ImblHashSet;
use imbl::OrdMap as ImblOrdMap;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use dugite_primitives::credentials::{Credential, Pointer};
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{
    Anchor, Constitution, DRep, GovActionId, ProtocolParamUpdate, Rational, Voter, VotingProcedure,
};
use dugite_primitives::value::Lovelace;
use serde::{Deserialize, Serialize};

use super::{
    DRepPulsingState, DRepRegistration, EnactedGovTerms, EpochSnapshots, GovRelation,
    GovernanceState, Likelihood, NonMyopic, PEdges, PGraph, PendingRewardUpdate, PoolRegistration,
    ProposalState, PulsedRatifyState, PulsingSnapshot, StakeDistributionState, StakeSnapshot,
};
use crate::plutus::SlotConfig;
use crate::utxo::UtxoSet;
use crate::utxo_diff::DiffSeq;
use dugite_primitives::block::Tip;
use dugite_primitives::era::Era;

/// Bincode wire view of `LedgerState`.
///
/// Field order matters — bincode is positional.  Any structural change to
/// this struct must be accompanied by a `LedgerState::SNAPSHOT_VERSION`
/// bump so the loader rejects out-of-date on-disk snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerStateSnapshot {
    /// Current UTxO set
    pub utxo_set: UtxoSet,
    /// Current tip of the chain
    pub tip: Tip,
    /// Current era
    pub era: Era,
    /// Pending era transition detected from the block stream.
    #[serde(skip, default)]
    pub pending_era_transition: Option<(Era, Era, EpochNo)>,
    /// Current epoch
    pub epoch: EpochNo,
    /// Shelley epoch length in slots
    pub epoch_length: u64,
    /// Number of Byron epochs before the Shelley hard fork.
    pub shelley_transition_epoch: u64,
    /// Byron epoch length in slots (10 * k). 0 = mainnet default (21600).
    pub byron_epoch_length: u64,
    /// Current protocol parameters (curPParams in Haskell).
    pub protocol_params: ProtocolParameters,
    /// Previous epoch's protocol parameters (Haskell's prevPParams).
    pub prev_protocol_params: ProtocolParameters,
    /// Decentralisation parameter from the previous epoch boundary.
    ///
    /// Stored as an exact `Rational` (was `f64` prior to v17 / issue #629).
    /// Required for byte-exact Haskell reward calc on mainnet, where
    /// `d ∈ {0, 0.05, …, 1.0}` cannot be represented losslessly in
    /// IEEE-754 binary64.
    pub prev_d: Rational,
    /// Protocol major version captured from the previous epoch boundary
    /// (Haskell's `prevPParams ^. ppProtocolVersionL.protocolVersionMajor`).
    pub prev_protocol_version_major: u64,
    /// Stake distribution
    pub stake_distribution: StakeDistributionStateWire,
    /// Treasury balance
    pub treasury: Lovelace,
    /// Pending treasury donations (Conway `TreasuryDonation`).
    pub pending_donations: Lovelace,
    /// Reserves balance (ADA not yet in circulation)
    pub reserves: Lovelace,
    /// Delegation state: credential_hash -> pool_id
    ///
    /// **OWNED, not Arc-shared.**  Earlier versions stored
    /// `Arc<HashMap<…>>` here, which made the snapshot view share the
    /// live LedgerState's Arc.  During the ~0.5–1 s window between
    /// view-build and snapshot-file-write completion, every block apply
    /// that touched this map via `Arc::make_mut` paid a CoW deep-clone
    /// of the entire ~100 k-entry HashMap because refcount was 2.
    /// Across the six Arc-shared maps the per-block cost climbed to
    /// ~460 ms steady-state once the chain reached epoch 25-ish on
    /// preview — observed as the "rate collapse at block 119k" symptom
    /// across multiple runs.  See #702.
    ///
    /// `BTreeMap`, not `HashMap` (#1088): bincode writes a map in
    /// iteration order, and a `HashMap`/`imbl::HashMap` reserialised after
    /// a round trip can order differently — two nodes with identical
    /// state would write different bytes.
    pub delegations: BTreeMap<Hash32, Hash28>,
    /// Pool registrations: pool_id -> pool registration
    pub pool_params: BTreeMap<Hash28, PoolRegistration>,
    /// Future pool parameters for re-registrations.
    pub future_pool_params: BTreeMap<Hash28, PoolRegistration>,
    /// Pool retirements pending: pool -> retirement epoch.
    pub pending_retirements: BTreeMap<Hash28, EpochNo>,
    /// `psVRFKeyHashes` — occurrences per pool VRF key hash (#1085).
    ///
    /// Persisted rather than recomputed on load: it is NOT a function of the
    /// pool set. See `CertSubState::vrf_key_hashes` for the counter-example
    /// (POOLREAP deletes a superseded key outright even when another pool still
    /// holds it, so a derived count and upstream's map disagree afterwards).
    /// `BTreeMap`, not `HashMap`: bincode writes a map in iteration order, and
    /// a `HashMap` reserialised after a round trip can order differently — the
    /// snapshot round-trip determinism guard catches exactly that.
    pub vrf_key_hashes: BTreeMap<Hash32, u64>,
    /// Stake snapshots for the Cardano "mark/set/go" snapshot model
    pub snapshots: EpochSnapshotsWire,
    /// Reward accounts: stake credential hash -> accumulated rewards.
    /// **OWNED, not Arc-shared** — see comment on `delegations`.
    pub reward_accounts: BTreeMap<Hash32, Lovelace>,
    /// Pointer map: certificate pointers -> credential hashes.
    pub pointer_map: BTreeMap<Pointer, Hash32>,
    /// Genesis delegates: genesis_key_hash -> (delegate_key_hash, vrf_key_hash).
    pub genesis_delegates: BTreeMap<Hash28, (Hash28, Hash32)>,
    /// Pending (not-yet-matured) genesis-delegate changes: `(maturity_slot,
    /// genesis_key_hash)` -> `(delegate_key_hash, vrf_key_hash)`. Haskell
    /// `dsFutureGenDelegs`. New in v28 (#804) — MUST be persisted: it is
    /// historical bootstrap-era queue state that cannot be reconstructed
    /// from the restored tip.
    pub future_gen_delegs: BTreeMap<(u64, Hash28), (Hash28, Hash32)>,
    /// Byron's `UPI.State` + `DI.State` (issue #1084). Already backed
    /// end-to-end by `BTreeMap`/`BTreeSet` on the LIVE type (no `*Wire`
    /// mirror needed — see `crate::eras::byron::ByronSubState`), so this is
    /// a direct clone, not a re-collect. Default-empty (and therefore
    /// zero-byte on the wire — bincode writes nothing for an empty
    /// collection) for any network with no Byron era.
    pub byron: crate::eras::byron::ByronSubState,
    /// Fees collected in the current epoch
    pub epoch_fees: Lovelace,
    /// Number of blocks produced by each pool in the current epoch.
    /// **OWNED, not Arc-shared** — see comment on `delegations`.
    pub epoch_blocks_by_pool: BTreeMap<Hash28, u64>,
    /// Total blocks in the current epoch
    pub epoch_block_count: u64,
    /// Evolving nonce (eta_v): accumulated hash of ALL VRF outputs.
    pub evolving_nonce: Hash32,
    /// Candidate nonce: snapshot of evolving_nonce that freezes late in each epoch.
    pub candidate_nonce: Hash32,
    /// Current epoch nonce.
    pub epoch_nonce: Hash32,
    /// Epoch nonce as it stood before the most recent epoch rotation
    /// (`praosStatePreviousEpochNonce`). Added in SNAPSHOT_VERSION 30 (#902).
    pub previous_epoch_nonce: Hash32,
    /// LAB nonce: prev_hash of the most recent block.
    pub lab_nonce: Hash32,
    /// Snapshot of lab_nonce at epoch boundary.
    pub last_epoch_block_nonce: Hash32,
    /// Active extra entropy (Shelley `ppExtraEntropy`); ZERO = NeutralNonce.
    /// New field — SNAPSHOT_VERSION bumped so pre-existing snapshots are
    /// rejected (bincode is positional; old data cannot supply this field).
    pub extra_entropy: Hash32,
    /// Randomness stabilisation window: ceiling(4k/f).
    pub randomness_stabilisation_window: u64,
    /// Stability window: ceiling(3k/f).
    pub stability_window_3kf: u64,
    /// Shelley genesis hash (used for initial nonce state)
    pub genesis_hash: Hash32,
    rolling_nonce: Hash32,
    stability_window: u64,
    first_block_hash_of_epoch: Option<Hash32>,
    prev_epoch_first_block_hash: Option<Hash32>,
    /// Current protocol parameter update proposals (pre-Conway).
    pub pending_pp_updates: BTreeMap<EpochNo, Vec<(Hash32, ProtocolParamUpdate)>>,
    /// Future protocol parameter update proposals (pre-Conway).
    pub future_pp_updates: BTreeMap<EpochNo, Vec<(Hash32, ProtocolParamUpdate)>>,
    /// Quorum for pre-Conway protocol parameter updates.
    pub update_quorum: u64,
    /// Conway governance state.
    /// **OWNED, not Arc-shared** — see comment on `delegations`.
    pub governance: GovernanceStateWire,
    /// Slot configuration for Plutus time conversion
    pub slot_config: SlotConfig,
    /// Whether stake distribution needs a full rebuild after snapshot load.
    #[serde(skip)]
    pub needs_stake_rebuild: bool,
    /// Pointer-addressed UTxO stake: pointer -> coin amount.
    pub ptr_stake: BTreeMap<Pointer, u64>,
    /// Whether pointer-addressed UTxO stake has been excluded from stake_distribution.
    ///
    /// Persisted across snapshot reloads because the value is set ONCE at
    /// the Babbage→Conway era transition (`ConwayRules::on_era_transition`)
    /// and gates the live `stake_routing` decision for every subsequent
    /// block apply. Issue #670: previously this field carried
    /// `#[serde(skip)]` and silently reverted to `false` on every snapshot
    /// reload, which mis-routed pointer-addressed UTxO stake into
    /// `epochs.ptr_stake` (Haskell `ConwayInstantStake` carries no
    /// `sisPtrStake` field) — a divergence on the verify-ledger-snapshot
    /// gate against the ancillary import.
    pub ptr_stake_excluded: bool,
    /// Pending reward update (drained at the next epoch boundary).
    pub pending_reward_update: Option<PendingRewardUpdateWire>,
    /// `EpochState.esNonMyopic` — per-pool `Likelihood` history plus the frozen
    /// reward pot (#1067).
    ///
    /// Persisted because it CANNOT be reconstructed: the likelihoods are a
    /// 0.9-decayed accumulator folded over every past epoch, and the reward pot
    /// is `_R` from a boundary whose inputs (reserves, fees, eta) have since
    /// moved. A node resuming without it would report values that only converge
    /// over ~20 epochs — which is precisely why adding it forces
    /// `SNAPSHOT_VERSION` 37 → 38 rather than a lazy backfill.
    pub non_myopic: NonMyopicWire,
    /// Running total of all stake key deposits locked in the ledger (lovelace).
    pub total_stake_key_deposits: u64,
    /// Script-type stake credentials.
    pub script_stake_credentials: BTreeSet<Hash32>,
    /// Pending MIR reserves-sourced reward deltas (Haskell
    /// `dsIRewards . irwdSrcReserves`; see issue #631).
    pub pending_mir_reserves: BTreeMap<Hash32, i128>,
    /// Pending MIR treasury-sourced reward deltas (Haskell
    /// `dsIRewards . irwdSrcTreasury`).
    pub pending_mir_treasury: BTreeMap<Hash32, i128>,
    /// Pending pot-to-pot accumulator: reserves drained at the next epoch
    /// boundary and routed to treasury (Haskell
    /// `dsIRewards . deltaReserves`).
    pub pending_mir_delta_reserves: i128,
    /// Pending pot-to-pot accumulator: treasury drained at the next epoch
    /// boundary and routed to reserves (Haskell
    /// `dsIRewards . deltaTreasury`).
    pub pending_mir_delta_treasury: i128,
    /// Per-block UTxO diffs for the last k blocks.
    #[serde(skip)]
    pub diff_seq: DiffSeq,
    /// The network this node is running on.
    #[serde(skip)]
    pub node_network: Option<dugite_primitives::network::NetworkId>,
    /// Operational certificate counters per pool.
    pub opcert_counters: BTreeMap<Hash28, u64>,
    /// Per-credential deposit paid at stake key registration time (lovelace).
    /// **OWNED, not Arc-shared** — see comment on `delegations`.
    pub stake_key_deposits: BTreeMap<Hash32, u64>,
    /// Per-pool deposit paid at pool registration time (lovelace).
    pub pool_deposits: BTreeMap<Hash28, u64>,
    /// #736: the pv≤6 RUPD startStep capture — the registered
    /// reward-account credential set frozen when the epoch's slot crossed
    /// `epoch_first_slot + 4k/f` (Haskell RUPD pulser `fvAddrsRew`).
    /// MUST be persisted: it is historical state that cannot be
    /// reconstructed from the restored tip. Before v22 this was dropped on
    /// load and lazily re-captured at the restored tip's slot, so any
    /// mid-epoch restart past the 4k/f mark in an Alonzo-or-earlier epoch
    /// computed the next boundary's rewards with a LATE credential set:
    /// rewards of window-deregistered accounts were never computed (left
    /// in reserves instead of routed to treasury → constant treasury
    /// shortfall) and window-re-registered old credentials were paid from
    /// the GO snapshot (stale reward balances). Observed live as the
    /// ~2998 ADA replay-seam offset at mainnet boundary 337→338.
    pub rupd_addrs_rew: Option<BTreeSet<Hash32>>,

    /// Whether a RUPD pulser exists for the epoch in progress (#1072).
    ///
    /// PERSISTED deliberately. A mid-epoch restart past the `4k/f` mark that
    /// dropped this would re-derive `SNothing` at the next boundary and skip a
    /// reward update Haskell applies — the same class of bug the
    /// `rupd_addrs_rew` doc records for mainnet 337→338, but affecting whether
    /// the update happens at all rather than how it is routed.
    pub rupd_pulser_started: bool,

    /// The 4k/f-frozen monetary step (Phase 1a). PERSISTED for the same reason
    /// as `rupd_pulser_started`: a mid-epoch restart past the mark that dropped
    /// it would re-derive the values from boundary-time state, silently
    /// reintroducing the accidental-correctness this freeze removes.
    pub rupd_monetary: Option<super::reward_pulser::MonetaryStep>,
    /// The WIRE-ONLY `nesRu` mirror (#1071) — see `EpochSubState::rupd_snapshot`
    /// for why it is a field separate from the pair above. PERSISTED for the
    /// same reason: a mid-epoch restart that dropped it would report `SNothing`
    /// on the N2C wire for the rest of the epoch even though a real pulser (per
    /// `rupd_pulser_started`) exists — a wire regression, not a consensus one,
    /// but still the #979 "confidently wrong" shape.
    pub rupd_snapshot: Option<PulsingRewUpdateWire>,
    /// #736 (same class): AVVM return amount pending at the next epoch
    /// boundary (Shelley→Allegra transition). Set once at the era
    /// transition and consumed at the following boundary; a mid-epoch
    /// restart inside that window must not lose it.
    pub pending_avvm_return: u64,
}

// ── From conversions for snapshot roundtrip ─────────────────────────

impl From<&super::LedgerState> for LedgerStateSnapshot {
    fn from(s: &super::LedgerState) -> Self {
        LedgerStateSnapshot {
            // UTxO sub-state
            utxo_set: s.utxo.utxo_set.clone(),
            diff_seq: s.utxo.diff_seq.clone(),
            epoch_fees: s.utxo.epoch_fees,
            pending_donations: s.utxo.pending_donations,
            // Cert sub-state.
            //
            // Deep-clone the Arc-shared HashMaps so the snapshot view
            // **owns** its data — the source Arc's refcount stays at 1
            // and the live LedgerState's subsequent `Arc::make_mut`
            // calls in `apply_delta_to_state` stay O(1) in-place
            // mutations.  Previously these were `Arc::clone` (shared),
            // which forced every block apply during the
            // ~0.5-1 s snapshot I/O window to CoW-deep-clone the
            // whole HashMap (10-30 MB each × 6 maps), producing the
            // 460 ms per-block ceiling at epoch 25+.
            //
            // Memory cost: one-time deep clone at view-build time.
            // At preview's ~100 k delegation entries that's ~10 MB
            // per map = ~60 MB total transient (held inside
            // `spawn_blocking` until file write completes), then dropped.
            // Negligible compared to the per-block savings.
            //
            // Every map/set here also moves from a hash-ordered container
            // to a key-ordered one (#1088) — the same one-time clone, just
            // collected into `BTreeMap`/`BTreeSet` instead.
            delegations: s.certs.delegations.iter().map(|(k, v)| (*k, *v)).collect(),
            pool_params: s
                .certs
                .pool_params
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            future_pool_params: s
                .certs
                .future_pool_params
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            pending_retirements: s
                .certs
                .pending_retirements
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            vrf_key_hashes: s
                .certs
                .vrf_key_hashes
                .iter()
                .map(|(h, c)| (*h, *c))
                .collect(),
            reward_accounts: s
                .certs
                .reward_accounts
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            stake_key_deposits: s
                .certs
                .stake_key_deposits
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            pool_deposits: s
                .certs
                .pool_deposits
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            total_stake_key_deposits: s.certs.total_stake_key_deposits,
            pointer_map: s.certs.pointer_map.iter().map(|(k, v)| (*k, *v)).collect(),
            stake_distribution: StakeDistributionStateWire::from(&s.certs.stake_distribution),
            script_stake_credentials: s.certs.script_stake_credentials.iter().copied().collect(),
            pending_mir_reserves: s
                .certs
                .pending_mir_reserves
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            pending_mir_treasury: s
                .certs
                .pending_mir_treasury
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            pending_mir_delta_reserves: s.certs.pending_mir_delta_reserves,
            pending_mir_delta_treasury: s.certs.pending_mir_delta_treasury,
            // Gov sub-state — owned, see comment above.
            governance: GovernanceStateWire::from(&*s.gov.governance),
            // Consensus sub-state
            evolving_nonce: s.consensus.evolving_nonce,
            candidate_nonce: s.consensus.candidate_nonce,
            epoch_nonce: s.consensus.epoch_nonce,
            previous_epoch_nonce: s.consensus.previous_epoch_nonce,
            lab_nonce: s.consensus.lab_nonce,
            last_epoch_block_nonce: s.consensus.last_epoch_block_nonce,
            extra_entropy: s.consensus.extra_entropy,
            rolling_nonce: s.consensus.rolling_nonce,
            first_block_hash_of_epoch: s.consensus.first_block_hash_of_epoch,
            prev_epoch_first_block_hash: s.consensus.prev_epoch_first_block_hash,
            // Owned, see comment on `delegations`.
            epoch_blocks_by_pool: s
                .consensus
                .epoch_blocks_by_pool
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            epoch_block_count: s.consensus.epoch_block_count,
            opcert_counters: s
                .consensus
                .opcert_counters
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            // Epoch sub-state
            snapshots: EpochSnapshotsWire::from(&s.epochs.snapshots),
            treasury: s.epochs.treasury,
            reserves: s.epochs.reserves,
            pending_reward_update: s
                .epochs
                .pending_reward_update
                .as_ref()
                .map(PendingRewardUpdateWire::from),
            non_myopic: NonMyopicWire::from(&s.epochs.non_myopic),
            pending_pp_updates: s.epochs.pending_pp_updates.clone(),
            future_pp_updates: s.epochs.future_pp_updates.clone(),
            needs_stake_rebuild: s.epochs.needs_stake_rebuild,
            ptr_stake: s.epochs.ptr_stake.iter().map(|(k, v)| (*k, *v)).collect(),
            ptr_stake_excluded: s.epochs.ptr_stake_excluded,
            protocol_params: s.epochs.protocol_params.clone(),
            prev_protocol_params: s.epochs.prev_protocol_params.clone(),
            prev_protocol_version_major: s.epochs.prev_protocol_version_major,
            prev_d: s.epochs.prev_d.clone(),
            // Coordination fields
            tip: s.tip.clone(),
            era: s.era,
            pending_era_transition: s.pending_era_transition,
            epoch: s.epoch,
            epoch_length: s.epoch_length,
            shelley_transition_epoch: s.shelley_transition_epoch,
            byron_epoch_length: s.byron_epoch_length,
            slot_config: s.slot_config,
            genesis_hash: s.genesis_hash,
            genesis_delegates: s.genesis_delegates.iter().map(|(k, v)| (*k, *v)).collect(),
            future_gen_delegs: s.future_gen_delegs.iter().map(|(k, v)| (*k, *v)).collect(),
            byron: s.byron.clone(),
            update_quorum: s.update_quorum,
            node_network: s.node_network,
            randomness_stabilisation_window: s.randomness_stabilisation_window,
            stability_window_3kf: s.stability_window_3kf,
            // Legacy field (always zero for new snapshots)
            stability_window: 0,
            // #736: persist the pv≤6 RUPD startStep capture — historical
            // state that a restart cannot re-derive.
            rupd_addrs_rew: s
                .epochs
                .rupd_addrs_rew
                .as_deref()
                .map(|set| set.iter().copied().collect()),
            rupd_pulser_started: s.epochs.rupd_pulser_started,
            rupd_monetary: s.epochs.rupd_monetary,
            rupd_snapshot: s
                .epochs
                .rupd_snapshot
                .as_ref()
                .map(PulsingRewUpdateWire::from),
            pending_avvm_return: s.epochs.pending_avvm_return,
        }
    }
}

impl From<LedgerStateSnapshot> for super::LedgerState {
    fn from(s: LedgerStateSnapshot) -> Self {
        use super::substates::*;

        super::LedgerState {
            utxo: UtxoSubState {
                utxo_set: s.utxo_set,
                diff_seq: s.diff_seq,
                epoch_fees: s.epoch_fees,
                pending_donations: s.pending_donations,
            },
            certs: CertSubState {
                // BTreeMap → imbl::HashMap / std::HashMap on load.
                // The conversion is O(N) once at startup — not in the hot path.
                delegations: s.delegations.into_iter().collect::<ImblHashMap<_, _>>(),
                pool_params: Arc::new(s.pool_params.into_iter().collect::<HashMap<_, _>>()),
                future_pool_params: s.future_pool_params.into_iter().collect(),
                pending_retirements: s.pending_retirements.into_iter().collect(),
                vrf_key_hashes: s.vrf_key_hashes.into_iter().collect::<ImblHashMap<_, _>>(),
                reward_accounts: s.reward_accounts.into_iter().collect::<ImblHashMap<_, _>>(),
                stake_key_deposits: s
                    .stake_key_deposits
                    .into_iter()
                    .collect::<ImblHashMap<_, _>>(),
                pool_deposits: s.pool_deposits.into_iter().collect(),
                total_stake_key_deposits: s.total_stake_key_deposits,
                pointer_map: s.pointer_map.into_iter().collect(),
                stake_distribution: s.stake_distribution.into(),
                script_stake_credentials: s.script_stake_credentials.into_iter().collect(),
                pending_mir_reserves: s.pending_mir_reserves.into_iter().collect(),
                pending_mir_treasury: s.pending_mir_treasury.into_iter().collect(),
                pending_mir_delta_reserves: s.pending_mir_delta_reserves,
                pending_mir_delta_treasury: s.pending_mir_delta_treasury,
            },
            gov: GovSubState {
                governance: Arc::new(s.governance.into()),
            },
            consensus: ConsensusSubState {
                evolving_nonce: s.evolving_nonce,
                candidate_nonce: s.candidate_nonce,
                epoch_nonce: s.epoch_nonce,
                previous_epoch_nonce: s.previous_epoch_nonce,
                lab_nonce: s.lab_nonce,
                last_epoch_block_nonce: s.last_epoch_block_nonce,
                extra_entropy: s.extra_entropy,
                rolling_nonce: s.rolling_nonce,
                first_block_hash_of_epoch: s.first_block_hash_of_epoch,
                prev_epoch_first_block_hash: s.prev_epoch_first_block_hash,
                epoch_blocks_by_pool: Arc::new(s.epoch_blocks_by_pool.into_iter().collect()),
                epoch_block_count: s.epoch_block_count,
                opcert_counters: s.opcert_counters.into_iter().collect(),
            },
            epochs: EpochSubState {
                snapshots: s.snapshots.into(),
                treasury: s.treasury,
                reserves: s.reserves,
                pending_reward_update: s.pending_reward_update.map(Into::into),
                non_myopic: s.non_myopic.into(),
                // Not persisted to ledger snapshots — this is a
                // post-boundary debug-dump aid only.  Recomputed on the
                // next epoch boundary.
                last_applied_rupd: None,
                pending_pp_updates: s.pending_pp_updates,
                future_pp_updates: s.future_pp_updates,
                needs_stake_rebuild: s.needs_stake_rebuild,
                ptr_stake: s.ptr_stake.into_iter().collect(),
                ptr_stake_excluded: s.ptr_stake_excluded,
                protocol_params: s.protocol_params,
                prev_protocol_params: s.prev_protocol_params,
                prev_protocol_version_major: s.prev_protocol_version_major,
                prev_d: s.prev_d,
                // #736: restored from the snapshot (v22+). The pre-v22
                // behaviour — drop on load and lazily re-capture at the
                // restored tip — used a LATE credential set whenever the
                // restart happened past the epoch's 4k/f mark, producing
                // the ~2998 ADA replay-seam treasury shortfall + stale
                // reward balances at the next pv≤6 boundary.
                rupd_addrs_rew: s
                    .rupd_addrs_rew
                    .map(|set| Arc::new(set.into_iter().collect::<HashSet<_>>())),
                rupd_pulser_started: s.rupd_pulser_started,
                rupd_monetary: s.rupd_monetary,
                rupd_snapshot: s.rupd_snapshot.map(Into::into),
                // Transient: rebuilt at the next block, completed at the boundary.
                rupd_fold: Default::default(),
                pending_avvm_return: s.pending_avvm_return,
            },
            tip: s.tip,
            era: s.era,
            pending_era_transition: s.pending_era_transition,
            epoch: s.epoch,
            epoch_length: s.epoch_length,
            shelley_transition_epoch: s.shelley_transition_epoch,
            byron_epoch_length: s.byron_epoch_length,
            slot_config: s.slot_config,
            genesis_hash: s.genesis_hash,
            genesis_delegates: s.genesis_delegates.into_iter().collect(),
            future_gen_delegs: s.future_gen_delegs.into_iter().collect(),
            byron: s.byron,
            update_quorum: s.update_quorum,
            node_network: s.node_network,
            randomness_stabilisation_window: s.randomness_stabilisation_window,
            stability_window_3kf: s.stability_window_3kf,
            security_param: 0, // Set from genesis config at startup via set_epoch_length()
            conway_genesis_init: None, // Set from genesis config at startup
            max_lovelace_supply: super::MAX_LOVELACE_SUPPLY,
            phase2_apply_horizon: None,
            cached_validation_registry: None,
        }
    }
}

// ── Wire mirrors for types embedded wholesale (#1088) ───────────────
//
// Each of these types is used BOTH live (inside `LedgerState`, via
// `imbl::HashMap`/`std::HashMap`/`HashSet` for O(1)-clone or per-block
// mutation reasons) AND cloned wholesale into `LedgerStateSnapshot`. Rather
// than change the live field's type — which would slow the per-block hot
// path for a property only the wire format needs — each gets a `*Wire`
// mirror: identical field-for-field, except every hash-ordered collection
// becomes a `BTreeMap`/`BTreeSet`. A field already backed by
// `imbl::OrdMap`/`OrdSet` (`proposals`, `votes_by_action`, `drep_expiry`, the
// `PRoot`/`PEdges` proposal trees) is carried through UNCHANGED — it already
// iterates in key order, so wrapping it again would just be noise.
//
// This is the same shape `vrf_key_hashes` and `drep_expiry` already used
// (`ImblHashMap` live, `BTreeMap`/`ImblOrdMap` on the wire) — extended to
// every other collection reachable from `LedgerStateSnapshot`.

/// Wire mirror of [`StakeDistributionState`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StakeDistributionStateWire {
    pub stake_map: BTreeMap<Hash32, Lovelace>,
}

impl From<&StakeDistributionState> for StakeDistributionStateWire {
    fn from(s: &StakeDistributionState) -> Self {
        StakeDistributionStateWire {
            stake_map: s.stake_map.iter().map(|(k, v)| (*k, *v)).collect(),
        }
    }
}

impl From<StakeDistributionStateWire> for StakeDistributionState {
    fn from(s: StakeDistributionStateWire) -> Self {
        StakeDistributionState {
            stake_map: s.stake_map.into_iter().collect(),
        }
    }
}

/// Wire mirror of [`NonMyopic`]. Used both for `LedgerState.epochs.non_myopic`
/// and for the copy riding on [`PendingRewardUpdate::non_myopic`] — one
/// mirror type, two call sites, exactly like the live type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NonMyopicWire {
    pub likelihoods: BTreeMap<Hash28, Likelihood>,
    pub reward_pot: Lovelace,
}

impl From<&NonMyopic> for NonMyopicWire {
    fn from(n: &NonMyopic) -> Self {
        NonMyopicWire {
            likelihoods: n.likelihoods.iter().map(|(k, v)| (*k, v.clone())).collect(),
            reward_pot: n.reward_pot,
        }
    }
}

impl From<NonMyopicWire> for NonMyopic {
    fn from(n: NonMyopicWire) -> Self {
        NonMyopic {
            likelihoods: n.likelihoods.into_iter().collect(),
            reward_pot: n.reward_pot,
        }
    }
}

/// Wire mirror of [`PendingRewardUpdate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRewardUpdateWire {
    pub rewards: BTreeMap<Hash32, Lovelace>,
    pub delta_treasury: u64,
    pub delta_reserves: i128,
    pub non_myopic: NonMyopicWire,
}

impl From<&PendingRewardUpdate> for PendingRewardUpdateWire {
    fn from(p: &PendingRewardUpdate) -> Self {
        PendingRewardUpdateWire {
            rewards: p.rewards.iter().map(|(k, v)| (*k, *v)).collect(),
            delta_treasury: p.delta_treasury,
            delta_reserves: p.delta_reserves,
            non_myopic: NonMyopicWire::from(&p.non_myopic),
        }
    }
}

impl From<PendingRewardUpdateWire> for PendingRewardUpdate {
    fn from(p: PendingRewardUpdateWire) -> Self {
        PendingRewardUpdate {
            rewards: p.rewards.into_iter().collect(),
            delta_treasury: p.delta_treasury,
            delta_reserves: p.delta_reserves,
            non_myopic: p.non_myopic.into(),
            // `raw_rewards` is `#[serde(skip)]` — wire-only, never persisted,
            // so there is nothing to restore. A restored snapshot's
            // `pending_reward_update`/`last_applied_rupd` therefore cannot
            // answer the N2C `Complete` arm's `rs` field for a PAST boundary;
            // only a live, in-memory reward update (query time only) can.
            raw_rewards: std::collections::HashMap::new(),
        }
    }
}

/// Wire mirror of [`super::reward_pulser::FreeVars`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FreeVarsWire {
    pub addrs_rew: Option<BTreeSet<Hash32>>,
    pub total_stake: u64,
    pub prot_ver: (u64, u64),
}

impl From<&super::reward_pulser::FreeVars> for FreeVarsWire {
    fn from(f: &super::reward_pulser::FreeVars) -> Self {
        FreeVarsWire {
            addrs_rew: f.addrs_rew.as_ref().map(|s| s.iter().copied().collect()),
            total_stake: f.total_stake,
            prot_ver: f.prot_ver,
        }
    }
}

impl From<FreeVarsWire> for super::reward_pulser::FreeVars {
    fn from(f: FreeVarsWire) -> Self {
        super::reward_pulser::FreeVars {
            addrs_rew: f.addrs_rew.map(|s| s.into_iter().collect()),
            total_stake: f.total_stake,
            prot_ver: f.prot_ver,
        }
    }
}

/// Wire mirror of [`super::reward_pulser::RewardSnapShot`] (#1071) —
/// `likelihoods`/`leaders` and `free_vars.addrs_rew` are `HashMap`/`HashSet`
/// on the live type, so they get the same ordered-container treatment as
/// every other map/set reachable from [`LedgerStateSnapshot`] (#1088).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewardSnapShotWire {
    pub fees: Lovelace,
    pub protocol_version: (u64, u64),
    pub non_myopic: NonMyopicWire,
    pub delta_r1: Lovelace,
    pub r: Lovelace,
    pub delta_t1: Lovelace,
    pub likelihoods: BTreeMap<Hash28, Likelihood>,
    pub leaders: BTreeMap<Hash32, Vec<super::reward_pulser::RewardEntry>>,
    pub free_vars: FreeVarsWire,
}

impl From<&super::reward_pulser::RewardSnapShot> for RewardSnapShotWire {
    fn from(s: &super::reward_pulser::RewardSnapShot) -> Self {
        RewardSnapShotWire {
            fees: s.fees,
            protocol_version: s.protocol_version,
            non_myopic: NonMyopicWire::from(&s.non_myopic),
            delta_r1: s.delta_r1,
            r: s.r,
            delta_t1: s.delta_t1,
            likelihoods: s.likelihoods.iter().map(|(k, v)| (*k, v.clone())).collect(),
            leaders: s.leaders.iter().map(|(k, v)| (*k, v.clone())).collect(),
            free_vars: FreeVarsWire::from(&s.free_vars),
        }
    }
}

impl From<RewardSnapShotWire> for super::reward_pulser::RewardSnapShot {
    fn from(s: RewardSnapShotWire) -> Self {
        super::reward_pulser::RewardSnapShot {
            fees: s.fees,
            protocol_version: s.protocol_version,
            non_myopic: s.non_myopic.into(),
            delta_r1: s.delta_r1,
            r: s.r,
            delta_t1: s.delta_t1,
            likelihoods: s.likelihoods.into_iter().collect(),
            leaders: s.leaders.into_iter().collect(),
            free_vars: s.free_vars.into(),
        }
    }
}

/// Wire mirror of [`super::reward_pulser::PulsingRewUpdate`] (#1071).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PulsingRewUpdateWire {
    Pulsing(Box<RewardSnapShotWire>),
    Complete(Box<RewardSnapShotWire>),
}

impl From<&super::reward_pulser::PulsingRewUpdate> for PulsingRewUpdateWire {
    fn from(p: &super::reward_pulser::PulsingRewUpdate) -> Self {
        match p {
            super::reward_pulser::PulsingRewUpdate::Pulsing(s) => {
                PulsingRewUpdateWire::Pulsing(Box::new(RewardSnapShotWire::from(s.as_ref())))
            }
            super::reward_pulser::PulsingRewUpdate::Complete(s) => {
                PulsingRewUpdateWire::Complete(Box::new(RewardSnapShotWire::from(s.as_ref())))
            }
        }
    }
}

impl From<PulsingRewUpdateWire> for super::reward_pulser::PulsingRewUpdate {
    fn from(p: PulsingRewUpdateWire) -> Self {
        match p {
            PulsingRewUpdateWire::Pulsing(s) => {
                super::reward_pulser::PulsingRewUpdate::Pulsing(Box::new((*s).into()))
            }
            PulsingRewUpdateWire::Complete(s) => {
                super::reward_pulser::PulsingRewUpdate::Complete(Box::new((*s).into()))
            }
        }
    }
}

/// Wire mirror of [`StakeSnapshot`]. Shared by `EpochSnapshotsWire`'s
/// mark/set/go fields — three instances of one type, exactly like the live
/// `StakeSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StakeSnapshotWire {
    pub epoch: EpochNo,
    pub delegations: BTreeMap<Hash32, Hash28>,
    pub pool_stake: BTreeMap<Hash28, Lovelace>,
    pub pool_params: BTreeMap<Hash28, PoolRegistration>,
    pub stake_distribution: BTreeMap<Hash32, Lovelace>,
    pub epoch_fees: Lovelace,
    pub epoch_block_count: u64,
    pub epoch_blocks_by_pool: BTreeMap<Hash28, u64>,
}

impl From<&StakeSnapshot> for StakeSnapshotWire {
    fn from(s: &StakeSnapshot) -> Self {
        StakeSnapshotWire {
            epoch: s.epoch,
            delegations: s.delegations.iter().map(|(k, v)| (*k, *v)).collect(),
            pool_stake: s.pool_stake.iter().map(|(k, v)| (*k, *v)).collect(),
            pool_params: s.pool_params.iter().map(|(k, v)| (*k, v.clone())).collect(),
            stake_distribution: s.stake_distribution.iter().map(|(k, v)| (*k, *v)).collect(),
            epoch_fees: s.epoch_fees,
            epoch_block_count: s.epoch_block_count,
            epoch_blocks_by_pool: s
                .epoch_blocks_by_pool
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
        }
    }
}

impl From<StakeSnapshotWire> for StakeSnapshot {
    fn from(s: StakeSnapshotWire) -> Self {
        StakeSnapshot {
            epoch: s.epoch,
            delegations: Arc::new(s.delegations.into_iter().collect()),
            pool_stake: s.pool_stake.into_iter().collect(),
            pool_params: Arc::new(s.pool_params.into_iter().collect()),
            stake_distribution: Arc::new(s.stake_distribution.into_iter().collect()),
            epoch_fees: s.epoch_fees,
            epoch_block_count: s.epoch_block_count,
            epoch_blocks_by_pool: Arc::new(s.epoch_blocks_by_pool.into_iter().collect()),
        }
    }
}

/// Wire mirror of [`EpochSnapshots`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpochSnapshotsWire {
    pub mark: Option<StakeSnapshotWire>,
    pub set: Option<StakeSnapshotWire>,
    pub go: Option<StakeSnapshotWire>,
    pub ss_fee: Lovelace,
    pub bprev_block_count: u64,
    pub bprev_blocks_by_pool: BTreeMap<Hash28, u64>,
    pub rupd_ready: bool,
}

impl From<&EpochSnapshots> for EpochSnapshotsWire {
    fn from(s: &EpochSnapshots) -> Self {
        EpochSnapshotsWire {
            mark: s.mark.as_ref().map(StakeSnapshotWire::from),
            set: s.set.as_ref().map(StakeSnapshotWire::from),
            go: s.go.as_ref().map(StakeSnapshotWire::from),
            ss_fee: s.ss_fee,
            bprev_block_count: s.bprev_block_count,
            bprev_blocks_by_pool: s
                .bprev_blocks_by_pool
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            rupd_ready: s.rupd_ready,
        }
    }
}

impl From<EpochSnapshotsWire> for EpochSnapshots {
    fn from(s: EpochSnapshotsWire) -> Self {
        EpochSnapshots {
            mark: s.mark.map(Into::into),
            set: s.set.map(Into::into),
            go: s.go.map(Into::into),
            ss_fee: s.ss_fee,
            bprev_block_count: s.bprev_block_count,
            bprev_blocks_by_pool: Arc::new(s.bprev_blocks_by_pool.into_iter().collect()),
            rupd_ready: s.rupd_ready,
        }
    }
}

/// Wire mirror of [`DRepRegistration`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DRepRegistrationWire {
    pub credential: Credential,
    pub deposit: Lovelace,
    pub anchor: Option<Anchor>,
    pub registered_epoch: EpochNo,
    pub drep_expiry: EpochNo,
    pub active: bool,
    pub delegs: BTreeSet<Hash32>,
}

impl From<&DRepRegistration> for DRepRegistrationWire {
    fn from(d: &DRepRegistration) -> Self {
        DRepRegistrationWire {
            credential: d.credential.clone(),
            deposit: d.deposit,
            anchor: d.anchor.clone(),
            registered_epoch: d.registered_epoch,
            drep_expiry: d.drep_expiry,
            active: d.active,
            delegs: d.delegs.iter().copied().collect(),
        }
    }
}

impl From<DRepRegistrationWire> for DRepRegistration {
    fn from(d: DRepRegistrationWire) -> Self {
        DRepRegistration {
            credential: d.credential,
            deposit: d.deposit,
            anchor: d.anchor,
            registered_epoch: d.registered_epoch,
            drep_expiry: d.drep_expiry,
            active: d.active,
            delegs: d.delegs.into_iter().collect::<ImblHashSet<_>>(),
        }
    }
}

/// Wire mirror of [`PGraph`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PGraphWire {
    pub nodes: BTreeMap<GovActionId, PEdges>,
}

impl From<&PGraph> for PGraphWire {
    fn from(g: &PGraph) -> Self {
        PGraphWire {
            nodes: g
                .nodes
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }
}

impl From<PGraphWire> for PGraph {
    fn from(g: PGraphWire) -> Self {
        PGraph {
            nodes: g.nodes.into_iter().collect::<ImblHashMap<_, _>>(),
        }
    }
}

/// Wire mirror of [`EnactedGovTerms`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnactedGovTermsWire {
    pub committee_expiration: BTreeMap<Hash32, EpochNo>,
    pub committee_threshold: Option<Rational>,
    pub constitution: Option<Constitution>,
    pub prev_gov_action_ids: GovRelation<Option<GovActionId>>,
}

impl From<&EnactedGovTerms> for EnactedGovTermsWire {
    fn from(e: &EnactedGovTerms) -> Self {
        EnactedGovTermsWire {
            committee_expiration: e
                .committee_expiration
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            committee_threshold: e.committee_threshold.clone(),
            constitution: e.constitution.clone(),
            prev_gov_action_ids: e.prev_gov_action_ids.clone(),
        }
    }
}

impl From<EnactedGovTermsWire> for EnactedGovTerms {
    fn from(e: EnactedGovTermsWire) -> Self {
        EnactedGovTerms {
            committee_expiration: e.committee_expiration.into_iter().collect(),
            committee_threshold: e.committee_threshold,
            constitution: e.constitution,
            prev_gov_action_ids: e.prev_gov_action_ids,
        }
    }
}

/// Wire mirror of [`PulsingSnapshot`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PulsingSnapshotWire {
    pub proposals: ImblOrdMap<GovActionId, ProposalState>,
    pub votes_by_action: ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
    pub committee_hot_keys: BTreeMap<Hash32, Hash32>,
    pub committee_expiration: BTreeMap<Hash32, EpochNo>,
    pub committee_resigned: BTreeMap<Hash32, Option<Anchor>>,
    pub committee_threshold: Option<Rational>,
    pub no_confidence: bool,
    pub enacted_pparam_update: Option<GovActionId>,
    pub enacted_hard_fork: Option<GovActionId>,
    pub enacted_committee: Option<GovActionId>,
    pub enacted_constitution: Option<GovActionId>,
    pub snapshot_epoch: EpochNo,
    pub treasury: u64,
    pub vote_delegations: BTreeMap<Hash32, DRep>,
    pub drep_distr: BTreeMap<Hash32, u64>,
    pub drep_expiry: ImblOrdMap<Hash32, EpochNo>,
    pub drep_no_confidence: u64,
    pub drep_abstain: u64,
    pub drep_no_confidence_delegated: bool,
    pub drep_abstain_delegated: bool,
}

impl From<&PulsingSnapshot> for PulsingSnapshotWire {
    fn from(p: &PulsingSnapshot) -> Self {
        PulsingSnapshotWire {
            proposals: p.proposals.clone(),
            votes_by_action: p.votes_by_action.clone(),
            committee_hot_keys: p.committee_hot_keys.iter().map(|(k, v)| (*k, *v)).collect(),
            committee_expiration: p
                .committee_expiration
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            committee_resigned: p
                .committee_resigned
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            committee_threshold: p.committee_threshold.clone(),
            no_confidence: p.no_confidence,
            enacted_pparam_update: p.enacted_pparam_update.clone(),
            enacted_hard_fork: p.enacted_hard_fork.clone(),
            enacted_committee: p.enacted_committee.clone(),
            enacted_constitution: p.enacted_constitution.clone(),
            snapshot_epoch: p.snapshot_epoch,
            treasury: p.treasury,
            vote_delegations: p
                .vote_delegations
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            drep_distr: p.drep_distr.iter().map(|(k, v)| (*k, *v)).collect(),
            drep_expiry: p.drep_expiry.clone(),
            drep_no_confidence: p.drep_no_confidence,
            drep_abstain: p.drep_abstain,
            drep_no_confidence_delegated: p.drep_no_confidence_delegated,
            drep_abstain_delegated: p.drep_abstain_delegated,
        }
    }
}

impl From<PulsingSnapshotWire> for PulsingSnapshot {
    fn from(p: PulsingSnapshotWire) -> Self {
        PulsingSnapshot {
            proposals: p.proposals,
            votes_by_action: p.votes_by_action,
            committee_hot_keys: p
                .committee_hot_keys
                .into_iter()
                .collect::<ImblHashMap<_, _>>(),
            committee_expiration: p
                .committee_expiration
                .into_iter()
                .collect::<ImblHashMap<_, _>>(),
            committee_resigned: p
                .committee_resigned
                .into_iter()
                .collect::<ImblHashMap<_, _>>(),
            committee_threshold: p.committee_threshold,
            no_confidence: p.no_confidence,
            enacted_pparam_update: p.enacted_pparam_update,
            enacted_hard_fork: p.enacted_hard_fork,
            enacted_committee: p.enacted_committee,
            enacted_constitution: p.enacted_constitution,
            snapshot_epoch: p.snapshot_epoch,
            treasury: p.treasury,
            vote_delegations: p
                .vote_delegations
                .into_iter()
                .collect::<ImblHashMap<_, _>>(),
            drep_distr: p.drep_distr.into_iter().collect::<ImblHashMap<_, _>>(),
            drep_expiry: p.drep_expiry,
            drep_no_confidence: p.drep_no_confidence,
            drep_abstain: p.drep_abstain,
            drep_no_confidence_delegated: p.drep_no_confidence_delegated,
            drep_abstain_delegated: p.drep_abstain_delegated,
        }
    }
}

/// Wire mirror of [`PulsedRatifyState`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PulsedRatifyStateWire {
    pub computed_at_epoch: EpochNo,
    pub enacted: Vec<GovActionId>,
    pub expired: Vec<GovActionId>,
    pub delayed: bool,
    pub cur_pparams: ProtocolParameters,
    pub has_pparams_changes: bool,
    pub enact_state: EnactedGovTermsWire,
}

impl From<&PulsedRatifyState> for PulsedRatifyStateWire {
    fn from(r: &PulsedRatifyState) -> Self {
        PulsedRatifyStateWire {
            computed_at_epoch: r.computed_at_epoch,
            enacted: r.enacted.clone(),
            expired: r.expired.clone(),
            delayed: r.delayed,
            cur_pparams: r.cur_pparams.clone(),
            has_pparams_changes: r.has_pparams_changes,
            enact_state: EnactedGovTermsWire::from(&r.enact_state),
        }
    }
}

impl From<PulsedRatifyStateWire> for PulsedRatifyState {
    fn from(r: PulsedRatifyStateWire) -> Self {
        PulsedRatifyState {
            computed_at_epoch: r.computed_at_epoch,
            enacted: r.enacted,
            expired: r.expired,
            delayed: r.delayed,
            cur_pparams: r.cur_pparams,
            has_pparams_changes: r.has_pparams_changes,
            enact_state: r.enact_state.into(),
        }
    }
}

/// Wire mirror of [`DRepPulsingState`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DRepPulsingStateWire {
    pub snapshot: PulsingSnapshotWire,
    pub ratify_state: PulsedRatifyStateWire,
}

impl From<&DRepPulsingState> for DRepPulsingStateWire {
    fn from(d: &DRepPulsingState) -> Self {
        DRepPulsingStateWire {
            snapshot: PulsingSnapshotWire::from(&d.snapshot),
            ratify_state: PulsedRatifyStateWire::from(&d.ratify_state),
        }
    }
}

impl From<DRepPulsingStateWire> for DRepPulsingState {
    fn from(d: DRepPulsingStateWire) -> Self {
        DRepPulsingState {
            snapshot: d.snapshot.into(),
            ratify_state: d.ratify_state.into(),
        }
    }
}

/// Wire mirror of [`GovernanceState`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GovernanceStateWire {
    pub dreps: BTreeMap<Hash32, DRepRegistrationWire>,
    pub vote_delegations: BTreeMap<Hash32, DRep>,
    pub committee_hot_keys: BTreeMap<Hash32, Hash32>,
    pub committee_expiration: BTreeMap<Hash32, EpochNo>,
    pub committee_resigned: BTreeMap<Hash32, Option<Anchor>>,
    pub script_committee_credentials: BTreeSet<Hash32>,
    pub script_committee_hot_credentials: BTreeSet<Hash32>,
    pub proposals: ImblOrdMap<GovActionId, ProposalState>,
    pub votes_by_action: ImblOrdMap<GovActionId, ImblOrdMap<Voter, VotingProcedure>>,
    pub proposal_roots: GovRelation<super::PRoot>,
    pub proposal_graph: GovRelation<PGraphWire>,
    pub drep_registration_count: u64,
    pub proposal_count: u64,
    pub constitution: Option<Constitution>,
    pub no_confidence: bool,
    pub committee_threshold: Option<Rational>,
    pub enacted_pparam_update: Option<GovActionId>,
    pub enacted_hard_fork: Option<GovActionId>,
    pub enacted_committee: Option<GovActionId>,
    pub enacted_constitution: Option<GovActionId>,
    pub last_ratified: Vec<(GovActionId, ProposalState)>,
    pub last_expired: Vec<GovActionId>,
    pub last_ratify_delayed: bool,
    pub num_dormant_epochs: u64,
    pub drep_pulsing_state: Option<DRepPulsingStateWire>,
    pub future_pparams: super::FuturePParams,
}

impl From<&GovernanceState> for GovernanceStateWire {
    fn from(g: &GovernanceState) -> Self {
        GovernanceStateWire {
            dreps: g
                .dreps
                .iter()
                .map(|(k, v)| (*k, DRepRegistrationWire::from(v)))
                .collect(),
            vote_delegations: g
                .vote_delegations
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            committee_hot_keys: g.committee_hot_keys.iter().map(|(k, v)| (*k, *v)).collect(),
            committee_expiration: g
                .committee_expiration
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            committee_resigned: g
                .committee_resigned
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            script_committee_credentials: g.script_committee_credentials.iter().copied().collect(),
            script_committee_hot_credentials: g
                .script_committee_hot_credentials
                .iter()
                .copied()
                .collect(),
            proposals: g.proposals.clone(),
            votes_by_action: g.votes_by_action.clone(),
            proposal_roots: g.proposal_roots.clone(),
            proposal_graph: GovRelation {
                pparam: PGraphWire::from(&g.proposal_graph.pparam),
                hard_fork: PGraphWire::from(&g.proposal_graph.hard_fork),
                committee: PGraphWire::from(&g.proposal_graph.committee),
                constitution: PGraphWire::from(&g.proposal_graph.constitution),
            },
            drep_registration_count: g.drep_registration_count,
            proposal_count: g.proposal_count,
            constitution: g.constitution.clone(),
            no_confidence: g.no_confidence,
            committee_threshold: g.committee_threshold.clone(),
            enacted_pparam_update: g.enacted_pparam_update.clone(),
            enacted_hard_fork: g.enacted_hard_fork.clone(),
            enacted_committee: g.enacted_committee.clone(),
            enacted_constitution: g.enacted_constitution.clone(),
            last_ratified: g.last_ratified.clone(),
            last_expired: g.last_expired.clone(),
            last_ratify_delayed: g.last_ratify_delayed,
            num_dormant_epochs: g.num_dormant_epochs,
            drep_pulsing_state: g
                .drep_pulsing_state
                .as_ref()
                .map(DRepPulsingStateWire::from),
            future_pparams: g.future_pparams.clone(),
        }
    }
}

impl From<GovernanceStateWire> for GovernanceState {
    fn from(g: GovernanceStateWire) -> Self {
        GovernanceState {
            dreps: g
                .dreps
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect::<ImblHashMap<_, _>>(),
            vote_delegations: g
                .vote_delegations
                .into_iter()
                .collect::<ImblHashMap<_, _>>(),
            committee_hot_keys: g
                .committee_hot_keys
                .into_iter()
                .collect::<ImblHashMap<_, _>>(),
            committee_expiration: g
                .committee_expiration
                .into_iter()
                .collect::<ImblHashMap<_, _>>(),
            committee_resigned: g
                .committee_resigned
                .into_iter()
                .collect::<ImblHashMap<_, _>>(),
            script_committee_credentials: g
                .script_committee_credentials
                .into_iter()
                .collect::<ImblHashSet<_>>(),
            script_committee_hot_credentials: g
                .script_committee_hot_credentials
                .into_iter()
                .collect::<ImblHashSet<_>>(),
            proposals: g.proposals,
            votes_by_action: g.votes_by_action,
            proposal_roots: g.proposal_roots,
            proposal_graph: GovRelation {
                pparam: g.proposal_graph.pparam.into(),
                hard_fork: g.proposal_graph.hard_fork.into(),
                committee: g.proposal_graph.committee.into(),
                constitution: g.proposal_graph.constitution.into(),
            },
            drep_registration_count: g.drep_registration_count,
            proposal_count: g.proposal_count,
            constitution: g.constitution,
            no_confidence: g.no_confidence,
            committee_threshold: g.committee_threshold,
            enacted_pparam_update: g.enacted_pparam_update,
            enacted_hard_fork: g.enacted_hard_fork,
            enacted_committee: g.enacted_committee,
            enacted_constitution: g.enacted_constitution,
            last_ratified: g.last_ratified,
            last_expired: g.last_expired,
            last_ratify_delayed: g.last_ratify_delayed,
            num_dormant_epochs: g.num_dormant_epochs,
            drep_pulsing_state: g.drep_pulsing_state.map(Into::into),
            future_pparams: g.future_pparams,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LedgerState;

    /// Every field the snapshot serializes must be non-trivial in the shared
    /// fixture (#967).
    ///
    /// The destructuring below has **no `..` rest pattern on purpose**. Adding
    /// a field to `LedgerStateSnapshot` makes this test fail to COMPILE until
    /// `test_fixtures::populated_ledger_state` populates it, which is what
    /// keeps the layout guard from silently narrowing again. Populating the
    /// fixture once would not have been enough: the fixture was empty for
    /// years and every SNAPSHOT_VERSION bump in that time was caught by review
    /// rather than by the hash.
    ///
    /// bincode writes nothing for a `None` and nothing for an empty
    /// collection, so a field left at its default contributes ZERO bytes and
    /// its layout is invisible to `snapshot_format_hash_stability`. Since
    /// #1088, EVERY map/set field below must additionally carry **2+**
    /// entries — with 0 or 1 entries there is nothing to reorder, so the
    /// nondeterminism the hash-stability test exists to catch is itself
    /// invisible at that width.
    #[test]
    fn fixture_populates_every_snapshot_field() {
        let state = crate::state::test_fixtures::populated_ledger_state();
        let snap = LedgerStateSnapshot::from(&state);

        let LedgerStateSnapshot {
            utxo_set,
            tip,
            era,
            pending_era_transition,
            epoch,
            epoch_length,
            shelley_transition_epoch,
            byron_epoch_length,
            protocol_params,
            prev_protocol_params,
            prev_d,
            prev_protocol_version_major,
            stake_distribution,
            treasury,
            pending_donations,
            reserves,
            delegations,
            pool_params,
            future_pool_params,
            pending_retirements,
            vrf_key_hashes,
            snapshots,
            reward_accounts,
            pointer_map,
            genesis_delegates,
            future_gen_delegs,
            epoch_fees,
            epoch_blocks_by_pool,
            epoch_block_count,
            evolving_nonce,
            candidate_nonce,
            epoch_nonce,
            previous_epoch_nonce,
            lab_nonce,
            last_epoch_block_nonce,
            extra_entropy,
            randomness_stabilisation_window,
            stability_window_3kf,
            genesis_hash,
            rolling_nonce,
            stability_window,
            first_block_hash_of_epoch,
            prev_epoch_first_block_hash,
            pending_pp_updates,
            future_pp_updates,
            update_quorum,
            governance,
            slot_config,
            needs_stake_rebuild,
            ptr_stake,
            ptr_stake_excluded,
            pending_reward_update,
            non_myopic,
            total_stake_key_deposits,
            script_stake_credentials,
            pending_mir_reserves,
            pending_mir_treasury,
            pending_mir_delta_reserves,
            pending_mir_delta_treasury,
            diff_seq,
            node_network,
            opcert_counters,
            stake_key_deposits,
            pool_deposits,
            rupd_addrs_rew,
            rupd_pulser_started,
            rupd_monetary,
            rupd_snapshot,
            pending_avvm_return,
            byron,
        } = &snap;

        // ── fields that are NOT serialized ──────────────────────────────
        //
        // `#[serde(skip)]`, so they contribute no bytes and cannot affect the
        // layout hash. Bound above only so the exhaustive match still compiles
        // and still forces a decision about any new field.
        let _ = (
            pending_era_transition,
            needs_stake_rebuild,
            diff_seq,
            node_network,
        );

        // ── deliberately left at its default ────────────────────────────
        //
        // `stability_window` is a legacy field the `From` impl hardcodes to 0
        // for every new snapshot. It still occupies bytes, so it is under the
        // hash; there is simply no non-zero value to give it.
        assert_eq!(*stability_window, 0, "legacy field is always written as 0");

        macro_rules! at_least_two {
            ($($v:expr, $name:literal);* $(;)?) => {
                $(assert!($v.len() >= 2, concat!($name, " has fewer than 2 entries — a \
                     single-entry (or empty) map/set has nothing to reorder, so it cannot \
                     exercise the #1088 determinism guard")));*
            };
        }
        at_least_two!(
            delegations, "delegations";
            pool_params, "pool_params";
            future_pool_params, "future_pool_params";
            pending_retirements, "pending_retirements";
            vrf_key_hashes, "vrf_key_hashes";
            reward_accounts, "reward_accounts";
            pointer_map, "pointer_map";
            genesis_delegates, "genesis_delegates";
            future_gen_delegs, "future_gen_delegs";
            epoch_blocks_by_pool, "epoch_blocks_by_pool";
            ptr_stake, "ptr_stake";
            script_stake_credentials, "script_stake_credentials";
            pending_mir_reserves, "pending_mir_reserves";
            pending_mir_treasury, "pending_mir_treasury";
            opcert_counters, "opcert_counters";
            stake_key_deposits, "stake_key_deposits";
            pool_deposits, "pool_deposits";
            stake_distribution.stake_map, "stake_distribution.stake_map";
        );
        // #1084: Byron's substate. Already `BTreeMap`/`BTreeSet` on the LIVE
        // type (no `*Wire` mirror), so the same 2+-entry convention applies
        // directly to `byron`'s nested maps.
        at_least_two!(
            byron.delegation.delegation_map, "byron.delegation.delegation_map";
            byron.delegation.delegation_map_rev, "byron.delegation.delegation_map_rev";
            byron.delegation.delegation_slots, "byron.delegation.delegation_slots";
            byron.delegation.key_epoch_delegations, "byron.delegation.key_epoch_delegations";
            byron.allowed_delegators, "byron.allowed_delegators";
            byron.update.registered_protocol_update_proposals,
                "byron.update.registered_protocol_update_proposals";
            byron.update.registered_software_update_proposals,
                "byron.update.registered_software_update_proposals";
            byron.update.confirmed_proposals, "byron.update.confirmed_proposals";
            byron.update.proposal_votes, "byron.update.proposal_votes";
            byron.update.registered_endorsements, "byron.update.registered_endorsements";
            byron.update.proposal_registration_slot, "byron.update.proposal_registration_slot";
            byron.update.app_versions, "byron.update.app_versions";
        );
        assert!(
            byron.delegation.scheduled.len() >= 2,
            "byron.delegation.scheduled has fewer than 2 entries"
        );
        assert!(
            byron.update.candidate_protocol_updates.len() >= 2,
            "byron.update.candidate_protocol_updates has fewer than 2 entries"
        );
        // `pending_pp_updates`/`future_pp_updates` are `BTreeMap<EpochNo,
        // Vec<(Hash32, ProtocolParamUpdate)>>` — already ordered at both
        // levels (the outer key and the inner `Vec`'s push order), so they
        // are outside #1088's scope and do not need 2+ OUTER keys. They
        // still need to be non-empty, so the layout inside a
        // `ProtocolParamUpdate` is not invisible to the hash.
        assert!(!pending_pp_updates.is_empty(), "pending_pp_updates");
        assert!(!future_pp_updates.is_empty(), "future_pp_updates");
        // #1067: both halves must be non-trivial. An empty `likelihoods` map
        // writes no element bytes at all, so a layout change inside
        // `Likelihood` would be invisible — the exact blindness this test
        // exists to remove.
        assert!(
            !non_myopic.likelihoods.is_empty(),
            "non_myopic.likelihoods is empty — the layout of a Likelihood \
             contributes no bytes and is invisible to the hash"
        );
        assert!(
            non_myopic
                .likelihoods
                .values()
                .all(|l| l.0.len() == crate::state::non_myopic::SAMPLE_SIZE),
            "non_myopic.likelihoods holds a short Likelihood — a truncated \
             sequence writes fewer bytes and weakens the hash"
        );
        assert!(
            rupd_monetary.is_some(),
            "rupd_monetary is None — bincode writes nothing for a None, so the \
             frozen MonetaryStep's layout would be invisible to the hash"
        );
        assert!(
            *rupd_pulser_started,
            "rupd_pulser_started is false — a bool at its default contributes \
             the same bytes as an absent field, so #1072's persisted state \
             would be invisible to the layout hash"
        );
        // #1071: the wire-only mirror. Its own map/set fields
        // (`likelihoods`/`leaders`/`free_vars.addrs_rew`) need the SAME 2+
        // entry treatment as `non_myopic.likelihoods` above — they are
        // `HashMap`/`HashSet` on the live `RewardSnapShot` and would
        // otherwise be invisible to the layout hash.
        let rupd_snap = match rupd_snapshot {
            Some(PulsingRewUpdateWire::Complete(s)) => s.as_ref(),
            other => {
                panic!("rupd_snapshot must be Some(Complete(_)) in this fixture, got {other:?}")
            }
        };
        assert!(
            rupd_snap.likelihoods.len() >= 2,
            "rupd_snapshot.likelihoods has fewer than 2 entries"
        );
        assert!(
            rupd_snap.leaders.len() >= 2,
            "rupd_snapshot.leaders has fewer than 2 entries"
        );
        assert!(
            rupd_snap
                .free_vars
                .addrs_rew
                .as_ref()
                .is_some_and(|s| s.len() >= 2),
            "rupd_snapshot.free_vars.addrs_rew is absent or has fewer than 2 entries"
        );
        assert_ne!(
            non_myopic.reward_pot.0, 0,
            "non_myopic.reward_pot is 0 — indistinguishable from its default"
        );
        assert!(
            !stake_distribution.stake_map.is_empty(),
            "stake_distribution"
        );

        macro_rules! present {
            ($($v:expr, $name:literal);* $(;)?) => {
                $(assert!($v.is_some(), concat!($name, " is None — bincode writes no \
                     payload for a None, so the layout inside it is invisible"));)*
            };
        }
        present!(
            first_block_hash_of_epoch, "first_block_hash_of_epoch";
            prev_epoch_first_block_hash, "prev_epoch_first_block_hash";
            pending_reward_update, "pending_reward_update";
            rupd_addrs_rew, "rupd_addrs_rew";
            snapshots.mark, "snapshots.mark";
            snapshots.set, "snapshots.set";
            snapshots.go, "snapshots.go";
            governance.constitution, "governance.constitution";
            governance.committee_threshold, "governance.committee_threshold";
            governance.enacted_pparam_update, "governance.enacted_pparam_update";
            governance.enacted_hard_fork, "governance.enacted_hard_fork";
            governance.enacted_committee, "governance.enacted_committee";
            governance.enacted_constitution, "governance.enacted_constitution";
            // The #966 field. This is the specific structure whose layout
            // change the guard failed to notice.
            governance.drep_pulsing_state, "governance.drep_pulsing_state";
        );
        at_least_two!(
            rupd_addrs_rew.as_ref().expect("checked above"), "rupd_addrs_rew";
            pending_reward_update.as_ref().expect("checked above").rewards,
                "pending_reward_update.rewards";
        );
        assert!(
            pending_reward_update
                .as_ref()
                .expect("checked above")
                .non_myopic
                .likelihoods
                .len()
                >= 2,
            "pending_reward_update.non_myopic.likelihoods has fewer than 2 entries"
        );
        {
            let mark = snapshots.mark.as_ref().expect("checked above");
            at_least_two!(
                mark.delegations, "snapshots.mark.delegations";
                mark.pool_stake, "snapshots.mark.pool_stake";
                mark.pool_params, "snapshots.mark.pool_params";
                mark.stake_distribution, "snapshots.mark.stake_distribution";
                mark.epoch_blocks_by_pool, "snapshots.mark.epoch_blocks_by_pool";
            );
        }
        at_least_two!(
            snapshots.bprev_blocks_by_pool, "snapshots.bprev_blocks_by_pool";
        );

        // `GovernanceState` is ONE field of the snapshot but many structures
        // under the hash, so it gets its own exhaustive destructure — again
        // with no `..`.
        //
        // #988 found this gap the hard way: it added `pulsed_ratify_state` to
        // `GovernanceState`, and the top-level destructure above did not fail
        // to compile, because `governance` is a single field there. A guard
        // that is exhaustive at one level only is exhaustive nowhere in
        // particular.
        let GovernanceStateWire {
            dreps,
            vote_delegations,
            committee_hot_keys,
            committee_expiration,
            committee_resigned,
            script_committee_credentials,
            script_committee_hot_credentials,
            proposals: gov_proposals,
            votes_by_action,
            proposal_roots,
            proposal_graph,
            drep_registration_count,
            proposal_count,
            constitution,
            no_confidence,
            committee_threshold,
            enacted_pparam_update,
            enacted_hard_fork,
            enacted_committee,
            enacted_constitution,
            last_ratified,
            last_expired,
            last_ratify_delayed,
            num_dormant_epochs,
            drep_pulsing_state,
            future_pparams,
        } = governance;
        let _ = (proposal_roots, last_ratified, last_expired);
        assert!(
            future_pparams.known().is_some(),
            "governance.future_pparams carries no payload — `ProtocolParameters`' \
             layout inside the enum is invisible to the hash (#977)"
        );
        at_least_two!(
            dreps, "governance.dreps";
            vote_delegations, "governance.vote_delegations";
            committee_hot_keys, "governance.committee_hot_keys";
            committee_expiration, "governance.committee_expiration";
            committee_resigned, "governance.committee_resigned";
            script_committee_credentials, "governance.script_committee_credentials";
            script_committee_hot_credentials, "governance.script_committee_hot_credentials";
            gov_proposals, "governance.proposals";
        );
        // #1088: the DRep's reverse-delegator index must also be 2+ wide —
        // it lives one level below `dreps`' own keys and would otherwise
        // stay invisible to the width check above.
        assert!(
            dreps.values().any(|d| d.delegs.len() >= 2),
            "no DRepRegistration.delegs has 2+ entries — that nested set's \
             ordering would go unexercised"
        );
        // `proposal_graph` is nested one level deeper than the rest of
        // `GovernanceState`'s maps: `GovRelation<PGraphWire>` holds FOUR
        // independent `PGraphWire.nodes` maps (one per governance purpose),
        // and none of them is reachable from a flat `at_least_two!` call on
        // `proposal_graph` itself.
        assert!(
            [
                &proposal_graph.pparam,
                &proposal_graph.hard_fork,
                &proposal_graph.committee,
                &proposal_graph.constitution,
            ]
            .iter()
            .any(|g| g.nodes.len() >= 2),
            "no GovRelation<PGraphWire> purpose tree has 2+ nodes — \
             PGraph.nodes' ordering would go unexercised"
        );

        // The pulser is ONE field here but two whole structures under the
        // hash, so it gets the same exhaustive treatment `GovernanceState`
        // does — the #988 gap was precisely a guard that stopped being
        // exhaustive one level down.
        let DRepPulsingStateWire {
            snapshot: pulsing_snapshot,
            ratify_state,
        } = drep_pulsing_state.as_ref().expect(
            "governance.drep_pulsing_state is None — the frozen pulser's \
                     layout is invisible to the hash (#988)",
        );
        assert!(
            !ratify_state.enacted.is_empty() && !ratify_state.expired.is_empty(),
            "the frozen RatifyState carries no ids — its Vec layouts are \
             invisible to the hash (#988)"
        );
        assert!(
            !pulsing_snapshot.proposals.is_empty(),
            "the frozen PulsingSnapshot carries no proposals (#903)"
        );
        assert!(
            pulsing_snapshot.treasury > 0,
            "PulsingSnapshot.treasury (#966)"
        );
        assert!(drep_registration_count > &0, "drep_registration_count");
        assert!(proposal_count > &0, "proposal_count");
        assert!(*no_confidence, "no_confidence");
        assert!(*last_ratify_delayed, "last_ratify_delayed");
        assert!(num_dormant_epochs > &0, "num_dormant_epochs");
        assert!(
            pulsing_snapshot.drep_no_confidence > 0,
            "PulsingSnapshot.drep_no_confidence"
        );
        assert!(
            pulsing_snapshot.drep_abstain > 0,
            "PulsingSnapshot.drep_abstain"
        );
        assert!(
            pulsing_snapshot.drep_no_confidence_delegated
                && pulsing_snapshot.drep_abstain_delegated,
            "PulsingSnapshot delegation flags must both be true — `false` is \
             the bincode default and so indistinguishable from unwritten (#994)"
        );
        assert!(constitution.is_some(), "constitution");
        assert!(committee_threshold.is_some(), "committee_threshold");
        assert!(enacted_pparam_update.is_some(), "enacted_pparam_update");
        assert!(enacted_hard_fork.is_some(), "enacted_hard_fork");
        assert!(enacted_committee.is_some(), "enacted_committee");
        assert!(enacted_constitution.is_some(), "enacted_constitution");
        at_least_two!(
            pulsing_snapshot.committee_hot_keys, "pulsing_snapshot.committee_hot_keys";
            pulsing_snapshot.committee_expiration, "pulsing_snapshot.committee_expiration";
            pulsing_snapshot.committee_resigned, "pulsing_snapshot.committee_resigned";
            pulsing_snapshot.vote_delegations, "pulsing_snapshot.vote_delegations";
            pulsing_snapshot.drep_distr, "pulsing_snapshot.drep_distr";
            pulsing_snapshot.drep_expiry, "pulsing_snapshot.drep_expiry";
            votes_by_action, "governance.votes_by_action";
            ratify_state.enact_state.committee_expiration,
                "ratify_state.enact_state.committee_expiration";
        );

        // Scalars must be distinguishable from the value a bare
        // `LedgerState::new()` would leave them at, so a field that silently
        // stopped being written is visible.
        macro_rules! nonzero {
            ($($v:expr, $name:literal);* $(;)?) => {
                $(assert_ne!($v, 0, concat!($name, " is zero — indistinguishable \
                     from an unpopulated fixture"));)*
            };
        }
        nonzero!(
            epoch.0, "epoch";
            *epoch_length, "epoch_length";
            *shelley_transition_epoch, "shelley_transition_epoch";
            *byron_epoch_length, "byron_epoch_length";
            *prev_protocol_version_major, "prev_protocol_version_major";
            treasury.0, "treasury";
            pending_donations.0, "pending_donations";
            reserves.0, "reserves";
            epoch_fees.0, "epoch_fees";
            *epoch_block_count, "epoch_block_count";
            *randomness_stabilisation_window, "randomness_stabilisation_window";
            *stability_window_3kf, "stability_window_3kf";
            *update_quorum, "update_quorum";
            *total_stake_key_deposits, "total_stake_key_deposits";
            *pending_mir_delta_reserves, "pending_mir_delta_reserves";
            *pending_mir_delta_treasury, "pending_mir_delta_treasury";
            *pending_avvm_return, "pending_avvm_return";
            prev_d.numerator, "prev_d";
        );
        macro_rules! nonzero_hash {
            ($($v:expr, $name:literal);* $(;)?) => {
                $(assert_ne!(*$v, Hash32::ZERO, concat!($name, " is the zero hash"));)*
            };
        }
        nonzero_hash!(
            evolving_nonce, "evolving_nonce";
            candidate_nonce, "candidate_nonce";
            epoch_nonce, "epoch_nonce";
            previous_epoch_nonce, "previous_epoch_nonce";
            lab_nonce, "lab_nonce";
            last_epoch_block_nonce, "last_epoch_block_nonce";
            extra_entropy, "extra_entropy";
            genesis_hash, "genesis_hash";
            rolling_nonce, "rolling_nonce";
        );
        assert!(*ptr_stake_excluded, "ptr_stake_excluded is at its default");

        // These have no meaningful `Default` to compare against; assert they
        // are at least reachable and structurally sound.
        let _ = (
            utxo_set,
            tip,
            era,
            protocol_params,
            prev_protocol_params,
            slot_config,
        );

        // A size floor, so a future refactor that accidentally empties the
        // fixture fails loudly instead of silently narrowing coverage again.
        let bytes = bincode::serialize(&snap).expect("serialize");
        // The floor is set ABOVE what a bare `LedgerState::new()` produces, so
        // a refactor that quietly reverts the fixture fails here rather than
        // silently narrowing coverage again. Measured 2026-08-03:
        //   populated  11_282 bytes
        //   empty       2_467 bytes   <- the old fixture
        // A floor of 2_000 would have passed on the empty one, which is the
        // same "assertion that cannot fail" shape this whole test is about.
        let empty =
            LedgerStateSnapshot::from(&LedgerState::new(ProtocolParameters::mainnet_defaults()));
        let empty_len = bincode::serialize(&empty).expect("serialize empty").len();
        assert!(
            bytes.len() > empty_len * 3,
            "populated snapshot serialized to {} bytes against an EMPTY fixture's \
             {} — the fixture has been gutted and the layout hash covers almost \
             nothing",
            bytes.len(),
            empty_len
        );
    }

    #[test]
    fn test_ledger_state_snapshot_roundtrip() {
        // Create a LedgerState with non-default values to catch field mismatches
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.epoch = EpochNo(42);
        state.epochs.treasury = Lovelace(1_000_000);
        state.epochs.reserves = Lovelace(999_000_000);

        // Convert to snapshot format
        let snapshot = LedgerStateSnapshot::from(&state);

        // Convert back
        let restored = LedgerState::from(snapshot);

        // Verify key fields survive the roundtrip
        assert_eq!(restored.epoch, state.epoch);
        assert_eq!(restored.epochs.treasury, state.epochs.treasury);
        assert_eq!(restored.epochs.reserves, state.epochs.reserves);
        assert_eq!(restored.era, state.era);
        assert_eq!(
            restored.epochs.protocol_params.protocol_version_major,
            state.epochs.protocol_params.protocol_version_major
        );
    }

    #[test]
    fn test_bincode_roundtrip_through_snapshot_format() {
        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        // Serialize via snapshot format
        let snapshot = LedgerStateSnapshot::from(&state);
        let bytes = bincode::serialize(&snapshot).expect("serialize");

        // Deserialize back through snapshot format
        let restored_snapshot: LedgerStateSnapshot =
            bincode::deserialize(&bytes).expect("deserialize");
        let restored = LedgerState::from(restored_snapshot);

        // Verify key fields
        assert_eq!(restored.epoch, state.epoch);
        assert_eq!(restored.era, state.era);
        assert_eq!(
            restored.epochs.protocol_params.protocol_version_major,
            state.epochs.protocol_params.protocol_version_major
        );
    }

    #[test]
    fn test_rupd_addrs_rew_survives_snapshot_roundtrip() {
        // #736: the pv≤6 RUPD startStep capture is HISTORICAL state (the
        // registered credential set as of the epoch's 4k/f slot). Dropping
        // it on load and re-capturing at the restored tip used a LATE set,
        // mis-routing the next boundary's rewards (~2998 ADA treasury
        // shortfall at mainnet 337→338). It must survive save/load exactly.
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let frozen: std::collections::HashSet<Hash32> =
            [Hash32::from_bytes([1u8; 32]), Hash32::from_bytes([2u8; 32])]
                .into_iter()
                .collect();
        state.epochs.rupd_addrs_rew = Some(Arc::new(frozen.clone()));
        state.epochs.pending_avvm_return = 318_200_000_000_000;

        let snapshot = LedgerStateSnapshot::from(&state);
        let bytes = bincode::serialize(&snapshot).expect("serialize");
        let restored_snapshot: LedgerStateSnapshot =
            bincode::deserialize(&bytes).expect("deserialize");
        let restored = LedgerState::from(restored_snapshot);

        assert_eq!(
            restored.epochs.rupd_addrs_rew.as_deref(),
            Some(&frozen),
            "frozen RUPD credential set must survive the roundtrip byte-exact"
        );
        assert_eq!(restored.epochs.pending_avvm_return, 318_200_000_000_000);

        // None must also roundtrip as None (boundary just crossed).
        let state2 = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let snap2 = LedgerStateSnapshot::from(&state2);
        let bytes2 = bincode::serialize(&snap2).expect("serialize");
        let restored2: LedgerStateSnapshot = bincode::deserialize(&bytes2).expect("deserialize");
        let restored2 = LedgerState::from(restored2);
        assert_eq!(restored2.epochs.rupd_addrs_rew, None);
    }

    /// Regression test for GitHub issue #755 (live-code-path verdict).
    ///
    /// For pv≥7 (Babbage/Conway, e.g. mainnet ep388 in Conway) the
    /// `rupd_addrs_rew` prefilter is bypassed entirely (`None` is correct).
    /// The three RUPD inputs that DO matter for Conway epoch-boundary reward
    /// calculation are:
    ///
    ///   1. `go` snapshot (stake/delegation/pool data two epochs ago)
    ///   2. `bprev_blocks_by_pool` (block production from the previous epoch)
    ///   3. `ss_fee` (fee pot captured by SNAP at the previous boundary)
    ///
    /// These are all carried inside `EpochSnapshots` which is a direct field
    /// of `LedgerStateSnapshot` (via `snapshots: EpochSnapshotsWire`) and is
    /// serialized by `#[derive(Serialize, Deserialize)]`.  A mid-epoch restart
    /// that saves then restores a snapshot MUST preserve all three byte-exact.
    ///
    /// The fourth RUPD input, `prev_protocol_params` (for `rho`, `tau`,
    /// `a0`, `n_opt`), is also directly serialized and validated here.
    ///
    /// The fingerprint forensics showed that the ep388→389 injection occurred
    /// in a val12 run (binary v8, dated 2026-06-11) using a snapshot restored
    /// 70.3% into epoch 388 — a Conway epoch with prev_pv=7.  Since `rupd_addrs_rew`
    /// is never set at pv≥7, the #736 persistence fix was NOT the protection
    /// mechanism.  The divergence must have been in the `go`/`bprev`/`ss_fee`
    /// data itself (mechanism #1: restart-perturbed snapshot content) OR a
    /// content-triggered deterministic bug in that epoch's specific data
    /// (mechanism #2).  The current v2.0.5+ code serializes all three RUPD
    /// inputs byte-exact — confirmed by this test and by 60+ clean boundary
    /// crossings since v2.0.3.  Verdict: STATE ARTIFACT, not a live defect.
    #[test]
    fn test_conway_rupd_inputs_survive_snapshot_roundtrip() {
        // Simulate the ledger state mid-Conway-epoch (pv=9, after ep388-style
        // snapshot restore). Wire up realistic go/bprev/ss_fee/prev_params so
        // the test catches any field that drops on save/restore.
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

        // Set pv=9 (Conway) so the pv≤6 prefilter is bypassed.
        state.epochs.protocol_params.protocol_version_major = 9;
        state.epochs.prev_protocol_params.protocol_version_major = 9;
        state.epochs.prev_protocol_version_major = 9;

        // For Conway, rupd_addrs_rew must be None (never set at pv≥7).
        // This is the correct in-memory state; it must survive as None.
        assert!(
            state.epochs.rupd_addrs_rew.is_none(),
            "pv≥7: rupd_addrs_rew must be None before snapshot"
        );

        // Build a synthetic `go` snapshot representative of a 2-epoch-old
        // mainnet-scale stake distribution.
        let pool_id_a = Hash28::from_bytes([0xA1u8; 28]);
        let pool_id_b = Hash28::from_bytes([0xB2u8; 28]);
        let delegator_1 = Hash32::from_bytes([0x11u8; 32]);
        let delegator_2 = Hash32::from_bytes([0x22u8; 32]);

        let mut pool_stake = std::collections::HashMap::new();
        pool_stake.insert(pool_id_a, Lovelace(18_000_000_000_000)); // 18M ADA
        pool_stake.insert(pool_id_b, Lovelace(7_500_000_000_000)); //  7.5M ADA

        let mut delegations = std::collections::HashMap::new();
        delegations.insert(delegator_1, pool_id_a);
        delegations.insert(delegator_2, pool_id_b);

        let mut stake_dist = std::collections::HashMap::new();
        stake_dist.insert(delegator_1, Lovelace(18_000_000_000_000));
        stake_dist.insert(delegator_2, Lovelace(7_500_000_000_000));

        let go_snap = super::super::StakeSnapshot {
            epoch: dugite_primitives::time::EpochNo(386),
            delegations: Arc::new(delegations),
            pool_stake,
            pool_params: Arc::new(std::collections::HashMap::new()),
            stake_distribution: Arc::new(stake_dist),
            epoch_fees: Lovelace(500_000_000),
            epoch_block_count: 21540,
            epoch_blocks_by_pool: Arc::new(std::collections::HashMap::new()),
        };
        state.epochs.snapshots.go = Some(go_snap.clone());

        // Set bprev (previous epoch's block production).
        let mut bprev_map = std::collections::HashMap::new();
        bprev_map.insert(pool_id_a, 312u64);
        bprev_map.insert(pool_id_b, 95u64);
        state.epochs.snapshots.bprev_blocks_by_pool = Arc::new(bprev_map.clone());
        state.epochs.snapshots.bprev_block_count = 407;

        // Set ss_fee (fee pot from SNAP at the previous boundary).
        let expected_ss_fee = Lovelace(9_876_543_210);
        state.epochs.snapshots.ss_fee = expected_ss_fee;

        // Set prev_protocol_params with realistic values.
        // rho=3/1000, tau=1/5, a0=3/10, n_opt=500 — typical mainnet Conway params.
        state.epochs.prev_protocol_params.rho = dugite_primitives::transaction::Rational {
            numerator: 3,
            denominator: 1000,
        };
        state.epochs.prev_protocol_params.tau = dugite_primitives::transaction::Rational {
            numerator: 1,
            denominator: 5,
        };
        state.epochs.prev_protocol_params.a0 = dugite_primitives::transaction::Rational {
            numerator: 3,
            denominator: 10,
        };
        state.epochs.prev_protocol_params.n_opt = 500;
        // prev_d must be 0/1 for pv≥7 (Conway forces d=0).
        state.epochs.prev_d = dugite_primitives::transaction::Rational {
            numerator: 0,
            denominator: 1,
        };

        // --- SNAPSHOT SAVE/RESTORE ROUND-TRIP ---
        let snapshot = LedgerStateSnapshot::from(&state);
        let bytes = bincode::serialize(&snapshot).expect("serialize");
        let restored_snapshot: LedgerStateSnapshot =
            bincode::deserialize(&bytes).expect("deserialize");
        let restored = LedgerState::from(restored_snapshot);

        // (A) Conway: rupd_addrs_rew must be None after restore — pv≥7 never captures it.
        assert!(
            restored.epochs.rupd_addrs_rew.is_none(),
            "#755: Conway (pv≥7) rupd_addrs_rew must be None after snapshot restore"
        );

        // (B) go snapshot survives byte-exact.
        let restored_go = restored
            .epochs
            .snapshots
            .go
            .as_ref()
            .expect("#755: go snapshot must survive snapshot restore");
        assert_eq!(
            restored_go.epoch, go_snap.epoch,
            "#755: go.epoch must survive restore"
        );
        assert_eq!(
            restored_go.pool_stake.get(&pool_id_a),
            Some(&Lovelace(18_000_000_000_000)),
            "#755: go.pool_stake[pool_a] must survive restore"
        );
        assert_eq!(
            restored_go.pool_stake.get(&pool_id_b),
            Some(&Lovelace(7_500_000_000_000)),
            "#755: go.pool_stake[pool_b] must survive restore"
        );
        assert_eq!(
            restored_go.delegations.get(&delegator_1),
            Some(&pool_id_a),
            "#755: go.delegations[cred_1] must survive restore"
        );
        assert_eq!(
            restored_go.stake_distribution.get(&delegator_1),
            Some(&Lovelace(18_000_000_000_000)),
            "#755: go.stake_distribution[cred_1] must survive restore"
        );

        // (C) bprev_blocks_by_pool survives byte-exact (pool reward attribution).
        let restored_bprev = &restored.epochs.snapshots.bprev_blocks_by_pool;
        assert_eq!(
            restored_bprev.get(&pool_id_a),
            Some(&312u64),
            "#755: bprev_blocks_by_pool[pool_a] must survive restore"
        );
        assert_eq!(
            restored_bprev.get(&pool_id_b),
            Some(&95u64),
            "#755: bprev_blocks_by_pool[pool_b] must survive restore"
        );
        assert_eq!(
            restored.epochs.snapshots.bprev_block_count, 407,
            "#755: bprev_block_count must survive restore"
        );

        // (D) ss_fee survives byte-exact (treasury-cut input).
        assert_eq!(
            restored.epochs.snapshots.ss_fee, expected_ss_fee,
            "#755: ss_fee must survive snapshot restore"
        );

        // (E) prev_protocol_params survives byte-exact (rho/tau/a0/n_opt).
        assert_eq!(
            restored.epochs.prev_protocol_params.rho, state.epochs.prev_protocol_params.rho,
            "#755: prev_protocol_params.rho must survive restore"
        );
        assert_eq!(
            restored.epochs.prev_protocol_params.tau, state.epochs.prev_protocol_params.tau,
            "#755: prev_protocol_params.tau must survive restore"
        );
        assert_eq!(
            restored.epochs.prev_protocol_params.n_opt, 500,
            "#755: prev_protocol_params.n_opt must survive restore"
        );
        assert_eq!(
            restored.epochs.prev_d, state.epochs.prev_d,
            "#755: prev_d must survive restore"
        );

        // (F) pending_avvm_return = 0 for Conway (Shelley→Allegra AVVM is long done).
        assert_eq!(
            restored.epochs.pending_avvm_return, 0,
            "#755: pending_avvm_return must be 0 for Conway"
        );
    }
}
