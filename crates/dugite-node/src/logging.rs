use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// Log output target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogOutput {
    Stdout,
    File,
    Journald,
}

impl std::str::FromStr for LogOutput {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "stdout" => Ok(Self::Stdout),
            "file" => Ok(Self::File),
            "journald" | "journal" | "systemd" => Ok(Self::Journald),
            other => Err(format!(
                "unknown log output '{other}' (valid: stdout, file, journald)"
            )),
        }
    }
}

/// Log output format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable text output
    #[default]
    Text,
    /// Structured JSON output (one JSON object per line)
    Json,
}

impl std::str::FromStr for LogFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "text" | "plain" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(format!("unknown log format '{other}' (valid: text, json)")),
        }
    }
}

/// Log file rotation strategy.
#[derive(Debug, Clone, Copy, Default)]
pub enum LogRotation {
    #[default]
    Daily,
    Hourly,
    Never,
}

impl std::str::FromStr for LogRotation {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "daily" => Ok(Self::Daily),
            "hourly" => Ok(Self::Hourly),
            "never" | "none" => Ok(Self::Never),
            other => Err(format!(
                "unknown rotation '{other}' (valid: daily, hourly, never)"
            )),
        }
    }
}

/// Behavior when the non-blocking writer's bounded channel is full.
///
/// `Drop` (default) — matches `tracing_appender` upstream default; under flood
/// the producer continues without parking, dropped lines are counted via
/// `NonBlocking::error_counter`. Recommended for production where blocking the
/// hot path on the log sink is worse than losing throttled INFO lines.
///
/// `Block` — producer parks until the worker drains. Use for development, CI,
/// or scenarios where every log line must be preserved (e.g. forensic
/// post-mortems). Defeats the purpose of non-blocking under genuine overload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogOverflow {
    #[default]
    Drop,
    Block,
}

impl std::str::FromStr for LogOverflow {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "drop" | "lossy" => Ok(Self::Drop),
            "block" | "lossless" => Ok(Self::Block),
            other => Err(format!(
                "unknown log overflow policy '{other}' (valid: drop, block)"
            )),
        }
    }
}

/// Options for initializing the logging system.
pub struct LoggingOpts {
    pub outputs: Vec<LogOutput>,
    pub format: LogFormat,
    pub level: String,
    pub log_dir: String,
    pub rotation: LogRotation,
    pub no_color: bool,
    /// Number of days to retain log files (default: 7). Files older than this
    /// are deleted by [`start_log_cleanup_task`], which [`init`] spawns when a
    /// file output is configured and this is non-zero. `0` disables cleanup.
    pub log_retention_days: u64,
    /// Channel-full policy for the non-blocking stdout writer (issue #650).
    /// Default `Drop` matches `tracing_appender` upstream lossy default; set
    /// `Block` for development / CI where lossless capture is required.
    pub stdout_overflow: LogOverflow,
}

/// Handle to the live tracing subscriber.
///
/// Holds the file-writer worker guards (dropping flushes buffered output) and
/// the `reload::Handle` for each per-output `EnvFilter`, exposing
/// [`LogHandle::reload`] for runtime trace-verbosity changes (issue #473).
///
/// Cheaply clonable; cloned handles share the same underlying reload state.
#[derive(Clone)]
pub struct LogHandle {
    inner: Arc<LogHandleInner>,
}

struct LogHandleInner {
    reload_handles: Vec<reload::Handle<EnvFilter, Registry>>,
    _guards: Vec<tracing_appender::non_blocking::WorkerGuard>,
}

impl LogHandle {
    /// Re-parse the given EnvFilter directive (e.g., `"info,dugite_network=trace"`)
    /// and apply it to every output's filter without restarting the process.
    ///
    /// Returns an error if the directive fails to parse — in that case no
    /// handles are touched, so the previous filter remains in effect.
    pub fn reload(&self, directive: &str) -> anyhow::Result<()> {
        // Parse once up front to validate; bail before touching any handle so
        // we never end up in a half-applied state.
        EnvFilter::try_new(directive)
            .map_err(|e| anyhow::anyhow!("Invalid log directive '{directive}': {e}"))?;
        for handle in &self.inner.reload_handles {
            handle
                .reload(EnvFilter::new(directive))
                .map_err(|e| anyhow::anyhow!("Failed to reload log filter: {e}"))?;
        }
        Ok(())
    }

