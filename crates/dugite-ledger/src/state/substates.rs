//! Component sub-states for LedgerState.
//!
//! These structs group related fields from the monolithic LedgerState into
//! independently borrowable components, enabling granular `&mut` access
//! for era-specific rule dispatch.
//!
//! Haskell equivalents:
//! - UtxoSubState  ≈ UTxOState
//! - CertSubState  ≈ CertState (DState + PState)
//! - GovSubState   ≈ ConwayGovState / GovState era
//! - ConsensusSubState ≈ ChainDepState + NewEpochState nonce fields
//! - EpochSubState ≈ EpochState + SnapShots + protocol parameters

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{ProtocolParamUpdate, Rational};
use dugite_primitives::value::Lovelace;

use crate::utxo::UtxoSet;
use crate::utxo_diff::DiffSeq;

use super::{
    EpochSnapshots, GovernanceState, PendingRewardUpdate, PoolRegistration, StakeDistributionState,
};
use dugite_primitives::protocol_params::ProtocolParameters;

/// UTxO state: the unspent transaction output set and per-epoch fee accumulator.
#[derive(Debug, Clone)]
pub struct UtxoSubState {
    pub utxo_set: UtxoSet,
    pub diff_seq: DiffSeq,
    pub epoch_fees: Lovelace,
    pub pending_donations: Lovelace,
}

/// Delegation and pool state: stake credentials, pool registrations, reward accounts.
#[derive(Debug, Clone)]
pub struct CertSubState {
    pub delegations: Arc<HashMap<Hash32, Hash28>>,
    pub pool_params: Arc<HashMap<Hash28, PoolRegistration>>,
    pub future_pool_params: HashMap<Hash28, PoolRegistration>,
    pub pending_retirements: HashMap<Hash28, EpochNo>,
    pub reward_accounts: Arc<HashMap<Hash32, Lovelace>>,
    /// Per-credential stake-key deposit balances.
    ///
    /// Wrapped in `Arc` so the per-block ValidationContext build can share the
    /// map via `Arc::clone` (refcount bump) instead of deep-cloning ~90 k
    /// entries on Conway+ preview.  Mutations go through `Arc::make_mut`,
    /// which is O(1) when the refcount is 1 (the apply path's normal case)
    /// and triggers a one-shot CoW clone only when a reader view is also
    /// holding a reference — same pattern as `reward_accounts` /
    /// `delegations` / `pool_params`.
    pub stake_key_deposits: Arc<HashMap<Hash32, u64>>,
    pub pool_deposits: HashMap<Hash28, u64>,
    pub total_stake_key_deposits: u64,
    pub pointer_map: HashMap<dugite_primitives::credentials::Pointer, Hash32>,
    pub stake_distribution: StakeDistributionState,
    pub script_stake_credentials: HashSet<Hash32>,
    /// Pending MIR per-credential reward deltas sourced from the reserves
    /// pot (Haskell `dsIRewards . irwdSrcReserves` — half of
    /// `InstantaneousRewards`).  Accumulated by MIR-cert apply during
    /// LEDGER STS and drained at the next epoch boundary by `applyMIR`;
    /// never credited directly to `reward_accounts` while a tx is
    /// processing.  Pre-Conway only; Conway removes MIR certs entirely.
    pub pending_mir_reserves: HashMap<Hash32, i128>,
    /// Pending MIR per-credential reward deltas sourced from the treasury
    /// pot (Haskell `dsIRewards . irwdSrcTreasury`).
    pub pending_mir_treasury: HashMap<Hash32, i128>,
    /// Pending pot-to-pot transfer accumulator (Haskell
    /// `dsIRewards . deltaReserves`): reserves drained at the next epoch
    /// boundary and routed to treasury.
    pub pending_mir_delta_reserves: i128,
    /// Pending pot-to-pot transfer accumulator (Haskell
    /// `dsIRewards . deltaTreasury`): treasury drained at the next epoch
    /// boundary and routed to reserves.
    pub pending_mir_delta_treasury: i128,
}

/// Governance state: proposals, votes, DReps, committee.
#[derive(Debug, Clone)]
pub struct GovSubState {
    pub governance: Arc<GovernanceState>,
}

/// Consensus-layer state: nonces, block production counters, opcert tracking.
#[derive(Debug, Clone)]
pub struct ConsensusSubState {
    pub evolving_nonce: Hash32,
    pub candidate_nonce: Hash32,
    pub epoch_nonce: Hash32,
    pub lab_nonce: Hash32,
    pub last_epoch_block_nonce: Hash32,
    /// Active extra entropy (Shelley `ppExtraEntropy`). `Hash32::ZERO` =
    /// NeutralNonce (the value on virtually every epoch). Set via a pre-Conway
    /// PP update (key 13) and folded into the epoch nonce at the TICKN rule:
    /// `η0 = candidate ⭒ prevHashNonce ⭒ extraEntropy`. Sticky across epochs
    /// until changed by another update. Lives here (rather than in
    /// `ProtocolParameters`) because it is consumed only by the nonce
    /// computation and must persist alongside the other consensus nonces.
    pub extra_entropy: Hash32,
    pub rolling_nonce: Hash32,
    pub first_block_hash_of_epoch: Option<Hash32>,
    pub prev_epoch_first_block_hash: Option<Hash32>,
    pub epoch_blocks_by_pool: Arc<HashMap<Hash28, u64>>,
    pub epoch_block_count: u64,
    pub opcert_counters: HashMap<Hash28, u64>,
}

/// Epoch-level state: snapshots, treasury/reserves, protocol parameters.
///
/// Protocol parameters live here because they change at epoch boundaries
/// (via governance enactment or pre-Conway PP update proposals). This allows
/// `process_epoch_transition` to mutate them via `&mut EpochSubState`.
#[derive(Debug, Clone)]
pub struct EpochSubState {
    pub snapshots: EpochSnapshots,
    pub treasury: Lovelace,
    pub reserves: Lovelace,
    pub pending_reward_update: Option<PendingRewardUpdate>,
    /// Reward update that was consumed by the most recent epoch-boundary
    /// handler.  Populated by the boundary handler immediately AFTER it
    /// `take()`s `pending_reward_update`, so debug dumpers (run AFTER
    /// `process_epoch_transition`) can still report the just-applied
    /// rupd.  Never read by ledger logic — feature-gate consumers only.
    pub last_applied_rupd: Option<PendingRewardUpdate>,
    pub pending_pp_updates: BTreeMap<EpochNo, Vec<(Hash32, ProtocolParamUpdate)>>,
    pub future_pp_updates: BTreeMap<EpochNo, Vec<(Hash32, ProtocolParamUpdate)>>,
    pub needs_stake_rebuild: bool,
    pub ptr_stake: HashMap<dugite_primitives::credentials::Pointer, u64>,
    pub ptr_stake_excluded: bool,
    pub protocol_params: ProtocolParameters,
    pub prev_protocol_params: ProtocolParameters,
    pub prev_protocol_version_major: u64,
    /// Decentralisation parameter captured at the previous epoch boundary
    /// (Haskell's `prevPParams ^. ppDG`).  Stored as an exact `Rational` so
    /// that `d >= 4/5` (overlay gate) and `(1 - d) * f * slotsPerEpoch`
    /// (expected-blocks calc) are byte-exact with Haskell — see issue #629.
    pub prev_d: Rational,
}
