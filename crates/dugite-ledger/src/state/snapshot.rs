//! Ledger state snapshot persistence: save, load, and UTxO store attachment.
//!
//! # Snapshot format
//!
//! All snapshots use bincode serialization of [`LedgerStateSnapshot`](super::snapshot_format::LedgerStateSnapshot).  The on-disk
//! layout is:
//!
//! ```text
//! [4 bytes]  magic  "DUGT"
//! [1 byte]   version (SNAPSHOT_VERSION)
//! [32 bytes] blake2b-256 checksum of the payload
//! [N bytes]  bincode payload (LedgerState)
//! ```
//!
//! Two legacy formats are also supported for backwards compatibility:
//! - **Legacy with checksum** – `DUGT` + 32-byte checksum + data (no version byte)
//! - **Legacy raw** – plain bincode with no header at all
//!
//! # Version policy
//!
//! Increment `SNAPSHOT_VERSION` whenever the serialized `LedgerState` layout
//! changes (adding, removing, or reordering fields).  Because bincode is
//! positional and not self-describing, structural changes break existing
//! snapshots.  This is acceptable — snapshots are an optimization, not critical
//! data.  The node can always reconstruct state from the chain.
//!
//! # Backend meta sidecar
//!
//! Each snapshot file `foo.bin` is accompanied by a JSON sidecar `foo.meta.json`
//! that records the UTxO backend used when the snapshot was taken.  On load,
//! callers use [`check_snapshot_backend_match`] to reject snapshots made with
//! a structurally-incompatible backend before loading the full bincode payload
//! — mirroring Haskell's `MetadataBackendMismatch` in `InitFailureRead`.  The
//! one asymmetric exception is a `dugite-mem` snapshot loaded under the
//! `dugite-lsm` backend: its inline UTxOs are *convertible* into the LSM store
//! at attach time (see [`BackendCheckResult::Convertible`]), so it is loaded
//! and migrated inline rather than discarded — the dugite analogue of Haskell's
//! offline `snapshot-converter` (`SnapshotConversion`, mem → LSM).  Missing
//! sidecars are handled via [`infer_backend_from_snapshot`] (back-compat for
//! existing `db-mainnet` snapshots that pre-date this feature).

use super::{LedgerError, LedgerState, MAX_SNAPSHOT_SIZE};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

// ─── Backend tag ─────────────────────────────────────────────────────────────

/// UTxO storage backend tag embedded in a snapshot's `.meta.json` sidecar.
///
/// Mirrors Haskell's `UTxOHDMemSnapshot` / `UTxOHDLSMSnapshot` in
/// `Ouroboros.Consensus.Storage.LedgerDB.Snapshots` — a discriminant that
/// lets the loader detect backend mismatches before touching the bincode
/// payload (which is observationally equivalent across backends but
/// structurally incompatible: LSM snapshots have an empty `utxo_set`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotBackend {
    /// In-memory HashMap backend (`utxo_set` is non-empty in the `.bin`).
    DugiteMem,
    /// LSM on-disk backend (`utxo_set` is empty in the `.bin`; UTxOs live
    /// in the adjacent `utxo-store/` LSM directory).
    DugiteLsm,
}

impl SnapshotBackend {
    /// The canonical string tag written into `meta.json`.
    pub fn as_tag(self) -> &'static str {
        match self {
            SnapshotBackend::DugiteMem => "dugite-mem",
            SnapshotBackend::DugiteLsm => "dugite-lsm",
        }
    }

    /// Parse a tag string back to a [`SnapshotBackend`].
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "dugite-mem" => Some(SnapshotBackend::DugiteMem),
            "dugite-lsm" => Some(SnapshotBackend::DugiteLsm),
            _ => None,
        }
    }
}

// ─── Meta sidecar ────────────────────────────────────────────────────────────

/// JSON sidecar written next to each `.bin` snapshot file.
///
/// Written atomically (tmp + rename) after every successful `.bin` write so
/// that a crash between the two leaves the `.bin` consistent (the next start
/// will either find a matching `.meta.json` or fall through to
/// [`infer_backend_from_snapshot`] for back-compat).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    /// Backend tag — must match the running node's configured backend.
    pub backend: String,
    /// Snapshot format version (`SNAPSHOT_VERSION`) at write time.
    pub snapshot_version: u8,
    /// Ledger tip slot (0 = origin).
    pub slot: u64,
    /// Ledger epoch number.
    pub epoch: u64,
    /// Number of UTxO entries at snapshot time (for diagnostics).
    pub utxo_count: u64,
    /// Blake2b-256 checksum of the bincode payload (hex, matches header bytes 5..37).
    pub state_checksum: String,
}

impl SnapshotMeta {
    /// Derive the sidecar path for a given snapshot `.bin` path.
    ///
    /// `ledger-snapshot-epoch5-slot12345.bin` → `ledger-snapshot-epoch5-slot12345.meta.json`
    pub fn sidecar_path(bin_path: &Path) -> PathBuf {
        let mut s = bin_path.as_os_str().to_owned();
        s.push(".meta.json");
        PathBuf::from(s)
    }

    /// Write `self` atomically as the sidecar next to `bin_path`.
    ///
    /// Uses a `.meta.json.tmp` staging file + rename for crash-safety.  A
    /// partial write never replaces an existing consistent sidecar.
    pub fn write_atomic(&self, bin_path: &Path) -> std::io::Result<()> {
        let meta_path = Self::sidecar_path(bin_path);
        let tmp_path = {
            let mut s = meta_path.as_os_str().to_owned();
            s.push(".tmp");
            PathBuf::from(s)
        };
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp_path, json.as_bytes())?;
        std::fs::rename(&tmp_path, &meta_path)
    }

    /// Load the sidecar for `bin_path`, or `None` if the file does not exist.
    ///
    /// Returns `None` (not `Err`) for missing sidecars so callers can apply
    /// the back-compat inference path ([`infer_backend_from_snapshot`]).
    pub fn load(bin_path: &Path) -> Option<SnapshotMeta> {
        let meta_path = Self::sidecar_path(bin_path);
        let raw = std::fs::read(&meta_path).ok()?;
        serde_json::from_slice(&raw)
            .map_err(|e| {
                warn!(
                    path = %meta_path.display(),
                    "Failed to parse snapshot meta sidecar: {e} — treating as absent"
                );
                e
            })
            .ok()
    }
}

// ─── Infer backend from snapshot bytes (back-compat) ─────────────────────────

/// Infer the backend from a loaded `LedgerState` and the adjacent filesystem.
///
/// Used when no `.meta.json` sidecar exists (snapshots written before this
/// feature was added — e.g. `db-mainnet` epoch-329 LSM snapshot).
///
/// Rule:
/// - If `utxo_set` is non-empty → `DugiteMem` (UTxOs are in the bincode).
/// - If `utxo_set` is empty AND a `utxo-store` directory exists in `db_path`
///   → `DugiteLsm` (UTxOs live in the LSM tree).
/// - If `utxo_set` is empty AND no `utxo-store` exists → cannot determine;
///   return `None` (caller should accept provisionally).
pub fn infer_backend_from_snapshot(state: &LedgerState, db_path: &Path) -> Option<SnapshotBackend> {
    if !state.utxo.utxo_set.is_empty() {
        return Some(SnapshotBackend::DugiteMem);
    }
    let utxo_store_path = db_path.join("utxo-store");
    if utxo_store_path.exists() {
        return Some(SnapshotBackend::DugiteLsm);
    }
    // Empty utxo_set and no utxo-store: fresh ledger or unknown — cannot infer.
    None
}

// ─── Backend mismatch guard ───────────────────────────────────────────────────

