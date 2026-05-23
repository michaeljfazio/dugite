use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use fs2::available_space;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::metrics::NodeMetrics;

/// Disk space warning thresholds
const WARNING_BYTES: u64 = 10 * 1024 * 1024 * 1024; // 10 GB
const CRITICAL_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GB
const FATAL_BYTES: u64 = 500 * 1024 * 1024; // 500 MB

/// Block ingestion is paused when free space falls below this threshold (1 GB).
///
/// This is intentionally above `FATAL_BYTES` (500 MB) so the guard fires before
/// the node is at extreme risk, giving operators time to react.
pub const PAUSE_THRESHOLD_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB

/// Block ingestion resumes only after free space has been ≥ this value for
/// `RECOVER_HOLD_SECS` consecutive seconds, preventing rapid oscillation.
pub const RECOVER_THRESHOLD_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GB

/// Number of consecutive seconds free space must be ≥ `RECOVER_THRESHOLD_BYTES`
/// before the ingestion-paused flag is cleared.
const RECOVER_HOLD_SECS: u64 = 60;

/// How often to check disk space (in seconds)
const CHECK_INTERVAL_SECS: u64 = 60;

/// Disk space severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskSpaceLevel {
    /// Plenty of space available
    Ok,
    /// Below 10 GB — operator should investigate
    Warning,
    /// Below 2 GB — node may soon be unable to store blocks
    Critical,
    /// Below 500 MB — node should refuse new blocks to protect data integrity
    Fatal,
}

impl std::fmt::Display for DiskSpaceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiskSpaceLevel::Ok => write!(f, "ok"),
            DiskSpaceLevel::Warning => write!(f, "warning"),
            DiskSpaceLevel::Critical => write!(f, "critical"),
            DiskSpaceLevel::Fatal => write!(f, "fatal"),
        }
    }
}

/// Returns the available disk space in bytes for the filesystem containing `path`.
pub fn check_disk_space(path: &Path) -> std::io::Result<u64> {
    available_space(path)
}

/// Returns (total_bytes, used_bytes) for the filesystem containing `path`.
/// Returns None if the statvfs call fails or on non-Unix platforms.
pub fn check_disk_total_used(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;

        // SAFETY: `mem::zeroed()` is safe here because:
        // 1. `statvfs` is a system call that writes all fields of the struct
        // 2. We immediately call `statvfs` which fills the struct before reading
        // 3. The struct is not used before being written by the syscall
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };

        // SAFETY: `statvfs(c_path.as_ptr(), &mut stat)` is safe because:
        // 1. `c_path` is a valid C string (CString::new succeeded)
        // 2. `stat` points to valid, aligned memory
        // 3. `statvfs` writes to the struct and we check return value for errors
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if ret != 0 {
            return None;
        }
        #[allow(clippy::unnecessary_cast)]
        let block_size = stat.f_frsize as u64;
        #[allow(clippy::unnecessary_cast)]
        let total = stat.f_blocks as u64 * block_size;
        #[allow(clippy::unnecessary_cast)]
        let used = total.saturating_sub(stat.f_bfree as u64 * block_size);
        Some((total, used))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Classify available bytes into a severity level.
pub fn classify_disk_space(available_bytes: u64) -> DiskSpaceLevel {
    if available_bytes < FATAL_BYTES {
        DiskSpaceLevel::Fatal
    } else if available_bytes < CRITICAL_BYTES {
        DiskSpaceLevel::Critical
    } else if available_bytes < WARNING_BYTES {
        DiskSpaceLevel::Warning
    } else {
        DiskSpaceLevel::Ok
    }
}

/// Format bytes as a human-readable string (e.g. "12.34 GB").
fn format_bytes(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else {
        format!("{:.2} MB", b / MB)
    }
}

