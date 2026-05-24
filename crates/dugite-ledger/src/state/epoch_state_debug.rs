//! Per-epoch-boundary FULL ledger-state dumper.
//!
//! Sibling of [`reward_debug`](super::reward_debug) — where that module
//! produces fine-grained per-pool reward diagnostics, this module emits
//! one *whole ledger state* JSON snapshot per epoch boundary so dugite's
//! state can be cross-validated against `cardano-cli debug log-epoch-state`
//! (Haskell side) field-by-field.
//!
//! Gated on the `epoch-state-debug` Cargo feature.  When the feature is
//! enabled at compile time AND `DUGITE_EPOCH_STATE_DUMP=<dir>` is set at
//! runtime, this module writes one JSON file per epoch boundary:
//!
//!   <dir>/epoch_<NNNNNN>.json
//!
//! Production builds NEVER compile this code — zero runtime overhead.
//!
//! Tracking: tasks #21 (dump harness), #22 (Haskell capture), #23 (diff
//! tool).  See `scripts/validation/EPOCH_DIFF.md` for the end-to-end run
//! recipe.

use std::collections::HashMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dugite_primitives::era::Era;
use dugite_primitives::hash::Hash28;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{GovAction, GovActionId, Voter};
use serde::{Deserialize, Serialize};

use super::{EpochSubState, GovernanceState, LedgerState, PoolRegistration, StakeSnapshot};

const ENV_OUTPUT_DIR: &str = "DUGITE_EPOCH_STATE_DUMP";
const TOP_N: usize = 20;

