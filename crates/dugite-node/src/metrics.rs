use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tracing::{error, info};

/// Duration in seconds after which the node is considered "stalled" if no blocks received
/// and sync progress is below 99%.
const STALLED_THRESHOLD_SECS: u64 = 300; // 5 minutes

/// Tracks process CPU utilization between samples.
///
/// Computes CPU percentage by comparing cumulative process CPU time (user +
/// kernel) across two wall-clock samples.  The percentage is relative to one
/// logical CPU core, so values > 100 are possible on multi-threaded workloads.
///
/// Platform notes:
/// - Linux: reads `/proc/self/stat` fields 14 (utime) and 15 (stime) in clock
///   ticks, then divides by the tick frequency obtained from `libc::sysconf`.
/// - macOS: shell out to `ps -o pcpu= -p <pid>` which reports instantaneous
///   CPU% directly — cheap enough for a 5-second polling window.
/// - Other platforms: returns 0.0 (no-op, no external dependencies needed).
struct CpuTracker {
    /// Wall-clock time of the previous sample.
    last_wall: std::time::Instant,
    /// Cumulative CPU ticks (utime + stime) at the previous sample.
    /// Stored as 0 on non-Linux platforms (unused).
    last_cpu_ticks: u64,
    /// CPU percentage from the most recent interval (updated on each `sample()`).
    last_pct: f64,
    /// Cumulative CPU seconds (utime + stime / CLK_TCK) updated on each sample.
    /// Used to expose `dugite_cpu_seconds_total`.
    cumulative_cpu_secs: f64,
}

impl CpuTracker {
    fn new() -> Self {
        Self {
            last_wall: std::time::Instant::now(),
            last_cpu_ticks: read_cpu_ticks_linux(),
            last_pct: 0.0,
            cumulative_cpu_secs: 0.0,
        }
    }

    /// Sample current CPU usage.
    ///
    /// Returns the CPU percentage consumed since the previous call (0.0–100.0+
    /// per core).  Also updates `self.cumulative_cpu_secs`.
    fn sample(&mut self) -> f64 {
        let pct = sample_cpu_pct_impl(
            &mut self.last_wall,
            &mut self.last_cpu_ticks,
            &mut self.cumulative_cpu_secs,
        );
        self.last_pct = pct;
        pct
    }
}

// ---------------------------------------------------------------------------
// Linux implementation — /proc/self/stat
// ---------------------------------------------------------------------------

/// Read the sum of `utime + stime` (in clock ticks) from `/proc/self/stat`.
/// Returns 0 on non-Linux platforms or if the file cannot be parsed.
#[cfg(target_os = "linux")]
fn read_cpu_ticks_linux() -> u64 {
    // /proc/self/stat is a single space-separated line; fields are 1-indexed
    // in the proc(5) man page.  Fields 14 (utime) and 15 (stime) are the
    // user-mode and kernel-mode CPU times in clock ticks.
    //
    // Field 2 (comm) can contain spaces inside parentheses, so we locate the
    // closing ')' and split from there to get the remaining positional fields.
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            // Skip past the closing ')' of the comm field.
            let after_comm = s.find(')')? + 1;
            let rest = s[after_comm..].trim_start();
            // Remaining fields are whitespace-separated; 0-indexed from here:
            //   0 = state (field 3)
            //   ...
            //  11 = utime (field 14)
            //  12 = stime (field 15)
            let fields: Vec<&str> = rest.split_whitespace().collect();
            let utime: u64 = fields.get(11)?.parse().ok()?;
            let stime: u64 = fields.get(12)?.parse().ok()?;
            Some(utime + stime)
        })
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn read_cpu_ticks_linux() -> u64 {
    0
}

/// Return the number of clock ticks per second (`_SC_CLK_TCK`).
///
/// 100 is the correct value on virtually all Linux systems but we read the
/// actual kernel-reported value to be accurate.  The result is cached after
/// the first call via a `std::sync::OnceLock`.
#[cfg(target_os = "linux")]
fn clk_tck() -> u64 {
    static CLK_TCK: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *CLK_TCK.get_or_init(|| {
        // SAFETY: sysconf is always safe to call with _SC_CLK_TCK.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks > 0 {
            ticks as u64
        } else {
            100
        }
    })
}

// ---------------------------------------------------------------------------
// Platform-dispatched sampling
// ---------------------------------------------------------------------------

/// Compute the CPU percentage consumed since the last call and update
/// the cumulative CPU seconds counter.
///
/// On Linux: delta_ticks / clk_tck / elapsed_wall_secs * 100.
/// On macOS: one `ps -o pcpu=` shell-out per call.
/// Elsewhere: 0.0 always.
#[cfg(target_os = "linux")]
fn sample_cpu_pct_impl(
    last_wall: &mut std::time::Instant,
    last_ticks: &mut u64,
    cumulative_secs: &mut f64,
) -> f64 {
    let now_wall = std::time::Instant::now();
    let elapsed_wall = now_wall.duration_since(*last_wall).as_secs_f64();

    let current_ticks = read_cpu_ticks_linux();
    let tck = clk_tck();

    // Guard against clock going backwards or zero elapsed time.
    if elapsed_wall < 0.001 || tck == 0 {
        return 0.0;
    }

    let delta_ticks = current_ticks.saturating_sub(*last_ticks);
    let delta_cpu_secs = delta_ticks as f64 / tck as f64;

    *cumulative_secs += delta_cpu_secs;
    *last_wall = now_wall;
    *last_ticks = current_ticks;

    // Clamp to a sane ceiling (400% = 4 fully-loaded cores).
    (delta_cpu_secs / elapsed_wall * 100.0).clamp(0.0, 400.0)
}