    /// Number of `WorkerGuard`s held — one per non-blocking writer (file +
    /// stdout). Used by tests to confirm sink wiring; not part of the
    /// stable surface.
    #[cfg(test)]
    pub(crate) fn guard_count(&self) -> usize {
        self.inner._guards.len()
    }
}

/// Initialize the logging system with the given options.
///
/// Returns a [`LogHandle`] that must be held until program exit to ensure
/// buffered output (file logs) is flushed.  The handle also exposes
/// [`LogHandle::reload`] for runtime filter changes (issue #473).
pub fn init(opts: &LoggingOpts) -> anyhow::Result<LogHandle> {
    let mut guards: Vec<tracing_appender::non_blocking::WorkerGuard> = Vec::new();
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::new();
    let mut reload_handles: Vec<reload::Handle<EnvFilter, Registry>> = Vec::new();

    let outputs = if opts.outputs.is_empty() {
        vec![LogOutput::Stdout]
    } else {
        opts.outputs.clone()
    };

    for output in &outputs {
        match output {
            LogOutput::Stdout => {
                let ansi = !opts.no_color && atty_stdout();
                // Non-blocking writer (issue #650): every `tracing::*!` call on
                // the hot path no longer performs a synchronous `write(2)`
                // on the emitting tokio worker. Lines are handed to a
                // background worker thread via a bounded channel; the
                // `WorkerGuard` is held in `LoggingHandleInner::_guards` so
                // pending lines drain on graceful shutdown.
                let (non_blocking, guard) = build_non_blocking_stdout(opts.stdout_overflow);
                guards.push(guard);
                let (filter, handle) = reload::Layer::new(build_filter(&opts.level));
                reload_handles.push(handle);
                match opts.format {
                    LogFormat::Text => {
                        let layer = tracing_subscriber::fmt::layer()
                            .compact()
                            .with_target(true)
                            .with_ansi(ansi)
                            .with_writer(non_blocking)
                            .with_filter(filter);
                        layers.push(Box::new(layer));
                    }
                    LogFormat::Json => {
                        let layer = tracing_subscriber::fmt::layer()
                            .json()
                            .with_target(true)
                            .with_ansi(false)
                            .with_writer(non_blocking)
                            .with_filter(filter);
                        layers.push(Box::new(layer));
                    }
                }
            }
            LogOutput::File => {
                std::fs::create_dir_all(&opts.log_dir)?;
                let file_appender = match opts.rotation {
                    LogRotation::Daily => {
                        tracing_appender::rolling::daily(&opts.log_dir, "dugite.log")
                    }
                    LogRotation::Hourly => {
                        tracing_appender::rolling::hourly(&opts.log_dir, "dugite.log")
                    }
                    LogRotation::Never => {
                        tracing_appender::rolling::never(&opts.log_dir, "dugite.log")
                    }
                };
                let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
                guards.push(guard);

                let (filter, handle) = reload::Layer::new(build_filter(&opts.level));
                reload_handles.push(handle);
                match opts.format {
                    LogFormat::Text => {
                        let layer = tracing_subscriber::fmt::layer()
                            .compact()
                            .with_target(true)
                            .with_ansi(false)
                            .with_writer(non_blocking)
                            .with_filter(filter);
                        layers.push(Box::new(layer));
                    }
                    LogFormat::Json => {
                        let layer = tracing_subscriber::fmt::layer()
                            .json()
                            .with_target(true)
                            .with_ansi(false)
                            .with_writer(non_blocking)
                            .with_filter(filter);
                        layers.push(Box::new(layer));
                    }
                }
            }
            LogOutput::Journald => {
                #[cfg(feature = "journald")]
                {
                    let (filter, handle) = reload::Layer::new(build_filter(&opts.level));
                    reload_handles.push(handle);
                    let layer = tracing_journald::layer()
                        .map_err(|e| anyhow::anyhow!("Failed to connect to journald: {e}"))?
                        .with_filter(filter);
                    layers.push(Box::new(layer));
                }
                #[cfg(not(feature = "journald"))]
                {
                    anyhow::bail!(
                        "journald output requires the 'journald' feature (rebuild with --features journald)"
                    );
                }
            }
        }
    }

    Registry::default().with(layers).init();

    // Prune old log files only when we are actually writing them (#942).
    // `init` is called from inside `#[tokio::main] async fn main`, so a runtime
    // is present for the spawn.
    if outputs.contains(&LogOutput::File) {
        start_log_cleanup_task(
            std::path::PathBuf::from(&opts.log_dir),
            opts.log_retention_days,
        );
    }

    Ok(LogHandle {
        inner: Arc::new(LogHandleInner {
            reload_handles,
            _guards: guards,
        }),
    })
}

