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
use imbl::HashMap as ImblHashMap;

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
    /// Stake credential → pool ID delegation map.
    ///
    /// Uses `imbl::HashMap` (persistent HAMT) so that per-block validation
    /// snapshots (cloned at block start) are O(1) structural shares instead
    /// of O(N) deep clones of ~784K entries.  Mutations (`insert`/`remove`)
    /// are O(log N) ≈ 20 hash-table ops for mainnet delegation counts.
    /// The net effect is eliminating a ~31 MB memcpy per cert-block — the
    /// #1 apply hotspot at Alonzo/Mary era scales (see perf issue #698).
    pub delegations: ImblHashMap<Hash32, Hash28>,
    pub pool_params: Arc<HashMap<Hash28, PoolRegistration>>,
    pub future_pool_params: HashMap<Hash28, PoolRegistration>,
    pub pending_retirements: HashMap<Hash28, EpochNo>,
    /// Reward account balances: stake credential hash → accumulated rewards.
    ///
    /// Uses `imbl::HashMap` for O(1) clone semantics — see `delegations`
    /// comment.  ~784K entries on mainnet; each entry ~40 bytes.  The
    /// per-block `block_reward_accounts` snapshot held in `apply_block`'s
    /// validation path is now a structural-share clone (O(1)) that sees the
    /// pre-block state, while mid-block mutations build a new version
    /// independently.  No `Arc::make_mut` CoW needed.
    pub reward_accounts: ImblHashMap<Hash32, Lovelace>,
    /// Per-credential stake-key deposit balances.
    ///
    /// Uses `imbl::HashMap` for O(1) clone semantics — see `delegations`
    /// comment.  ~90K entries on mainnet at Conway.
    pub stake_key_deposits: ImblHashMap<Hash32, u64>,
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
    /// The epoch nonce as it stood *before* the most recent epoch rotation
    /// (`praosStatePreviousEpochNonce`). Haskell's `tickChainDepState` sets
    /// this to the old `epochNonce` at the same tick where `epochNonce` is
    /// rotated to `candidateNonce ⭒ lastEpochBlockNonce`, and carries it
    /// forward unchanged on non-boundary ticks.
    ///
    /// It is not an input to any nonce derivation — it exists so that Peras
    /// certificates appearing in blocks can be validated against the epoch
    /// they were produced in. dugite tracks it because it is field [5] of the
    /// 8-field `PraosState` that cardano-node 11.0.x expects on the
    /// `DebugChainDepState` (LSQ tag 13) response (#902); a 7-field response
    /// is rejected outright with an `enforceSize` mismatch.
    pub previous_epoch_nonce: Hash32,
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
    /// Haskell `EpochState.esNonMyopic` — per-pool `Likelihood` history plus the
    /// reward pot frozen at the boundary that produced it.
    ///
    /// A sibling of `snapshots` here for the same reason it is a sibling of
    /// `esSnapshots` upstream: it is epoch-boundary state, but it is NOT part of
    /// `SnapShots` and does not rotate mark/set/go. It is replaced wholesale at
    /// each boundary by `updateNonMyopic`, whose output is carried in
    /// [`PendingRewardUpdate::non_myopic`].
    ///
    /// Cannot be reconstructed from current state — the likelihoods are a
    /// 0.9-decayed accumulator over every past epoch — which is why adding it
    /// forces a `SNAPSHOT_VERSION` bump rather than a lazy backfill.
    pub non_myopic: super::non_myopic::NonMyopic,
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
    /// #11 — pre-Babbage (pv≤6) reward prefilter: the registered reward-account
    /// credential set frozen at `startStep`. Haskell's RUPD pulser FreeVars sets
    /// `fvAddrsRew = Map.keysSet (accounts)` during TICK at the first block whose
    /// slot is `> epoch_first_slot + randomness_stabilisation_window` (4k/f), and
    /// that capture happens BEFORE the triggering block's certs are applied
    /// (TICK precedes the block body). Both the per-member prefilter
    /// (`rewardOnePoolMember`) and the leader/operator prefilter (`collectLRs`
    /// `isAccountRegistered`) test membership of THIS frozen set, NOT the
    /// boundary-time accounts. Captured during epoch N, consumed by
    /// `compute_reward_update` at the N→N+1 boundary, then cleared. `None` ⇒ not
    /// captured this epoch ⇒ fall back to boundary-time `reward_accounts`
    /// (matches Haskell's `RewardsTooLate` forced-startStep-at-boundary path).
    /// Serialized in snapshots (v22+, commit `40db083021`, issue #736) — a
    /// mid-epoch restart past the 4k/f mark that drops this field falls back to
    /// LATE boundary-time accounts, mis-routing treasury (observed at mainnet
    /// 337→338: ~2998 ADA shortfall). pv≥7 never reads it (prefilter bypassed);
    /// its snapshot value is always None for Conway epochs.
    pub rupd_addrs_rew: Option<Arc<HashSet<Hash32>>>,

    /// Whether a RUPD pulser exists for the epoch currently being closed —
    /// Haskell's `nesRu :: StrictMaybe PulsingRewUpdate` reduced to the one bit
    /// the boundary needs today (#1072).
    ///
    /// ```haskell
    /// -- NewEpoch.hs:161, identically ConwayNewEpoch.hs:172
    /// es' <- case ru of
    ///   SNothing -> pure es          -- NO reward update: no deltaR, no deltaT,
    ///                                --  no rewards, no fee drain
    ///   SJust p@(Pulsing _ _) -> ... completeRupd p ... updateRewards
    ///   SJust (Complete ru')  -> updateRewards es eNo ru'
    /// ```
    ///
    /// `ShelleyRUPD` only ever leaves `SNothing` when a block arrives with
    /// `determineRewardTiming /= RewardsTooEarly`, i.e. strictly after
    /// `epoch_first + 4k/f`. If no block lands in that window the pulser is
    /// never started and the boundary applies NOTHING. dugite applied a full
    /// reward update at every boundary regardless, which diverges permanently:
    /// pots move on one side only, and a later withdrawal against a reward only
    /// dugite credited is a block-validity split.
    ///
    /// Reachability is not theoretical. The window is `4k/f` wide — 80 slots on
    /// the devnet — and both the chaos suite's SIGKILL and Round 3's 90 s
    /// outage exceed it.
    ///
    /// Set by the per-block capture in `apply.rs`, consumed and cleared by the
    /// boundary, exactly like [`Self::rupd_addrs_rew`].
    ///
    /// # KNOWN GAP a bool cannot express (deferred to Phase 2)
    ///
    /// `Tick.hs`'s `bheadTransition` builds `RupdEnv bprev es` from **nes0** —
    /// the PRE-boundary state — while passing `nesRu nes1`, post-NEWEPOCH. So a
    /// single tick that both crosses a boundary AND lands past the NEW epoch's
    /// `start_after` makes Haskell start a pulser frozen over PRE-rotation
    /// `bprev`/`ssStakeGo`/`ssFee`, and apply it at the following boundary.
    ///
    /// dugite would instead compute at that boundary from POST-rotation state —
    /// different inputs, different update. A bool cannot represent "a pulser
    /// exists, frozen over the previous epoch's environment"; only the real
    /// `Pulsing(RewardSnapShot, Pulser)` can.
    ///
    /// Reachable on an outage longer than one stabilisation window that spans a
    /// boundary — 320+ slots on the devnet, i.e. within chaos-round territory.
    /// NOT fixed by this field, and recorded here rather than left silent.
    ///
    /// PHASE 2 REPLACES THIS with the real `Option<PulsingRewUpdate>` carrying
    /// the frozen `RewardSnapShot` and the pulser. It is a bool today because
    /// that is the whole of what the boundary decision needs while the update
    /// is still computed in one pass — not because the distinction between
    /// `Pulsing` and `Complete` does not matter. It does, and it is ledger
    /// state; see `state::reward_pulser`.
    pub rupd_pulser_started: bool,

    /// The monetary half of `startStep`, FROZEN at the 4k/f mark (Phase 1a).
    ///
    /// `deltaR1`, `deltaT1` and `_R` are computed from `casReserves`,
    /// `prevPParams`, `ssFee` and `nesBprev` as they stood at the freeze
    /// instant — which is what Haskell does (`PulsingReward.hs:117-141`),
    /// rather than re-reading them at the boundary.
    ///
    /// The VALUES are the same either way: reserves cannot move mid-epoch (MIR
    /// queues into `dsIRewards` and drains at the boundary; `applyRUpd` IS the
    /// boundary), and the other three are written only by SNAP/EPOCH/NEWEPOCH.
    /// dugite was therefore right by accident; this makes it right by
    /// construction, and removes the unstated invariant that
    /// `pending_avvm_return` exists to patch around.
    ///
    /// `None` when the epoch has not reached its mark — which is exactly when
    /// [`Self::rupd_pulser_started`] is false, so the two move together.
    pub rupd_monetary: Option<super::reward_pulser::MonetaryStep>,

    /// The RUPD member fold in flight (Phase 3). TRANSIENT — see
    /// [`super::reward_pulser::InFlightFold`]; it is deliberately absent from
    /// `LedgerStateSnapshot`, so a restart mid-epoch rebuilds it and the
    /// boundary completes it to the identical answer.
    pub rupd_fold: super::reward_pulser::InFlightFold,

    /// AVVM coin returned to reserves by `returnRedeemAddrsToReserves` at the
    /// Shelley→Allegra era boundary, captured so the SAME-boundary reward update
    /// is computed from PRE-AVVM reserves. In Haskell the reward update applied
    /// entering the Allegra epoch (`nesRu`) was computed mid-previous-epoch via
    /// `startStep` BEFORE the AVVM return (which happens at the era translation,
    /// `TranslateEra AllegraEra NewEpochState`), so its `deltaR1 = rho*reserves`
    /// and `totalStake = maxSupply - reserves` use pre-AVVM reserves; the reward
    /// is then APPLIED to post-AVVM reserves. dugite's `on_era_transition` adds
    /// the AVVM to `reserves` BEFORE `process_epoch_transition` computes the
    /// reward, so without this the reward calc would see post-AVVM reserves
    /// (mainnet ep236: +318.2M ADA inflated reserves → deltaR1 over by ~954K ADA
    /// → -561K ADA reserves / +184K ADA treasury divergence). Set in
    /// `return_redeem_addrs_to_reserves`, consumed (and reset) by the first
    /// `compute_reward_update` of the boundary. Serialized in snapshots (v22+,
    /// commit `40db083021`, issue #736); zero for all Conway epochs.
    pub pending_avvm_return: u64,
}