/// Core pause/resume state machine for block ingestion back-pressure.
///
/// Extracted from `start_disk_monitor` so it can be unit-tested without a real
/// filesystem or tokio runtime.
///
/// # Arguments
///
/// * `available` — bytes of free space on the database volume.
/// * `ingestion_paused` — shared flag read by the sync/apply paths.
/// * `recover_ticks` — mutable counter: consecutive ticks above `RECOVER_THRESHOLD_BYTES`.
/// * `paused_ticks` — mutable counter: consecutive ticks spent paused (for logging).
/// * `recover_ticks_needed` — how many ticks above `RECOVER_THRESHOLD_BYTES` are required
///   before clearing the flag (= `RECOVER_HOLD_SECS / CHECK_INTERVAL_SECS`).
pub(crate) fn evaluate_ingestion_pause(
    available: u64,
    ingestion_paused: &Arc<AtomicBool>,
    recover_ticks: &mut u64,
    paused_ticks: &mut u64,
    recover_ticks_needed: u64,
) {
    let currently_paused = ingestion_paused.load(Ordering::Relaxed);

    if available < PAUSE_THRESHOLD_BYTES {
        // Enter / stay in paused state.
        *recover_ticks = 0;
        if !currently_paused {
            ingestion_paused.store(true, Ordering::Relaxed);
            *paused_ticks = 0;
        } else {
            *paused_ticks = paused_ticks.saturating_add(1);
        }
    } else if currently_paused {
        // Potentially recovering.
        if available >= RECOVER_THRESHOLD_BYTES {
            *recover_ticks = recover_ticks.saturating_add(1);
            if *recover_ticks >= recover_ticks_needed {
                ingestion_paused.store(false, Ordering::Relaxed);
                *paused_ticks = 0;
            }
        } else {
            // Between PAUSE and RECOVER thresholds — reset recovery counter, stay paused.
            *recover_ticks = 0;
            *paused_ticks = paused_ticks.saturating_add(1);
        }
    } else {
        // Healthy — not paused, not below pause threshold.
        *recover_ticks = 0;
        *paused_ticks = 0;
    }
}