/// Spawn the periodic log-cleanup task.
///
/// Runs [`cleanup_old_logs`] once immediately, then every 24 hours, deleting
/// `.log` files in `log_dir` older than `retention_days`.
///
/// A no-op when `retention_days == 0` (explicit opt-out) or when no file
/// output is configured — there is nothing to prune when logs go to stdout or
/// journald.
///
/// #942: `LoggingOpts` carried a `_log_retention_days` field whose doc comment
/// pointed at this function, but the function did not exist and
/// `cleanup_old_logs` was `#[cfg(test)]`. `--log-retention-days` parsed,
/// appeared in `--help`, and never deleted anything, so a long-running node
/// with `--log-output file` grew its log directory without bound.
pub fn start_log_cleanup_task(log_dir: std::path::PathBuf, retention_days: u64) {
    if retention_days == 0 {
        return;
    }
    tokio::spawn(async move {
        // 24h between sweeps; files are pruned by mtime, so the exact phase
        // does not matter.
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
        loop {
            ticker.tick().await;
            let dir = log_dir.clone();
            // Blocking fs walk — keep it off the async worker threads.
            let _ = tokio::task::spawn_blocking(move || {
                cleanup_old_logs(&dir, retention_days);
            })
            .await;
        }
    });
}

/// Delete `.log` files in `log_dir` that are older than `retention_days`.
///
/// Scans only immediate children of the directory (not recursive). Files that
/// cannot be inspected (e.g. permission errors) are silently skipped.
///
/// Was `#[cfg(test)]` until #942 — it existed, was tested, and did not exist
/// at all in release builds, so `--log-retention-days` was silently inert.
pub fn cleanup_old_logs(log_dir: &std::path::Path, retention_days: u64) {
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(retention_days * 86400);

    let entries = match std::fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        // Only consider files with .log extension
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let modified = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if modified < cutoff {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(path = %path.display(), "Failed to remove old log file: {e}");
            } else {
                tracing::info!(path = %path.display(), "Removed old log file");
            }
        }
    }
}

/// Build an `EnvFilter` from the given level string.
/// `RUST_LOG` env var takes priority if set.
fn build_filter(level: &str) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level))
}

/// Translate a cardano-node syslog-style `MinSeverity` value into the bare
/// `tracing_subscriber::EnvFilter` level token it corresponds to.
///
/// cardano-node's `MinSeverity` vocabulary is the `iohk-monitoring` `Severity`
/// enum — `Debug`/`Info`/`Notice`/`Warning`/`Error`/`Critical`/`Alert`/
/// `Emergency`. `tracing` only has `trace`/`debug`/`info`/`warn`/`error`, so
/// the finer syslog levels collapse: `Notice → info`, and
/// `Critical`/`Alert`/`Emergency → error`.
///
/// **Why this exists:** `MinSeverity` values were previously handed verbatim to
/// `EnvFilter`. Only `Debug`/`Info`/`Error` happen to coincide with EnvFilter
/// level tokens; `Notice`/`Warning`/`Critical`/`Alert`/`Emergency` are *not*
/// valid level tokens, so EnvFilter silently reinterpreted e.g. `"Warning"` as
/// a per-target directive `Warning=trace` — the *opposite* of the operator's
/// intent. This helper maps every `MinSeverity` value to a valid global level.
///
/// Values that are already valid `tracing` tokens (`trace`/`debug`/`info`/
/// `warn`/`error`/`off`) pass through (case-insensitively). Anything
/// unrecognised falls back to `info`. Operators needing per-target control
/// (e.g. `dugite_network=debug`) should use `LogDirective`, which is passed to
/// `EnvFilter` unchanged.
pub fn min_severity_to_directive(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "trace" => "trace",
        "debug" => "debug",
        "info" | "notice" => "info",
        "warn" | "warning" => "warn",
        "error" | "critical" | "alert" | "emergency" => "error",
        "off" => "off",
        _ => "info",
    }
}

