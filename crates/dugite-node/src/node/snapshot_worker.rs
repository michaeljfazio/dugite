//! Background snapshot worker (issue #695).
//!
//! `SnapshotWorker` is a long-lived task that owns the disk-bound bincode
//! walk + atomic rename + prune that previously ran on the apply path.
//! The apply path now builds a `LedgerStateSnapshot` view under the
//! ledger write lock (Phase A), fires a single `try_send` on a bounded
//! mpsc channel, and continues — mirroring cardano-node's
//! `ledgerDbTaskWatcher` model.
//!
//! ## Lifecycle
//!
//! - `spawn_snapshot_worker` constructs the channel and spawns the
//!   worker task. The caller holds the `Sender` and the `JoinHandle`.
//! - At shutdown the caller drops the `Sender`. The worker's
//!   `recv().await` returns `None`, the task exits cleanly, and the
//!   `snapshot_worker_alive` gauge drops to 0.
//!
//! ## Backpressure
//!
//! Channel capacity = 1. If the worker is still writing the previous
//! snapshot when a new request arrives, the apply path's `try_send`
//! returns `Full` and the save is skipped (logged + counted). This
//! mirrors cardano-node's `SnapshotDelayRange` random-delay gate, which
//! skips taking a snapshot when one was taken too recently.
//!
//! ## Panic isolation
//!
//! Each per-request write runs inside `tokio::task::spawn_blocking` +
//! `std::panic::catch_unwind`. A malformed `LedgerStateSnapshot` or a
//! bincode/IO panic increments `snapshot_failed_total` but does not
//! kill the worker task.

use std::path::PathBuf;
use std::sync::Arc;

use dugite_ledger::LedgerStateSnapshot;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use super::epoch::{link_latest_snapshot, prune_old_snapshots_in_dir};
use crate::metrics::NodeMetrics;

/// Default bounded channel capacity. Capacity 1 means at most one
/// in-flight save + one buffered request; further requests are
/// `Full`-skipped (matches cardano-node's delay-gate semantics).
pub const SNAPSHOT_WORKER_CHANNEL_CAPACITY: usize = 1;

/// One unit of work for the snapshot worker.
///
/// Built by `Node::try_snapshot_async` under the ledger write lock and
/// then sent via `mpsc`. All fields are `Send + 'static` — the big
/// shared maps inside `view` are `Arc::clone`d (delegations,
/// reward_accounts, pool_params, governance) while the few unsynced
/// maps are deep-cloned at view construction. The worker then walks
/// the view via bincode without re-touching the ledger.
pub struct SnapshotRequest {
    pub view: LedgerStateSnapshot,
    pub epoch: u64,
    pub slot: u64,
    pub utxo_count: usize,
    /// UTxO backend the snapshot was taken with — written into the
    /// `.meta.json` sidecar so a backend-mismatched snapshot is rejected
    /// on load (mirrors Haskell's `SnapshotMetadata` backend tag).
    pub backend: dugite_ledger::SnapshotBackend,
    /// ImmutableDB tip slot at enqueue time (#762). The prune floor guard
    /// spares the newest snapshot at-or-below this slot so a restart always
    /// retains a replayable recovery anchor. `0` disables the floor guard.
    pub immutable_tip_slot: u64,
}

/// Construct the channel + spawn the worker task.
///
/// Returns the sender (clone-and-store on `Node`) and the join handle
/// (await during shutdown to ensure the worker drains in-flight work
/// before the synchronous final save fires).
pub fn spawn_snapshot_worker(
    database_path: PathBuf,
    max_snapshots: usize,
    metrics: Arc<NodeMetrics>,
) -> (mpsc::Sender<SnapshotRequest>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(SNAPSHOT_WORKER_CHANNEL_CAPACITY);
    let handle = tokio::spawn(run_snapshot_worker(
        database_path,
        max_snapshots,
        metrics,
        rx,
    ));
    (tx, handle)
}

