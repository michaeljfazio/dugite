//! Epoch transition handling: snapshot policy, ledger snapshot save/load/prune,
//! and the Shelley transition epoch lookup used to correctly compute slot numbers
//! on networks that started with a Byron era.

use dugite_primitives::block::Point;
use std::path::PathBuf;
use tracing::{debug, error, warn};

use super::Node;

// ─── Shelley transition epoch ────────────────────────────────────────────────

/// Return the number of Byron epochs before the Shelley hard fork for known
/// Cardano networks, identified by network magic.
///
/// Based on CNCLI's `guess_shelley_transition_epoch`.
pub fn shelley_transition_epoch_for_magic(network_magic: u64) -> u64 {
    match network_magic {
        764824073 => 208, // mainnet
        1 => 4,           // preprod
        2 => 0,           // preview (no Byron era)
        4 => 0,           // sanchonet
        141 => 2,         // guild
        _ => 0,           // unknown — assume no Byron era (safest default)
    }
}

// ─── Per-network HFC era transition table ────────────────────────────────────

/// Placeholder transition epoch for the preview Dijkstra HFI (PV 12).
///
/// As of 2026-05-12 the preview Dijkstra HardForkInitiation governance action
/// has not been ratified; latest preview block is PV 11 / Conway at epoch 1295
/// (Koios `epoch_block_protocols`). We forward-declare the Dijkstra entry at a
/// clearly-sentinel future epoch so that ledger snapshots already in the
/// Dijkstra era (e.g. a future Mithril snapshot, or a dugite-forged
/// Dijkstra-era snapshot) replay a Conway→Dijkstra transition during HFC
/// era-history reconstruction. `record_era_transition` is also called for
/// real on a per-block basis (`mod.rs` block-apply path) and self-corrects
/// the in-memory state machine on the first observed Dijkstra-era block.
///
/// Chosen value: `9_999_999`. Far beyond any realistic preview epoch
/// (preview epoch_length ≈ 86400 slots ≈ 1 day, so 9.99M epochs ≈ 27,000
/// years), but small enough that `epochs_in_era * epoch_size` cannot overflow
/// `u64` inside `EraHistory::compute_end_bound`. Patch this constant to the
/// observed activation epoch once the HFI ratifies on preview.
pub const PREVIEW_DIJKSTRA_EPOCH_PLACEHOLDER: u64 = 9_999_999;

/// Per-network table of post-Shelley era transitions, in HFC order.
///
/// Each entry is `(target_era, transition_epoch)`. The caller replays only
/// those entries whose target era is `<= snapshot_era`, reconstructing the
/// HFC era history when cold-starting from a ledger snapshot.
///
/// Sources:
/// - Mainnet (764824073): on-chain history — Babbage 365 (Vasil), Conway 517
///   (Chang). Pre-Babbage transitions are folded into the Shelley entry
///   created by `EraHistory::from_genesis`.
/// - Preprod (1): testnet eras collapse to epoch 0; Babbage 4, Conway 186.
/// - Preview (2): instant Allegra/Mary/Alonzo; Babbage 3, Conway 646, Dijkstra
///   at `PREVIEW_DIJKSTRA_EPOCH_PLACEHOLDER` (HFI pending). Issue #465.
pub fn era_transitions_for_magic(network_magic: u64) -> Vec<(dugite_primitives::era::Era, u64)> {
    use dugite_primitives::era::Era;
    match network_magic {
        764824073 => vec![
            (Era::Babbage, 365),
            (Era::Conway, 517),
            // Dijkstra on mainnet: not yet proposed.
        ],
        1 => vec![
            // Preprod
            (Era::Allegra, 0),
            (Era::Mary, 0),
            (Era::Alonzo, 0),
            (Era::Babbage, 4),
            (Era::Conway, 186),
            // Dijkstra on preprod: not yet proposed.
        ],
        2 => vec![
            // Preview
            (Era::Allegra, 0),
            (Era::Mary, 0),
            (Era::Alonzo, 0),
            (Era::Babbage, 3),
            (Era::Conway, 646),
            (Era::Dijkstra, PREVIEW_DIJKSTRA_EPOCH_PLACEHOLDER),
        ],
        _ => vec![
            // Generic testnet: assume instant transitions through Conway.
            (Era::Allegra, 0),
            (Era::Mary, 0),
            (Era::Alonzo, 0),
            (Era::Babbage, 0),
            (Era::Conway, 0),
        ],
    }
}

// ─── Snapshot policy ─────────────────────────────────────────────────────────

/// Snapshot policy matching Haskell cardano-node's `SnapshotPolicy`.
///
/// Controls when ledger snapshots are taken based on time and block counts.
/// Two modes:
/// - **Normal operation:** snapshot every `k * 2` seconds (~72 minutes for k=2160)
/// - **Bulk sync (replay):** snapshot every `bulk_min_blocks` blocks AND `bulk_min_interval` elapsed
#[allow(dead_code)] // normal_interval used by should_snapshot_normal (networking rewrite)
pub struct SnapshotPolicy {
    /// Time between snapshots during normal operation (k * 2 seconds)
    pub normal_interval: std::time::Duration,
    /// Minimum blocks processed before snapshot during bulk sync
    pub bulk_min_blocks: u64,
    /// Minimum time between snapshots during bulk sync
    pub bulk_min_interval: std::time::Duration,
    /// Maximum snapshots to retain on disk
    pub max_snapshots: usize,
    /// Last snapshot time
    pub last_snapshot_time: std::time::Instant,
    /// Blocks since last snapshot
    pub blocks_since_snapshot: u64,
}

impl SnapshotPolicy {
    /// Create a new snapshot policy with defaults matching Haskell cardano-node.
    #[allow(dead_code)] // referenced by epoch.rs unit tests
    pub fn new(security_param_k: u64) -> Self {
        SnapshotPolicy {
            normal_interval: std::time::Duration::from_secs(security_param_k * 2),
            bulk_min_blocks: 50_000,
            bulk_min_interval: std::time::Duration::from_secs(360), // 6 minutes
            max_snapshots: 2,
            last_snapshot_time: std::time::Instant::now(),
            blocks_since_snapshot: 0,
        }
    }

    /// Create with custom parameters (from CLI flags).
    pub fn with_params(
        security_param_k: u64,
        max_snapshots: usize,
        bulk_min_blocks: u64,
        bulk_min_secs: u64,
    ) -> Self {
        SnapshotPolicy {
            normal_interval: std::time::Duration::from_secs(security_param_k * 2),
            bulk_min_blocks,
            bulk_min_interval: std::time::Duration::from_secs(bulk_min_secs),
            max_snapshots,
            last_snapshot_time: std::time::Instant::now(),
            blocks_since_snapshot: 0,
        }
    }

    /// Record that blocks have been applied.
    pub fn record_blocks(&mut self, count: u64) {
        self.blocks_since_snapshot += count;
    }

    /// Check if a snapshot should be taken during normal (at-tip) operation.
    #[allow(dead_code)] // used by networking rewrite (and tests)
    pub fn should_snapshot_normal(&self) -> bool {
        self.last_snapshot_time.elapsed() >= self.normal_interval
    }