#[cfg(target_os = "macos")]
fn sample_cpu_pct_impl(
    last_wall: &mut std::time::Instant,
    _last_ticks: &mut u64,
    cumulative_secs: &mut f64,
) -> f64 {
    // `ps -o pcpu=` emits the CPU% since process start (not since last call),
    // which is what we want for the gauge display.  The shell-out takes ~5 ms
    // on macOS; acceptable for a monitoring interval of >= 2 seconds.
    let pct = std::process::Command::new("ps")
        .args(["-o", "pcpu=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0);

    // Update cumulative seconds from the elapsed wall time and the current %.
    let now_wall = std::time::Instant::now();
    let elapsed_wall = now_wall.duration_since(*last_wall).as_secs_f64();
    *last_wall = now_wall;
    // Approximate: pct is since-start average, so delta = pct/100 * elapsed_wall.
    *cumulative_secs += (pct / 100.0) * elapsed_wall;

    pct.clamp(0.0, 400.0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn sample_cpu_pct_impl(
    _last_wall: &mut std::time::Instant,
    _last_ticks: &mut u64,
    _cumulative_secs: &mut f64,
) -> f64 {
    0.0
}

/// Sync progress threshold (as percentage * 100) at or above which the node is "healthy".
const SYNCED_THRESHOLD: u64 = 9990; // 99.9% (stored as pct * 100)

/// Fixed histogram bucket boundaries (in milliseconds) for latency tracking.
const LATENCY_BUCKETS_MS: &[f64] = &[
    1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0,
];

/// Prometheus-style histogram with fixed buckets.
#[derive(Debug)]
pub struct Histogram {
    /// Count of observations in each bucket (cumulative upper bound).
    buckets: Vec<AtomicU64>,
    /// Total count of observations.
    count: AtomicU64,
    /// Sum of all observed values (stored as f64 bits for atomicity).
    sum_bits: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Histogram {
            buckets: (0..LATENCY_BUCKETS_MS.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            count: AtomicU64::new(0),
            sum_bits: AtomicU64::new(f64::to_bits(0.0)),
        }
    }

    /// Record an observation (value in milliseconds).
    /// Increments the first bucket whose upper bound >= value_ms.
    pub fn observe(&self, value_ms: f64) {
        for (i, &bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
            if value_ms <= bound {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        // Approximate sum update — relaxed ordering is fine for metrics
        loop {
            let old_bits = self.sum_bits.load(Ordering::Relaxed);
            let old_sum = f64::from_bits(old_bits);
            let new_sum = old_sum + value_ms;
            if self
                .sum_bits
                .compare_exchange_weak(
                    old_bits,
                    f64::to_bits(new_sum),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }
    }

    /// Format as Prometheus histogram lines.
    fn to_prometheus(&self, name: &str, help: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} histogram\n"));
        let mut cumulative = 0u64;
        for (i, &bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!("{name}_bucket{{le=\"{bound}\"}} {cumulative}\n"));
        }
        let total = self.count.load(Ordering::Relaxed);
        out.push_str(&format!("{name}_bucket{{le=\"+Inf\"}} {total}\n"));
        let sum = f64::from_bits(self.sum_bits.load(Ordering::Relaxed));
        out.push_str(&format!("{name}_sum {sum}\n"));
        out.push_str(&format!("{name}_count {total}\n"));
        out
    }
}

/// Get the current resident set size (RSS) of this process in bytes.
fn get_resident_memory_bytes() -> u64 {
    get_resident_memory_bytes_impl()
}

#[cfg(target_os = "linux")]
fn get_resident_memory_bytes_impl() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn get_resident_memory_bytes_impl() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn get_resident_memory_bytes_impl() -> u64 {
    0
}

/// Get total system physical memory in bytes.
///
/// Used to compute the process RSS fraction for the TUI memory bar.
/// Returns 0 on unsupported platforms (bar will be hidden).
fn get_total_memory_bytes() -> u64 {
    get_total_memory_bytes_impl()
}

#[cfg(target_os = "linux")]
fn get_total_memory_bytes_impl() -> u64 {
    // /proc/meminfo line: "MemTotal:       16384000 kB"
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn get_total_memory_bytes_impl() -> u64 {
    // sysctl hw.memsize returns total physical RAM as a string integer.
    std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn get_total_memory_bytes_impl() -> u64 {
    0
}

/// Compute sync progress as a percentage in `[0, 100]`.
///
/// `applied_slot` is our local tip's slot number.  `peer_tip_slot` is the
/// maximum tip slot reported by any peer (the network tip, as best we know
/// it).  When `applied_slot >= peer_tip_slot` we are at or ahead of the
/// known tip → 100%.  When `peer_tip_slot == 0` no peer has reported a
/// tip yet — return 0% to make health checks correctly report "not
/// synced" until a real measurement is available; this also avoids the
/// pre-fix bug where `set_sync_progress(100.0)` was called unconditionally
/// in block-apply paths, making dugite-monitor display 100% while the node
/// was at <20% of the chain.
pub fn compute_sync_progress(applied_slot: u64, peer_tip_slot: u64) -> f64 {
    if peer_tip_slot == 0 {
        return 0.0;
    }
    if applied_slot >= peer_tip_slot {
        return 100.0;
    }
    (applied_slot as f64 / peer_tip_slot as f64) * 100.0
}

/// Node metrics for monitoring
pub struct NodeMetrics {
    pub blocks_received: AtomicU64,
    pub blocks_applied: AtomicU64,
    pub transactions_received: AtomicU64,
    pub transactions_validated: AtomicU64,
    pub transactions_rejected: AtomicU64,
    pub peers_connected: AtomicU64,
    pub peers_outbound: AtomicU64,
    pub peers_inbound: AtomicU64,
    pub peers_duplex: AtomicU64,
    pub peers_cold: AtomicU64,
    pub peers_warm: AtomicU64,
    pub peers_hot: AtomicU64,
    // Connection manager counters (Haskell ConnectionManagerCounters compat)
    pub conn_full_duplex: AtomicU64,
    pub conn_duplex: AtomicU64,
    pub conn_unidirectional: AtomicU64,
    pub conn_inbound: AtomicU64,
    pub conn_outbound: AtomicU64,
    pub conn_terminating: AtomicU64,
    pub sync_progress_pct: AtomicU64,
    /// Maximum tip slot reported by any peer via ChainSync (`MsgRollForward`
    /// / `MsgRollBackward` tip field).  Acts as the denominator for sync
    /// progress so we don't have to read `candidate_chains` from every
    /// block-apply site.  Monotonic via `fetch_max`.
    pub max_peer_tip_slot: AtomicU64,
    pub slot_number: AtomicU64,
    pub block_number: AtomicU64,
    pub epoch_number: AtomicU64,
    pub utxo_count: AtomicU64,
    pub mempool_tx_count: AtomicU64,
    pub mempool_tx_max: AtomicU64,
    pub mempool_bytes: AtomicU64,
    pub rollback_count: AtomicU64,
    pub blocks_forged: AtomicU64,
    pub delegation_count: AtomicU64,
    pub treasury_lovelace: AtomicU64,
    pub reserves_lovelace: AtomicU64,
    /// Total currently-registered DReps (matches `cardano-cli conway query drep-state`
    /// length, i.e. the size of the `vsDReps` map).  Includes DReps whose activity
    /// window has expired — they remain registered until explicitly deregistered.
    pub drep_count: AtomicU64,
    /// Subset of `drep_count` still within their activity window (`active` flag is
    /// true).  Only active DReps contribute voting power during ratification.
    pub drep_active: AtomicU64,
    /// Monotonic counter of all `RegDRep` certificates ever observed.
    pub drep_registrations_total: AtomicU64,
    /// Number of stake credentials currently delegated to a DRep (any variant,
    /// including `AlwaysAbstain` / `AlwaysNoConfidence`).
    pub vote_delegation_count: AtomicU64,
    /// Active governance proposals (size of `proposals` BTreeMap).
    pub proposal_count: AtomicU64,
    pub pool_count: AtomicU64,
    /// Committee hot-key authorizations (cold → hot).  A hot-authorized cold key
    /// is considered an "active" constitutional committee member in Haskell.
    pub committee_hot_count: AtomicU64,
    /// Total committee members with a known expiration epoch (includes both
    /// hot-authorized and not-yet-authorized cold keys).
    pub committee_total_count: AtomicU64,
    /// Committee members that have resigned (still in `committee_resigned` map
    /// until the next UpdateCommittee action).
    pub committee_resigned_count: AtomicU64,
    /// 1 when the committee is in a no-confidence state (dissolved by a
    /// ratified `NoConfidence` action), 0 otherwise.
    pub committee_no_confidence: AtomicU64,
    /// Committee quorum threshold as basis points (0–10000).  0 when unset.
    pub committee_threshold_bps: AtomicU64,
    /// Cumulative dormant-epoch counter since Conway genesis.  A dormant epoch
    /// is one where the `proposals` map was empty at the epoch boundary.
    /// Inflated values directly reduce every future `drep_expiry` computed at
    /// registration/vote time, so this is useful as a divergence signal.
    pub gov_dormant_epochs: AtomicU64,
    /// 1 when a constitution is set in the governance state, 0 otherwise.
    pub constitution_present: AtomicU64,
    // Conway governance protocol parameters (surfaced from ProtocolParameters).
    pub pparam_drep_deposit_lovelace: AtomicU64,
    pub pparam_drep_activity_epochs: AtomicU64,
    pub pparam_gov_action_deposit_lovelace: AtomicU64,
    pub pparam_gov_action_lifetime_epochs: AtomicU64,
    pub pparam_committee_min_size: AtomicU64,
    pub pparam_committee_max_term_length: AtomicU64,
    pub disk_total_bytes: AtomicU64,
    pub disk_used_bytes: AtomicU64,
    pub disk_available_bytes: AtomicU64,
    // Block production metrics
    pub leader_checks_total: AtomicU64,
    pub leader_checks_not_elected: AtomicU64,
    pub forge_failures: AtomicU64,
    pub blocks_announced: AtomicU64,
    /// Forged blocks that lost a race to incoming blocks and were NOT adopted
    /// as the selected-chain tip. These blocks are stored as fork blocks in
    /// VolatileDB but the ledger apply + announcement were skipped to keep
    /// the forge path correct. A persistently non-zero value here indicates
    /// forge scheduling lag or adversarial slot battles.
    pub forge_race_lost: AtomicU64,
    /// Forge broadcasts where the broadcast channel had zero subscribers
    /// at send time (no N2N peer connected). The forged block is stored
    /// locally but WILL be orphaned — every non-zero tick here is a
    /// propagation failure.
    pub forge_announce_no_subscribers: AtomicU64,
    // Protocol error metrics
    pub n2n_connections_total: AtomicU64,
    pub n2c_connections_total: AtomicU64,
    pub n2n_connections_active: AtomicU64,
    pub n2c_connections_active: AtomicU64,
    // N2C LocalTxSubmission counters (from dugite-cli submit-tx)
    pub n2c_txs_submitted: AtomicU64,
    pub n2c_txs_accepted: AtomicU64,
    pub n2c_txs_rejected: AtomicU64,
    /// Per-protocol-error-type counts (label → count).
    protocol_errors: std::sync::Mutex<HashMap<String, u64>>,
    /// Peer handshake RTT histogram (milliseconds) — cumulative, for Prometheus.
    pub peer_handshake_rtt_ms: Histogram,
    /// Block fetch latency histogram (milliseconds per block)
    pub peer_block_fetch_ms: Histogram,
    /// Current average RTT across connected peers (milliseconds, gauge).
    /// Updated on each KeepAlive pong from the PeerManager's EWMA values.
    pub peer_rtt_avg_ms: AtomicU64,
    /// Current minimum RTT across connected peers (milliseconds, gauge).
    pub peer_rtt_min_ms: AtomicU64,
    /// Current maximum RTT across connected peers (milliseconds, gauge).
    pub peer_rtt_max_ms: AtomicU64,
    /// Number of currently-connected peers with EWMA RTT in [0, 50) ms.
    /// Gauge — refreshed on every KeepAlive pong.
    pub peer_rtt_band_0_50: AtomicU64,
    /// Number of currently-connected peers with EWMA RTT in [50, 100) ms.
    pub peer_rtt_band_50_100: AtomicU64,
    /// Number of currently-connected peers with EWMA RTT in [100, 200) ms.
    pub peer_rtt_band_100_200: AtomicU64,
    /// Number of currently-connected peers with EWMA RTT >= 200 ms.
    pub peer_rtt_band_200_plus: AtomicU64,
    /// Total number of currently-connected peers contributing to RTT bands
    /// (i.e. warm/hot peers with at least one keepalive measurement).
    pub peer_rtt_samples: AtomicU64,
    /// Node uptime in seconds
    startup_instant: std::time::Instant,
    /// Per-validation-error-type rejection counts (label → count).
    validation_errors: std::sync::Mutex<HashMap<String, u64>>,
    /// Epoch milliseconds when the last block was received (0 = never)
    pub last_block_received_at: AtomicU64,
    /// Epoch millis of last RollForward event (for chainsync_idle calculation)
    pub last_roll_forward_at: AtomicU64,
    /// Duration of last ledger replay in seconds (stored as f64 bits)
    pub replay_duration_secs: AtomicU64,
    /// Tip age in seconds (wall_clock - slot_to_time(tip_slot))
    pub tip_age_secs: AtomicU64,
    /// POSIX time of the tip slot in milliseconds (for dynamic tip_age computation).
    pub tip_slot_time_ms: AtomicU64,
    /// Seconds since last RollForward event
    pub chainsync_idle_secs: AtomicU64,
    /// CPU tracker for process CPU utilization measurement.
    cpu_tracker: std::sync::Mutex<CpuTracker>,
    /// Peak resident memory observed since node start, in bytes.
    /// Exposed as `dugite_mem_peak_bytes` Prometheus gauge.
    peak_mem_bytes: AtomicU64,
    /// Network magic number (764824073=mainnet, 2=preview, 1=preprod).
    pub network_magic: AtomicU64,
    /// Slots per epoch from the active Shelley genesis. Exposed as
    /// `dugite_epoch_length` so downstream tools (dugite-monitor, dashboards)
    /// can compute epoch progress and ETA without hard-coding network defaults.
    pub epoch_length_slots: AtomicU64,
    /// Slot duration in milliseconds from the active Shelley genesis. Exposed
    /// as `dugite_slot_length_ms`. With this and `epoch_length_slots` clients
    /// can derive total epoch wall-clock time and remaining time precisely.
    pub slot_length_ms: AtomicU64,
    /// `activeSlotsCoeff` × 1000 from the active Shelley genesis (Praos f).
    /// Exposed as `dugite_active_slots_coeff_x1000` (rational scaled to an
    /// integer for the Prometheus encoder). 200 means f=0.20.
    pub active_slots_coeff_x1000: AtomicU64,
    /// Liveness threshold in seconds — `/live` returns 503 when no block has
    /// been applied within this window (and the node is not freshly started).
    /// Default 600s (10 minutes). 0 disables the threshold (always 200).
    pub liveness_threshold_secs: AtomicU64,
    /// 1 when running as a block producer (forge credentials loaded), 0 for relay.
    ///
    /// Exposed as `dugite_is_block_producer` gauge so the TUI can show the
    /// correct role label without inspecting CLI arguments directly.
    pub is_block_producer: AtomicU64,
    /// Hex-encoded pool ID (28-byte Blake2b-224 of the cold verification key).
    ///
    /// Empty string when running as a relay.  Emitted as a Prometheus info metric
    /// with a `pool_id` label so operators can identify the producing pool at a
    /// glance in the TUI without opening the logs.
    pool_id_hex: std::sync::Mutex<String>,
    /// When true, emit additional `cardano_node_metrics_*` aliases alongside the
    /// native `dugite_*` metrics.  Allows existing cardano-node Grafana dashboards
    /// to work without modification.  Controlled by `--compat-metrics` CLI flag.
    compat_metrics: std::sync::atomic::AtomicBool,
    /// Diffusion mode: 0 = InitiatorAndResponder, 1 = InitiatorOnly.
    /// Set once at startup from the `DiffusionMode` config field.
    pub diffusion_mode: AtomicU64,
    /// 1 when peer sharing mini-protocol is enabled, 0 when disabled.
    /// Set once at startup based on config and block producer status.
    pub peer_sharing_enabled: AtomicU64,
    /// Number of slot-battle forge attempts (i.e. forges where our
    /// wall-clock slot equalled the ledger tip's slot at forge time —
    /// a peer's block for the same slot was applied milliseconds before
    /// our forge ticker fired). Each is a competing block parented at
    /// the tip's parent; chain selection's VRF tiebreaker decides which
    /// of the two ends up on the canonical chain. Mirrors Haskell's
    /// `mkCurrentBlockContext` EQ branch in NodeKernel.hs.
    pub forge_slot_battles_total: AtomicU64,
    /// `dugite_config_reload_total{result="applied"}` — SIGHUP reloads that
    /// updated at least one hot-reloadable field and applied the change live.
    ///
    /// # HELP dugite_config_reload_total Count of SIGHUP-triggered config reloads by result
    pub config_reload_applied: AtomicU64,
    /// `dugite_config_reload_total{result="ignored"}` — SIGHUP reloads where
    /// every changed field requires a restart; the live config was not altered.
    pub config_reload_ignored: AtomicU64,
    /// `dugite_config_reload_total{result="rejected"}` — SIGHUP reloads where
    /// the config file failed to parse; the live config was not altered.
    pub config_reload_rejected: AtomicU64,

    // ── Peer governor target gauges (hot-reloadable via SIGHUP) ────────────
    //
    // Exposed as `dugite_peer_governor_target{name="..."}` so the integration
    // test and Prometheus alerts can observe that SIGHUP target changes took
    // effect within 10 seconds.
    /// Target number of active (hot) peers (`TargetNumberOfActivePeers`).
    pub peer_governor_target_active: AtomicU64,
    /// Target number of established (warm) peers (`TargetNumberOfEstablishedPeers`).
    pub peer_governor_target_established: AtomicU64,
    /// Maximum number of known (cold) peers (`TargetNumberOfKnownPeers`).
    pub peer_governor_target_known: AtomicU64,
    /// Target number of root peers (`TargetNumberOfRootPeers`).
    pub peer_governor_target_root: AtomicU64,
    /// Target number of active big-ledger peers.
    pub peer_governor_target_active_big: AtomicU64,
    /// Target number of established big-ledger peers.
    pub peer_governor_target_established_big: AtomicU64,
    /// Maximum number of known big-ledger peers.
    pub peer_governor_target_known_big: AtomicU64,
}

/// Plain-data view of the governance-related ledger state and Conway
/// protocol parameters, passed to [`NodeMetrics::set_governance_snapshot`].
///
/// Kept as a primitive struct so `metrics.rs` stays free of `dugite-ledger`
/// type dependencies — the caller is responsible for flattening
/// `LedgerState` into these fields.
#[derive(Debug, Clone, Copy, Default)]
pub struct GovernanceSnapshot {
    pub delegation_count: u64,
    pub treasury_lovelace: u64,
    pub reserves_lovelace: u64,
    pub pool_count: u64,
    pub drep_total: u64,
    pub drep_active: u64,
    pub drep_registrations_total: u64,
    pub vote_delegation_count: u64,
    pub proposal_count: u64,
    pub committee_hot_count: u64,
    pub committee_total_count: u64,
    pub committee_resigned_count: u64,
    pub committee_no_confidence: bool,
    pub committee_threshold_bps: u64,
    pub gov_dormant_epochs: u64,
    pub constitution_present: bool,
    pub pparam_drep_deposit_lovelace: u64,
    pub pparam_drep_activity_epochs: u64,
    pub pparam_gov_action_deposit_lovelace: u64,
    pub pparam_gov_action_lifetime_epochs: u64,
    pub pparam_committee_min_size: u64,
    pub pparam_committee_max_term_length: u64,
}

impl NodeMetrics {
    pub fn new() -> Self {
        NodeMetrics {
            blocks_received: AtomicU64::new(0),
            blocks_applied: AtomicU64::new(0),
            transactions_received: AtomicU64::new(0),
            transactions_validated: AtomicU64::new(0),
            transactions_rejected: AtomicU64::new(0),
            peers_connected: AtomicU64::new(0),
            peers_outbound: AtomicU64::new(0),
            peers_inbound: AtomicU64::new(0),
            peers_duplex: AtomicU64::new(0),
            peers_cold: AtomicU64::new(0),
            peers_warm: AtomicU64::new(0),
            peers_hot: AtomicU64::new(0),
            conn_full_duplex: AtomicU64::new(0),
            conn_duplex: AtomicU64::new(0),
            conn_unidirectional: AtomicU64::new(0),
            conn_inbound: AtomicU64::new(0),
            conn_outbound: AtomicU64::new(0),
            conn_terminating: AtomicU64::new(0),
            sync_progress_pct: AtomicU64::new(0),
            max_peer_tip_slot: AtomicU64::new(0),
            slot_number: AtomicU64::new(0),
            block_number: AtomicU64::new(0),
            epoch_number: AtomicU64::new(0),
            utxo_count: AtomicU64::new(0),
            mempool_tx_count: AtomicU64::new(0),
            mempool_tx_max: AtomicU64::new(0),
            mempool_bytes: AtomicU64::new(0),
            rollback_count: AtomicU64::new(0),
            blocks_forged: AtomicU64::new(0),
            delegation_count: AtomicU64::new(0),
            treasury_lovelace: AtomicU64::new(0),
            reserves_lovelace: AtomicU64::new(0),
            drep_count: AtomicU64::new(0),
            drep_active: AtomicU64::new(0),
            drep_registrations_total: AtomicU64::new(0),
            vote_delegation_count: AtomicU64::new(0),
            proposal_count: AtomicU64::new(0),
            pool_count: AtomicU64::new(0),
            committee_hot_count: AtomicU64::new(0),
            committee_total_count: AtomicU64::new(0),
            committee_resigned_count: AtomicU64::new(0),
            committee_no_confidence: AtomicU64::new(0),
            committee_threshold_bps: AtomicU64::new(0),
            gov_dormant_epochs: AtomicU64::new(0),
            constitution_present: AtomicU64::new(0),
            pparam_drep_deposit_lovelace: AtomicU64::new(0),
            pparam_drep_activity_epochs: AtomicU64::new(0),
            pparam_gov_action_deposit_lovelace: AtomicU64::new(0),
            pparam_gov_action_lifetime_epochs: AtomicU64::new(0),
            pparam_committee_min_size: AtomicU64::new(0),
            pparam_committee_max_term_length: AtomicU64::new(0),
            disk_total_bytes: AtomicU64::new(0),
            disk_used_bytes: AtomicU64::new(0),
            disk_available_bytes: AtomicU64::new(0),
            leader_checks_total: AtomicU64::new(0),
            leader_checks_not_elected: AtomicU64::new(0),
            forge_failures: AtomicU64::new(0),
            blocks_announced: AtomicU64::new(0),
            forge_race_lost: AtomicU64::new(0),
            forge_announce_no_subscribers: AtomicU64::new(0),
            n2n_connections_total: AtomicU64::new(0),
            n2c_connections_total: AtomicU64::new(0),
            n2n_connections_active: AtomicU64::new(0),
            n2c_connections_active: AtomicU64::new(0),
            n2c_txs_submitted: AtomicU64::new(0),
            n2c_txs_accepted: AtomicU64::new(0),
            n2c_txs_rejected: AtomicU64::new(0),
            protocol_errors: std::sync::Mutex::new(HashMap::new()),
            peer_handshake_rtt_ms: Histogram::new(),
            peer_block_fetch_ms: Histogram::new(),
            peer_rtt_avg_ms: AtomicU64::new(0),
            peer_rtt_min_ms: AtomicU64::new(0),
            peer_rtt_max_ms: AtomicU64::new(0),
            peer_rtt_band_0_50: AtomicU64::new(0),
            peer_rtt_band_50_100: AtomicU64::new(0),
            peer_rtt_band_100_200: AtomicU64::new(0),
            peer_rtt_band_200_plus: AtomicU64::new(0),
            peer_rtt_samples: AtomicU64::new(0),
            startup_instant: std::time::Instant::now(),
            validation_errors: std::sync::Mutex::new(HashMap::new()),
            last_block_received_at: AtomicU64::new(0),
            last_roll_forward_at: AtomicU64::new(0),
            replay_duration_secs: AtomicU64::new(0),
            tip_age_secs: AtomicU64::new(0),
            tip_slot_time_ms: AtomicU64::new(0),
            chainsync_idle_secs: AtomicU64::new(0),
            cpu_tracker: std::sync::Mutex::new(CpuTracker::new()),
            peak_mem_bytes: AtomicU64::new(0),
            network_magic: AtomicU64::new(0),
            epoch_length_slots: AtomicU64::new(0),
            slot_length_ms: AtomicU64::new(0),
            active_slots_coeff_x1000: AtomicU64::new(0),
            liveness_threshold_secs: AtomicU64::new(600),
            is_block_producer: AtomicU64::new(0),
            pool_id_hex: std::sync::Mutex::new(String::new()),
            compat_metrics: std::sync::atomic::AtomicBool::new(false),
            diffusion_mode: AtomicU64::new(0),
            peer_sharing_enabled: AtomicU64::new(1),
            forge_slot_battles_total: AtomicU64::new(0),
            config_reload_applied: AtomicU64::new(0),
            config_reload_ignored: AtomicU64::new(0),
            config_reload_rejected: AtomicU64::new(0),
            peer_governor_target_active: AtomicU64::new(0),
            peer_governor_target_established: AtomicU64::new(0),
            peer_governor_target_known: AtomicU64::new(0),
            peer_governor_target_root: AtomicU64::new(0),
            peer_governor_target_active_big: AtomicU64::new(0),
            peer_governor_target_established_big: AtomicU64::new(0),
            peer_governor_target_known_big: AtomicU64::new(0),
        }
    }

    /// Enable or disable `cardano_node_metrics_*` compatibility aliases.
    ///
    /// When enabled, `to_prometheus()` emits a second set of metric lines using
    /// the naming convention used by cardano-node (Haskell), so existing Grafana
    /// dashboards built for cardano-node continue to work without modification.
    pub fn set_compat_metrics(&self, enabled: bool) {
        self.compat_metrics
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record a transaction validation error by type.
    pub fn record_validation_error(&self, error_type: &str) {
        if let Ok(mut map) = self.validation_errors.lock() {
            *map.entry(error_type.to_string()).or_insert(0) += 1;
        }
    }

    /// Record a protocol-level error by label (e.g. "n2n_handshake_failed").
    #[allow(dead_code)] // used by networking rewrite
    pub fn record_protocol_error(&self, label: &str) {
        if let Ok(mut map) = self.protocol_errors.lock() {
            *map.entry(label.to_string()).or_insert(0) += 1;
        }
    }

    /// Record a peer handshake latency observation.
    pub fn record_handshake_rtt(&self, rtt_ms: f64) {
        self.peer_handshake_rtt_ms.observe(rtt_ms);
    }

    /// Record a per-block fetch latency observation.
    pub fn record_block_fetch_latency(&self, ms_per_block: f64) {
        self.peer_block_fetch_ms.observe(ms_per_block);
    }

    /// Update the current peer RTT gauges from PeerManager EWMA values.
    ///
    /// `latencies` must be the set of EWMA latency values (ms) for all
    /// currently-connected peers (warm or hot) that have at least one
    /// keepalive measurement. The Haskell node aggregates an analogous
    /// `Map peer PeerGSV` by snapshot on each metric read; we refresh
    /// these gauges on every KeepAlive pong.
    ///
    /// All gauges (min/avg/max + per-band counts + sample total) are
    /// recomputed from scratch — when a peer disconnects its entry is
    /// dropped from `connected_peer_latencies()` and its contribution
    /// to the bands disappears on the next refresh.
    pub fn update_peer_rtt_gauges(&self, latencies: &[f64]) {
        let mut band_0_50: u64 = 0;
        let mut band_50_100: u64 = 0;
        let mut band_100_200: u64 = 0;
        let mut band_200_plus: u64 = 0;
        for &ms in latencies {
            if ms < 50.0 {
                band_0_50 += 1;
            } else if ms < 100.0 {
                band_50_100 += 1;
            } else if ms < 200.0 {
                band_100_200 += 1;
            } else {
                band_200_plus += 1;
            }
        }
        self.peer_rtt_band_0_50.store(band_0_50, Ordering::Relaxed);
        self.peer_rtt_band_50_100
            .store(band_50_100, Ordering::Relaxed);
        self.peer_rtt_band_100_200
            .store(band_100_200, Ordering::Relaxed);
        self.peer_rtt_band_200_plus
            .store(band_200_plus, Ordering::Relaxed);
        self.peer_rtt_samples
            .store(latencies.len() as u64, Ordering::Relaxed);

        if latencies.is_empty() {
            self.peer_rtt_avg_ms.store(0, Ordering::Relaxed);
            self.peer_rtt_min_ms.store(0, Ordering::Relaxed);
            self.peer_rtt_max_ms.store(0, Ordering::Relaxed);
            return;
        }
        let sum: f64 = latencies.iter().sum();
        let avg = sum / latencies.len() as f64;
        let min = latencies.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = latencies.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        self.peer_rtt_avg_ms
            .store(f64::to_bits(avg), Ordering::Relaxed);
        self.peer_rtt_min_ms
            .store(f64::to_bits(min), Ordering::Relaxed);
        self.peer_rtt_max_ms
            .store(f64::to_bits(max), Ordering::Relaxed);
    }

    /// Update the peer governor target gauges from a live `RuntimeConfig`.
    ///
    /// Called both at node startup (to initialise the gauges) and on every
    /// SIGHUP reload so Prometheus reflects the new targets immediately.
    #[allow(clippy::too_many_arguments)]
    pub fn set_peer_governor_targets(
        &self,
        active: usize,
        established: usize,
        known: usize,
        root: usize,
        active_big: usize,
        established_big: usize,
        known_big: usize,
    ) {
        self.peer_governor_target_active
            .store(active as u64, Ordering::Relaxed);
        self.peer_governor_target_established
            .store(established as u64, Ordering::Relaxed);
        self.peer_governor_target_known
            .store(known as u64, Ordering::Relaxed);
        self.peer_governor_target_root
            .store(root as u64, Ordering::Relaxed);
        self.peer_governor_target_active_big
            .store(active_big as u64, Ordering::Relaxed);
        self.peer_governor_target_established_big
            .store(established_big as u64, Ordering::Relaxed);
        self.peer_governor_target_known_big
            .store(known_big as u64, Ordering::Relaxed);
    }

    pub fn add_blocks_received(&self, count: u64) {
        self.blocks_received.fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_blocks_applied(&self, count: u64) {
        self.blocks_applied.fetch_add(count, Ordering::Relaxed);
    }

    pub fn set_slot(&self, slot: u64) {
        self.slot_number.store(slot, Ordering::Relaxed);
    }

    pub fn set_block_number(&self, block_no: u64) {
        self.block_number.store(block_no, Ordering::Relaxed);
    }

    pub fn set_epoch(&self, epoch: u64) {
        self.epoch_number.store(epoch, Ordering::Relaxed);
    }

    pub fn set_sync_progress(&self, pct: f64) {
        self.sync_progress_pct
            .store((pct * 100.0) as u64, Ordering::Relaxed);
    }

    /// Update the maximum peer-reported tip slot.  Monotonic: only grows.
    /// Called from ChainSync on every `MsgRollForward` / `MsgRollBackward`
    /// using the message's `tip_slot` field (the peer's current tip,
    /// independent of how far we've fetched from it).
    pub fn update_peer_tip(&self, tip_slot: u64) {
        self.max_peer_tip_slot
            .fetch_max(tip_slot, Ordering::Relaxed);
    }

    /// Read the current maximum peer-reported tip slot.
    pub fn get_peer_tip(&self) -> u64 {
        self.max_peer_tip_slot.load(Ordering::Relaxed)
    }

    /// Recompute and store sync progress from our applied tip slot and the
    /// peer-reported tip slot.  Use this instead of `set_sync_progress(100.0)`
    /// in any block-apply path during sync — only the actual tip-following
    /// case should report 100%.
    pub fn refresh_sync_progress(&self, applied_slot: u64) {
        let peer_tip = self.get_peer_tip();
        self.set_sync_progress(compute_sync_progress(applied_slot, peer_tip));
    }

    pub fn set_utxo_count(&self, count: u64) {
        self.utxo_count.store(count, Ordering::Relaxed);
    }

    pub fn set_mempool_count(&self, count: u64) {
        self.mempool_tx_count.store(count, Ordering::Relaxed);
    }

    pub fn set_mempool_max(&self, max: u64) {
        self.mempool_tx_max.store(max, Ordering::Relaxed);
    }

    pub fn set_disk_available_bytes(&self, bytes: u64) {
        self.disk_available_bytes.store(bytes, Ordering::Relaxed);
    }

    pub fn set_disk_total_bytes(&self, bytes: u64) {
        self.disk_total_bytes.store(bytes, Ordering::Relaxed);
    }

    pub fn set_disk_used_bytes(&self, bytes: u64) {
        self.disk_used_bytes.store(bytes, Ordering::Relaxed);
    }

    /// Record that a block was just received (updates timestamp to now).
    pub fn record_block_received(&self) {
        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_block_received_at
            .store(now_millis, Ordering::Relaxed);
    }

    /// Returns the node health status: "healthy", "syncing", or "stalled".
    ///
    /// - "healthy": sync_progress >= 99.9%
    /// - "stalled": last block received > 5 minutes ago AND sync_progress < 99%
    /// - "syncing": everything else (actively catching up)
    pub fn health_status(&self) -> &'static str {
        let sync_pct = self.sync_progress_pct.load(Ordering::Relaxed);

        // Fully synced
        if sync_pct >= SYNCED_THRESHOLD {
            return "healthy";
        }

        // Check for stalled condition
        let last_block_ms = self.last_block_received_at.load(Ordering::Relaxed);
        if last_block_ms > 0 && sync_pct < 9900 {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let elapsed_secs = now_ms.saturating_sub(last_block_ms) / 1000;
            if elapsed_secs > STALLED_THRESHOLD_SECS {
                return "stalled";
            }
        }

        "syncing"
    }

    /// Returns the ISO 8601 timestamp of the last block received, or None if no block received yet.
    pub fn last_block_received_iso(&self) -> Option<String> {
        let ms = self.last_block_received_at.load(Ordering::Relaxed);
        if ms == 0 {
            return None;
        }
        let secs = (ms / 1000) as i64;
        let nanos = ((ms % 1000) * 1_000_000) as u32;
        let dt = chrono::DateTime::from_timestamp(secs, nanos)?;
        Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    }

    /// Returns uptime in seconds since node startup.
    pub fn uptime_seconds(&self) -> u64 {
        self.startup_instant.elapsed().as_secs()
    }

    /// Check if the node is ready (sync_progress >= 99.9%).
    /// Used for Kubernetes readiness probes.
    pub fn is_ready(&self) -> bool {
        self.sync_progress_pct.load(Ordering::Relaxed) >= SYNCED_THRESHOLD
    }

    /// Liveness probe: returns true if the node has applied a block within
    /// `liveness_threshold_secs`, or if no block has yet been received but the
    /// node has been up for less than the threshold (warm-up grace period).
    ///
    /// Used by Kubernetes liveness probes — returns 503 from `/live` only when
    /// the event loop appears wedged (no recent progress, past warm-up).
    /// A threshold of 0 disables the check (always alive).
    pub fn is_alive(&self) -> bool {
        let threshold = self.liveness_threshold_secs.load(Ordering::Relaxed);
        if threshold == 0 {
            return true;
        }
        let last_block_ms = self.last_block_received_at.load(Ordering::Relaxed);
        if last_block_ms == 0 {
            // No block yet — grant a warm-up grace equal to the threshold.
            return self.uptime_seconds() < threshold;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let elapsed_secs = now_ms.saturating_sub(last_block_ms) / 1000;
        elapsed_secs <= threshold
    }

    /// Render metrics in cardano-node's EKG (System.Remote.Monitoring) JSON
    /// shape so that existing dashboards polling `:12788/` continue to work.
    ///
    /// EKG groups metrics under a nested object tree where each leaf is
    /// `{"type": "c", "val": N}` (counter) or `{"type": "g", "val": N}` (gauge).
    /// The `cardano.node.metrics.*` namespace mirrors what cardano-node exposes.
    pub fn to_ekg_json(&self) -> String {
        use std::sync::atomic::Ordering::Relaxed;
        let g = |v: u64| format!(r#"{{"type":"g","val":{v}}}"#);
        let c = |v: u64| format!(r#"{{"type":"c","val":{v}}}"#);

        let slot = self.slot_number.load(Relaxed);
        let block = self.block_number.load(Relaxed);
        let epoch = self.epoch_number.load(Relaxed);
        let density = self.sync_progress_pct.load(Relaxed) as f64 / 10000.0;
        let mempool_tx = self.mempool_tx_count.load(Relaxed);
        let mempool_bytes = self.mempool_bytes.load(Relaxed);
        let utxo = self.utxo_count.load(Relaxed);
        let peers = self.peers_connected.load(Relaxed);
        let blocks_applied = self.blocks_applied.load(Relaxed);
        let blocks_forged = self.blocks_forged.load(Relaxed);
        let txs_processed = self.transactions_validated.load(Relaxed);

        format!(
            r#"{{"cardano":{{"node":{{"metrics":{{"slotNum_int":{slot_g},"blockNum_int":{block_g},"epoch_int":{epoch_g},"density_real":{{"type":"g","val":{density:.6}}},"txsInMempool_int":{mtx_g},"mempoolBytes_int":{mb_g},"utxoSize_int":{utxo_g},"connectedPeers_int":{peers_g},"blocksForgedNum_int":{forged_c},"served":{{"block":{{"count_int":{served_c}}}}},"txsProcessedNum_int":{txp_c},"Forge":{{"forge_adopted_int":{forged_c}}}}}}}}},"rts":{{"gc":{{"current_bytes_used":{used_g}}}}}}}"#,
            slot_g = g(slot),
            block_g = g(block),
            epoch_g = g(epoch),
            mtx_g = g(mempool_tx),
            mb_g = g(mempool_bytes),
            utxo_g = g(utxo),
            peers_g = g(peers),
            forged_c = c(blocks_forged),
            served_c = c(blocks_applied),
            txp_c = c(txs_processed),
            used_g = g(self.peak_mem_bytes.load(Relaxed)),
        )
    }

    /// Record a RollForward event timestamp for chainsync_idle tracking.
    pub fn record_roll_forward(&self) {
        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_roll_forward_at
            .store(now_millis, Ordering::Relaxed);
    }

    /// Set the tip slot time in milliseconds (POSIX). Tip age is computed dynamically.
    pub fn set_tip_slot_time_ms(&self, slot_time_ms: u64) {
        self.tip_slot_time_ms.store(slot_time_ms, Ordering::Relaxed);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let age = now_ms.saturating_sub(slot_time_ms) / 1000;
        self.tip_age_secs.store(age, Ordering::Relaxed);
    }

    /// Set the replay duration in seconds.
    pub fn set_replay_duration_secs(&self, secs: u64) {
        self.replay_duration_secs.store(secs, Ordering::Relaxed);
    }

    /// Set the network magic number.
    pub fn set_network_magic(&self, magic: u64) {
        self.network_magic.store(magic, Ordering::Relaxed);
    }

    /// Record Shelley-derived chain parameters that don't change at runtime
    /// without a hard fork: epoch length (slots), slot duration (ms), and
    /// `activeSlotsCoeff` × 1000. Call once at startup after the Shelley
    /// genesis has been parsed.
    pub fn set_shelley_chain_params(
        &self,
        epoch_length_slots: u64,
        slot_length_ms: u64,
        active_slots_coeff: f64,
    ) {
        self.epoch_length_slots
            .store(epoch_length_slots, Ordering::Relaxed);
        self.slot_length_ms.store(slot_length_ms, Ordering::Relaxed);
        self.active_slots_coeff_x1000.store(
            (active_slots_coeff * 1000.0).round() as u64,
            Ordering::Relaxed,
        );
    }

    /// Record P2P networking configuration state.
    ///
    /// Call once during node startup to set the P2P-related gauges from the
    /// node configuration.  These are read by the TUI to display the correct
    /// P2P status and diffusion mode.
    ///
    /// - `diffusion_mode`: the `DiffusionMode` config enum
    /// - `peer_sharing`: whether peer sharing mini-protocol is enabled
    pub fn set_p2p_config(
        &self,
        diffusion_mode: &crate::config::DiffusionMode,
        peer_sharing: bool,
    ) {
        self.diffusion_mode.store(
            match diffusion_mode {
                crate::config::DiffusionMode::InitiatorAndResponder => 0,
                crate::config::DiffusionMode::InitiatorOnly => 1,
            },
            Ordering::Relaxed,
        );
        self.peer_sharing_enabled
            .store(u64::from(peer_sharing), Ordering::Relaxed);
    }

    /// Update every governance-related gauge from a flattened snapshot.
    ///
    /// Called from the node's startup init path (`run`) and the sync loop's
    /// periodic metric-refresh site.  Single code path means the three
    /// previously-duplicated store sites cannot drift.
    pub fn set_governance_snapshot(&self, s: &GovernanceSnapshot) {
        self.delegation_count
            .store(s.delegation_count, Ordering::Relaxed);
        self.treasury_lovelace
            .store(s.treasury_lovelace, Ordering::Relaxed);
        self.reserves_lovelace
            .store(s.reserves_lovelace, Ordering::Relaxed);
        self.pool_count.store(s.pool_count, Ordering::Relaxed);
        self.drep_count.store(s.drep_total, Ordering::Relaxed);
        self.drep_active.store(s.drep_active, Ordering::Relaxed);
        self.drep_registrations_total
            .store(s.drep_registrations_total, Ordering::Relaxed);
        self.vote_delegation_count
            .store(s.vote_delegation_count, Ordering::Relaxed);
        self.proposal_count
            .store(s.proposal_count, Ordering::Relaxed);
        self.committee_hot_count
            .store(s.committee_hot_count, Ordering::Relaxed);
        self.committee_total_count
            .store(s.committee_total_count, Ordering::Relaxed);
        self.committee_resigned_count
            .store(s.committee_resigned_count, Ordering::Relaxed);
        self.committee_no_confidence
            .store(u64::from(s.committee_no_confidence), Ordering::Relaxed);
        self.committee_threshold_bps
            .store(s.committee_threshold_bps, Ordering::Relaxed);
        self.gov_dormant_epochs
            .store(s.gov_dormant_epochs, Ordering::Relaxed);
        self.constitution_present
            .store(u64::from(s.constitution_present), Ordering::Relaxed);
        self.pparam_drep_deposit_lovelace
            .store(s.pparam_drep_deposit_lovelace, Ordering::Relaxed);
        self.pparam_drep_activity_epochs
            .store(s.pparam_drep_activity_epochs, Ordering::Relaxed);
        self.pparam_gov_action_deposit_lovelace
            .store(s.pparam_gov_action_deposit_lovelace, Ordering::Relaxed);
        self.pparam_gov_action_lifetime_epochs
            .store(s.pparam_gov_action_lifetime_epochs, Ordering::Relaxed);
        self.pparam_committee_min_size
            .store(s.pparam_committee_min_size, Ordering::Relaxed);
        self.pparam_committee_max_term_length
            .store(s.pparam_committee_max_term_length, Ordering::Relaxed);
    }

    /// Record block producer mode.
    ///
    /// Call once during node startup when forge credentials are loaded.
    /// Sets `dugite_is_block_producer` to 1 and stores the pool ID hex string
    /// so the TUI can display the role and abbreviated pool identifier.
    pub fn set_block_producer(&self, pool_id_hex: &str) {
        self.is_block_producer.store(1, Ordering::Relaxed);
        if let Ok(mut guard) = self.pool_id_hex.lock() {
            *guard = pool_id_hex.to_string();
        }
    }

    /// Compute and store the chainsync idle time.
    pub fn update_chainsync_idle(&self) {
        let last_rf = self.last_roll_forward_at.load(Ordering::Relaxed);
        if last_rf > 0 {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let idle_secs = now_ms.saturating_sub(last_rf) / 1000;
            self.chainsync_idle_secs.store(idle_secs, Ordering::Relaxed);
        }
    }

    /// Format metrics as Prometheus exposition format
    pub(crate) fn to_prometheus(&self) -> String {
        // Recompute tip_age dynamically for freshness
        let slot_time_ms = self.tip_slot_time_ms.load(Ordering::Relaxed);
        if slot_time_ms > 0 {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            self.tip_age_secs.store(
                now_ms.saturating_sub(slot_time_ms) / 1000,
                Ordering::Relaxed,
            );
        }
        // Sample the CPU tracker on every scrape.  This computes the
        // percentage CPU consumed since the previous scrape interval and
        // accumulates cumulative seconds.  Both values are emitted below.
        let (cpu_pct, cpu_secs_total) = if let Ok(mut tracker) = self.cpu_tracker.lock() {
            let pct = tracker.sample();
            (pct, tracker.cumulative_cpu_secs)
        } else {
            (0.0, 0.0)
        };

        let mut out = String::with_capacity(2048);

        // Counters (monotonically increasing totals)
        let counters: &[(&str, &str, &AtomicU64)] = &[
            (
                "dugite_blocks_received_total",
                "Total blocks received from peers",
                &self.blocks_received,
            ),
            (
                "dugite_blocks_applied_total",
                "Total blocks applied to ledger",
                &self.blocks_applied,
            ),
            (
                "dugite_transactions_received_total",
                "Total transactions received",
                &self.transactions_received,
            ),
            (
                "dugite_transactions_validated_total",
                "Total transactions validated",
                &self.transactions_validated,
            ),
            (
                "dugite_transactions_rejected_total",
                "Total transactions rejected",
                &self.transactions_rejected,
            ),
            (
                "dugite_rollback_count_total",
                "Total number of chain rollbacks",
                &self.rollback_count,
            ),
            (
                "dugite_blocks_forged_total",
                "Total blocks forged by this node",
                &self.blocks_forged,
            ),
            (
                "dugite_leader_checks_total",
                "Total VRF leader checks performed",
                &self.leader_checks_total,
            ),
            (
                "dugite_leader_checks_not_elected_total",
                "Leader checks where node was not elected",
                &self.leader_checks_not_elected,
            ),
            (
                "dugite_forge_failures_total",
                "Block forge attempts that failed",
                &self.forge_failures,
            ),
            (
                "dugite_blocks_announced_total",
                "Blocks successfully announced to peers",
                &self.blocks_announced,
            ),
            (
                "dugite_forge_race_lost_total",
                "Forged blocks that lost a race to incoming blocks (not adopted as tip)",
                &self.forge_race_lost,
            ),
            (
                "dugite_forge_announce_no_subscribers_total",
                "Forge announcements sent with zero broadcast subscribers (propagation failures)",
                &self.forge_announce_no_subscribers,
            ),
            (
                "dugite_forge_slot_battles_total",
                "Forge attempts where wall-clock slot equalled the ledger tip slot \
                 (a peer forged at our slot first); each is a competing block whose \
                 fate is decided by chain selection's VRF tiebreaker",
                &self.forge_slot_battles_total,
            ),
            (
                "dugite_n2n_connections_total",
                "Total N2N connections accepted",
                &self.n2n_connections_total,
            ),
            (
                "dugite_n2c_connections_total",
                "Total N2C connections accepted",
                &self.n2c_connections_total,
            ),
        ];

        // Gauges (can go up and down)
        let gauges: &[(&str, &str, &AtomicU64)] = &[
            (
                "dugite_peers_connected",
                "Number of connected peers",
                &self.peers_connected,
            ),
            (
                "dugite_peers_outbound",
                "Outbound peer connections (initiated by us)",
                &self.peers_outbound,
            ),
            (
                "dugite_peers_inbound",
                "Inbound peer connections (initiated by remote)",
                &self.peers_inbound,
            ),
            (
                "dugite_peers_duplex",
                "Duplex (bidirectional) peer connections",
                &self.peers_duplex,
            ),
            (
                "dugite_peers_cold",
                "Number of cold (known but unconnected) peers",
                &self.peers_cold,
            ),
            (
                "dugite_peers_warm",
                "Number of warm (connected, not syncing) peers",
                &self.peers_warm,
            ),
            (
                "dugite_peers_hot",
                "Number of hot (actively syncing) peers",
                &self.peers_hot,
            ),
            (
                "dugite_conn_full_duplex",
                "Connections in full duplex state (both sides active)",
                &self.conn_full_duplex,
            ),
            (
                "dugite_conn_duplex",
                "Connections negotiated as Duplex (InitiatorAndResponder)",
                &self.conn_duplex,
            ),
            (
                "dugite_conn_unidirectional",
                "Connections negotiated as Unidirectional (InitiatorOnly)",
                &self.conn_unidirectional,
            ),
            (
                "dugite_conn_inbound",
                "Inbound connections (remote initiated)",
                &self.conn_inbound,
            ),
            (
                "dugite_conn_outbound",
                "Outbound connections (locally initiated)",
                &self.conn_outbound,
            ),
            (
                "dugite_conn_terminating",
                "Connections currently being torn down",
                &self.conn_terminating,
            ),
            (
                "dugite_sync_progress_percent",
                "Chain sync progress (0-10000, divide by 100 for %)",
                &self.sync_progress_pct,
            ),
            (
                "dugite_max_peer_tip_slot",
                "Maximum tip slot reported by any peer via ChainSync (denominator for sync_progress_percent)",
                &self.max_peer_tip_slot,
            ),
            (
                "dugite_slot_number",
                "Current slot number",
                &self.slot_number,
            ),
            (
                "dugite_block_number",
                "Current block number",
                &self.block_number,
            ),
            (
                "dugite_epoch_number",
                "Current epoch number",
                &self.epoch_number,
            ),
            (
                "dugite_utxo_count",
                "Number of entries in the UTxO set",
                &self.utxo_count,
            ),
            (
                "dugite_mempool_tx_count",
                "Number of transactions in the mempool",
                &self.mempool_tx_count,
            ),
            (
                "dugite_mempool_tx_max",
                "Maximum transaction capacity of the mempool",
                &self.mempool_tx_max,
            ),
            (
                "dugite_mempool_bytes",
                "Size of mempool in bytes",
                &self.mempool_bytes,
            ),
            (
                "dugite_delegation_count",
                "Number of active stake delegations",
                &self.delegation_count,
            ),
            (
                "dugite_treasury_lovelace",
                "Total lovelace in the treasury",
                &self.treasury_lovelace,
            ),
            (
                "dugite_reserves_lovelace",
                "Total lovelace remaining in the reserves pot",
                &self.reserves_lovelace,
            ),
            (
                "dugite_drep_count",
                "Total number of registered DReps in ledger state (active + inactive). For DReps with delegated voting power use Koios drep_epoch_summary.dreps.",
                &self.drep_count,
            ),
            (
                "dugite_drep_active",
                "DReps still within their activity window (voting-eligible subset of dugite_drep_count)",
                &self.drep_active,
            ),
            (
                "dugite_drep_registrations_total",
                "Monotonic counter of RegDRep certificates observed since node start",
                &self.drep_registrations_total,
            ),
            (
                "dugite_vote_delegation_count",
                "Stake credentials currently delegated to a DRep (any variant)",
                &self.vote_delegation_count,
            ),
            (
                "dugite_proposal_count",
                "Number of active governance proposals",
                &self.proposal_count,
            ),
            (
                "dugite_pool_count",
                "Number of registered stake pools",
                &self.pool_count,
            ),
            (
                "dugite_committee_hot_count",
                "Committee members with a hot-key authorization",
                &self.committee_hot_count,
            ),
            (
                "dugite_committee_total_count",
                "Committee members with an expiration epoch (active + unauthorized cold keys)",
                &self.committee_total_count,
            ),
            (
                "dugite_committee_resigned_count",
                "Committee members that have resigned",
                &self.committee_resigned_count,
            ),
            (
                "dugite_committee_no_confidence",
                "1 when the committee is in a no-confidence (dissolved) state",
                &self.committee_no_confidence,
            ),
            (
                "dugite_committee_threshold_bps",
                "Committee quorum threshold in basis points (0-10000)",
                &self.committee_threshold_bps,
            ),
            (
                "dugite_gov_dormant_epochs",
                "Cumulative dormant-epoch counter since Conway genesis",
                &self.gov_dormant_epochs,
            ),
            (
                "dugite_constitution_present",
                "1 when a constitution is set in the governance state",
                &self.constitution_present,
            ),
            (
                "dugite_pparam_drep_deposit_lovelace",
                "Deposit required to register a DRep (drepDeposit)",
                &self.pparam_drep_deposit_lovelace,
            ),
            (
                "dugite_pparam_drep_activity_epochs",
                "DRep activity window in epochs (drepActivity)",
                &self.pparam_drep_activity_epochs,
            ),
            (
                "dugite_pparam_gov_action_deposit_lovelace",
                "Deposit required to submit a governance action (govActionDeposit)",
                &self.pparam_gov_action_deposit_lovelace,
            ),
            (
                "dugite_pparam_gov_action_lifetime_epochs",
                "Maximum governance action lifetime in epochs (govActionLifetime)",
                &self.pparam_gov_action_lifetime_epochs,
            ),
            (
                "dugite_pparam_committee_min_size",
                "Minimum constitutional committee size",
                &self.pparam_committee_min_size,
            ),
            (
                "dugite_pparam_committee_max_term_length",
                "Maximum committee term length in epochs",
                &self.pparam_committee_max_term_length,
            ),
            (
                "dugite_disk_total_bytes",
                "Total disk space in bytes on the database volume",
                &self.disk_total_bytes,
            ),
            (
                "dugite_disk_used_bytes",
                "Used disk space in bytes on the database volume",
                &self.disk_used_bytes,
            ),
            (
                "dugite_disk_available_bytes",
                "Available disk space in bytes on the database volume",
                &self.disk_available_bytes,
            ),
            (
                "dugite_n2n_connections_active",
                "Currently active N2N connections",
                &self.n2n_connections_active,
            ),
            (
                "dugite_n2c_connections_active",
                "Currently active N2C connections",
                &self.n2c_connections_active,
            ),
            (
                "dugite_n2c_txs_submitted_total",
                "Total transactions submitted via N2C LocalTxSubmission",
                &self.n2c_txs_submitted,
            ),
            (
                "dugite_n2c_txs_accepted_total",
                "Transactions accepted via N2C LocalTxSubmission",
                &self.n2c_txs_accepted,
            ),
            (
                "dugite_n2c_txs_rejected_total",
                "Transactions rejected via N2C LocalTxSubmission",
                &self.n2c_txs_rejected,
            ),
            (
                "dugite_tip_age_seconds",
                "Seconds since the tip slot time",
                &self.tip_age_secs,
            ),
            (
                "dugite_chainsync_idle_seconds",
                "Seconds since last ChainSync RollForward event",
                &self.chainsync_idle_secs,
            ),
            (
                "dugite_ledger_replay_duration_seconds",
                "Duration of last ledger replay in seconds",
                &self.replay_duration_secs,
            ),
            (
                "dugite_network_magic",
                "Network magic number (764824073=mainnet, 2=preview, 1=preprod)",
                &self.network_magic,
            ),
            (
                "dugite_epoch_length",
                "Slots per epoch from the active Shelley genesis",
                &self.epoch_length_slots,
            ),
            (
                "dugite_slot_length_ms",
                "Slot duration in milliseconds from the active Shelley genesis",
                &self.slot_length_ms,
            ),
            (
                "dugite_active_slots_coeff_x1000",
                "activeSlotsCoeff (Praos f) scaled by 1000, e.g. 200 = f=0.20",
                &self.active_slots_coeff_x1000,
            ),
            (
                "dugite_is_block_producer",
                "1 when running as a block producer (forge credentials loaded), 0 for relay",
                &self.is_block_producer,
            ),
            (
                "dugite_diffusion_mode",
                "Diffusion mode: 0 = InitiatorAndResponder, 1 = InitiatorOnly",
                &self.diffusion_mode,
            ),
            (
                "dugite_peer_sharing_enabled",
                "1 when peer sharing mini-protocol is enabled, 0 when disabled",
                &self.peer_sharing_enabled,
            ),
        ];

        for (name, help, value) in counters {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} counter\n{name} {}\n",
                value.load(Ordering::Relaxed)
            ));
        }

        for (name, help, value) in gauges {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {}\n",
                value.load(Ordering::Relaxed)
            ));
        }

        // Uptime gauge
        let uptime_secs = self.startup_instant.elapsed().as_secs();
        out.push_str(&format!(
            "# HELP dugite_uptime_seconds Time since node startup\n# TYPE dugite_uptime_seconds gauge\ndugite_uptime_seconds {uptime_secs}\n"
        ));

        // Pool ID info metric — emitted only when running as a block producer.
        //
        // Uses a Prometheus info metric pattern: a gauge permanently set to 1
        // with a `pool_id` label carrying the hex-encoded pool identifier.
        // The TUI reads `dugite_pool_id_info` and parses the label value from
        // the metrics text to display an abbreviated pool ID in the Node panel.
        if self.is_block_producer.load(Ordering::Relaxed) == 1 {
            if let Ok(guard) = self.pool_id_hex.lock() {
                if !guard.is_empty() {
                    out.push_str(&format!(
                        "# HELP dugite_pool_id_info Block producer pool identity\n\
                         # TYPE dugite_pool_id_info gauge\n\
                         dugite_pool_id_info{{pool_id=\"{}\"}} 1\n",
                        *guard
                    ));
                }
            }
        }

        // CPU metrics — emitted as both a gauge (current %) and a counter
        // (cumulative seconds of CPU time consumed since node start).
        //
        // `dugite_cpu_percent`: instantaneous CPU utilisation (user + kernel)
        //   relative to one logical core.  >100 is possible on multi-threaded
        //   workloads.
        // `dugite_cpu_seconds_total`: monotonically increasing counter of
        //   wall-adjusted CPU seconds consumed since node start.
        out.push_str(&format!(
            "# HELP dugite_cpu_percent Process CPU utilisation as a percentage of one core\n\
             # TYPE dugite_cpu_percent gauge\n\
             dugite_cpu_percent {cpu_pct:.3}\n"
        ));
        out.push_str(&format!(
            "# HELP dugite_cpu_seconds_total Cumulative CPU time consumed by the process in seconds\n\
             # TYPE dugite_cpu_seconds_total counter\n\
             dugite_cpu_seconds_total {cpu_secs_total:.6}\n"
        ));

        // Resident memory gauge
        let rss = get_resident_memory_bytes();
        out.push_str(&format!(
            "# HELP dugite_mem_resident_bytes Resident set size in bytes\n# TYPE dugite_mem_resident_bytes gauge\ndugite_mem_resident_bytes {rss}\n"
        ));

        // Total system physical memory gauge — used by the TUI memory bar to
        // show RSS as a percentage of total RAM rather than a raw byte value.
        let mem_total = get_total_memory_bytes();
        if mem_total > 0 {
            out.push_str(&format!(
                "# HELP dugite_mem_total_bytes Total physical memory on this host in bytes\n\
                 # TYPE dugite_mem_total_bytes gauge\n\
                 dugite_mem_total_bytes {mem_total}\n"
            ));
        }

        // Track peak RSS (monotonically increasing high-water mark).
        let _ = self.peak_mem_bytes.fetch_max(rss, Ordering::Relaxed);
        let peak = self.peak_mem_bytes.load(Ordering::Relaxed);
        out.push_str(&format!(
            "# HELP dugite_mem_peak_bytes Peak resident set size in bytes\n# TYPE dugite_mem_peak_bytes gauge\ndugite_mem_peak_bytes {peak}\n"
        ));

        // Validation error breakdown
        if let Ok(errors) = self.validation_errors.lock() {
            if !errors.is_empty() {
                out.push_str(
                    "# HELP dugite_validation_errors_total Transaction validation errors by type\n",
                );
                out.push_str("# TYPE dugite_validation_errors_total counter\n");
                let mut sorted: Vec<_> = errors.iter().collect();
                sorted.sort_by_key(|(k, _)| (*k).clone());
                for (error_type, count) in sorted {
                    out.push_str(&format!(
                        "dugite_validation_errors_total{{error=\"{error_type}\"}} {count}\n"
                    ));
                }
            }
        }

        // Protocol error breakdown
        if let Ok(errors) = self.protocol_errors.lock() {
            if !errors.is_empty() {
                out.push_str("# HELP dugite_protocol_errors_total Protocol errors by type\n");
                out.push_str("# TYPE dugite_protocol_errors_total counter\n");
                let mut sorted: Vec<_> = errors.iter().collect();
                sorted.sort_by_key(|(k, _)| (*k).clone());
                for (error_type, count) in sorted {
                    out.push_str(&format!(
                        "dugite_protocol_errors_total{{error=\"{error_type}\"}} {count}\n"
                    ));
                }
            }
        }

        // Config reload counter — labeled by result (applied/ignored/rejected).
        //
        // Emitted unconditionally (even when all counts are zero) so that
        // Prometheus alert rules can use `absent()` / `increase()` without
        // needing a "metric not found" guard.
        {
            let applied = self.config_reload_applied.load(Ordering::Relaxed);
            let ignored = self.config_reload_ignored.load(Ordering::Relaxed);
            let rejected = self.config_reload_rejected.load(Ordering::Relaxed);
            out.push_str(
                "# HELP dugite_config_reload_total Count of SIGHUP-triggered config reloads by result\n",
            );
            out.push_str("# TYPE dugite_config_reload_total counter\n");
            out.push_str(&format!(
                "dugite_config_reload_total{{result=\"applied\"}} {applied}\n"
            ));
            out.push_str(&format!(
                "dugite_config_reload_total{{result=\"ignored\"}} {ignored}\n"
            ));
            out.push_str(&format!(
                "dugite_config_reload_total{{result=\"rejected\"}} {rejected}\n"
            ));
        }

        // Peer governor target gauges — emitted unconditionally so alert rules
        // can use `absent()` without a "metric not found" guard.
        {
            let active = self.peer_governor_target_active.load(Ordering::Relaxed);
            let established = self
                .peer_governor_target_established
                .load(Ordering::Relaxed);
            let known = self.peer_governor_target_known.load(Ordering::Relaxed);
            let root = self.peer_governor_target_root.load(Ordering::Relaxed);
            let active_big = self.peer_governor_target_active_big.load(Ordering::Relaxed);
            let established_big = self
                .peer_governor_target_established_big
                .load(Ordering::Relaxed);
            let known_big = self.peer_governor_target_known_big.load(Ordering::Relaxed);
            out.push_str(
                "# HELP dugite_peer_governor_target Peer governor target counts by name\n",
            );
            out.push_str("# TYPE dugite_peer_governor_target gauge\n");
            out.push_str(&format!(
                "dugite_peer_governor_target{{name=\"active\"}} {active}\n"
            ));
            out.push_str(&format!(
                "dugite_peer_governor_target{{name=\"established\"}} {established}\n"
            ));
            out.push_str(&format!(
                "dugite_peer_governor_target{{name=\"known\"}} {known}\n"
            ));
            out.push_str(&format!(
                "dugite_peer_governor_target{{name=\"root\"}} {root}\n"
            ));
            out.push_str(&format!(
                "dugite_peer_governor_target{{name=\"active_big\"}} {active_big}\n"
            ));
            out.push_str(&format!(
                "dugite_peer_governor_target{{name=\"established_big\"}} {established_big}\n"
            ));
            out.push_str(&format!(
                "dugite_peer_governor_target{{name=\"known_big\"}} {known_big}\n"
            ));
        }

        // Histograms
        out.push_str(&self.peer_handshake_rtt_ms.to_prometheus(
            "dugite_peer_handshake_rtt_ms",
            "Peer handshake round-trip time in milliseconds",
        ));
        out.push_str(&self.peer_block_fetch_ms.to_prometheus(
            "dugite_peer_block_fetch_ms",
            "Per-block fetch latency in milliseconds",
        ));

        // Current peer RTT gauges (from KeepAlive EWMA, not cumulative histogram).
        let rtt_avg = f64::from_bits(self.peer_rtt_avg_ms.load(Ordering::Relaxed));
        let rtt_min = f64::from_bits(self.peer_rtt_min_ms.load(Ordering::Relaxed));
        let rtt_max = f64::from_bits(self.peer_rtt_max_ms.load(Ordering::Relaxed));
        out.push_str(
            "# HELP dugite_peer_rtt_avg_ms Current average peer RTT in milliseconds (EWMA)\n",
        );
        out.push_str("# TYPE dugite_peer_rtt_avg_ms gauge\n");
        out.push_str(&format!("dugite_peer_rtt_avg_ms {rtt_avg:.1}\n"));
        out.push_str(
            "# HELP dugite_peer_rtt_min_ms Current minimum peer RTT in milliseconds (EWMA)\n",
        );
        out.push_str("# TYPE dugite_peer_rtt_min_ms gauge\n");
        out.push_str(&format!("dugite_peer_rtt_min_ms {rtt_min:.1}\n"));
        out.push_str(
            "# HELP dugite_peer_rtt_max_ms Current maximum peer RTT in milliseconds (EWMA)\n",
        );
        out.push_str("# TYPE dugite_peer_rtt_max_ms gauge\n");
        out.push_str(&format!("dugite_peer_rtt_max_ms {rtt_max:.1}\n"));

        // Per-band RTT gauges — counts of currently-connected peers (warm/hot
        // with a KeepAlive measurement) bucketed by EWMA RTT.  Refreshed on
        // every KeepAlive pong; peers that disconnect drop out automatically.
        let band_0_50 = self.peer_rtt_band_0_50.load(Ordering::Relaxed);
        let band_50_100 = self.peer_rtt_band_50_100.load(Ordering::Relaxed);
        let band_100_200 = self.peer_rtt_band_100_200.load(Ordering::Relaxed);
        let band_200_plus = self.peer_rtt_band_200_plus.load(Ordering::Relaxed);
        let rtt_samples = self.peer_rtt_samples.load(Ordering::Relaxed);
        out.push_str("# HELP dugite_peer_rtt_band_0_50 Connected peers with EWMA RTT < 50ms\n");
        out.push_str("# TYPE dugite_peer_rtt_band_0_50 gauge\n");
        out.push_str(&format!("dugite_peer_rtt_band_0_50 {band_0_50}\n"));
        out.push_str(
            "# HELP dugite_peer_rtt_band_50_100 Connected peers with EWMA RTT in [50,100)ms\n",
        );
        out.push_str("# TYPE dugite_peer_rtt_band_50_100 gauge\n");
        out.push_str(&format!("dugite_peer_rtt_band_50_100 {band_50_100}\n"));
        out.push_str(
            "# HELP dugite_peer_rtt_band_100_200 Connected peers with EWMA RTT in [100,200)ms\n",
        );
        out.push_str("# TYPE dugite_peer_rtt_band_100_200 gauge\n");
        out.push_str(&format!("dugite_peer_rtt_band_100_200 {band_100_200}\n"));
        out.push_str(
            "# HELP dugite_peer_rtt_band_200_plus Connected peers with EWMA RTT >= 200ms\n",
        );
        out.push_str("# TYPE dugite_peer_rtt_band_200_plus gauge\n");
        out.push_str(&format!("dugite_peer_rtt_band_200_plus {band_200_plus}\n"));
        out.push_str(
            "# HELP dugite_peer_rtt_samples Number of connected peers with at least one RTT sample\n",
        );
        out.push_str("# TYPE dugite_peer_rtt_samples gauge\n");
        out.push_str(&format!("dugite_peer_rtt_samples {rtt_samples}\n"));

        // cardano-node compatibility aliases.
        //
        // When --compat-metrics is set, emit a second set of metric lines using
        // the `cardano_node_metrics_*` naming convention.  This allows operators
        // to reuse existing cardano-node Grafana dashboards without modification.
        //
        // Naming rules follow the cardano-node EKG metric export convention:
        //   - Integer gauges use the `_int` suffix.
        //   - The density metric is a real-valued fraction in [0, 1].
        //   - forge metrics use the full EKG path as the metric name.
        //
        // NOTE: We emit only GAUGE lines (no # TYPE or # HELP declarations) for
        // the compat names because Prometheus rejects duplicate TYPE declarations
        // when the same name appears twice, and the compat names are aliases, not
        // independent metrics.  Prometheus will infer the type as "untyped" for
        // lines without a TYPE header, which is harmless for dashboard queries.
        if self.compat_metrics.load(Ordering::Relaxed) {
            // slotNum_int — current slot number
            out.push_str(&format!(
                "cardano_node_metrics_slotNum_int {}\n",
                self.slot_number.load(Ordering::Relaxed)
            ));

            // blockNum_int — current block number
            out.push_str(&format!(
                "cardano_node_metrics_blockNum_int {}\n",
                self.block_number.load(Ordering::Relaxed)
            ));

            // epoch_int — current epoch number
            out.push_str(&format!(
                "cardano_node_metrics_epoch_int {}\n",
                self.epoch_number.load(Ordering::Relaxed)
            ));

            // connectedPeers_int — total connected peers
            out.push_str(&format!(
                "cardano_node_metrics_connectedPeers_int {}\n",
                self.peers_connected.load(Ordering::Relaxed)
            ));

            // utxoSize_int — UTxO set size
            out.push_str(&format!(
                "cardano_node_metrics_utxoSize_int {}\n",
                self.utxo_count.load(Ordering::Relaxed)
            ));

            // txsInMempool_int — mempool transaction count
            out.push_str(&format!(
                "cardano_node_metrics_txsInMempool_int {}\n",
                self.mempool_tx_count.load(Ordering::Relaxed)
            ));

            // mempoolBytes_int — mempool size in bytes
            out.push_str(&format!(
                "cardano_node_metrics_mempoolBytes_int {}\n",
                self.mempool_bytes.load(Ordering::Relaxed)
            ));

            // Forge_forge_adopted_int — blocks forged and adopted
            out.push_str(&format!(
                "cardano_node_metrics_Forge_forge_adopted_int {}\n",
                self.blocks_forged.load(Ordering::Relaxed)
            ));

            // density_real — chain density as a fraction in [0, 1].
            //
            // dugite stores sync progress as (percentage * 100), i.e. 0–10000
            // for 0%–100%.  Divide by 10000 to produce the [0, 1] density
            // fraction that cardano-node's EKG dashboard panel expects.
            let density = self.sync_progress_pct.load(Ordering::Relaxed) as f64 / 10000.0;
            out.push_str(&format!("cardano_node_metrics_density_real {density:.6}\n"));
        }

        out
    }
}

impl Default for NodeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Bind a TCP listener for the metrics HTTP server.
///
/// Retries up to `MAX_RETRIES` times (1-second delay between attempts) to
/// tolerate brief port-in-use windows when the node is restarted quickly
/// after a crash (`TIME_WAIT` / previous instance still shutting down).
///
/// Returns the bound listener on success, or the last bind error on failure.
/// Non-retryable errors (permission denied, etc.) are returned immediately.
///
/// This function is separate from `start_metrics_server` so callers that need
/// to fail fast on bind failure (e.g. `--require-metrics`) can call it
/// synchronously before spawning the server loop.
pub async fn bind_metrics_listener(port: u16) -> Result<TcpListener, std::io::Error> {
    let addr = format!("0.0.0.0:{port}");

    const MAX_RETRIES: u32 = 5;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

    let mut last_err = None;
    for attempt in 1..=MAX_RETRIES {
        match TcpListener::bind(&addr).await {
            Ok(l) => {
                info!(
                    url = format_args!("http://{addr}/metrics"),
                    "Metrics server started"
                );
                return Ok(l);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                if attempt < MAX_RETRIES {
                    tracing::warn!(
                        port,
                        attempt,
                        max = MAX_RETRIES,
                        "Metrics port in use, retrying in 1s"
                    );
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                last_err = Some(e);
            }
            Err(e) => {
                // Non-retryable error (permission denied, etc.)
                error!("Failed to start metrics server on {addr}: {e}");
                return Err(e);
            }
        }
    }

    let e = last_err.unwrap();
    error!("Failed to start metrics server on {addr} after {MAX_RETRIES} attempts: {e}");
    Err(e)
}

/// Run the metrics HTTP server accept loop on an already-bound listener.
///
/// Responds to each request with Prometheus-format metrics, health endpoints,
/// etc.  Exits when `shutdown_rx` fires.
pub async fn run_metrics_server(
    listener: TcpListener,
    metrics: Arc<NodeMetrics>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    // Connection timeout: if a client connects but does not send an HTTP
    // request within this window, the task is dropped and the connection closed.
    // Prevents an abandoned or slow client from blocking the metrics server
    // (the old sequential loop had no timeout and no per-connection spawning).
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, _peer) = match accept_result {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!("Metrics server accept error: {e}");
                        continue;
                    }
                };

                // Spawn a task per connection so one slow scraper cannot block others.
                let metrics_clone = metrics.clone();
                tokio::spawn(async move {
                    // Apply a hard timeout so abandoned connections don't linger.
                    let _ = tokio::time::timeout(
                        READ_TIMEOUT,
                        handle_metrics_connection(stream, metrics_clone),
                    )
                    .await;
                });
            }
            _ = shutdown_rx.changed() => {
                info!("Metrics server shutting down");
                break;
            }
        }
    }
}

/// Start an HTTP metrics server on the given port.
///
/// Convenience wrapper that binds (with retries) then runs the accept loop.
/// Returns an error if the bind phase fails.
///
/// For callers that need to fail fast before spawning (e.g. `--require-metrics`),
/// use [`bind_metrics_listener`] + [`run_metrics_server`] directly.
pub async fn start_metrics_server(
    port: u16,
    metrics: Arc<NodeMetrics>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(), std::io::Error> {
    let listener = bind_metrics_listener(port).await?;
    run_metrics_server(listener, metrics, shutdown_rx).await;
    Ok(())
}

/// Handle a single HTTP request on the metrics server.
///
/// Reads the request line, generates the response, and writes it back.
/// Called from a spawned task so that one slow or abandoned connection
/// cannot block the accept loop from serving subsequent scrapers.
async fn handle_metrics_connection(mut stream: tokio::net::TcpStream, metrics: Arc<NodeMetrics>) {
    let mut buf = [0u8; 1024];
    let n = match tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await {
        Ok(n) => n,
        Err(_) => return,
    };
    let request = std::str::from_utf8(&buf[..n]).unwrap_or("");

    let response = route_request(request, &metrics);

    let _ = stream.write_all(response.as_bytes()).await;
}

/// Build an HTTP response string for a given raw request line.
///
/// Extracted so unit tests can exercise the routing logic without binding
/// a TCP socket.
pub fn route_request(request: &str, metrics: &NodeMetrics) -> String {
    if request.starts_with("GET /live") {
        // Kubernetes liveness probe: 200 if event loop is making progress
        // (recent block applied OR within warm-up grace), 503 if wedged.
        let threshold = metrics.liveness_threshold_secs.load(Ordering::Relaxed);
        if metrics.is_alive() {
            let body = format!(r#"{{"alive":true,"threshold_secs":{threshold}}}"#);
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        } else {
            let last_block_ts = metrics.last_block_received_iso();
            let last_block_json = match &last_block_ts {
                Some(ts) => format!("\"{}\"", ts),
                None => "null".to_string(),
            };
            let body = format!(
                r#"{{"alive":false,"threshold_secs":{threshold},"last_block_received_at":{last_block_json}}}"#
            );
            format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        }
    } else if request.starts_with("GET /ekg") {
        // cardano-node EKG (System.Remote.Monitoring) compatibility endpoint.
        // Returns the nested-object JSON layout that legacy gLiveView / CNTools
        // dashboards expect when polling port 12788.
        let body = metrics.to_ekg_json();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    } else if request.starts_with("GET /ready") {
        // Kubernetes readiness probe: 200 if synced, 503 if not
        if metrics.is_ready() {
            let body = r#"{"ready":true}"#;
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        } else {
            let sync_pct = metrics.sync_progress_pct.load(Ordering::Relaxed) as f64 / 100.0;
            let body = format!("{{\"ready\":false,\"sync_progress\":{sync_pct:.2}}}");
            format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        }
    } else if request.starts_with("GET /health") {
        let status = metrics.health_status();
        let uptime = metrics.uptime_seconds();
        let slot = metrics.slot_number.load(Ordering::Relaxed);
        let block = metrics.block_number.load(Ordering::Relaxed);
        let epoch = metrics.epoch_number.load(Ordering::Relaxed);
        let sync_pct = metrics.sync_progress_pct.load(Ordering::Relaxed) as f64 / 100.0;
        let peers = metrics.peers_connected.load(Ordering::Relaxed);
        let last_block_ts = metrics.last_block_received_iso();
        let last_block_json = match &last_block_ts {
            Some(ts) => format!("\"{}\"", ts),
            None => "null".to_string(),
        };
        let body = format!(
            "{{\"status\":\"{status}\",\"uptime_seconds\":{uptime},\"slot_number\":{slot},\"block_number\":{block},\"epoch_number\":{epoch},\"sync_progress\":{sync_pct:.2},\"peers_connected\":{peers},\"last_block_received_at\":{last_block_json}}}"
        );
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    } else {
        let body = metrics.to_prometheus();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the preprod 2026-05-11 100%-sync display bug.
    /// Block-apply paths used to call `set_sync_progress(100.0)`
    /// unconditionally, making dugite-monitor display 100% while the node
    /// was at <20% of the chain.  The fix routes all block-apply paths
    /// through `refresh_sync_progress` → `compute_sync_progress`.
    #[test]
    fn test_compute_sync_progress_during_bulk_sync() {
        // Mid bulk sync: applied 25M / peer tip 122M → ~20.4%.
        let pct = compute_sync_progress(25_462_569, 122_773_031);
        assert!((20.0..21.0).contains(&pct), "expected ~20.7%, got {pct}");
    }

    #[test]
    fn test_compute_sync_progress_at_or_past_tip() {
        // Equal slots → 100%.
        assert_eq!(compute_sync_progress(100, 100), 100.0);
        // Past tip (we just forged) → 100%.
        assert_eq!(compute_sync_progress(105, 100), 100.0);
    }

    #[test]
    fn test_compute_sync_progress_pre_peer_state() {
        // No peer tip known yet → 0% (not 100%, which would falsely
        // report "healthy" before the first ChainSync intersection).
        assert_eq!(compute_sync_progress(0, 0), 0.0);
        assert_eq!(compute_sync_progress(12345, 0), 0.0);
    }

    #[test]
    fn test_update_peer_tip_is_monotonic() {
        let metrics = NodeMetrics::new();
        assert_eq!(metrics.get_peer_tip(), 0);
        metrics.update_peer_tip(1_000);
        assert_eq!(metrics.get_peer_tip(), 1_000);
        // Later peer reports a smaller tip → ignored (monotonic).
        metrics.update_peer_tip(500);
        assert_eq!(metrics.get_peer_tip(), 1_000);
        // Larger tip wins.
        metrics.update_peer_tip(2_500);
        assert_eq!(metrics.get_peer_tip(), 2_500);
    }

    #[test]
    fn test_refresh_sync_progress_during_bulk_sync() {
        let metrics = NodeMetrics::new();
        metrics.update_peer_tip(100);
        metrics.refresh_sync_progress(25);
        // 25/100 = 25% → stored as 2500 in the centi-percent gauge.
        assert_eq!(metrics.sync_progress_pct.load(Ordering::Relaxed), 2500);
    }

    #[test]
    fn test_refresh_sync_progress_at_tip() {
        let metrics = NodeMetrics::new();
        metrics.update_peer_tip(100);
        metrics.refresh_sync_progress(100);
        assert_eq!(metrics.sync_progress_pct.load(Ordering::Relaxed), 10_000);
    }

    #[test]
    fn test_metrics() {
        let metrics = NodeMetrics::new();
        assert_eq!(metrics.blocks_applied.load(Ordering::Relaxed), 0);

        metrics.add_blocks_applied(2);
        assert_eq!(metrics.blocks_applied.load(Ordering::Relaxed), 2);

        metrics.set_slot(12345);
        assert_eq!(metrics.slot_number.load(Ordering::Relaxed), 12345);
    }

    #[test]
    fn test_prometheus_output() {
        let metrics = NodeMetrics::new();
        metrics.set_slot(99999);
        metrics.set_epoch(42);
        metrics.add_blocks_applied(100);

        let output = metrics.to_prometheus();
        assert!(output.contains("dugite_slot_number 99999"));
        assert!(output.contains("dugite_epoch_number 42"));
        assert!(output.contains("dugite_blocks_applied_total 100"));
        assert!(output.contains("# HELP"));
        // Verify correct metric types
        assert!(output.contains("# TYPE dugite_blocks_applied_total counter"));
        assert!(output.contains("# TYPE dugite_slot_number gauge"));
        assert!(output.contains("# TYPE dugite_rollback_count_total counter"));
        assert!(output.contains("# TYPE dugite_peers_connected gauge"));
    }

    // --- Liveness probe tests ---

    #[test]
    fn test_is_alive_warmup_grace_when_no_block_received() {
        // Fresh node, no block yet → alive (within warm-up grace window).
        let metrics = NodeMetrics::new();
        metrics
            .liveness_threshold_secs
            .store(600, Ordering::Relaxed);
        assert!(metrics.is_alive());
    }

    #[test]
    fn test_is_alive_disabled_with_zero_threshold() {
        let metrics = NodeMetrics::new();
        metrics.liveness_threshold_secs.store(0, Ordering::Relaxed);
        // Even with a very stale "last block" timestamp, threshold=0 → always alive.
        metrics.last_block_received_at.store(1, Ordering::Relaxed);
        assert!(metrics.is_alive());
    }

    #[test]
    fn test_is_alive_recent_block() {
        let metrics = NodeMetrics::new();
        metrics
            .liveness_threshold_secs
            .store(600, Ordering::Relaxed);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        metrics
            .last_block_received_at
            .store(now_ms, Ordering::Relaxed);
        assert!(metrics.is_alive());
    }

    #[test]
    fn test_is_alive_stale_block_after_threshold() {
        let metrics = NodeMetrics::new();
        metrics.liveness_threshold_secs.store(60, Ordering::Relaxed);
        // Pretend last block arrived 10 minutes ago.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let ten_min_ago = now_ms.saturating_sub(600_000);
        metrics
            .last_block_received_at
            .store(ten_min_ago, Ordering::Relaxed);
        assert!(!metrics.is_alive());
    }

    #[test]
    fn test_live_route_returns_200_when_alive() {
        let metrics = NodeMetrics::new();
        metrics
            .liveness_threshold_secs
            .store(600, Ordering::Relaxed);
        let resp = route_request("GET /live HTTP/1.1\r\n\r\n", &metrics);
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
        assert!(resp.contains("\"alive\":true"));
        assert!(resp.contains("\"threshold_secs\":600"));
    }

    #[test]
    fn test_live_route_returns_503_when_stale() {
        let metrics = NodeMetrics::new();
        metrics.liveness_threshold_secs.store(60, Ordering::Relaxed);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        metrics
            .last_block_received_at
            .store(now_ms.saturating_sub(600_000), Ordering::Relaxed);
        let resp = route_request("GET /live HTTP/1.1\r\n\r\n", &metrics);
        assert!(
            resp.starts_with("HTTP/1.1 503 Service Unavailable"),
            "got: {resp}"
        );
        assert!(resp.contains("\"alive\":false"));
    }

    // --- EKG endpoint tests ---

    #[test]
    fn test_ekg_route_returns_expected_keys() {
        let metrics = NodeMetrics::new();
        metrics.set_slot(12345);
        metrics.set_block_number(678);
        metrics.set_epoch(9);
        metrics.mempool_tx_count.store(7, Ordering::Relaxed);
        metrics.blocks_forged.store(3, Ordering::Relaxed);
        metrics.utxo_count.store(42, Ordering::Relaxed);
        metrics.peers_connected.store(5, Ordering::Relaxed);

        let resp = route_request("GET /ekg HTTP/1.1\r\n\r\n", &metrics);
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("application/json"));
        // EKG namespace + key metrics from issue #322
        assert!(resp.contains("\"cardano\""), "missing cardano root");
        assert!(resp.contains("\"slotNum_int\""), "missing slot");
        assert!(resp.contains("\"blockNum_int\""), "missing blockNum");
        assert!(
            resp.contains("\"blocksForgedNum_int\""),
            "missing blocksForgedNum"
        );
        assert!(
            resp.contains("\"txsInMempool_int\""),
            "missing txsInMempool"
        );
        assert!(resp.contains("\"density_real\""), "missing density");
        assert!(resp.contains("\"epoch_int\""), "missing epoch");
        // EKG counter/gauge wrapping
        assert!(
            resp.contains("\"type\":\"g\"") && resp.contains("\"type\":\"c\""),
            "missing EKG type/val wrapping"
        );
        // Values surface (gauge)
        assert!(resp.contains("\"val\":12345"));
        assert!(resp.contains("\"val\":7"));
    }

    #[test]
    fn test_ekg_json_parses_as_valid_json() {
        let metrics = NodeMetrics::new();
        metrics.set_slot(1);
        let body = metrics.to_ekg_json();
        let v: serde_json::Value =
            serde_json::from_str(&body).expect("EKG output must be valid JSON");
        // Navigate to the canonical EKG path used by gLiveView dashboards.
        let slot = &v["cardano"]["node"]["metrics"]["slotNum_int"]["val"];
        assert_eq!(slot.as_u64(), Some(1));
    }

    #[test]
    fn test_compat_metrics_disabled_by_default() {
        // With the default NodeMetrics no compat aliases should appear.
        let metrics = NodeMetrics::new();
        metrics.set_slot(42);
        let output = metrics.to_prometheus();
        assert!(
            !output.contains("cardano_node_metrics_"),
            "compat aliases must not appear when compat_metrics is false"
        );
    }

    #[test]
    fn test_compat_metrics_enabled() {
        let metrics = NodeMetrics::new();
        metrics.set_compat_metrics(true);

        // Set known values so we can assert exact alias output.
        metrics.set_slot(100_000);
        metrics.set_block_number(50_000);
        metrics.set_epoch(410);
        metrics.peers_connected.store(8, Ordering::Relaxed);
        metrics.set_utxo_count(23_000_000);
        metrics.set_mempool_count(7);
        metrics.mempool_bytes.store(14_336, Ordering::Relaxed);
        metrics.blocks_forged.store(3, Ordering::Relaxed);
        // 50% sync stored as 5000 (pct * 100)
        metrics.sync_progress_pct.store(5000, Ordering::Relaxed);

        let output = metrics.to_prometheus();

        // Each alias must be present with the correct value.
        assert!(
            output.contains("cardano_node_metrics_slotNum_int 100000"),
            "slotNum_int alias missing or wrong"
        );
        assert!(
            output.contains("cardano_node_metrics_blockNum_int 50000"),
            "blockNum_int alias missing or wrong"
        );
        assert!(
            output.contains("cardano_node_metrics_epoch_int 410"),
            "epoch_int alias missing or wrong"
        );
        assert!(
            output.contains("cardano_node_metrics_connectedPeers_int 8"),
            "connectedPeers_int alias missing or wrong"
        );
        assert!(
            output.contains("cardano_node_metrics_utxoSize_int 23000000"),
            "utxoSize_int alias missing or wrong"
        );
        assert!(
            output.contains("cardano_node_metrics_txsInMempool_int 7"),
            "txsInMempool_int alias missing or wrong"
        );
        assert!(
            output.contains("cardano_node_metrics_mempoolBytes_int 14336"),
            "mempoolBytes_int alias missing or wrong"
        );
        assert!(
            output.contains("cardano_node_metrics_Forge_forge_adopted_int 3"),
            "Forge_forge_adopted_int alias missing or wrong"
        );
        // 5000 / 10000 = 0.5 density
        assert!(
            output.contains("cardano_node_metrics_density_real 0.500000"),
            "density_real alias missing or wrong"
        );

        // Native dugite metrics must still be present alongside compat aliases.
        assert!(
            output.contains("dugite_slot_number 100000"),
            "native dugite_slot_number must still be present"
        );
    }

    #[test]
    fn test_compat_metrics_can_be_toggled() {
        // Verify that set_compat_metrics can be called multiple times and takes effect.
        let metrics = NodeMetrics::new();
        metrics.set_slot(1);

        // Off initially
        let out1 = metrics.to_prometheus();
        assert!(!out1.contains("cardano_node_metrics_"));

        // Enable
        metrics.set_compat_metrics(true);
        let out2 = metrics.to_prometheus();
        assert!(out2.contains("cardano_node_metrics_slotNum_int 1"));

        // Disable again
        metrics.set_compat_metrics(false);
        let out3 = metrics.to_prometheus();
        assert!(!out3.contains("cardano_node_metrics_"));
    }

    #[test]
    fn test_compat_density_real_zero_and_full() {
        let metrics = NodeMetrics::new();
        metrics.set_compat_metrics(true);

        // 0% sync
        metrics.sync_progress_pct.store(0, Ordering::Relaxed);
        let out = metrics.to_prometheus();
        assert!(
            out.contains("cardano_node_metrics_density_real 0.000000"),
            "density_real must be 0.0 at 0% sync"
        );

        // 100% sync stored as 10000
        metrics.sync_progress_pct.store(10000, Ordering::Relaxed);
        let out = metrics.to_prometheus();
        assert!(
            out.contains("cardano_node_metrics_density_real 1.000000"),
            "density_real must be 1.0 at 100% sync"
        );
    }

    #[test]
    fn test_histogram_observe() {
        let h = Histogram::new();
        h.observe(5.0); // → bucket le=5
        h.observe(50.0); // → bucket le=50
        h.observe(500.0); // → bucket le=500

        assert_eq!(h.count.load(Ordering::Relaxed), 3);
        let sum = f64::from_bits(h.sum_bits.load(Ordering::Relaxed));
        assert!((sum - 555.0).abs() < 0.01);

        // Each observation lands in exactly one bucket
        assert_eq!(h.buckets[1].load(Ordering::Relaxed), 1); // le=5.0
        assert_eq!(h.buckets[4].load(Ordering::Relaxed), 1); // le=50.0
        assert_eq!(h.buckets[7].load(Ordering::Relaxed), 1); // le=500.0

        // Verify cumulative output via prometheus format
        let output = h.to_prometheus("test", "test");
        assert!(output.contains("test_bucket{le=\"5\"} 1"));
        assert!(output.contains("test_bucket{le=\"50\"} 2")); // cumulative: 5 + 50
        assert!(output.contains("test_bucket{le=\"500\"} 3")); // cumulative: all three
        assert!(output.contains("test_bucket{le=\"+Inf\"} 3"));
    }

    #[test]
    fn test_histogram_prometheus_format() {
        let h = Histogram::new();
        h.observe(10.0);
        h.observe(100.0);

        let output = h.to_prometheus("test_latency", "Test latency");
        assert!(output.contains("# TYPE test_latency histogram"));
        assert!(output.contains("test_latency_bucket{le=\"10\"} 1"));
        assert!(output.contains("test_latency_bucket{le=\"100\"} 2"));
        assert!(output.contains("test_latency_bucket{le=\"+Inf\"} 2"));
        assert!(output.contains("test_latency_sum 110"));
        assert!(output.contains("test_latency_count 2"));
    }

    #[test]
    fn test_prometheus_output_includes_histograms() {
        let metrics = NodeMetrics::new();
        metrics.record_handshake_rtt(50.0);
        metrics.record_block_fetch_latency(25.0);

        let output = metrics.to_prometheus();
        assert!(output.contains("dugite_peer_handshake_rtt_ms_bucket"));
        assert!(output.contains("dugite_peer_block_fetch_ms_bucket"));
        assert!(output.contains("dugite_uptime_seconds"));
    }

    #[test]
    fn test_handshake_rtt_records_to_histogram() {
        let metrics = NodeMetrics::new();
        metrics.record_handshake_rtt(42.0);
        metrics.record_handshake_rtt(150.0);
        let output = metrics.to_prometheus();
        assert!(output.contains("dugite_peer_handshake_rtt_ms_count 2"));
        // 42ms lands in le=50 bucket, 150ms lands in le=250 bucket
        assert!(output.contains("peer_handshake_rtt_ms_bucket{le=\"50\"} 1"));
        assert!(output.contains("peer_handshake_rtt_ms_bucket{le=\"250\"} 2"));
    }

    #[test]
    fn test_block_fetch_latency_records_to_histogram() {
        let metrics = NodeMetrics::new();
        metrics.record_block_fetch_latency(25.0);
        metrics.record_block_fetch_latency(300.0);
        let output = metrics.to_prometheus();
        assert!(output.contains("dugite_peer_block_fetch_ms_count 2"));
        assert!(output.contains("peer_block_fetch_ms_bucket{le=\"25\"} 1"));
        assert!(output.contains("peer_block_fetch_ms_bucket{le=\"500\"} 2"));
    }

    #[test]
    fn test_prometheus_output_includes_cpu_metrics() {
        // Verify that the two CPU-related metrics are always present in the
        // Prometheus output, even when the measured value is zero (which is the
        // case on platforms without a sampling implementation or immediately
        // after node start before any meaningful CPU has been consumed).
        let metrics = NodeMetrics::new();
        let output = metrics.to_prometheus();

        // Both metrics must be present with correct types.
        assert!(
            output.contains("# TYPE dugite_cpu_percent gauge"),
            "dugite_cpu_percent gauge TYPE declaration missing"
        );
        assert!(
            output.contains("# TYPE dugite_cpu_seconds_total counter"),
            "dugite_cpu_seconds_total counter TYPE declaration missing"
        );
        // The gauge line must exist (value may be 0.000 on first call).
        assert!(
            output.contains("dugite_cpu_percent "),
            "dugite_cpu_percent value line missing"
        );
        assert!(
            output.contains("dugite_cpu_seconds_total "),
            "dugite_cpu_seconds_total value line missing"
        );
        // The resident-memory metric should still be present alongside CPU.
        assert!(output.contains("dugite_mem_resident_bytes "));
    }

    #[test]
    fn test_cpu_tracker_cumulative_seconds_non_negative() {
        // After two sample() calls the cumulative CPU seconds must be >= 0.
        let mut tracker = CpuTracker::new();
        let _pct1 = tracker.sample();
        let _pct2 = tracker.sample();
        assert!(
            tracker.cumulative_cpu_secs >= 0.0,
            "cumulative CPU seconds must be non-negative, got {}",
            tracker.cumulative_cpu_secs
        );
    }

    #[test]
    fn test_health_status_healthy() {
        let metrics = NodeMetrics::new();
        // 99.9% = 9990 (stored as pct * 100)
        metrics.sync_progress_pct.store(9990, Ordering::Relaxed);
        assert_eq!(metrics.health_status(), "healthy");

        // Above threshold is also healthy
        metrics.sync_progress_pct.store(10000, Ordering::Relaxed);
        assert_eq!(metrics.health_status(), "healthy");
    }

    #[test]
    fn test_health_status_syncing() {
        let metrics = NodeMetrics::new();
        // 50% sync, recently received a block
        metrics.sync_progress_pct.store(5000, Ordering::Relaxed);
        metrics.record_block_received();
        assert_eq!(metrics.health_status(), "syncing");
    }

    #[test]
    fn test_health_status_stalled() {
        let metrics = NodeMetrics::new();
        // Below 99% and last block was > 5 minutes ago
        metrics.sync_progress_pct.store(5000, Ordering::Relaxed);
        let five_min_ago_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            - (STALLED_THRESHOLD_SECS + 10) * 1000;
        metrics
            .last_block_received_at
            .store(five_min_ago_ms, Ordering::Relaxed);
        assert_eq!(metrics.health_status(), "stalled");
    }

    #[test]
    fn test_health_status_not_stalled_when_synced() {
        let metrics = NodeMetrics::new();
        // Even if last block was long ago, if we're synced we're healthy
        metrics.sync_progress_pct.store(9990, Ordering::Relaxed);
        let old_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            - 600_000; // 10 minutes ago
        metrics
            .last_block_received_at
            .store(old_ms, Ordering::Relaxed);
        assert_eq!(metrics.health_status(), "healthy");
    }

    #[test]
    fn test_readiness_check() {
        let metrics = NodeMetrics::new();

        // Not ready at 0%
        assert!(!metrics.is_ready());

        // Not ready at 99%
        metrics.sync_progress_pct.store(9900, Ordering::Relaxed);
        assert!(!metrics.is_ready());

        // Ready at 99.9%
        metrics.sync_progress_pct.store(9990, Ordering::Relaxed);
        assert!(metrics.is_ready());

        // Ready at 100%
        metrics.sync_progress_pct.store(10000, Ordering::Relaxed);
        assert!(metrics.is_ready());
    }

    #[test]
    fn test_last_block_received_iso() {
        let metrics = NodeMetrics::new();

        // No block received yet
        assert!(metrics.last_block_received_iso().is_none());

        // Record a block
        metrics.record_block_received();
        let iso = metrics.last_block_received_iso();
        assert!(iso.is_some());
        let ts = iso.unwrap();
        // Should be a valid ISO 8601 string containing 'T' and 'Z'
        assert!(ts.contains('T'));
        assert!(ts.contains('Z'));
    }

    #[test]
    fn test_record_block_received_updates_timestamp() {
        let metrics = NodeMetrics::new();
        assert_eq!(metrics.last_block_received_at.load(Ordering::Relaxed), 0);
        metrics.record_block_received();
        let ts = metrics.last_block_received_at.load(Ordering::Relaxed);
        assert!(ts > 0);
        // Should be within the last second
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(now_ms - ts < 1000);
    }

    #[test]
    fn test_peer_rtt_gauges_update_and_emit() {
        let metrics = NodeMetrics::new();

        // No latencies — gauges should be zero.
        metrics.update_peer_rtt_gauges(&[]);
        let output = metrics.to_prometheus();
        assert!(output.contains("dugite_peer_rtt_avg_ms 0.0"));
        assert!(output.contains("dugite_peer_rtt_min_ms 0.0"));
        assert!(output.contains("dugite_peer_rtt_max_ms 0.0"));
        assert!(output.contains("dugite_peer_rtt_band_0_50 0\n"));
        assert!(output.contains("dugite_peer_rtt_band_50_100 0\n"));
        assert!(output.contains("dugite_peer_rtt_band_100_200 0\n"));
        assert!(output.contains("dugite_peer_rtt_band_200_plus 0\n"));
        assert!(output.contains("dugite_peer_rtt_samples 0\n"));

        // Two peers with different latencies.
        metrics.update_peer_rtt_gauges(&[40.0, 120.0]);
        let output = metrics.to_prometheus();
        // avg = (40 + 120) / 2 = 80
        assert!(output.contains("dugite_peer_rtt_avg_ms 80.0"));
        assert!(output.contains("dugite_peer_rtt_min_ms 40.0"));
        assert!(output.contains("dugite_peer_rtt_max_ms 120.0"));
        // 40ms → band_0_50; 120ms → band_100_200
        assert!(output.contains("dugite_peer_rtt_band_0_50 1\n"));
        assert!(output.contains("dugite_peer_rtt_band_50_100 0\n"));
        assert!(output.contains("dugite_peer_rtt_band_100_200 1\n"));
        assert!(output.contains("dugite_peer_rtt_band_200_plus 0\n"));
        assert!(output.contains("dugite_peer_rtt_samples 2\n"));

        // Verify bands shrink when peers disconnect (no carry-over).
        metrics.update_peer_rtt_gauges(&[40.0]);
        let output = metrics.to_prometheus();
        assert!(output.contains("dugite_peer_rtt_band_0_50 1\n"));
        assert!(output.contains("dugite_peer_rtt_band_100_200 0\n"));
        assert!(output.contains("dugite_peer_rtt_samples 1\n"));

        // Boundary: exactly 50ms goes into [50,100), exactly 200ms into [200,inf).
        metrics.update_peer_rtt_gauges(&[49.999, 50.0, 99.999, 100.0, 199.999, 200.0]);
        let output = metrics.to_prometheus();
        assert!(output.contains("dugite_peer_rtt_band_0_50 1\n"));
        assert!(output.contains("dugite_peer_rtt_band_50_100 2\n"));
        assert!(output.contains("dugite_peer_rtt_band_100_200 2\n"));
        assert!(output.contains("dugite_peer_rtt_band_200_plus 1\n"));
        assert!(output.contains("dugite_peer_rtt_samples 6\n"));
    }

    // D3/D4: governance gauges must reflect the snapshot immediately —
    // no 5-second delay.  set_governance_snapshot is called unconditionally
    // per block; this test confirms the stored values are visible right away.
    #[test]
    fn test_governance_snapshot_immediate() {
        let metrics = NodeMetrics::new();

        // Verify initial state is zero.
        assert_eq!(metrics.proposal_count.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.drep_count.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.drep_active.load(Ordering::Relaxed), 0);

        // Simulate the snapshot arriving from a block that added proposals and
        // DRep registrations.
        let snap = GovernanceSnapshot {
            delegation_count: 1000,
            treasury_lovelace: 500_000_000,
            reserves_lovelace: 200_000_000,
            pool_count: 3000,
            drep_total: 8920,
            drep_active: 7500,
            drep_registrations_total: 9100,
            vote_delegation_count: 450_000,
            proposal_count: 22,
            committee_hot_count: 7,
            committee_total_count: 8,
            committee_resigned_count: 0,
            committee_no_confidence: false,
            committee_threshold_bps: 6700,
            gov_dormant_epochs: 0,
            constitution_present: true,
            pparam_drep_deposit_lovelace: 500_000_000,
            pparam_drep_activity_epochs: 20,
            pparam_gov_action_deposit_lovelace: 100_000_000_000,
            pparam_gov_action_lifetime_epochs: 6,
            pparam_committee_min_size: 7,
            pparam_committee_max_term_length: 146,
        };
        metrics.set_governance_snapshot(&snap);

        // Gauges must be visible immediately — no delay gate.
        assert_eq!(metrics.proposal_count.load(Ordering::Relaxed), 22);
        assert_eq!(metrics.drep_count.load(Ordering::Relaxed), 8920);
        assert_eq!(metrics.drep_active.load(Ordering::Relaxed), 7500);

        // Prometheus text output must carry the updated values.
        let output = metrics.to_prometheus();
        assert!(output.contains("dugite_proposal_count 22\n"));
        assert!(output.contains("dugite_drep_count 8920\n"));
        assert!(output.contains("dugite_drep_active 7500\n"));
    }

    // D5: dugite_drep_count HELP text must document the active+inactive
    // semantics and point to Koios for DReps-with-delegated-stake.
    #[test]
    fn test_drep_count_help_text() {
        let metrics = NodeMetrics::new();
        let output = metrics.to_prometheus();
        assert!(
            output.contains("active + inactive"),
            "HELP text must mention 'active + inactive' — got: {}",
            output
                .lines()
                .find(|l| l.contains("dugite_drep_count"))
                .unwrap_or("<not found>")
        );
        assert!(
            output.contains("Koios drep_epoch_summary"),
            "HELP text must reference Koios drep_epoch_summary"
        );
    }

    // ── Metrics server bind + serve tests ───────────────────────────────────

    /// Verify that `bind_metrics_listener` binds successfully on a
    /// kernel-assigned port (port 0) and that the listener's local address
    /// is a valid socket with a non-zero port.
    ///
    /// Uses port 0 so the OS picks any free ephemeral port — no hardcoded
    /// port number means this test is safe to run in any CI environment.
    #[tokio::test]
    async fn test_bind_metrics_listener_custom_port_zero() {
        let listener = bind_metrics_listener(0)
            .await
            .expect("bind_metrics_listener(0) must succeed");
        let local_addr = listener
            .local_addr()
            .expect("listener must have a local address");
        assert!(
            local_addr.port() > 0,
            "OS-assigned port must be non-zero, got {local_addr}"
        );
    }

    /// Full round-trip: bind on an OS-assigned port, start the metrics server
    /// in a background task, issue an HTTP GET /metrics request, assert a 200
    /// with the correct Prometheus payload, then shut down cleanly.
    ///
    /// This test validates the entire custom-port path end-to-end: bind →
    /// accept loop → response routing.
    #[tokio::test]
    async fn test_metrics_server_responds_on_custom_port() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let metrics = Arc::new(NodeMetrics::new());
        metrics.set_slot(42_000);

        // Bind on an OS-assigned port so this test never collides with a node.
        let listener = bind_metrics_listener(0).await.expect("bind must succeed");
        let bound_port = listener.local_addr().expect("local_addr").port();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let metrics_clone = metrics.clone();
        let server_handle = tokio::spawn(async move {
            run_metrics_server(listener, metrics_clone, shutdown_rx).await;
        });

        // Give the accept loop a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Issue a minimal HTTP GET request and read the full response.
        let mut stream = TcpStream::connect(format!("127.0.0.1:{bound_port}"))
            .await
            .expect("connect must succeed");
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write must succeed");

        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        let response_str = String::from_utf8_lossy(&response);

        assert!(
            response_str.starts_with("HTTP/1.1 200 OK"),
            "expected 200 OK, got: {}",
            &response_str[..response_str.len().min(100)]
        );
        assert!(
            response_str.contains("dugite_slot_number 42000"),
            "expected slot metric in response"
        );
        assert!(
            response_str.contains("text/plain"),
            "expected Prometheus content-type"
        );

        // Shut down the server task cleanly.
        shutdown_tx.send(true).ok();
        let _ = server_handle.await;
    }
}