/// Outcome of a backend-mismatch check.  The caller decides how to handle
/// each variant; typically `Mismatch` → skip the snapshot and try the next.
#[derive(Debug, PartialEq, Eq)]
pub enum BackendCheckResult {
    /// Backend matches the configured one (or cannot be determined). Safe to use.
    Ok,
    /// The snapshot was made with the in-memory backend (`dugite-mem`, UTxOs
    /// inline in the `.bin`) but the node is configured for the LSM backend
    /// (`dugite-lsm`). This asymmetric pair is *convertible at load time*: the
    /// inline `utxo_set` is migrated into the freshly-opened LSM store by the
    /// node's existing `attach_utxo_store` drain, so the snapshot can be loaded
    /// rather than discarded.
    ///
    /// This is the dugite analogue of Haskell's offline `snapshot-converter`
    /// (`Ouroboros.Consensus.Cardano.SnapshotConversion`, which converts a
    /// `UTxOHDMemSnapshot` to a `UTxOHDLSMSnapshot`). Because dugite's `.bin`
    /// payload is backend-agnostic (the only structural difference is whether
    /// `utxo_set` is populated inline), the conversion is performed inline on
    /// the first `run --utxo-backend lsm` after a `mithril-import` instead of
    /// forcing a full from-genesis replay.
    ///
    /// The reverse pairing (an LSM snapshot under the in-memory backend) is
    /// **not** convertible at load — those UTxOs live in the adjacent
    /// `utxo-store/` LSM tree, not inline — and remains a [`Mismatch`].
    ///
    /// [`Mismatch`]: BackendCheckResult::Mismatch
    Convertible {
        snapshot_backend: SnapshotBackend,
        configured_backend: SnapshotBackend,
    },
    /// The snapshot was made with a different backend and cannot be converted
    /// at load time. Must not be loaded.
    Mismatch {
        snapshot_backend: SnapshotBackend,
        configured_backend: SnapshotBackend,
    },
}

/// Classify a concrete `(snapshot_backend, configured_backend)` pair into the
/// load-time outcome. Centralised so the explicit-sidecar path and the
/// inference fallback share one rule.
///
/// * Equal backends → [`BackendCheckResult::Ok`].
/// * `DugiteMem` snapshot under a `DugiteLsm` node → [`BackendCheckResult::Convertible`]
///   (inline UTxOs are drained into the LSM store at attach time).
/// * Any other unequal pair → [`BackendCheckResult::Mismatch`].
fn classify_backend_pair(
    snapshot_backend: SnapshotBackend,
    configured_backend: SnapshotBackend,
) -> BackendCheckResult {
    match (snapshot_backend, configured_backend) {
        (a, b) if a == b => BackendCheckResult::Ok,
        (SnapshotBackend::DugiteMem, SnapshotBackend::DugiteLsm) => {
            BackendCheckResult::Convertible {
                snapshot_backend,
                configured_backend,
            }
        }
        _ => BackendCheckResult::Mismatch {
            snapshot_backend,
            configured_backend,
        },
    }
}

/// Check whether the meta sidecar's backend matches `configured_backend`.
///
/// `bin_path` is the path to the `.bin` snapshot file.  The sidecar is read
/// lazily inside this function.  If the sidecar is absent, `state` and
/// `db_path` are used for inference via [`infer_backend_from_snapshot`].
///
/// Returns [`BackendCheckResult::Ok`] when:
/// - The sidecar matches the configured backend.
/// - No sidecar exists and inference yields the same backend or is indeterminate.
///
/// Returns [`BackendCheckResult::Convertible`] when the snapshot is a
/// `dugite-mem` snapshot loaded under a `dugite-lsm` node — the inline UTxOs
/// can be migrated into the LSM store at attach time (see the variant docs).
///
/// Returns [`BackendCheckResult::Mismatch`] when the sidecar (or inference)
/// conclusively identifies a *different*, non-convertible backend from
/// `configured_backend`.
pub fn check_snapshot_backend_match(
    bin_path: &Path,
    state: &LedgerState,
    db_path: &Path,
    configured_backend: SnapshotBackend,
) -> BackendCheckResult {
    // Try explicit sidecar first.
    if let Some(meta) = SnapshotMeta::load(bin_path) {
        if let Some(snap_backend) = SnapshotBackend::from_tag(&meta.backend) {
            return classify_backend_pair(snap_backend, configured_backend);
        }
        // Unknown tag string — treat as indeterminate; accept provisionally.
        warn!(
            backend_tag = %meta.backend,
            "Unknown snapshot backend tag in meta sidecar — accepting provisionally"
        );
        return BackendCheckResult::Ok;
    }

    // No sidecar — fall back to inference (back-compat for pre-meta snapshots).
    if let Some(inferred) = infer_backend_from_snapshot(state, db_path) {
        return classify_backend_pair(inferred, configured_backend);
    }
    // Could not be determined — accept provisionally.
    BackendCheckResult::Ok
}

/// Quarantine extension applied to snapshot files whose on-disk format
/// version is older than [`LedgerState::SNAPSHOT_VERSION`] and for which
/// no in-place migration path exists.
///
/// The file is renamed `<name>.bin` → `<name>.bin.vNN-unreadable` (where
/// `NN` is the snapshot's version byte). This:
///
///   1. Preserves the bytes for post-mortem analysis instead of silently
///      losing them when the caller falls through to a fresh-ledger start.
///   2. Removes the file from [`crate::startup::enumerate_snapshots`] (which
///      filters by the `.bin` suffix) so the next restart does not retry the
///      same unreadable snapshot in a loop.
///   3. Keeps the rename **inside** the database directory so operators
///      can find and inspect (or delete) it without hunting elsewhere on
///      the filesystem.
///
/// See issue #609.
const QUARANTINE_SUFFIX_PREFIX: &str = "v";
const QUARANTINE_SUFFIX_TAIL: &str = "-unreadable";

/// Rename `path` to `<path>.v{version}-unreadable` so that:
///
///   * the file is preserved for forensic inspection, and
///   * the next restart does not pick it up via the `.bin` enumerator.
///
/// Logs the rename outcome; failures are demoted to a warning because the
/// caller can still make forward progress (it will fall back to a fresh
/// ledger via chain replay). Returns the quarantine destination on
/// success or `None` if the rename failed.
fn quarantine_unreadable_snapshot(path: &Path, version: u8) -> Option<PathBuf> {
    // Build `<original-name>.v{N}-unreadable`. We append rather than
    // replace the extension so that `ledger-snapshot-epoch400-slot12345.bin`
    // becomes `ledger-snapshot-epoch400-slot12345.bin.v15-unreadable`,
    // preserving every original filename component for diagnostics.
    let mut target = path.as_os_str().to_owned();
    target.push(".");
    target.push(format!(
        "{QUARANTINE_SUFFIX_PREFIX}{version}{QUARANTINE_SUFFIX_TAIL}"
    ));
    let target = PathBuf::from(target);

    match std::fs::rename(path, &target) {
        Ok(()) => {
            warn!(
                original = %path.display(),
                quarantined = %target.display(),
                snapshot_version = version,
                "Quarantined unreadable ledger snapshot — chain will be \
                 replayed from ImmutableDB on next start. Inspect or \
                 delete the .{QUARANTINE_SUFFIX_PREFIX}{version}{QUARANTINE_SUFFIX_TAIL} \
                 file once recovery completes."
            );
            Some(target)
        }
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "Failed to rename unreadable snapshot for quarantine — \
                 operator may need to delete it manually before next start \
                 if it is the primary snapshot."
            );
            None
        }
    }
}

/// `std::io::Write` adapter that forwards every byte to an inner writer
/// and simultaneously feeds it into a blake2b-256 hasher.
///
/// Used by [`LedgerState::save_snapshot`] so that the snapshot digest can
/// be computed while `bincode::serialize_into` streams the payload to a
/// buffered file, without first materialising the whole payload in a
/// `Vec<u8>`.
struct HashingWriter<'a, W: std::io::Write> {
    inner: &'a mut W,
    hasher: dugite_primitives::hash::Blake2b256Hasher,
    bytes_written: u64,
}