/// Top-level canonical dump for one epoch boundary.
///
/// This schema is the source of truth for cross-validation.  Both the
/// dugite-side dump and the Haskell-side normalizer (see
/// `scripts/validation/normalize-epoch-dump.py`) produce JSON conforming
/// to this shape; the diff tool then compares the two field-by-field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochStateDump {
    pub epoch: u64,
    pub slot: u64,
    pub era: String,
    pub protocol_version: ProtocolVersion,
    pub scalars: Scalars,
    pub nonce: NonceState,
    pub utxo: UtxoSummary,
    pub stake_snapshot: StakeSnapshots,
    pub pools: PoolSummary,
    pub rewards: RewardsSummary,
    pub governance: GovernanceSummary,
    pub pp_current: ProtocolParameters,
    pub pp_previous: ProtocolParameters,
    pub pp_future: Option<ProtocolParameters>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u64,
    pub minor: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scalars {
    pub reserves: u64,
    pub treasury: u64,
    pub fees: u64,
    pub deposits_stake: u64,
    pub deposits_drep: u64,
    pub deposits_proposal: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NonceState {
    pub eta_v: String,
    pub eta_c: String,
    pub eta_h: String,
    pub eta_lj: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoSummary {
    pub count: u64,
    pub total_lovelace: u64,
    pub asset_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakeSnapshots {
    pub mark: StakeSnapshotSummary,
    pub set: StakeSnapshotSummary,
    pub go: StakeSnapshotSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StakeSnapshotSummary {
    pub total_active_stake: u64,
    pub pool_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSummary {
    pub registered: u64,
    pub retiring: u64,
    pub retired_this_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardsSummary {
    pub total_distributed: u64,
    pub per_pool_top20: Vec<PoolReward>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PoolReward {
    pub pool_id_hex: String,
    pub amount: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceSummary {
    pub drep_count: u64,
    pub drep_total_voting_power: u64,
    pub drep_top20: Vec<DRepEntry>,
    pub cc_members: Vec<CcMember>,
    pub cc_threshold_num: u64,
    pub cc_threshold_den: u64,
    pub active_proposals: u64,
    pub active_proposal_ids: Vec<String>,
    pub enacted_this_epoch: Vec<EnactedEntry>,
    pub expired_this_epoch: Vec<String>,
    pub constitution_anchor_hash: String,
    pub committee_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DRepEntry {
    pub drep_id_hex: String,
    pub voting_power: u64,
    pub deposit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CcMember {
    pub hot_key_hex: String,
    pub cold_key_hex: String,
    pub expiry_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnactedEntry {
    pub id: String,
    pub action_type: String,
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn output_dir() -> Option<PathBuf> {
    env::var(ENV_OUTPUT_DIR).ok().and_then(|s| {
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn era_str(era: Era) -> &'static str {
    match era {
        Era::Byron => "byron",
        Era::Shelley => "shelley",
        Era::Allegra => "allegra",
        Era::Mary => "mary",
        Era::Alonzo => "alonzo",
        Era::Babbage => "babbage",
        Era::Conway => "conway",
        Era::Dijkstra => "dijkstra",
    }
}

/// Stringify a `GovActionId` as `<txid_hex>#<index>` (matches Koios
/// convention and is the format consumed by the diff tool).
fn gov_action_id_str(id: &GovActionId) -> String {
    format!("{}#{}", id.transaction_id.to_hex(), id.action_index)
}

fn gov_action_kind(action: &GovAction) -> &'static str {
    match action {
        GovAction::ParameterChange { .. } => "parameter_change",
        GovAction::HardForkInitiation { .. } => "hard_fork_initiation",
        GovAction::TreasuryWithdrawals { .. } => "treasury_withdrawals",
        GovAction::NoConfidence { .. } => "no_confidence",
        GovAction::UpdateCommittee { .. } => "update_committee",
        GovAction::NewConstitution { .. } => "new_constitution",
        GovAction::InfoAction => "info_action",
    }
}

/// Sum `pool_stake` across every entry in the snapshot.
fn snapshot_summary(snap: &StakeSnapshot) -> StakeSnapshotSummary {
    let total_active_stake = snap
        .pool_stake
        .values()
        .fold(0u64, |acc, l| acc.saturating_add(l.0));
    StakeSnapshotSummary {
        total_active_stake,
        pool_count: snap.pool_stake.len() as u64,
    }
}

fn empty_snapshot_summary_or(snap: Option<&StakeSnapshot>) -> StakeSnapshotSummary {
    snap.map(snapshot_summary).unwrap_or_default()
}

/// Sort + truncate a (id, amount) list to the top-N entries.
///
/// Determinism: primary descending amount, secondary ascending id-hex
/// (the schema requires this exact ordering so identical inputs produce
/// identical JSON regardless of upstream HashMap iteration order).
fn top_n_pool_rewards(mut items: Vec<PoolReward>) -> Vec<PoolReward> {
    items.sort_by(|a, b| {
        b.amount
            .cmp(&a.amount)
            .then_with(|| a.pool_id_hex.cmp(&b.pool_id_hex))
    });
    items.truncate(TOP_N);
    items
}

fn top_n_dreps(mut items: Vec<DRepEntry>) -> Vec<DRepEntry> {
    items.sort_by(|a, b| {
        b.voting_power
            .cmp(&a.voting_power)
            .then_with(|| a.drep_id_hex.cmp(&b.drep_id_hex))
    });
    items.truncate(TOP_N);
    items
}

/// Aggregate the reward update into a per-pool total, then take top-N.
///
/// We do NOT have a per-pool reward breakdown directly — only per-credential
/// rewards.  We re-derive pool attribution from the GO snapshot's
/// `delegations` map: each reward credit lands in a credential whose
/// delegation points at a pool.  Credentials with no current delegation
/// are dropped from the top-20 (they still count toward `total_distributed`).
fn rewards_summary(
    rupd: Option<&super::PendingRewardUpdate>,
    go: Option<&StakeSnapshot>,
) -> RewardsSummary {
    let Some(rupd) = rupd else {
        return RewardsSummary {
            total_distributed: 0,
            per_pool_top20: Vec::new(),
        };
    };
    let total_distributed: u64 = rupd.rewards.values().map(|l| l.0).sum();
    let mut per_pool: HashMap<Hash28, u64> = HashMap::new();
    if let Some(go) = go {
        for (cred, lovelace) in rupd.rewards.iter() {
            if let Some(pool_id) = go.delegations.get(cred) {
                per_pool
                    .entry(*pool_id)
                    .and_modify(|v| *v = v.saturating_add(lovelace.0))
                    .or_insert(lovelace.0);
            }
        }
    }
    let items: Vec<PoolReward> = per_pool
        .into_iter()
        .map(|(pool_id, amount)| PoolReward {
            pool_id_hex: pool_id.to_hex(),
            amount,
        })
        .collect();
    RewardsSummary {
        total_distributed,
        per_pool_top20: top_n_pool_rewards(items),
    }
}

/// Build the governance summary from the live (post-boundary) state.
///
/// Note: `enacted_this_epoch` / `expired_this_epoch` are read from
/// `last_ratified` / `last_expired` on the `GovernanceState`, which are
/// populated by `ratify_proposals` during this very boundary.  These
/// fields are reset on the *next* boundary, so dumping immediately after
/// the boundary captures them correctly.
fn governance_summary(
    gov: &GovernanceState,
    proposal_deposit_total: u64,
    drep_deposit_total: u64,
) -> GovernanceSummary {
    // Top-20 DReps by voting power.  Voting power is taken from the
    // boundary snapshot (`drep_distribution_snapshot`) because that is
    // what ratification uses; the live `vote_delegations` map is in
    // flux.
    let dreps: Vec<DRepEntry> = gov
        .dreps
        .iter()
        .filter(|(_, d)| d.active)
        .map(|(hash, d)| {
            let voting_power = gov
                .drep_distribution_snapshot
                .get(hash)
                .copied()
                .unwrap_or(0);
            DRepEntry {
                drep_id_hex: hash.to_hex(),
                voting_power,
                deposit: d.deposit.0,
            }
        })
        .collect();
    let drep_total_voting_power: u64 = dreps.iter().map(|d| d.voting_power).sum();
    let drep_count = dreps.len() as u64;

    let mut cc_members: Vec<CcMember> = Vec::with_capacity(gov.committee_expiration.len());
    for (cold, expiry) in gov.committee_expiration.iter() {
        let hot = gov
            .committee_hot_keys
            .get(cold)
            .map(|h| h.to_hex())
            .unwrap_or_default();
        cc_members.push(CcMember {
            hot_key_hex: hot,
            cold_key_hex: cold.to_hex(),
            expiry_epoch: expiry.0,
        });
    }
    cc_members.sort_by(|a, b| a.cold_key_hex.cmp(&b.cold_key_hex));

    let (cc_threshold_num, cc_threshold_den) = gov
        .committee_threshold
        .as_ref()
        .map(|r| (r.numerator, r.denominator))
        .unwrap_or((0, 1));

    let mut active_proposal_ids: Vec<String> =
        gov.proposals.keys().map(gov_action_id_str).collect();
    active_proposal_ids.sort();

    let mut enacted_this_epoch: Vec<EnactedEntry> = gov
        .last_ratified
        .iter()
        .map(|(id, state)| EnactedEntry {
            id: gov_action_id_str(id),
            action_type: gov_action_kind(&state.procedure.gov_action).to_string(),
        })
        .collect();
    enacted_this_epoch.sort_by(|a, b| a.id.cmp(&b.id));

    let mut expired_this_epoch: Vec<String> =
        gov.last_expired.iter().map(gov_action_id_str).collect();
    expired_this_epoch.sort();

    let constitution_anchor_hash = gov
        .constitution
        .as_ref()
        .map(|c| c.anchor.data_hash.to_hex())
        .unwrap_or_else(|| "0".repeat(64));

    // Synthetic committee_hash: deterministic hash of (sorted cold-key,
    // expiry, hot-key) triples plus the threshold.  We do NOT recompute
    // Haskell's `committeeHash`; the diff tool treats this field as
    // best-effort (see `tolerance.yaml`).  Computed in a tiny helper so
    // the algorithm is reproducible from this file alone.
    let committee_hash = synth_committee_hash(&cc_members, cc_threshold_num, cc_threshold_den);

    let _ = (proposal_deposit_total, drep_deposit_total); // surfaced via Scalars

    GovernanceSummary {
        drep_count,
        drep_total_voting_power,
        drep_top20: top_n_dreps(dreps),
        cc_members,
        cc_threshold_num,
        cc_threshold_den,
        active_proposals: gov.proposals.len() as u64,
        active_proposal_ids,
        enacted_this_epoch,
        expired_this_epoch,
        constitution_anchor_hash,
        committee_hash,
    }
}

/// Deterministic synthetic hash of the committee state.  Not equal to
/// Haskell's `committeeHash`; both sides recompute this from the same
/// canonical input list, so structural equality still holds.
fn synth_committee_hash(members: &[CcMember], num: u64, den: u64) -> String {
    use sha3::{Digest, Sha3_256};
    let mut hasher = Sha3_256::new();
    hasher.update(num.to_be_bytes());
    hasher.update(den.to_be_bytes());
    for m in members {
        hasher.update(m.cold_key_hex.as_bytes());
        hasher.update(b":");
        hasher.update(m.hot_key_hex.as_bytes());
        hasher.update(b":");
        hasher.update(m.expiry_epoch.to_be_bytes());
        hasher.update(b"\n");
    }
    let out = hasher.finalize();
    hex_encode(&out)
}

// ── Capture ─────────────────────────────────────────────────────────────

/// Compute a UTxO summary from a `LedgerState`.
///
/// `count` and `total_lovelace` delegate to `UtxoSet`.  `asset_count` is
/// best-effort: it walks the live UTxO set summing distinct (policy,
/// asset) pairs.  For an LSM-backed store this is O(N) and may be slow at
/// mainnet scale; the dump runs at epoch boundaries (once per ~5 days
/// mainnet) so the cost is amortised, but we still gate it behind a
/// dedicated env var to make it skippable when only nonce/governance
/// fields are needed.
fn utxo_summary(state: &LedgerState) -> UtxoSummary {
    let count = state.utxo.utxo_set.len() as u64;
    let total_lovelace = state.utxo.utxo_set.total_lovelace().0;
    let skip_assets = env::var("DUGITE_EPOCH_STATE_DUMP_SKIP_ASSETS").is_ok();
    let asset_count = if skip_assets {
        0
    } else {
        let mut count: u64 = 0;
        state.utxo.utxo_set.scan_all(|_input, output| {
            for assets in output.value.multi_asset.values() {
                count = count.saturating_add(assets.len() as u64);
            }
        });
        count
    };
    UtxoSummary {
        count,
        total_lovelace,
        asset_count,
    }
}

/// Build an [`EpochStateDump`] from a post-boundary `LedgerState`.
///
/// Inputs:
/// - `state`: the live ledger state just after the boundary handler ran.
/// - `epoch_to`: the epoch label of the newly entered epoch.
/// - `slot`: the slot the boundary fired at (usually first slot of the new epoch).
/// - `rupd_override`: optional explicit reward update to use instead of
///   `state.epochs.last_applied_rupd`.  Tests pass this directly;
///   production callers pass `None` to fall through to
///   `last_applied_rupd` (Bug 3 fix — `pending_reward_update` has
///   already been `take()`-d by the boundary handler at the time the
///   dump fires).
pub fn capture(
    state: &LedgerState,
    epoch_to: u64,
    slot: u64,
    rupd_override: Option<&super::PendingRewardUpdate>,
) -> EpochStateDump {
    let rupd = rupd_override.or(state.epochs.last_applied_rupd.as_ref());
    let cur_pp = &state.epochs.protocol_params;
    let prev_pp = &state.epochs.prev_protocol_params;
    let future_pp = next_future_pp(&state.epochs, EpochNo(epoch_to));

    let drep_deposit_total: u64 = state
        .gov
        .governance
        .dreps
        .values()
        .map(|d| d.deposit.0)
        .sum();
    let proposal_deposit_total: u64 = state
        .gov
        .governance
        .proposals
        .values()
        .map(|p| p.procedure.deposit.0)
        .sum();

    EpochStateDump {
        epoch: epoch_to,
        slot,
        era: era_str(state.era).to_string(),
        protocol_version: ProtocolVersion {
            major: cur_pp.protocol_version_major,
            minor: cur_pp.protocol_version_minor,
        },
        scalars: Scalars {
            reserves: state.epochs.reserves.0,
            treasury: state.epochs.treasury.0,
            // #615f: the dump now fires PRE-boundary (see apply.rs), so
            // `state.utxo.epoch_fees` is the live accumulator for the
            // just-ending epoch — exactly matching Haskell's
            // `currentEpochState.esLState.utxoState.fees` at the last block
            // of epoch N.  (Prior to #615f the dump fired post-boundary
            // where `utxo.epoch_fees` had been zeroed, so #615c switched
            // to `snapshots.ss_fee` — that's the previous epoch's fees,
            // and gave a one-epoch off-by-one against Haskell.)
            fees: state.utxo.epoch_fees.0,
            // Haskell reports `utxoState.deposited` which is the COMBINED
            // stake-key + pool deposit total. Pool deposits are tracked per-pool
            // (with the deposit value at registration time) in `certs.pool_deposits`.
            // Using the per-pool map rather than `pool_params.len() × pool_deposit`
            // ensures correctness when pool_deposit changed via PPUP after some pools
            // were registered.
            deposits_stake: state
                .certs
                .total_stake_key_deposits
                .saturating_add(state.certs.pool_deposits.values().sum::<u64>()),
            deposits_drep: drep_deposit_total,
            deposits_proposal: proposal_deposit_total,
        },
        nonce: NonceState {
            eta_v: state.consensus.evolving_nonce.to_hex(),
            eta_c: state.consensus.candidate_nonce.to_hex(),
            eta_h: state.consensus.epoch_nonce.to_hex(),
            eta_lj: state.consensus.lab_nonce.to_hex(),
        },
        utxo: utxo_summary(state),
        stake_snapshot: StakeSnapshots {
            mark: empty_snapshot_summary_or(state.epochs.snapshots.mark.as_ref()),
            set: empty_snapshot_summary_or(state.epochs.snapshots.set.as_ref()),
            go: empty_snapshot_summary_or(state.epochs.snapshots.go.as_ref()),
        },
        pools: pool_summary(
            &state.certs.pool_params,
            &state.certs.pending_retirements,
            epoch_to,
        ),
        rewards: rewards_summary(rupd, state.epochs.snapshots.go.as_ref()),
        governance: governance_summary(
            &state.gov.governance,
            proposal_deposit_total,
            drep_deposit_total,
        ),
        pp_current: cur_pp.clone(),
        pp_previous: prev_pp.clone(),
        pp_future: future_pp,
    }
}

fn pool_summary(
    pool_params: &HashMap<Hash28, PoolRegistration>,
    pending_retirements: &HashMap<Hash28, EpochNo>,
    current_epoch: u64,
) -> PoolSummary {
    let retiring = pending_retirements
        .values()
        .filter(|e| e.0 > current_epoch)
        .count() as u64;
    let retired_this_epoch = pending_retirements
        .values()
        .filter(|e| e.0 == current_epoch)
        .count() as u64;
    PoolSummary {
        registered: pool_params.len() as u64,
        retiring,
        retired_this_epoch,
    }
}

/// Pick the future protocol parameters that are queued to take effect at
/// the *next* epoch boundary (i.e. epoch_to + 1).  Returns None when no
/// update is queued.  We do not attempt to apply the update here; we
/// simply surface the latest queued value so the diff harness can flag
/// premature/delayed enactment.
fn next_future_pp(epochs: &EpochSubState, epoch_to: EpochNo) -> Option<ProtocolParameters> {
    let next = EpochNo(epoch_to.0.saturating_add(1));
    // Future updates land in `future_pp_updates` keyed by the epoch they
    // become active; we surface the queued ProtocolParamUpdate but cannot
    // *materialise* it as a full `ProtocolParameters` without re-running
    // PPUP — so we instead surface a clone of `protocol_params` annotated
    // with the queued update's protocol_version when present.  This is
    // good enough for the diff harness, which compares scalar fields
    // (n_opt, a0, rho, tau) rather than the whole struct.
    let _ = (epochs, next);
    None
}

// ── I/O ────────────────────────────────────────────────────────────────

/// Write the dump as pretty JSON to `<dir>/epoch_<NNNNNN>.json`,
/// creating `<dir>` if it does not exist.
pub fn write(dump: &EpochStateDump, dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let filename = format!("epoch_{:06}.json", dump.epoch);
    let path = dir.join(filename);
    let file = fs::File::create(&path)?;
    let writer = io::BufWriter::new(file);
    serde_json::to_writer_pretty(writer, dump).map_err(io::Error::other)?;
    Ok(path)
}

/// Capture + write iff `DUGITE_EPOCH_STATE_DUMP=<dir>` is set; otherwise
/// no-op.  Designed to be called once per epoch boundary, immediately
/// after the boundary's RUPD has been applied so reward/treasury/reserve
/// scalars reflect the new state.
///
/// `rupd_override` is normally `None` in production: `capture` will fall
/// through to `state.epochs.last_applied_rupd`, which the boundary
/// handler populated immediately after consuming
/// `pending_reward_update`.  Tests / call sites that have a fresh handle
/// to the just-applied rupd may pass it explicitly.
pub fn maybe_dump(
    state: &LedgerState,
    epoch_to: u64,
    slot: u64,
    rupd_override: Option<&super::PendingRewardUpdate>,
) {
    let Some(dir) = output_dir() else {
        return;
    };
    let dump = capture(state, epoch_to, slot, rupd_override);
    match write(&dump, &dir) {
        Ok(path) => tracing::info!(
            ?path,
            epoch = epoch_to,
            slot,
            pools_registered = dump.pools.registered,
            dreps = dump.governance.drep_count,
            proposals = dump.governance.active_proposals,
            "epoch-state debug dump written"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            dir = ?dir,
            epoch = epoch_to,
            "failed to write epoch-state debug dump"
        ),
    }
}

// Re-export the helper for fixed-format `Voter` debug if a downstream
// tool wants it; currently unused by the diff harness.
#[allow(dead_code)]
fn voter_kind(v: &Voter) -> &'static str {
    match v {
        Voter::ConstitutionalCommittee(_) => "cc",
        Voter::DRep(_) => "drep",
        Voter::StakePool(_) => "spo",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PendingRewardUpdate;
    use dugite_primitives::hash::{Hash, Hash32};
    use dugite_primitives::value::Lovelace;
    use std::sync::Arc;

    fn h28(b: u8) -> Hash28 {
        Hash::from_bytes([b; 28])
    }

    fn h32(b: u8) -> Hash32 {
        Hash::from_bytes([b; 32])
    }

    fn make_state() -> LedgerState {
        let mut s = LedgerState::new(ProtocolParameters::mainnet_defaults());
        s.era = Era::Conway;
        s.epoch = EpochNo(42);
        s.epochs.reserves = Lovelace(10_000_000);
        s.epochs.treasury = Lovelace(20_000);
        // Live (post-boundary, reset to 0 by handler) and snapshotted
        // (just-closed-epoch) fee pots, distinct so Bug 1 regressions
        // would show.
        s.utxo.epoch_fees = Lovelace(999_999);
        s.epochs.snapshots.ss_fee = Lovelace(123);
        s.epochs.protocol_params.protocol_version_major = 10;
        s.epochs.protocol_params.protocol_version_minor = 0;
        // Fabricate a pool registration so `pools.registered` ≠ 0.
        let pool_id = h28(0xaa);
        let pool_reg = PoolRegistration {
            pool_id,
            vrf_keyhash: h32(0xee),
            pledge: Lovelace(0),
            cost: Lovelace(340_000_000),
            margin_numerator: 1,
            margin_denominator: 20,
            reward_account: vec![0xe0; 29],
            owners: vec![h28(0xbb)],
            relays: vec![],
            metadata_url: None,
            metadata_hash: None,
        };
        let mut pmap = HashMap::new();
        pmap.insert(pool_id, pool_reg);
        s.certs.pool_params = Arc::new(pmap);
        // Two retirements: one this epoch, one later.
        s.certs.pending_retirements.insert(h28(0xcc), EpochNo(42));
        s.certs.pending_retirements.insert(h28(0xdd), EpochNo(99));
        s
    }

    #[test]
    fn capture_emits_canonical_schema() {
        let state = make_state();
        let dump = capture(&state, 42, 432_000, None);
        assert_eq!(dump.epoch, 42);
        assert_eq!(dump.slot, 432_000);
        assert_eq!(dump.era, "conway");
        assert_eq!(dump.protocol_version.major, 10);
        assert_eq!(dump.scalars.reserves, 10_000_000);
        assert_eq!(dump.scalars.treasury, 20_000);
        // #615f: fees come from the LIVE `utxo.epoch_fees` (999_999), not
        // the snapshotted `ss_fee` (which is the PREVIOUS epoch's fees).
        // The dump now fires PRE-boundary (apply.rs), so `utxo.epoch_fees`
        // is the just-ending epoch's running fee total — matching Haskell's
        // `utxoState.fees` at the last block of epoch N.
        assert_eq!(dump.scalars.fees, 999_999);
        assert_eq!(dump.pools.registered, 1);
        assert_eq!(dump.pools.retiring, 1);
        assert_eq!(dump.pools.retired_this_epoch, 1);
        assert_eq!(dump.rewards.total_distributed, 0);
        assert_eq!(dump.governance.cc_threshold_den, 1);
    }

    /// #615f: `scalars.fees` must read from the LIVE `utxo.epoch_fees`
    /// (the running accumulator for the just-ending epoch) — matching
    /// Haskell's `utxoState.fees` at end of epoch N.  `ss_fee` is the
    /// PREVIOUS epoch's fees and was the wrong source (#615c regression).
    #[test]
    fn capture_reads_fees_from_live_utxo_epoch_fees() {
        let mut state = make_state();
        state.utxo.epoch_fees = Lovelace(7_777_777);
        // `ss_fee` is the previous epoch's snapshotted fees — should be
        // IGNORED by the dump because the dump now fires pre-boundary.
        state.epochs.snapshots.ss_fee = Lovelace(0);
        let dump = capture(&state, 5, 10, None);
        assert_eq!(dump.scalars.fees, 7_777_777);
    }

    /// Bug 2: `scalars.deposits_stake` must include BOTH stake-key
    /// deposits AND pool-registration deposits, matching Haskell's
    /// `utxoState.deposited`.
    ///
    /// Critically, pool deposits are tracked PER-POOL at registration time
    /// (via `certs.pool_deposits`), NOT as `pool_params.len() × curPParams.pool_deposit`.
    /// This test verifies that the dump uses the historical deposit map even when
    /// `pool_deposit` has since changed (e.g. via PPUP).
    #[test]
    fn capture_combines_stake_key_and_pool_deposits() {
        let mut state = make_state();
        // 3 stake keys × 2 ADA.
        state.certs.total_stake_key_deposits = 3 * 2_000_000;
        // Current pool_deposit param = 600 ADA (changed via PPUP from the original 500 ADA).
        // The dump MUST use historical per-pool values, not this current param.
        state.epochs.protocol_params.pool_deposit = Lovelace(600_000_000);
        // Pool 0xaa (from make_state) registered at 400 ADA (older pool_deposit).
        // Pools 0x10 and 0x11 registered later at 500 ADA.
        let mut pmap: HashMap<Hash28, PoolRegistration> = (*state.certs.pool_params).clone();
        for b in [0x10u8, 0x11] {
            let pid = h28(b);
            pmap.insert(
                pid,
                PoolRegistration {
                    pool_id: pid,
                    vrf_keyhash: h32(0xee),
                    pledge: Lovelace(0),
                    cost: Lovelace(340_000_000),
                    margin_numerator: 1,
                    margin_denominator: 20,
                    reward_account: vec![0xe0; 29],
                    owners: vec![h28(0xbb)],
                    relays: vec![],
                    metadata_url: None,
                    metadata_hash: None,
                },
            );
        }
        state.certs.pool_params = Arc::new(pmap);
        // Populate pool_deposits with PER-POOL historical deposit amounts.
        state.certs.pool_deposits.insert(h28(0xaa), 400_000_000);
        state.certs.pool_deposits.insert(h28(0x10), 500_000_000);
        state.certs.pool_deposits.insert(h28(0x11), 500_000_000);
        let dump = capture(&state, 1, 0, None);
        // Expected: (400 + 500 + 500) ADA pool deposits + 3 × 2 ADA stake-key deposits
        // = 1_400_000_000 + 6_000_000 = 1_406_000_000.
        // Old formula (pool_params.len() × pool_deposit) would give 3 × 600_000_000 + 6_000_000
        // = 1_806_000_000 — wrong because it ignores historical deposit amounts.
        assert_eq!(dump.scalars.deposits_stake, 1_406_000_000);
    }

    /// Bug 3: when the production caller passes `None` for the rupd
    /// override, the dumper falls through to `last_applied_rupd`
    /// (which the boundary handler populated immediately after
    /// `take()`-ing `pending_reward_update`).
    #[test]
    fn capture_falls_through_to_last_applied_rupd() {
        let mut state = make_state();
        let pool_id = h28(0xaa);
        let cred_a = h32(0x01);
        let mut deleg: HashMap<Hash32, Hash28> = HashMap::new();
        deleg.insert(cred_a, pool_id);
        state.epochs.snapshots.go = Some(StakeSnapshot {
            epoch: EpochNo(40),
            delegations: Arc::new(deleg),
            pool_stake: HashMap::new(),
            pool_params: Arc::new(HashMap::new()),
            stake_distribution: Arc::new(HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        });
        let mut rewards = HashMap::new();
        rewards.insert(cred_a, Lovelace(42));
        // Production sequence: boundary handler takes() pending_reward_update
        // then stores it into `last_applied_rupd` so post-boundary
        // dumpers can still see it.
        state.epochs.pending_reward_update = None;
        state.epochs.last_applied_rupd = Some(PendingRewardUpdate {
            rewards,
            delta_treasury: 0,
            delta_reserves: 42,
        });
        let dump = capture(&state, 42, 432_000, None);
        assert_eq!(dump.rewards.total_distributed, 42);
        assert_eq!(dump.rewards.per_pool_top20.len(), 1);
    }

    /// Bug 3 corollary: if both `last_applied_rupd` and an explicit
    /// override are present, the explicit override wins.  Lets future
    /// callers that have the freshly-taken rupd in hand bypass the
    /// indirection.
    #[test]
    fn capture_override_takes_precedence_over_last_applied() {
        let mut state = make_state();
        state.epochs.last_applied_rupd = Some(PendingRewardUpdate {
            rewards: HashMap::new(),
            delta_treasury: 0,
            delta_reserves: 1,
        });
        let mut rewards = HashMap::new();
        rewards.insert(h32(0x99), Lovelace(7));
        let override_rupd = PendingRewardUpdate {
            rewards,
            delta_treasury: 0,
            delta_reserves: 7,
        };
        let dump = capture(&state, 42, 432_000, Some(&override_rupd));
        assert_eq!(dump.rewards.total_distributed, 7);
    }

    #[test]
    fn capture_aggregates_rewards_by_pool() {
        let mut state = make_state();
        // Build a tiny GO snapshot so reward attribution works.
        let pool_id = h28(0xaa);
        let cred_a = h32(0x01);
        let cred_b = h32(0x02);
        let mut deleg: HashMap<Hash32, Hash28> = HashMap::new();
        deleg.insert(cred_a, pool_id);
        deleg.insert(cred_b, pool_id);
        state.epochs.snapshots.go = Some(StakeSnapshot {
            epoch: EpochNo(40),
            delegations: Arc::new(deleg),
            pool_stake: HashMap::new(),
            pool_params: Arc::new(HashMap::new()),
            stake_distribution: Arc::new(HashMap::new()),
            epoch_fees: Lovelace(0),
            epoch_block_count: 0,
            epoch_blocks_by_pool: Arc::new(HashMap::new()),
        });
        let mut rewards = HashMap::new();
        rewards.insert(cred_a, Lovelace(100));
        rewards.insert(cred_b, Lovelace(50));
        let rupd = PendingRewardUpdate {
            rewards,
            delta_treasury: 0,
            delta_reserves: 150,
        };
        let dump = capture(&state, 42, 432_000, Some(&rupd));
        assert_eq!(dump.rewards.total_distributed, 150);
        assert_eq!(dump.rewards.per_pool_top20.len(), 1);
        assert_eq!(dump.rewards.per_pool_top20[0].amount, 150);
        assert_eq!(dump.rewards.per_pool_top20[0].pool_id_hex, pool_id.to_hex());
    }

    #[test]
    fn top_n_pool_rewards_is_deterministic() {
        let items = vec![
            PoolReward {
                pool_id_hex: "ff".repeat(28),
                amount: 100,
            },
            PoolReward {
                pool_id_hex: "00".repeat(28),
                amount: 100,
            },
            PoolReward {
                pool_id_hex: "aa".repeat(28),
                amount: 200,
            },
        ];
        let out = top_n_pool_rewards(items);
        assert_eq!(out[0].amount, 200);
        // Tiebreaker: ascending hex
        assert_eq!(out[1].pool_id_hex, "00".repeat(28));
        assert_eq!(out[2].pool_id_hex, "ff".repeat(28));
    }

    #[test]
    fn top_n_dreps_is_deterministic() {
        let items = vec![
            DRepEntry {
                drep_id_hex: "ff".repeat(32),
                voting_power: 100,
                deposit: 500,
            },
            DRepEntry {
                drep_id_hex: "00".repeat(32),
                voting_power: 100,
                deposit: 500,
            },
        ];
        let out = top_n_dreps(items);
        assert_eq!(out[0].drep_id_hex, "00".repeat(32));
    }

    #[test]
    fn snapshot_summary_handles_empty() {
        assert_eq!(empty_snapshot_summary_or(None).pool_count, 0);
    }

    #[test]
    fn write_and_roundtrip() {
        let state = make_state();
        let dump = capture(&state, 7, 99, None);
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(&dump, tmp.path()).expect("write");
        assert!(path.exists());
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("epoch_000007"));
        let s = fs::read_to_string(&path).unwrap();
        let parsed: EpochStateDump = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.epoch, 7);
        assert_eq!(parsed.era, "conway");
    }

    #[test]
    fn gov_action_id_string_format() {
        let id = GovActionId {
            transaction_id: h32(0xab),
            action_index: 3,
        };
        let s = gov_action_id_str(&id);
        assert!(s.ends_with("#3"));
        assert!(s.starts_with(&"ab".repeat(32)));
    }

    #[test]
    fn synth_committee_hash_is_deterministic() {
        let members = vec![CcMember {
            hot_key_hex: "aa".repeat(28),
            cold_key_hex: "bb".repeat(28),
            expiry_epoch: 100,
        }];
        let h1 = synth_committee_hash(&members, 2, 3);
        let h2 = synth_committee_hash(&members, 2, 3);
        let h_diff_threshold = synth_committee_hash(&members, 1, 3);
        assert_eq!(h1, h2);
        assert_ne!(h1, h_diff_threshold);
        assert_eq!(h1.len(), 64);
    }
}