/// Spawn a background task that periodically checks disk space on the database volume,
/// logs warnings at appropriate severity levels, updates the Prometheus metric, and
/// sets/clears `ingestion_paused` to back-pressure block ingestion when disk is low.
///
/// # Pause / resume hysteresis
///
/// * **Pause**: `ingestion_paused` is set to `true` the instant free space drops below
///   `PAUSE_THRESHOLD_BYTES` (1 GB).  The sync loop checks this flag before committing
///   any block to ChainDB, so new blocks are held without touching the database.
/// * **Resume**: the flag is cleared only after free space has been ≥
///   `RECOVER_THRESHOLD_BYTES` (5 GB) for at least `RECOVER_HOLD_SECS` (60 s)
///   consecutive seconds.  This prevents rapid pause/resume oscillation when the
///   operator is actively freeing space.
///
/// While paused, an INFO message is logged once per minute with the current free space
/// and how long ingestion has been suspended.
pub async fn start_disk_monitor(
    database_path: std::path::PathBuf,
    metrics: Arc<NodeMetrics>,
    mut shutdown_rx: watch::Receiver<bool>,
    disk_level_tx: watch::Sender<DiskSpaceLevel>,
    ingestion_paused: Arc<AtomicBool>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(CHECK_INTERVAL_SECS));

    // Hysteresis state.
    let recover_ticks_needed = RECOVER_HOLD_SECS / CHECK_INTERVAL_SECS.max(1);
    let mut recover_ticks: u64 = 0;
    // How many ticks have we been paused?  Used to compute pause duration for logging.
    let mut paused_ticks: u64 = 0;

    // Do the first check immediately.
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown_rx.changed() => {
                debug!("Disk monitor shutting down");
                return;
            }
        }

        match check_disk_space(&database_path) {
            Ok(available) => {
                metrics.set_disk_available_bytes(available);
                // Also update total/used metrics for the monitor dashboard.
                if let Some((total, used)) = check_disk_total_used(&database_path) {
                    metrics.set_disk_total_bytes(total);
                    metrics.set_disk_used_bytes(used);
                }
                let level = classify_disk_space(available);
                // Publish the current disk space level so the sync loop can react
                let _ = disk_level_tx.send(level);
                let human = format_bytes(available);

                // ── Ingestion-pause logic ────────────────────────────────────
                //
                // Snapshot the flag BEFORE calling evaluate_ingestion_pause so
                // we can log the state transition accurately.
                let was_paused = ingestion_paused.load(Ordering::Relaxed);
                evaluate_ingestion_pause(
                    available,
                    &ingestion_paused,
                    &mut recover_ticks,
                    &mut paused_ticks,
                    recover_ticks_needed,
                );
                let is_paused = ingestion_paused.load(Ordering::Relaxed);

                // Log state transitions and periodic status.
                if available < PAUSE_THRESHOLD_BYTES {
                    if !was_paused {
                        // Just entered paused state.
                        error!(
                            available_bytes = available,
                            pause_threshold_bytes = PAUSE_THRESHOLD_BYTES,
                            "FATAL: Disk space critically low ({human}) — \
                             block ingestion PAUSED to protect data integrity. \
                             Free at least {} to resume.",
                            format_bytes(RECOVER_THRESHOLD_BYTES),
                        );
                    } else {
                        // Already paused — log INFO once per minute.
                        let paused_secs = paused_ticks * CHECK_INTERVAL_SECS;
                        info!(
                            available_bytes = available,
                            paused_secs,
                            "Disk space low ({human}): block ingestion still paused \
                             (paused for {paused_secs}s). Free at least {} to resume.",
                            format_bytes(RECOVER_THRESHOLD_BYTES),
                        );
                    }
                } else if was_paused && !is_paused {
                    // Just cleared paused state (sustained recovery reached).
                    info!(
                        available_bytes = available,
                        "Disk space recovered ({human}) — block ingestion RESUMED."
                    );
                } else if was_paused {
                    // Still paused; log recovery progress or continued wait.
                    let paused_secs = paused_ticks * CHECK_INTERVAL_SECS;
                    if available >= RECOVER_THRESHOLD_BYTES {
                        let secs_recovered = recover_ticks * CHECK_INTERVAL_SECS;
                        info!(
                            available_bytes = available,
                            paused_secs,
                            "Disk space recovering ({human}): {secs_recovered}s of {}s \
                             sustained recovery before ingestion resumes.",
                            RECOVER_HOLD_SECS,
                        );
                    } else {
                        info!(
                            available_bytes = available,
                            paused_secs,
                            "Disk space low ({human}): block ingestion still paused \
                             (paused for {paused_secs}s). Free at least {} to resume.",
                            format_bytes(RECOVER_THRESHOLD_BYTES),
                        );
                    }
                }

                // ── Severity logging ─────────────────────────────────────────
                match level {
                    DiskSpaceLevel::Fatal => {
                        error!(
                            available_bytes = available,
                            "FATAL: Disk space critically low ({human}) — \
                             node should stop accepting new blocks to protect data integrity"
                        );
                    }
                    DiskSpaceLevel::Critical => {
                        error!(
                            available_bytes = available,
                            "CRITICAL: Disk space very low ({human}) — \
                             node may soon be unable to store blocks"
                        );
                    }
                    DiskSpaceLevel::Warning => {
                        warn!(
                            available_bytes = available,
                            "Disk space low ({human}) — consider freeing space or expanding volume"
                        );
                    }
                    DiskSpaceLevel::Ok => {
                        // Only log at debug level when things are healthy
                        tracing::debug!(
                            available_bytes = available,
                            "Disk space check: {human} available"
                        );
                    }
                }
            }
            Err(e) => {
                error!(
                    "Failed to check disk space on {}: {e}",
                    database_path.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_check_disk_space_returns_reasonable_value() {
        // Check disk space on the current directory — should always succeed and
        // return a positive value on any system with a working filesystem.
        let available = check_disk_space(Path::new(".")).expect("check_disk_space should succeed");
        // Any modern OS should have at least 1 MB free on the root filesystem
        assert!(
            available > 1024 * 1024,
            "expected at least 1 MB free, got {available} bytes"
        );
    }

    #[test]
    fn test_check_disk_space_nonexistent_path() {
        let result = check_disk_space(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_err(), "should fail for nonexistent path");
    }

    #[test]
    fn test_classify_disk_space_ok() {
        // 20 GB — well above all thresholds
        let level = classify_disk_space(20 * 1024 * 1024 * 1024);
        assert_eq!(level, DiskSpaceLevel::Ok);
    }

    #[test]
    fn test_classify_disk_space_warning() {
        // 5 GB — below warning (10 GB), above critical (2 GB)
        let level = classify_disk_space(5 * 1024 * 1024 * 1024);
        assert_eq!(level, DiskSpaceLevel::Warning);
    }

    #[test]
    fn test_classify_disk_space_critical() {
        // 1 GB — below critical (2 GB), above fatal (500 MB)
        let level = classify_disk_space(1024 * 1024 * 1024);
        assert_eq!(level, DiskSpaceLevel::Critical);
    }

    #[test]
    fn test_classify_disk_space_fatal() {
        // 100 MB — below fatal (500 MB)
        let level = classify_disk_space(100 * 1024 * 1024);
        assert_eq!(level, DiskSpaceLevel::Fatal);
    }

    #[test]
    fn test_classify_disk_space_zero() {
        let level = classify_disk_space(0);
        assert_eq!(level, DiskSpaceLevel::Fatal);
    }

    #[test]
    fn test_classify_disk_space_boundary_warning() {
        // Exactly at the warning threshold — should be warning (strictly less than)
        let level = classify_disk_space(WARNING_BYTES);
        assert_eq!(level, DiskSpaceLevel::Ok);

        let level = classify_disk_space(WARNING_BYTES - 1);
        assert_eq!(level, DiskSpaceLevel::Warning);
    }

    #[test]
    fn test_classify_disk_space_boundary_critical() {
        let level = classify_disk_space(CRITICAL_BYTES);
        assert_eq!(level, DiskSpaceLevel::Warning);

        let level = classify_disk_space(CRITICAL_BYTES - 1);
        assert_eq!(level, DiskSpaceLevel::Critical);
    }

    #[test]
    fn test_classify_disk_space_boundary_fatal() {
        let level = classify_disk_space(FATAL_BYTES);
        assert_eq!(level, DiskSpaceLevel::Critical);

        let level = classify_disk_space(FATAL_BYTES - 1);
        assert_eq!(level, DiskSpaceLevel::Fatal);
    }

    #[test]
    fn test_format_bytes_gb() {
        let s = format_bytes(10 * 1024 * 1024 * 1024);
        assert_eq!(s, "10.00 GB");
    }

    #[test]
    fn test_format_bytes_mb() {
        let s = format_bytes(512 * 1024 * 1024);
        assert_eq!(s, "512.00 MB");
    }

    #[test]
    fn test_disk_space_level_display() {
        assert_eq!(DiskSpaceLevel::Ok.to_string(), "ok");
        assert_eq!(DiskSpaceLevel::Warning.to_string(), "warning");
        assert_eq!(DiskSpaceLevel::Critical.to_string(), "critical");
        assert_eq!(DiskSpaceLevel::Fatal.to_string(), "fatal");
    }

    #[tokio::test]
    async fn test_disk_level_watch_channel() {
        use std::time::Duration;
        use tokio::sync::watch;

        let metrics = Arc::new(NodeMetrics::new());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (disk_level_tx, mut disk_level_rx) = watch::channel(DiskSpaceLevel::Ok);
        let ingestion_paused = Arc::new(AtomicBool::new(false));

        // Spawn the monitor on the current directory (always has space)
        let db_path = std::path::PathBuf::from(".");
        let paused_clone = ingestion_paused.clone();
        tokio::spawn(async move {
            start_disk_monitor(db_path, metrics, shutdown_rx, disk_level_tx, paused_clone).await;
        });

        // Wait for the first check to publish a level
        tokio::time::timeout(Duration::from_secs(5), disk_level_rx.changed())
            .await
            .expect("timed out waiting for disk level update")
            .expect("watch channel closed unexpectedly");

        // On any dev machine, the level should be Ok (plenty of space)
        let level = *disk_level_rx.borrow();
        assert_eq!(
            level,
            DiskSpaceLevel::Ok,
            "expected Ok disk level on dev machine, got {level:?}"
        );

        // Ingestion should NOT be paused on a healthy dev machine
        assert!(
            !ingestion_paused.load(Ordering::Relaxed),
            "ingestion_paused should be false when disk has plenty of space"
        );

        // Shutdown the monitor
        shutdown_tx.send(true).ok();
    }

    /// Verify the pause/resume logic using a controlled `Arc<AtomicBool>`.
    ///
    /// We exercise the monitor logic directly through `evaluate_ingestion_pause`,
    /// bypassing the real filesystem, so this test runs without any temp-dir
    /// or mock-mount infrastructure and produces no flaky I/O dependencies.
    #[test]
    fn test_ingestion_pause_sets_flag_below_threshold() {
        // Below PAUSE_THRESHOLD_BYTES → flag must be set immediately.
        let paused = Arc::new(AtomicBool::new(false));
        let available = PAUSE_THRESHOLD_BYTES - 1;
        evaluate_ingestion_pause(available, &paused, &mut 0u64, &mut 0u64, 1);
        assert!(
            paused.load(Ordering::Relaxed),
            "ingestion_paused should be true when available < PAUSE_THRESHOLD_BYTES"
        );
    }

    #[test]
    fn test_ingestion_pause_not_cleared_before_recover_hold() {
        // Simulate: flag already set, free space now above RECOVER_THRESHOLD_BYTES,
        // but only for 0 ticks → should stay paused.
        let paused = Arc::new(AtomicBool::new(true));
        let available = RECOVER_THRESHOLD_BYTES + 1;
        let recover_ticks_needed = 2u64;
        let mut recover_ticks = 0u64;
        let mut paused_ticks = 5u64;
        evaluate_ingestion_pause(
            available,
            &paused,
            &mut recover_ticks,
            &mut paused_ticks,
            recover_ticks_needed,
        );
        // Still one tick short of recovery — must remain paused.
        assert!(
            paused.load(Ordering::Relaxed),
            "ingestion_paused should still be true after only 1 recovery tick (need {recover_ticks_needed})"
        );
        assert_eq!(
            recover_ticks, 1,
            "recover_ticks should have incremented to 1"
        );
    }

    #[test]
    fn test_ingestion_pause_cleared_after_recover_hold() {
        // Simulate: flag set, free space above RECOVER_THRESHOLD_BYTES.
        // Drive the function for `recover_ticks_needed` ticks — flag must clear.
        let paused = Arc::new(AtomicBool::new(true));
        let available = RECOVER_THRESHOLD_BYTES + 1;
        let recover_ticks_needed = 2u64;
        let mut recover_ticks = 0u64;
        let mut paused_ticks = 5u64;

        // Tick 1 — not yet recovered.
        evaluate_ingestion_pause(
            available,
            &paused,
            &mut recover_ticks,
            &mut paused_ticks,
            recover_ticks_needed,
        );
        assert!(
            paused.load(Ordering::Relaxed),
            "should still be paused after tick 1"
        );

        // Tick 2 — meet threshold → should clear.
        evaluate_ingestion_pause(
            available,
            &paused,
            &mut recover_ticks,
            &mut paused_ticks,
            recover_ticks_needed,
        );
        assert!(
            !paused.load(Ordering::Relaxed),
            "ingestion_paused should be cleared after {recover_ticks_needed} sustained recovery ticks"
        );
    }

    #[test]
    fn test_ingestion_pause_recover_counter_resets_on_dip() {
        // Simulate: mid-recovery, free space dips below RECOVER_THRESHOLD_BYTES
        // → recover_ticks must reset to 0 and flag must stay set.
        let paused = Arc::new(AtomicBool::new(true));
        let recover_ticks_needed = 3u64;
        let mut recover_ticks = 2u64; // already 2 ticks in
        let mut paused_ticks = 10u64;

        // Available is between PAUSE and RECOVER thresholds → recovery counter resets.
        let available = PAUSE_THRESHOLD_BYTES + 1; // above PAUSE but below RECOVER
        evaluate_ingestion_pause(
            available,
            &paused,
            &mut recover_ticks,
            &mut paused_ticks,
            recover_ticks_needed,
        );
        assert!(
            paused.load(Ordering::Relaxed),
            "should remain paused when space dips back below RECOVER_THRESHOLD_BYTES"
        );
        assert_eq!(
            recover_ticks, 0,
            "recover_ticks must reset to 0 when space dips below RECOVER_THRESHOLD_BYTES"
        );
    }

    #[test]
    fn test_ingestion_not_paused_above_threshold() {
        // Well above PAUSE_THRESHOLD_BYTES, flag starts false → must stay false.
        let paused = Arc::new(AtomicBool::new(false));
        let available = RECOVER_THRESHOLD_BYTES + 1024 * 1024 * 1024;
        let mut recover_ticks = 0u64;
        let mut paused_ticks = 0u64;
        evaluate_ingestion_pause(available, &paused, &mut recover_ticks, &mut paused_ticks, 1);
        assert!(
            !paused.load(Ordering::Relaxed),
            "ingestion_paused must remain false when disk space is healthy"
        );
    }

    #[tokio::test]
    async fn test_shutdown_timeout_completes() {
        // Verify that a fast shutdown completes within the timeout
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            // Simulate shutdown work that completes quickly
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        })
        .await;
        assert!(
            result.is_ok(),
            "fast shutdown should complete within timeout"
        );
    }

    #[tokio::test]
    async fn test_shutdown_timeout_expires() {
        // Verify that a slow shutdown is detected by the timeout
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), async {
            // Simulate shutdown work that takes too long
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        })
        .await;
        assert!(result.is_err(), "slow shutdown should trigger timeout");
    }

    #[test]
    fn test_metrics_integration() {
        let metrics = NodeMetrics::new();
        assert_eq!(metrics.disk_available_bytes.load(Ordering::Relaxed), 0);

        metrics.set_disk_available_bytes(42_000_000_000);
        assert_eq!(
            metrics.disk_available_bytes.load(Ordering::Relaxed),
            42_000_000_000
        );

        let output = metrics.to_prometheus();
        assert!(output.contains("dugite_disk_available_bytes 42000000000"));
        assert!(output.contains("# TYPE dugite_disk_available_bytes gauge"));
    }
}
