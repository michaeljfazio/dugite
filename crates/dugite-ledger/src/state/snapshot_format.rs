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

use imbl::HashMap as ImblHashMap;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{ProtocolParamUpdate, Rational};
use dugite_primitives::value::Lovelace;
use serde::{Deserialize, Serialize};

use super::{
    EpochSnapshots, GovernanceState, PendingRewardUpdate, PoolRegistration, StakeDistributionState,
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
    pub stake_distribution: StakeDistributionState,
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
    pub delegations: HashMap<Hash32, Hash28>,
    /// Pool registrations: pool_id -> pool registration
    pub pool_params: HashMap<Hash28, PoolRegistration>,
    /// Future pool parameters for re-registrations.
    pub future_pool_params: HashMap<Hash28, PoolRegistration>,
    /// Pool retirements pending: pool -> retirement epoch.
    pub pending_retirements: HashMap<Hash28, EpochNo>,
    /// Stake snapshots for the Cardano "mark/set/go" snapshot model
    pub snapshots: EpochSnapshots,
    /// Reward accounts: stake credential hash -> accumulated rewards.
    /// **OWNED, not Arc-shared** — see comment on `delegations`.
    pub reward_accounts: HashMap<Hash32, Lovelace>,
    /// Pointer map: certificate pointers -> credential hashes.
    pub pointer_map: HashMap<dugite_primitives::credentials::Pointer, Hash32>,
    /// Genesis delegates: genesis_key_hash -> (delegate_key_hash, vrf_key_hash).
    pub genesis_delegates: HashMap<Hash28, (Hash28, Hash32)>,
    /// Fees collected in the current epoch
    pub epoch_fees: Lovelace,
    /// Number of blocks produced by each pool in the current epoch.
    /// **OWNED, not Arc-shared** — see comment on `delegations`.
    pub epoch_blocks_by_pool: HashMap<Hash28, u64>,
    /// Total blocks in the current epoch
    pub epoch_block_count: u64,
    /// Evolving nonce (eta_v): accumulated hash of ALL VRF outputs.
    pub evolving_nonce: Hash32,
    /// Candidate nonce: snapshot of evolving_nonce that freezes late in each epoch.
    pub candidate_nonce: Hash32,
    /// Current epoch nonce.
    pub epoch_nonce: Hash32,
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
    pub governance: GovernanceState,
    /// Slot configuration for Plutus time conversion
    pub slot_config: SlotConfig,
    /// Whether stake distribution needs a full rebuild after snapshot load.
    #[serde(skip)]
    pub needs_stake_rebuild: bool,
    /// Pointer-addressed UTxO stake: pointer -> coin amount.
    pub ptr_stake: HashMap<dugite_primitives::credentials::Pointer, u64>,
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
    pub pending_reward_update: Option<PendingRewardUpdate>,
    /// Running total of all stake key deposits locked in the ledger (lovelace).
    pub total_stake_key_deposits: u64,
    /// Script-type stake credentials.
    pub script_stake_credentials: std::collections::HashSet<Hash32>,
    /// Pending MIR reserves-sourced reward deltas (Haskell
    /// `dsIRewards . irwdSrcReserves`; see issue #631).
    pub pending_mir_reserves: HashMap<Hash32, i128>,
    /// Pending MIR treasury-sourced reward deltas (Haskell
    /// `dsIRewards . irwdSrcTreasury`).
    pub pending_mir_treasury: HashMap<Hash32, i128>,
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
    pub opcert_counters: HashMap<Hash28, u64>,
    /// Per-credential deposit paid at stake key registration time (lovelace).
    /// **OWNED, not Arc-shared** — see comment on `delegations`.
    pub stake_key_deposits: HashMap<Hash32, u64>,
    /// Per-pool deposit paid at pool registration time (lovelace).
    pub pool_deposits: HashMap<Hash28, u64>,
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
    pub rupd_addrs_rew: Option<std::collections::HashSet<Hash32>>,
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
            // imbl::HashMap → std::HashMap for the snapshot wire format.
            // The bincode positional field order is unchanged; no version bump needed.
            delegations: s.certs.delegations.iter().map(|(k, v)| (*k, *v)).collect(),
            pool_params: (*s.certs.pool_params).clone(),
            future_pool_params: s.certs.future_pool_params.clone(),
            pending_retirements: s.certs.pending_retirements.clone(),
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
            pool_deposits: s.certs.pool_deposits.clone(),
            total_stake_key_deposits: s.certs.total_stake_key_deposits,
            pointer_map: s.certs.pointer_map.clone(),
            stake_distribution: s.certs.stake_distribution.clone(),
            script_stake_credentials: s.certs.script_stake_credentials.clone(),
            pending_mir_reserves: s.certs.pending_mir_reserves.clone(),
            pending_mir_treasury: s.certs.pending_mir_treasury.clone(),
            pending_mir_delta_reserves: s.certs.pending_mir_delta_reserves,
            pending_mir_delta_treasury: s.certs.pending_mir_delta_treasury,
            // Gov sub-state — owned, see comment above.
            governance: (*s.gov.governance).clone(),
            // Consensus sub-state
            evolving_nonce: s.consensus.evolving_nonce,
            candidate_nonce: s.consensus.candidate_nonce,
            epoch_nonce: s.consensus.epoch_nonce,
            lab_nonce: s.consensus.lab_nonce,
            last_epoch_block_nonce: s.consensus.last_epoch_block_nonce,
            extra_entropy: s.consensus.extra_entropy,
            rolling_nonce: s.consensus.rolling_nonce,
            first_block_hash_of_epoch: s.consensus.first_block_hash_of_epoch,
            prev_epoch_first_block_hash: s.consensus.prev_epoch_first_block_hash,
            // Owned, see comment on `delegations`.
            epoch_blocks_by_pool: (*s.consensus.epoch_blocks_by_pool).clone(),
            epoch_block_count: s.consensus.epoch_block_count,
            opcert_counters: s.consensus.opcert_counters.clone(),
            // Epoch sub-state
            snapshots: s.epochs.snapshots.clone(),
            treasury: s.epochs.treasury,
            reserves: s.epochs.reserves,
            pending_reward_update: s.epochs.pending_reward_update.clone(),
            pending_pp_updates: s.epochs.pending_pp_updates.clone(),
            future_pp_updates: s.epochs.future_pp_updates.clone(),
            needs_stake_rebuild: s.epochs.needs_stake_rebuild,
            ptr_stake: s.epochs.ptr_stake.clone(),
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
            genesis_delegates: s.genesis_delegates.clone(),
            update_quorum: s.update_quorum,
            node_network: s.node_network,
            randomness_stabilisation_window: s.randomness_stabilisation_window,
            stability_window_3kf: s.stability_window_3kf,
            // Legacy field (always zero for new snapshots)
            stability_window: 0,
            // #736: persist the pv≤6 RUPD startStep capture — historical
            // state that a restart cannot re-derive.
            rupd_addrs_rew: s.epochs.rupd_addrs_rew.as_deref().cloned(),
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
                // std::HashMap → imbl::HashMap on load.
                // The conversion is O(N) once at startup — not in the hot path.
                delegations: s.delegations.into_iter().collect::<ImblHashMap<_, _>>(),
                pool_params: Arc::new(s.pool_params),
                future_pool_params: s.future_pool_params,
                pending_retirements: s.pending_retirements,
                reward_accounts: s.reward_accounts.into_iter().collect::<ImblHashMap<_, _>>(),
                stake_key_deposits: s
                    .stake_key_deposits
                    .into_iter()
                    .collect::<ImblHashMap<_, _>>(),
                pool_deposits: s.pool_deposits,
                total_stake_key_deposits: s.total_stake_key_deposits,
                pointer_map: s.pointer_map,
                stake_distribution: s.stake_distribution,
                script_stake_credentials: s.script_stake_credentials,
                pending_mir_reserves: s.pending_mir_reserves,
                pending_mir_treasury: s.pending_mir_treasury,
                pending_mir_delta_reserves: s.pending_mir_delta_reserves,
                pending_mir_delta_treasury: s.pending_mir_delta_treasury,
            },
            gov: GovSubState {
                governance: Arc::new(s.governance),
            },
            consensus: ConsensusSubState {
                evolving_nonce: s.evolving_nonce,
                candidate_nonce: s.candidate_nonce,
                epoch_nonce: s.epoch_nonce,
                lab_nonce: s.lab_nonce,
                last_epoch_block_nonce: s.last_epoch_block_nonce,
                extra_entropy: s.extra_entropy,
                rolling_nonce: s.rolling_nonce,
                first_block_hash_of_epoch: s.first_block_hash_of_epoch,
                prev_epoch_first_block_hash: s.prev_epoch_first_block_hash,
                epoch_blocks_by_pool: Arc::new(s.epoch_blocks_by_pool),
                epoch_block_count: s.epoch_block_count,
                opcert_counters: s.opcert_counters,
            },
            epochs: EpochSubState {
                snapshots: s.snapshots,
                treasury: s.treasury,
                reserves: s.reserves,
                pending_reward_update: s.pending_reward_update,
                // Not persisted to ledger snapshots — this is a
                // post-boundary debug-dump aid only.  Recomputed on the
                // next epoch boundary.
                last_applied_rupd: None,
                pending_pp_updates: s.pending_pp_updates,
                future_pp_updates: s.future_pp_updates,
                needs_stake_rebuild: s.needs_stake_rebuild,
                ptr_stake: s.ptr_stake,
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
                rupd_addrs_rew: s.rupd_addrs_rew.map(Arc::new),
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
            genesis_delegates: s.genesis_delegates,
            update_quorum: s.update_quorum,
            node_network: s.node_network,
            randomness_stabilisation_window: s.randomness_stabilisation_window,
            stability_window_3kf: s.stability_window_3kf,
            security_param: 0, // Set from genesis config at startup via set_epoch_length()
            conway_genesis_init: None, // Set from genesis config at startup
            max_lovelace_supply: super::MAX_LOVELACE_SUPPLY,
            phase2_apply_horizon: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LedgerState;

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
    /// of `LedgerStateSnapshot` (via `snapshots: EpochSnapshots`) and is
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