/// The worker's main loop.
///
/// Receives requests until the sender is dropped, then exits. Each
/// request is processed on the tokio blocking pool with a
/// `catch_unwind` guard so a panic on one request doesn't disable
/// future snapshots.
pub async fn run_snapshot_worker(
    database_path: PathBuf,
    max_snapshots: usize,
    metrics: Arc<NodeMetrics>,
    mut rx: mpsc::Receiver<SnapshotRequest>,
) {
    metrics.set_snapshot_worker_alive(true);
    info!("snapshot worker started");

    while let Some(req) = rx.recv().await {
        let db = database_path.clone();
        // spawn_blocking owns the CPU+disk-bound work. The view is
        // moved across threads (it is Send + 'static). Metrics that
        // belong to the worker (failure counter, alive gauge) are
        // bumped in the outer loop — process_request only does I/O.
        let join = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                process_request(&req, &db, max_snapshots)
            }))
        })
        .await;

        match join {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => {
                metrics.inc_snapshot_failed();
                error!(error = %e, "snapshot worker write failed");
            }
            Ok(Err(_panic)) => {
                metrics.inc_snapshot_failed();
                error!("snapshot worker write panicked (caught — worker stays alive)");
            }
            Err(join_err) => {
                metrics.inc_snapshot_failed();
                error!(error = %join_err, "snapshot worker JoinError");
            }
        }
    }

    metrics.set_snapshot_worker_alive(false);
    info!("snapshot worker exiting (sender dropped)");
}

/// Per-request body. Stays panic-safe; returns `Err` on any I/O or
/// serialisation failure so the worker can bump the failure metric.
fn process_request(
    req: &SnapshotRequest,
    database_path: &std::path::Path,
    max_snapshots: usize,
) -> Result<(), String> {
    let epoch_path = database_path.join(format!(
        "ledger-snapshot-epoch{}-slot{}.bin",
        req.epoch, req.slot
    ));
    let latest_path = database_path.join("ledger-snapshot.bin");

    let total_bytes = dugite_ledger::LedgerState::write_snapshot_view_to_path(
        &req.view,
        &epoch_path,
        req.backend,
    )
    .map_err(|e| format!("write snapshot: {e}"))?;

    if let Err(e) = link_latest_snapshot(&epoch_path, &latest_path) {
        // Non-fatal: the epoch-tagged snapshot is on disk and startup
        // can fall back to it via `find_best_snapshot_for_rollback`.
        warn!(error = %e, "failed to refresh latest.bin hardlink");
    }

    // Prune retains `max_snapshots + 1` so the freshly-written one
    // doesn't get nuked by its own prune pass. #762: also spare the newest
    // snapshot at-or-below the ImmutableDB tip (restart-recovery anchor).
    let floor = (req.immutable_tip_slot > 0).then_some(req.immutable_tip_slot);
    prune_old_snapshots_in_dir(database_path, max_snapshots.saturating_add(1), floor);

    info!(
        epoch = req.epoch,
        slot = req.slot,
        utxos = req.utxo_count,
        bytes_mb = format_args!("{:.1}", total_bytes as f64 / 1_048_576.0),
        "Snapshot     saved"
    );
    Ok(())
}

/// Pre-flight check used by `Node::try_snapshot_async` to skip the
/// Phase-A view-build when the worker is busy.
///
/// `tokio::sync::mpsc::Sender::capacity` returns the current spare
/// capacity (`max_capacity - queued`). For our `channel(1)` channel:
///
/// - `capacity() == 1` — worker is idle or processing without a
///   buffered item; a fresh send will succeed.
/// - `capacity() == 0` — one item is queued; a `try_send` would
///   return `Full`. Skip without taking the ledger lock.
pub fn has_capacity(tx: &mpsc::Sender<SnapshotRequest>) -> bool {
    tx.capacity() > 0
}