impl<'a, W: std::io::Write> std::io::Write for HashingWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        // Only hash bytes that were actually accepted by the underlying
        // writer — partial writes must not double-hash the prefix.
        self.hasher.update(&buf[..n]);
        self.bytes_written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl LedgerState {
    /// Current snapshot format version.
    ///
    /// Bincode is positional and not self-describing — any change to
    /// `LedgerStateSnapshot` field types or order is a breaking format
    /// change.  Bump this constant on every structural change and any
    /// snapshot written by a prior binary will be quarantined on load
    /// (see `load_snapshot`); operators rebuild from ImmutableDB chunks.
    ///
    /// dugite is pre-1.0 and makes no snapshot back-compat guarantee:
    /// there is no migration shim and no `serde(default)` fallbacks on
    /// fields added after launch.
    /// v22 (#736): added `rupd_addrs_rew` (pv≤6 RUPD startStep capture)
    /// and `pending_avvm_return` — both previously dropped on load,
    /// breaking byte-exactness across mid-epoch restarts in pv≤6 epochs.
    /// v23 (#766): `ProtocolParameters::min_fee_ref_script_cost_per_byte`
    /// changed from `u64` to `Rational` (Conway `NonNegativeInterval`), so
    /// the bincode layout of every embedded `protocol_params` /
    /// `prev_protocol_params` grew by one `u64` (the denominator).
    /// v24 (#770): `CostModels` gained `unknown_cost_models:
    /// BTreeMap<u8, Vec<i64>>` (unknown-language entries preserved per Haskell
    /// `_costModelsUnknown`), which bincode serializes as a length-prefixed
    /// sequence — changing the layout of every embedded `protocol_params` /
    /// `prev_protocol_params`.
    /// v25 (#782): no bincode layout change here, but `LedgerSeq`'s rollback
    /// delta model (`crates/dugite-ledger/src/ledger_seq.rs`) gained snapshot
    /// coverage for several `LedgerState` fields (`genesis_delegates`, `era`,
    /// `pending_donations`, `opcert_counters`, pre-Conway PPUP proposal maps,
    /// pending MIR accumulators, `pointer_map`, `script_stake_credentials`,
    /// `total_stake_key_deposits`, `extra_entropy`) that the delta model
    /// previously omitted entirely. An anchor advanced by an OLDER binary
    /// (`LedgerSeq::advance_anchor` → `apply_delta_to_state`) could have
    /// silently regressed one of those fields to a stale value before being
    /// persisted here. Bumping the version quarantines any snapshot saved
    /// under the old (incomplete) delta model, forcing a clean rebuild from
    /// ImmutableDB replay instead of trusting a possibly-mis-advanced anchor.
    /// v26 (#799): `ProposalState` (embedded wholesale inside
    /// `GovernanceState`, which is bincode-serialized as part of every
    /// `LedgerState` snapshot) gained a new `submission_index: u64` field
    /// used to recover on-chain proposal submission order for the
    /// ratification tie-break sort. This is a positional bincode layout
    /// change for every embedded `ProposalState` (live `proposals`,
    /// `last_ratified`, and `PulsingSnapshot::proposals`) — a snapshot
    /// written by a prior binary must be quarantined, not silently
    /// misinterpreted.
    /// v27 (#796): `PendingRewardUpdate::delta_reserves` changed from `u64`
    /// to `i128` so a degraded/low-block epoch (`epoch_fees` exceeding
    /// `treasury_cut + total_distributed`) can represent Haskell's signed
    /// `deltaR` reserves credit instead of silently saturating it to 0.
    /// `PendingRewardUpdate` is embedded in every `LedgerState` snapshot via
    /// `EpochSubState::pending_reward_update` / `last_applied_rupd`, so this
    /// is a positional bincode layout change — a snapshot written by a
    /// prior binary must be quarantined, not silently misinterpreted as a
    /// `u64`.
    /// v28 (#804): `LedgerState` gained a new top-level field
    /// `future_gen_delegs` (Haskell `dsFutureGenDelegs`, the pending queue
    /// behind `Certificate::GenesisKeyDelegation`), added to
    /// `LedgerStateSnapshot` right after `genesis_delegates` — a positional
    /// bincode layout change for every snapshot. Also closes the live-path
    /// gap where `GenesisKeyDelegation` certs were silently dropped
    /// (`eras::common::apply_shelley_cert`'s catch-all), leaving
    /// `genesis_delegates` permanently stale for any chain that crossed a
    /// real genesis-delegate rotation.
    ///
    /// v29: `GovernanceState.votes_by_action` (and
    /// `PulsingSnapshot.votes_by_action`) changed its per-action value
    /// type from `Vec<(Voter, VotingProcedure)>` to
    /// `imbl::OrdMap<Voter, VotingProcedure>`. The linear `Vec` scan used to
    /// implement last-vote-wins was O(n) per vote, collapsing to O(n^2) on
    /// governance actions that accumulate hundreds of thousands of votes
    /// (observed on preprod: ~396k votes on a single InfoAction). The map
    /// gives O(log n) last-wins inserts, matching Haskell's `Map voter Vote`.
    /// Serialized layout changes for every snapshot with any recorded votes.
    /// v30 (#902): `ConsensusSubState` gained `previous_epoch_nonce`
    /// (`praosStatePreviousEpochNonce`). Snapshots written by v29 and earlier
    /// do not carry it, so they must be replayed rather than loaded.
    /// v31 (#919): `ProtocolParameters` (embedded in every `protocol_params` /
    /// `prev_protocol_params` field) gained `min_utxo_value: Lovelace` (flat
    /// Shelley/Allegra/Mary `minUTxOValue`) and `coins_per_utxo_word: Lovelace`
    /// (lossless Alonzo `coinsPerUTxOWord`) — both required for the per-era
    /// minimum-UTxO dispatch that fixes false `OutputTooSmall` rejections of
    /// real Shelley/Allegra/Mary mainnet transactions. Positional bincode
    /// layout change for every embedded `protocol_params`/`prev_protocol_params`.
    /// v32 (#966): `PulsingSnapshot` (embedded in `GovernanceState`, which
    /// is bincode-serialized as part of every `LedgerState` snapshot) gained
    /// `treasury: u64` — Haskell `ensTreasury`, the frozen pot that
    /// `withdrawalCanWithdraw` gates `TreasuryWithdrawals` against. It was the
    /// one `dpEnactState` term never captured, so ratification fell back to the
    /// LIVE `epochs.treasury`, which by that point already included the current
    /// boundary's `applyRUpd`. Haskell's is sealed one boundary earlier, so a
    /// withdrawal that became affordable at boundary B enacted at B on dugite
    /// and at B+1 on cardano-node. Positional bincode layout change: snapshots
    /// written by v31 and earlier must be replayed, not loaded.
    // 33: #988 added `GovernanceState.pulsed_ratify_state` — the frozen DRep
    // pulser result. A positional bincode change inside `GovernanceState`, so
    // existing snapshots cannot supply it and must be rejected.
    // 34: #977 added `GovernanceState.future_pparams`.
    // 35: #988 step 2 — the epoch boundary now APPLIES `pulsed_ratify_state`
    //     instead of recomputing the ratification there, so the stored plan
    //     stopped being advisory and became consensus-bearing. Its `expired`
    //     field also changed meaning: it used to be `last_expired`, i.e. the
    //     ids actually removed INCLUDING descendants, and is now Haskell's
    //     `rsExpired` — only the candidates that failed ratification while past
    //     their expiry, with descendant expansion left to
    //     `proposalsApplyEnactment`. The layout is unchanged, so a v34 snapshot
    //     would load and its plan would be applied verbatim; rejecting them is
    //     what stops a plan decided under the old semantics from driving a
    //     boundary under the new ones.
    // 36: #988 step 3 — the five ad-hoc frozen fields
    //     (`drep_distribution_snapshot` + its two companions,
    //     `ratification_snapshot`, `pulsed_ratify_state`) collapsed into one
    //     `drep_pulsing_state: Option<DRepPulsingState>`, Haskell's
    //     `DRComplete PulsingSnapshot RatifyState`. A positional bincode
    //     layout change; a v35 snapshot would mis-deserialize, and v35 DID
    //     reach disk during validation.
    // 37: #994 added the two `PulsingSnapshot` presence flags. A scalar total
    //     cannot distinguish "no account delegates to AlwaysAbstain" from
    //     "accounts delegate zero stake", and upstream's `psDRepDistr` is one
    //     map whose keys exist only for the former. Positional bincode change.
    // 38: #1067 added `EpochSubState.non_myopic` (`EpochState.esNonMyopic`) and
    //     `PendingRewardUpdate.non_myopic` (`RewardUpdate.nonMyopic`). Two
    //     positional bincode additions, so a v37 snapshot cannot supply them.
    //     Neither field is reconstructible after the fact: `likelihoodsNM` is a
    //     0.9-decayed accumulator folded over every past epoch, and
    //     `rewardPotNM` is `_R` from a boundary whose inputs (reserves, fees,
    //     eta) have since moved. A lazy backfill would therefore not converge
    //     for ~20 epochs while looking authoritative the whole time — the #979
    //     failure mode — so existing DBs replay chunks instead.
    //     #1073 EXTENDS 38 in place with `PulsedRatifyState.enact_state`
    //     (`EnactedGovTerms`) — the `ensCommittee` / `ensConstitution` /
    //     `ensPrevGovActionIds` half of `rsEnactState`, which upstream returns
    //     from RATIFY alongside the `ensCurPParams` dugite already stored.
    //     Another positional bincode addition. It extends rather than bumps for
    //     the reason `xtask/tests/snapshot_one_bump_invariant.rs` exists: 38 is
    //     committed and gate-validated but NOT tagged, so no released artefact
    //     carries the narrower layout and operators replay exactly once. A
    //     local pre-change v38 database is NOT protected by that and must be
    //     re-replayed — the version number cannot reject a layout that shares
    //     its number.
    //
    //     #1085 extends 38 again with `DRepRegistration.delegs` — Haskell's
    //     `drepDelegs`, the reverse index `ConwayUnRegDRep` uses to clear its
    //     delegators. Same reasoning, same untagged window. The Mithril import
    //     path rebuilds it from the forward map, which is exact at PV10+; a
    //     chunk replay builds it from the certificates directly.
    //     (Commits landing this referred to it as "#1084" before the 2026-08-14
    //     renumbering recorded in CLAUDE.md; #1084 itself is the unrelated
    //     Byron delegation/update-state issue this file's v39 entry is about.)
    //
    // 39: v2.8.0 tagged 38 with ONLY the #1067/#1073/#1085 layout above. The
    //     one-bump plan `xtask/tests/snapshot_one_bump_invariant.rs` protected
    //     — "extend 38 in place while nothing tagged carries it" — is void the
    //     moment a release does, so this is a real bump, not another
    //     extension. That guard is deleted (see its own final commit message
    //     for what replaces it); this comment block is now what future
    //     extensions append to instead, exactly as 37 and 38 did before it.
    //
    //     39 ALSO carries #1088: every map/set field reachable from
    //     `LedgerStateSnapshot` now writes in key order (`BTreeMap`/`BTreeSet`,
    //     or a `*Wire` mirror struct for a type shared with live state) instead
    //     of whatever order `HashMap`/`imbl::HashMap` happened to iterate in.
    //     Not a positional field-count change — a `HashMap<K,V>` and a
    //     `BTreeMap<K,V>` holding the same entries decode identically either
    //     way, since bincode's map decode just reads length + pairs and
    //     inserts them — but it ships under 39 rather than as a drop-in fix
    //     because it lands in the same re-sync as #1067/#1073/#1085 and
    //     because "the bytes decode the same" is not the property that
    //     matters here: TWO NODES with identical state used to write DIFFERENT
    //     bytes, and that is now fixed for every reachable field, verified via
    //     `snapshot_format_hash_stability` in
    //     `crates/dugite-ledger/tests/snapshot_stability.rs`.
    //
    //     39 ALSO carries #1084: `LedgerStateSnapshot` gains `byron`, mirroring
    //     the new top-level `LedgerState.byron: ByronSubState` field — Byron's
    //     `UPI.State` (update-proposal system: registered proposals, votes,
    //     endorsements, candidate protocol updates, the adopted protocol-
    //     parameter record) and `DI.State` (heavyweight delegation: the active
    //     `delegator -> delegate` bimap plus its scheduling queue), the two
    //     `ChainValidationState` fields Byron carries beyond the UTxO set
    //     already modelled. Neither is reconstructible after the fact: a
    //     resumed node with no delegation map cannot verify Byron block
    //     issuers' authority, and a resumed node with no adopted-parameters
    //     record would silently regress `byronProtocolParams` to genesis
    //     values on every restart mid-Byron — which is precisely the #979
    //     failure mode `EpochSubState.non_myopic` was bumped to avoid in v38.
    //     A positional bincode addition, so it ships under the SAME
    //     version 39 rather than a further bump — 39 was bumped this same
    //     session specifically anticipating both this and #1071's addition
    //     (see that commit's message). Already `BTreeMap`/`BTreeSet`-backed
    //     end to end on the LIVE type (no `*Wire` mirror needed, unlike the
    //     fields #1088 above had to retrofit), so #1088's determinism
    //     property holds for it from the start.
    pub(crate) const SNAPSHOT_VERSION: u8 = 39;

    /// The current on-disk snapshot layout version.
    ///
    /// A thin public wrapper around [`Self::SNAPSHOT_VERSION`] (itself
    /// `pub(crate)` so callers outside this crate cannot depend on the exact
    /// number for anything other than the check below). Exists so
    /// `crates/dugite-ledger/tests/snapshot_stability.rs` — an external
    /// integration-test crate — can tie its pinned layout hash to the
    /// version it was computed against, replacing
    /// `xtask/tests/snapshot_one_bump_invariant.rs` (deleted: its `git
    /// tag`-based mechanism was vacuous under CI's shallow checkouts, which
    /// carry no tags). See that test for the full design.
    pub fn snapshot_version() -> u8 {
        Self::SNAPSHOT_VERSION
    }

    /// Save ledger state snapshot to disk using bincode serialization.
    ///
    /// Format: `[4-byte magic "DUGT"][1-byte version][32-byte blake2b checksum][bincode data]`
    ///
    /// The write is atomic: data is written to a `.tmp` file and then renamed
    /// over the final path so that a crash mid-write does not produce a partial
    /// or corrupt snapshot file.
    ///
    /// The payload is streamed through a two-pass write:
    ///
    ///   1. First pass: `bincode::serialize_into` writes directly to the
    ///      temp file via a buffered writer while simultaneously feeding
    ///      every byte into an incremental blake2b hasher. No full
    ///      in-memory `Vec<u8>` copy of the snapshot is ever produced.
    ///   2. Second pass: seek back to the header slot and overwrite the
    ///      placeholder checksum with the computed hash.
    ///
    /// Prior to #403 this function allocated a single contiguous `Vec<u8>`
    /// via `bincode::serialize(&snapshot)` before writing it out. At
    /// preview scale (~3M UTxOs in the in-memory backend) that allocation
    /// was multiple GB and contributed materially to the post-replay OOM
    /// that killed dugite-node on 32 GB hosts.
    pub fn save_snapshot(&self, path: &Path) -> Result<(), LedgerError> {
        // Build the serde-friendly snapshot view (Arc::clone for the big
        // shared maps, HashMap::clone for the rest) and delegate to the
        // off-lock-friendly writer. Issue #649 (`save_ledger_snapshot` on
        // the node side) takes the same `snapshot` view inside the lock and
        // performs `write_snapshot_view_to_path` from a `spawn_blocking`
        // task — see crates/dugite-node/src/node/epoch.rs.
        let snapshot = super::snapshot_format::LedgerStateSnapshot::from(self);
        // The in-memory `utxo_set` is empty exactly when an LSM store is
        // attached (UTxOs live on-disk); derive the backend tag from that.
        let backend = if self.utxo.utxo_set.has_store() {
            SnapshotBackend::DugiteLsm
        } else {
            SnapshotBackend::DugiteMem
        };
        let total_bytes = Self::write_snapshot_view_to_path(&snapshot, path, backend)?;
        info!(
            "Snapshot     saved (epoch={}, {} UTxOs, {:.1} MB)",
            self.epoch.0,
            self.utxo.utxo_set.len(),
            total_bytes as f64 / 1_048_576.0,
        );
        Ok(())
    }

    /// Serialise a `LedgerStateSnapshot` to `path` using the canonical
    /// versioned format described above. Returns the total bytes written
    /// (header + checksum + payload) so the caller can report metrics.
    ///
    /// **Issue #649** — this is the lock-free factor of [`Self::save_snapshot`].
    /// `LedgerStateSnapshot` is `Send + 'static` (HashMaps + Arcs), so the
    /// node's snapshot path captures the view inside its `LedgerState`
    /// write lock and then hands it to `tokio::task::spawn_blocking` for
    /// the disk-bound write, releasing the lock seconds earlier.
    ///
    /// Atomicity / crash safety is preserved unchanged: the payload is
    /// streamed to `<path>.tmp` via `HashingWriter` (no full `Vec<u8>` in
    /// memory — see #403), the checksum placeholder is rewritten by
    /// seeking back to its offset, the file is flushed, and the `.tmp`
    /// file is atomically renamed over `path`. A crash before the rename
    /// leaves `path` untouched; a crash after leaves a consistent file.
    pub fn write_snapshot_view_to_path(
        snapshot: &super::snapshot_format::LedgerStateSnapshot,
        path: &Path,
        backend: SnapshotBackend,
    ) -> Result<u64, LedgerError> {
        use dugite_primitives::hash::Blake2b256Hasher;
        use std::io::{Seek, SeekFrom, Write};

        let tmp_path = path.with_extension("tmp");

        let file = std::fs::File::create(&tmp_path)
            .map_err(|e| LedgerError::EpochTransition(format!("Failed to create snapshot: {e}")))?;
        let mut writer = std::io::BufWriter::with_capacity(1 << 20, file);

        // Header: "DUGT" (4) + version (1) + blake2b placeholder (32).
        // We fill the checksum slot with zeros, remember its offset, and
        // rewrite it once the payload has been streamed and hashed.
        writer.write_all(b"DUGT").map_err(|e| {
            LedgerError::EpochTransition(format!("Failed to write snapshot header: {e}"))
        })?;
        writer.write_all(&[Self::SNAPSHOT_VERSION]).map_err(|e| {
            LedgerError::EpochTransition(format!("Failed to write snapshot version: {e}"))
        })?;
        const CHECKSUM_LEN: usize = 32;
        let checksum_offset: u64 = 4 + 1;
        writer.write_all(&[0u8; CHECKSUM_LEN]).map_err(|e| {
            LedgerError::EpochTransition(format!("Failed to write checksum placeholder: {e}"))
        })?;

        // Stream the payload through a `HashingWriter` that forwards writes
        // to the buffered file while updating a blake2b-256 hasher in-line.
        // `bincode::serialize_into` never materialises the whole payload in
        // memory — it walks the struct and emits bytes incrementally.
        let mut hashing_writer = HashingWriter {
            inner: &mut writer,
            hasher: Blake2b256Hasher::new(),
            bytes_written: 0,
        };
        bincode::serialize_into(&mut hashing_writer, snapshot).map_err(|e| {
            LedgerError::EpochTransition(format!("Failed to serialize ledger state: {e}"))
        })?;
        let payload_bytes = hashing_writer.bytes_written;
        let checksum = hashing_writer.hasher.finalize();

        // Rewrite the checksum placeholder now that the payload hash is
        // known.  Flush afterwards so the file on disk is consistent before
        // the atomic rename.
        writer.flush().map_err(|e| {
            LedgerError::EpochTransition(format!("Failed to flush snapshot payload: {e}"))
        })?;
        writer
            .seek(SeekFrom::Start(checksum_offset))
            .map_err(|e| LedgerError::EpochTransition(format!("Failed to seek snapshot: {e}")))?;
        writer.write_all(checksum.as_bytes()).map_err(|e| {
            LedgerError::EpochTransition(format!("Failed to write snapshot checksum: {e}"))
        })?;
        writer
            .flush()
            .map_err(|e| LedgerError::EpochTransition(format!("Failed to flush snapshot: {e}")))?;
        drop(writer);

        let total_bytes = 4 + 1 + CHECKSUM_LEN as u64 + payload_bytes;

        std::fs::rename(&tmp_path, path)
            .map_err(|e| LedgerError::EpochTransition(format!("Failed to rename snapshot: {e}")))?;

        // Write the backend meta sidecar (mirrors Haskell's per-snapshot
        // `SnapshotMetadata` with its backend tag). Emitted AFTER the atomic
        // `.bin` rename so a crash between the two leaves a consistent `.bin`
        // that `load_snapshot` can still read — a missing sidecar is handled
        // by the back-compat inference path in `check_snapshot_backend_match`.
        let state_checksum: String = checksum
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let meta = SnapshotMeta {
            backend: backend.as_tag().to_string(),
            snapshot_version: Self::SNAPSHOT_VERSION,
            slot: snapshot.tip.point.slot().map(|s| s.0).unwrap_or(0),
            epoch: snapshot.epoch.0,
            utxo_count: snapshot.utxo_set.len() as u64,
            state_checksum,
        };
        if let Err(e) = meta.write_atomic(path) {
            // Non-fatal: the `.bin` is consistent on disk; a missing sidecar
            // falls through to backend inference on load. Log loudly so the
            // operator notices a half-written snapshot dir.
            warn!(
                path = %path.display(),
                "Failed to write snapshot meta sidecar: {e}"
            );
        }
        Ok(total_bytes)
    }

    /// Load ledger state snapshot from disk.
    ///
    /// Rejects snapshots larger than [`MAX_SNAPSHOT_SIZE`] to prevent OOM.
    ///
    /// Supports three formats:
    /// - **Versioned (v1+):** `DUGT` + version byte + 32-byte checksum + data
    /// - **Legacy with checksum:** `DUGT` + 32-byte checksum + data (no version byte)
    /// - **Legacy raw:** plain bincode without any header
    pub fn load_snapshot(path: &Path) -> Result<Self, LedgerError> {
        let raw = std::fs::read(path)
            .map_err(|e| LedgerError::EpochTransition(format!("Failed to read snapshot: {e}")))?;

        // Reject oversized snapshot files to prevent OOM from malicious data
        if raw.len() > MAX_SNAPSHOT_SIZE {
            return Err(LedgerError::EpochTransition(format!(
                "Snapshot size {} exceeds maximum allowed size {}",
                raw.len(),
                MAX_SNAPSHOT_SIZE
            )));
        }

        // Snapshot framing is `DUGT(4) + version(1) + checksum(32) + payload`.
        // No back-compat: any other shape or mismatched version is rejected
        // and quarantined so the caller can rebuild from ImmutableDB chunks.
        if raw.len() < 37 || &raw[..4] != b"DUGT" {
            return Err(LedgerError::EpochTransition(
                "Snapshot is missing the DUGT framing header — delete the \
                 snapshot to re-sync from chain"
                    .to_string(),
            ));
        }
        let version = raw[4];
        if version != Self::SNAPSHOT_VERSION {
            // Quarantine the file so the same unreadable bytes do not get
            // retried on the next restart; ChainDB is untouched so the
            // ledger will be rebuilt by replaying chunks.
            quarantine_unreadable_snapshot(path, version);
            return Err(LedgerError::EpochTransition(format!(
                "Snapshot version {version} does not match the current \
                 SNAPSHOT_VERSION {}. The unreadable snapshot has been \
                 quarantined; the ledger will be rebuilt by replaying \
                 ImmutableDB chunks.",
                Self::SNAPSHOT_VERSION,
            )));
        }
        let stored_checksum = &raw[5..37];
        let payload = &raw[37..];
        let computed = dugite_primitives::hash::blake2b_256(payload);
        if computed.as_bytes() != stored_checksum {
            return Err(LedgerError::EpochTransition(
                "Snapshot checksum mismatch — file may be corrupted".to_string(),
            ));
        }
        debug!(version, "Loading versioned snapshot");
        let data = payload;

        // Use bincode options with size limit as defense-in-depth against
        // malicious payloads that encode enormous internal allocations.
        // Must use with_fixint_encoding() to match bincode::serialize() defaults.
        use bincode::Options;
        let snapshot: super::snapshot_format::LedgerStateSnapshot = bincode::options()
            .with_fixint_encoding()
            .allow_trailing_bytes()
            .with_limit(MAX_SNAPSHOT_SIZE as u64)
            .deserialize(data)
            .map_err(|e| {
                LedgerError::EpochTransition(format!("Failed to deserialize ledger state: {e}"))
            })?;
        let mut state = LedgerState::from(snapshot);
        state.utxo.utxo_set.rebuild_address_index();
        // Re-enable indexing so subsequent insert/remove operations maintain the index.
        // The #[serde(skip)] on indexing_enabled defaults to false after deserialization.
        state.utxo.utxo_set.set_indexing_enabled(true);
        // After loading a snapshot, incremental stake tracking may have drifted.
        // Rebuild stake distribution from the full UTxO set, then recompute
        // pool_stake for all existing snapshots (mark/set/go).
        //
        // IMPORTANT: Only run if the UTxO set is non-empty. When using an LSM-backed
        // UTxO store, the store hasn't been attached yet at this point — the in-memory
        // set is empty. Running rebuild_stake_distribution on an empty set would wipe
        // all pool_stake values, causing block producers to see zero stake. The caller
        // (dugite-node) runs rebuild + recompute again AFTER attaching the LSM store.
        if !state.utxo.utxo_set.is_empty() {
            state.rebuild_stake_distribution();
            state.recompute_snapshot_pool_stakes();
        }
        // Trigger one full rebuild at the next epoch boundary to correct any drift
        // from the snapshot (which may have been saved with stale incremental state).
        // After that single rebuild, incremental tracking takes over.
        state.epochs.needs_stake_rebuild = true;
        // After loading a snapshot, the node is past genesis — RUPD should fire
        // at the next epoch boundary.
        state.epochs.snapshots.rupd_ready = true;
        debug!(
            "Snapshot loaded from {} ({:.1} MB, {} UTxOs, epoch {})",
            path.display(),
            raw.len() as f64 / 1_048_576.0,
            state.utxo.utxo_set.len(),
            state.epoch.0,
        );
        Ok(state)
    }

    /// Save the attached UTxO store's LSM snapshot.
    ///
    /// Call this after `save_snapshot()` when using on-disk UTxO storage.
    /// Requires mutable access because `LsmTree::save_snapshot` is `&mut self`.
    pub fn save_utxo_snapshot(&mut self) -> Result<(), LedgerError> {
        if let Some(store) = self.utxo.utxo_set.store_mut() {
            // Delete any existing snapshot first to avoid "already exists" error
            let _ = store.delete_snapshot("ledger");
            store.save_snapshot("ledger").map_err(|e| {
                LedgerError::EpochTransition(format!("Failed to save UTxO store snapshot: {e}"))
            })?;
            debug!("UTxO store snapshot saved ({} entries)", store.len());
        }
        Ok(())
    }

    /// Attach an on-disk UTxO store to this ledger state.
    ///
    /// All subsequent UTxO operations will use the LSM-backed store.
    /// If the ledger has in-memory UTxOs (from bincode snapshot load),
    /// they are migrated to the store before attachment.
    pub fn attach_utxo_store(&mut self, mut store: crate::utxo_store::UtxoStore) {
        // Migrate any in-memory UTxOs to the store. `iter()` used to
        // materialise every entry into a throw-away `Vec` before the copy
        // loop; at preview scale (~3M UTxOs) that intermediate buffer was
        // multi-GB.  Stream the HashMap directly instead (#403).
        if !self.utxo.utxo_set.is_empty() && !self.utxo.utxo_set.has_store() {
            let count = self.utxo.utxo_set.len();
            tracing::info!("Migrating {} in-memory UTxOs to on-disk store", count);
            self.utxo.utxo_set.scan_all(|input, output| {
                store.insert(input.clone(), output.clone());
            });
        }
        store.set_indexing_enabled(true);
        store.rebuild_address_index();
        self.utxo.utxo_set.attach_store(store);
        tracing::info!("UTxO store attached ({} entries)", self.utxo.utxo_set.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LedgerStateSnapshot;
    use dugite_primitives::era::Era;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::EpochNo;
    use dugite_primitives::value::Lovelace;

    // -----------------------------------------------------------------------
    // 1. Save/load roundtrip: verify that key fields survive serialisation
    // -----------------------------------------------------------------------

    // ── Issue #649: write_snapshot_view_to_path tests ─────────────────

    /// `write_snapshot_view_to_path` is the off-lock-friendly factor of
    /// `save_snapshot`. Given a `LedgerStateSnapshot`, it must produce a
    /// byte-identical on-disk file that `load_snapshot` can read back to
    /// the same field values.
    #[test]
    fn test_write_snapshot_view_roundtrips_via_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("view-roundtrip.bin");

        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.epoch = EpochNo(11);
        state.epochs.treasury = Lovelace(123_456_789);
        state.era = Era::Conway;

        let view = LedgerStateSnapshot::from(&state);
        let bytes =
            LedgerState::write_snapshot_view_to_path(&view, &path, SnapshotBackend::DugiteMem)
                .expect("static write must succeed");
        assert!(bytes > 0, "writer must report payload bytes written");

        let loaded = LedgerState::load_snapshot(&path).unwrap();
        assert_eq!(loaded.epoch, EpochNo(11));
        assert_eq!(loaded.epochs.treasury, Lovelace(123_456_789));
        assert_eq!(loaded.era, Era::Conway);
    }

    /// `save_snapshot(&self)` and `write_snapshot_view_to_path(&view)`
    /// must produce byte-identical output when given the same logical
    /// state. The whole point of factoring is for `save_ledger_snapshot`
    /// on the node side to clone a `Snapshot` under the lock and drive
    /// the disk write off the lock with no semantic change.
    #[test]
    fn test_write_view_byte_identical_to_save_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("via-save.bin");
        let p2 = dir.path().join("via-view.bin");

        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.epoch = EpochNo(3);
        state.epochs.treasury = Lovelace(999_999);
        state.era = Era::Conway;

        state.save_snapshot(&p1).unwrap();
        let view = LedgerStateSnapshot::from(&state);
        LedgerState::write_snapshot_view_to_path(&view, &p2, SnapshotBackend::DugiteMem).unwrap();

        let bytes1 = std::fs::read(&p1).unwrap();
        let bytes2 = std::fs::read(&p2).unwrap();
        assert_eq!(
            bytes1, bytes2,
            "static write must produce byte-identical output to save_snapshot"
        );
    }

    // ── Backend meta-tag + mismatch guard (Haskell-mirror, Phase 1) ──────

    #[test]
    fn test_snapshot_backend_tag_roundtrip() {
        for b in [SnapshotBackend::DugiteMem, SnapshotBackend::DugiteLsm] {
            assert_eq!(SnapshotBackend::from_tag(b.as_tag()), Some(b));
        }
        assert_eq!(SnapshotBackend::from_tag("not-a-backend"), None);
    }

    #[test]
    fn test_meta_sidecar_written_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger-snapshot-epoch5-slot999.bin");
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.epoch = EpochNo(5);
        let view = LedgerStateSnapshot::from(&state);
        LedgerState::write_snapshot_view_to_path(&view, &path, SnapshotBackend::DugiteLsm).unwrap();

        // The `.bin.meta.json` sidecar must exist and roundtrip.
        let meta = SnapshotMeta::load(&path).expect("sidecar must exist after write");
        assert_eq!(meta.backend, "dugite-lsm");
        assert_eq!(meta.snapshot_version, LedgerState::SNAPSHOT_VERSION);
        // blake2b-256 → 64 hex chars.
        assert_eq!(meta.state_checksum.len(), 64);
    }

    #[test]
    fn test_guard_rejects_backend_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger-snapshot.bin");
        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let view = LedgerStateSnapshot::from(&state);
        // Snapshot tagged as the LSM backend.
        LedgerState::write_snapshot_view_to_path(&view, &path, SnapshotBackend::DugiteLsm).unwrap();

        // Loading under the in-memory backend is a mismatch (must be rejected).
        assert!(matches!(
            check_snapshot_backend_match(&path, &state, dir.path(), SnapshotBackend::DugiteMem),
            BackendCheckResult::Mismatch { .. }
        ));
        // Loading under the matching LSM backend is fine.
        assert_eq!(
            check_snapshot_backend_match(&path, &state, dir.path(), SnapshotBackend::DugiteLsm),
            BackendCheckResult::Ok
        );
    }

    #[test]
    fn test_guard_mem_snapshot_under_lsm_is_convertible() {
        // The mithril-import path always writes a `dugite-mem` snapshot (inline
        // UTxOs). Loading it under the LSM backend must NOT be a hard mismatch
        // (which would force a from-genesis replay); it is `Convertible`, and
        // the node migrates the inline UTxOs into the LSM store at attach time.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger-snapshot.bin");
        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let view = LedgerStateSnapshot::from(&state);
        LedgerState::write_snapshot_view_to_path(&view, &path, SnapshotBackend::DugiteMem).unwrap();

        assert!(
            matches!(
                check_snapshot_backend_match(&path, &state, dir.path(), SnapshotBackend::DugiteLsm),
                BackendCheckResult::Convertible {
                    snapshot_backend: SnapshotBackend::DugiteMem,
                    configured_backend: SnapshotBackend::DugiteLsm,
                }
            ),
            "a dugite-mem snapshot under the LSM backend must be convertible, not rejected"
        );
        // The same mem snapshot under the mem backend is a plain match.
        assert_eq!(
            check_snapshot_backend_match(&path, &state, dir.path(), SnapshotBackend::DugiteMem),
            BackendCheckResult::Ok
        );
    }

    #[test]
    fn test_guard_lsm_snapshot_under_mem_is_hard_mismatch() {
        // The reverse direction is NOT convertible: an LSM snapshot keeps its
        // UTxOs in the adjacent `utxo-store/` tree, not inline, so loading it
        // under the in-memory backend must be a hard mismatch (matches Haskell
        // `V2/InMemory.hs`, which rejects any non-`UTxOHDMemSnapshot`).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger-snapshot.bin");
        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let view = LedgerStateSnapshot::from(&state);
        LedgerState::write_snapshot_view_to_path(&view, &path, SnapshotBackend::DugiteLsm).unwrap();

        assert!(matches!(
            check_snapshot_backend_match(&path, &state, dir.path(), SnapshotBackend::DugiteMem),
            BackendCheckResult::Mismatch {
                snapshot_backend: SnapshotBackend::DugiteLsm,
                configured_backend: SnapshotBackend::DugiteMem,
            }
        ));
    }

    #[test]
    fn test_guard_backcompat_missing_sidecar_infers_backend() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger-snapshot.bin");
        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let view = LedgerStateSnapshot::from(&state);
        LedgerState::write_snapshot_view_to_path(&view, &path, SnapshotBackend::DugiteLsm).unwrap();
        // Simulate a pre-meta snapshot (e.g. existing db-mainnet): drop the sidecar.
        std::fs::remove_file(SnapshotMeta::sidecar_path(&path)).unwrap();
        // Empty in-mem utxo_set + a `utxo-store` dir ⇒ inferred DugiteLsm.
        std::fs::create_dir_all(dir.path().join("utxo-store")).unwrap();
        assert_eq!(
            check_snapshot_backend_match(&path, &state, dir.path(), SnapshotBackend::DugiteLsm),
            BackendCheckResult::Ok,
            "no-sidecar LSM db must still load under LSM (back-compat)"
        );
        assert!(matches!(
            check_snapshot_backend_match(&path, &state, dir.path(), SnapshotBackend::DugiteMem),
            BackendCheckResult::Mismatch { .. }
        ));
    }

    #[test]
    fn test_guard_backcompat_indeterminate_is_accepted() {
        // Empty utxo_set, no utxo-store dir, no sidecar ⇒ cannot infer ⇒
        // accept provisionally under any backend (do not reject).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger-snapshot.bin");
        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let view = LedgerStateSnapshot::from(&state);
        LedgerState::write_snapshot_view_to_path(&view, &path, SnapshotBackend::DugiteMem).unwrap();
        std::fs::remove_file(SnapshotMeta::sidecar_path(&path)).unwrap();
        assert_eq!(
            check_snapshot_backend_match(&path, &state, dir.path(), SnapshotBackend::DugiteLsm),
            BackendCheckResult::Ok
        );
    }

    /// Save a `LedgerState` with recognisable field values, load it back, and
    /// verify that `epoch`, `treasury`, and `era` are preserved exactly.
    #[test]
    fn test_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.bin");

        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.epoch = EpochNo(7);
        state.epochs.treasury = Lovelace(42_000_000);
        state.era = Era::Conway;

        state.save_snapshot(&path).unwrap();
        let loaded = LedgerState::load_snapshot(&path).unwrap();

        assert_eq!(loaded.epoch, EpochNo(7), "epoch must survive roundtrip");
        assert_eq!(
            loaded.epochs.treasury,
            Lovelace(42_000_000),
            "treasury must survive roundtrip"
        );
        assert_eq!(loaded.era, Era::Conway, "era must survive roundtrip");
    }

    // -----------------------------------------------------------------------
    // 2. Magic bytes: first 4 bytes of the on-disk file must be b"DUGT"
    // -----------------------------------------------------------------------

    /// Save a snapshot and verify that the raw on-disk file starts with the
    /// expected `DUGT` magic word.
    #[test]
    fn test_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("magic.bin");

        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.save_snapshot(&path).unwrap();

        let raw = std::fs::read(&path).unwrap();
        assert!(raw.len() >= 4, "snapshot file must be at least 4 bytes");
        assert_eq!(&raw[..4], b"DUGT", "first 4 bytes must be magic word DUGT");
    }

    // -----------------------------------------------------------------------
    // 3. Checksum verification: stored checksum matches blake2b-256 of payload
    // -----------------------------------------------------------------------

    /// Save a snapshot, then manually re-derive the blake2b-256 checksum over
    /// the payload region (bytes 37..) and assert it equals the stored
    /// checksum (bytes 5..37).
    #[test]
    fn test_checksum_verification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checksum.bin");

        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.save_snapshot(&path).unwrap();

        let raw = std::fs::read(&path).unwrap();

        // Header layout: DUGT(4) + version(1) + checksum(32) + payload(N)
        assert!(
            raw.len() > 37,
            "snapshot must be longer than the 37-byte header"
        );
        let stored_checksum = &raw[5..37];
        let payload = &raw[37..];
        let computed = dugite_primitives::hash::blake2b_256(payload);
        assert_eq!(
            computed.as_bytes(),
            stored_checksum,
            "stored checksum must equal blake2b-256(payload)"
        );
    }

    // -----------------------------------------------------------------------
    // 4. Corrupted data detected: flipping a payload byte must cause an error
    // -----------------------------------------------------------------------

    /// Save a snapshot, flip a single byte in the payload region, then attempt
    /// to load it — the checksum mismatch must produce a `LedgerError`.
    #[test]
    fn test_corrupted_data_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.bin");

        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.save_snapshot(&path).unwrap();

        // Flip a byte in the payload (byte 40 is well within the payload region).
        let mut raw = std::fs::read(&path).unwrap();
        assert!(
            raw.len() > 40,
            "snapshot must be long enough to corrupt byte 40"
        );
        raw[40] ^= 0xFF;
        std::fs::write(&path, &raw).unwrap();

        let result = LedgerState::load_snapshot(&path);
        assert!(
            result.is_err(),
            "loading a corrupted snapshot must return an error"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("checksum") || msg.contains("corrupt"),
            "error message must mention checksum or corruption, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // 5. Size limit enforcement: MAX_SNAPSHOT_SIZE constant and reject path
    // -----------------------------------------------------------------------

    /// Verify that `MAX_SNAPSHOT_SIZE` is 10 GiB and that a file whose raw
    /// length exceeds the limit is rejected before any deserialisation attempt.
    #[test]
    fn test_size_limit_enforcement() {
        // The constant must be exactly 10 GiB.
        assert_eq!(
            MAX_SNAPSHOT_SIZE,
            10 * 1024 * 1024 * 1024,
            "MAX_SNAPSHOT_SIZE must be 10 GiB"
        );

        // Write a tiny file whose first 8 bytes encode a length field larger
        // than MAX_SNAPSHOT_SIZE — the raw-bytes size check triggers first.
        // We achieve this by writing (MAX_SNAPSHOT_SIZE + 1) bytes so that
        // the check `raw.len() > MAX_SNAPSHOT_SIZE` fires immediately.
        //
        // Writing 10 GiB to disk is impractical in a unit test, so instead
        // we construct a file that *claims* (via its bincode length prefix)
        // to contain an enormous allocation.  The with_limit() guard inside
        // load_snapshot rejects it at the deserialization stage.
        let dir = tempfile::tempdir().unwrap();
        let malicious_path = dir.path().join("malicious.bin");

        // Raw bincode (no DUGT header, so raw path taken): a u64 length
        // that exceeds MAX_SNAPSHOT_SIZE.
        let huge_len: u64 = (MAX_SNAPSHOT_SIZE as u64) + 1;
        let mut payload = huge_len.to_le_bytes().to_vec();
        payload.extend_from_slice(&[0u8; 64]); // padding so the file exists
        std::fs::write(&malicious_path, &payload).unwrap();

        let result = LedgerState::load_snapshot(&malicious_path);
        // The file is < MAX_SNAPSHOT_SIZE bytes so the raw-size gate passes,
        // but bincode's with_limit() should reject the giant allocation.
        assert!(
            result.is_err(),
            "a snapshot claiming a huge allocation must be rejected"
        );
    }

    // -----------------------------------------------------------------------
    // 6. Header-less snapshots must be rejected (no back-compat path)
    // -----------------------------------------------------------------------

    /// A plain bincode-serialised snapshot without the `DUGT` framing header
    /// must be rejected by `load_snapshot`. Pre-1.0 dugite makes no snapshot
    /// back-compat guarantee; the legacy raw-bincode loader was removed
    /// along with the rest of the snapshot back-compat machinery.
    #[test]
    fn test_unframed_snapshot_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-raw.bin");

        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.epoch = EpochNo(3);

        let snapshot = LedgerStateSnapshot::from(&state);
        let raw_bincode = bincode::serialize(&snapshot).unwrap();
        std::fs::write(&path, &raw_bincode).unwrap();

        let err = LedgerState::load_snapshot(&path).unwrap_err();
        assert!(
            matches!(err, LedgerError::EpochTransition(_)),
            "expected EpochTransition error, got {err:?}",
        );
    }

    // -----------------------------------------------------------------------
    // 7. Version byte in header: byte at position 4 must equal SNAPSHOT_VERSION
    // -----------------------------------------------------------------------

    /// Save a snapshot and assert that byte 4 (the version field) equals
    /// the current `SNAPSHOT_VERSION` constant (whatever it happens to be).
    #[test]
    fn test_version_in_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("version.bin");

        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.save_snapshot(&path).unwrap();

        let raw = std::fs::read(&path).unwrap();
        assert!(raw.len() > 4, "snapshot must be longer than 4 bytes");
        assert_eq!(
            raw[4],
            LedgerState::SNAPSHOT_VERSION,
            "byte 4 must be SNAPSHOT_VERSION ({})",
            LedgerState::SNAPSHOT_VERSION
        );
    }

    // -----------------------------------------------------------------------
    // 8. Atomic write: the .tmp file must NOT exist after save completes
    // -----------------------------------------------------------------------

    /// Save a snapshot and verify that the `.tmp` staging file has been
    /// renamed away and does not exist on disk after `save_snapshot` returns.
    #[test]
    fn test_atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atomic.bin");
        let tmp_path = path.with_extension("tmp");

        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.save_snapshot(&path).unwrap();

        // The final file must exist.
        assert!(
            path.exists(),
            "final snapshot file must exist after save_snapshot"
        );
        // The temporary staging file must have been renamed away.
        assert!(
            !tmp_path.exists(),
            ".tmp staging file must not exist after save_snapshot completes (atomic rename)"
        );
    }

    // -----------------------------------------------------------------------
    // 9. Version-mismatch quarantine (issue #609)
    // -----------------------------------------------------------------------
    //
    // When the on-disk SNAPSHOT_VERSION is older than the current code's
    // SNAPSHOT_VERSION, the bincode layout is not forward-compatible: any
    // field added, removed, or reordered in `LedgerStateSnapshot` (or any of
    // its transitively-serialised types like `CostModels`) makes the prior
    // stream undecodable. Issue #609 reported the user-visible failure mode:
    //
    //   WARN dugite_ledger::state::snapshot: Snapshot version mismatch
    //         snapshot_version=15 current_version=16
    //   Failed to load ledger snapshot, starting fresh:
    //         tag for enum is not valid, found 65
    //
    // Followed by silent deletion of the ledger snapshot and a full from-
    // genesis re-sync.
    //
    // The two tests below pin the new behaviour:
    //
    //   * `load_snapshot` returns `Err` *immediately* on a version that is
    //     less than `SNAPSHOT_VERSION` — it must not attempt the doomed
    //     bincode decode.
    //   * The unreadable file is renamed to `<name>.vNN-unreadable` so that
    //     (a) its bytes are preserved for forensic inspection and (b) the
    //     `.bin` suffix-based enumerator in `dugite_node::startup` does not
    //     pick it back up on the next restart.

    /// Build a synthetic snapshot file whose header advertises `version`
    /// (must be ≥ 1, < `SNAPSHOT_VERSION`, and < 128 so that the versioned
    /// header branch is taken) but whose payload is empty. The blake2b
    /// checksum is the digest of the empty payload, so the header itself is
    /// internally consistent — we want `load_snapshot` to reject on the
    /// version check, not on a checksum mismatch.
    fn write_versioned_snapshot_stub(path: &Path, version: u8) {
        assert!(
            version > 0 && version < 128 && version < LedgerState::SNAPSHOT_VERSION,
            "stub helper expects an older, valid version byte"
        );
        let mut bytes = Vec::with_capacity(37);
        bytes.extend_from_slice(b"DUGT");
        bytes.push(version);
        let empty_payload_hash = dugite_primitives::hash::blake2b_256(&[]);
        bytes.extend_from_slice(empty_payload_hash.as_bytes());
        std::fs::write(path, &bytes).unwrap();
    }

    /// Older snapshot versions must produce a clear, version-aware error
    /// *before* bincode is invoked. Issue #609 — silent chain wipe.
    #[test]
    fn test_version_mismatch_returns_error_and_does_not_attempt_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.bin");
        // Use the version one behind the current code path — that's the
        // exact case from the #609 reproduction (v15 read by v16 binary).
        let old_version = LedgerState::SNAPSHOT_VERSION - 1;
        write_versioned_snapshot_stub(&path, old_version);

        let result = LedgerState::load_snapshot(&path);
        let err = result.expect_err("older snapshot version must be rejected");
        let msg = err.to_string();

        // Error must surface BOTH version numbers so an operator reading
        // logs can immediately tell which bump caused the rejection.
        assert!(
            msg.contains(&old_version.to_string()),
            "error must mention on-disk version {old_version}, got: {msg}"
        );
        assert!(
            msg.contains(&LedgerState::SNAPSHOT_VERSION.to_string()),
            "error must mention current SNAPSHOT_VERSION {}, got: {msg}",
            LedgerState::SNAPSHOT_VERSION
        );
        // Error must NOT be the bincode tag-decode noise from the old code
        // path. If this assertion ever fires, the early-return version
        // guard has regressed and silent chain wipe is back.
        assert!(
            !msg.contains("tag for enum is not valid"),
            "version mismatch must not surface bincode-internal errors, got: {msg}"
        );
    }

    /// Quarantine renames the unreadable file to `<name>.vNN-unreadable`
    /// inside the same directory, so that (a) the bytes survive for
    /// forensics and (b) the next startup does not retry it (the
    /// `.bin`-suffix enumerator no longer matches).
    #[test]
    fn test_version_mismatch_quarantines_original_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quarantine.bin");
        let old_version = LedgerState::SNAPSHOT_VERSION - 1;
        write_versioned_snapshot_stub(&path, old_version);

        let _ = LedgerState::load_snapshot(&path);

        // The original `.bin` must no longer exist — otherwise the next
        // restart would attempt to load it again and hit the same failure.
        assert!(
            !path.exists(),
            "unreadable snapshot must be moved out of the .bin slot"
        );

        // The expected quarantine path must exist and contain the original
        // header bytes (so post-mortem tooling can decode the version
        // field even after the file is renamed).
        let expected_quarantine = {
            let mut s = path.as_os_str().to_owned();
            s.push(format!(".v{old_version}-unreadable"));
            PathBuf::from(s)
        };
        assert!(
            expected_quarantine.exists(),
            "quarantined file must exist at {}",
            expected_quarantine.display()
        );
        let bytes = std::fs::read(&expected_quarantine).unwrap();
        assert_eq!(&bytes[..4], b"DUGT", "quarantine must preserve magic word");
        assert_eq!(
            bytes[4], old_version,
            "quarantine must preserve original version byte"
        );

        // The quarantine extension must not match the `*.bin` glob the
        // startup enumerator uses, so the next restart skips it. We
        // assert that here against the literal suffix the enumerator
        // checks for (`.bin`), which would be brittle to re-import — but
        // is sufficient as a defense-in-depth check that the rename did
        // its job.
        assert!(
            !expected_quarantine
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".bin"))
                .unwrap_or(false),
            "quarantined filename must not end with .bin"
        );
    }
}