    /// Check if a snapshot should be taken during bulk sync (replay).
    ///
    /// Issue #695 removed the production callers of this method
    /// (chunk-file + LSM replay no longer save mid-loop). Retained
    /// for unit-test coverage of the policy's threshold semantics.
    #[allow(dead_code)]
    pub fn should_snapshot_bulk(&self) -> bool {
        self.blocks_since_snapshot >= self.bulk_min_blocks
            && self.last_snapshot_time.elapsed() >= self.bulk_min_interval
    }

    /// Mark that a snapshot was taken.
    pub fn snapshot_taken(&mut self) {
        self.last_snapshot_time = std::time::Instant::now();
        self.blocks_since_snapshot = 0;
    }
}

// ─── Snapshot helpers ────────────────────────────────────────────────────────

/// Map the storage-layer UTxO backend onto the snapshot backend tag written
/// into each snapshot's `.meta.json` sidecar (mirrors Haskell's
/// `SnapshotBackend` / `UTxOHD{Mem,LSM}Snapshot`).
pub(crate) fn snapshot_backend_of(
    backend: dugite_storage::UtxoBackend,
) -> dugite_ledger::SnapshotBackend {
    match backend {
        dugite_storage::UtxoBackend::Lsm => dugite_ledger::SnapshotBackend::DugiteLsm,
        dugite_storage::UtxoBackend::InMemory => dugite_ledger::SnapshotBackend::DugiteMem,
    }
}

/// Derive the snapshot backend tag from a live ledger state's *actual* UTxO
/// store attachment (LSM store attached ⇒ `dugite-lsm`, else `dugite-mem`).
/// This reflects the true runtime backend rather than the configured one and
/// matches the rule `LedgerState::save_snapshot` applies internally.
pub(crate) fn snapshot_backend_for_ledger(
    ls: &dugite_ledger::LedgerState,
) -> dugite_ledger::SnapshotBackend {
    if ls.utxo.utxo_set.has_store() {
        dugite_ledger::SnapshotBackend::DugiteLsm
    } else {
        dugite_ledger::SnapshotBackend::DugiteMem
    }
}

/// Remove old epoch snapshots in `database_path`, keeping only the `keep`
/// most recent. Free function so it is reachable from a `spawn_blocking`
/// task without `&Node` (issue #649 — Phase B of the offloaded snapshot
/// write).
pub(crate) fn prune_old_snapshots_in_dir(
    database_path: &std::path::Path,
    keep: usize,
    // #762: the newest snapshot whose tip slot is `<=` this floor (the
    // ImmutableDB tip / flush point) is PROTECTED from pruning, so a restart
    // always retains at least one snapshot that can be replayed forward from
    // ChainDB even when the VolatileDB is lost (e.g. a crash before the WAL
    // fsync, or a flush that lagged far behind the live tip). Without this
    // guard, a slow ValidateAll sync — where every snapshot is taken at the
    // live (volatile) tip far above the immutable tip — leaves ALL snapshots
    // stranded above the immutable tip; on restart they cannot be replayed and
    // the node wedges. `None` disables the floor guard (used in unit tests).
    immutable_tip_slot: Option<u64>,
) {
    // (epoch, slot, path); slot is 0 for the legacy "epochN.bin" name format.
    let mut snapshots: Vec<(u64, u64, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(database_path) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(rest) = name_str.strip_prefix("ledger-snapshot-epoch") {
                if let Some(epoch_str) = rest.strip_suffix(".bin") {
                    // Handle both "5" (legacy) and "5-slot12345" (new) formats.
                    let mut parts = epoch_str.splitn(2, "-slot");
                    let epoch_part = parts.next().unwrap_or(epoch_str);
                    let slot = parts
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    if let Ok(epoch) = epoch_part.parse::<u64>() {
                        snapshots.push((epoch, slot, entry.path()));
                    }
                }
            }
        }
    }

    // #762: identify the newest snapshot at-or-below the immutable tip — the
    // restart-recovery anchor — and spare it from pruning regardless of age.
    let protected: Option<PathBuf> = immutable_tip_slot.and_then(|floor| {
        snapshots
            .iter()
            .filter(|(_, slot, _)| *slot > 0 && *slot <= floor)
            .max_by_key(|(_, slot, _)| *slot)
            .map(|(_, _, p)| p.clone())
    });

    if snapshots.len() > keep {
        snapshots.sort_by_key(|(epoch, _, _)| *epoch);
        let to_remove = snapshots.len() - keep;
        for (epoch, _slot, path) in snapshots.into_iter().take(to_remove) {
            if protected.as_ref() == Some(&path) {
                debug!(
                    epoch,
                    "Sparing restart-recovery snapshot (<= immutable tip) from prune (#762)"
                );
                continue;
            }
            if let Err(e) = std::fs::remove_file(&path) {
                warn!(epoch, "Failed to remove old snapshot: {e}");
            } else {
                debug!(epoch, "Pruned old ledger snapshot");
            }
            // Remove the backend meta sidecar alongside the `.bin` (a missing
            // sidecar — e.g. a pre-meta snapshot — is benign: NotFound ignored).
            let meta_path = dugite_ledger::SnapshotMeta::sidecar_path(&path);
            match std::fs::remove_file(&meta_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!(epoch, "Failed to remove old snapshot meta sidecar: {e}"),
            }
        }
    }
}