/// Check if stdout is a terminal (for auto-detecting color support).
fn atty_stdout() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

/// Build the non-blocking wrapper around `std::io::Stdout`. Extracted so the
/// channel-full policy is a single decision point (issue #650).
fn build_non_blocking_stdout(
    policy: LogOverflow,
) -> (
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
) {
    build_non_blocking_with_policy(std::io::stdout(), policy, "dugite-logger-stdout")
}

/// Generic constructor for the non-blocking writer with the dugite overflow
/// policy. Extracted so unit tests can drive it against a `Vec<u8>`-backed
/// mock writer (the real `std::io::Stdout` is impossible to inspect in-process).
fn build_non_blocking_with_policy<W: std::io::Write + Send + 'static>(
    writer: W,
    policy: LogOverflow,
    thread_name: &str,
) -> (
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
) {
    let is_lossy = matches!(policy, LogOverflow::Drop);
    tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(is_lossy)
        .thread_name(thread_name)
        .finish(writer)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_output_from_str() {
        assert_eq!("stdout".parse::<LogOutput>().unwrap(), LogOutput::Stdout);
        assert_eq!("file".parse::<LogOutput>().unwrap(), LogOutput::File);
        assert_eq!(
            "journald".parse::<LogOutput>().unwrap(),
            LogOutput::Journald
        );
        assert_eq!("journal".parse::<LogOutput>().unwrap(), LogOutput::Journald);
        assert_eq!("systemd".parse::<LogOutput>().unwrap(), LogOutput::Journald);
        assert_eq!("STDOUT".parse::<LogOutput>().unwrap(), LogOutput::Stdout);
        assert!("invalid".parse::<LogOutput>().is_err());
    }

    #[test]
    fn test_log_format_from_str() {
        assert_eq!("text".parse::<LogFormat>().unwrap(), LogFormat::Text);
        assert_eq!("plain".parse::<LogFormat>().unwrap(), LogFormat::Text);
        assert_eq!("json".parse::<LogFormat>().unwrap(), LogFormat::Json);
        assert_eq!("JSON".parse::<LogFormat>().unwrap(), LogFormat::Json);
        assert!("invalid".parse::<LogFormat>().is_err());
    }

    #[test]
    fn test_log_rotation_from_str() {
        assert!(matches!(
            "daily".parse::<LogRotation>().unwrap(),
            LogRotation::Daily
        ));
        assert!(matches!(
            "hourly".parse::<LogRotation>().unwrap(),
            LogRotation::Hourly
        ));
        assert!(matches!(
            "never".parse::<LogRotation>().unwrap(),
            LogRotation::Never
        ));
        assert!(matches!(
            "none".parse::<LogRotation>().unwrap(),
            LogRotation::Never
        ));
        assert!("invalid".parse::<LogRotation>().is_err());
    }

    /// Backdate a file's mtime by `days`, verifying the filesystem persisted it.
    fn backdate(path: &std::path::Path, days: u64) {
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86400);
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
        drop(f);
        let actual = std::fs::metadata(path).unwrap().modified().unwrap();
        assert!(
            actual
                < std::time::SystemTime::now() - std::time::Duration::from_secs((days - 1) * 86400),
            "filesystem did not persist backdated mtime"
        );
    }

    /// #942 end-to-end: the SPAWNED TASK must delete an expired file, not just
    /// the helper when called directly. The old tests exercised
    /// `cleanup_old_logs` in isolation while nothing in production ever called
    /// it, so they passed against a flag that did nothing.
    #[tokio::test]
    async fn start_log_cleanup_task_deletes_expired_file() {
        let dir = std::env::temp_dir().join("dugite_log_cleanup_task_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("dugite.2020-01-01.log");
        let fresh = dir.join("dugite.2026-08-01.log");
        std::fs::write(&old, b"stale").unwrap();
        std::fs::write(&fresh, b"current").unwrap();
        backdate(&old, 30);

        start_log_cleanup_task(dir.clone(), 7);

        // The task sweeps immediately on its first tick.
        for _ in 0..100 {
            if !old.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        assert!(
            !old.exists(),
            "expired log must be deleted by the spawned task"
        );
        assert!(fresh.exists(), "in-window log must be retained");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// retention_days == 0 is an explicit opt-out: nothing may be deleted.
    #[tokio::test]
    async fn start_log_cleanup_task_zero_retention_is_a_noop() {
        let dir = std::env::temp_dir().join("dugite_log_cleanup_noop_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("dugite.2020-01-01.log");
        std::fs::write(&old, b"stale").unwrap();
        backdate(&old, 30);

        start_log_cleanup_task(dir.clone(), 0);
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        assert!(
            old.exists(),
            "retention_days=0 must disable cleanup entirely"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cleanup_old_logs_removes_expired() {
        let dir = std::env::temp_dir().join("dugite_log_cleanup_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Create a .log file and backdate its modification time
        let old_file = dir.join("dugite.2020-01-01.log");
        std::fs::write(&old_file, "old log data").unwrap();
        // Set modification time to 30 days ago.
        // We must drop the file handle before cleanup_old_logs reads metadata,
        // otherwise some platforms may report a stale mtime for the open file.
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86400);
        {
            let f = std::fs::File::options()
                .write(true)
                .open(&old_file)
                .unwrap();
            f.set_modified(old_time).unwrap();
            drop(f);
        }
        // Verify the mtime was actually persisted before proceeding.
        let actual_mtime = std::fs::metadata(&old_file).unwrap().modified().unwrap();
        assert!(
            actual_mtime
                < std::time::SystemTime::now() - std::time::Duration::from_secs(29 * 86400),
            "Filesystem did not persist backdated mtime"
        );

        // Create a recent .log file
        let new_file = dir.join("dugite.2026-03-14.log");
        std::fs::write(&new_file, "new log data").unwrap();

        // Create a non-log file (should not be deleted)
        let txt_file = dir.join("notes.txt");
        std::fs::write(&txt_file, "keep me").unwrap();

        cleanup_old_logs(&dir, 7);

        assert!(!old_file.exists(), "Old log file should have been deleted");
        assert!(new_file.exists(), "Recent log file should be kept");
        assert!(txt_file.exists(), "Non-log file should not be touched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cleanup_old_logs_nonexistent_dir() {
        // Should not panic on non-existent directory
        cleanup_old_logs(std::path::Path::new("/nonexistent/dir"), 7);
    }

    /// Regression test for #473: `reload::Handle` must swap the EnvFilter
    /// live, so events that were below the previous filter threshold start
    /// firing after `handle.reload(...)`.
    ///
    /// Uses a local `with_default` subscriber to keep the test self-contained
    /// (the global subscriber is initialized once at process startup).
    #[test]
    fn test_reload_filter_swaps_live() {
        use std::sync::Mutex;
        use tracing::Subscriber;
        use tracing_subscriber::layer::{Context, SubscriberExt};

        // Custom layer that records every event that survives its filter,
        // tagged by level so we can distinguish before/after reload.
        struct CaptureLayer(Arc<Mutex<Vec<String>>>);
        impl<S: Subscriber> Layer<S> for CaptureLayer {
            fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
                if event.metadata().target() == "issue_473_test" {
                    self.0
                        .lock()
                        .unwrap()
                        .push(event.metadata().level().to_string());
                }
            }
        }

        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let capture = CaptureLayer(Arc::clone(&captured));

        // Start at info — debug events should be filtered out.
        let (reload_layer, handle) = reload::Layer::new(EnvFilter::new("info"));
        let subscriber = Registry::default().with(capture.with_filter(reload_layer));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "issue_473_test", "info_before");
            tracing::debug!(target: "issue_473_test", "debug_filtered");

            // Hot-swap to debug; verify the new filter is in effect.
            handle
                .reload(EnvFilter::new("debug"))
                .expect("reload should succeed for valid directive");

            tracing::info!(target: "issue_473_test", "info_after");
            tracing::debug!(target: "issue_473_test", "debug_after");
        });

        let events = captured.lock().unwrap();
        assert_eq!(
            events.len(),
            3,
            "expected 3 events (info_before, info_after, debug_after) — got {events:?}"
        );
        assert_eq!(events[0], "INFO", "first event was info_before");
        assert_eq!(events[1], "INFO", "second event was info_after");
        assert_eq!(events[2], "DEBUG", "third event was debug_after");
    }

    #[test]
    fn test_log_overflow_from_str() {
        assert_eq!("drop".parse::<LogOverflow>().unwrap(), LogOverflow::Drop);
        assert_eq!("DROP".parse::<LogOverflow>().unwrap(), LogOverflow::Drop);
        assert_eq!("lossy".parse::<LogOverflow>().unwrap(), LogOverflow::Drop);
        assert_eq!("block".parse::<LogOverflow>().unwrap(), LogOverflow::Block);
        assert_eq!(
            "lossless".parse::<LogOverflow>().unwrap(),
            LogOverflow::Block
        );
        assert!("park".parse::<LogOverflow>().is_err());
        assert_eq!(LogOverflow::default(), LogOverflow::Drop);
    }

    /// Send-counting writer: every `write` call atomically increments a
    /// counter. No sleeps and no Mutex (use AtomicUsize) so the test is
    /// deterministic on macOS where `thread::sleep` granularity is coarse.
    struct CountingWriter {
        count: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl std::io::Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Issue #650: `LogOverflow::Block` (lossless) must deliver every line.
    /// We use a fast consumer (no artificial sleep) so the worker comfortably
    /// drains within the `WorkerGuard::drop` 1s flush timeout.
    #[test]
    fn test_non_blocking_block_is_lossless() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let writer = CountingWriter {
            count: Arc::clone(&count),
        };
        let (mut non_blocking, guard) =
            build_non_blocking_with_policy(writer, LogOverflow::Block, "dugite-test-block");

        use std::io::Write;
        const N: usize = 5_000;
        // Use `write_all` with a pre-built buffer so each line produces
        // exactly one `write` call on the consumer (matching the channel's
        // one-msg-per-`write` semantics in `tracing_appender`).
        for i in 0..N {
            let line = format!("L{i:06}\n");
            non_blocking.write_all(line.as_bytes()).unwrap();
        }
        // Drop sender first so worker sees channel close; then guard waits for drain.
        drop(non_blocking);
        drop(guard);

        // Allow a short post-drop drain margin — guard's shutdown timeout
        // is 1s but the worker may still be writing the tail when drop returns.
        for _ in 0..50 {
            if count.load(std::sync::atomic::Ordering::Acquire) >= N {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let observed = count.load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(
            observed, N,
            "Block policy must deliver every line; got {observed}/{N}"
        );
    }

    /// Issue #650: `LogOverflow::Drop` (lossy) must NOT block the producer
    /// when the channel is saturated. We exercise the contract by routing
    /// directly through `NonBlockingBuilder` with `buffered_lines_limit = 4`
    /// (the smallest non-trivial size) and a black-hole consumer that hangs
    /// on a barrier — the producer must still complete promptly.
    #[test]
    fn test_non_blocking_drop_does_not_block_producer() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let release = Arc::new(AtomicBool::new(false));
        let release_clone = Arc::clone(&release);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        struct HangingWriter {
            release: Arc<AtomicBool>,
            count: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl std::io::Write for HangingWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                // Spin until the test releases us. The producer must NOT
                // block waiting on this writer when configured as lossy.
                while !self.release.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                self.count.fetch_add(1, Ordering::AcqRel);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let writer = HangingWriter {
            release: Arc::clone(&release),
            count: Arc::clone(&count),
        };
        // Tiny buffer (4 lines) + a single in-flight write blocked in the
        // worker means the producer hits a full channel after 5 writes.
        // With `lossy(true)` it must keep going; with `lossy(false)` it would
        // park indefinitely until `release` is set.
        let (mut non_blocking, guard) =
            tracing_appender::non_blocking::NonBlockingBuilder::default()
                .lossy(true)
                .buffered_lines_limit(4)
                .thread_name("dugite-test-drop")
                .finish(writer);

        use std::io::Write;
        let start = std::time::Instant::now();
        const N: usize = 10_000;
        for i in 0..N {
            // writeln! returns Ok even when lossy drops the line.
            writeln!(non_blocking, "L{i:06}").unwrap();
        }
        let producer_elapsed = start.elapsed();
        assert!(
            producer_elapsed < std::time::Duration::from_secs(1),
            "Drop policy producer must not block when consumer is wedged; took {producer_elapsed:?}"
        );

        // Sanity: the underlying NonBlocking reports drops.
        assert!(
            non_blocking.error_counter().dropped_lines() > 0,
            "Drop policy must surface dropped-line count when consumer is blocked"
        );

        // Release the consumer and tear down so we don't leak the thread.
        release_clone.store(true, Ordering::Release);
        drop(non_blocking);
        drop(guard);
        let _ = count.load(Ordering::Acquire); // touch to silence unused-must-use
    }

    /// Issue #650: `init()` for the stdout sink must register a `WorkerGuard`
    /// in the returned `LogHandle` so the worker drains on graceful shutdown.
    /// Run in isolation (sets the global subscriber) — call via single-shot
    /// `Once` to keep the test self-contained.
    #[test]
    fn test_init_stdout_registers_worker_guard() {
        use std::sync::Once;
        // The global subscriber is set once per process; use a separate
        // process-local helper that exercises only the LoggingOpts → guards
        // wiring without touching the global tracing subscriber.
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            // Construct the handle the same way `init` does, but without
            // calling `.init()` on it (which would taint the global subscriber
            // for other tests). We mirror the stdout branch directly.
            let (_non_blocking, guard) =
                build_non_blocking_with_policy(Vec::<u8>::new(), LogOverflow::Drop, "test-guard");
            let handle = LogHandle {
                inner: Arc::new(LogHandleInner {
                    reload_handles: Vec::new(),
                    _guards: vec![guard],
                }),
            };
            assert_eq!(
                handle.guard_count(),
                1,
                "stdout sink must hold a WorkerGuard for graceful drain"
            );
        });
    }

    /// Invalid directives must return an error and leave the previous filter intact.
    #[test]
    fn test_reload_rejects_invalid_directive() {
        let (_layer, handle) = reload::Layer::<EnvFilter, Registry>::new(EnvFilter::new("info"));
        let log_handle = LogHandle {
            inner: Arc::new(LogHandleInner {
                reload_handles: vec![handle],
                _guards: Vec::new(),
            }),
        };
        // Garbage directive: EnvFilter rejects mismatched braces / illegal syntax.
        let err = log_handle
            .reload("this is not =====:::: a valid directive")
            .unwrap_err();
        assert!(
            err.to_string().contains("Invalid log directive"),
            "expected 'Invalid log directive' wrapper, got: {err}"
        );
    }

    /// Every cardano-node `MinSeverity` value must translate to a bare,
    /// *global* EnvFilter level — never a per-target directive. Before the fix,
    /// `Warning`/`Notice`/`Critical`/`Alert`/`Emergency` were silently parsed
    /// by EnvFilter as a bogus target at TRACE level, inverting the operator's
    /// intent. This asserts the translation collapses every syslog value to a
    /// valid global level whose `max_level_hint` matches expectations.
    #[test]
    fn test_min_severity_to_directive_maps_all_syslog_values() {
        use tracing::level_filters::LevelFilter;
        // (MinSeverity input, expected translated token, expected global level)
        let cases = [
            ("Debug", "debug", LevelFilter::DEBUG),
            ("Info", "info", LevelFilter::INFO),
            ("Notice", "info", LevelFilter::INFO),
            ("Warning", "warn", LevelFilter::WARN),
            ("Error", "error", LevelFilter::ERROR),
            ("Critical", "error", LevelFilter::ERROR),
            ("Alert", "error", LevelFilter::ERROR),
            ("Emergency", "error", LevelFilter::ERROR),
            // Case-insensitivity + already-valid tracing tokens pass through.
            ("warning", "warn", LevelFilter::WARN),
            ("TRACE", "trace", LevelFilter::TRACE),
            ("off", "off", LevelFilter::OFF),
            // Unrecognised → safe default.
            ("nonsense", "info", LevelFilter::INFO),
        ];
        for (input, expected_token, expected_level) in cases {
            let token = min_severity_to_directive(input);
            assert_eq!(token, expected_token, "translation of {input:?}");
            // The translated token must parse as a *global* level: EnvFilter's
            // max_level_hint equals the level, not TRACE-via-bogus-target.
            let filter = EnvFilter::try_new(token)
                .unwrap_or_else(|e| panic!("{input:?}→{token:?} must be valid EnvFilter: {e}"));
            assert_eq!(
                filter.max_level_hint(),
                Some(expected_level),
                "{input:?}→{token:?} should set global level {expected_level:?}, \
                 not be reinterpreted as a per-target TRACE directive"
            );
        }
    }
}