/// Result of [`Node::try_snapshot_async`].
///
/// Callers branch on this to decide whether to mark their scheduler's
/// counter as "snapshot taken". Skipping on `Skipped` / `Closed`
/// preserves the scheduler's `blocks_since_snapshot` so the next
/// block retriggers the check — otherwise the
/// `DEFAULT_SNAPSHOT_INTERVAL` (2000 blocks ≈ 11 h on mainnet) would
/// delay the next attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotEnqueue {
    /// Successfully sent to the worker. The save will fire on the
    /// background task. Caller should record the snapshot as taken.
    Enqueued,
    /// Channel was full (worker still writing the previous snapshot).
    /// Save was not performed. Caller must NOT mark the scheduler as
    /// having snapshotted; the next block will retrigger the check.
    Skipped,
    /// Worker sender is gone (channel closed at shutdown).
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_ledger::LedgerState;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use tempfile::tempdir;

    fn build_view() -> LedgerStateSnapshot {
        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        LedgerStateSnapshot::from(&state)
    }

    /// Happy path: a single request is written to disk with the
    /// `DUGT` framing header and the latest.bin hardlink.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_writes_request_to_disk() {
        let dir = tempdir().unwrap();
        let metrics = Arc::new(NodeMetrics::new());

        let (tx, handle) = spawn_snapshot_worker(dir.path().to_path_buf(), 2, metrics.clone());

        tx.send(SnapshotRequest {
            view: build_view(),
            epoch: 42,
            slot: 1234,
            utxo_count: 0,
            backend: dugite_ledger::SnapshotBackend::DugiteMem,
            immutable_tip_slot: 0,
        })
        .await
        .unwrap();

        // Drop the sender so the worker exits and we can deterministically
        // observe completion via the JoinHandle.
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(10), handle)
            .await
            .expect("worker did not exit in time")
            .expect("worker JoinHandle errored");

        let path = dir.path().join("ledger-snapshot-epoch42-slot1234.bin");
        assert!(path.exists(), "epoch-tagged snapshot must exist");
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(&raw[..4], b"DUGT", "magic word must be present");

        let latest = dir.path().join("ledger-snapshot.bin");
        assert!(latest.exists(), "latest.bin must be hardlinked");

        assert_eq!(
            metrics
                .snapshot_failed_total
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "no failures should have been recorded"
        );
        assert_eq!(
            metrics
                .snapshot_worker_alive
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "alive gauge must be cleared after exit"
        );
    }

    /// `has_capacity` reflects channel state across the saturation
    /// boundary.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn has_capacity_tracks_channel_saturation() {
        let (tx, mut rx) = mpsc::channel::<SnapshotRequest>(SNAPSHOT_WORKER_CHANNEL_CAPACITY);
        assert!(has_capacity(&tx), "fresh channel has capacity");

        tx.try_send(SnapshotRequest {
            view: build_view(),
            epoch: 1,
            slot: 0,
            utxo_count: 0,
            backend: dugite_ledger::SnapshotBackend::DugiteMem,
            immutable_tip_slot: 0,
        })
        .unwrap();
        assert!(
            !has_capacity(&tx),
            "channel must be saturated after one queued message"
        );

        // Drain — capacity returns.
        let _ = rx.recv().await.unwrap();
        assert!(has_capacity(&tx), "capacity restored after drain");
    }

    /// Dropping the sender causes the worker to exit cleanly within
    /// a short timeout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_exits_when_sender_dropped() {
        let dir = tempdir().unwrap();
        let metrics = Arc::new(NodeMetrics::new());

        let (tx, handle) = spawn_snapshot_worker(dir.path().to_path_buf(), 2, metrics.clone());
        drop(tx);

        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("worker did not exit within timeout")
            .expect("worker join errored");

        assert_eq!(
            metrics
                .snapshot_worker_alive
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "alive gauge must drop to 0 on exit"
        );
    }

    /// After processing N snapshots with `max_snapshots = 2`, only the
    /// two newest epoch-tagged files survive on disk (plus `latest.bin`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prune_runs_after_each_write() {
        let dir = tempdir().unwrap();
        let metrics = Arc::new(NodeMetrics::new());

        let (tx, handle) = spawn_snapshot_worker(dir.path().to_path_buf(), 2, metrics.clone());

        for epoch in 1..=4u64 {
            tx.send(SnapshotRequest {
                view: build_view(),
                epoch,
                slot: epoch * 1000,
                utxo_count: 0,
                backend: dugite_ledger::SnapshotBackend::DugiteMem,
                immutable_tip_slot: 0,
            })
            .await
            .unwrap();
        }
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(15), handle)
            .await
            .expect("worker did not exit in time")
            .expect("worker JoinHandle errored");

        // Enumerate epoch-tagged files.
        let mut epoch_files: Vec<u64> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| {
                let e = e.ok()?;
                let name = e.file_name().to_string_lossy().to_string();
                let rest = name.strip_prefix("ledger-snapshot-epoch")?;
                let epoch_str = rest.strip_suffix(".bin")?;
                let epoch_part = epoch_str.split("-slot").next()?;
                epoch_part.parse::<u64>().ok()
            })
            .collect();
        epoch_files.sort_unstable();

        // `max_snapshots + 1 = 3` are retained so the final write is
        // not pruned by its own pass.
        assert_eq!(
            epoch_files,
            vec![2, 3, 4],
            "only the three most recent epoch-tagged snapshots should remain"
        );
    }

    /// Smoke test: the `SnapshotEnqueue` variants are `Eq` so callers
    /// can match on them deterministically.
    #[test]
    fn snapshot_enqueue_variants_compare_equal() {
        assert_eq!(SnapshotEnqueue::Enqueued, SnapshotEnqueue::Enqueued);
        assert_ne!(SnapshotEnqueue::Enqueued, SnapshotEnqueue::Skipped);
        assert_ne!(SnapshotEnqueue::Skipped, SnapshotEnqueue::Closed);
    }
}