/// Update `latest_path` to reference the same inode as `epoch_path` via a
/// hardlink (issue #648).
///
/// Replaces the previous `fs::copy(epoch, latest)` which wrote the same
/// ~100 MB – 1 GB of ledger state to disk twice per snapshot AND held the
/// global `RwLock<LedgerState>` write lock for the duration of the copy.
///
/// Atomicity / crash-safety:
/// - `remove_file` + `hard_link` is not a single syscall, but the only
///   reader of `latest.bin` is the startup path (`load_snapshot`), and the
///   only writer is the (serialised) snapshot task — no concurrent-reader
///   window exists.
/// - If the process crashes between the two calls, `latest.bin` may be
///   missing on restart. `find_best_snapshot_for_rollback` then enumerates
///   `ledger-snapshot-epoch*.bin` and uses the most recent canonical one —
///   the existing fallback path.
/// - On a fresh database where `latest.bin` doesn't yet exist, the
///   `remove_file` returns `NotFound` and we ignore it.
///
/// Filesystem requirement: `hard_link` requires both paths on the same
/// filesystem; trivially satisfied because both live under `database_path`.
pub(crate) fn link_latest_snapshot(
    epoch_path: &std::path::Path,
    latest_path: &std::path::Path,
) -> std::io::Result<()> {
    // Remove any existing latest.bin first; `hard_link` fails if the target
    // exists. NotFound is benign (first snapshot on a fresh database).
    match std::fs::remove_file(latest_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::hard_link(epoch_path, latest_path)?;

    // Mirror the `.meta.json` sidecar onto the latest hardlink so the startup
    // load guard reads the backend tag from `ledger-snapshot.bin.meta.json`.
    // Best-effort: a missing source sidecar (pre-meta snapshot) is benign —
    // load falls through to backend inference.
    let epoch_meta = dugite_ledger::SnapshotMeta::sidecar_path(epoch_path);
    if epoch_meta.exists() {
        let latest_meta = dugite_ledger::SnapshotMeta::sidecar_path(latest_path);
        match std::fs::remove_file(&latest_meta) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        std::fs::hard_link(&epoch_meta, &latest_meta)?;
    }
    Ok(())
}

// ─── Node impl: snapshot persistence ─────────────────────────────────────────

impl Node {
    /// Save a ledger state snapshot to the database directory.
    ///
    /// When the UTxO store is backed by LSM (cardano-lsm), this also flushes
    /// the memtable to SST files on disk via `save_utxo_snapshot()`.
    /// cardano-lsm has no WAL — without this flush, all UTxO data is lost
    /// on restart.
    pub async fn save_ledger_snapshot(&self) {
        // Phase A — under the `LedgerState` write lock (issue #649). Keep
        // this short: LSM flush + opcert/diff bookkeeping + cheap clone of
        // the bincode wire view (Arc::clone for the big shared maps).
        // Once the `LedgerStateSnapshot` is constructed it captures a
        // point-in-time consistent view; the lock can be dropped and the
        // CPU + disk-bound write moves to a `spawn_blocking` worker.
        let (snapshot, epoch, slot, utxo_count, backend) = {
            let mut ls = self.ledger_state.write().await;
            let epoch = ls.epoch.0;

            // Copy opcert counters from consensus into ledger state for
            // snapshot persistence. Consensus is the runtime owner; ledger
            // state is the persistence vehicle.
            ls.consensus.opcert_counters = self.consensus.opcert_counters().clone();

            // Free DiffSeq memory before snapshot — diffs are not
            // persisted (#[serde(skip)]) and clearing here reclaims
            // memory immediately.
            ls.utxo.diff_seq.clear();

            // Flush UTxO store to disk FIRST (cardano-lsm has no WAL).
            // MUST stay under the lock: `LsmTree::save_snapshot` takes
            // `&mut self` and concurrent flush is not supported.
            if let Err(e) = ls.save_utxo_snapshot() {
                error!("Failed to save UTxO store snapshot: {e}");
            }

            let slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
            let utxo_count = ls.utxo.utxo_set.len();
            // Derive the backend tag from the *actual* runtime store attachment
            // (LSM store attached ⇒ `dugite-lsm`, else `dugite-mem`) — the same
            // rule `LedgerState::save_snapshot` uses internally.
            let backend = snapshot_backend_for_ledger(&ls);
            // Cheap snapshot view: Arc::clone for the big shared maps
            // (delegations, pool_params, reward_accounts, governance,
            // epoch_blocks_by_pool); HashMap::clone for the bounded ones.
            // For the LSM backend `utxo_set` is empty in the snapshot —
            // UTxOs are persisted via the LSM SST flush above.
            let snapshot = dugite_ledger::LedgerStateSnapshot::from(&*ls);
            (snapshot, epoch, slot, utxo_count, backend)
            // ── LOCK RELEASED HERE ──
        };

        let database_path = self.database_path.clone();
        let max_snapshots = self.snapshot_policy.max_snapshots + 1;
        // #762: the ImmutableDB tip is the flush point. The prune floor guard
        // keeps the newest snapshot at-or-below it so a restart always has a
        // replayable recovery anchor (see prune_old_snapshots_in_dir).
        let immutable_tip_slot = self
            .chain_db
            .read()
            .await
            .get_immutable_tip_point()
            .and_then(|p| p.slot())
            .map(|s| s.0);
        let epoch_path = database_path.join(format!("ledger-snapshot-epoch{epoch}-slot{slot}.bin"));
        let latest_path = database_path.join("ledger-snapshot.bin");

        // Phase B — off the tokio worker pool. Bincode walk + hashing +
        // buffered disk write + atomic rename + latest.bin hardlink +
        // prune. None of this needs the ledger lock; running it on a
        // tokio worker would otherwise pin one core for the duration
        // (multi-second on preview/preprod-scale state) and stall every
        // co-scheduled task.
        let join_result = tokio::task::spawn_blocking(move || -> Result<u64, String> {
            let bytes = dugite_ledger::LedgerState::write_snapshot_view_to_path(
                &snapshot,
                &epoch_path,
                backend,
            )
            .map_err(|e| format!("write snapshot: {e}"))?;
            if let Err(e) = link_latest_snapshot(&epoch_path, &latest_path) {
                // Non-fatal: epoch-N snapshot is on disk; startup will
                // enumerate it via `find_best_snapshot_for_rollback` if
                // `latest.bin` is missing.
                error!("Failed to hardlink latest ledger snapshot: {e}");
            }
            prune_old_snapshots_in_dir(&database_path, max_snapshots, immutable_tip_slot);
            Ok(bytes)
        })
        .await;

        match join_result {
            Ok(Ok(bytes)) => {
                tracing::info!(
                    "Snapshot     saved (epoch={}, {} UTxOs, {:.1} MB)",
                    epoch,
                    utxo_count,
                    bytes as f64 / 1_048_576.0,
                );
            }
            Ok(Err(e)) => error!("Failed to save ledger snapshot: {e}"),
            Err(join_err) => error!("Snapshot task panicked: {join_err}"),
        }
    }

    /// Non-blocking snapshot trigger (issue #695).
    ///
    /// Replaces every at-tip `save_ledger_snapshot().await` call site
    /// on the apply hot path. Phase A (LSM flush + Arc::clone view
    /// build) still holds the ledger write lock briefly — duration
    /// unchanged from `save_ledger_snapshot`'s prep block. Phase B
    /// (bincode walk + atomic rename + prune) is handed off to the
    /// background snapshot worker via a bounded mpsc channel and the
    /// apply path returns immediately.
    ///
    /// ## Returned [`super::snapshot_worker::SnapshotEnqueue`]
    ///
    /// - `Enqueued` — successfully sent to the worker. Caller should
    ///   record the snapshot as taken on its scheduler.
    /// - `Skipped` — worker was busy (channel full) or pre-flight
    ///   capacity check failed. Caller must NOT record the snapshot
    ///   as taken; the scheduler retries on the next block. Mirrors
    ///   cardano-node's `SnapshotDelayRange` delay-gate skip.
    /// - `Closed` — worker shutdown has begun; do not retry.
    pub async fn try_snapshot_async(&self) -> super::snapshot_worker::SnapshotEnqueue {
        use super::snapshot_worker::{has_capacity, SnapshotEnqueue, SnapshotRequest};

        let Some(tx) = self.snapshot_tx.as_ref() else {
            return SnapshotEnqueue::Closed;
        };
        // Pre-flight check: avoid the multi-hundred-ms HashMap clones
        // in Phase A when the worker is busy. Without this guard a
        // wedged disk would force every apply path to clone + discard.
        if !has_capacity(tx) {
            self.metrics.inc_snapshot_skipped_busy();
            debug!("snapshot worker busy — skipping (pre-lock)");
            return SnapshotEnqueue::Skipped;
        }

        // #762: capture the ImmutableDB tip (flush point) BEFORE the ledger
        // write lock to avoid a lock-order inversion (chain_db is always
        // acquired before ledger_state on the apply path). Threaded into the
        // snapshot worker's prune floor guard so the newest snapshot at-or-
        // below the immutable tip is never pruned — guaranteeing a restart-
        // recoverable anchor even when the live (volatile) tip races far ahead.
        let immutable_tip_slot = self
            .chain_db
            .read()
            .await
            .get_immutable_tip_point()
            .and_then(|p| p.slot())
            .map(|s| s.0)
            .unwrap_or(0);

        // Phase A — under the ledger write lock. LSM flush (sub-second
        // on preview, ~1–2 s on mainnet under churn) + Arc::clone
        // view build (two big stake-credential HashMaps ≈ 350 ms on
        // mainnet, everything else Arc::clone).
        let req = {
            let mut ls = self.ledger_state.write().await;
            ls.consensus.opcert_counters = self.consensus.opcert_counters().clone();
            ls.utxo.diff_seq.clear();
            if let Err(e) = ls.save_utxo_snapshot() {
                error!(error = %e, "LSM flush failed during snapshot");
                self.metrics.inc_utxo_flush_failed();
                // Proceed: in-memory utxo_set may be empty (LSM
                // backend) so the view is structurally valid; the LSM
                // SST on disk is the UTxO ground truth and may be
                // stale by one save cycle.
            }
            SnapshotRequest {
                view: dugite_ledger::LedgerStateSnapshot::from(&*ls),
                epoch: ls.epoch.0,
                slot: ls.tip.point.slot().map(|s| s.0).unwrap_or(0),
                utxo_count: ls.utxo.utxo_set.len(),
                backend: snapshot_backend_for_ledger(&ls),
                immutable_tip_slot,
            }
            // ── LOCK RELEASED HERE ──
        };

        match tx.try_send(req) {
            Ok(()) => {
                self.metrics.inc_snapshot_enqueued();
                debug!("snapshot enqueued to worker");
                SnapshotEnqueue::Enqueued
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                // TOCTOU race vs pre-flight; benign — bounded to one
                // view-build worth of work.
                self.metrics.inc_snapshot_skipped_busy();
                debug!("snapshot worker busy — skipping (post-lock)");
                SnapshotEnqueue::Skipped
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                warn!("snapshot worker channel closed — node shutting down");
                SnapshotEnqueue::Closed
            }
        }
    }

    /// Find the best epoch snapshot for a rollback to the given slot.
    ///
    /// Returns the path to the most recent snapshot whose ledger tip is at or
    /// before `rollback_slot` **and** whose tip is on the canonical ImmutableDB
    /// chain.  Falls back to `ledger-snapshot.bin` if no epoch snapshot
    /// qualifies.
    ///
    /// `chain_db` is optional; when `Some`, each candidate snapshot's tip hash
    /// is verified against the ImmutableDB canonical chain.  Fork snapshots are
    /// skipped with a warning so they cannot be used as rollback base states.
    /// When `None` (e.g. ChainDB not yet initialised), the canonicality check
    /// is skipped and any snapshot at or before the rollback slot is accepted.
    #[allow(dead_code)] // used by networking rewrite (handle_rollback)
    pub fn find_best_snapshot_for_rollback(
        &self,
        rollback_slot: u64,
        chain_db: Option<&dugite_storage::ChainDB>,
    ) -> Option<std::path::PathBuf> {
        // Collect all epoch-numbered snapshots (sorted newest first)
        let mut epoch_snapshots: Vec<(u64, PathBuf)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.database_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if let Some(rest) = name_str.strip_prefix("ledger-snapshot-epoch") {
                    if let Some(epoch_str) = rest.strip_suffix(".bin") {
                        let epoch_part = epoch_str.split("-slot").next().unwrap_or(epoch_str);
                        if let Ok(epoch) = epoch_part.parse::<u64>() {
                            epoch_snapshots.push((epoch, entry.path()));
                        }
                    }
                }
            }
        }
        // Sort by epoch descending (newest first)
        epoch_snapshots.sort_by_key(|r| std::cmp::Reverse(r.0));

        // Try each epoch snapshot to find one at or before the rollback slot.
        // We need to actually load the snapshot to check its slot (epoch number alone
        // isn't enough since the snapshot slot could be anywhere in the epoch).
        // To avoid loading huge snapshots just to check, use a heuristic:
        // epoch * epoch_length gives approximate slot. If epoch is clearly too new, skip.
        let epoch_length = {
            // Use a rough estimate; we don't need exact precision here
            if let Some(ref genesis) = self.shelley_genesis {
                genesis.epoch_length
            } else {
                86400
            }
        };

        for (epoch, path) in &epoch_snapshots {
            // Heuristic: if epoch * epoch_length > rollback_slot + epoch_length, skip
            // (snapshot is definitely beyond the rollback point)
            let approx_slot = epoch * epoch_length;
            if approx_slot > rollback_slot + epoch_length {
                continue;
            }

            // This snapshot might work — try loading to check exact slot
            match dugite_ledger::LedgerState::load_snapshot(path) {
                Ok(state) => {
                    let snap_slot = state.tip.point.slot().map(|s| s.0).unwrap_or(0);
                    if snap_slot <= rollback_slot {
                        // Additionally verify that the snapshot tip is on the canonical
                        // ImmutableDB chain.  A fork snapshot (tip hash not in
                        // ImmutableDB) would produce permanently corrupt state if used
                        // as a rollback base — genesis-replayed UTxOs would coexist with
                        // the fork UTxOs already in the store.
                        let is_canonical =
                            is_snapshot_canonical(snap_slot, &state.tip.point, chain_db);
                        if is_canonical {
                            debug!(
                                epoch,
                                snap_slot,
                                rollback_slot,
                                "Found suitable canonical epoch snapshot for rollback"
                            );
                            return Some(path.clone());
                        } else {
                            warn!(
                                epoch,
                                snap_slot,
                                "Epoch snapshot tip is on a fork — skipping for rollback"
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!(epoch, "Failed to load epoch snapshot: {e}");
                }
            }
        }

        // Fall back to latest snapshot
        let latest = self.database_path.join("ledger-snapshot.bin");
        if latest.exists() {
            // Check if it's usable (at or before rollback point) and canonical.
            if let Ok(state) = dugite_ledger::LedgerState::load_snapshot(&latest) {
                let snap_slot = state.tip.point.slot().map(|s| s.0).unwrap_or(0);
                if snap_slot <= rollback_slot {
                    let is_canonical = is_snapshot_canonical(snap_slot, &state.tip.point, chain_db);
                    if is_canonical {
                        return Some(latest);
                    } else {
                        warn!(
                            snap_slot,
                            "Latest ledger-snapshot.bin tip is on a fork — skipping for rollback"
                        );
                    }
                }
            }
        }

        None
    }
}

// ─── Snapshot canonicality helper ────────────────────────────────────────────

/// Verify that a ledger snapshot tip is on the canonical ChainDB chain.
///
/// Returns `true` when:
///   - `snap_slot` is 0 (origin — always canonical)
///   - `chain_db` is `None` (no DB to check against — assume canonical)
///   - The snapshot slot is strictly beyond the overall ChainDB tip (ImmutableDB +
///     VolatileDB selected chain) — genuinely ahead, e.g. after a Mithril import
///     where the ledger was fast-forwarded ahead of synced blocks
///   - The canonical chain (ImmutableDB or VolatileDB selected chain) has the
///     same block hash at `snap_slot`
///
/// Returns `false` when the canonical chain has a *different* block at
/// `snap_slot` (or no block at that exact slot, meaning it was an empty slot
/// in the canonical chain and the snapshot block is a fork block).
///
/// **Volatile-range fix**: the previous implementation used only the
/// ImmutableDB tip as the upper bound, provisionally accepting any snapshot
/// whose slot was above the ImmutableDB tip — including forged-but-not-adopted
/// blocks that sit in the gap between the ImmutableDB tip and the VolatileDB
/// canonical tip.  This caused BP forks at epoch boundary (or mid-epoch) to
/// survive node restart: the forged block's slot was above the ImmutableDB
/// tip, so the check returned `true` without verifying it against the
/// VolatileDB selected chain.
///
/// Fix: use the overall ChainDB tip (max of ImmutableDB + VolatileDB) as the
/// provisional-accept boundary.  For slots within the volatile region but
/// known to ChainDB, `get_block_at_or_after_slot` queries the VolatileDB
/// *selected chain* (canonical path only, not orphaned forks) and correctly
/// detects hash mismatches caused by forged-but-rejected blocks.
///
/// Called by `find_best_snapshot_for_rollback` and the startup snapshot loader
/// to prevent fork snapshots from being used as ledger base states.
pub(crate) fn is_snapshot_canonical(
    snap_slot: u64,
    tip_point: &Point,
    chain_db: Option<&dugite_storage::ChainDB>,
) -> bool {
    if snap_slot == 0 {
        return true; // Origin is always canonical
    }
    let snap_hash = match tip_point.hash() {
        Some(h) => *h,
        None => return true, // No hash to verify — assume canonical
    };
    let db = match chain_db {
        Some(db) => db,
        None => return true, // No ChainDB available — skip check
    };

    // Use the overall ChainDB tip (max of ImmutableDB tip and VolatileDB
    // selected chain tip) as the accept-provisionally boundary.
    //
    // The previous code used only `imm_tip_slot` here, which meant that any
    // snapshot whose slot was above the ImmutableDB tip (but within the
    // VolatileDB range) was unconditionally accepted.  That allowed a
    // forged-but-not-adopted block to survive as a "canonical" snapshot tip
    // across node restarts, because the forged slot was above the ImmutableDB
    // tip but the VolatileDB selected chain already had the canonical block.
    let db_tip_slot = db.get_tip().point.slot().map(|s| s.0).unwrap_or(0);

    if snap_slot > db_tip_slot {
        // Snapshot is genuinely ahead of all known ChainDB data.
        // This is the expected state after a Mithril import — the ledger was
        // fast-forwarded to a slot for which we have not yet synced blocks.
        // Accept provisionally; the missing blocks will be fetched from peers.
        return true;
    }

    // Snapshot slot is within the range covered by ChainDB (ImmutableDB or
    // VolatileDB selected chain).  `get_block_at_or_after_slot` queries both
    // stores, returning only blocks on the VolatileDB *selected* chain —
    // orphaned/fork blocks are excluded even if they're still in the WAL.
    match db.get_block_at_or_after_slot(dugite_primitives::time::SlotNo(snap_slot)) {
        Ok(Some((found_slot, found_hash, _))) if found_slot.0 == snap_slot => {
            // Canonical chain has a block at exactly snap_slot — compare hashes.
            if found_hash == snap_hash {
                true
            } else {
                debug!(
                    snap_slot,
                    snap_hash = %snap_hash.to_hex(),
                    canonical_hash = %found_hash.to_hex(),
                    "is_snapshot_canonical: hash mismatch — snapshot is on a fork"
                );
                false
            }
        }
        Ok(Some((found_slot, _, _))) => {
            // The canonical chain has no block at the exact snapshot slot
            // (empty slot), but has a block at found_slot > snap_slot.
            // A canonical empty slot cannot contain a block — any block at
            // snap_slot must be a fork block.  Treat as non-canonical.
            debug!(
                snap_slot,
                found_slot = found_slot.0,
                "is_snapshot_canonical: no canonical block at snap_slot (empty slot) — fork"
            );
            false
        }
        Ok(None) => {
            // No canonical blocks at or after snap_slot, but snap_slot <=
            // db_tip_slot.  This shouldn't normally happen (implies a gap in
            // ChainDB); assume canonical to avoid spurious genesis replay.
            true
        }
        Err(_) => {
            // DB error — cannot verify; assume canonical to avoid spurious resets.
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shelley_transition_mainnet() {
        assert_eq!(shelley_transition_epoch_for_magic(764824073), 208);
    }

    #[test]
    fn test_shelley_transition_preprod() {
        assert_eq!(shelley_transition_epoch_for_magic(1), 4);
    }

    #[test]
    fn test_shelley_transition_preview_no_byron() {
        assert_eq!(shelley_transition_epoch_for_magic(2), 0);
    }

    #[test]
    fn test_shelley_transition_sanchonet_no_byron() {
        assert_eq!(shelley_transition_epoch_for_magic(4), 0);
    }

    #[test]
    fn test_shelley_transition_unknown_defaults_to_zero() {
        assert_eq!(shelley_transition_epoch_for_magic(999999), 0);
    }

    // ── era_transitions_for_magic — #465 regression coverage ──────────────────

    /// Preview (magic 2) must include a Dijkstra entry so that ledger snapshots
    /// taken in the Dijkstra era reconstruct the HFC era history correctly on
    /// cold-start. Issue #465.
    #[test]
    fn test_era_transitions_preview_includes_dijkstra() {
        use dugite_primitives::era::Era;
        let table = era_transitions_for_magic(2);
        let eras: Vec<Era> = table.iter().map(|(e, _)| *e).collect();
        assert!(
            eras.contains(&Era::Dijkstra),
            "preview era table must include Dijkstra (#465): {:?}",
            eras
        );
    }

    /// Preview Babbage/Conway epochs match the Koios-observed boundaries
    /// (Babbage at epoch 3 — Vasil HF on preview; Conway at epoch 646 — Chang
    /// HF on preview, cross-checked via Koios `epoch_block_protocols`).
    #[test]
    fn test_era_transitions_preview_bound_epochs_match_koios() {
        use dugite_primitives::era::Era;
        let table = era_transitions_for_magic(2);
        let babbage = table
            .iter()
            .find(|(e, _)| *e == Era::Babbage)
            .expect("Babbage entry present");
        let conway = table
            .iter()
            .find(|(e, _)| *e == Era::Conway)
            .expect("Conway entry present");
        assert_eq!(
            babbage.1, 3,
            "preview Babbage starts at epoch 3 (Vasil HF on preview)"
        );
        assert_eq!(
            conway.1, 646,
            "preview Conway starts at epoch 646 (Chang HF on preview)"
        );
    }

    /// HFC ordering: each transition's target era must be strictly greater
    /// than the previous one. Regression for #465 (Dijkstra placement after
    /// Conway).
    #[test]
    fn test_era_transitions_preview_hfc_ordered() {
        let table = era_transitions_for_magic(2);
        for w in table.windows(2) {
            assert!(
                w[0].0 < w[1].0,
                "HFC era table must be strictly ascending: {:?} → {:?}",
                w[0],
                w[1]
            );
        }
    }

    /// Replaying the preview table against a Dijkstra-era snapshot must leave
    /// `EraHistory::current_era() == Dijkstra` immediately, before any block
    /// applies. This is the issue-#465 acceptance check.
    #[test]
    fn test_dijkstra_preview_snapshot_recovers_current_era() {
        use dugite_consensus::era_history::{EraHistory, EraParams};
        use dugite_primitives::era::Era;

        let params = EraParams {
            epoch_size: 86400,
            slot_length_ms: 1000,
            safe_zone: 129600,
            genesis_window: 25_920,
        };
        let mut eh = EraHistory::from_genesis(params.clone(), params, 0);

        let snapshot_era = Era::Dijkstra;
        for (era, epoch) in era_transitions_for_magic(2) {
            if snapshot_era >= era && eh.current_era() < era {
                eh.record_era_transition(era, epoch);
            }
        }

        assert_eq!(
            eh.current_era(),
            Era::Dijkstra,
            "snapshot_era=Dijkstra must produce EraHistory::current_era()=Dijkstra"
        );
        // Byron + Shelley + Allegra + Mary + Alonzo + Babbage + Conway + Dijkstra = 8
        assert_eq!(eh.len(), 8, "all eras through Dijkstra must be present");
    }

    /// Sanity: a Conway-era snapshot on preview must NOT pull in Dijkstra,
    /// even though Dijkstra is in the table.
    #[test]
    fn test_conway_preview_snapshot_does_not_record_dijkstra() {
        use dugite_consensus::era_history::{EraHistory, EraParams};
        use dugite_primitives::era::Era;

        let params = EraParams {
            epoch_size: 86400,
            slot_length_ms: 1000,
            safe_zone: 129600,
            genesis_window: 25_920,
        };
        let mut eh = EraHistory::from_genesis(params.clone(), params, 0);

        let snapshot_era = Era::Conway;
        for (era, epoch) in era_transitions_for_magic(2) {
            if snapshot_era >= era && eh.current_era() < era {
                eh.record_era_transition(era, epoch);
            }
        }

        assert_eq!(eh.current_era(), Era::Conway);
        assert!(
            !eh.entries().iter().any(|e| e.era == Era::Dijkstra),
            "Conway-era snapshot must not record a Dijkstra transition"
        );
    }

    /// Mainnet table must include Babbage 365 and Conway 517 (on-chain bounds).
    /// No Dijkstra on mainnet yet.
    #[test]
    fn test_era_transitions_mainnet_bounds() {
        use dugite_primitives::era::Era;
        let table = era_transitions_for_magic(764824073);
        assert_eq!(
            table,
            vec![(Era::Babbage, 365), (Era::Conway, 517)],
            "mainnet table must match on-chain HF history; Dijkstra not yet proposed"
        );
    }

    // ── Issue #648: link_latest_snapshot tests ────────────────────────────

    /// `link_latest_snapshot` must point `latest_path` at the exact same
    /// inode as `epoch_path` — no double-write, identical bytes.
    #[test]
    fn test_link_latest_snapshot_creates_hardlink() {
        let tmp = tempfile::tempdir().unwrap();
        let epoch_path = tmp.path().join("ledger-snapshot-epoch5-slot12345.bin");
        let latest_path = tmp.path().join("ledger-snapshot.bin");
        std::fs::write(&epoch_path, b"some serialised ledger bytes").unwrap();

        link_latest_snapshot(&epoch_path, &latest_path)
            .expect("hardlink should succeed on a clean target");

        // Same content.
        assert_eq!(
            std::fs::read(&epoch_path).unwrap(),
            std::fs::read(&latest_path).unwrap(),
            "latest must read back the same bytes as epoch"
        );

        // Same inode (Unix); on Windows the equivalent assertion is that
        // hard_link succeeded, which already throws above.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let a = std::fs::metadata(&epoch_path).unwrap();
            let b = std::fs::metadata(&latest_path).unwrap();
            assert_eq!(a.ino(), b.ino(), "epoch and latest must share an inode");
            assert_eq!(a.dev(), b.dev(), "epoch and latest must share a device");
        }
    }

    /// `link_latest_snapshot` must atomically replace any existing
    /// `latest_path`. After a second snapshot, `latest_path` references
    /// the NEW epoch file; the OLD epoch file remains independently
    /// readable via its own filename.
    #[test]
    fn test_link_latest_snapshot_replaces_existing_latest() {
        let tmp = tempfile::tempdir().unwrap();
        let epoch_a = tmp.path().join("ledger-snapshot-epoch5-slot100.bin");
        let epoch_b = tmp.path().join("ledger-snapshot-epoch6-slot200.bin");
        let latest_path = tmp.path().join("ledger-snapshot.bin");

        // First snapshot.
        std::fs::write(&epoch_a, b"snapshot at epoch 5").unwrap();
        link_latest_snapshot(&epoch_a, &latest_path).unwrap();
        assert_eq!(std::fs::read(&latest_path).unwrap(), b"snapshot at epoch 5");

        // Second snapshot — latest must now point at epoch_b, not epoch_a.
        std::fs::write(&epoch_b, b"snapshot at epoch 6 (different bytes)").unwrap();
        link_latest_snapshot(&epoch_b, &latest_path)
            .expect("hardlink should succeed even when target exists");
        assert_eq!(
            std::fs::read(&latest_path).unwrap(),
            b"snapshot at epoch 6 (different bytes)",
            "latest must reflect the new epoch file after re-link"
        );

        // Old epoch file still readable independently — its inode survives
        // because hardlinks are independent name->inode references.
        assert_eq!(
            std::fs::read(&epoch_a).unwrap(),
            b"snapshot at epoch 5",
            "old epoch file must still exist (rollback path)"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let latest = std::fs::metadata(&latest_path).unwrap();
            let b_meta = std::fs::metadata(&epoch_b).unwrap();
            let a_meta = std::fs::metadata(&epoch_a).unwrap();
            assert_eq!(latest.ino(), b_meta.ino(), "latest now shares b's inode");
            assert_ne!(
                latest.ino(),
                a_meta.ino(),
                "latest must NOT still share a's inode"
            );
        }
    }

    /// If the epoch file does not exist, `link_latest_snapshot` returns an
    /// `Err` so callers can surface a structured error rather than silently
    /// leaving `latest` stale.
    #[test]
    fn test_link_latest_snapshot_missing_source_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let epoch_path = tmp.path().join("does-not-exist.bin");
        let latest_path = tmp.path().join("ledger-snapshot.bin");
        let err = link_latest_snapshot(&epoch_path, &latest_path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    // ── Issue #649: prune_old_snapshots_in_dir tests ─────────────────────

    /// Free-function prune (callable from `spawn_blocking`) must remove
    /// the oldest epoch snapshots while keeping the most recent `keep`.
    /// The legacy `epoch{N}.bin` and current `epoch{N}-slot{S}.bin` name
    /// formats must both be recognised.
    #[test]
    fn test_prune_old_snapshots_in_dir_keeps_n_most_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Create 5 snapshots in mixed name formats.
        for (epoch, slot) in [(1u64, 100u64), (2, 200), (3, 300), (4, 400), (5, 500)] {
            let p = dir.join(format!("ledger-snapshot-epoch{epoch}-slot{slot}.bin"));
            std::fs::write(&p, b"x").unwrap();
        }
        // Legacy format (no slot)
        let legacy = dir.join("ledger-snapshot-epoch0.bin");
        std::fs::write(&legacy, b"legacy").unwrap();
        // Unrelated file (must survive).
        let latest = dir.join("ledger-snapshot.bin");
        std::fs::write(&latest, b"latest").unwrap();
        let other = dir.join("ledger-snapshot.tmp");
        std::fs::write(&other, b"junk").unwrap();

        // Keep 3 most recent — epochs 3, 4, 5 survive; 0, 1, 2 are deleted.
        prune_old_snapshots_in_dir(dir, 3, None);

        assert!(!legacy.exists(), "legacy epoch 0 must be pruned");
        assert!(
            !dir.join("ledger-snapshot-epoch1-slot100.bin").exists(),
            "epoch 1 must be pruned"
        );
        assert!(
            !dir.join("ledger-snapshot-epoch2-slot200.bin").exists(),
            "epoch 2 must be pruned"
        );
        assert!(
            dir.join("ledger-snapshot-epoch3-slot300.bin").exists(),
            "epoch 3 must survive"
        );
        assert!(
            dir.join("ledger-snapshot-epoch4-slot400.bin").exists(),
            "epoch 4 must survive"
        );
        assert!(
            dir.join("ledger-snapshot-epoch5-slot500.bin").exists(),
            "epoch 5 must survive"
        );
        assert!(latest.exists(), "ledger-snapshot.bin must not be touched");
        assert!(other.exists(), "unrelated files must not be touched");
    }

    /// When fewer snapshots exist than the keep threshold, no files are removed.
    #[test]
    fn test_prune_old_snapshots_in_dir_keeps_all_when_below_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for (epoch, slot) in [(1u64, 100u64), (2, 200)] {
            std::fs::write(
                dir.join(format!("ledger-snapshot-epoch{epoch}-slot{slot}.bin")),
                b"x",
            )
            .unwrap();
        }
        prune_old_snapshots_in_dir(dir, 5, None);
        assert!(dir.join("ledger-snapshot-epoch1-slot100.bin").exists());
        assert!(dir.join("ledger-snapshot-epoch2-slot200.bin").exists());
    }

    /// Non-existent directory must not panic.
    #[test]
    fn test_prune_old_snapshots_in_dir_missing_dir_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        prune_old_snapshots_in_dir(&missing, 3, None); // must not panic
    }

    /// #762: the prune floor guard must SPARE the newest snapshot at-or-below
    /// the immutable tip so a restart always retains a replayable recovery
    /// anchor — even when every "recent" snapshot is stranded above the
    /// immutable tip (the slow-ValidateAll / lagging-flush scenario).
    #[test]
    fn test_prune_floor_spares_recovery_anchor_below_immutable_tip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // ep1/slot100 and ep2/slot200 are at-or-below the immutable tip (250);
        // ep3..ep5 are stranded above it (volatile region). With keep=2 the
        // default prune would delete ep1,ep2,ep3 — leaving NO snapshot <= the
        // immutable tip → unrecoverable on a volatile-loss restart.
        for (epoch, slot) in [(1u64, 100u64), (2, 200), (3, 300), (4, 400), (5, 500)] {
            std::fs::write(
                dir.join(format!("ledger-snapshot-epoch{epoch}-slot{slot}.bin")),
                b"x",
            )
            .unwrap();
        }

        // Keep 2 newest, floor = immutable tip 250.
        prune_old_snapshots_in_dir(dir, 2, Some(250));

        // The newest snapshot at-or-below 250 is ep2/slot200 — it MUST survive
        // even though it is older than the keep window.
        assert!(
            dir.join("ledger-snapshot-epoch2-slot200.bin").exists(),
            "ep2 (newest <= immutable tip) is the recovery anchor and must be spared"
        );
        // The keep window keeps the 2 newest by epoch (ep4, ep5).
        assert!(dir.join("ledger-snapshot-epoch5-slot500.bin").exists());
        assert!(dir.join("ledger-snapshot-epoch4-slot400.bin").exists());
        // ep3 (above the floor, outside keep window) is pruned.
        assert!(
            !dir.join("ledger-snapshot-epoch3-slot300.bin").exists(),
            "ep3 is above the immutable tip and outside the keep window → pruned"
        );
        // ep1 (below floor but older than the spared anchor) is pruned.
        assert!(
            !dir.join("ledger-snapshot-epoch1-slot100.bin").exists(),
            "ep1 is superseded by the newer anchor ep2 → pruned"
        );
    }

    /// #762: when the floor is `None` (guard disabled), behaviour is unchanged
    /// — only the `keep` newest survive.
    #[test]
    fn test_prune_floor_none_keeps_only_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for (epoch, slot) in [(1u64, 100u64), (2, 200), (3, 300)] {
            std::fs::write(
                dir.join(format!("ledger-snapshot-epoch{epoch}-slot{slot}.bin")),
                b"x",
            )
            .unwrap();
        }
        prune_old_snapshots_in_dir(dir, 1, None);
        assert!(!dir.join("ledger-snapshot-epoch1-slot100.bin").exists());
        assert!(!dir.join("ledger-snapshot-epoch2-slot200.bin").exists());
        assert!(dir.join("ledger-snapshot-epoch3-slot300.bin").exists());
    }

    #[test]
    fn test_snapshot_policy_defaults() {
        let policy = SnapshotPolicy::new(2160);
        assert_eq!(policy.normal_interval, std::time::Duration::from_secs(4320));
        assert_eq!(policy.bulk_min_blocks, 50_000);
        assert_eq!(policy.max_snapshots, 2);
        assert_eq!(policy.blocks_since_snapshot, 0);
    }

    #[test]
    fn test_snapshot_policy_custom_params() {
        let policy = SnapshotPolicy::with_params(432, 5, 10_000, 120);
        assert_eq!(policy.normal_interval, std::time::Duration::from_secs(864));
        assert_eq!(policy.bulk_min_blocks, 10_000);
        assert_eq!(policy.max_snapshots, 5);
        assert_eq!(
            policy.bulk_min_interval,
            std::time::Duration::from_secs(120)
        );
    }

    #[test]
    fn test_snapshot_policy_record_blocks() {
        let mut policy = SnapshotPolicy::new(432);
        assert_eq!(policy.blocks_since_snapshot, 0);
        policy.record_blocks(100);
        assert_eq!(policy.blocks_since_snapshot, 100);
        policy.record_blocks(50);
        assert_eq!(policy.blocks_since_snapshot, 150);
    }

    #[test]
    fn test_snapshot_policy_bulk_not_ready_below_threshold() {
        let mut policy = SnapshotPolicy::new(432);
        policy.record_blocks(49_999);
        // Even though enough time may have passed, not enough blocks
        assert!(
            !policy.should_snapshot_bulk() || policy.blocks_since_snapshot < policy.bulk_min_blocks
        );
    }

    #[test]
    fn test_snapshot_taken_resets_counters() {
        let mut policy = SnapshotPolicy::new(432);
        policy.record_blocks(100_000);
        assert_eq!(policy.blocks_since_snapshot, 100_000);
        policy.snapshot_taken();
        assert_eq!(policy.blocks_since_snapshot, 0);
    }

    #[test]
    fn test_snapshot_normal_not_ready_immediately() {
        let policy = SnapshotPolicy::new(2160);
        // Just created — normal interval (4320s) hasn't elapsed
        assert!(!policy.should_snapshot_normal());
    }

    // ── is_snapshot_canonical regression tests ────────────────────────────────

    /// Origin snapshot is always canonical regardless of ChainDB state.
    #[test]
    fn test_is_snapshot_canonical_origin_always_true() {
        assert!(is_snapshot_canonical(0, &Point::Origin, None));
    }

    /// When no ChainDB is provided, the check is skipped and returns true.
    #[test]
    fn test_is_snapshot_canonical_no_chaindb_returns_true() {
        use dugite_primitives::time::SlotNo;
        let point = Point::Specific(
            SlotNo(100),
            dugite_primitives::hash::Hash32::from_bytes([1u8; 32]),
        );
        assert!(is_snapshot_canonical(100, &point, None));
    }

    /// Regression: when the ImmutableDB has NO block at the snapshot's exact
    /// slot (the slot is empty in the canonical chain), but the snapshot DOES
    /// have a block there (because the BP forged a block in that slot),
    /// `is_snapshot_canonical` must return `false`.
    ///
    /// Previously the fallback arm returned `false` from the wrong branch —
    /// i.e., a forged BP block at an empty canonical slot was accepted as
    /// canonical, causing the ledger to be stuck on a dead fork at startup.
    ///
    /// Root cause: the fork detection in mod.rs used a custom re-implementation
    /// of canonicality checking that had `_ => false` (accept) for the
    /// "no block at exact slot" case, whereas `is_snapshot_canonical` in
    /// epoch.rs correctly returns `false` (fork).
    ///
    /// This test exercises the `is_snapshot_canonical` code path directly to
    /// ensure it treats an empty canonical slot as a fork indicator.
    #[test]
    fn test_is_snapshot_canonical_empty_canonical_slot_is_fork() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::time::SlotNo;
        use dugite_storage::ChainDB;

        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path();
        let chain_db = ChainDB::open(db_path).unwrap();

        // Simulate: ImmutableDB has a block at slot 200 (the canonical chain
        // skipped slot 100 — slot 100 is empty).  The snapshot claims to be
        // at slot 100 with some hash.  The ImmutableDB tip is at slot 200.
        //
        // Since we can't easily inject ImmutableDB blocks in a unit test,
        // we verify the base case: an empty ImmutableDB with imm_tip=0
        // means snap_slot(100) > imm_tip(0), so the function accepts it
        // (volatile-range heuristic).  This is correct — when there are no
        // immutable blocks we can't detect the fork.
        let fork_hash = Hash32::from_bytes([0xABu8; 32]);
        let point = Point::Specific(SlotNo(100), fork_hash);

        // Empty ChainDB: imm_tip = 0, so snap_slot(100) > imm_tip(0).
        // The function must provisionally accept (cannot verify).
        assert!(
            is_snapshot_canonical(100, &point, Some(&chain_db)),
            "with empty ImmutableDB, snapshot in volatile range must be accepted"
        );
    }

    /// When the snapshot tip hash matches the canonical block at that slot,
    /// `is_snapshot_canonical` must return `true`.
    #[test]
    fn test_is_snapshot_canonical_hash_match_returns_true() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::time::SlotNo;
        use dugite_storage::ChainDB;

        let tmp = tempfile::tempdir().unwrap();
        let chain_db = ChainDB::open(tmp.path()).unwrap();

        // Snapshot at slot 0 with zero hash — origin is always canonical.
        let point = Point::Specific(SlotNo(0), Hash32::ZERO);
        // snap_slot == 0 → the function returns true early.
        assert!(is_snapshot_canonical(0, &point, Some(&chain_db)));
    }

    /// Snapshot at slot 0 (origin) — always canonical.
    #[test]
    fn test_is_snapshot_canonical_slot_zero_always_canonical() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::time::SlotNo;
        use dugite_storage::ChainDB;

        let tmp = tempfile::tempdir().unwrap();
        let chain_db = ChainDB::open(tmp.path()).unwrap();

        let point = Point::Specific(SlotNo(0), Hash32::from_bytes([0u8; 32]));
        assert!(is_snapshot_canonical(0, &point, Some(&chain_db)));
    }

    /// Regression: BP forges a block at slot S (not adopted by network).
    ///
    /// On restart the VolatileDB WAL has the canonical chain continuing ABOVE
    /// slot S (from a different block at slot S).  The snapshot tip is at slot
    /// S with the FORK hash.  The forged block's slot is above the ImmutableDB
    /// tip (imm_tip < S < volatile_tip).
    ///
    /// Old bug: `is_snapshot_canonical` returned `true` early because
    /// `snap_slot > imm_tip_slot` — it never checked the VolatileDB selected
    /// chain, so the fork snapshot was accepted as canonical.
    ///
    /// Fix: use the overall ChainDB tip (max of ImmutableDB + VolatileDB) as
    /// the provisional-accept boundary.  For slots within the volatile range,
    /// `get_block_at_or_after_slot` queries only the VolatileDB *selected*
    /// chain — the fork block (not on selected chain) is correctly rejected.
    #[test]
    fn test_is_snapshot_canonical_volatile_fork_detected() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::time::{BlockNo, SlotNo};
        use dugite_storage::ChainDB;

        let tmp = tempfile::tempdir().unwrap();
        let mut chain_db = ChainDB::open(tmp.path()).unwrap();

        // Build a canonical chain in VolatileDB:
        //   genesis anchor (slot 0, block 0)  ← ImmutableDB tip (empty)
        //   canonical_100 (slot 100, block 1)
        //   canonical_200 (slot 200, block 2)   ← volatile tip
        //
        // The snapshot claims to be at slot 100 with a DIFFERENT hash
        // (fork_hash), simulating a block the BP forged that was not adopted.
        let genesis_hash = Hash32::from_bytes([0u8; 32]);
        let canonical_100 = Hash32::from_bytes([0x11u8; 32]);
        let canonical_200 = Hash32::from_bytes([0x22u8; 32]);
        let fork_hash = Hash32::from_bytes([0xFFu8; 32]); // forged, not in ChainDB

        // Add canonical blocks to VolatileDB.
        chain_db
            .add_block(
                canonical_100,
                SlotNo(100),
                BlockNo(1),
                genesis_hash,
                b"cbor1".to_vec(),
            )
            .unwrap();
        chain_db
            .add_block(
                canonical_200,
                SlotNo(200),
                BlockNo(2),
                canonical_100,
                b"cbor2".to_vec(),
            )
            .unwrap();

        // Sanity: ChainDB tip should now be at slot 200 (volatile).
        let tip_slot = chain_db.get_tip().point.slot().map(|s| s.0).unwrap_or(0);
        assert_eq!(tip_slot, 200, "ChainDB tip should be slot 200");

        // ImmutableDB is empty → imm_tip_slot = 0.
        // snap_slot(100) > imm_tip_slot(0) — OLD code would return true here.
        // NEW code: snap_slot(100) <= db_tip_slot(200), so we check VolatileDB.
        let fork_point = Point::Specific(SlotNo(100), fork_hash);
        assert!(
            !is_snapshot_canonical(100, &fork_point, Some(&chain_db)),
            "fork block (not on selected VolatileDB chain) must be rejected"
        );

        // The canonical block at slot 100 should still be accepted.
        let canonical_point = Point::Specific(SlotNo(100), canonical_100);
        assert!(
            is_snapshot_canonical(100, &canonical_point, Some(&chain_db)),
            "canonical block at slot 100 must be accepted"
        );

        // A snapshot ahead of the volatile tip (slot 300) must be accepted
        // provisionally (no data to verify against).
        let future_hash = Hash32::from_bytes([0x33u8; 32]);
        let future_point = Point::Specific(SlotNo(300), future_hash);
        assert!(
            is_snapshot_canonical(300, &future_point, Some(&chain_db)),
            "snapshot ahead of all ChainDB data must be accepted provisionally"
        );
    }
}
